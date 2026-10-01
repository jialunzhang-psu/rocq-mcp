//! Process-wide project runtimes and per-connection MCP sessions.

use crate::{
    adapter::{dispatch, public_error},
    checkpoint::CheckpointBook,
    schema::tool_definitions,
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation,
        InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, RequestMetaObject, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
};
use rocq_engine::{
    ArtifactFingerprint, DuneProject, Engine, Error, ErrorKind, FileId, PetActor,
    RequestCancellation, with_request_cancellation,
};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

const CODEX_SANDBOX_STATE_META: &str = "codex/sandbox-state-meta";
const CODEX_SANDBOX_CWD: &str = "sandboxCwd";

/// The sole live PET actor and operation barrier for one canonical Dune
/// project. Serializing whole operations is intentional: PET itself is
/// serialized, and it makes publication invalidation atomic across sessions.
pub(crate) struct ProjectRuntime {
    project: DuneProject,
    actor: PetActor,
    operation: Mutex<()>,
    /// Last source/consumer identity coordinated with PET. Access is always
    /// made while `operation` is held; a dedicated mutex keeps the invariant
    /// explicit and makes diagnostic/test reads harmless.
    artifacts: Mutex<BTreeMap<FileId, ArtifactFingerprint>>,
}

impl ProjectRuntime {
    pub(crate) fn project(&self) -> &DuneProject {
        &self.project
    }
    pub(crate) fn actor(&self) -> &PetActor {
        &self.actor
    }
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn artifact(&self, file: &FileId) -> Option<ArtifactFingerprint> {
        self.artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(file)
            .cloned()
    }

    pub(crate) fn record_artifact(&self, file: FileId, fingerprint: ArtifactFingerprint) {
        self.artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(file, fingerprint);
    }

    pub(crate) fn forget_artifact(&self, file: &FileId) {
        self.artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(file);
    }

    pub(crate) fn clear_artifacts(&self) {
        self.artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

/// Mutable state owned by exactly one MCP connection.
#[derive(Default)]
pub(crate) struct Selection {
    pub(crate) project: Option<Arc<ProjectRuntime>>,
    pub(crate) checkpoints: CheckpointBook,
}

/// A connection cell is shared by clones of one `rmcp` handler. Its `Drop`
/// runs only after the last clone and releases every checkpoint-owned PET ID.
pub(crate) struct SessionCell {
    pub(crate) selection: Mutex<Selection>,
    progress: Mutex<ProgressBook>,
}

/// Last pollable operation record for one MCP connection.  The progress mutex
/// is deliberately independent of the selection/project barriers so a query
/// can observe a long Dune build or PET replay while that operation owns both
/// of the latter locks.
struct ProgressBook {
    generation: u64,
    record: Option<ProgressRecord>,
}

struct ProgressRecord {
    operation: String,
    status: &'static str,
    phase: String,
    completed: u64,
    total: Option<u64>,
    target: Option<Value>,
    log_summary: String,
    started: Instant,
    terminal_elapsed: Option<Duration>,
}

impl SessionCell {
    fn new() -> Self {
        Self {
            selection: Mutex::new(Selection::default()),
            progress: Mutex::new(ProgressBook {
                generation: 0,
                record: None,
            }),
        }
    }

    pub(crate) fn project(&self) -> Option<Arc<ProjectRuntime>> {
        self.selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project
            .clone()
    }

    /// Begin a new observable operation and retain its generation even after
    /// completion so a fast operation cannot disappear between polls.
    pub(crate) fn begin_progress(&self, operation: &str, target: Option<Value>) -> u64 {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        progress.generation = progress.generation.saturating_add(1);
        progress.record = Some(ProgressRecord {
            operation: operation.to_owned(),
            status: "running",
            phase: "admitted".into(),
            completed: 0,
            total: None,
            target,
            log_summary: String::new(),
            started: Instant::now(),
            terminal_elapsed: None,
        });
        progress.generation
    }

    /// Update the currently running generation.  Late updates from a prior
    /// worker are ignored rather than corrupting a newer operation's record.
    pub(crate) fn update_progress(
        &self,
        generation: u64,
        phase: &str,
        completed: u64,
        total: Option<u64>,
        log_summary: impl AsRef<str>,
    ) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if progress.generation != generation {
            return;
        }
        let Some(record) = progress.record.as_mut() else {
            return;
        };
        if record.status != "running" {
            return;
        }
        record.phase = phase.to_owned();
        record.completed = completed;
        record.total = total;
        let log = log_summary.as_ref();
        if !log.is_empty() {
            record.log_summary = bounded_log(log);
        }
    }

    pub(crate) fn update_current_progress(
        &self,
        phase: &str,
        completed: u64,
        total: Option<u64>,
        log_summary: impl AsRef<str>,
    ) {
        let generation = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation;
        self.update_progress(generation, phase, completed, total, log_summary);
    }

    pub(crate) fn set_progress_target(&self, target: Value) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = progress.record.as_mut()
            && record.status == "running"
        {
            record.target = Some(target);
        }
    }

