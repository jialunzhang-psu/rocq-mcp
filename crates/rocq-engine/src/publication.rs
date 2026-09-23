//! Close publication transaction.  Solved traces are durable before entering
//! this module; this module is the sole owner of source/build side effects.

use super::*;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;

/// Publication failures are retained as pending proof state and therefore do
/// not belong to the public engine error vocabulary.
#[derive(Debug)]
pub(crate) enum PublishError {
    User(Error),
    Deferred,
}
impl PublishError {
    fn user(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::User(Error::new(kind, message))
    }
    fn deferred() -> Self {
        Self::Deferred
    }
    fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
    pub(crate) fn into_user(self) -> Option<Error> {
        match self {
            Self::User(error) => Some(error),
            Self::Deferred => None,
        }
    }
    pub(crate) fn into_user_or(self, kind: ErrorKind, message: &'static str) -> Error {
        match self {
            Self::User(error) => error,
            Self::Deferred => Error::new(kind, message),
        }
    }
}
impl From<Error> for PublishError {
    fn from(error: Error) -> Self {
        Self::User(error)
    }
}
type PublishResult<T> = std::result::Result<T, PublishError>;

/// Repository transition failures during close remain owned by automatic
/// publication recovery. Only a deterministic project-size violation belongs
/// in the user contract.
fn publication_repository_error(error: RepositoryError) -> PublishError {
    match error {
        RepositoryError::Oversize => PublishError::user(
            ErrorKind::InvalidConfiguration,
            "publication record exceeds the supported project size",
        ),
        RepositoryError::Storage
        | RepositoryError::Corrupt
        | RepositoryError::Duplicate
        | RepositoryError::Invalid
        | RepositoryError::IllegalTransition => PublishError::deferred(),
    }
}

/// Abort at a named transaction boundary only in the explicit fault-injection
/// test build. Normal engine binaries compile this to a no-op and cannot be
/// crashed through an environment variable.
#[inline]
fn fault_point(name: &str) {
    #[cfg(not(feature = "fault-injection"))]
    let _ = name;
    #[cfg(feature = "fault-injection")]
    if std::env::var("ROCQ_ENGINE_FAULT_POINT").ok().as_deref() == Some(name) {
        std::process::abort();
    }
}

/// Publishes one solved candidate through staging, native validation and an
/// atomic source replacement.  Failure leaves the durable solved record intact.
pub(crate) fn publish(
    engine: &Engine,
    project: &Path,
    attachment: &project_state::ProjectAttachment,
    candidate: &SolvedCandidate,
) -> PublishResult<ProofState> {
    let _pending = attachment
        .pending(&candidate.declaration.identity)
        .or_else(|| {
            attachment
                .recover_pending(&candidate.declaration.identity)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            PublishError::user(
                ErrorKind::InvalidConfiguration,
                "durable solved proof record is unreadable",
            )
        })?;
    // Frozen baseline is checked before invoking a compiler so an unfinished
    // local can never be mistaken for an ordinary native validation failure.
    for forbidden in &candidate.baseline.forbidden_locals {
        if candidate.commands.iter().any(|command| {
            command
                .0
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word == forbidden)
        }) {
            reject(
                attachment,
                candidate,
                RejectionPhase::TrustAudit,
                "unfinished_dependency",
                "proof depends on an unfinished local declaration",
            )?;
            return Err(PublishError::user(
                ErrorKind::UnfinishedDependency,
                format!("proof depends on unfinished local declaration '{forbidden}'"),
            ));
        }
    }
    let replacements = replacements(project, candidate)?;
    let target = replacements
        .first()
        .map(FileReplacement::relative)
        .ok_or_else(PublishError::deferred)?;
    let stage = tempfile::tempdir().map_err(|_| PublishError::deferred())?;
    let staged_project = copy_project(project, stage.path())?;
    for replacement in &replacements {
        let target = staged_project.join(replacement.relative());
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_failure)?;
        }
        fs::write(&target, &replacement.contents).map_err(io_failure)?;
        sync_file(&target)?;
    }
    if let Err(error) = native_build(&staged_project, target, engine.config.close_timeout) {
        if error.is_deferred() && layout::dune_workspace_root(&staged_project).is_none() {
            reject(
                attachment,
                candidate,
                RejectionPhase::NativeValidation,
                "native_validation",
                "native validation rejected the proof",
            )?;
        }
        return Err(error);
    }
    if let Err(error) = native_trust_audit(
        &staged_project,
        target,
        candidate,
        engine.config.close_timeout,
    ) {
        match error {
            TrustAuditError::AxiomDependency(name) => {
                let message =
                    format!("axiom dependency is outside the authorized baseline: {name}");
                reject(
                    attachment,
                    candidate,
                    RejectionPhase::TrustAudit,
                    "axiom_dependency_out_of_scope",
                    &message,
                )?;
                return Err(PublishError::user(
                    ErrorKind::AxiomDependencyOutOfScope,
                    message,
                ));
            }
            TrustAuditError::BuildTimeout => {
                return Err(PublishError::user(
                    ErrorKind::BuildTimeout,
                    "trust audit timed out",
                ));
            }
            TrustAuditError::Deferred => return Err(PublishError::deferred()),
        }
    }

    let proof = ClosedProof::new(
        candidate.clone(),
        TrustAuditResult::ClosedUnderFrozenBaseline,
        replacements,
    )
    .map_err(publication_repository_error)?;
    // Design note: the complete replacement plan becomes durable before the
    // first source rename, making every crash window replayable.
    attachment
        .promote_candidate(&candidate.declaration.identity, proof.clone())
        .map_err(publication_repository_error)?;
    fault_point("after_promote");
    publish_closed(engine, project, attachment, &proof)
}

