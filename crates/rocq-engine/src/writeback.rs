//! Synchronous PET/Dune-backed source writeback.
//!
//! This module owns exactly one transaction: derive a replacement from PET's
//! range, compare-and-swap the source, build it with Dune, and restore the old
//! bytes if validation fails. It stores no publication lifecycle or recovery
//! record.

use super::*;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;

/// Fully rendered single-file CAS operation. PET ranges are consumed while
/// constructing this value and are never retained as parallel source state.
struct WritebackPlan {
    path: PathBuf,
    expected_digest: [u8; 32],
    replacement: Vec<u8>,
}

/// PET-derived source trust policy: exact explicit axioms and forbidden holes.
type FrozenTrust = (Vec<(String, String)>, BTreeSet<String>);

/// Publish a PET-completed trace. Success means the source was atomically
/// replaced and Dune accepted the resulting compilation unit. Every failure is
/// returned directly; no `Pending` state is manufactured.
pub(crate) fn publish(
    engine: &Engine,
    project: &Path,
    declaration: &DeclarationSource,
    commands: &[CanonicalTactic],
    native: &pet::PetState,
) -> Result<ProofState> {
    let assumptions = engine
        .pet
        .candidate_assumptions(project, native, declaration, commands)
        .map_err(|error| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                format!("PET assumption audit failed: {error}"),
            )
        })?;
    let (allowed_axioms, forbidden_locals) = frozen_trust(
        &engine.pet,
        project,
        &declaration.info.identity,
        &assumptions,
        engine.config.operation_timeout,
    )?;
    audit_assumption_report(&assumptions, &allowed_axioms, &forbidden_locals)?;

    for retry in 0..=3 {
        let plan = replacement_plan(engine, project, declaration, commands)?;
        let original = fs::read(&plan.path).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "target source disappeared during writeback",
            )
        })?;
        let current_digest = <[u8; 32]>::from(Sha256::digest(&original));
        if current_digest != plan.expected_digest {
            if retry == 3 {
                return Err(Error::new(
                    ErrorKind::DeclarationChanged,
                    "target source kept changing during writeback",
                ));
            }
            continue;
        }
        atomic_write(&plan.path, &plan.replacement)?;
        let target = plan.path.strip_prefix(project).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "source target escapes project",
            )
        })?;
        if let Err(build_error) = native_build(
            project,
            target,
            engine.config.close_timeout,
            engine.config.operation_timeout,
        ) {
            atomic_write(&plan.path, &original).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Dune rejected the proof and the original source could not be restored",
                )
            })?;
            // Dune owns derived artifacts and will reconcile them on its next
            // build. The source CAS has been rolled back, so preserve the
            // original concrete build failure instead of inventing Pending.
            return Err(build_error);
        }
        engine.pet.invalidate_states(project);
        return Ok(ProofState {
            attempt: None,
            theorem: DeclarationInfo {
                identity: declaration.info.identity.clone(),
                kind: declaration.info.kind,
                statement: declaration.info.statement.clone(),
            },
            lifecycle: ProofLifecycle::Completed,
            focused_goals: 0,
            unfocused_goals: 0,
            shelved_goals: 0,
            given_up_goals: 0,
            goals: String::new(),
        });
    }
    unreachable!("bounded CAS loop returns on its final iteration")
}

/// Re-derive the current source edit from Dune ownership and PET ranges. The
/// returned expected digest is the sole compare-and-swap precondition.
fn replacement_plan(
    engine: &Engine,
    project: &Path,
    declaration: &DeclarationSource,
    commands: &[CanonicalTactic],
) -> Result<WritebackPlan> {
    if declaration.anchor.header.start < declaration.anchor.header.end {
        let source = engine
            .pet
            .source_span(project, declaration)
            .map_err(|error| engine.pet_error(error))?;
        let bytes = fs::read(&source.anchor.source).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "target source is unavailable",
            )
        })?;
        if source.info.kind != declaration.info.kind
            || source.info.statement != declaration.info.statement
        {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "target declaration changed while proof was open",
            ));
        }
        let replacement = render_replacement(
            &bytes,
            source
                .anchor
                .declaration
                .as_ref()
                .map(|range| range.start..range.end)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::DeclarationChanged,
                        "PET declaration range is unavailable",
                    )
                })?,
            source.anchor.header.end,
            declaration.info.kind,
            commands,
        )?;
        return Ok(WritebackPlan {
            path: source.anchor.source,
            expected_digest: Sha256::digest(&bytes).into(),
            replacement,
        });
    }

    // A newly declared theorem is not yet in PET's table of contents. Dune
    // still owns the target source and PET has already supplied an exact
    // zero-width insertion point (EOF for top level, before `End` for a
    // nested module).
    let layout = dune::Layout::load(project, &[], engine.config.operation_timeout)?;
    let path = layout.target(&declaration.library)?;
    if !path.is_file() || !layout.files().contains(&path) {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune has not selected the declaration target source",
        ));
    }
    let bytes = fs::read(&path).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declaration target source is unavailable",
        )
    })?;
    if <[u8; 32]>::from(Sha256::digest(&bytes)) != declaration.anchor.digest {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "declaration target changed while proof was open",
        ));
    }
    let insertion = declaration.anchor.header.start;
    if insertion != declaration.anchor.header.end || insertion > bytes.len() {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "PET declaration insertion point is invalid",
        ));
    }
    let mut inserted = String::new();
    if insertion > 0 && bytes.get(insertion - 1) != Some(&b'\n') {
        inserted.push('\n');
    }
    inserted.push_str(&declaration.info.statement);
    inserted.push_str(".\n");
    append_proof(&mut inserted, declaration.info.kind, commands);
    let mut replacement = bytes[..insertion].to_vec();
    replacement.extend_from_slice(inserted.as_bytes());
    replacement.extend_from_slice(&bytes[insertion..]);
    Ok(WritebackPlan {
        path,
        expected_digest: Sha256::digest(&bytes).into(),
        replacement,
    })
}

