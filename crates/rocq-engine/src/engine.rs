//! Engine facade: project attachment and public lifecycle operations.

use super::*;

/// The semantic engine.  The forest stores only logical roots and canonical
/// actions; PET state is deliberately not represented here.
pub struct Engine {
    pub(crate) config: EngineConfig,
    pub(crate) forest: TraceForest<OpenDeclaration, CanonicalTactic>,
    pub(crate) registry: project_state::ProjectRegistry,
    pub(crate) pet_runtime: pet_runtime::PetRuntime,
    pub(crate) attempts: Mutex<HashMap<CursorId, PathBuf>>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Design note: native children are reaped while project attachments
        // and their cross-process locks are still alive.
        self.pet_runtime.shutdown();
    }
}

impl Engine {
    /// Attaches the project after waiting for a competing engine owner up to
    /// the configured operation deadline. Durable-state corruption is a
    /// project configuration error, never a lock/publication implementation
    /// error exposed to MCP.
    pub(crate) fn attach_project(
        &self,
        project: &Path,
    ) -> Result<std::sync::Arc<project_state::ProjectAttachment>> {
        let deadline = std::time::Instant::now()
            .checked_add(self.config.operation_timeout)
            .unwrap_or_else(std::time::Instant::now);
        loop {
            match self.registry.attach(project) {
                Ok(attachment) => return Ok(attachment),
                Err(project_state::AttachmentError::Contended)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(project_state::AttachmentError::Contended) => {
                    return Err(Error::new(
                        ErrorKind::ProjectTimeout,
                        "timed out waiting for exclusive project ownership",
                    ));
                }
                Err(project_state::AttachmentError::Recovery) => {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "durable proof state is corrupt",
                    ));
                }
                Err(project_state::AttachmentError::ProjectState) => {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        "project state directory is unavailable",
                    ));
                }
            }
        }
    }

    pub fn new(config: EngineConfig) -> Result<Self> {
        if !(1..=64).contains(&config.max_pet_processes) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "max PET processes must be between 1 and 64",
            ));
        }
        let forest = TraceForest::new(ForestConfig::new(
            config.trace_memory_bytes,
            &config.state_parent,
        ))
        .map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "trace state directory cannot be initialized",
            )
        })?;
        let pet_runtime = pet_runtime::PetRuntime::new(
            &config.state_parent,
            config.operation_timeout,
            config.max_pet_processes,
            config.runtime_cache_bytes,
        )
        .map_err(|error| Error::new(ErrorKind::InvalidConfiguration, error.to_string()))?;
        Ok(Self {
            config,
            forest,
            registry: project_state::ProjectRegistry::new(),
            pet_runtime,
            attempts: Mutex::new(HashMap::new()),
        })
    }

    /// Reads the current source view and merges durable recovery by identity.
    pub fn catalog(&self, project: &Path) -> Result<ProjectCatalog> {
        self.catalog_inner(project)
    }
    fn catalog_inner(&self, project: &Path) -> Result<ProjectCatalog> {
        let attachment = self.attach_project(project)?;
        let _gate = attachment.read();
        let pending = attachment.pending_records();
        discover(
            attachment.root(),
            self.owned_path(attachment.root()),
            &pending,
        )
    }

    /// Opens one parsed unfinished declaration or returns its recovered durable
    /// state.  Completed source declarations never create a trace.
    pub fn open(&self, project: &Path, identity: DeclarationIdentity) -> Result<ProofState> {
        self.open_inner(project, identity)
    }

    /// Resolve a public logical name or unambiguous suffix and open it.
    ///
    /// Invalid, missing, and ambiguous names return the same typed errors used
    /// by target queries; no resolution policy is delegated to transport code.
    pub fn open_named(&self, project: &Path, name: &str) -> Result<ProofState> {
        let catalog = self.catalog_inner(project)?;
        let identity = resolve_declaration(&catalog, name)?.identity.clone();
        self.open_inner(project, identity)
    }

    fn open_inner(&self, project: &Path, identity: DeclarationIdentity) -> Result<ProofState> {
        validate_identity(&identity)?;
        let attachment = self.attach_project(project)?;
        let _gate = attachment.read();
        let pending = attachment.pending_records();
        let catalog = discover(
            attachment.root(),
            self.owned_path(attachment.root()),
            &pending,
        )?;
        let info = catalog
            .declarations
            .iter()
            .find(|item| item.identity == identity)
            .cloned()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("declaration '{}' was not found", format_name(&identity)),
                )
            })?;

        if let Some(record) = pending.get(&identity) {
            if matches!(record.phase, ProofPhase::Solved(_) | ProofPhase::Closed(_)) {
                let phase = record.phase.clone();
                drop(_gate);
                let _write = attachment.write();
                let published = match phase {
                    ProofPhase::Solved(candidate) => {
                        publication::publish(self, attachment.root(), &attachment, &candidate)
                    }
                    ProofPhase::Closed(proof) => {
                        publication::publish_closed(self, attachment.root(), &attachment, &proof)
                    }
                    ProofPhase::Rejected(_) => return Ok(recovered_state(info, record)),
                };
                return match published {
                    Ok(state) => {
                        attachment.remove_pending(&identity);
                        self.pet_runtime.detach(attachment.root());
                        Ok(state)
                    }
                    Err(error) => match error.into_user() {
                        Some(error) => Err(error),
                        None => Ok(recovered_state(info, record)),
                    },
                };
            }
            return Ok(recovered_state(info, record));
        }
        if info.status == ProofLifecycle::Completed {
            return Ok(state(None, info, ProofLifecycle::Completed, 0, None));
        }
        let source = catalog.sources.get(&identity).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidDeclaration,
                "unfinished declaration has no source anchor",
            )
        })?;
        let open = OpenDeclaration {
            kind: source.info.kind,
            identity: source.info.identity.clone(),
            anchor: source.anchor.clone(),
        };
        let key = root_key(&open.identity)?;
        let cursor = self
            .forest
            .open(key, || Ok::<_, Error>(open.clone()))
            .map_err(trace_call_error)?;
        self.remember_attempt(cursor, attachment.root());
        // Design note: opening is an interactive operation, so its goals must
        // come from PET rather than a topology-only placeholder.
        let native = self.replay_state(attachment.root(), &open, &[])?;
        Ok(self.state_from_pet(AttemptId(cursor), &open, &native, ProofLifecycle::Open, 0))
    }

    /// Declares a logical theorem without editing source.  The layout and
    /// lexical context are resolved now and represented by a real digest anchor.
    pub fn declare(&self, project: &Path, declaration: NewDeclaration) -> Result<ProofState> {
        self.declare_inner(project, declaration)
    }
    fn declare_inner(&self, project: &Path, declaration: NewDeclaration) -> Result<ProofState> {
        validate_new_declaration(&declaration)?;
        let attachment = self.attach_project(project)?;
        let _gate = attachment.read();
        let pending = attachment.pending_records();
        let catalog = discover(
            attachment.root(),
            self.owned_path(attachment.root()),
            &pending,
        )?;
        if catalog
            .declarations
            .iter()
            .any(|item| item.identity == declaration.identity)
            || pending.contains_key(&declaration.identity)
        {
            return Err(Error::new(
                ErrorKind::InvalidDeclaration,
                "declaration identity is already present",
            ));
        }

        let layout = layout::Layout::load(attachment.root(), &[])?;
        let path = layout.target(&declaration.identity.library)?;
        if !path.starts_with(attachment.root()) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "declaration target is outside the project",
            ));
        }
        let source = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "declaration target is unavailable",
                ));
            }
        };
        let source_exists = path.exists();
        if source_exists {
            let occurrences =
                context_occurrences(&String::from_utf8_lossy(&source), &declaration.context)?;
            if occurrences == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidDeclaration,
                    "requested lexical context does not exist",
                ));
            }
            if occurrences > 1 {
                return Err(Error::new(
                    ErrorKind::Ambiguous,
                    "requested lexical context is ambiguous",
                ));
            }
        }
        if !source_exists {
            if !declaration.context.is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidDeclaration,
                    "a new source file cannot contain an existing lexical context",
                ));
            }
            if let Some(parent) = path.parent()
                && !parent.starts_with(attachment.root())
            {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "declaration target is outside the project",
                ));
            }
        }

        let source_digest = Sha256::digest(&source).into();
        let normalized_statement = new_declaration_header(&declaration)?;
        let anchor = SourceAnchor {
            source_digest,
            normalized_statement: normalized_statement.clone(),
            context: declaration.context.clone(),
            old_body_digest: Sha256::digest([]).into(),
        };
        let open = OpenDeclaration {
            kind: declaration.kind,
            identity: declaration.identity,
            anchor,
        };
        let cursor = self
            .forest
            .open(root_key(&open.identity)?, || Ok::<_, Error>(open.clone()))
            .map_err(trace_call_error)?;
        #[cfg(all(feature = "fault-injection", unix))]
        declare_race_barrier()?;
        self.remember_attempt(cursor, attachment.root());
        let native = self.replay_state(attachment.root(), &open, &[])?;
        Ok(self.state_from_pet(AttemptId(cursor), &open, &native, ProofLifecycle::Open, 0))
    }

    /// Replays a candidate prefix in PET before publishing the corresponding edge.
    /// A rejected native step is never inserted into the immutable forest.
    pub(crate) fn remember_attempt(&self, cursor: CursorId, project: &Path) {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(cursor, project.to_owned());
    }
    pub(crate) fn attempt_project(&self, attempt: AttemptId) -> Result<PathBuf> {
        self.attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&attempt.0)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "proof attempt is no longer open"))
    }
    pub(crate) fn replay_state(
        &self,
        project: &Path,
        root: &OpenDeclaration,
        actions: &[CanonicalTactic],
    ) -> Result<pet_runtime::PetState> {
        self.pet_runtime
            .replay(project, root, actions)
            .map_err(|e| self.pet_error(e))
    }
    pub(crate) fn pet_error(&self, error: pet_runtime::PetError) -> Error {
        let message = error.to_string();
        match error {
            pet_runtime::PetError::Timeout => Error::new(ErrorKind::ProofTimeout, message),
            pet_runtime::PetError::Remote { .. } => Error::new(ErrorKind::ProofStepFailed, message),
            pet_runtime::PetError::Stale
            | pet_runtime::PetError::Protocol(_)
            | pet_runtime::PetError::OutputOverflow => {
                Error::new(ErrorKind::InvalidConfiguration, message)
            }
            pet_runtime::PetError::Invalid(ref detail)
                if detail.contains("interface changed") || detail.contains("disappeared") =>
            {
                Error::new(ErrorKind::DeclarationChanged, message)
            }
            pet_runtime::PetError::Invalid(_) => {
                Error::new(ErrorKind::InvalidConfiguration, message)
            }
            pet_runtime::PetError::Environment(_) => {
                Error::new(ErrorKind::InvalidConfiguration, message)
            }
            // A process failure reaches this boundary only after the runtime's
            // fresh-process replay has failed. It is an operation failure, not
            // a new public catch-all class.
            pet_runtime::PetError::ProcessFailure(_) => {
                Error::new(ErrorKind::ProofTimeout, message)
            }
        }
    }
    pub(crate) fn state_from_pet(
        &self,
        attempt: AttemptId,
        root: &OpenDeclaration,
        native: &pet_runtime::PetState,
        lifecycle: ProofLifecycle,
        accepted_commands: usize,
    ) -> ProofState {
        let goals = &native.goals;
        let info = DeclarationInfo {
            identity: root.identity.clone(),
            context: root.anchor.context.clone(),
            kind: root.kind,
            statement: root.anchor.normalized_statement.clone(),
            status: lifecycle,
        };
        let text = render_pet_goals(goals);
        ProofState {
            attempt: Some(attempt),
            theorem: info,
            lifecycle,
            focused_goals: goals.focused.len(),
            unfocused_goals: goals.unfocused.len(),
            shelved_goals: goals.shelved.len(),
            given_up_goals: goals.given_up.len(),
            goals: text,
            accepted_commands,
            recovery: None,
        }
    }

    pub(crate) fn owned_path(&self, root: &Path) -> Vec<PathBuf> {
        let state = self.config.state_parent.as_path();
        if state.starts_with(root) && state != root {
            vec![state.to_owned()]
        } else {
            Vec::new()
        }
    }
}

