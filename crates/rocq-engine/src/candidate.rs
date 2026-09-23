//! Solved-branch arbitration and durable candidate construction.

use super::*;
use std::io::Read;
use std::{env, fs};
use walkdir::WalkDir;

/// Freeze the process/toolchain and unfinished-local trust boundary used by a
/// selected candidate. Explicit axiom authorization remains fail-closed here:
/// I3 may accept only roots proved to be present in this frozen baseline.
pub(crate) fn frozen_baseline(
    project: &Path,
    target: &DeclarationIdentity,
    timeout: Duration,
) -> Result<TrustBaseline> {
    let catalog = discover(project, Vec::new(), &BTreeMap::new())?;
    let forbidden_locals = catalog
        .declarations
        .iter()
        .filter(|item| item.status != ProofLifecycle::Completed && item.identity != *target)
        .map(|item| item.identity.constant.clone())
        .collect::<Vec<_>>();
    let explicit_axioms = explicit_axioms(project)?;
    let explicit_axioms = native_normalize_axioms(project, explicit_axioms, timeout)?;
    let toolchain = toolchain_identity(project, timeout)?;
    let external_artifacts = external_artifacts(project, timeout)?;
    TrustBaseline::new(
        explicit_axioms,
        external_artifacts,
        forbidden_locals,
        toolchain,
    )
    .map_err(repository_error)
}

/// Freeze the content identity of compiled libraries visible through Rocq's
/// default load path.  Project-owned sources are handled by the local
/// declaration/trust checks; only artifacts outside the project are admitted
/// here.  A changed or unreadable artifact is omitted, which makes the later
/// native audit fail closed rather than silently blessing a replacement.
fn external_artifacts(
    project: &Path,
    timeout: Duration,
) -> Result<Vec<(LogicalLibrary, [u8; 32])>> {
    let output = native_process::run(
        "rocq",
        [
            std::ffi::OsString::from("compile"),
            std::ffi::OsString::from("-where"),
        ],
        project,
        timeout,
    )
    .map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Rocq load path unavailable",
        )
    })?;
    if output.timed_out {
        return Err(Error::new(
            ErrorKind::ProofTimeout,
            "Rocq load path timed out",
        ));
    }
    if output.overflow || !output.status.success() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Rocq load path unavailable",
        ));
    }
    let default_root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !default_root.is_dir() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Rocq load path unavailable",
        ));
    }
    let project = fs::canonicalize(project).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "project root is unavailable",
        )
    })?;
    let mut roots = vec![(default_root, None)];
    roots.extend(
        layout::external_load_paths(&project)?
            .into_iter()
            .map(|(root, prefix)| (root, Some(prefix))),
    );
    let mut artifacts = Vec::new();
    for (root, configured_prefix) in roots {
        for entry in WalkDir::new(&root).follow_links(false).max_depth(8) {
            let entry = entry.map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Rocq artifact traversal failed",
                )
            })?;
            let path = entry.path();
            if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "vo") {
                continue;
            }
            let canonical = fs::canonicalize(path).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Rocq artifact is unavailable",
                )
            })?;
            if canonical.starts_with(&project) {
                continue;
            }
            let relative = canonical.strip_prefix(&root).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Rocq artifact path is invalid",
                )
            })?;
            let mut components = relative
                .components()
                .filter_map(|component| {
                    let std::path::Component::Normal(value) = component else {
                        return None;
                    };
                    value.to_str().map(str::to_owned)
                })
                .collect::<Vec<_>>();
            let Some(last) = components.last_mut() else {
                continue;
            };
            if !last.ends_with(".vo") {
                continue;
            }
            last.truncate(last.len() - 3);
            // The installation root contains `theories/` (logical prefix Coq)
            // and `user-contrib/<package>/` (logical prefix package).  Other
            // directories are not part of Rocq's effective logical load path.
            let logical = if let Some(prefix) = configured_prefix.clone() {
                let mut names = prefix.0;
                names.extend(components);
                names
            } else if components.first().is_some_and(|x| x == "theories") {
                components.drain(..1);
                let mut names = vec!["Coq".to_owned()];
                names.extend(components);
                names
            } else if components.first().is_some_and(|x| x == "user-contrib")
                && components.len() >= 2
            {
                components.drain(..1);
                components
            } else {
                continue;
            };
            if logical.iter().any(|component| !valid_identifier(component)) {
                continue;
            }
            let mut file = fs::File::open(&canonical).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Rocq artifact is unavailable",
                )
            })?;
            let mut digest = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer).map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidConfiguration,
                        "Rocq artifact is unavailable",
                    )
                })?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
            artifacts.push((LogicalLibrary(logical), <[u8; 32]>::from(digest.finalize())));
        }
    }
    Ok(artifacts)
}