fn render_replacement(
    bytes: &[u8],
    range: Range<usize>,
    header_end: usize,
    kind: DeclarationKind,
    commands: &[CanonicalTactic],
) -> Result<Vec<u8>> {
    if range.start > header_end || header_end > range.end || range.end > bytes.len() {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "PET returned an invalid declaration range",
        ));
    }
    let header = std::str::from_utf8(&bytes[range.start..header_end]).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Rocq source is not valid UTF-8",
        )
    })?;
    let mut body = header.to_owned();
    if !body.ends_with(char::is_whitespace) {
        body.push('\n');
    }
    append_proof(&mut body, kind, commands);
    let mut replacement = bytes[..range.start].to_vec();
    replacement.extend_from_slice(body.as_bytes());
    replacement.extend_from_slice(&bytes[range.end..]);
    Ok(replacement)
}

fn append_proof(body: &mut String, kind: DeclarationKind, commands: &[CanonicalTactic]) {
    if kind != DeclarationKind::Definition {
        body.push_str("Proof.\n");
    }
    for command in commands {
        body.push_str(&command.0);
        body.push('\n');
    }
    body.push_str(kind.terminator());
    body.push('\n');
}

fn frozen_trust(
    pet: &pet::PetRuntime,
    project: &Path,
    target: &DeclarationIdentity,
    report: &pet::PetAssumptionReport,
    timeout: Duration,
) -> Result<FrozenTrust> {
    let layout = dune::Layout::load(project, &[], timeout)?;
    // Design note: `Print Assumptions` is the dependency oracle. Inspect only
    // the Dune-owned source files that can define assumptions PET actually
    // returned; scanning every project file would turn one close into a
    // workspace index and make unrelated broken files semantically relevant.
    let files = report
        .assumptions
        .iter()
        .filter_map(|(name, _)| layout.source_for_constant(name))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let trust = pet
        .source_trust(project, &files)
        .map_err(|error| Error::new(ErrorKind::InvalidConfiguration, error.to_string()))?;
    let target_name = format_name(target);
    Ok((
        trust.explicit_axioms,
        trust
            .admitted
            .into_iter()
            .filter(|name| name != &target_name)
            .collect(),
    ))
}

fn audit_assumption_report(
    report: &pet::PetAssumptionReport,
    allowed: &[(String, String)],
    forbidden: &BTreeSet<String>,
) -> Result<()> {
    for (name, ty) in &report.assumptions {
        if forbidden.contains(name) {
            return Err(Error::new(
                ErrorKind::UnfinishedDependency,
                format!("proof depends on unfinished local declaration '{name}'"),
            ));
        }
        if !allowed
            .iter()
            .any(|(allowed_name, allowed_type)| name == allowed_name && ty == allowed_type)
        {
            return Err(Error::new(
                ErrorKind::AxiomDependencyOutOfScope,
                format!("axiom dependency is outside the authorized baseline: {name}"),
            ));
        }
    }
    Ok(())
}

/// Audit an already compiled source theorem under the same PET-supplied trust
/// policy as interactive writeback.
pub(crate) fn audit_existing_source(
    engine: &Engine,
    project: &Path,
    source: &DeclarationSource,
    report: &pet::PetAssumptionReport,
    metadata_timeout: Duration,
    _close_timeout: Option<Duration>,
) -> Result<()> {
    let (allowed, forbidden) = frozen_trust(
        &engine.pet,
        project,
        &source.info.identity,
        report,
        metadata_timeout,
    )?;
    audit_assumption_report(report, &allowed, &forbidden)
}

/// Build one Dune-selected source target.
pub(crate) fn native_build(
    project: &Path,
    target: &Path,
    timeout: Option<Duration>,
    layout_timeout: Duration,
) -> Result<()> {
    let (workspace, dune_target) = dune_target(project, target, layout_timeout)?;
    let output = dune::run_dune(
        [OsString::from("build"), dune_target.into_os_string()],
        &workspace,
        timeout,
        None,
    )
    .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "failed to execute Dune"))?;
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
            "Dune rejected the proof source",
        ));
    }
    Ok(())
}

/// Map a project-relative source to the build target reported by Dune.
pub(crate) fn dune_target(
    project: &Path,
    target: &Path,
    timeout: Duration,
) -> Result<(PathBuf, PathBuf)> {
    let layout = dune::Layout::load(project, &[], timeout)?;
    let workspace = layout.workspace_root().to_owned();
    let source = fs::canonicalize(project.join(target)).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune source target is unavailable",
        )
    })?;
    let build_target = layout.build_target(&source)?;
    Ok((workspace, build_target))
}

/// Atomically replace one file and fsync both file and containing directory.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "source has no parent directory",
        )
    })?;
    let tmp = parent.join(format!(".rocq-mcp-writeback-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "atomic source replacement failed",
        )
    })
}