    /// Mark one generation terminal while retaining its bounded summary for
    /// later polling.  No notification is emitted.
    pub(crate) fn finish_progress(&self, generation: u64, result: &Result<Value, Error>) {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if progress.generation != generation {
            return;
        }
        let Some(record) = progress.record.as_mut() else {
            return;
        };
        record.terminal_elapsed = Some(record.started.elapsed());
        match result {
            Ok(value) => {
                if let Some(error) = value.get("error") {
                    record.status = "failed";
                    record.phase = "failed".into();
                    if let Some(message) = error.get("message").and_then(Value::as_str) {
                        record.log_summary = bounded_log(message);
                    }
                } else {
                    record.status = "completed";
                    record.phase = "completed".into();
                    if let Some(total) = record.total {
                        record.completed = total;
                    }
                }
            }
            Err(error) => {
                record.status = if error.kind == ErrorKind::RequestCancelled {
                    "cancelled"
                } else {
                    "failed"
                };
                record.phase = record.status.into();
                record.log_summary = bounded_log(&error.message);
            }
        }
    }

    /// Return an instantaneous, read-only snapshot.  This method takes no
    /// admission, project, selection, or PET lock and is therefore safe to use
    /// as the polling endpoint for a concurrently running operation.
    pub(crate) fn progress_json(&self, observed_generation: Option<u64>) -> Value {
        let progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = &progress.record else {
            return json!({
                "generation": progress.generation,
                "status": "idle",
                "changed": observed_generation != Some(progress.generation),
            });
        };
        let elapsed = record
            .terminal_elapsed
            .unwrap_or_else(|| record.started.elapsed())
            .as_millis();
        let elapsed_ms = u64::try_from(elapsed).unwrap_or(u64::MAX);
        let mut value = json!({
            "generation": progress.generation,
            "operation": record.operation,
            "status": record.status,
            "phase": record.phase,
            "completed": record.completed,
            "elapsed_ms": elapsed_ms,
            "changed": observed_generation != Some(progress.generation),
        });
        if let Some(total) = record.total {
            value["total"] = json!(total);
        }
        if let Some(target) = &record.target {
            value["target"] = target.clone();
        }
        if !record.log_summary.is_empty() {
            value["log_summary"] = json!(record.log_summary);
        }
        value
    }
}

