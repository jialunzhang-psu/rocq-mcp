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
/// Return the process-wide immutable catalog of the public MCP tools.
pub fn tool_definitions() -> &'static [Tool] {
    use std::sync::OnceLock;
    static T: OnceLock<Vec<Tool>> = OnceLock::new();
    T.get_or_init(|| {
        let s = json!({"type": "string", "minLength": 1});
        let declaration_id = json!({
            "type":"object",
            "properties":{
                "file":s,
                "qualified_path":{"type":"array","items":s,"minItems":1}
            },
            "required":["file","qualified_path"],
            "additionalProperties":false
        });
        let timeout_ms = json!({
            "type":"integer",
            "minimum":1,
            "description":"Optional deadline in milliseconds, applied independently to each proof fragment."
        });
        // Design note: a root-level oneOf is flattened incorrectly by some
        // tool-discovery clients, leaving only its first kind visible. Keep
        // the wire schema flat and enforce kind-specific fields in dispatch.
        let query_schema = schema(
            json!({
                "kind": {"enum": ["goals", "search", "about", "print", "assumptions", "dependencies", "type", "notations", "locate_symbol", "progress"]},
                "target": declaration_id,
                "at": declaration_id,
                "expression": s,
                "pattern": s,
                "scope": {"enum": ["focused", "unfocused", "shelved", "given_up", "all"]},
                "goal_id": {"type":"array", "minItems":1},
                "offset": {"type":"integer","minimum":0},
                "symbol": s,
                "limit": {"type":"integer","minimum":1,"maximum":100},
                "generation": {"type":"integer","minimum":0},
                "diff": {"type":"boolean"},
                "structured": {"type":"boolean","description":"Include structured local contexts in a goals response."},
            }),
            &["kind"],
        );
        let v = [
            (
                "start",
                schema(json!({"project_path":s}), &["project_path"]),
            ),
            (
                "list_files",
                schema(json!({}), &[]),
            ),
            (
                "list_decls",
                schema(json!({"file":s}), &["file"]),
            ),
            (
                "query",
                query_schema,
            ),
            (
                "declare",
                schema(
                    json!({"name":s,"statement":s,"kind":s,"file":s}),
                    &["name", "statement", "file"],
                ),
            ),
            (
                "prove",
                schema(json!({"target":declaration_id.clone()}), &["target"]),
            ),
            (
                "abandon",
                schema(json!({"target":declaration_id}), &["target"]),
            ),
            (
                "check",
                schema(
                    json!({
                        "attempts":{"type":"array","items":s,"minItems":1,"maxItems":20},
                        "timeout_ms":timeout_ms.clone(),
                        "trace":{"type":"boolean","description":"Include a bounded sentence trace for rejected fragments."},
                        "structured":{"type":"boolean","description":"Include structured goal contexts and semantic goal diffs."}
                    }),
                    &["attempts"],
                ),
            ),
            (
                "try",
                schema(
                    json!({
                        "attempts":{"type":"array","items":s,"minItems":1,"maxItems":20},
                        "timeout_ms":timeout_ms,
                        "trace":{"type":"boolean","description":"Include a bounded sentence trace for rejected fragments."},
                        "structured":{"type":"boolean","description":"Include structured goal contexts and semantic goal diffs."}
                    }),
                    &["attempts"],
                ),
            ),
            (
                "rewind",
                schema(
                    json!({
                        "steps":{"type":"integer","minimum":1},
                        "checkpoint":{"type":"integer","minimum":1}
                    }),
                    &[],
                ),
            ),
        ];
        v.into_iter()
            .map(|(n, sc)| {
                Tool::new(n, tool_description(n), sc).with_annotations(
                    ToolAnnotations::default()
                        .read_only(matches!(n, "query" | "try" | "list_files" | "list_decls"))
                        .open_world(false),
                )
            })
            .collect()
    })
}
