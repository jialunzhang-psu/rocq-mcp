use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A stable logical user identity. It survives disconnects and server restarts,
/// but is never sent to the MCP server as an internal handle.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct UserId(String);

impl UserId {
    /// Return the trace-level identity exactly as written in the input file.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for UserId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(serde::de::Error::custom("user must be a non-empty string"));
        }
        Ok(Self(value))
    }
}

/// One public MCP tool call. `args` is always an object because MCP tool
/// arguments have an object schema.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub tool: String,
    pub args: Map<String, Value>,
}

/// The five-event trace language. Lifecycle events have no expected business
/// output; only `Command` carries the user/command/expected triple.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Command {
        user: UserId,
        command: Command,
        expected: Value,
        /// Adjacent commands with the same group run simultaneously. The
        /// group is trace metadata and is never forwarded to MCP.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parallel_group: Option<String>,
    },
    UserConnect {
        user: UserId,
    },
    UserDisconnect {
        user: UserId,
    },
    ServerStart,
    ServerKill,
}

/// An event paired with its physical JSONL line for actionable failures.
#[derive(Clone, Debug, PartialEq)]
pub struct LocatedEvent {
    pub line: usize,
    pub event: Event,
}

/// A fully parsed trace in replay order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trace {
    pub events: Vec<LocatedEvent>,
}
