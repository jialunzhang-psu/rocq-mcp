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
            let source = if trace
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .starts_with("dune__")
            {
                lab.join("project_dune/theories/Main.v")
            } else {
                lab.join("project_coq/Main.v")
            };
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
    } else if fixture == "proof"
        && trace
            .to_string_lossy()
            .contains("/declare_state_unavailable/")
    {
        let lab = lab.to_path_buf();
        let lifecycle = trace
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.split("__").nth(1))
            .unwrap_or("")
            .to_owned();
        let target_starts = if lifecycle == "connected" { 1 } else { 2 };
        let mut starts = 0usize;
        runner = runner.with_after_event_hook(move |_line, event| {
            if let Event::Command { command, .. } = event
                && command.tool == "start"
            {
                starts += 1;
                if starts == target_starts {
                    fs::write(lab.join("state/pet-workspaces"), b"blocked").map_err(|source| {
                        rocq_e2e::TraceError::Io {
                            operation: "block disposable PET workspace directory",
                            source,
                        }
                    })?;
                }
            }
            Ok(())
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
                        Some("malformed_coqproject") => failure = Some("layout_invalid"),
                        Some("ambiguous_project") => failure = Some("layout_ambiguous"),
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
                        Some("layout_invalid") => fs::write(project.join("_CoqProject"), "-Q\n"),
                        Some("layout_ambiguous") => {
                            fs::write(project.join("_CoqProject"), "-Q . One\n-Q . Two\n")
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
        let mut project = None::<String>;
        runner = runner.with_after_event_hook(move |_line, event| {
            let Event::Command { command, .. } = event else {
                return Ok(());
            };
            if command.tool == "start" {
                project = command
                    .args
                    .get("project_path")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
            } else if command.tool == "declare" {
                // Arm only after the public declaration is open. The wrapper
                // fires later, during the candidate's load-path capture.
                fs::write(lab.join("axiom-inject.armed"), b"").map_err(|source| {
                    rocq_e2e::TraceError::Io {
                        operation: "arm post-baseline axiom injection",
                        source,
                    }
                })?;
            } else if command.tool == "check" {
                let Some(selected) = project.as_deref() else {
                    return Err(rocq_e2e::TraceError::InvalidConfiguration {
                        path: lab.clone(),
                        message: "axiom fixture has no selected project".into(),
                    });
                };
                let source_path = if selected == "project_dune" {
                    lab.join(selected).join("theories/Main.v")
                } else {
                    lab.join(selected).join("Main.v")
                };
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
        let mut project = None::<String>;
        runner = runner.with_after_event_hook(move |_line, event| {
            let Event::Command { command, .. } = event else {
                return Ok(());
            };
            if command.tool == "start" {
                project = command
                    .args
                    .get("project_path")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
            } else if command.tool == "prove" {
                let (file, source) =
                    match command.args.get("theorem").and_then(|value| value.as_str()) {
                        Some("t") => ("A.v", "Theorem t : False. Admitted.\n"),
                        Some("l") => ("B.v", "Lemma l : False. Admitted.\n"),
                        Some("d") => ("C.v", "Definition d : False. Admitted.\n"),
                        _ => {
                            return Err(rocq_e2e::TraceError::InvalidConfiguration {
                                path: lab.clone(),
                                message: "declaration-change fixture has unknown proof target"
                                    .into(),
                            });
                        }
                    };
                let Some(selected) = project.as_deref() else {
                    return Err(rocq_e2e::TraceError::InvalidConfiguration {
                        path: lab.clone(),
                        message: "declaration-change fixture has no selected project".into(),
                    });
                };
                let source_path = if selected == "project_dune" {
                    lab.join(selected).join("theories").join(file)
                } else {
                    lab.join(selected).join(file)
                };
                // Design note: mutation follows a successful public `prove`
                // response, so `check` must reject the old declaration view.
                fs::write(&source_path, source).map_err(|source| rocq_e2e::TraceError::Io {
                    operation: "change declaration header in disposable fixture",
                    source,
                })?;
            }
            Ok(())
        });
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
                | "check_multi_invalid_configuration"
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
            let (config_path, invalid_content) =
                if change == "check_publication_invalid_configuration" && recovery == "dune" {
                    (
                        lab.join("project_config_dune/theories/dune"),
                        "(rocq.theory (name Demo)\n",
                    )
                } else {
                    (lab.join("project_config/_CoqProject"), "-Q\n")
                };
            let mut starts = 0usize;
            let mut changed = false;
            runner = runner.with_after_event_hook(move |_line, event| {
                if let Event::Command { command, .. } = event {
                    if command.tool == "start" {
                        starts += 1;
                    }
                    let ready = if change == "check_publication_invalid_configuration" {
                        command.tool == "declare" && starts == target_starts
                    } else if change == "check_multi_invalid_configuration"
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
        } else if matches!(recovery.as_str(), "same_connection" | "reconnect")
            || (change == "dune_modules" && candidate_or_kind == "solved_pending")
            || (change == "configuration_changed" && candidate_or_kind == "solved_pending")
        {
            let working_dir = lab.to_path_buf();
            let marker = lab.join("server-start.count");
            let real_mcp = production_server.to_path_buf();
            let mut phase = 0usize;
            let mut starts = 0usize;
            let mut armed = false;
            runner = runner.with_after_event_hook(move |line, event| {
                if let Event::Command { command, .. } = event {
                    if command.tool == "start" {
                        starts += 1;
                    }
                    if change == "dune_modules"
                        && candidate_or_kind == "solved_pending"
                        && command.tool == "prove"
                        && !armed
                    {
                        fs::write(working_dir.join("dune-timeout.arm"), b"").map_err(|source| {
                            rocq_e2e::TraceError::Io {
                                operation: "arm Dune publication timeout fixture",
                                source,
                            }
                        })?;
                        armed = true;
                    }
                }
                let mutate = match event {
                    Event::Command { command, .. }
                        if recovery == "same_connection"
                            && phase == 0
                            && command.tool
                                == if candidate_or_kind == "solved_pending" {
                                    "check"
                                } else {
                                    "prove"
                                } =>
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
                if change == "configuration_changed"
                    && candidate_or_kind == "solved_pending"
                    && let Event::Command { command, .. } = event
                {
                    let restore =
                        (recovery == "same_connection" && phase == 1 && command.tool == "prove")
                            || (recovery != "same_connection"
                                && command.tool == "start"
                                && starts == 2);
                    if restore {
                        fs::write(
                            working_dir.join("project_config/_CoqProject"),
                            "-Q . Demo\n",
                        )
                        .map_err(|source| rocq_e2e::TraceError::Io {
                            operation: "restore e2e project configuration",
                            source,
                        })?;
                        phase = 2;
                    }
                }
                Ok(())
            });
        }
    } else if fixture == "toolchain_change" {
        let stem = trace
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let mut parts = stem.split("__");
        let change = parts.next().unwrap_or("");
        let candidate = parts.next().unwrap_or("").to_owned();
        let recovery = parts.next().unwrap_or("").to_owned();
        if matches!(recovery.as_str(), "same_connection" | "reconnect") {
            let executable = if change == "rocq_identity_changed" {
                "rocq"
            } else {
                "pet"
            };
            let path = lab.join("bin").join(executable);
            let mut changed = false;
            runner = runner.with_after_event_hook(move |_line, event| {
                let trigger = match event {
                    Event::Command { command, .. } if recovery == "same_connection" => {
                        command.tool
                            == if candidate == "solved_pending" {
                                "check"
                            } else {
                                "prove"
                            }
                    }
                    Event::UserDisconnect { .. } if recovery == "reconnect" => true,
                    _ => false,
                };
                if trigger && !changed {
                    let mut file =
                        fs::OpenOptions::new()
                            .append(true)
                            .open(&path)
                            .map_err(|source| rocq_e2e::TraceError::Io {
                                operation: "open fixture executable for identity change",
                                source,
                            })?;
                    file.write_all(b"\n# fixture identity changed\n")
                        .map_err(|source| rocq_e2e::TraceError::Io {
                            operation: "change fixture executable identity",
                            source,
                        })?;
                    changed = true;
                }
                Ok(())
            });
        }
    }
    Ok(runner)
}