/// Test-only barrier between source anchoring and PET document construction.
/// The external E2E fixture mutates its disposable source after receiving the
/// byte and acknowledges completion before replay resumes. Normal builds do
/// not contain this branch or expose any test command to MCP users.
#[cfg(all(feature = "fault-injection", unix))]
fn declare_race_barrier() -> Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let Ok(socket) = std::env::var("ROCQ_ENGINE_DECLARE_RACE_SOCKET") else {
        return Ok(());
    };
    let mut stream = UnixStream::connect(socket).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    let timeout = Some(Duration::from_secs(5));
    stream.set_read_timeout(timeout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    stream.set_write_timeout(timeout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    stream.write_all(&[1]).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    let mut acknowledged = [0u8; 1];
    stream.read_exact(&mut acknowledged).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    if acknowledged != [1] {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        ));
    }
    Ok(())
}

/// Render only PET's structured semantic fields; this is presentation, never
/// goal inference or console-output parsing.
fn render_pet_goals(goals: &pet_runtime::PetGoals) -> String {
    let mut output = String::new();
    for (label, collection) in [
        ("focused", &goals.focused),
        ("unfocused", &goals.unfocused),
        ("shelved", &goals.shelved),
        ("given_up", &goals.given_up),
    ] {
        output.push_str(label);
        output.push_str(":\n");
        for goal in collection {
            for hypothesis in &goal.hypotheses {
                output.push_str("  ");
                output.push_str(&hypothesis.names.join(" "));
                if let Some(definition) = &hypothesis.definition {
                    output.push_str(" := ");
                    output.push_str(definition);
                }
                output.push_str(" : ");
                output.push_str(&hypothesis.ty);
                output.push('\n');
            }
            output.push_str("  ============================\n  ");
            output.push_str(&goal.ty);
            output.push('\n');
        }
    }
    output
}

