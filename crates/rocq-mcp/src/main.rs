use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServiceExt, transport::stdio};
use rocq_engine::{Engine, EngineConfig};
use rocq_mcp::RocqServer;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = std::env::var_os("ROCQ_NEW_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("rocq-mcp-new"));
    let mut http = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => http = Some(args.next().ok_or("--http requires an address")?),
            "--stdio" => {}
            "--state-dir" => return Err("--state-dir is configured with ROCQ_NEW_STATE_DIR".into()),
            "-h" | "--help" => {
                println!("rocq-mcp [--stdio|--http ADDRESS]");
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let max_pet_processes = std::env::var("ROCQ_MAX_PET_PROCESSES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(4);
    let engine = Arc::new(Engine::new(EngineConfig {
        state_parent: state,
        operation_timeout: Duration::from_secs(20),
        // Design note: process capacity is a deployment resource limit, not an
        // MCP/session concern. Keeping it in process configuration lets a
        // one-process deployment exercise real eviction and replay semantics.
        max_pet_processes,
        ..Default::default()
    })?);
    if let Some(address) = http {
        let address: std::net::SocketAddr = address.parse()?;
        if !address.ip().is_loopback() {
            return Err("HTTP address must be loopback".into());
        }
        let cancellation = CancellationToken::new();
        let service = StreamableHttpService::new(
            move || Ok(RocqServer::new(engine.clone())),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default()
                .with_cancellation_token(cancellation.child_token()),
        );
        let router = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind(address).await?;
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                cancellation.cancel();
            })
            .await?;
    } else {
        let service = RocqServer::new(engine).serve(stdio()).await?;
        service.waiting().await?;
    }
    Ok(())
}
