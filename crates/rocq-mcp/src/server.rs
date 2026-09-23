//! MCP lifecycle and per-connection selection.

use crate::{
    adapter::{dispatch, public_error},
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
use rocq_engine::{AttemptId, Engine, Error, ErrorKind};
use serde_json::Value;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Default)]
pub(crate) struct Selection {
    pub(crate) project: Option<PathBuf>,
    pub(crate) attempt: Option<AttemptId>,
}
/// One MCP connection's user-facing selection plus a private engine capability.
/// The attempt id is never serialized or accepted from a client.
#[derive(Clone)]
pub struct RocqServer {
    engine: Arc<Engine>,
    selection: Arc<Mutex<Selection>>,
    admission: Arc<AsyncMutex<()>>,
}
impl RocqServer {
    /// Create the thin adapter around one process-wide engine runtime.
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            selection: Arc::new(Mutex::new(Selection::default())),
            admission: Arc::new(AsyncMutex::new(())),
        }
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
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(McpError::invalid_params(
                "tool catalog is not paginated",
                None,
            ));
        }
        Ok(ListToolsResult::with_all_items(tool_definitions().to_vec()))
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        tool_definitions().iter().find(|t| t.name == name).cloned()
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
        let engine = self.engine.clone();
        let selection = self.selection.clone();
        let admission = self.admission.clone();
        let cancel = context.ct.clone();
        // Design note: synchronous PET calls run behind a per-session admission
        // gate and on a blocking worker; cancellation before admission is
        // observable, while an admitted engine operation is allowed to finish.
        let guard = tokio::select! {
            _ = context.ct.cancelled() => {
                return Err(McpError::invalid_request(
                    "request cancelled before admission",
                    None,
                ));
            }
            guard = admission.lock_owned() => guard,
        };
        // Design note: cancellation after admission cannot undo an engine
        // operation, so the worker is joined before returning a response.
        let result = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            if cancel.is_cancelled() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "request cancelled before admission",
                ));
            }
            dispatch(&engine, &selection, &name, args)
        })
        .await
        .map_err(|_| McpError::internal_error("operation worker failed", None))?;
        match result {
            Ok(v) => Ok(CallToolResult::structured(v).into()),
            Err(e) => Ok(CallToolResult::structured_error(public_error(&e)).into()),
        }
    }
}