fn reject(
    attachment: &project_state::ProjectAttachment,
    candidate: &SolvedCandidate,
    phase: RejectionPhase,
    code: &str,
    message: &str,
) -> PublishResult<()> {
    let diagnostic = Diagnostic::new(code, message).map_err(publication_repository_error)?;
    let rejection = CandidateRejection::new(candidate.clone(), phase, vec![diagnostic])
        .map_err(publication_repository_error)?;
    attachment
        .reject_candidate(&candidate.declaration.identity, rejection)
        .map_err(publication_repository_error)?;
    Ok(())
}

/// Resumes a durable closed replacement plan. Existing new digests are
/// adopted idempotently; mismatched source is retained as a pending conflict.
pub(crate) fn publish_closed(
    engine: &Engine,
    project: &Path,
    attachment: &project_state::ProjectAttachment,
    proof: &ClosedProof,
) -> PublishResult<ProofState> {
    publish_closed_attempt(engine, project, attachment, proof, 0)
}

/// Apply a durable close plan. A compare-and-swap mismatch is handled by
/// rebuilding the plan against the latest source and retrying a bounded number
/// of times; it is not a user-facing publication conflict. If the declaration
/// itself changed, `replacements` returns `DeclarationChanged` instead.
fn publish_closed_attempt(
    engine: &Engine,
    project: &Path,
    attachment: &project_state::ProjectAttachment,
    proof: &ClosedProof,
    retries: u8,
) -> PublishResult<ProofState> {
    let originals = proof
        .replacements
        .iter()
        .map(|r| {
            let path = project.join(r.relative());
            Ok((path, r.old_contents().map(|bytes| bytes.to_vec())))
        })
        .collect::<PublishResult<Vec<_>>>()?;
    // Validate every compare-and-swap precondition before the first rename.
    fault_point("before_replace");
    for replacement in &proof.replacements {
        let path = project.join(replacement.relative());
        let current = fs::read(&path).ok();
        let digest = current
            .as_ref()
            .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
        if digest == Some(replacement.new_digest()) {
            continue;
        }
        if digest != replacement.old_digest() {
            if retries >= 3 {
                return Err(PublishError::deferred());
            }
            // Design note: reconstruct the replacement from the latest source
            // so unrelated editor changes are preserved. The declaration
            // identity/header/context is checked by `replacements`.
            let latest = replacements(project, &proof.candidate)?;
            let rebased = ClosedProof::new(proof.candidate.clone(), proof.audit, latest)
                .map_err(publication_repository_error)?;
            return publish_closed_attempt(engine, project, attachment, &rebased, retries + 1);
        }
    }
    for replacement in &proof.replacements {
        let path = project.join(replacement.relative());
        let current = fs::read(&path).ok();
        if current.as_ref().is_some_and(|bytes| {
            <[u8; 32]>::from(Sha256::digest(bytes)) == replacement.new_digest()
        }) {
            continue;
        }
        if let Err(error) = atomic_write(&path, &replacement.contents) {
            if restore_originals(&originals) {
                return Err(error);
            }
            return Err(PublishError::deferred());
        }
        fault_point("between_replacements");
    }
    fault_point("after_replace");
    let target = proof
        .replacements
        .first()
        .map(FileReplacement::relative)
        .ok_or_else(PublishError::deferred)?;
    if let Err(error) = native_build(project, target, engine.config.close_timeout) {
        if !restore_originals(&originals) {
            return Err(PublishError::deferred());
        }
        // Restore the derived view as well; otherwise callers could observe
        // old source paired with artifacts from the rejected publication.
        if rebuild_previous(project, proof, engine.config.close_timeout).is_err() {
            return Err(PublishError::deferred());
        }
        return Err(error);
    }
    fault_point("after_final_build");
    // A completed source/build view cannot coexist with PETs loaded from the
    // previous view. Teardown therefore precedes durable acknowledgement.
    engine.pet_runtime.detach(project);
    fault_point("before_ack");
    attachment
        .repository()
        .acknowledge_closed(&proof.candidate.declaration.identity)
        .map_err(publication_repository_error)?;
    attachment.remove_pending(&proof.candidate.declaration.identity);
    let info = DeclarationInfo {
        identity: proof.candidate.declaration.identity.clone(),
        context: proof.candidate.declaration.anchor.context.clone(),
        kind: proof.candidate.declaration.kind,
        statement: proof
            .candidate
            .declaration
            .anchor
            .normalized_statement
            .clone(),
        status: ProofLifecycle::Completed,
    };
    Ok(ProofState {
        attempt: None,
        theorem: info,
        lifecycle: ProofLifecycle::Completed,
        focused_goals: 0,
        unfocused_goals: 0,
        shelved_goals: 0,
        given_up_goals: 0,
        goals: String::new(),
        accepted_commands: proof.candidate.commands.len(),
        recovery: Some(RecoveredProof::Closed),
    })
}

