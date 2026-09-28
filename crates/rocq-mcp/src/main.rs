use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServiceExt, transport::stdio};
use rocq_engine::{Engine, EngineConfig};
use rocq_mcp::ServerRuntime;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut http = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--http" => http = Some(args.next().ok_or("--http requires an address")?),
            "--stdio" => {}
            "-h" | "--help" => {
                println!("rocq-mcp [--stdio|--http ADDRESS]");
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let command_timeout = std::env::var("ROCQ_COMMAND_TIMEOUT_SECS")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .map(std::time::Duration::from_secs);
    let engine = Arc::new(Engine::new(EngineConfig { command_timeout })?);
    let runtime = Arc::new(ServerRuntime::new(engine));
    if let Some(address) = http {
        let address: std::net::SocketAddr = address.parse()?;
        if !address.ip().is_loopback() {
            return Err("HTTP address must be loopback".into());
        }
        let cancellation = CancellationToken::new();
        let service = StreamableHttpService::new(
            move || Ok(runtime.connection()),
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
        let service = runtime.connection().serve(stdio()).await?;
        service.waiting().await?;
    }
    Ok(())
}
