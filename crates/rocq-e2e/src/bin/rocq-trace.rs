use rocq_e2e::{ServerConfig, run_trace};
use std::path::PathBuf;

/// Replay one JSONL trace against one external rocq-mcp executable.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let server = required(&mut args, "SERVER_EXECUTABLE")?;
    let state = required(&mut args, "STATE_DIRECTORY")?;
    let trace = required(&mut args, "TRACE_FILE")?;
    if args.next().is_some() {
        return Err("usage: rocq-trace SERVER_EXECUTABLE STATE_DIRECTORY TRACE_FILE".into());
    }
    run_trace(trace, ServerConfig::new(server, state)).await?;
    Ok(())
}

fn required(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<PathBuf, String> {
    args.next().map(PathBuf::from).ok_or_else(|| {
        format!("missing {name}; usage: rocq-trace SERVER_EXECUTABLE STATE_DIRECTORY TRACE_FILE")
    })
}