/// Collects only explicit vernacular assumption declarations. Admitted proofs
/// are represented separately as forbidden locals and can never enter this set.
fn explicit_axioms(project: &Path) -> Result<Vec<(String, String)>> {
    let layout = layout::Layout::load(project, &[])?;
    let mut output = Vec::new();
    for file in layout.files() {
        let source = read_source(&file)?;
        let mut modules = Vec::<String>::new();
        for range in sentence_ranges(&source)? {
            let sentence = normalize_sentence(&source[range]);
            let words = lexical_words(&sentence);
            let first = words.first().map(|word| word.to_ascii_lowercase());
            if matches!(
                first.as_deref(),
                Some(
                    "axiom" | "axioms" | "parameter" | "parameters" | "conjecture" | "conjectures"
                )
            ) && let Some((names, ty)) = sentence.split_once(':')
            {
                let ty = ty.split_whitespace().collect::<Vec<_>>().join(" ");
                for name in lexical_words(names).into_iter().skip(1) {
                    if valid_identifier(&name) {
                        let logical = modules
                            .iter()
                            .chain(std::iter::once(&name))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(".");
                        output.push((logical, ty.clone()));
                    }
                }
            }
            if let Some(event) = scope_event(&sentence) {
                match event {
                    ScopeEvent::Open(LexicalScope::Module(name)) => modules.push(name),
                    ScopeEvent::Close(name)
                        if modules
                            .last()
                            .is_some_and(|last| name.is_empty() || *last == name) =>
                    {
                        modules.pop();
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(output)
}

/// Re-checks every explicit baseline assumption in a disposable project copy.
/// The source scanner only finds declaration boundaries; native `Check` is the
/// authority for the stored kernel type. Failure is fail-closed and never
/// mutates the user's project.
fn native_normalize_axioms(
    project: &Path,
    axioms: Vec<(String, String)>,
    timeout: Duration,
) -> Result<Vec<(String, String)>> {
    if axioms.is_empty() {
        return Ok(axioms);
    }
    let stage = tempfile::tempdir().map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "cannot create trust staging area",
        )
    })?;
    let staged_project = publication::copy_project(project, stage.path()).map_err(|error| {
        error.into_user_or(
            ErrorKind::InvalidConfiguration,
            "trust staging storage is unavailable",
        )
    })?;
    let layout = layout::Layout::load(&staged_project, &[])?;
    let mut remaining = axioms.clone();
    let mut normalized = BTreeMap::<String, String>::new();
    for file in layout.files() {
        let original = read_source(&file)?;
        let mut checks = String::new();
        let mut matched = Vec::new();
        for (name, _) in &remaining {
            let leaf = name.rsplit('.').next().unwrap_or(name);
            if original.contains(&format!("Axiom {leaf}"))
                || original.contains(&format!("Parameter {leaf}"))
                || original.contains(&format!("Conjecture {leaf}"))
            {
                checks.push_str(&format!("\nCheck {name}.\n"));
                matched.push(name.clone());
            }
        }
        if checks.is_empty() {
            continue;
        }
        fs::write(&file, format!("{original}{checks}")).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "trust staging write failed",
            )
        })?;
        let relative = file.strip_prefix(&staged_project).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "trust staging path invalid",
            )
        })?;
        let checked = if let Some((workspace, target)) =
            publication::dune_target(&staged_project, relative)
        {
            native_process::run(
                "dune",
                [std::ffi::OsString::from("build"), target.into_os_string()],
                &workspace,
                timeout,
            )
        } else {
            publication::native_build(&staged_project, relative, timeout).map_err(|error| {
                error.into_user_or(
                    ErrorKind::InvalidConfiguration,
                    "explicit baseline could not be compiled",
                )
            })?;
            let _ = fs::remove_file(file.with_extension("vo"));
            let mut args = vec![std::ffi::OsString::from("compile")];
            args.extend(layout::compiler_options(&staged_project)?);
            args.push(relative.as_os_str().to_owned());
            native_process::run("rocq", args, &staged_project, timeout)
        }
        .map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "native baseline type check unavailable",
            )
        })?;
        if checked.timed_out || checked.overflow || !checked.status.success() {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "native baseline type check failed",
            ));
        }
        let report = format!(
            "{}{}",
            String::from_utf8_lossy(&checked.stdout),
            String::from_utf8_lossy(&checked.stderr)
        );
        for name in &matched {
            if let Some(ty) = checked_type(&report, name) {
                normalized.insert(name.clone(), ty);
            }
        }
        for name in matched {
            remaining.retain(|(candidate, _)| candidate != &name);
        }
    }
    if !remaining.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "explicit baseline assumption could not be natively resolved",
        ));
    }
    if normalized.len() != axioms.len() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "native baseline type report was incomplete",
        ));
    }
    Ok(axioms
        .into_iter()
        .map(|(name, ty)| (name.clone(), normalized.remove(&name).unwrap_or(ty)))
        .collect())
}

