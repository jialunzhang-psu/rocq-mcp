use crate::{Command, Result, TraceError};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::RunningService,
    transport::StreamableHttpClientTransport,
};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;

/// One real MCP session. A separate value is created for every connected user.
pub(crate) struct UserConnection {
    service: RunningService<RoleClient, ()>,
}

impl UserConnection {
    pub(crate) async fn connect(
        endpoint: &str,
        line: usize,
        user: &str,
        request_timeout: Duration,
    ) -> Result<Self> {
        let transport = StreamableHttpClientTransport::from_uri(endpoint);
        let service = timeout(request_timeout, ().serve(transport))
            .await
            .map_err(|_| TraceError::Transport {
                line,
                user: user.into(),
                message: format!("connect timed out after {request_timeout:?}"),
            })?
            .map_err(|error| TraceError::Transport {
                line,
                user: user.into(),
                message: format!("connect failed: {error}"),
            })?;
        Ok(Self { service })
    }

    /// Call one public tool and return only its structured business output.
    pub(crate) async fn call(
        &self,
        command: &Command,
        line: usize,
        user: &str,
        request_timeout: Duration,
    ) -> Result<Value> {
        let result = timeout(
            request_timeout,
            self.service.call_tool(
                CallToolRequestParams::new(command.tool.clone())
                    .with_arguments(command.args.clone()),
            ),
        )
        .await
        .map_err(|_| TraceError::Transport {
            line,
            user: user.into(),
            message: format!("tool call timed out after {request_timeout:?}"),
        })?
        .map_err(|error| TraceError::Transport {
            line,
            user: user.into(),
            message: format!("tool call failed: {error}"),
        })?;
        structured_output(result, line, user)
    }

    pub(crate) async fn disconnect(
        self,
        line: usize,
        user: &str,
        request_timeout: Duration,
    ) -> Result<()> {
        timeout(request_timeout, self.service.cancel())
            .await
            .map_err(|_| TraceError::Transport {
                line,
                user: user.into(),
                message: format!("disconnect timed out after {request_timeout:?}"),
            })?
            .map(|_| ())
            .map_err(|error| TraceError::Transport {
                line,
                user: user.into(),
                message: format!("disconnect failed: {error}"),
            })
    }
}

fn structured_output(result: CallToolResult, line: usize, user: &str) -> Result<Value> {
    result
        .structured_content
        .ok_or_else(|| TraceError::Transport {
            line,
            user: user.into(),
            message: "tool returned no structured content".into(),
        })
}
