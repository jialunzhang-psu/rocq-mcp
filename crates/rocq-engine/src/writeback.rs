//! PET-range-based compare-and-swap publication.

use crate::types::PetWorkspace;
use crate::{DeclarationTarget, DuneProject, Error, ErrorKind, PetActor, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    time::Duration,
};

/// Fully prepared, side-effect-free source transaction. Constructing this
/// value performs the source CAS check and range rendering before MCP destroys
/// any usable checkpoint handle.
pub(crate) struct PreparedPublication {
    path: std::path::PathBuf,
    workspace: PetWorkspace,
    original: Vec<u8>,
    replacement: Vec<u8>,
    replacement_digest: [u8; 32],
}

pub(crate) fn prepare(
    project: &DuneProject,
    target: &DeclarationTarget,
    fragments: &[String],
) -> Result<PreparedPublication> {
    if !target.anchor.replaceable {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "source range is shared by multiple declarations",
        ));
    }
    let path = target.anchor.source.clone();
    let workspace = project.pet_workspace(&path)?;
    let original = fs::read(&path).map_err(|_| {
        Error::new(
            ErrorKind::DeclarationChanged,
            "target source is unavailable",
        )
    })?;
    if digest(&original) != target.anchor.digest {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "target source changed while proof was open",
        ));
    }
    let replacement = replacement(&original, target, fragments)?;
    let replacement_digest = digest(&replacement);
    Ok(PreparedPublication {
        path,
        workspace,
        original,
        replacement,
        replacement_digest,
    })
}

/// Commit one prepared publication after MCP invalidates the project epoch.
/// Any failure after replacement performs a CAS-safe restoration, rebuilds the
/// source that actually won the race, and refreshes PET before returning.
#[cfg(test)]
pub(crate) fn publish<T>(
    project: &DuneProject,
    actor: &PetActor,
    prepared: PreparedPublication,
    timeout: Option<Duration>,
    validate: impl FnOnce(&DuneProject, &PetActor) -> Result<T>,
) -> Result<T> {
    let mut progress = |_: &'static str, _: &str| {};
    publish_with_progress(project, actor, prepared, timeout, &mut progress, validate)
}

pub(crate) fn publish_with_progress<T>(
    project: &DuneProject,
    actor: &PetActor,
    prepared: PreparedPublication,
    timeout: Option<Duration>,
    progress: &mut dyn FnMut(&'static str, &str),
    validate: impl FnOnce(&DuneProject, &PetActor) -> Result<T>,
) -> Result<T> {
    let PreparedPublication {
        path,
        workspace,
        original,
        replacement,
        replacement_digest,
    } = prepared;
    let original_digest = digest(&original);
    progress("writeback", "checking source ownership before atomic write");
    let current = fs::read(&path);
    if current
        .as_ref()
        .map_or(true, |current| digest(current) != original_digest)
    {
        // The MCP owner has already begun a project epoch. Admit the external
        // winner through Dune/PET, but never replace its bytes.
        restore_epoch(
            project,
            actor,
            &path,
            &workspace,
            &original,
            replacement_digest,
            timeout,
            progress,
        )?;
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "target source changed before publication commit",
        ));
    }
    if let Err(error) = atomic_write(&path, &replacement) {
        restore_epoch(
            project,
            actor,
            &path,
            &workspace,
            &original,
            replacement_digest,
            timeout,
            progress,
        )?;
        return Err(error);
    }

    let result = (|| {
        progress("dune_build", "building the published Dune target");
        native_build(project, &path, timeout)?;
        require_digest(&path, replacement_digest)?;
        progress("pet_refresh", "refreshing PET after native build");
        actor.refresh_workspace(&workspace).map_err(refresh_error)?;
        require_digest(&path, replacement_digest)?;
        progress("trust_audit", "auditing the completed declaration");
        let value = validate(project, actor)?;
        require_digest(&path, replacement_digest)?;
        Ok(value)
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            progress(
                "rollback_build",
                "restoring and rebuilding the previous source",
            );
            let restored = restore_epoch(
                project,
                actor,
                &path,
                &workspace,
                &original,
                replacement_digest,
                timeout,
                progress,
            )?;
            if restored {
                Err(error)
            } else {
                Err(Error::new(
                    ErrorKind::DeclarationChanged,
                    format!("source changed during failed publication; original error: {error}"),
                ))
            }
        }
    }
}