fn checked_type(report: &str, name: &str) -> Option<String> {
    let lines = report.lines().collect::<Vec<_>>();
    let marker = lines.iter().position(|line| line.trim() == name)?;
    let mut ty = String::new();
    for line in lines.into_iter().skip(marker + 1) {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(':') {
            ty.push_str(rest.trim());
        } else if !ty.is_empty() {
            ty.push(' ');
            ty.push_str(line);
        }
    }
    (!ty.is_empty()).then_some(ty)
}

pub(crate) fn solved_candidate(
    project: &Path,
    root: OpenDeclaration,
    commands: Vec<CanonicalTactic>,
    timeout: Duration,
) -> Result<SolvedCandidate> {
    let baseline = frozen_baseline(project, &root.identity, timeout)?;
    SolvedCandidate::new(root, commands, baseline).map_err(repository_error)
}

pub(crate) fn repository_error(error: RepositoryError) -> Error {
    match error {
        RepositoryError::Duplicate => Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration already has a retained proof",
        ),
        RepositoryError::Invalid => {
            Error::new(ErrorKind::InvalidRequest, "proof candidate is invalid")
        }
        RepositoryError::IllegalTransition | RepositoryError::Corrupt => Error::new(
            ErrorKind::InvalidConfiguration,
            "durable proof state is corrupt",
        ),
        RepositoryError::Oversize => Error::new(
            ErrorKind::InvalidRequest,
            format!("durable candidate error: {error:?}"),
        ),
        RepositoryError::Storage => Error::new(
            ErrorKind::InvalidConfiguration,
            "durable proof storage is unavailable",
        ),
    }
}

pub(crate) fn toolchain_identity(project: &Path, timeout: Duration) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    for executable in ["pet", "rocq"] {
        let path = executable_path(executable).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Rocq toolchain executable is unavailable",
            )
        })?;
        let metadata = fs::metadata(&path).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Rocq toolchain executable is unavailable",
            )
        })?;
        if metadata.len() > 128 * 1024 * 1024 {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Rocq toolchain executable is oversized",
            ));
        }
        let mut file = fs::File::open(&path).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Rocq toolchain executable is unavailable",
            )
        })?;
        digest.update(executable.as_bytes());
        digest.update(metadata.len().to_le_bytes());
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let count = file.read(&mut buffer).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "Rocq toolchain executable is unavailable",
                )
            })?;
            if count == 0 {
                break;
            }
            total += count as u64;
            digest.update(&buffer[..count]);
        }
        if total != metadata.len() {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "toolchain changed during identity capture",
            ));
        }
        let version = native_process::run(
            executable,
            [std::ffi::OsString::from("--version")],
            project,
            timeout,
        )
        .map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "toolchain version unavailable",
            )
        })?;
        if version.timed_out {
            return Err(Error::new(
                ErrorKind::ProofTimeout,
                "toolchain version timed out",
            ));
        }
        if version.overflow {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "toolchain version output exceeded its bound",
            ));
        }
        if !version.status.success() {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "toolchain version failed",
            ));
        }
        digest.update((version.stdout.len() as u64).to_le_bytes());
        digest.update(&version.stdout);
        digest.update((version.stderr.len() as u64).to_le_bytes());
        digest.update(&version.stderr);
    }
    Ok(digest.finalize().into())
}

fn executable_path(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .and_then(|path| fs::canonicalize(path).ok())
}

