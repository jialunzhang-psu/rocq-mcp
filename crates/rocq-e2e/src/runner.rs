use crate::{
    Event, Result, ServerConfig, Trace, TraceError, UserId, assertion::assert_output,
    client::UserConnection, open_trace, parser::parse_event, server::ServerController,
};
use std::{collections::HashMap, io::BufRead, path::Path};

/// Fixture observer run after a successfully replayed event; it may mutate
/// only disposable test state and reports failure through the runner result.
type EventHook = dyn FnMut(usize, &Event) -> Result<()> + Send;

/// Executes one trace against an external rocq-mcp process. Each logical user
/// owns an independent MCP session; only the server state directory is shared.
pub struct TraceRunner {
    server: ServerController,
    users: HashMap<UserId, UserConnection>,
    after_event: Option<Box<EventHook>>,
    pending_parallel: Option<(usize, Event)>,
}

impl TraceRunner {
    /// Create a runner. This validates configuration but does not start a server;
    /// the trace must contain `server_start` explicitly.
    pub fn new(config: ServerConfig) -> Result<Self> {
        Ok(Self {
            server: ServerController::new(config)?,
            users: HashMap::new(),
            after_event: None,
            pending_parallel: None,
        })
    }

    /// Install a fixture-only observer after each successfully replayed event.
    /// The observer may mutate only the disposable test environment; it cannot
    /// change trace commands or expected outputs, and its failures fail replay.
    pub fn with_after_event_hook(
        mut self,
        hook: impl FnMut(usize, &Event) -> Result<()> + Send + 'static,
    ) -> Self {
        self.after_event = Some(Box::new(hook));
        self
    }

    /// Replay every event in source order and stop at the first failure.
    pub async fn run(&mut self, trace: &Trace) -> Result<()> {
        for located in &trace.events {
            self.execute(located.line, &located.event).await?;
        }
        self.validate_finished(trace.events.last().map_or(0, |event| event.line))
    }