fn bounded_log(value: &str) -> String {
    const LIMIT: usize = 4096;
    if value.len() <= LIMIT {
        return value.to_owned();
    }
    let mut end = LIMIT;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

impl Drop for SessionCell {
    fn drop(&mut self) {
        let selection = self
            .selection
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(project) = selection.project.clone() else {
            return;
        };
        let _operation = project.lock();
        if let Some(proof) = selection.checkpoints.clear() {
            let states = proof
                .checkpoints
                .values()
                .filter_map(|checkpoint| checkpoint.pet_state)
                .collect::<Vec<_>>();
            // A lost child has already released all of its IDs. Drop cannot
            // report an error, so exact release is best-effort only here.
            let _ = project.actor.release_states(&states);
        }
    }
}

/// Process-wide owner of project sharing and connection discovery. The maps
/// contain weak references so detached projects and closed sessions shut down
/// without an eviction policy.
pub struct ServerRuntime {
    pub(crate) engine: Arc<Engine>,
    projects: Mutex<BTreeMap<PathBuf, Weak<ProjectRuntime>>>,
    sessions: Mutex<Vec<Weak<SessionCell>>>,
}

impl ServerRuntime {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            projects: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(Vec::new()),
        }
    }

    /// Create one independent connection session backed by this shared
    /// process-wide runtime.
    pub fn connection(self: &Arc<Self>) -> RocqServer {
        let session = Arc::new(SessionCell::new());
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::downgrade(&session));
        RocqServer {
            runtime: Arc::clone(self),
            session,
            admission: Arc::new(AsyncMutex::new(())),
        }
    }

    /// Return the one runtime for a Dune project, creating it on first attach.
    pub(crate) fn project(&self, project: DuneProject) -> Arc<ProjectRuntime> {
        let key = project.id().to_owned();
        let mut projects = self
            .projects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        projects.retain(|_, runtime| runtime.strong_count() != 0);
        if let Some(runtime) = projects.get(&key).and_then(Weak::upgrade) {
            return runtime;
        }
        let runtime = Arc::new(ProjectRuntime {
            project,
            actor: PetActor::new(),
            operation: Mutex::new(()),
            artifacts: Mutex::new(BTreeMap::new()),
        });
        projects.insert(key, Arc::downgrade(&runtime));
        runtime
    }

    /// Clear every exported state handle owned by sessions attached to one
    /// project. The caller holds that project's operation lock and its own
    /// session lock, so the current cell is handled separately to avoid
    /// recursive locking.
    pub(crate) fn invalidate_project_states(
        &self,
        project: &ProjectRuntime,
        current: &SessionCell,
        selection: &mut Selection,
    ) {
        selection.checkpoints.invalidate_states();
        let sessions = {
            let mut entries = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let live = entries.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            entries.retain(|entry| entry.strong_count() != 0);
            live
        };
        for session in sessions {
            if std::ptr::eq(Arc::as_ptr(&session), current) {
                continue;
            }
            let mut other = session
                .selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if other
                .project
                .as_ref()
                .is_some_and(|attached| std::ptr::eq(Arc::as_ptr(attached), project))
            {
                other.checkpoints.invalidate_states();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn restart_pet_for_test(&self, project: &Arc<ProjectRuntime>) {
        let _operation = project.lock();
        project.actor.restart();
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for session in sessions {
            let mut selection = session
                .selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if selection
                .project
                .as_ref()
                .is_some_and(|attached| Arc::ptr_eq(attached, project))
            {
                selection.checkpoints.invalidate_states();
            }
        }
    }
}

/// One MCP connection. Clones share only this connection's selection; all
/// connections created by one `ServerRuntime` share project PET actors.
#[derive(Clone)]
pub struct RocqServer {
    runtime: Arc<ServerRuntime>,
    pub(crate) session: Arc<SessionCell>,
    admission: Arc<AsyncMutex<()>>,
}

impl RocqServer {
    /// Convenience constructor for an isolated stdio/test server. HTTP
    /// callers should create one `ServerRuntime` and call `connection` for
    /// every protocol connection.
    pub fn new(engine: Arc<Engine>) -> Self {
        Arc::new(ServerRuntime::new(engine)).connection()
    }
}

/// Convert a client-supplied local `file:` URI into an absolute native path.
/// Non-file, non-local, relative, queried, and fragmented URIs are rejected so
/// a relative project path never silently acquires server-process semantics.
fn local_file_uri_path(uri: &str, source: &str) -> Result<PathBuf, Error> {
    let parsed = Url::parse(uri).map_err(|error| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            format!("{source} is not a valid URI: {error}"),
        )
    })?;
    if parsed.scheme() != "file" {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            format!(
                "{source} must be a local file URI, got scheme '{}'",
                parsed.scheme()
            ),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            format!("{source} must not contain a query or fragment"),
        ));
    }
    let path = parsed.to_file_path().map_err(|()| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            format!("{source} does not identify a local path on this MCP server"),
        )
    })?;
    if !path.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            format!("{source} must identify an absolute path"),
        ));
    }
    Ok(path)
}