/// Restore the original bytes only when our exact replacement still owns the
/// source. The resulting source is built and admitted as a fresh PET epoch.
// Design note: rollback receives the original transaction tuple plus the
// progress sink explicitly so it cannot accidentally consult mutable global
// publication state while repairing a failed CAS/build.
#[allow(clippy::too_many_arguments)]
fn restore_epoch(
    project: &DuneProject,
    actor: &PetActor,
    path: &Path,
    workspace: &PetWorkspace,
    original: &[u8],
    replacement_digest: [u8; 32],
    timeout: Option<Duration>,
    progress: &mut dyn FnMut(&'static str, &str),
) -> Result<bool> {
    let current = fs::read(path);
    let restored = match current {
        Ok(current) if digest(&current) == replacement_digest => {
            atomic_write(path, original)?;
            true
        }
        // An unreadable/missing path or different digest belongs to the
        // external winner. Never recreate or overwrite it during rollback.
        Ok(_) | Err(_) => false,
    };
    progress("rollback_build", "building the source selected by rollback");
    let build = native_build(project, path, timeout).map_err(|error| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            format!("publication rollback build failed: {error}"),
        )
    });
    progress("rollback_refresh", "refreshing PET after rollback build");
    let refresh = actor.refresh_workspace(workspace).map_err(|error| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            format!(
                "publication rollback could not refresh the project context: {}",
                error.public_message()
            ),
        )
    });
    match (build, refresh) {
        (Ok(()), Ok(())) => Ok(restored),
        (Err(build), Ok(())) => Err(build),
        (Ok(()), Err(refresh)) => Err(refresh),
        (Err(build), Err(refresh)) => Err(Error::new(
            ErrorKind::InvalidConfiguration,
            format!("{build}; additionally, {refresh}"),
        )),
    }
}

fn replacement(
    original: &[u8],
    target: &DeclarationTarget,
    fragments: &[String],
) -> Result<Vec<u8>> {
    let range = target.anchor.declaration.start..target.anchor.declaration.end;
    if range.start > range.end || range.end > original.len() {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "declaration range is inconsistent with the current source",
        ));
    }
    let mut body = if let Some(header) = &target.new_header {
        let mut text = String::new();
        if range.start > 0 && original[range.start - 1] != b'\n' {
            text.push('\n');
        }
        text.push_str(header);
        text.push_str(".\n");
        text
    } else {
        let header_end = target.anchor.header.end;
        if range.start > header_end || header_end > range.end {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "declaration header range is inconsistent with the current source",
            ));
        }
        let header = std::str::from_utf8(&original[range.start..header_end])
            .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "Rocq source is not UTF-8"))?;
        let mut text = header.to_owned();
        if !header.trim_end().ends_with('.') {
            text.push('.');
        }
        text.push('\n');
        text
    };
    if target.info.kind != crate::DeclarationKind::Definition {
        body.push_str("Proof.\n");
    }
    for fragment in fragments {
        body.push_str(fragment.trim());
        body.push('\n');
    }
    body.push_str(target.info.kind.terminator());
    body.push('\n');
    let mut output = original[..range.start].to_vec();
    output.extend_from_slice(body.as_bytes());
    output.extend_from_slice(&original[range.end..]);
    Ok(output)
}

