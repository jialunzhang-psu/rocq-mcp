//! Real process-boundary smoke test: the client speaks JSON-RPC over stdio.
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

fn call_tool(
    input: &mut impl Write,
    output: &mut impl BufRead,
    id: u64,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        })
    )
    .unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], id, "{name}: {response}");
    response["result"].clone()
}

#[test]
fn official_stdio_transport_serves_initialize_and_tools_list() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
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
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 10);
    drop(input);
    let _ = child.wait();
}

#[test]
fn rewind_uses_monotonic_request_checkpoints_and_preserves_branches() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(
        project.path().join("A.v"),
        "Theorem t : forall A B C D : Prop, A -> B -> C -> D -> A. Admitted.\n\
         Theorem u : True. Admitted.\n",
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"rewind-test","version":"1"}}})
    )
    .unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();

    let start = call_tool(
        &mut input,
        &mut output,
        2,
        "start",
        serde_json::json!({"project_path":project.path()}),
    );
    assert_eq!(start["isError"], false, "{start}");

    let prove = call_tool(
        &mut input,
        &mut output,
        3,
        "prove",
        serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","t"]}}),
    );
    assert_eq!(prove["isError"], false, "{prove}");
    assert_eq!(
        prove["structuredContent"]["target"],
        serde_json::json!({"file":"A.v","qualified_path":["Demo","A","t"]})
    );
    assert!(prove["structuredContent"].get("theorem").is_none());
    assert!(prove["structuredContent"].get("statement").is_none());
    let root = prove["structuredContent"]["checkpoint"].as_u64().unwrap();

    // A rejected multi-sentence fragment commits none of its accepted prefix.
    let rejected = call_tool(
        &mut input,
        &mut output,
        4,
        "check",
        serde_json::json!({"attempts":["intro A. intro B. this_is_not_a_tactic."]}),
    );
    let rejected_content = &rejected["structuredContent"];
    assert_eq!(rejected["isError"], false, "{rejected}");
    assert!(rejected_content.get("selected").is_none());
    assert!(rejected_content.get("error").is_none());
    assert_eq!(rejected_content["rejected"][0]["kind"], "proof_step_failed");
    assert_eq!(rejected_content["state"]["checkpoint"], root);

    // Ordered check skips a rejected alternative and atomically selects the
    // first fully accepted multi-sentence fragment.
    let accepted = call_tool(
        &mut input,
        &mut output,
        5,
        "check",
        serde_json::json!({"attempts":[
            "intro A. this_is_not_a_tactic.",
            "intro A. intro B."
        ]}),
    );
    let accepted_content = &accepted["structuredContent"];
    assert_eq!(accepted_content["selected"], 1);
    assert_eq!(accepted_content["rejected"][0]["kind"], "proof_step_failed");
    assert!(accepted_content.get("error").is_none());
    let partial_checkpoint = accepted_content["state"]["checkpoint"].as_u64().unwrap();
    assert!(partial_checkpoint > root);

    // Hypothetical attempts have no checkpoint and do not advance selection.
    let tried = call_tool(
        &mut input,
        &mut output,
        6,
        "try",
        serde_json::json!({"attempts":["intro C.","this_is_not_a_tactic."]}),
    );
    assert!(
        tried["structuredContent"]["attempts"][0]["state"]
            .get("checkpoint")
            .is_none()
    );
    assert!(
        tried["structuredContent"]["attempts"][0]
            .get("error")
            .is_none()
    );
    assert!(
        tried["structuredContent"]["attempts"][1]
            .get("state")
            .is_none()
    );
    assert_eq!(
        tried["structuredContent"]["attempts"][1]["error"]["kind"],
        "proof_step_failed"
    );
    let goals = call_tool(
        &mut input,
        &mut output,
        60,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(
        goals["structuredContent"]["checkpoint"], partial_checkpoint,
        "read-only goal inspection must report the selected checkpoint"
    );

    let one_back = call_tool(&mut input, &mut output, 7, "rewind", serde_json::json!({}));
    assert_eq!(one_back["structuredContent"]["checkpoint"], root);
    assert!(one_back["structuredContent"].get("rewound_from").is_none());
    assert!(one_back["structuredContent"].get("rewound_to").is_none());

    let first = call_tool(
        &mut input,
        &mut output,
        8,
        "check",
        serde_json::json!({"attempts":["intros A B C."]}),
    )["structuredContent"]["state"]["checkpoint"]
        .as_u64()
        .unwrap();
    let second = call_tool(
        &mut input,
        &mut output,
        9,
        "check",
        serde_json::json!({"attempts":["intro D."]}),
    )["structuredContent"]["state"]["checkpoint"]
        .as_u64()
        .unwrap();
    let third = call_tool(
        &mut input,
        &mut output,
        10,
        "check",
        serde_json::json!({"attempts":["intro HA."]}),
    )["structuredContent"]["state"]["checkpoint"]
        .as_u64()
        .unwrap();
    assert!(root < first && first < second && second < third);

    let two_back = call_tool(
        &mut input,
        &mut output,
        11,
        "rewind",
        serde_json::json!({"steps":2}),
    );
    assert_eq!(two_back["structuredContent"]["checkpoint"], first);

    let exact_old_suffix = call_tool(
        &mut input,
        &mut output,
        12,
        "rewind",
        serde_json::json!({"checkpoint":third}),
    );
    assert_eq!(exact_old_suffix["structuredContent"]["checkpoint"], third);

    let _ = call_tool(
        &mut input,
        &mut output,
        13,
        "rewind",
        serde_json::json!({"checkpoint":root}),
    );
    let branch = call_tool(
        &mut input,
        &mut output,
        14,
        "check",
        serde_json::json!({"attempts":["intros A B C D."]}),
    )["structuredContent"]["state"]["checkpoint"]
        .as_u64()
        .unwrap();
    assert!(branch > third, "checkpoint IDs must never be reused");
    let old_branch = call_tool(
        &mut input,
        &mut output,
        15,
        "rewind",
        serde_json::json!({"checkpoint":third}),
    );
    assert_eq!(
        old_branch["structuredContent"]["checkpoint"], third,
        "rewinding must preserve the old branch"
    );

    // A replacement request is rejected without changing the selected proof.
    let rejected_replacement = call_tool(
        &mut input,
        &mut output,
        16,
        "prove",
        serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","u"]}}),
    );
    assert_eq!(rejected_replacement["isError"], true);
    assert_eq!(
        rejected_replacement["structuredContent"]["kind"],
        "invalid_request"
    );
    let still_selected = call_tool(
        &mut input,
        &mut output,
        17,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(still_selected["structuredContent"]["checkpoint"], third);
    let abandoned = call_tool(
        &mut input,
        &mut output,
        18,
        "abandon",
        serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","t"]}}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");

    // Explicit abandonment invalidates old IDs without resetting the allocator.
    let prove_u = call_tool(
        &mut input,
        &mut output,
        19,
        "prove",
        serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","u"]}}),
    );
    let u_root = prove_u["structuredContent"]["checkpoint"].as_u64().unwrap();
    assert!(u_root > branch);
    let stale = call_tool(
        &mut input,
        &mut output,
        20,
        "rewind",
        serde_json::json!({"checkpoint":root}),
    );
    assert_eq!(stale["isError"], true);
    assert_eq!(stale["structuredContent"]["kind"], "invalid_request");
    for (id, args) in [
        (21, serde_json::json!({"steps":0})),
        (22, serde_json::json!({"steps":1,"checkpoint":u_root})),
        (23, serde_json::json!({})),
    ] {
        let invalid = call_tool(&mut input, &mut output, id, "rewind", args);
        assert_eq!(invalid["isError"], true, "{invalid}");
        assert_eq!(invalid["structuredContent"]["kind"], "invalid_request");
    }

    let completed = call_tool(
        &mut input,
        &mut output,
        24,
        "check",
        serde_json::json!({"attempts":["exact I."]}),
    );
    assert_eq!(
        completed["structuredContent"]["state"]["status"], "Completed",
        "{completed}"
    );
    assert!(
        completed["structuredContent"]["state"]
            .get("goals")
            .is_none()
    );
    assert!(
        completed["structuredContent"]["state"]
            .get("checkpoint")
            .is_none()
    );
    let after_complete = call_tool(&mut input, &mut output, 25, "rewind", serde_json::json!({}));
    assert_eq!(after_complete["isError"], true);
    assert_eq!(
        after_complete["structuredContent"]["message"],
        "call prove first"
    );
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn start_survives_a_competing_dune_build() {
    let project = tempfile::tempdir().unwrap();
    let signal_dir = tempfile::tempdir().unwrap();
    let fifo = signal_dir.path().join("ready");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("A.v"), "Theorem t : True. Admitted.\n").unwrap();
    fs::write(project.path().join("dune"), format!(
        "(rocq.theory (name Demo))\n(rule (target x) (action (bash \"echo ready > {}; sleep 1; touch x\")))\n",
        fifo.display()
    )).unwrap();
    let mut build = Command::new("dune")
        .args(["build", "x"])
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut signal = String::new();
    fs::File::open(&fifo)
        .unwrap()
        .read_to_string(&mut signal)
        .unwrap();
    assert_eq!(signal.trim(), "ready");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"],
        1
    );
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"start","arguments":{"project_path":project.path()}}})).unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(
        response["result"]["structuredContent"],
        serde_json::json!({}),
        "{response}"
    );
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(build.wait().unwrap().success());
}