impl Engine {
    /// Linearize one goals-clear branch, durably bind its immutable tactic
    /// prefix, and retire every competing branch. Source publication is a
    /// later transaction phase; success here means `Pending`.
    pub(crate) fn close_solved(
        &self,
        attempt: AttemptId,
        native: &pet_runtime::PetState,
    ) -> Result<Option<(ProofState, Option<Error>)>> {
        if !(native.proof_finished && native.goals.all_clear()) {
            return Ok(None);
        }
        let project = self.attempt_project(attempt)?;
        let attachment = self.attach_project(&project)?;
        let _gate = attachment.write();
        let selected = self.forest.inspect(attempt.0).map_err(trace_error)?;
        let root = selected.root().clone();
        let accepted_commands = selected.actions().len();
        let mut persisted = None;
        let outcome = self.forest.close(attempt.0, |view| {
            let candidate = solved_candidate(
                &project,
                view.root().clone(),
                view.actions().to_vec(),
                self.config.operation_timeout,
            )?;
            match attachment.persist_candidate(candidate.clone()) {
                Ok(record) => {
                    persisted = Some(record);
                    Ok(())
                }
                Err(RepositoryError::Duplicate) => {
                    let existing = attachment
                        .pending(&candidate.declaration.identity)
                        .or_else(|| {
                            attachment
                                .recover_pending(&candidate.declaration.identity)
                                .ok()
                                .flatten()
                        })
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::InvalidConfiguration,
                                "durable proof record is unreadable",
                            )
                        })?;
                    let same = match &existing.phase {
                        ProofPhase::Solved(stored) => *stored == candidate,
                        ProofPhase::Closed(closed) => closed.candidate == candidate,
                        ProofPhase::Rejected(rejected) => rejected.candidate() == &candidate,
                    };
                    if !same {
                        // Design note: one theorem family retains the first
                        // durable solved branch. A racing branch adopts that
                        // record; choosing a different proof is not a user
                        // conflict.
                        persisted = Some(existing);
                        return Ok(());
                    }
                    persisted = Some(existing);
                    Ok(())
                }
                Err(error) => Err(repository_error(error)),
            }
        });
        match outcome {
            Ok(trace_forest::CloseOutcome::Closed)
            | Ok(trace_forest::CloseOutcome::AlreadyRetired) => {}
            Err(trace_forest::CallError::Callback(error)) => return Err(error),
            Err(trace_forest::CallError::Forest(error)) => return Err(trace_error(error)),
        }
        let record = persisted
            .or_else(|| attachment.pending(&root.identity))
            .or_else(|| attachment.recover_pending(&root.identity).ok().flatten())
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    "the completed proof was already published",
                )
            })?;
        let candidate = match &record.phase {
            ProofPhase::Solved(candidate) => candidate.clone(),
            ProofPhase::Closed(closed) => closed.candidate.clone(),
            ProofPhase::Rejected(rejected) => {
                let mut state = self.state_from_pet(
                    attempt,
                    &root,
                    native,
                    ProofLifecycle::Rejected,
                    accepted_commands,
                );
                state.attempt = None;
                state.recovery = Some(RecoveredProof::Rejected {
                    candidate: rejected.candidate().clone(),
                    phase: rejected.phase(),
                    diagnostics: rejected.diagnostics().to_vec(),
                });
                return Ok(Some((state, None)));
            }
        };
        // Design note: close is the only source boundary. Publication owns
        // staging, native validation, atomic replacement and repository ack.
        let publication = publication::publish(self, &project, &attachment, &candidate);
        self.pet_runtime.detach(&project);
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&attempt.0);
        let mut state = self.state_from_pet(
            attempt,
            &root,
            native,
            ProofLifecycle::Pending,
            accepted_commands,
        );
        state.attempt = None;
        let latest = attachment.pending(&root.identity).unwrap_or(record);
        state.recovery = match latest.phase {
            ProofPhase::Solved(candidate) => Some(RecoveredProof::Solved { candidate }),
            ProofPhase::Rejected(rejection) => Some(RecoveredProof::Rejected {
                candidate: rejection.candidate().clone(),
                phase: rejection.phase(),
                diagnostics: rejection.diagnostics().to_vec(),
            }),
            ProofPhase::Closed(_) => Some(RecoveredProof::Closed),
        };
        match publication {
            Ok(published) => Ok(Some((published, None))),
            Err(error) => {
                let rejected = matches!(state.recovery, Some(RecoveredProof::Rejected { .. }));
                state.lifecycle = if rejected {
                    ProofLifecycle::Rejected
                } else {
                    ProofLifecycle::Pending
                };
                state.theorem.status = state.lifecycle;
                Ok(Some((state, error.into_user())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_load_path_artifacts_are_content_identified() {
        let project = tempfile::tempdir().unwrap();
        let artifacts = external_artifacts(project.path(), Duration::from_secs(10)).unwrap();
        let (library, digest) = artifacts
            .iter()
            .find(|(library, _)| library.0 == ["Coq", "Init", "Logic"])
            .expect("standard Coq.Init.Logic artifact must be visible");
        assert_eq!(library.0, ["Coq", "Init", "Logic"]);
        assert_ne!(*digest, [0; 32]);
    }
}