fn state(
    attempt: Option<AttemptId>,
    theorem: DeclarationInfo,
    lifecycle: ProofLifecycle,
    accepted_commands: usize,
    recovery: Option<RecoveredProof>,
) -> ProofState {
    ProofState {
        attempt,
        theorem,
        lifecycle,
        focused_goals: 0,
        unfocused_goals: 0,
        shelved_goals: 0,
        given_up_goals: 0,
        goals: String::new(),
        accepted_commands,
        recovery,
    }
}

fn recovered_state(info: DeclarationInfo, record: &PendingRecord) -> ProofState {
    let (lifecycle, recovery) = match &record.phase {
        ProofPhase::Solved(candidate) => (
            ProofLifecycle::Pending,
            RecoveredProof::Solved {
                candidate: candidate.clone(),
            },
        ),
        ProofPhase::Rejected(rejection) => (
            ProofLifecycle::Rejected,
            RecoveredProof::Rejected {
                candidate: rejection.candidate().clone(),
                phase: rejection.phase(),
                diagnostics: rejection.diagnostics().to_vec(),
            },
        ),
        ProofPhase::Closed(_) => (ProofLifecycle::Completed, RecoveredProof::Closed),
    };
    state(None, info, lifecycle, 0, Some(recovery))
}

pub(crate) fn validate_identity(identity: &DeclarationIdentity) -> Result<()> {
    if identity.library.0.is_empty()
        || !identity
            .library
            .0
            .iter()
            .chain(identity.modules.iter())
            .chain(std::iter::once(&identity.constant))
            .all(|part| valid_identifier(part))
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration identity is invalid",
        ));
    }
    if identity.modules.len() > 128 {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration identity is too deep",
        ));
    }
    Ok(())
}

