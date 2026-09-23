use serde_json::Value;
use std::{fmt, path::PathBuf};

/// A source-located trace parse or execution failure.
#[derive(Debug)]
pub enum TraceError {
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    Parse {
        line: usize,
        message: String,
    },
    Lifecycle {
        line: usize,
        message: String,
    },
    Server {
        line: usize,
        message: String,
    },
    Transport {
        line: usize,
        user: String,
        message: String,
    },
    OutputMismatch {
        line: usize,
        expected: Value,
        actual: Value,
    },
    InvalidConfiguration {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Parse { line, message } => write!(f, "trace line {line}: {message}"),
            Self::Lifecycle { line, message } => write!(f, "trace line {line}: {message}"),
            Self::Server { line, message } => write!(f, "trace line {line}: server: {message}"),
            Self::Transport {
                line,
                user,
                message,
            } => write!(f, "trace line {line}: user '{user}': {message}"),
            Self::OutputMismatch {
                line,
                expected,
                actual,
            } => write!(
                f,
                "trace line {line}: output mismatch\nexpected: {expected}\nactual:   {actual}"
            ),
            Self::InvalidConfiguration { path, message } => write!(
                f,
                "invalid runner configuration at {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TraceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, TraceError>;
