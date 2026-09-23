//! Replayable process-boundary E2E traces for rocq-mcp.
//!
//! The crate deliberately depends on the public MCP transport only. Its modules
//! separate trace syntax, output assertions, client sessions, server lifecycle,
//! and orchestration so the runner does not duplicate server business logic.

mod assertion;
mod client;
mod error;
mod model;
mod parser;
mod runner;
mod server;

pub use error::{Result, TraceError};
pub use model::{Command, Event, LocatedEvent, Trace, UserId};
pub use parser::parse_trace;
pub use runner::TraceRunner;
pub use server::ServerConfig;

use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
};

/// Open either plain JSONL or a zstd-compressed JSONL trace as a streaming
/// reader. Large boundary traces never need to fit in process memory.
pub fn open_trace(path: impl AsRef<Path>) -> io::Result<Box<dyn BufRead + Send>> {
    let path = path.as_ref();
    let file = File::open(path)?;
    if path.extension().is_some_and(|extension| extension == "zst") {
        let decoder = zstd::stream::read::Decoder::new(file)?;
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Parse a trace from disk while retaining JSONL line numbers.
pub fn load_trace(path: impl AsRef<Path>) -> Result<Trace> {
    let reader = open_trace(path.as_ref()).map_err(|source| TraceError::Io {
        operation: "open trace",
        source,
    })?;
    parse_trace(reader)
}

/// Convenience entry point for one trace file and one isolated runner config.
pub async fn run_trace(path: impl AsRef<Path>, config: ServerConfig) -> Result<()> {
    let trace = load_trace(path)?;
    TraceRunner::new(config)?.run(&trace).await
}
