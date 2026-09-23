//! MCP tool catalog and input schemas.

use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Value, json};

const COMMANDS: &str = include_str!("../COMMANDS.md");

/// Return one tool's complete handbook section as its MCP description.
/// A missing section is a packaging error and fails catalog initialization.
fn tool_description(name: &str) -> &'static str {
    let heading = format!("## `{name}`\n");
    let start = COMMANDS
        .find(&heading)
        .unwrap_or_else(|| panic!("missing COMMANDS.md section for {name}"))
        + heading.len();
    let tail = &COMMANDS[start..];
    tail[..tail.find("\n## ").unwrap_or(tail.len())].trim()
}

/// Build a closed object schema with the given properties and required fields.
fn schema(p: Value, r: &[&str]) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([
        ("type".to_owned(), Value::String("object".to_owned())),
        ("properties".to_owned(), p),
        ("required".to_owned(), json!(r)),
        ("additionalProperties".to_owned(), Value::Bool(false)),
    ])
}
/// Return the process-wide immutable catalog of six public MCP tools.
pub fn tool_definitions() -> &'static [Tool] {
    use std::sync::OnceLock;
    static T: OnceLock<Vec<Tool>> = OnceLock::new();
    T.get_or_init(|| {
        let s = json!({"type": "string", "minLength": 1});
        // Design note: a root-level oneOf is flattened incorrectly by some
        // tool-discovery clients, leaving only its first kind visible. Keep
        // the wire schema flat and enforce kind-specific fields in dispatch.
        let query_schema = schema(
            json!({
                "kind": {"enum": ["goals", "search", "statement", "proof", "definition", "assumptions", "dependencies", "type", "notations"]},
                "target": s,
                "expression": s,
                "name_contains": s,
                "statement_pattern": s,
                "status": {"enum": ["Open", "Completed", "Pending", "Rejected"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "offset": {"type": "integer", "minimum": 0},
            }),
            &["kind"],
        );
        let v = [
            (
                "start",
                schema(json!({"project_path":s}), &["project_path"]),
            ),
            (
                "query",
                query_schema,
            ),
            (
                "declare",
                schema(
                    json!({"name":s,"statement":s,"kind":s,"library":s}),
                    &["name", "statement"],
                ),
            ),
            (
                "prove",
                schema(json!({"theorem":s}), &["theorem"]),
            ),
            (
                "check",
                schema(json!({"commands":s}), &["commands"]),
            ),
            (
                "check_multi",
                schema(
                    json!({"candidates":{"type":"array","items":s,"minItems":1,"maxItems":20}}),
                    &["candidates"],
                ),
            ),
        ];
        v.into_iter()
            .map(|(n, sc)| {
                Tool::new(n, tool_description(n), sc).with_annotations(
                    ToolAnnotations::default()
                        .read_only(n == "query" || n == "check_multi")
                        .open_world(false),
                )
            })
            .collect()
    })
}
