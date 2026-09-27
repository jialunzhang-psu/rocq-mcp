use rocq_engine::{DeclarationIdentity, Engine, EngineConfig, ErrorKind};
use std::{fs, path::Path, time::Duration};

fn engine(state: &Path) -> Engine {
    Engine::new(EngineConfig {
        state_parent: state.to_owned(),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap()
}

fn all_declarations(
    engine: &Engine,
    project: &Path,
) -> Result<Vec<rocq_engine::DeclarationInfo>, rocq_engine::Error> {
    let mut declarations = Vec::new();
    for file in engine.list_files(project)? {
        declarations.extend(engine.list_decls(project, &file)?);
    }
    Ok(declarations)
}

fn id(library: &[&str], constant: &str) -> DeclarationIdentity {
    DeclarationIdentity {
        file: crate::FileId("Main.v".into()),
        qualified_path: library
            .iter()
            .map(|x| (*x).into())
            .chain(std::iter::once(constant.into()))
            .collect(),
    }
}

#[test]
fn non_dune_directory_does_not_activate_a_source_walker() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("nested")).unwrap();
    fs::write(
        project.path().join("nested/Hidden.v"),
        "Theorem hidden : True. Admitted.\n",
    )
    .unwrap();
    fs::write(project.path().join("_CoqProject"), "-Q nested Hidden\n").unwrap();

    let error = match crate::dune::Layout::load(project.path(), &[], Duration::from_secs(5)) {
        Ok(_) => panic!("non-Dune source walker unexpectedly provided a layout"),
        Err(error) => error,
    };
    assert_eq!(error.kind, ErrorKind::InvalidConfiguration);
    assert!(error.message.contains("Dune workspace discovery failed"));
}

#[test]
fn source_contract_has_no_path_based_new_declaration_or_regex_identity_parser() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();
    let parser_source =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/pet/mod.rs")).unwrap();
    let publication_source =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/writeback.rs")).unwrap();
    assert!(!source.contains("NewTheorem") && !source.contains("regex::"));
    assert!(
        !parser_source.contains("fn parse_file("),
        "declaration semantics must come from PET, not a local parser"
    );
    assert!(
        !source.contains("identity: source["),
        "byte offsets may locate text but never define identity"
    );
    assert!(
        !publication_source.contains("with_extension(\"vo\")"),
        "native artifact targets must come from Dune's reported rules"
    );
}

#[test]
fn generated_and_vcs_trees_are_not_logical_project_sources() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::create_dir_all(project.path().join("_build/default")).unwrap();
    fs::create_dir_all(project.path().join(".git/objects")).unwrap();
    fs::create_dir_all(project.path().join(".hg/store")).unwrap();
    fs::create_dir_all(project.path().join(".svn/pristine")).unwrap();
    fs::create_dir_all(project.path().join("target/debug")).unwrap();
    fs::write(
        project.path().join("Real.v"),
        "Theorem real : True. Admitted.\n",
    )
    .unwrap();
    for (path, name) in [
        ("_build/default/Generated.v", "generated"),
        (".git/objects/Hidden.v", "git_hidden"),
        (".hg/store/Hidden.v", "hg_hidden"),
        (".svn/pristine/Hidden.v", "svn_hidden"),
        ("target/debug/Hidden.v", "target_hidden"),
    ] {
        fs::write(
            project.path().join(path),
            format!("Theorem {name} : True. Admitted.\n"),
        )
        .unwrap();
    }

    let state = tempfile::tempdir().unwrap();
    let declarations = all_declarations(&engine(state.path()), project.path()).unwrap();
    assert_eq!(
        declarations
            .iter()
            .map(|item| item.identity.constant().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["real"],
        "build products and VCS metadata must never become project declarations"
    );
}

#[test]
fn attached_dune_project_uses_dune_selected_sources() {
    let Some(path) = std::env::var_os("ROCQ_MCP_TEST_DUNE_PROJECT") else {
        return;
    };
    let root = fs::canonicalize(path).unwrap();
    let layout = crate::dune::Layout::load(&root, &[], Duration::from_secs(20)).unwrap();
    assert!(
        layout
            .files()
            .iter()
            .any(|path| path.ends_with("Spine/Roadmap.v"))
    );
    assert!(
        !layout
            .files()
            .iter()
            .any(|path| path.to_string_lossy().contains(".rocq-mcp"))
    );
}