/// Read Codex's negotiated per-call working directory metadata. Absence is
/// distinguishable from malformed metadata so standard MCP roots can be used
/// only when the client did not send this extension at all.
fn codex_sandbox_cwd(meta: &RequestMetaObject) -> Result<Option<PathBuf>, Error> {
    let Some(sandbox) = meta.get(CODEX_SANDBOX_STATE_META) else {
        return Ok(None);
    };
    let uri = sandbox
        .as_object()
        .and_then(|sandbox| sandbox.get(CODEX_SANDBOX_CWD))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                format!(
                    "request metadata '{CODEX_SANDBOX_STATE_META}' must contain string field '{CODEX_SANDBOX_CWD}'"
                ),
            )
        })?;
    local_file_uri_path(uri, "client working directory").map(Some)
}

/// Select the sole client workspace root and convert it to a native path.
/// Zero roots cannot define relative-path semantics; multiple roots are
/// deliberately ambiguous rather than choosing the first client entry.
fn sole_workspace_root(uris: &[String]) -> Result<PathBuf, Error> {
    match uris {
        [] => Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "relative project_path cannot be resolved because the client returned no workspace roots; pass an absolute project_path",
        )),
        [uri] => local_file_uri_path(uri, "client workspace root"),
        roots => Err(Error::new(
            ErrorKind::Ambiguous,
            format!(
                "relative project_path is ambiguous because the client returned {} workspace roots; pass an absolute project_path",
                roots.len()
            ),
        )),
    }
}

/// Resolve `start.project_path` against client context before the synchronous
/// adapter runs. Absolute paths and invalid argument shapes are left for the
/// adapter unchanged; a valid relative path is rewritten to an absolute path.
#[allow(
    deprecated,
    reason = "MCP roots remain the standard fallback for legacy clients"
)]
async fn resolve_start_project_path(
    name: &str,
    args: &mut Value,
    context: &RequestContext<RoleServer>,
) -> Result<(), Error> {
    if name != "start" {
        return Ok(());
    }
    let Some(object) = args.as_object() else {
        return Ok(());
    };
    if object.keys().any(|field| field != "project_path") {
        return Ok(());
    }
    let Some(project_path) = object
        .get("project_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return Ok(());
    };
    let requested = PathBuf::from(project_path);
    if requested.is_absolute() {
        return Ok(());
    }

    // Design note: the per-request Codex cwd represents the calling turn,
    // while MCP roots are connection-wide and may be broader. Prefer the
    // former when negotiated, but retain roots interoperability for clients
    // that implement the standard capability.
    let base = if let Some(cwd) = codex_sandbox_cwd(&context.meta)? {
        cwd
    } else if context
        .client_capabilities()
        .is_some_and(|capabilities| capabilities.roots.is_some())
    {
        let roots = tokio::select! {
            _ = context.ct.cancelled() => {
                return Err(Error::new(
                    ErrorKind::RequestCancelled,
                    "request was cancelled while resolving the client workspace root",
                ));
            }
            result = context.peer.list_roots() => result.map_err(|error| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    format!("could not obtain the client workspace root: {error}; pass an absolute project_path"),
                )
            })?,
        };
        let uris = roots
            .roots
            .into_iter()
            .map(|root| root.uri)
            .collect::<Vec<_>>();
        sole_workspace_root(&uris)?
    } else {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "relative project_path requires a client working directory, but this client supplied neither Codex sandbox metadata nor MCP roots; pass an absolute project_path",
        ));
    };
    let resolved = base.join(requested);
    let resolved = resolved.to_str().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "resolved project_path is not valid UTF-8; pass an absolute UTF-8 project_path",
        )
    })?;
    args["project_path"] = Value::String(resolved.to_owned());
    Ok(())
}

