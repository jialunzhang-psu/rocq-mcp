//! Black-box checks for the public MCP catalog. Domain behavior belongs to e2e.
use rocq_mcp::tool_definitions;
use serde_json::Value;

#[test]
fn catalog_is_the_public_six_tool_contract() {
    let tools = tool_definitions();
    assert_eq!(tools.len(), 6);
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec!["start", "query", "declare", "prove", "check", "check_multi",]
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
        for forbidden in ["cursor", "attempt", "workspace", "pet_pid", "file"] {
            assert!(
                !schema.contains(forbidden),
                "{forbidden} leaked in {}",
                tool.name
            );
        }
    }
}