#[test]
fn dune_metadata_is_independent_of_a_competing_build() {
    use std::io::Read;
    use std::process::{Command, Stdio};

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
        "(lang dune 3.22)\n(name lockprobe)\n",
    )
    .unwrap();
    fs::write(
        project.path().join("dune"),
        format!(
            "(rule (target x) (action (bash \"echo ready > {}; sleep 1; touch x\")))\n",
            fifo.display()
        ),
    )
    .unwrap();
    let mut build = Command::new("dune")
        .args(["build", "x"])
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The action signals only after Dune has acquired its build lock.
    let mut signal = String::new();
    std::fs::File::open(&fifo)
        .unwrap()
        .read_to_string(&mut signal)
        .unwrap();
    assert_eq!(signal.trim(), "ready");
    let layout = crate::dune::Layout::load(project.path(), &[], Duration::from_secs(10));
    assert!(layout.is_ok(), "metadata query failed during build");
    assert!(
        build.try_wait().unwrap().is_none(),
        "query waited for the build"
    );
    assert!(build.wait().unwrap().success());
}

#[test]
fn syntactically_closed_but_uncompilable_proof_is_not_completed() {
    use std::process::Command;
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
    fs::write(
        project.path().join("A.v"),
        "Theorem t : True. Proof. exact nonexistent. Qed.\n",
    )
    .unwrap();
    assert!(
        !Command::new("dune")
            .arg("build")
            .current_dir(project.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    let declarations = all_declarations(&engine, project.path()).unwrap();
    let theorem = declarations
        .iter()
        .find(|item| item.identity.constant() == Some("t"))
        .unwrap();
    let failure = engine
        .open(project.path(), theorem.identity.clone())
        .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::InvalidDeclaration);
}

#[test]
fn unrelated_or_malformed_shared_build_lock_cannot_reclassify_metadata_errors() {
    use fs2::FileExt;
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("dune-project"), "(lang dune 3.22)\n").unwrap();
    fs::create_dir(project.path().join("_build")).unwrap();
    let lock = fs::File::create(project.path().join("_build/.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    assert!(crate::dune::Layout::load(project.path(), &[], Duration::from_secs(10)).is_ok());
    fs::write(project.path().join("dune-project"), "(lang dune invalid)\n").unwrap();
    let error = crate::dune::Layout::load(project.path(), &[], Duration::from_secs(10))
        .err()
        .expect("invalid Dune workspace accepted");
    assert_eq!(error.kind, ErrorKind::InvalidConfiguration);
    assert!(error.message.contains("Dune workspace discovery failed"));
}

#[test]
fn invalid_dune_workspace_does_not_masquerade_as_lock_contention() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("dune-project"), "(lang dune invalid)\n").unwrap();
    let error = crate::dune::Layout::load(project.path(), &[], Duration::from_secs(20))
        .err()
        .expect("invalid Dune workspace accepted");
    assert_eq!(error.kind, ErrorKind::InvalidConfiguration);
    assert!(error.message.contains("Dune workspace discovery failed"));
    assert!(!error.message.contains("lock timed out"));
}

#[test]
fn dune_qualified_subdirs_and_exclusions_define_the_source_set() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::write(
        project.path().join("dune"),
        "(include_subdirs qualified)\n(dirs :standard \\ omitted)\n(rocq.theory (name Demo))\n",
    )
    .unwrap();
    for dir in ["Nested", "omitted", ".rocq-mcp/session", "_build/default"] {
        fs::create_dir_all(project.path().join(dir)).unwrap();
    }
    for file in [
        "Nested/Included.v",
        "omitted/Excluded.v",
        ".rocq-mcp/session/source.v",
        "_build/default/Generated.v",
    ] {
        fs::write(project.path().join(file), "Theorem t : True. Admitted.\n").unwrap();
    }
    let root = fs::canonicalize(project.path()).unwrap();
    let layout = crate::dune::Layout::load(&root, &[], Duration::from_secs(20)).unwrap();
    assert_eq!(layout.files(), vec![root.join("Nested/Included.v")]);
}

#[test]
fn dune_theory_recursively_parses_its_own_stanza_and_explicit_modules() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.22)\n(using rocq 0.12)\n",
    )
    .unwrap();
    fs::create_dir_all(project.path().join("_build/default")).unwrap();
    fs::write(
        project.path().join("dune"),
        "; (rocq.theory (name Wrong.Library) (modules Ghost))\n\
         (rule (target bogus) (action (write-file bogus \"a literal (name Wrong.Library)\")))\n\
         (rocq.theory\n\
           (name \"Demo.Library\")\n\
           (modules\n\
             Main\n\
             Nested))\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem main : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Nested.v"),
        "Theorem nested : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Ignored.v"),
        "Theorem ignored : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("_build/default/Generated.v"),
        "Theorem generated : True. Admitted.\n",
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let declarations = all_declarations(&engine(state.path()), project.path()).unwrap();
    assert_eq!(
        declarations
            .iter()
            .map(|item| item.identity.clone())
            .collect::<Vec<_>>(),
        vec![
            id(&["Demo", "Library", "Main"], "main"),
            DeclarationIdentity {
                file: crate::FileId("Nested.v".into()),
                qualified_path: vec![
                    "Demo".into(),
                    "Library".into(),
                    "Nested".into(),
                    "nested".into(),
                ],
            },
        ],
        "only modules belonging to the rocq.theory stanza may define this library"
    );
}
