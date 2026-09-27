use crate::{Result, TraceError};
use std::{
    collections::BTreeMap, ffi::OsString, net::TcpListener, path::PathBuf, process::Stdio,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    process::{Child, Command},
    time::{Instant, sleep},
};

/// Configuration for the externally spawned server. The state directory is
/// reused across `server_kill`/`server_start`, which makes restart traces real.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// External rocq-mcp binary under test.
    pub executable: PathBuf,
    /// Durable engine state reused by every restart in the trace.
    pub state_dir: PathBuf,
    /// Child working directory used to resolve relative project paths.
    pub working_dir: PathBuf,
    /// Maximum time allowed for the HTTP listener to become reachable.
    pub startup_timeout: Duration,
    /// Maximum time allowed for an MCP connection handshake or disconnect.
    pub transport_timeout: Duration,
    /// Optional fixture watchdog for one tool call.
    ///
    /// Normal calls have no harness deadline: a valid Dune build can take an
    /// arbitrary amount of time, and engine-owned PET/Dune safety boundaries
    /// report their own typed failures. Fault fixtures set this explicitly so
    /// an intentionally dead child cannot leave the replay hanging.
    pub call_timeout: Option<Duration>,
    environment: BTreeMap<OsString, OsString>,
}

impl ServerConfig {
    pub fn new(executable: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            state_dir: state_dir.into(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            // Starting 64 isolated production processes is an intentional E2E
            // load shape. The deadline diagnoses a stuck child, not scheduler
            // latency while the host admits a full parallel batch.
            startup_timeout: Duration::from_secs(60),
            transport_timeout: Duration::from_secs(60),
            call_timeout: None,
            environment: BTreeMap::new(),
        }
    }

    /// Resolve relative project paths in trace commands from this directory.
    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = working_dir.into();
        self
    }

    /// Add one child-process environment override. Black-box fault fixtures use
    /// this without exposing test controls through the MCP interface.
    pub fn with_env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.insert(name.into(), value.into());
        self
    }

    /// Set a fixture-only watchdog for each MCP tool call in a replay.
    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        self.call_timeout = Some(timeout);
        self
    }
}

/// Owns at most one external rocq-mcp process and its current HTTP endpoint.
pub(crate) struct ServerController {
    config: ServerConfig,
    running: Option<RunningServer>,
}

struct RunningServer {
    child: Child,
    endpoint: String,
}

impl ServerController {
    pub(crate) fn new(config: ServerConfig) -> Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            config,
            running: None,
        })
    }

    pub(crate) fn endpoint(&self) -> Option<&str> {
        self.running.as_ref().map(|server| server.endpoint.as_str())
    }

    pub(crate) fn transport_timeout(&self) -> Duration {
        self.config.transport_timeout
    }

    pub(crate) fn call_timeout(&self) -> Option<Duration> {
        self.config.call_timeout
    }

    /// Spawn the server and wait until its TCP listener accepts connections.
    pub(crate) async fn start(&mut self, line: usize) -> Result<()> {
        if self.running.is_some() {
            return Err(TraceError::Lifecycle {
                line,
                message: "server is already running".into(),
            });
        }
        std::fs::create_dir_all(&self.config.state_dir).map_err(|source| TraceError::Io {
            operation: "create server state directory",
            source,
        })?;
        let address = reserve_loopback_address().map_err(|source| TraceError::Io {
            operation: "reserve server address",
            source,
        })?;
        let mut child = Command::new(&self.config.executable)
            .arg("--http")
            .arg(address.to_string())
            .env("ROCQ_NEW_STATE_DIR", &self.config.state_dir)
            .envs(&self.config.environment)
            .current_dir(&self.config.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| TraceError::Io {
                operation: "spawn rocq-mcp",
                source,
            })?;
        let deadline = Instant::now() + self.config.startup_timeout;
        loop {
            if TcpStream::connect(address).await.is_ok() {
                break;
            }
            if let Some(status) = child.try_wait().map_err(|source| TraceError::Io {
                operation: "inspect rocq-mcp",
                source,
            })? {
                return Err(TraceError::Server {
                    line,
                    message: format!("exited during startup with {status}"),
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill().await;
                return Err(TraceError::Server {
                    line,
                    message: "startup timed out".into(),
                });
            }
            sleep(Duration::from_millis(20)).await;
        }
        self.running = Some(RunningServer {
            child,
            endpoint: format!("http://{address}/mcp"),
        });
        Ok(())
    }

    /// Abruptly kill the process. No graceful MCP shutdown is attempted.
    pub(crate) async fn kill(&mut self, line: usize) -> Result<()> {
        let mut server = self.running.take().ok_or_else(|| TraceError::Lifecycle {
            line,
            message: "server is not running".into(),
        })?;
        if server
            .child
            .try_wait()
            .map_err(|source| TraceError::Io {
                operation: "inspect rocq-mcp before kill",
                source,
            })?
            .is_none()
        {
            server.child.kill().await.map_err(|source| TraceError::Io {
                operation: "kill rocq-mcp",
                source,
            })?;
        }
        let _ = server.child.wait().await;
        Ok(())
    }
}

impl Drop for ServerController {
    fn drop(&mut self) {
        if let Some(server) = &mut self.running {
            let _ = server.child.start_kill();
        }
    }
}

fn validate_config(config: &ServerConfig) -> Result<()> {
    if !config.executable.is_file() {
        return Err(TraceError::InvalidConfiguration {
            path: config.executable.clone(),
            message: "server executable is not a file".into(),
        });
    }
    if !config.working_dir.is_dir() {
        return Err(TraceError::InvalidConfiguration {
            path: config.working_dir.clone(),
            message: "server working directory is not a directory".into(),
        });
    }
    if config.startup_timeout.is_zero() {
        return Err(TraceError::InvalidConfiguration {
            path: config.executable.clone(),
            message: "startup timeout must be positive".into(),
        });
    }
    if config.transport_timeout.is_zero() {
        return Err(TraceError::InvalidConfiguration {
            path: config.executable.clone(),
            message: "transport timeout must be positive".into(),
        });
    }
    if config.call_timeout.is_some_and(|timeout| timeout.is_zero()) {
        return Err(TraceError::InvalidConfiguration {
            path: config.executable.clone(),
            message: "call timeout must be positive".into(),
        });
    }
    Ok(())
}

fn reserve_loopback_address() -> std::io::Result<std::net::SocketAddr> {
    // Design note: the production CLI accepts an address rather than a listener;
    // reserve port zero only long enough to obtain an isolated loopback port.
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.local_addr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_calls_have_no_harness_deadline_but_faults_can_opt_in() {
        let config = ServerConfig::new("server", "state");
        assert_eq!(config.call_timeout, None);
        let config = config.with_call_timeout(Duration::from_secs(3));
        assert_eq!(config.call_timeout, Some(Duration::from_secs(3)));
        assert_eq!(config.transport_timeout, Duration::from_secs(60));
    }
}
