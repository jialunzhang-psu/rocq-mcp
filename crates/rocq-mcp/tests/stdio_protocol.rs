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

#[cfg(target_os = "linux")]
fn pet_child_for_project(server: u32, project: &std::path::Path) -> u32 {
    let project = project.to_string_lossy();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let tasks = fs::read_dir(format!("/proc/{server}/task")).unwrap();
        for task in tasks.flatten() {
            let children = fs::read_to_string(task.path().join("children")).unwrap_or_default();
            for child in children.split_whitespace() {
                let Ok(pid) = child.parse::<u32>() else {
                    continue;
                };
                let command = fs::read(format!("/proc/{pid}/cmdline"))
                    .unwrap_or_default()
                    .split(|byte| *byte == 0)
                    .filter_map(|part| std::str::from_utf8(part).ok())
                    .collect::<Vec<_>>()
                    .join(" ");
                if command.contains("/pet") && command.contains(project.as_ref()) {
                    return pid;
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PET child for {project} was not found"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn kill_pet_and_wait(server: u32, project: &std::path::Path) -> u32 {
    let pet = pet_child_for_project(server, project);
    let status = Command::new("kill")
        .args(["-KILL", &pet.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let state = fs::read_to_string(format!("/proc/{pet}/status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("State:"))
                    .map(str::to_owned)
            });
        if state.is_none()
            || state
                .as_deref()
                .is_some_and(|line| line.contains("Z (zombie)"))
        {
            return pet;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PET {pet} did not exit: {state:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn wait_for_pid_exit(pid: u32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let state = fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("State:"))
                    .map(str::to_owned)
            });
        if state.is_none()
            || state
                .as_deref()
                .is_some_and(|line| line.contains("Z (zombie)"))
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PET {pid} survived request cancellation: {state:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
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
fn missing_search_reference_is_not_configuration_and_keeps_the_proof_live() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(project.path().join("A.v"), "Theorem t : True. Admitted.\n").unwrap();

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
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"typed-error-test","version":"1"}}})
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
    let proved = call_tool(
        &mut input,
        &mut output,
        3,
        "prove",
        serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","t"]}}),
    );
    assert_eq!(proved["isError"], false, "{proved}");
    let checkpoint = proved["structuredContent"]["checkpoint"].clone();

    let missing = call_tool(
        &mut input,
        &mut output,
        4,
        "query",
        serde_json::json!({"kind":"search","pattern":"pt_generated"}),
    );
    assert_eq!(missing["isError"], true, "{missing}");
    assert_eq!(
        missing["structuredContent"]["kind"], "not_found",
        "{missing}"
    );
    assert!(
        missing["structuredContent"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("pt_generated"))
    );
    let missing_message = missing["structuredContent"]["message"].as_str().unwrap();
    assert!(!missing_message.contains("PET"), "{missing_message}");
    assert!(!missing_message.contains("-32008"), "{missing_message}");
    assert!(!missing_message.contains("Next step:"), "{missing_message}");

    let malformed = call_tool(
        &mut input,
        &mut output,
        5,
        "query",
        serde_json::json!({"kind":"type","expression":"("}),
    );
    assert_eq!(malformed["isError"], true, "{malformed}");
    assert_eq!(
        malformed["structuredContent"]["kind"], "query_failed",
        "{malformed}"
    );

    // A semantic query rejection must not invalidate the selected PET state.
    let goals = call_tool(
        &mut input,
        &mut output,
        6,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(goals["isError"], false, "{goals}");
    assert_eq!(goals["structuredContent"]["checkpoint"], checkpoint);
    let abandoned = call_tool(
        &mut input,
        &mut output,
        7,
        "abandon",
        serde_json::json!({"target":{"file":"A.v","qualified_path":["Demo","A","t"]}}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[cfg(target_os = "linux")]
#[test]
fn transport_loss_keeps_attachment_replays_trace_and_allows_retirement() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(
        project.path().join("A.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server_pid = child.id();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(
        input,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"transport-recovery-test","version":"1"}}})
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

    assert_eq!(
        call_tool(
            &mut input,
            &mut output,
            2,
            "start",
            serde_json::json!({"project_path":project.path()}),
        )["isError"],
        false
    );
    let target = serde_json::json!({"file":"A.v","qualified_path":["Demo","A","t"]});
    let opened = call_tool(
        &mut input,
        &mut output,
        3,
        "prove",
        serde_json::json!({"target":target.clone()}),
    );
    assert_eq!(opened["isError"], false, "{opened}");
    let advanced = call_tool(
        &mut input,
        &mut output,
        4,
        "check",
        serde_json::json!({"attempts":["intros P H."]}),
    );
    assert_eq!(advanced["isError"], false, "{advanced}");
    let checkpoint = advanced["structuredContent"]["state"]["checkpoint"].clone();

    let killed = kill_pet_and_wait(server_pid, project.path());
    let lost = call_tool(
        &mut input,
        &mut output,
        5,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(lost["isError"], true, "{lost}");
    assert_eq!(lost["structuredContent"]["kind"], "pet_lost", "{lost}");
    let message = lost["structuredContent"]["message"].as_str().unwrap();
    assert!(message.contains("Broken pipe"), "{message}");
    assert!(message.contains("signal: 9 (SIGKILL)"), "{message}");
    assert!(message.contains("Next step:"), "{message}");
    assert!(!message.contains("PET"), "{message}");

    // No second `start` or `prove`: the connection attachment and checkpoint
    // text survived, so the next safe operation replaces PET and replays the
    // exact root-to-current path under the original checkpoint identity.
    let replayed = call_tool(
        &mut input,
        &mut output,
        6,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(
        replayed["isError"], false,
        "killed PET {killed}: {replayed}"
    );
    assert_eq!(replayed["structuredContent"]["checkpoint"], checkpoint);
    assert!(
        replayed["structuredContent"]["goals"]
            .as_str()
            .is_some_and(|goals| goals.contains("P : Prop") && goals.contains("H : P")),
        "{replayed}"
    );

    // A dead PET must not make explicit reattachment fail while releasing old
    // integer IDs. Reattachment retires this proof but keeps the project.
    kill_pet_and_wait(server_pid, project.path());
    let restarted = call_tool(
        &mut input,
        &mut output,
        7,
        "start",
        serde_json::json!({"project_path":project.path()}),
    );
    assert_eq!(restarted["isError"], false, "{restarted}");
    let no_proof = call_tool(
        &mut input,
        &mut output,
        8,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(no_proof["structuredContent"]["kind"], "invalid_request");
    let no_proof_message = no_proof["structuredContent"]["message"].as_str().unwrap();
    assert!(no_proof_message.starts_with("call prove first"));
    assert!(no_proof_message.contains("Next step:"));

    // The same retirement rule applies to abandon: loss means the old IDs no
    // longer exist, so abandonment succeeds and a replacement remains usable.
    let reopened = call_tool(
        &mut input,
        &mut output,
        9,
        "prove",
        serde_json::json!({"target":target.clone()}),
    );
    assert_eq!(reopened["isError"], false, "{reopened}");
    kill_pet_and_wait(server_pid, project.path());
    let abandoned = call_tool(
        &mut input,
        &mut output,
        10,
        "abandon",
        serde_json::json!({"target":target}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");
    let listed = call_tool(
        &mut input,
        &mut output,
        11,
        "list_decls",
        serde_json::json!({"file":"A.v"}),
    );
    assert_eq!(listed["isError"], false, "{listed}");
    assert_eq!(
        listed["structuredContent"]["declarations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    drop(input);
    assert!(child.wait().unwrap().success());
}

#[cfg(target_os = "linux")]
#[test]
fn runaway_tactics_honor_fragment_deadlines_and_mcp_cancellation() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(
        project.path().join("A.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();
    let source_before = fs::read(project.path().join("A.v")).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_rocq-mcp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server_pid = child.id();
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (responses_tx, responses_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            responses_tx
                .send(serde_json::from_str::<serde_json::Value>(&line).unwrap())
                .unwrap();
        }
    });
    let send = |input: &mut std::process::ChildStdin, value: serde_json::Value| {
        writeln!(input, "{value}").unwrap();
        input.flush().unwrap();
    };
    let receive = |id: u64, timeout: std::time::Duration| {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("MCP response deadline expired");
            let response = responses_rx
                .recv_timeout(remaining)
                .expect("MCP response deadline expired");
            if response["id"] == id {
                return response;
            }
        }
    };
    let call = |input: &mut std::process::ChildStdin,
                id: u64,
                name: &str,
                arguments: serde_json::Value| {
        send(
            input,
            serde_json::json!({
                "jsonrpc":"2.0", "id":id, "method":"tools/call",
                "params":{"name":name,"arguments":arguments}
            }),
        );
        receive(id, std::time::Duration::from_secs(5))["result"].clone()
    };

    send(
        &mut input,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cancellation-test","version":"1"}}}),
    );
    assert_eq!(receive(1, std::time::Duration::from_secs(2))["id"], 1);
    send(
        &mut input,
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    assert_eq!(
        call(
            &mut input,
            2,
            "start",
            serde_json::json!({"project_path":project.path()}),
        )["isError"],
        false
    );
    let target = serde_json::json!({"file":"A.v","qualified_path":["Demo","A","t"]});
    let opened = call(
        &mut input,
        3,
        "prove",
        serde_json::json!({"target":target.clone()}),
    );
    assert_eq!(opened["isError"], false, "{opened}");
    let root = opened["structuredContent"]["checkpoint"].as_u64().unwrap();

    // Each alternative receives its own deadline. The first one destroys the
    // PET epoch; the second is replayed from the same root and still runs.
    let tried = call(
        &mut input,
        4,
        "try",
        serde_json::json!({
            "attempts":["let rec loop n := loop n in loop 0.","idtac."],
            "timeout_ms":50
        }),
    );
    assert_eq!(tried["isError"], false, "{tried}");
    assert_eq!(
        tried["structuredContent"]["attempts"][0]["error"]["kind"], "proof_step_timeout",
        "{tried}"
    );
    assert!(
        tried["structuredContent"]["attempts"][1]["state"]
            .get("checkpoint")
            .is_none(),
        "{tried}"
    );
    let after_try = call(&mut input, 5, "query", serde_json::json!({"kind":"goals"}));
    assert_eq!(after_try["structuredContent"]["checkpoint"], root);

    let checked = call(
        &mut input,
        6,
        "check",
        serde_json::json!({
            "attempts":["let rec loop n := loop n in loop 0.","intros P H."],
            "timeout_ms":50
        }),
    );
    assert_eq!(checked["isError"], false, "{checked}");
    assert_eq!(checked["structuredContent"]["selected"], 1, "{checked}");
    assert_eq!(
        checked["structuredContent"]["rejected"][0]["kind"], "proof_step_timeout",
        "{checked}"
    );
    let checkpoint = checked["structuredContent"]["state"]["checkpoint"]
        .as_u64()
        .unwrap();
    assert!(checkpoint > root);

    // No wrapper deadline: protocol cancellation itself must preempt PET and
    // release the serialized project/connection locks promptly.
    let pet_before_cancel = pet_child_for_project(server_pid, project.path());
    send(
        &mut input,
        serde_json::json!({
            "jsonrpc":"2.0", "id":7, "method":"tools/call",
            "params":{"name":"try","arguments":{"attempts":["let rec loop n := loop n in loop 0."]}}
        }),
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    send(
        &mut input,
        serde_json::json!({
            "jsonrpc":"2.0", "method":"notifications/cancelled",
            "params":{"requestId":7,"reason":"bounded regression"}
        }),
    );
    // MCP declares a cancelled request's result unused, and rmcp may suppress
    // its late response entirely. Observe the required side effect directly.
    wait_for_pid_exit(pet_before_cancel);

    let recovered = call(&mut input, 8, "query", serde_json::json!({"kind":"goals"}));
    assert_eq!(recovered["isError"], false, "{recovered}");
    assert_eq!(
        recovered["structuredContent"]["checkpoint"], checkpoint,
        "{recovered}"
    );
    let replacement_pet = pet_child_for_project(server_pid, project.path());
    assert_ne!(replacement_pet, pet_before_cancel);
    let abandoned = call(
        &mut input,
        9,
        "abandon",
        serde_json::json!({"target":target}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");

    drop(input);
    assert!(child.wait().unwrap().success());
    wait_for_pid_exit(replacement_pet);
    reader.join().unwrap();
    assert_eq!(fs::read(project.path().join("A.v")).unwrap(), source_before);
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
    let after_complete_message = after_complete["structuredContent"]["message"]
        .as_str()
        .unwrap();
    assert!(after_complete_message.starts_with("call prove first"));
    assert!(after_complete_message.contains("Next step:"));
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
    let started = call_tool(
        &mut input,
        &mut output,
        2,
        "start",
        serde_json::json!({"project_path":project.path()}),
    );
    assert_eq!(started["isError"], false, "{started}");
    let proved = call_tool(
        &mut input,
        &mut output,
        3,
        "prove",
        serde_json::json!({"target":{"file":"B.v","qualified_path":["Demo","B","t"]}}),
    );
    assert_eq!(proved["isError"], false, "{proved}");
    let active_checkpoint = proved["structuredContent"]["checkpoint"].clone();
    for (id, name, arguments) in [
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
    // Named queries must honor the requested target file even while B.v's
    // proof is selected. A.v is intentionally not imported by B.v.
    let other_target = serde_json::json!({
        "file":"A.v",
        "qualified_path":["Demo","A","first"]
    });
    for (id, kind) in [
        (11, "about"),
        (12, "print"),
        (13, "assumptions"),
        (14, "dependencies"),
    ] {
        let response = call_tool(
            &mut input,
            &mut output,
            id,
            "query",
            serde_json::json!({"kind":kind,"target":other_target.clone()}),
        );
        assert_eq!(response["isError"], false, "{kind}: {response}");
        assert!(
            response["structuredContent"]["text"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "{kind}: {response}"
        );
    }
    let goals = call_tool(
        &mut input,
        &mut output,
        15,
        "query",
        serde_json::json!({"kind":"goals"}),
    );
    assert_eq!(goals["isError"], false, "{goals}");
    assert_eq!(goals["structuredContent"]["checkpoint"], active_checkpoint);
    for (id, kind) in [(16, "statement"), (17, "proof"), (18, "definition")] {
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

#[test]
fn proof_observability_preserves_pet_focus_ranges_and_bounded_goal_pages() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    let many_goal = (0..80)
        .map(|index| format!("P {index}"))
        .collect::<Vec<_>>()
        .join(" /\\ ");
    let large_context = (1000..1120)
        .map(|index| format!("P {index}"))
        .collect::<Vec<_>>()
        .join(" /\\ ");
    let source = format!(
        "Theorem shelved : True. Admitted.\n\
         Theorem branched : True /\\ True. Admitted.\n\
         Theorem diagnostic : True. Admitted.\n\
         Theorem many (P : nat -> Prop) (H : {large_context}) : {many_goal}. Admitted.\n"
    );
    fs::write(project.path().join("A.v"), &source).unwrap();

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
        serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":"2025-11-25","capabilities":{},
                      "clientInfo":{"name":"observability-test","version":"1"}}
        })
    )
    .unwrap();
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
    let started = call_tool(
        &mut input,
        &mut output,
        2,
        "start",
        serde_json::json!({"project_path":project.path()}),
    );
    assert_eq!(started["isError"], false, "{started}");
    let target = |leaf: &str| serde_json::json!({"file":"A.v","qualified_path":["Demo","A",leaf]});

    let shelved = target("shelved");
    let opened = call_tool(
        &mut input,
        &mut output,
        3,
        "prove",
        serde_json::json!({"target":shelved.clone()}),
    );
    assert_eq!(opened["isError"], false, "{opened}");
    let parked = call_tool(
        &mut input,
        &mut output,
        4,
        "check",
        serde_json::json!({"attempts":["shelve."]}),
    );
    assert_eq!(parked["isError"], false, "{parked}");
    let parked = &parked["structuredContent"]["state"];
    assert_eq!(parked["status"], "Open");
    assert_eq!(parked["goals"], "");
    assert_eq!(parked["goal_counts"]["focused"], 0);
    assert_eq!(parked["goal_counts"]["shelved"], 1);
    assert_eq!(parked["goal_counts"]["total"], 1);
    let shelved_id = parked["focus"]["shelved_goal_ids"][0].clone();
    let shelved_scope = call_tool(
        &mut input,
        &mut output,
        5,
        "query",
        serde_json::json!({"kind":"goals","scope":"shelved"}),
    );
    assert_eq!(shelved_scope["isError"], false, "{shelved_scope}");
    assert!(
        shelved_scope["structuredContent"]["goals"]
            .as_str()
            .unwrap()
            .contains("True")
    );
    let by_id = call_tool(
        &mut input,
        &mut output,
        6,
        "query",
        serde_json::json!({"kind":"goals","goal_id":shelved_id.clone()}),
    );
    assert_eq!(by_id["isError"], false, "{by_id}");
    assert!(
        by_id["structuredContent"]["goals"]
            .as_str()
            .unwrap()
            .contains("True")
    );
    let conflicting_selector = call_tool(
        &mut input,
        &mut output,
        7,
        "query",
        serde_json::json!({"kind":"goals","scope":"all","goal_id":shelved_id}),
    );
    assert_eq!(
        conflicting_selector["isError"], true,
        "{conflicting_selector}"
    );
    assert_eq!(
        conflicting_selector["structuredContent"]["kind"],
        "invalid_request"
    );
    let abandoned = call_tool(
        &mut input,
        &mut output,
        8,
        "abandon",
        serde_json::json!({"target":shelved}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");

    let branched = target("branched");
    for (id, name, arguments) in [
        (9, "prove", serde_json::json!({"target":branched.clone()})),
        (10, "check", serde_json::json!({"attempts":["split."]})),
    ] {
        let result = call_tool(&mut input, &mut output, id, name, arguments);
        assert_eq!(result["isError"], false, "{name}: {result}");
    }
    let focused = call_tool(
        &mut input,
        &mut output,
        11,
        "check",
        serde_json::json!({"attempts":["- exact I."]}),
    );
    assert_eq!(focused["isError"], false, "{focused}");
    let focused = &focused["structuredContent"]["state"];
    assert_eq!(focused["status"], "Open");
    assert_eq!(focused["goals"], "");
    assert_eq!(focused["goal_counts"]["focused"], 0);
    assert_eq!(focused["goal_counts"]["unfocused"], 1);
    assert!(focused["focus"]["depth"].as_u64().unwrap() >= 1);
    assert!(
        focused["focus"]["next_bullet"]
            .as_str()
            .unwrap()
            .contains('-')
    );
    let unfocused_id = focused["focus"]["stack"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|frame| {
            frame["left_goal_ids"]
                .as_array()
                .unwrap()
                .iter()
                .chain(frame["right_goal_ids"].as_array().unwrap().iter())
        })
        .next()
        .unwrap()
        .clone();
    let unfocused = call_tool(
        &mut input,
        &mut output,
        12,
        "query",
        serde_json::json!({"kind":"goals","goal_id":unfocused_id}),
    );
    assert_eq!(unfocused["isError"], false, "{unfocused}");
    assert!(
        unfocused["structuredContent"]["goals"]
            .as_str()
            .unwrap()
            .contains("True")
    );
    assert_eq!(
        call_tool(
            &mut input,
            &mut output,
            13,
            "abandon",
            serde_json::json!({"target":branched}),
        )["isError"],
        false
    );

    let diagnostic = target("diagnostic");
    let opened = call_tool(
        &mut input,
        &mut output,
        14,
        "prove",
        serde_json::json!({"target":diagnostic.clone()}),
    );
    let diagnostic_checkpoint = opened["structuredContent"]["checkpoint"].clone();
    let rejected = call_tool(
        &mut input,
        &mut output,
        15,
        "check",
        serde_json::json!({"attempts":["idtac \"🦀\". nonsense."]}),
    );
    assert_eq!(rejected["isError"], false, "{rejected}");
    let rejected = &rejected["structuredContent"];
    assert_eq!(rejected["state"]["checkpoint"], diagnostic_checkpoint);
    assert_eq!(rejected["rejected"][0]["kind"], "proof_step_failed");
    assert_eq!(
        rejected["rejected"][0]["diagnostic"]["byte_range"],
        serde_json::json!({"start":14,"end":22})
    );
    assert_eq!(
        call_tool(
            &mut input,
            &mut output,
            16,
            "abandon",
            serde_json::json!({"target":diagnostic}),
        )["isError"],
        false
    );

    let many = target("many");
    let opened = call_tool(
        &mut input,
        &mut output,
        17,
        "prove",
        serde_json::json!({"target":many.clone()}),
    );
    assert_eq!(opened["isError"], false, "{opened}");
    let hypothetical = call_tool(
        &mut input,
        &mut output,
        18,
        "try",
        serde_json::json!({"attempts":["repeat split."]}),
    );
    assert_eq!(hypothetical["isError"], false, "{hypothetical}");
    let hypothetical = &hypothetical["structuredContent"]["attempts"][0]["state"];
    assert_eq!(hypothetical["goals_truncated"], true);
    assert!(hypothetical.get("goals_next_offset").is_none());
    assert!(hypothetical["goals"].as_str().unwrap().len() <= 32 * 1024);
    let committed = call_tool(
        &mut input,
        &mut output,
        19,
        "check",
        serde_json::json!({"attempts":["repeat split."]}),
    );
    assert_eq!(committed["isError"], false, "{committed}");
    let state = &committed["structuredContent"]["state"];
    assert_eq!(state["goal_counts"]["focused"], 80);
    assert_eq!(state["goal_counts"]["total"], 80);
    assert_eq!(
        state["focus"]["focused_goal_ids"].as_array().unwrap().len(),
        80
    );
    let mut rendered = state["goals"].as_str().unwrap().to_owned();
    let mut offset = state["goals_next_offset"].as_u64().unwrap();
    let mut request_id = 20;
    loop {
        let page = call_tool(
            &mut input,
            &mut output,
            request_id,
            "query",
            serde_json::json!({"kind":"goals","offset":offset}),
        );
        request_id += 1;
        assert_eq!(page["isError"], false, "{page}");
        let page = &page["structuredContent"];
        rendered.push_str(page["goals"].as_str().unwrap());
        let Some(next) = page.get("next_offset").and_then(serde_json::Value::as_u64) else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert!(rendered.len() > 32 * 1024);
    assert!(rendered.contains("P 0"), "missing first goal");
    assert!(rendered.contains("P 79"), "missing last goal");
    let abandoned = call_tool(
        &mut input,
        &mut output,
        request_id,
        "abandon",
        serde_json::json!({"target":many}),
    );
    assert_eq!(abandoned["isError"], false, "{abandoned}");

    drop(input);
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(project.path().join("A.v")).unwrap(),
        source
    );
}
