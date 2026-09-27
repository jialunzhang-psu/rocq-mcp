use super::*;
use std::time::Duration;

#[test]
fn frame_reader_rejects_malformed_and_oversized_headers() {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "printf 'Content-Length: 8388609\\n\\n'"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    assert_eq!(read_response(&mut reader, 1), Err(PetError::OutputOverflow));
    let _ = child.wait();
}

#[test]
fn source_positions_count_utf16_units_not_utf8_bytes() {
    let source = "(* 𝄞 *)\nTheorem t : True.";
    assert_eq!(
        source_position(source, source.find('T').unwrap()).unwrap(),
        json!({"line": 1, "character": 0})
    );
    assert_eq!(
        source_position(source, "(* 𝄞".len()).unwrap(),
        json!({"line": 0, "character": 5})
    );
    assert!(source_position(source, 4).is_err());
}

#[test]
fn feedback_projects_pet_level_message_pairs_without_json_noise() {
    assert_eq!(
        parse_feedback(&json!({"feedback": [[3, "truth : True"], [2, "note"]]})).unwrap(),
        "truth : True\nnote"
    );
    assert!(parse_feedback(&json!({"feedback": ["not a pair"]})).is_err());
}

#[test]
fn pet_ast_keeps_same_named_nested_headers_distinct() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    std::fs::create_dir(project.path().join("theories")).unwrap();
    std::fs::write(
        project.path().join("theories/dune"),
        "(rocq.theory (name Main))\n",
    )
    .unwrap();
    let source = project.path().join("Main.v");
    let text = "Module A.\nTheorem t : True. Admitted.\nEnd A.\nModule B.\nTheorem t : True. Admitted.\nEnd B.\n";
    fs::write(&source, text).unwrap();
    let process = PetProcess::spawn(
        project.path().to_owned(),
        Duration::from_secs(5),
        &configured_pet_binary(),
    )
    .unwrap();
    for (offset, end) in text.match_indices("Theorem t : True.") {
        process
            .validate_header_locked(
                &source,
                text,
                "t",
                DeclarationKind::Theorem,
                offset + end.len(),
            )
            .unwrap();
    }
}

#[test]
fn pet_ast_enumerates_explicit_axioms_not_section_variables() {
    let project = tempfile::tempdir().unwrap();
    let source = project.path().join("Main.v");
    fs::write(
            &source,
            "Axioms a b : True.\nModule M.\nAxiom c : True.\nEnd M.\nModule Alias := M.\nSection S.\nVariable v : True.\nEnd S.\nModule Type T.\nAxiom signature_only : True.\nEnd T.\nDefinition s := \"Axiom ghost : True.\".\nAxiom d : True.\n",
        )
        .unwrap();
    let process = PetProcess::spawn(
        project.path().to_owned(),
        Duration::from_secs(5),
        &configured_pet_binary(),
    )
    .unwrap();
    assert_eq!(
        process.source_trust_file(&source).unwrap(),
        PetSourceTrust {
            explicit_axioms: vec![
                ("Main.a".into(), "True".into()),
                ("Main.b".into(), "True".into()),
                ("Main.M.c".into(), "True".into()),
                ("Main.d".into(), "True".into()),
            ],
            admitted: vec![],
        }
    );
}

#[test]
fn pet_ast_enumerates_admitted_theorems_and_proof_definitions() {
    let project = tempfile::tempdir().unwrap();
    let source = project.path().join("Main.v");
    fs::write(
            &source,
            "Module M.\nTheorem bad : True. Admitted.\nDefinition bad_def : True. Proof. Admitted.\nTheorem good : True. exact I. Qed.\nDefinition good_def : True. exact I. Defined.\nEnd M.\n",
        )
        .unwrap();
    let process = PetProcess::spawn(
        project.path().to_owned(),
        Duration::from_secs(5),
        &configured_pet_binary(),
    )
    .unwrap();
    assert_eq!(
        process.source_trust_file(&source).unwrap().admitted,
        vec!["Main.M.bad", "Main.M.bad_def"]
    );
}

#[test]
fn goal_parser_keeps_all_four_collections_structured() {
    let value = json!({
        "goals": [{"info":{"evar":["Ser_Evar", 1],"name":null},"hyps":[],"ty":"True"}],
        "stack": [[{"info":{"evar":["Ser_Evar", 2],"name":"x"},"hyps":[],"ty":"False"}]],
        "shelf": [],
        "given_up": []
    });
    let goals = parse_goals(&value, true).unwrap();
    assert_eq!(goals.focused.len(), 1);
    assert_eq!(goals.unfocused.len(), 1);
    assert!(goals.shelved.is_empty());
    assert!(goals.given_up.is_empty());
}

#[test]
fn capacity_rejects_invalid_bounds() {
    assert!(PetRuntime::new(Duration::from_secs(1), 0, 1024).is_err());
    assert!(PetRuntime::new(Duration::from_secs(1), 65, 1024).is_err());
}

#[test]
fn replay_uses_real_pet_and_returns_structured_goals() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    std::fs::create_dir(project.path().join("theories")).unwrap();
    std::fs::write(
        project.path().join("theories/dune"),
        "(rocq.theory (name Main))\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("theories/Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let runtime = PetRuntime::new(Duration::from_secs(10), 1, 1024).unwrap();
    let root = DeclarationSource {
        info: crate::DeclarationInfo {
            identity: crate::DeclarationIdentity {
                file: crate::FileId("theories/Main.v".into()),
                qualified_path: vec!["Main".into(), "Main".into(), "t".into()],
            },
            kind: crate::DeclarationKind::Theorem,
            statement: "Theorem t : True".into(),
        },
        library: crate::LogicalLibrary(vec!["Main".into(), "Main".into()]),
        anchor: crate::types::PetAnchor {
            source: project.path().join("theories/Main.v"),
            digest: Sha256::digest(b"Theorem t : True. Admitted.\n").into(),
            header: crate::types::PetRange { start: 0, end: 17 },
            declaration: Some(crate::types::PetRange { start: 0, end: 27 }),
        },
    };
    let state = runtime
        .restore_state(project.path(), &root, None, &[])
        .unwrap();
    assert_eq!(state.goals.focused.len(), 1);
    assert!(state.goals.unfocused.is_empty());
    let solved_actions = [CanonicalTactic("exact I.".into())];
    let incremental = runtime
        .fork_state(project.path(), &root, &state, &[], &solved_actions[0])
        .unwrap();
    assert!(incremental.proof_finished);
    assert!(incremental.goals.all_clear());
    let solved = runtime
        .restore_state(project.path(), &root, None, &solved_actions)
        .unwrap();
    assert!(solved.proof_finished);
    assert!(solved.goals.all_clear());
    for command in [
        "Axiom injected : True.",
        "Admitted.",
        "Ltac injected := idtac.",
    ] {
        let result = runtime.restore_state(
            project.path(),
            &root,
            None,
            &[CanonicalTactic(command.into())],
        );
        assert!(result.is_err(), "PET accepted non-proof input: {command}");
    }
    assert!(matches!(
        runtime.restore_state(
            project.path(),
            &root,
            None,
            &[CanonicalTactic("admit.".into())],
        ),
        Err(PetError::UnsafeProofCommand(_))
    ));
    runtime.shutdown();
}