impl ServerHandler for RocqServer {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        capabilities
            .experimental
            .get_or_insert_default()
            .insert(CODEX_SANDBOX_STATE_META.to_owned(), Default::default());
        ServerConfig::new(capabilities)
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_server_info(Implementation::new("rocq-mcp", env!("CARGO_PKG_VERSION")))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        context.peer.set_peer_info(request);
        Ok(self.get_info())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(McpError::invalid_params(
                "tool catalog is not paginated; omit the cursor and retry",
                None,
            ));
        }
        Ok(ListToolsResult::with_all_items(tool_definitions().to_vec()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tool_definitions()
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if self.get_tool(&request.name).is_none() {
            return Err(McpError::invalid_params(
                "unknown tool; call tools/list and use one of the returned names",
                None,
            ));
        }
        let name = request.name;
        let mut args = Value::Object(request.arguments.unwrap_or_default());
        // Design note: progress is a query kind rather than a pushed MCP
        // notification.  It must bypass this connection's admission mutex and
        // the project operation barrier, otherwise a client could not observe
        // the very build/replay that currently owns those locks.
        if name.as_ref() == "query" && args.get("kind").and_then(Value::as_str) == Some("progress")
        {
            let result = progress_query(&self.session, &args);
            return Ok(match result {
                Ok(value) => CallToolResult::structured(value).into(),
                Err(error) => CallToolResult::structured_error(public_error(&error)).into(),
            });
        }
        if let Err(error) = resolve_start_project_path(name.as_ref(), &mut args, &context).await {
            return Ok(CallToolResult::structured_error(public_error(&error)).into());
        }
        let runtime = Arc::clone(&self.runtime);
        let session = Arc::clone(&self.session);
        let admission = Arc::clone(&self.admission);
        let cancel = context.ct.clone();
        let guard = tokio::select! {
            _ = context.ct.cancelled() => {
                return Err(McpError::invalid_request(
                    "request cancelled before admission; retry when ready",
                    None,
                ));
            }
            guard = admission.lock_owned() => guard,
        };
        let progress_target = args.get("target").or_else(|| args.get("at")).cloned();
        let generation = session.begin_progress(name.as_ref(), progress_target);
        // Synchronous Dune/PET calls execute off the async runtime. rmcp
        // cancels `context.ct` when the peer sends notifications/cancelled or
        // disconnects; mirror it into the blocking PET response wait so an
        // admitted runaway tactic cannot outlive its abandoned request.
        let pet_cancel = RequestCancellation::new();
        let watched_pet_cancel = pet_cancel.clone();
        let watched_protocol_cancel = cancel.clone();
        let query_deadline = (name.as_ref() == "query")
            .then(|| std::env::var("ROCQ_QUERY_TIMEOUT_SECS").ok())
            .flatten()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs)
            .or_else(|| (name.as_ref() == "query").then_some(Duration::from_secs(240)));
        let cancellation_watch = tokio::spawn(async move {
            if let Some(deadline) = query_deadline {
                tokio::select! {
                    _ = watched_protocol_cancel.cancelled() => watched_pet_cancel.cancel(),
                    _ = tokio::time::sleep(deadline) => watched_pet_cancel.timeout(),
                }
            } else {
                watched_protocol_cancel.cancelled().await;
                watched_pet_cancel.cancel();
            }
        });
        let progress_session = Arc::clone(&session);
        let joined = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let result = if cancel.is_cancelled() {
                Err(Error::new(
                    ErrorKind::RequestCancelled,
                    "request was cancelled before admission",
                ))
            } else {
                let (result, timed_out) = with_request_cancellation(&pet_cancel, || {
                    let result = dispatch(&runtime, &session, &name, args);
                    // The thread-local cancellation scope is still active
                    // here. Reading it after `with_request_cancellation`
                    // returns would always observe the restored outer scope
                    // and lose the distinction between cancellation and the
                    // query watchdog deadline.
                    (result, pet_cancel.is_timed_out())
                });
                if timed_out {
                    Err(Error::new(
                        ErrorKind::QueryTimeout,
                        "query deadline exceeded",
                    ))
                } else {
                    result
                }
            };
            progress_session.finish_progress(generation, &result);
            result
        })
        .await;
        cancellation_watch.abort();
        let result = joined.map_err(|_| {
            McpError::internal_error(
                "operation worker failed; retry the request or restart the server",
                None,
            )
        })?;
        match result {
            Ok(value) => Ok(CallToolResult::structured(value).into()),
            Err(error) => Ok(CallToolResult::structured_error(public_error(&error)).into()),
        }
    }
}

