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
        ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
};
use rocq_engine::{DuneProject, Engine, Error, ErrorKind, PetActor};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::Mutex as AsyncMutex;

/// The sole live PET actor and operation barrier for one canonical Dune
/// project. Serializing whole operations is intentional: PET itself is
/// serialized, and it makes publication invalidation atomic across sessions.
pub(crate) struct ProjectRuntime {
    project: DuneProject,
    actor: PetActor,
    operation: Mutex<()>,
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
}

impl SessionCell {
    fn new() -> Self {
        Self {
            selection: Mutex::new(Selection::default()),
        }
    }

    pub(crate) fn project(&self) -> Option<Arc<ProjectRuntime>> {
        self.selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .project
            .clone()
    }
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
        project: &Arc<ProjectRuntime>,
        current: &Arc<SessionCell>,
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
            if Arc::ptr_eq(&session, current) {
                continue;
            }
            let mut other = session
                .selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if other
                .project
                .as_ref()
                .is_some_and(|attached| Arc::ptr_eq(attached, project))
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

impl ServerHandler for RocqServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
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
                "tool catalog is not paginated",
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
            return Err(McpError::invalid_params("unknown tool", None));
        }
        let name = request.name;
        let args = Value::Object(request.arguments.unwrap_or_default());
        let runtime = Arc::clone(&self.runtime);
        let session = Arc::clone(&self.session);
        let admission = Arc::clone(&self.admission);
        let cancel = context.ct.clone();
        let guard = tokio::select! {
            _ = context.ct.cancelled() => {
                return Err(McpError::invalid_request("request cancelled before admission", None));
            }
            guard = admission.lock_owned() => guard,
        };
        // Synchronous Dune/PET calls execute off the async runtime. Once
        // admitted, an operation runs to its transaction boundary.
        let result = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            if cancel.is_cancelled() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "request cancelled before admission",
                ));
            }
            dispatch(&runtime, &session, &name, args)
        })
        .await
        .map_err(|_| McpError::internal_error("operation worker failed", None))?;
        match result {
            Ok(value) => Ok(CallToolResult::structured(value).into()),
            Err(error) => Ok(CallToolResult::structured_error(public_error(&error)).into()),
        }
    }
}