fn restore_originals(originals: &[(PathBuf, Option<Vec<u8>>)]) -> bool {
    let mut success = true;
    for (path, old) in originals {
        let restored = match old {
            Some(bytes) => atomic_write(path, bytes).is_ok(),
            None => match fs::remove_file(path) {
                Ok(()) => path.parent().is_some_and(|parent| {
                    File::open(parent).and_then(|file| file.sync_all()).is_ok()
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => false,
            },
        };
        success &= restored;
    }
    success
}

fn rebuild_previous(project: &Path, proof: &ClosedProof, timeout: Duration) -> PublishResult<()> {
    let first = proof
        .replacements
        .first()
        .ok_or_else(PublishError::deferred)?;
    if first.old_digest().is_some() {
        return native_build(project, first.relative(), timeout);
    }
    if let Some(workspace) = layout::dune_workspace_root(project) {
        let output = native_process::run("dune", [OsString::from("build")], workspace, timeout)
            .map_err(io_failure)?;
        if output.timed_out {
            return Err(PublishError::user(
                ErrorKind::BuildTimeout,
                "rollback build timed out",
            ));
        }
        if !output.status.success() || output.overflow {
            return Err(PublishError::deferred());
        }
    }
    Ok(())
}

fn replacements(
    project: &Path,
    candidate: &SolvedCandidate,
) -> PublishResult<Vec<FileReplacement>> {
    let pending = BTreeMap::new();
    let catalog = discover(project, Vec::new(), &pending)?;
    if let Some(source) = catalog.sources.get(&candidate.declaration.identity) {
        let bytes = fs::read(&source.path).map_err(io_failure)?;
        if source.info.kind != candidate.declaration.kind
            || source.info.statement != candidate.declaration.anchor.normalized_statement
            || source.info.context != candidate.declaration.anchor.context
        {
            return Err(PublishError::user(
                ErrorKind::DeclarationChanged,
                "target declaration changed while proof was open",
            ));
        }
        let header = String::from_utf8_lossy(&bytes[source.range.start..source.header_end]);
        let mut body = header.to_string();
        if !body.ends_with(char::is_whitespace) {
            body.push('\n');
        }
        if candidate.declaration.kind != DeclarationKind::Definition {
            body.push_str("Proof.\n");
        }
        for command in &candidate.commands {
            body.push_str(&command.0);
            body.push('\n');
        }
        body.push_str(candidate.declaration.kind.terminator());
        body.push('\n');
        let mut contents = bytes[..source.range.start].to_vec();
        contents.extend_from_slice(body.as_bytes());
        contents.extend_from_slice(&bytes[source.range.end..]);
        return Ok(vec![
            FileReplacement::with_original(
                source
                    .path
                    .strip_prefix(project)
                    .map_err(|_| {
                        PublishError::user(
                            ErrorKind::InvalidConfiguration,
                            "source path escapes project",
                        )
                    })?
                    .to_path_buf(),
                Some(bytes.clone()),
                contents,
            )
            .map_err(publication_repository_error)?,
        ]);
    }
    let layout = layout::Layout::load(project, &[])?;
    let path = layout.target(&candidate.declaration.identity.library)?;
    let relative = path
        .strip_prefix(project)
        .map_err(|_| PublishError::user(ErrorKind::InvalidConfiguration, "target escapes project"))?
        .to_path_buf();
    let existed = path.exists();
    let existing = fs::read(&path).unwrap_or_default();
    if context_occurrences(
        &String::from_utf8_lossy(&existing),
        &candidate.declaration.anchor.context,
    )? != 1
    {
        return Err(PublishError::user(
            ErrorKind::DeclarationChanged,
            "declaration context is missing or ambiguous",
        ));
    }
    let insertion = context_insertion_offset(
        &String::from_utf8_lossy(&existing),
        &candidate.declaration.anchor.context,
    )?
    .ok_or_else(|| {
        PublishError::user(
            ErrorKind::DeclarationChanged,
            "declaration context disappeared",
        )
    })?;
    let mut inserted = String::new();
    if insertion == existing.len() && !existing.is_empty() && !existing.ends_with(b"\n") {
        inserted.push('\n');
    }
    if existing.is_empty() {
        for scope in &candidate.declaration.anchor.context {
            match scope {
                LexicalScope::Module(name) => inserted.push_str(&format!("Module {name}.\n")),
                LexicalScope::Section(name) => inserted.push_str(&format!("Section {name}.\n")),
            }
        }
    }
    inserted.push_str(&candidate.declaration.anchor.normalized_statement);
    inserted.push_str(".\n");
    if candidate.declaration.kind != DeclarationKind::Definition {
        inserted.push_str("Proof.\n");
    }
    for command in &candidate.commands {
        inserted.push_str(&command.0);
        inserted.push('\n');
    }
    inserted.push_str(candidate.declaration.kind.terminator());
    inserted.push('\n');
    if existing.is_empty() {
        for scope in candidate.declaration.anchor.context.iter().rev() {
            inserted.push_str(&format!(
                "End {}.\n",
                match scope {
                    LexicalScope::Module(n) | LexicalScope::Section(n) => n,
                }
            ));
        }
    }
    let mut body = existing[..insertion].to_vec();
    body.extend_from_slice(inserted.as_bytes());
    body.extend_from_slice(&existing[insertion..]);
    let old = existed.then(|| existing.clone());
    let source_replacement = FileReplacement::with_original(relative, old, body)
        .map_err(publication_repository_error)?;
    let mut replacements = vec![source_replacement];
    if !existed && let Some(metadata) = dune_modules_replacement(project, &path)? {
        replacements.push(metadata);
    }
    Ok(replacements)
}

/// Adds a newly-created compilation unit to the nearest explicit Dune modules
/// field. Projects using implicit `:standard` discovery need no metadata edit.
fn dune_modules_replacement(
    project: &Path,
    target: &Path,
) -> PublishResult<Option<FileReplacement>> {
    let module = target
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            PublishError::user(ErrorKind::InvalidConfiguration, "invalid module filename")
        })?;
    let mut directory = target.parent();
    while let Some(current) = directory {
        if !current.starts_with(project) {
            break;
        }
        let dune = current.join("dune");
        if dune.is_file() {
            let bytes = fs::read(&dune).map_err(io_failure)?;
            let source = std::str::from_utf8(&bytes).map_err(|_| {
                PublishError::user(ErrorKind::InvalidConfiguration, "Dune file is not UTF-8")
            })?;
            let Some((start, close)) = explicit_modules_range(source)? else {
                return Ok(None);
            };
            let field = &source[start..close];
            if field
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '\'')
                .any(|word| word == module)
            {
                return Ok(None);
            }
            let mut updated = bytes[..close].to_vec();
            updated.extend_from_slice(format!(" {module}").as_bytes());
            updated.extend_from_slice(&bytes[close..]);
            let relative = dune
                .strip_prefix(project)
                .map_err(|_| {
                    PublishError::user(ErrorKind::InvalidConfiguration, "Dune path escapes project")
                })?
                .to_path_buf();
            return FileReplacement::with_original(relative, Some(bytes), updated)
                .map(Some)
                .map_err(publication_repository_error);
        }
        if current == project {
            break;
        }
        directory = current.parent();
    }
    Ok(None)
}

