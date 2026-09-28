//! Black-box checks for the public MCP catalog. Domain behavior belongs to e2e.
use rocq_mcp::tool_definitions;
use serde_json::Value;

#[test]
fn catalog_is_the_public_ten_tool_contract() {
    let tools = tool_definitions();
    assert_eq!(tools.len(), 10);
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec![
            "start",
            "list_files",
            "list_decls",
            "query",
            "declare",
            "prove",
            "abandon",
            "check",
            "try",
            "rewind",
        ]
    );
    for tool in tools {
        let value: Value = serde_json::to_value(tool).unwrap();
        if tool.name != "query" {
            assert_eq!(value["inputSchema"]["additionalProperties"], false);
        }
        assert_eq!(value["annotations"]["openWorldHint"], false);
    }
}

#[test]
fn only_start_accepts_a_physical_project_path() {
    for tool in tool_definitions() {
        let schema = serde_json::to_value(tool).unwrap()["inputSchema"].to_string();
        if tool.name == "start" {
            assert!(schema.contains("project_path"));
        } else {
            assert!(!schema.contains("project_path"));
        }
        for forbidden in ["cursor", "attempt_id", "workspace", "pet_pid"] {
            assert!(
                !schema.contains(forbidden),
                "{forbidden} leaked in {}",
                tool.name
            );
        }
    }
}

#[test]
fn file_ids_are_exposed_only_where_the_lazy_protocol_needs_them() {
    for tool in tool_definitions() {
        let schema = serde_json::to_value(tool).unwrap()["inputSchema"].to_string();
        let accepts_file_id = matches!(
            tool.name.as_ref(),
            "list_decls" | "query" | "declare" | "prove" | "abandon"
        );
        assert_eq!(
            schema.contains("\"file\""),
            accepts_file_id,
            "unexpected FileId exposure in {}",
            tool.name
        );
    }
}

#[test]
fn prove_and_abandon_use_the_same_target_field() {
    for name in ["prove", "abandon"] {
        let tool = tool_definitions()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap();
        let schema = serde_json::to_value(tool).unwrap()["inputSchema"].clone();
        assert_eq!(schema["required"], serde_json::json!(["target"]));
        assert!(schema["properties"].get("target").is_some());
        assert!(schema["properties"].get("declaration").is_none());
    }
}