    /// Replay a JSONL file incrementally and retain only the current event.
    ///
    /// Input and output semantics are identical to [`Self::run`]. Parse,
    /// lifecycle, transport, and assertion errors retain their physical line.
    /// The server and all user transports must be balanced when EOF is reached.
    pub async fn run_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let mut reader = open_trace(path.as_ref()).map_err(|source| TraceError::Io {
            operation: "open trace",
            source,
        })?;
        let mut source = String::new();
        let mut line = 0usize;
        loop {
            source.clear();
            let bytes = reader
                .read_line(&mut source)
                .map_err(|source| TraceError::Io {
                    operation: "read trace",
                    source,
                })?;
            if bytes == 0 {
                break;
            }
            line += 1;
            if source.trim().is_empty() {
                continue;
            }
            let event = parse_event(line, &source)?;
            self.execute(line, &event).await?;
        }
        self.validate_finished(line)
    }

    /// Require balanced lifecycle events.  A successful replay must not leave
    /// a child process or a user transport alive behind the test; callers that
    /// intentionally stop at an intermediate state should stop on an event
    /// error instead of treating that replay as successful.
    fn validate_finished(&self, line: usize) -> Result<()> {
        if self.pending_parallel.is_some() {
            return Err(TraceError::Lifecycle {
                line,
                message: "parallel group ended without a second command".into(),
            });
        }
        if !self.users.is_empty() {
            return Err(TraceError::Lifecycle {
                line,
                message: "trace ended with connected users".into(),
            });
        }
        if self.server.endpoint().is_some() {
            return Err(TraceError::Lifecycle {
                line,
                message: "trace ended with a running server".into(),
            });
        }
        Ok(())
    }

    async fn execute(&mut self, line: usize, event: &Event) -> Result<()> {
        if let Some((first_line, first)) = self.pending_parallel.take() {
            let (
                Event::Command {
                    parallel_group: Some(first_group),
                    ..
                },
                Event::Command {
                    parallel_group: Some(second_group),
                    ..
                },
            ) = (&first, event)
            else {
                return Err(TraceError::Lifecycle {
                    line,
                    message: "parallel group must contain two adjacent commands".into(),
                });
            };
            if first_group != second_group {
                return Err(TraceError::Lifecycle {
                    line,
                    message: "adjacent parallel commands have different groups".into(),
                });
            }
            Self::execute_parallel(
                &self.users,
                self.server.request_timeout(),
                first_line,
                &first,
                line,
                event,
            )
            .await?;
            if let Some(hook) = self.after_event.as_mut() {
                hook(first_line, &first)?;
                hook(line, event)?;
            }
            return Ok(());
        }
        if matches!(
            event,
            Event::Command {
                parallel_group: Some(_),
                ..
            }
        ) {
            self.pending_parallel = Some((line, event.clone()));
            return Ok(());
        }
        let result = match event {
            Event::ServerStart => self.server.start(line).await,
            Event::ServerKill => {
                self.server.kill(line).await?;
                // Design note: killing the process invalidates every transport;
                // reconnects must be explicit events, never implicit recovery.
                self.users.clear();
                Ok(())
            }
            Event::UserConnect { user } => self.connect(line, user).await,
            Event::UserDisconnect { user } => self.disconnect(line, user).await,
            Event::Command {
                user,
                command,
                expected,
                ..
            } => {
                let connection = self.users.get(user).ok_or_else(|| TraceError::Lifecycle {
                    line,
                    message: format!("user '{}' is not connected", user.as_str()),
                })?;
                let actual = connection
                    .call(command, line, user.as_str(), self.server.request_timeout())
                    .await;
                if expected == &serde_json::json!({"$transport":"lost"}) {
                    match actual {
                        Err(TraceError::Transport { .. }) => Ok(()),
                        Err(other) => Err(other),
                        Ok(value) => Err(TraceError::OutputMismatch {
                            line,
                            expected: expected.clone(),
                            actual: value,
                        }),
                    }
                } else {
                    assert_output(line, expected, &actual?)
                }
            }
        };
        result?;
        if let Some(hook) = self.after_event.as_mut() {
            hook(line, event)?;
        }
        Ok(())
    }

    /// Poll two independent MCP sessions together and compare the unordered
    /// pair of complete outputs. This proves overlap without assuming which
    /// user wins a legitimate race.
    async fn execute_parallel(
        users: &HashMap<UserId, UserConnection>,
        timeout: std::time::Duration,
        first_line: usize,
        first: &Event,
        second_line: usize,
        second: &Event,
    ) -> Result<()> {
        let (
            Event::Command {
                user: user_a,
                command: command_a,
                expected: expected_a,
                ..
            },
            Event::Command {
                user: user_b,
                command: command_b,
                expected: expected_b,
                ..
            },
        ) = (first, second)
        else {
            unreachable!()
        };
        if user_a == user_b {
            return Err(TraceError::Lifecycle {
                line: second_line,
                message: "parallel commands require distinct users".into(),
            });
        }
        let connection_a = users.get(user_a).ok_or_else(|| TraceError::Lifecycle {
            line: first_line,
            message: format!("user '{}' is not connected", user_a.as_str()),
        })?;
        let connection_b = users.get(user_b).ok_or_else(|| TraceError::Lifecycle {
            line: second_line,
            message: format!("user '{}' is not connected", user_b.as_str()),
        })?;
        let (actual_a, actual_b) = tokio::join!(
            connection_a.call(command_a, first_line, user_a.as_str(), timeout),
            connection_b.call(command_b, second_line, user_b.as_str(), timeout),
        );
        let actual_a = actual_a?;
        let actual_b = actual_b?;
        let matches = |expected: &serde_json::Value, actual: &serde_json::Value| {
            expected
                .get("$one_of")
                .and_then(serde_json::Value::as_array)
                .map_or(expected == actual, |choices| choices.contains(actual))
        };
        if (matches(expected_a, &actual_a) && matches(expected_b, &actual_b))
            || (matches(expected_a, &actual_b) && matches(expected_b, &actual_a))
        {
            Ok(())
        } else {
            Err(TraceError::OutputMismatch {
                line: first_line,
                expected: serde_json::json!([expected_a, expected_b]),
                actual: serde_json::json!([actual_a, actual_b]),
            })
        }
    }

    async fn connect(&mut self, line: usize, user: &UserId) -> Result<()> {
        if self.users.contains_key(user) {
            return Err(TraceError::Lifecycle {
                line,
                message: format!("user '{}' is already connected", user.as_str()),
            });
        }
        let endpoint = self
            .server
            .endpoint()
            .ok_or_else(|| TraceError::Lifecycle {
                line,
                message: "server is not running".into(),
            })?;
        let connection =
            UserConnection::connect(endpoint, line, user.as_str(), self.server.request_timeout())
                .await?;
        self.users.insert(user.clone(), connection);
        Ok(())
    }

    async fn disconnect(&mut self, line: usize, user: &UserId) -> Result<()> {
        let connection = self
            .users
            .remove(user)
            .ok_or_else(|| TraceError::Lifecycle {
                line,
                message: format!("user '{}' is not connected", user.as_str()),
            })?;
        connection
            .disconnect(line, user.as_str(), self.server.request_timeout())
            .await
    }
}