/// Finds the close parenthesis of one unambiguous `(modules ...)` field while
/// respecting Dune strings and line comments.
fn explicit_modules_range(source: &str) -> PublishResult<Option<(usize, usize)>> {
    let bytes = source.as_bytes();
    let mut index = 0usize;
    let mut found = None;
    while index < bytes.len() {
        if bytes[index] == b';' {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |n| index + n + 1);
            continue;
        }
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
            }
            index = index.saturating_add(1);
            continue;
        }
        if bytes[index] == b'(' && source[index + 1..].starts_with("modules") {
            let after = index + 1 + "modules".len();
            if after < bytes.len() && (bytes[after].is_ascii_whitespace() || bytes[after] == b')') {
                let mut depth = 1usize;
                let mut cursor = after;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        b';' => {
                            cursor = source[cursor..]
                                .find('\n')
                                .map_or(bytes.len(), |n| cursor + n)
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                if depth != 0 {
                    return Err(PublishError::user(
                        ErrorKind::InvalidConfiguration,
                        "unterminated Dune modules field",
                    ));
                }
                if found.replace((after, cursor)).is_some() {
                    return Err(PublishError::user(
                        ErrorKind::Ambiguous,
                        "multiple Dune modules fields",
                    ));
                }
                index = cursor;
            }
        }
        index += 1;
    }
    Ok(found)
}

