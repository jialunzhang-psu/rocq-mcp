//! Real process-boundary smoke test: the client speaks JSON-RPC over stdio.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn official_stdio_transport_serves_initialize_and_tools_list() {
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .env("ROCQ_NEW_STATE_DIR", state.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("rocq-mcp binary starts");
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut output = BufReader::new(stdout);
    writeln!(input, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-11-25","capabilities":{{}},"clientInfo":{{"name":"e2e","version":"1"}}}}}}"#).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let initialize: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["result"]["serverInfo"]["name"], "rocq-mcp");

    writeln!(
        input,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    )
    .unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let tools: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(tools["id"], 2);
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);
    drop(input);
    let _ = child.wait();
}