/// Build exactly the target Dune reported for one source. No correctness
/// deadline exists unless the operator explicitly configured one.
pub(crate) fn native_build(
    project: &DuneProject,
    source: &Path,
    timeout: Option<Duration>,
) -> Result<()> {
    let target = project.build_target(source)?;
    let output = crate::dune::run_dune(
        [
            std::ffi::OsString::from("build"),
            target.as_os_str().to_owned(),
        ],
        project.id(),
        timeout,
        None,
    )
    .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "failed to execute Dune"))?;
    if output.cancelled {
        return Err(Error::new(
            ErrorKind::RequestCancelled,
            "native build was cancelled",
        ));
    }
    if output.timed_out {
        return Err(Error::new(
            ErrorKind::BuildTimeout,
            "native build timed out",
        ));
    }
    if output.overflow {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune output exceeded the configured bound",
        ));
    }
    if !output.status.success() {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            format!(
                "proof source was rejected: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(())
}

/// Run a build epoch for an already-published declaration. PET is refreshed
/// after both success and normal Dune rejection because either build may have
/// replaced intermediate artifacts before reaching its terminal status.
pub(crate) fn build_and_refresh<T>(
    project: &DuneProject,
    actor: &PetActor,
    source: &Path,
    timeout: Option<Duration>,
    progress: &mut dyn FnMut(&'static str, &str),
    validate: impl FnOnce(&DuneProject, &PetActor) -> Result<T>,
) -> Result<T> {
    let workspace = project.pet_workspace(source)?;
    progress("dune_build", "building the selected Dune target");
    let build = native_build(project, source, timeout);
    progress("pet_refresh", "refreshing PET after native build");
    let refresh = actor.refresh_workspace(&workspace).map_err(refresh_error);
    match (build, refresh) {
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Ok(())) => {
            progress("trust_audit", "auditing the completed declaration");
            validate(project, actor)
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    // RISK: POSIX has no atomic "replace only if these bytes still match"
    // primitive. The transaction checks the digest immediately before this
    // same-directory atomic rename and verifies ownership afterward, but a
    // non-cooperating external writer can still race inside that narrow
    // compare/rename interval. Project-internal writers are serialized by the
    // MCP operation barrier.
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidConfiguration, "source has no parent"))?;
    let result = (|| -> std::io::Result<()> {
        let permissions = fs::metadata(path)?.permissions();
        let mut temporary = tempfile::Builder::new()
            .prefix(".rocq-mcp-writeback-")
            .tempfile_in(parent)?;
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        temporary.as_file().set_permissions(permissions)?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        File::open(parent)?.sync_all()
    })();
    result.map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "atomic source replacement failed",
        )
    })
}

