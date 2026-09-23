//! MCP tool catalog and input schemas.

use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Value, json};

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
        let target_queries = [
            "statement",
            "proof",
            "definition",
            "assumptions",
            "dependencies",
        ]
        .into_iter()
        .map(|kind| {
            json!({
                "type": "object",
                "properties": {"kind": {"const": kind}, "target": s},
                "required": ["kind", "target"],
                "additionalProperties": false,
            })
        })
        .collect::<Vec<_>>();
        let expression_queries = ["type", "notations"]
            .into_iter()
            .map(|kind| {
                json!({
                    "type": "object",
                    "properties": {"kind": {"const": kind}, "expression": s},
                    "required": ["kind", "expression"],
                    "additionalProperties": false,
                })
            })
            .collect::<Vec<_>>();
        let mut query_variants = target_queries;
        query_variants.extend(expression_queries);
        query_variants.push(json!({
            "type": "object",
            "properties": {"kind": {"const": "goals"}},
            "required": ["kind"],
            "additionalProperties": false,
        }));
        query_variants.push(json!({
            "type": "object",
            "properties": {
                "kind": {"const": "search"},
                "name_contains": s,
                "statement_pattern": s,
                "status": {"enum": ["Open", "Completed", "Pending", "Rejected"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "offset": {"type": "integer", "minimum": 0},
            },
            "required": ["kind"],
            "additionalProperties": false,
        }));
        let v = [
            (
                "start",
                "Attach to a project",
                schema(json!({"project_path":s}), &["project_path"]),
            ),
            (
                "query",
                "Query declarations or goals",
                serde_json::Map::from_iter([
                    ("type".to_owned(), Value::String("object".to_owned())),
                    ("oneOf".to_owned(), Value::Array(query_variants)),
                ]),
            ),
            (
                "declare",
                "Declare a theorem",
                schema(
                    json!({"name":s,"statement":s,"kind":s,"library":s}),
                    &["name", "statement"],
                ),
            ),
            (
                "prove",
                "Select a theorem",
                schema(json!({"theorem":s}), &["theorem"]),
            ),
            (
                "check",
                "Submit proof commands",
                schema(json!({"commands":s}), &["commands"]),
            ),
            (
                "check_multi",
                "Try candidates",
                schema(
                    json!({"candidates":{"type":"array","items":s,"minItems":1,"maxItems":20}}),
                    &["candidates"],
                ),
            ),
        ];
        v.into_iter()
            .map(|(n, d, sc)| {
                Tool::new(n, d, sc).with_annotations(
                    ToolAnnotations::default()
                        .read_only(n == "query" || n == "check_multi")
                        .open_world(false),
                )
            })
            .collect()
    })
}