/// Validate and serve the lock-free progress query.  `generation` is an
/// optional caller-observed value used only to compute `changed`; polling is
/// stateless and never blocks waiting for a transition.
fn progress_query(session: &SessionCell, args: &Value) -> Result<Value, Error> {
    let object = args.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidRequest,
            "query arguments must be an object",
        )
    })?;
    if object.get("kind").and_then(Value::as_str) != Some("progress") {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "progress query kind must be progress",
        ));
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "kind" | "generation"))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "progress query has an unknown field",
        ));
    }
    let generation = object
        .get("generation")
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidRequest,
                    "generation must be a non-negative integer",
                )
            })
        })
        .transpose()?;
    Ok(session.progress_json(generation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_generation_and_terminal_record_are_pollable_without_notifications() {
        let session = SessionCell::new();
        assert_eq!(session.progress_json(None)["status"], "idle");
        let generation = session.begin_progress("check", Some(json!({"file":"A.v"})));
        session.update_progress(
            generation,
            "proof_step",
            1,
            Some(2),
            "evaluating alternative 2",
        );
        let running = session.progress_json(Some(generation));
        assert_eq!(running["status"], "running");
        assert_eq!(running["changed"], false);
        assert_eq!(running["phase"], "proof_step");
        assert_eq!(running["completed"], 1);
        assert_eq!(running["total"], 2);
        session.finish_progress(generation, &Ok(json!({"selected": 1})));
        let done = session.progress_json(Some(generation));
        assert_eq!(done["status"], "completed");
        assert_eq!(done["phase"], "completed");
        assert_eq!(done["completed"], 2);
        assert!(done["elapsed_ms"].as_u64().is_some());
        let next = session.begin_progress("query", None);
        assert!(next > generation);
        assert_eq!(session.progress_json(Some(generation))["changed"], true);
    }

    #[test]
    fn progress_query_rejects_unknown_and_mistyped_fields() {
        let session = SessionCell::new();
        assert_eq!(
            progress_query(&session, &json!({"kind":"progress","extra":true}))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(
            progress_query(&session, &json!({"kind":"progress","generation":"1"}))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest
        );
    }

    #[test]
    fn codex_cwd_and_workspace_roots_require_local_unambiguous_file_uris() {
        let meta: RequestMetaObject = serde_json::from_value(json!({
            "codex/sandbox-state-meta": {
                "sandboxCwd": "file:///tmp/rocq%20workspace"
            }
        }))
        .unwrap();
        assert_eq!(
            codex_sandbox_cwd(&meta).unwrap(),
            Some(PathBuf::from("/tmp/rocq workspace"))
        );

        let empty = sole_workspace_root(&[]).unwrap_err();
        assert_eq!(empty.kind, ErrorKind::InvalidConfiguration);
        let ambiguous = sole_workspace_root(&[
            "file:///tmp/first".to_owned(),
            "file:///tmp/second".to_owned(),
        ])
        .unwrap_err();
        assert_eq!(ambiguous.kind, ErrorKind::Ambiguous);
        let remote = sole_workspace_root(&["https://example.test/project".to_owned()]).unwrap_err();
        assert_eq!(remote.kind, ErrorKind::InvalidConfiguration);
        assert!(remote.message.contains("scheme 'https'"));
    }
}