/// Copies a Dune workspace or non-Dune project to a disposable root and
/// returns the corresponding attached-project directory in the copy.
pub(crate) fn copy_project(source: &Path, target: &Path) -> PublishResult<PathBuf> {
    let workspace = layout::dune_workspace_root(source).unwrap_or(source);
    let relative_project = source
        .strip_prefix(workspace)
        .map_err(|_| PublishError::deferred())?;
    let build_dir = layout::dune_build_directory(source)?;
    let engine_state = project_state::excluded_state_tree(source)?;
    for entry in walkdir::WalkDir::new(workspace)
        .into_iter()
        .filter_entry(|entry| {
            !build_dir
                .as_ref()
                .is_some_and(|dir| entry.path().starts_with(dir))
                && !entry.path().starts_with(&engine_state)
        })
    {
        let entry = entry.map_err(|_| PublishError::deferred())?;
        let path = entry.path();
        let relative = path
            .strip_prefix(workspace)
            .map_err(|_| PublishError::deferred())?;
        let out = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&out).map_err(io_failure)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(io_failure)?;
            }
            fs::copy(path, out).map_err(io_failure)?;
        }
    }
    Ok(target.join(relative_project))
}

pub(crate) fn native_build(stage: &Path, target: &Path, timeout: Duration) -> PublishResult<()> {
    let Some((workspace, dune_target)) = dune_target(stage, target) else {
        return direct_native_build(stage, target, timeout);
    };
    // Building the target artifact asks Dune for precisely its dependency
    // closure, so unrelated pre-existing broken modules do not veto close.
    let output = native_process::run(
        "dune",
        [OsString::from("build"), dune_target.into_os_string()],
        &workspace,
        timeout,
    )
    .map_err(io_failure)?;
    if output.timed_out {
        Err(PublishError::user(
            ErrorKind::BuildTimeout,
            "native build timed out",
        ))
    } else if output.overflow {
        Err(PublishError::deferred())
    } else if output.status.success() {
        Ok(())
    } else {
        Err(PublishError::deferred())
    }
}

/// Maps a target relative to an attached project into its Dune workspace.
/// Dune owns build targets and load paths; callers do not synthesize `-R/-Q`.
pub(crate) fn dune_target(project: &Path, target: &Path) -> Option<(PathBuf, PathBuf)> {
    let workspace = layout::dune_workspace_root(project)?;
    let relative = project.strip_prefix(workspace).ok()?.join(target);
    Some((workspace.to_owned(), relative.with_extension("vo")))
}

