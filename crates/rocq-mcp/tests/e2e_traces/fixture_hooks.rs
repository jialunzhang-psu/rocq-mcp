//! Fixture-only environment mutations after public trace events.
use rocq_e2e::{Event, TraceRunner};
use std::{fs, io::Write, path::Path, process::Command as ProcessCommand};

/// Attach filesystem/toolchain changes without adding a sixth public trace event.
pub fn attach_hooks(
    mut runner: TraceRunner,
    fixture: &str,
    trace: &Path,
    lab: &Path,
    production_server: &Path,
) -> rocq_e2e::Result<TraceRunner> {
    if fixture == "declare_race" {
        #[cfg(unix)]
        {
            use std::{
                io::Read,
                os::unix::net::UnixListener,
                thread,
                time::{Duration, Instant},
            };
            let socket = lab.join("declare-race.sock");
            let listener =
                UnixListener::bind(&socket).map_err(|source| rocq_e2e::TraceError::Io {
                    operation: "bind declaration-race fixture socket",
                    source,
                })?;
            listener
                .set_nonblocking(true)
                .map_err(|source| rocq_e2e::TraceError::Io {
                    operation: "configure declaration-race fixture socket",
                    source,
                })?;
            let source = lab.join("project_dune/theories/Main.v");
            // Design note: the listener exists before the public command starts;
            // the worker changes only copied source and cannot wait indefinitely.
            let worker = thread::spawn(move || -> std::io::Result<()> {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(error) => return Err(error),
                    }
                };
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                let mut request = [0u8; 1];
                stream.read_exact(&mut request)?;
                if request != [1] {
                    return Err(std::io::Error::other("unexpected declaration-race request"));
                }
                fs::OpenOptions::new()
                    .append(true)
                    .open(source)?
                    .write_all(b"Theorem fresh : False. Admitted.\n")?;
                stream.write_all(&[1])
            });
            let mut worker = Some(worker);
            runner = runner.with_after_event_hook(move |_line, event| {
                if let Event::Command { command, .. } = event
                    && command.tool == "declare"
                {
                    worker
                        .take()
                        .expect("one race per trace")
                        .join()
                        .map_err(|_| rocq_e2e::TraceError::InvalidConfiguration {
                            path: socket.clone(),
                            message: "declaration-race worker panicked".into(),
                        })?
                        .map_err(|source| rocq_e2e::TraceError::Io {
                            operation: "mutate declaration-race source",
                            source,
                        })?;
                }
                Ok(())
            });
        }
        #[cfg(not(unix))]
        return Err(rocq_e2e::TraceError::InvalidConfiguration {
            path: lab.to_path_buf(),
            message: "declaration-race fixture requires Unix".into(),
        });
    } else if fixture == "basic"
        && trace
            .to_string_lossy()
            .contains("/generated/start_failures/")
    {
        let lab = lab.to_path_buf();
        let mut failure = None::<&'static str>;
        let mut armed = false;
        runner = runner.with_after_event_hook(move |_line, event| {
            match event {
                Event::Command { command, .. } if command.tool == "start" => {
                    match command
                        .args
                        .get("project_path")
                        .and_then(|value| value.as_str())
                    {
                        Some("missing-project") => failure = Some("project_unavailable"),
                        Some("malformed_dune") => failure = Some("layout_invalid"),
                        Some("project") if failure.is_some() => armed = true,
                        _ => {}
                    }
                }
                Event::UserDisconnect { .. } if armed => {
                    let project = lab.join("project");
                    let result = match failure {
                        Some("project_unavailable") => {
                            fs::rename(&project, lab.join("removed_project"))
                        }
                        Some("layout_invalid") => {
                            fs::write(project.join("dune"), "(rocq.theory (name Demo)\n")
                        }
                        _ => unreachable!("armed fixture must know its failure"),
                    };
                    result.map_err(|source| rocq_e2e::TraceError::Io {
                        operation: "mutate disconnected project fixture",
                        source,
                    })?;
                    armed = false;
                    failure = None;
                }
                _ => {}
            }
            Ok(())
        });
    } else if fixture == "axiom_injection" {
        let lab = lab.to_path_buf();
        runner = runner.with_after_event_hook(move |_line, event| {
            let Event::Command { command, .. } = event else {
                return Ok(());
            };
            if command.tool == "declare" {
                let source_path = lab.join("project_dune/theories/Main.v");
                // The declaration has already frozen its PET-derived trust
                // baseline. Mutate the disposable source directly; no native
                // command-name hook or legacy load-path call is involved.
                fs::write(
                    source_path,
                    "Axiom witness : True.\nTheorem seed : True. Proof. exact I. Qed.\n",
                )
                .map_err(|source| rocq_e2e::TraceError::Io {
                    operation: "inject post-baseline axiom",
                    source,
                })?;
            } else if command.tool == "check" {
                let source_path = lab.join("project_dune/theories/Main.v");
                fs::write(
                    source_path,
                    "Definition witness : True := I.\nTheorem seed : True. Proof. exact I. Qed.\n",
                )
                .map_err(|source| rocq_e2e::TraceError::Io {
                    operation: "restore axiom fixture before next case",
                    source,
                })?;
            }
            Ok(())
        });
    } else if fixture == "declaration_change" {
        let lab = lab.to_path_buf();
        runner = runner.with_after_event_hook(move |_line, event| {
            let Event::Command { command, .. } = event else {
                return Ok(());
            };
            if command.tool == "prove" {
                let declaration = command.args.get("declaration");
                let file = declaration
                    .and_then(|value| value.get("file"))
                    .and_then(|value| value.as_str());
                let leaf = declaration
                    .and_then(|value| value.get("qualified_path"))
                    .and_then(|value| value.as_array())
                    .and_then(|path| path.last())
                    .and_then(|value| value.as_str());
                let (file, source) = match (file, leaf) {
                    (Some("theories/A.v"), Some("t")) => {
                        ("theories/A.v", "Theorem t : False. Admitted.\n")
                    }
                    (Some("theories/B.v"), Some("l")) => {
                        ("theories/B.v", "Lemma l : False. Admitted.\n")
                    }
                    (Some("theories/C.v"), Some("d")) => {
                        ("theories/C.v", "Definition d : False. Admitted.\n")
                    }
                    _ => {
                        return Err(rocq_e2e::TraceError::InvalidConfiguration {
                            path: lab.clone(),
                            message: "declaration-change fixture has unknown proof target".into(),
                        });
                    }
                };
                // Design note: the mutation follows the exact structured
                // DeclarationId used by the public request. The fixture must
                // not revive the removed name-only `theorem` protocol.
                let source_path = lab.join("project_dune").join(file);
                // Design note: mutation follows a successful public `prove`
                // response, so `check` must reject the old declaration view.
                fs::write(&source_path, source).map_err(|source| rocq_e2e::TraceError::Io {
                    operation: "change declaration header in disposable fixture",
                    source,
                })?;
            }
            Ok(())
        });
    } else if fixture == "pet_timeout" {
        let source = fs::read_to_string(trace).map_err(|source| rocq_e2e::TraceError::Io {
            operation: "read PET-timeout trace",
            source,
        })?;
        let timeout_lines = source
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .filter(|event| event.get("expected").is_some_and(contains_proof_timeout))
                    .map(|_| index + 1)
            })
            .collect::<Vec<_>>();
        if timeout_lines.len() > 1 || timeout_lines.first() == Some(&1) {
            return Err(rocq_e2e::TraceError::InvalidConfiguration {
                path: trace.to_path_buf(),
                message: "PET-timeout trace must contain one armable timeout command".into(),
            });
        }
        if let Some(timeout_line) = timeout_lines.first().copied() {
            let arm_after = timeout_line - 1;
            let marker = lab.join("pet-timeout.arm");
            // Design note: fault activation follows the exact successful event
            // immediately before the command whose public oracle is a timeout.
            // It is independent of how many disposable/cached PET processes
            // setup happened to require.
            runner = runner.with_after_event_hook(move |line, _event| {
                if line == arm_after {
                    fs::write(&marker, b"armed").map_err(|source| rocq_e2e::TraceError::Io {
                        operation: "arm PET-timeout fixture",
                        source,
                    })?;
                }
                Ok(())
            });
        }
    } else if fixture == "source_change" {
        let stem = trace
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        let mut parts = stem.split("__");
        let change = parts.next().unwrap_or("").to_owned();
        let candidate_or_kind = parts.next().unwrap_or("").to_owned();
        let recovery = parts.next().unwrap_or("").to_owned();
        let fourth_axis = parts.next().unwrap_or("").to_owned();
        if matches!(
            change.as_str(),
            "query_invalid_configuration"
                | "prove_invalid_configuration"
                | "try_invalid_configuration"
                | "check_publication_invalid_configuration"
        ) {
            let lifecycle = if change == "check_publication_invalid_configuration" {
                fourth_axis.as_str()
            } else if change != "query_invalid_configuration" {
                candidate_or_kind.as_str()
            } else {
                recovery.as_str()
            };
            let target_starts = if lifecycle == "connected" { 1 } else { 2 };
            let config_path = lab.join("project_config_dune/theories/dune");
            let invalid_content = "(rocq.theory (name Demo)\n";
            let mut starts = 0usize;
            let mut changed = false;
            runner = runner.with_after_event_hook(move |_line, event| {
                if let Event::Command { command, .. } = event {
                    if command.tool == "start" {
                        starts += 1;
                    }
                    let ready = if change == "check_publication_invalid_configuration" {
                        command.tool == "declare" && starts == target_starts
                    } else if change == "try_invalid_configuration"
                        || (change == "query_invalid_configuration" && candidate_or_kind == "goals")
                    {
                        command.tool == "prove" && starts == target_starts
                    } else {
                        command.tool == "start" && starts == target_starts
                    };
                    if ready && !changed {
                        // Design note: mutate only the copied fixture after a
                        // successful selection, never the user's real project.
                        fs::write(&config_path, invalid_content).map_err(|source| {
                            rocq_e2e::TraceError::Io {
                                operation: "invalidate e2e project configuration",
                                source,
                            }
                        })?;
                        changed = true;
                    }
                }
                Ok(())
            });
        } else if matches!(recovery.as_str(), "same_connection" | "reconnect") {
            let working_dir = lab.to_path_buf();
            let marker = lab.join("server-start.count");
            let real_mcp = production_server.to_path_buf();
            let mut phase = 0usize;
            let mut starts = 0usize;
            runner = runner.with_after_event_hook(move |line, event| {
                if let Event::Command { command, .. } = event
                    && command.tool == "start"
                {
                    starts += 1;
                }
                let mutate = match event {
                    Event::Command { command, .. }
                        if recovery == "same_connection"
                            && phase == 0
                            && command.tool == "prove" =>
                    {
                        true
                    }
                    Event::UserDisconnect { .. } if recovery == "reconnect" && phase == 0 => true,
                    Event::Command { command, .. }
                        if change == "source_restored"
                            && phase == 1
                            && command.tool == "start"
                            && starts == 2 =>
                    {
                        true
                    }
                    _ => false,
                };
                if mutate {
                    let status = ProcessCommand::new(working_dir.join("bin/rocq-mcp-wrapper"))
                        .arg("--mutate-only")
                        .current_dir(&working_dir)
                        .env("ROCQ_E2E_REAL_MCP", &real_mcp)
                        .env("ROCQ_E2E_SERVER_COUNT", &marker)
                        .env("ROCQ_E2E_CHANGE", &change)
                        .status()
                        .map_err(|error| rocq_e2e::TraceError::Server {
                            line,
                            message: format!("fixture mutation failed: {error}"),
                        })?;
                    if !status.success() {
                        return Err(rocq_e2e::TraceError::Server {
                            line,
                            message: format!("fixture mutation exited with {status}"),
                        });
                    }
                    phase += 1;
                }
                Ok(())
            });
        }
    }
    Ok(runner)
}

/// Whether one expected response contains the typed timeout at any envelope
/// depth. `check.rejected`, try results, and top-level errors all use this
/// same fixture oracle.
fn contains_proof_timeout(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.get("kind").and_then(serde_json::Value::as_str) == Some("proof_timeout")
                || object.values().any(contains_proof_timeout)
        }
        serde_json::Value::Array(values) => values.iter().any(contains_proof_timeout),
        _ => false,
    }
}
