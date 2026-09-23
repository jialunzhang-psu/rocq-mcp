use crate::{Command, Event, LocatedEvent, Result, Trace, TraceError};
use std::io::BufRead;

/// Parse a strict JSONL trace. Empty lines are ignored; every non-empty line
/// must contain exactly one of the five events.
pub fn parse_trace(reader: impl BufRead) -> Result<Trace> {
    let mut events = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let number = index + 1;
        let line = line.map_err(|source| TraceError::Io {
            operation: "read trace",
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let event = parse_event(number, &line)?;
        events.push(LocatedEvent {
            line: number,
            event,
        });
    }
    Ok(Trace { events })
}

/// Parse and validate one non-empty physical JSONL line.
///
/// This is shared by whole-file parsing and streaming replay so large matrix
/// traces do not require retaining every repeated boundary payload in memory.
pub(crate) fn parse_event(line: usize, source: &str) -> Result<Event> {
    let value: serde_json::Value =
        serde_json::from_str(source).map_err(|error| TraceError::Parse {
            line,
            message: error.to_string(),
        })?;
    reject_unknown_event_fields(line, &value)?;
    let event: Event = serde_json::from_value(value).map_err(|error| TraceError::Parse {
        line,
        message: error.to_string(),
    })?;
    validate_event(line, &event)?;
    Ok(event)
}

fn reject_unknown_event_fields(line: usize, value: &serde_json::Value) -> Result<()> {
    let object = value.as_object().ok_or_else(|| TraceError::Parse {
        line,
        message: "event must be a JSON object".into(),
    })?;
    let kind = object.get("event").and_then(serde_json::Value::as_str);
    let allowed: &[&str] = match kind {
        Some("command") => &["event", "user", "command", "expected", "parallel_group"],
        Some("user_connect" | "user_disconnect") => &["event", "user"],
        Some("server_start" | "server_kill") => &["event"],
        _ => return Ok(()), // Serde reports a missing or unknown discriminator.
    };
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(TraceError::Parse {
            line,
            message: format!("unknown field '{field}'"),
        });
    }
    Ok(())
}

fn validate_event(line: usize, event: &Event) -> Result<()> {
    if let Event::Command {
        command,
        expected,
        parallel_group,
        ..
    } = event
    {
        validate_command(line, command)?;
        if !expected.is_object() {
            return Err(TraceError::Parse {
                line,
                message: "expected must be a JSON object".into(),
            });
        }
        if expected.get("$transport").is_some()
            && expected != &serde_json::json!({"$transport":"lost"})
        {
            return Err(TraceError::Parse {
                line,
                message: "expected transport outcome must be exactly {'$transport':'lost'}".into(),
            });
        }
        if parallel_group
            .as_deref()
            .is_some_and(|group| group.trim().is_empty())
        {
            return Err(TraceError::Parse {
                line,
                message: "parallel_group must be non-empty".into(),
            });
        }
        if let Some(alternatives) = expected.get("$one_of")
            && (parallel_group.is_none()
                || expected.as_object().is_none_or(|value| value.len() != 1)
                || alternatives.as_array().is_none_or(|items| {
                    items.is_empty() || items.iter().any(|item| !item.is_object())
                }))
        {
            return Err(TraceError::Parse {
                line,
                message:
                    "expected $one_of requires a parallel group and a non-empty array of objects"
                        .into(),
            });
        }
    }
    Ok(())
}

fn validate_command(line: usize, command: &Command) -> Result<()> {
    if command.tool.trim().is_empty() {
        return Err(TraceError::Parse {
            line,
            message: "command.tool must be a non-empty string".into(),
        });
    }
    const TOOLS: [&str; 6] = ["start", "query", "declare", "prove", "check", "check_multi"];
    if !TOOLS.contains(&command.tool.as_str()) {
        return Err(TraceError::Parse {
            line,
            message: format!("unknown command tool '{}'", command.tool),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_all_five_events_with_physical_lines() {
        let input = r#"{"event":"server_start"}
{"event":"user_connect","user":"alice"}
{"event":"command","user":"alice","command":{"tool":"query","args":{"kind":"goals"}},"expected":{"kind":"invalid_request","message":"x"}}
{"event":"user_disconnect","user":"alice"}
{"event":"server_kill"}
"#;
        let trace = parse_trace(Cursor::new(input)).unwrap();
        assert_eq!(trace.events.len(), 5);
        assert_eq!(trace.events[2].line, 3);
    }

    #[test]
    fn rejects_unknown_fields_and_empty_identities() {
        for input in [
            r#"{"event":"server_start","extra":1}"#,
            r#"{"event":"user_connect","user":" "}"#,
            r#"{"event":"command","user":"a","command":{"tool":"","args":{}},"expected":{}}"#,
        ] {
            assert!(parse_trace(Cursor::new(input)).is_err(), "accepted {input}");
        }
    }
}