/// Builds the target's local dependency closure in topological order using
/// Rocq's own dependency extractor. Unrelated project files are never compiled.
fn direct_native_build(stage: &Path, target: &Path, timeout: Duration) -> PublishResult<()> {
    let started = std::time::Instant::now();
    let layout = layout::Layout::load(stage, &[])?;
    let files = layout.files();
    let mut dep_args = vec![OsString::from("dep")];
    dep_args.extend(layout::compiler_options(stage)?);
    for file in &files {
        dep_args.push(
            file.strip_prefix(stage)
                .map_err(|_| {
                    PublishError::user(
                        ErrorKind::InvalidConfiguration,
                        "dependency path escapes project",
                    )
                })?
                .as_os_str()
                .to_owned(),
        );
    }
    let dependency = native_process::run("rocq", dep_args, stage, timeout).map_err(io_failure)?;
    if dependency.timed_out {
        return Err(PublishError::user(
            ErrorKind::BuildTimeout,
            "dependency analysis timed out",
        ));
    }
    if dependency.overflow || !dependency.status.success() {
        return Err(PublishError::deferred());
    }
    let graph = parse_dependency_graph(&String::from_utf8_lossy(&dependency.stdout));
    let target_vo = target.with_extension("vo").to_string_lossy().into_owned();
    let mut ordered = Vec::new();
    let mut visiting = BTreeSet::new();
    dependency_order(&target_vo, &graph, &mut visiting, &mut ordered)?;
    for vo in ordered {
        let source = PathBuf::from(vo).with_extension("v");
        let remaining = timeout
            .checked_sub(started.elapsed())
            .ok_or_else(|| PublishError::user(ErrorKind::BuildTimeout, "native build timed out"))?;
        let mut args = vec![OsString::from("compile")];
        args.extend(layout::compiler_options(stage)?);
        args.push(source.into_os_string());
        let output = native_process::run("rocq", args, stage, remaining).map_err(io_failure)?;
        if output.timed_out {
            return Err(PublishError::user(
                ErrorKind::BuildTimeout,
                "native build timed out",
            ));
        }
        if output.overflow || !output.status.success() {
            return Err(PublishError::deferred());
        }
    }
    Ok(())
}

fn parse_dependency_graph(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut graph = BTreeMap::new();
    for line in text.lines() {
        let Some((targets, dependencies)) = line.split_once(':') else {
            continue;
        };
        let Some(target) = targets
            .split_whitespace()
            .find(|word| word.ends_with(".vo"))
        else {
            continue;
        };
        graph.insert(
            target.to_owned(),
            dependencies
                .split_whitespace()
                .filter(|word| word.ends_with(".vo"))
                .map(str::to_owned)
                .collect(),
        );
    }
    graph
}