#[test]
fn absolute_custom_dune_build_dir_survives_start_and_proof_publication() {
    let project = tempfile::tempdir().unwrap();
    let build_dir = project.path().join("custom");
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(project.path().join("A.v"), "Theorem t : True. Admitted.\n").unwrap();
    let initial = Command::new("dune")
        .arg("build")
        .env("DUNE_BUILD_DIR", &build_dir)
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(initial.status.success(), "{:?}", initial.stderr);

    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .env("DUNE_BUILD_DIR", &build_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    for (id, name, arguments) in [
        (
            2,
            "start",
            serde_json::json!({"project_path":project.path()}),
        ),
        (3, "list_decls", serde_json::json!({"file":"A.v"})),
        (
            4,
            "prove",
            serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","t"]}}),
        ),
        (5, "check", serde_json::json!({"attempts":["exact I."]})),
    ] {
        writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
        input.flush().unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["isError"], false, "{name}: {response}");
        if name == "start" {
            assert_eq!(
                response["result"]["structuredContent"],
                serde_json::json!({})
            );
        }
        if name == "list_decls" {
            assert!(
                response["result"]["structuredContent"]
                    .get("file")
                    .is_none()
            );
            assert!(
                response["result"]["structuredContent"]["declarations"][0]
                    .get("name")
                    .is_none()
            );
            assert_eq!(
                response["result"]["structuredContent"]["declarations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        if name == "check" {
            assert!(
                response["result"]["structuredContent"]
                    .get("error")
                    .is_none()
            );
            assert_eq!(
                response["result"]["structuredContent"]["state"]["status"],
                "Completed"
            );
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(
        fs::read_to_string(project.path().join("A.v"))
            .unwrap()
            .contains("Qed.")
    );
}

#[test]
fn type_query_uses_selected_proof_unless_explicit_at_overrides_it() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(project.path().join("A.v"), "Definition first := 0.\n").unwrap();
    fs::write(
        project.path().join("B.v"),
        "Definition local_def := 1.\nTheorem t : True. Admitted.\n",
    )
    .unwrap();
    let built = Command::new("dune")
        .arg("build")
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(built.status.success(), "{:?}", built.stderr);
    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    for (id, name, arguments) in [
        (
            2,
            "start",
            serde_json::json!({"project_path":project.path()}),
        ),
        (
            3,
            "prove",
            serde_json::json!({"target":{"file":"B.v","qualified_path":["Demo","B","t"]}}),
        ),
        (
            4,
            "query",
            serde_json::json!({"kind":"type","expression":"local_def"}),
        ),
        (
            5,
            "query",
            serde_json::json!({
                "kind":"type",
                "expression":"first",
                "at":{"file":"A.v","qualified_path":["Demo","A","first"]}
            }),
        ),
    ] {
        writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
        input.flush().unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["isError"], false, "{name}: {response}");
        if name == "query" {
            assert!(
                response["result"]["structuredContent"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("nat")
            );
        }
    }
    let local_target = serde_json::json!({"file":"B.v","qualified_path":["Demo","B","local_def"]});
    for (id, kind) in [(6, "about"), (7, "print")] {
        let response = call_tool(
            &mut input,
            &mut output,
            id,
            "query",
            serde_json::json!({"kind":kind,"target":local_target.clone()}),
        );
        assert_eq!(response["isError"], false, "{kind}: {response}");
        assert!(
            !response["structuredContent"]["text"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }
    for (id, kind) in [(8, "statement"), (9, "proof"), (10, "definition")] {
        let response = call_tool(
            &mut input,
            &mut output,
            id,
            "query",
            serde_json::json!({"kind":kind,"target":local_target.clone()}),
        );
        assert_eq!(response["isError"], true, "{kind}: {response}");
        assert_eq!(response["structuredContent"]["kind"], "invalid_request");
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn abandon_discards_an_open_declaration_and_allows_redeclaration() {
    let project = tempfile::tempdir().unwrap();
    // Dune is the sole project/load-path authority; this fixture must not
    // rely on the removed legacy CoqProject walker.
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(project.path().join("Main.v"), "Definition base := 0.\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    let started = call_tool(
        &mut input,
        &mut output,
        2,
        "start",
        serde_json::json!({"project_path":project.path()}),
    );
    assert_eq!(started["isError"], false, "{started}");

    let redundant_prefix = call_tool(
        &mut input,
        &mut output,
        3,
        "declare",
        serde_json::json!({"name":"Demo.Main.fresh","statement":"True","file":"Main.v"}),
    );
    assert_eq!(redundant_prefix["isError"], true, "{redundant_prefix}");
    assert_eq!(
        redundant_prefix["structuredContent"]["kind"],
        "invalid_declaration"
    );

    let declared = call_tool(
        &mut input,
        &mut output,
        4,
        "declare",
        serde_json::json!({"name":"fresh","statement":"True","file":"Main.v"}),
    );
    assert_eq!(declared["isError"], false, "{declared}");
    assert_eq!(
        declared["structuredContent"]["target"],
        serde_json::json!({"file":"Main.v","qualified_path":["Demo","Main","fresh"]})
    );
    let checkpoint = declared["structuredContent"]["checkpoint"].clone();

    let blocked_declare = call_tool(
        &mut input,
        &mut output,
        5,
        "declare",
        serde_json::json!({"name":"other","statement":"True","file":"Main.v"}),
    );
    assert_eq!(blocked_declare["isError"], true, "{blocked_declare}");
    assert_eq!(
        blocked_declare["structuredContent"]["kind"],
        "invalid_request"
    );
    let still_selected = call_tool(
        &mut input,
        &mut output,
        6,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(
        still_selected["structuredContent"]["checkpoint"],
        checkpoint
    );
    assert_eq!(
        still_selected["structuredContent"]["target"],
        declared["structuredContent"]["target"]
    );

    let abandoned = call_tool(
        &mut input,
        &mut output,
        7,
        "abandon",
        serde_json::json!({"target":{"file":"Main.v","qualified_path":["Demo","Main","fresh"]}}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");
    assert_eq!(abandoned["structuredContent"], serde_json::json!({}));
    let redeclared = call_tool(
        &mut input,
        &mut output,
        8,
        "declare",
        serde_json::json!({"name":"fresh","statement":"True","file":"Main.v"}),
    );
    assert_eq!(redeclared["isError"], false, "{redeclared}");
    drop(input);
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(project.path().join("Main.v")).unwrap(),
        "Definition base := 0.\n"
    );
}