pub(crate) fn trace_error(error: trace_forest::Error) -> Error {
    match error {
        trace_forest::Error::UnknownCursor
        | trace_forest::Error::Retired
        | trace_forest::Error::PayloadCodec
        | trace_forest::Error::CorruptSpill => {
            Error::new(ErrorKind::NotFound, "proof attempt is no longer open")
        }
        trace_forest::Error::Closing | trace_forest::Error::ConcurrentPreparationFailed => {
            Error::new(
                ErrorKind::ProofTimeout,
                "proof transition did not settle in time",
            )
        }
        trace_forest::Error::InvalidKey | trace_forest::Error::SpillUnavailable => Error::new(
            ErrorKind::InvalidConfiguration,
            "trace state storage is unavailable",
        ),
    }
}

fn trace_call_error(error: trace_forest::CallError<Error>) -> Error {
    match error {
        trace_forest::CallError::Callback(error) => error,
        trace_forest::CallError::Forest(error) => trace_error(error),
    }
}

fn validate_new_declaration(declaration: &NewDeclaration) -> Result<()> {
    validate_identity(&declaration.identity)?;
    let modules = declaration
        .context
        .iter()
        .filter_map(|scope| match scope {
            LexicalScope::Module(name) => Some(name),
            LexicalScope::Section(_) => None,
        })
        .collect::<Vec<_>>();
    if modules != declaration.identity.modules.iter().collect::<Vec<_>>() {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "module context does not match declaration identity",
        ));
    }
    if declaration.context.len() > 128
        || declaration
            .context
            .iter()
            .any(|scope| !valid_identifier(scope_name(scope)))
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "lexical context is invalid",
        ));
    }
    if declaration.statement.trim().is_empty() || declaration.statement.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration statement is empty or oversized",
        ));
    }
    let header = new_declaration_header(declaration)?;
    let Some((kind, name, _)) = declaration_header(&header) else {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration statement is not a supported header",
        ));
    };
    if kind != declaration.kind || name != declaration.identity.constant {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration statement identity does not match request",
        ));
    }
    Ok(())
}