fn require_digest(path: &Path, expected: [u8; 32]) -> Result<()> {
    let current = fs::read(path).map_err(|_| {
        Error::new(
            ErrorKind::DeclarationChanged,
            "published source became unavailable",
        )
    })?;
    if digest(&current) == expected {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::DeclarationChanged,
            "source changed during publication",
        ))
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn refresh_error(error: crate::pet::PetError) -> Error {
    let kind = match &error {
        crate::pet::PetError::Cancelled => ErrorKind::RequestCancelled,
        crate::pet::PetError::TimedOut { .. } => ErrorKind::ProofStepTimeout,
        error if error.lost() => ErrorKind::PetLost,
        error if error.is_internal_failure() => ErrorKind::PetFailure,
        _ => ErrorKind::InvalidConfiguration,
    };
    let projected = Error::new(kind, error.public_message());
    if error.is_semantic() {
        projected.semantic()
    } else {
        projected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeclarationIdentity, Engine, EngineConfig, FileId, OpenResult, PetStateId};

    struct Fixture {
        _directory: tempfile::TempDir,
        engine: Engine,
        project: DuneProject,
        actor: PetActor,
        target: DeclarationTarget,
        root_state: PetStateId,
        source: std::path::PathBuf,
        original: Vec<u8>,
        dune_file: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let dune_file = directory.path().join("dune");
        fs::write(
            directory.path().join("dune-project"),
            "(lang dune 3.22)\n(using rocq 0.12)\n",
        )
        .unwrap();
        fs::write(&dune_file, "(rocq.theory (name Demo))\n").unwrap();
        let source = directory.path().join("A.v");
        let original = b"Theorem t : True. Admitted.\n".to_vec();
        fs::write(&source, &original).unwrap();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let project = engine.attach(directory.path()).unwrap();
        let actor = PetActor::new();
        engine
            .reconcile_source(&project, &FileId("A.v".into()))
            .unwrap();
        let identity = DeclarationIdentity {
            file: FileId("A.v".into()),
            qualified_path: vec!["Demo".into(), "A".into(), "t".into()],
        };
        let opened = match engine.open_declaration(&project, &actor, identity).unwrap() {
            OpenResult::Open(opened) => *opened,
            OpenResult::Published(_) => panic!("fixture theorem must be open"),
        };
        Fixture {
            _directory: directory,
            engine,
            project,
            actor,
            target: opened.target,
            root_state: opened.state,
            source,
            original,
            dune_file,
        }
    }

    #[test]
    fn publication_rejects_a_range_shared_by_multiple_declarations() {
        let fixture = fixture();
        let mut target = fixture.target.clone();
        target.anchor.replaceable = false;
        let error = prepare(&fixture.project, &target, &["exact I.".into()])
            .err()
            .expect("shared range must fail closed");
        assert_eq!(error.kind, ErrorKind::InvalidDeclaration);
        assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
    }

    #[test]
    fn commit_rechecks_source_and_admits_the_external_winner() {
        let fixture = fixture();
        let prepared = prepare(&fixture.project, &fixture.target, &["exact I.".into()]).unwrap();
        let external = b"Theorem t : True. exact I. Qed.\nDefinition external_won := 1.\n";
        fs::write(&fixture.source, external).unwrap();
        let error = publish(&fixture.project, &fixture.actor, prepared, None, |_, _| {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::DeclarationChanged);
        assert_eq!(fs::read(&fixture.source).unwrap(), external);
        assert!(fixture.actor.goals(fixture.root_state).is_err());
    }

    #[test]
    fn rollback_never_replaces_an_external_edit() {
        let fixture = fixture();
        let prepared = prepare(&fixture.project, &fixture.target, &["exact I.".into()]).unwrap();
        let external = b"Theorem t : True. exact I. Qed.\nDefinition external_won := 2.\n";
        let source = fixture.source.clone();
        let error = publish(&fixture.project, &fixture.actor, prepared, None, |_, _| {
            fs::write(&source, external).unwrap();
            Err::<(), _>(Error::new(
                ErrorKind::InvalidDeclaration,
                "injected post-build validation failure",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::DeclarationChanged);
        assert_eq!(fs::read(&fixture.source).unwrap(), external);
    }

    #[test]
    fn failed_build_restores_source_and_refreshes_pet() {
        let fixture = fixture();
        let prepared = prepare(&fixture.project, &fixture.target, &["exact I.".into()]).unwrap();
        fs::write(&fixture.dune_file, "(this is not a valid dune stanza)\n").unwrap();
        assert!(
            publish(&fixture.project, &fixture.actor, prepared, None, |_, _| Ok(
                ()
            ))
            .is_err()
        );
        assert_eq!(fs::read(&fixture.source).unwrap(), fixture.original);
        assert!(fixture.actor.goals(fixture.root_state).is_err());

        fs::write(&fixture.dune_file, "(rocq.theory (name Demo))\n").unwrap();
        fixture
            .engine
            .reconcile_source(&fixture.project, &FileId("A.v".into()))
            .unwrap();
        let declarations = fixture
            .engine
            .list_decls(&fixture.project, &fixture.actor, &FileId("A.v".into()))
            .unwrap();
        assert_eq!(declarations.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn successful_atomic_write_preserves_source_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = fixture();
        fs::set_permissions(&fixture.source, fs::Permissions::from_mode(0o640)).unwrap();
        let prepared = prepare(&fixture.project, &fixture.target, &["exact I.".into()]).unwrap();
        publish(&fixture.project, &fixture.actor, prepared, None, |_, _| {
            Ok(())
        })
        .unwrap();
        assert_eq!(
            fs::metadata(&fixture.source).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}