fn dependency_order(
    target: &str,
    graph: &BTreeMap<String, Vec<String>>,
    visiting: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> PublishResult<()> {
    if ordered.iter().any(|item| item == target) {
        return Ok(());
    }
    if !visiting.insert(target.to_owned()) {
        return Err(PublishError::user(
            ErrorKind::InvalidConfiguration,
            "cyclic Rocq dependency graph",
        ));
    }
    if let Some(dependencies) = graph.get(target) {
        for dependency in dependencies {
            if graph.contains_key(dependency) {
                dependency_order(dependency, graph, visiting, ordered)?;
            }
        }
    }
    visiting.remove(target);
    ordered.push(target.to_owned());
    Ok(())
}

/// Asks Rocq's kernel for the transitive assumption roots of the staged proof.
/// Only explicit assumptions frozen into the candidate baseline are accepted.
#[derive(Debug)]
enum TrustAuditError {
    AxiomDependency(String),
    BuildTimeout,
    Deferred,
}

fn native_trust_audit(
    stage: &Path,
    target: &Path,
    candidate: &SolvedCandidate,
    timeout: Duration,
) -> std::result::Result<(), TrustAuditError> {
    // Design note: the candidate freezes the executable/version identity when
    // solved; recovery must not silently re-audit it under a different
    // toolchain, even if all source-level assumptions still match.
    match candidate::toolchain_identity(stage, timeout) {
        Ok(identity) if identity == candidate.baseline.toolchain => {}
        Ok(_) => return Err(TrustAuditError::Deferred),
        Err(error) if error.kind == ErrorKind::ProofTimeout => {
            return Err(TrustAuditError::BuildTimeout);
        }
        Err(_) => return Err(TrustAuditError::Deferred),
    }
    let path = stage.join(target);
    let mut source = fs::read(&path).map_err(|_| TrustAuditError::Deferred)?;
    source.extend_from_slice(
        format!(
            "\nPrint Assumptions {}.\n",
            local_name(&candidate.declaration.identity)
        )
        .as_bytes(),
    );
    fs::write(&path, source).map_err(|_| TrustAuditError::Deferred)?;
    let (program, args, working_dir) =
        if let Some((workspace, dune_target)) = dune_target(stage, target) {
            (
                "dune",
                vec![OsString::from("build"), dune_target.into_os_string()],
                workspace,
            )
        } else {
            let mut args = vec![OsString::from("compile")];
            args.extend(layout::compiler_options(stage).map_err(|_| TrustAuditError::Deferred)?);
            args.push(target.as_os_str().to_owned());
            ("rocq", args, stage.to_owned())
        };
    let output = native_process::run(program, args, &working_dir, timeout)
        .map_err(|_| TrustAuditError::Deferred)?;
    if output.timed_out {
        return Err(TrustAuditError::BuildTimeout);
    }
    if output.overflow {
        return Err(TrustAuditError::Deferred);
    }
    if !output.status.success() {
        return Err(TrustAuditError::Deferred);
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if text.contains("Closed under the global context") {
        return Ok(());
    }
    let allowed = &candidate.baseline.explicit_axioms;
    let mut saw_header = false;
    let mut assumptions = Vec::<(String, String)>::new();
    for raw in text.lines() {
        let continuation = raw.starts_with(char::is_whitespace);
        let line = raw.trim();
        if line == "Axioms:" {
            saw_header = true;
            continue;
        }
        if !saw_header || line.is_empty() {
            continue;
        }
        if !continuation && let Some((name, ty)) = line.split_once(':') {
            let name = name.trim();
            if validate_name(name).is_ok() {
                assumptions.push((
                    name.to_owned(),
                    ty.split_whitespace().collect::<Vec<_>>().join(" "),
                ));
            }
        } else if let Some((_, ty)) = assumptions.last_mut() {
            if !ty.is_empty() {
                ty.push(' ');
            }
            ty.push_str(&line.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    if !saw_header {
        return Err(TrustAuditError::Deferred);
    }
    for (name, ty) in assumptions {
        if allowed.iter().any(|(allowed_name, allowed_type)| {
            (name == *allowed_name || name.ends_with(&format!(".{allowed_name}")))
                && ty == *allowed_type
        }) {
            continue;
        }
        match external_assumption_authorized(stage, target, &name, candidate, timeout) {
            ExternalAuthorization::Authorized => continue,
            ExternalAuthorization::Changed => return Err(TrustAuditError::Deferred),
            ExternalAuthorization::Internal => return Err(TrustAuditError::Deferred),
            ExternalAuthorization::OutOfScope => {
                return Err(TrustAuditError::AxiomDependency(name));
            }
        }
    }
    Ok(())
}

/// Matches a native assumption root to the frozen compiled library identity.
/// The root is accepted only when its owning logical library has an exact
/// content digest in the candidate baseline; names alone are insufficient.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExternalAuthorization {
    Authorized,
    Changed,
    OutOfScope,
    Internal,
}

fn external_assumption_authorized(
    stage: &Path,
    target: &Path,
    name: &str,
    candidate: &SolvedCandidate,
    timeout: Duration,
) -> ExternalAuthorization {
    if candidate
        .baseline
        .forbidden_locals
        .iter()
        .any(|local| name == local || name.ends_with(&format!(".{local}")))
    {
        return ExternalAuthorization::OutOfScope;
    }
    let Some(declared_library) = native_declared_library(stage, target, name, timeout) else {
        return ExternalAuthorization::Internal;
    };
    let Ok(root) = native_process::run(
        "rocq",
        [OsString::from("compile"), OsString::from("-where")],
        stage,
        timeout,
    ) else {
        return ExternalAuthorization::Internal;
    };
    if root.timed_out || root.overflow || !root.status.success() {
        return ExternalAuthorization::Internal;
    }
    let root = PathBuf::from(String::from_utf8_lossy(&root.stdout).trim());
    let mut roots = vec![(root, None)];
    let Ok(external) = layout::external_load_paths(stage) else {
        return ExternalAuthorization::Internal;
    };
    roots.extend(
        external
            .into_iter()
            .map(|(path, prefix)| (path, Some(prefix))),
    );
    let Some((library, baseline_digest)) = candidate
        .baseline
        .external_artifacts
        .iter()
        .find(|(library, _)| library.0.join(".") == declared_library)
    else {
        return ExternalAuthorization::OutOfScope;
    };
    for (root, configured_prefix) in &roots {
        let Some(relative) = artifact_relative_path(&library.0, configured_prefix.as_ref()) else {
            continue;
        };
        let path = root.join(relative).with_extension("vo");
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        let digest = <[u8; 32]>::from(Sha256::digest(bytes));
        return if digest == *baseline_digest {
            ExternalAuthorization::Authorized
        } else {
            ExternalAuthorization::Changed
        };
    }
    ExternalAuthorization::Internal
}

fn artifact_relative_path(
    library: &[String],
    configured_prefix: Option<&LogicalLibrary>,
) -> Option<PathBuf> {
    if let Some(prefix) = configured_prefix {
        if !library.starts_with(&prefix.0) {
            return None;
        }
        let mut path = PathBuf::new();
        for component in library.iter().skip(prefix.0.len()) {
            path.push(component);
        }
        return Some(path);
    }
    if library.first().is_some_and(|x| x == "Coq") {
        let mut path = PathBuf::from("theories");
        for component in library.iter().skip(1) {
            path.push(component);
        }
        return Some(path);
    }
    if !library.is_empty() {
        let mut path = PathBuf::from("user-contrib");
        for component in library {
            path.push(component);
        }
        return Some(path);
    }
    None
}

/// Resolves an assumption constant through Rocq itself.  `Print Assumptions`
/// intentionally reports the short constant name, so textual prefix matching
/// cannot identify the owning external library (for example `classic` is
/// declared in `Stdlib.Logic.Classical_Prop`).
fn native_declared_library(
    stage: &Path,
    target: &Path,
    name: &str,
    timeout: Duration,
) -> Option<String> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '.'))
    {
        return None;
    }
    let path = stage.join(target);
    let mut source = fs::read(&path).ok()?;
    source.extend_from_slice(format!("\nAbout {name}.\n").as_bytes());
    fs::write(&path, source).ok()?;
    // Rocq may consider an existing staged `.vo` current and skip the source;
    // remove only derived staging products so `About` is guaranteed to run.
    let _ = fs::remove_file(path.with_extension("vo"));
    let _ = fs::remove_file(path.with_extension("glob"));
    let (program, args, working_dir) =
        if let Some((workspace, dune_target)) = dune_target(stage, target) {
            (
                "dune",
                vec![OsString::from("build"), dune_target.into_os_string()],
                workspace,
            )
        } else {
            let mut args = vec![OsString::from("compile")];
            args.extend(layout::compiler_options(stage).ok()?);
            args.push(target.as_os_str().to_owned());
            ("rocq", args, stage.to_owned())
        };
    let output = native_process::run(program, args, &working_dir, timeout).ok()?;
    if output.timed_out || output.overflow || !output.status.success() {
        return None;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Declared in library "))
        .map(|library| library.split_once(',').map_or(library, |(name, _)| name))
        .map(str::trim)
        .filter(|library| !library.is_empty())
        .map(str::to_owned)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> PublishResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io_failure(std::io::Error::other("no parent")))?;
    if !parent.exists() {
        fs::create_dir_all(parent).map_err(io_failure)?;
        // Persist the newly-created directory chain before publishing a file
        // into it; durable proof metadata can then recover either state.
        File::open(parent)
            .map_err(io_failure)?
            .sync_all()
            .map_err(io_failure)?;
    }
    let tmp = parent.join(format!(".rocq-engine-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> PublishResult<()> {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(io_failure)?;
        f.write_all(bytes).map_err(io_failure)?;
        f.sync_all().map_err(io_failure)?;
        fs::rename(&tmp, path).map_err(io_failure)?;
        File::open(parent)
            .map_err(io_failure)?
            .sync_all()
            .map_err(io_failure)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn sync_file(path: &Path) -> PublishResult<()> {
    File::open(path)
        .map_err(io_failure)?
        .sync_all()
        .map_err(io_failure)
}
fn io_failure(_: std::io::Error) -> PublishError {
    PublishError::deferred()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_publication_is_state_not_a_public_error() {
        assert!(PublishError::deferred().into_user().is_none());
        let error = PublishError::user(ErrorKind::BuildTimeout, "deadline")
            .into_user()
            .expect("a declared user error remains public");
        assert_eq!(error.kind, ErrorKind::BuildTimeout);
    }
}