fn new_declaration_header(declaration: &NewDeclaration) -> Result<String> {
    let body = normalize_fragment(&declaration.statement)?;
    if body.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration statement is empty",
        ));
    }
    let keyword = declaration_kind_keyword(declaration.kind);
    let prefix = format!("{keyword} {}", declaration.identity.constant);
    if body
        .split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case(keyword))
    {
        Ok(body.trim_end_matches('.').trim().to_owned())
    } else {
        Ok(format!("{prefix} : {body}"))
    }
}

fn declaration_kind_keyword(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Theorem => "Theorem",
        DeclarationKind::Lemma => "Lemma",
        DeclarationKind::Fact => "Fact",
        DeclarationKind::Remark => "Remark",
        DeclarationKind::Corollary => "Corollary",
        DeclarationKind::Proposition => "Proposition",
        DeclarationKind::Definition => "Definition",
    }
}

fn root_key(identity: &DeclarationIdentity) -> Result<RootKey> {
    let mut digest = Sha256::new();
    for part in identity
        .library
        .0
        .iter()
        .chain(identity.modules.iter())
        .chain(std::iter::once(&identity.constant))
    {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    Ok(RootKey::from_digest(digest.finalize().into()))
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64 * 1024
        && value
            .chars()
            .all(|x| x == '_' || x == '\'' || x.is_alphanumeric())
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64 * 1024
        || name.split('.').any(|part| !valid_identifier(part))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "query name is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn validate_native_fragment(fragment: &str) -> Result<()> {
    if fragment.trim().is_empty()
        || fragment.len() > 1024 * 1024
        || fragment.chars().any(char::is_control)
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "query expression is invalid",
        ));
    }
    let ranges = sentence_ranges(fragment).map_err(|_| {
        Error::new(
            ErrorKind::InvalidRequest,
            "query expression has unterminated syntax",
        )
    })?;
    if ranges.len() > 1 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "query expression contains multiple vernacular sentences",
        ));
    }
    Ok(())
}

pub(crate) fn validate_search(
    name: Option<&str>,
    statement: Option<&str>,
    limit: usize,
) -> Result<()> {
    if let Some(name) = name {
        validate_name(name)?;
    }
    if let Some(statement) = statement {
        validate_native_fragment(statement)?;
    }
    if limit > 10_000 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "query limit is too large",
        ));
    }
    Ok(())
}
