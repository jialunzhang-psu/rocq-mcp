//! T2b public catalog, logical-layout, and declaration-placement tests.

use crate::repository::ProofRepository;
use rocq_engine::{
    CandidateRejection, CanonicalTactic, ClosedProof, DeclarationIdentity, DeclarationKind,
    Diagnostic, Engine, EngineConfig, ErrorKind, FileReplacement, LexicalScope, LogicalLibrary,
    NewDeclaration, OpenDeclaration, ProofLifecycle, Query, QueryResult, RecoveredProof,
    RejectionPhase, SolvedCandidate, SourceAnchor, TrustAuditResult, TrustBaseline,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn engine(state: &Path) -> Engine {
    Engine::new(EngineConfig {
        state_parent: state.to_owned(),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap()
}

fn id(library: &[&str], modules: &[&str], constant: &str) -> DeclarationIdentity {
    DeclarationIdentity {
        library: LogicalLibrary(library.iter().map(|x| (*x).into()).collect()),
        modules: modules.iter().map(|x| (*x).into()).collect(),
        constant: constant.into(),
    }
}

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.unwrap();
        if entry.path().starts_with(root.join("_build")) || !entry.file_type().is_file() {
            continue;
        }
        out.insert(
            entry.path().strip_prefix(root).unwrap().to_owned(),
            fs::read(entry.path()).unwrap(),
        );
    }
    out
}

fn candidate(identity: DeclarationIdentity) -> SolvedCandidate {
    SolvedCandidate::new(
        OpenDeclaration {
            kind: DeclarationKind::Theorem,
            identity,
            anchor: SourceAnchor {
                source_digest: [1; 32],
                normalized_statement: "True".into(),
                context: vec![],
                old_body_digest: [2; 32],
            },
        },
        vec![CanonicalTactic("exact I.".into())],
        TrustBaseline::new(vec![], vec![], vec![], [3; 32]).unwrap(),
    )
    .unwrap()
}

#[test]
fn coqproject_q_r_i_mappings_are_unique_and_fail_closed_when_unsafe_or_overlapping() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("q")).unwrap();
    fs::create_dir_all(project.path().join("r/Nested")).unwrap();
    fs::create_dir_all(project.path().join("include")).unwrap();
    fs::write(
        project.path().join("q/A.v"),
        "Theorem qa : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("r/Nested/B.v"),
        "Theorem rb : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("_CoqProject"),
        "-Q q QLib\n-R r RLib\n-I include\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let catalog = engine(state.path()).catalog(project.path()).unwrap();
    assert!(
        catalog
            .declarations
            .iter()
            .any(|x| x.identity == id(&["QLib", "A"], &[], "qa"))
    );
    assert!(
        catalog
            .declarations
            .iter()
            .any(|x| x.identity == id(&["RLib", "Nested", "B"], &[], "rb"))
    );

    let unsafe_project = tempfile::tempdir().unwrap();
    fs::write(
        unsafe_project.path().join("A.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        unsafe_project.path().join("_CoqProject"),
        "-Q ../escape Bad\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    assert!(engine(state.path()).catalog(unsafe_project.path()).is_err());

    let overlapping = tempfile::tempdir().unwrap();
    fs::create_dir_all(overlapping.path().join("sub")).unwrap();
    fs::write(
        overlapping.path().join("sub/A.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        overlapping.path().join("_CoqProject"),
        "-Q . Root\n-Q sub Root.Sub\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    assert!(engine(state.path()).catalog(overlapping.path()).is_err());
}

#[test]
fn dune_theory_and_library_module_section_boundaries_stay_structured() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune"),
        "(rocq.theory (name Demo.Lib) (modules Main))\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "(* nested (* comment with Module Fake. *) *)\nModule Outer.\nSection S.\nModule Inner.\nTheorem λ : True. Admitted.\nEnd Inner.\nEnd S.\nEnd Outer.\nDefinition quoted := \"End NotAScope.\".\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let catalog = engine(state.path()).catalog(project.path()).unwrap();
    let item = catalog
        .declarations
        .iter()
        .find(|x| x.identity.constant == "λ")
        .unwrap();
    assert_eq!(
        item.identity.library,
        LogicalLibrary(vec!["Demo".into(), "Lib".into(), "Main".into()])
    );
    assert_eq!(item.identity.modules, vec!["Outer", "Inner"]);
    assert_eq!(
        item.context,
        vec![
            LexicalScope::Module("Outer".into()),
            LexicalScope::Section("S".into()),
            LexicalScope::Module("Inner".into())
        ]
    );
    assert!(
        catalog
            .declarations
            .iter()
            .any(|x| x.identity.constant == "quoted")
    );
}

#[test]
fn completed_and_unfinished_declarations_have_structured_lifecycles() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem done : True. Proof. exact I. Qed.\nDefinition def : True := I.\nTheorem admitted : True. Admitted.\nTheorem aborted : True. Abort.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    let catalog = engine.catalog(project.path()).unwrap();
    for name in ["done", "def"] {
        let identity = catalog
            .declarations
            .iter()
            .find(|x| x.identity.constant == name)
            .unwrap()
            .identity
            .clone();
        let state = engine.open(project.path(), identity).unwrap();
        assert_eq!(state.lifecycle, ProofLifecycle::Completed);
        assert!(state.attempt.is_none());
    }
    for name in ["admitted", "aborted"] {
        let identity = catalog
            .declarations
            .iter()
            .find(|x| x.identity.constant == name)
            .unwrap()
            .identity
            .clone();
        let state = engine.open(project.path(), identity).unwrap();
        assert_eq!(state.lifecycle, ProofLifecycle::Open);
        assert!(state.attempt.is_some());
    }
}

#[test]
fn declare_is_logical_and_side_effect_free_before_close() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("_CoqProject"), "-Q . Demo\n").unwrap();
    fs::write(project.path().join("Main.v"), "Section S.\nEnd S.\n").unwrap();
    let before = tree_bytes(project.path());
    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    let declaration = NewDeclaration {
        kind: DeclarationKind::Theorem,
        identity: id(&["Demo", "Created"], &[], "fresh"),
        context: vec![],
        statement: "True".into(),
    };
    let opened = engine.declare(project.path(), declaration).unwrap();
    assert!(opened.attempt.is_some());
    assert_eq!(
        tree_bytes(project.path()),
        before,
        "declare must not create source or an Admitted placeholder"
    );

    let empty_library = NewDeclaration {
        identity: id(&[], &[], "bad"),
        ..NewDeclaration {
            kind: DeclarationKind::Theorem,
            identity: id(&["x"], &[], "x"),
            context: vec![],
            statement: "True".into(),
        }
    };
    assert!(engine.declare(project.path(), empty_library).is_err());
    let missing_context = NewDeclaration {
        kind: DeclarationKind::Theorem,
        identity: id(&["Demo", "Main"], &[], "missing"),
        context: vec![LexicalScope::Section("Missing".into())],
        statement: "True".into(),
    };
    assert!(engine.declare(project.path(), missing_context).is_err());
}

#[test]
fn recovered_phases_bind_by_logical_identity_without_trace_or_storage_leakage() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("Main.v"), "Theorem solved : True. Admitted.\nTheorem rejected : True. Admitted.\nTheorem closed : True. Admitted.\n").unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let solved = candidate(id(&["Main"], &[], "solved"));
    repo.persist(solved).unwrap();
    let rejected = candidate(id(&["Main"], &[], "rejected"));
    let rejected_id = rejected.declaration.identity.clone();
    repo.persist(rejected.clone()).unwrap();
    repo.reject(
        &rejected_id,
        CandidateRejection::new(
            rejected,
            RejectionPhase::TrustAudit,
            vec![Diagnostic::new("reject", "no trust").unwrap()],
        )
        .unwrap(),
    )
    .unwrap();
    let closed = candidate(id(&["Main"], &[], "closed"));
    let closed_id = closed.declaration.identity.clone();
    repo.persist(closed.clone()).unwrap();
    repo.promote(
        &closed_id,
        ClosedProof::new(
            closed,
            TrustAuditResult::ClosedUnderFrozenBaseline,
            vec![
                FileReplacement::with_original(PathBuf::from("Main.v"), None, b"x".to_vec())
                    .unwrap(),
            ],
        )
        .unwrap(),
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    let catalog = engine.catalog(project.path()).unwrap();
    for (name, expected) in [
        ("solved", ProofLifecycle::Pending),
        ("rejected", ProofLifecycle::Rejected),
        ("closed", ProofLifecycle::Completed),
    ] {
        assert_eq!(
            catalog
                .declarations
                .iter()
                .find(|x| x.identity.constant == name)
                .unwrap()
                .status,
            expected
        );
    }
    let solved_error = engine
        .open(project.path(), id(&["Main"], &[], "solved"))
        .unwrap_err();
    assert_eq!(solved_error.kind, ErrorKind::DeclarationChanged);
    let rejected = engine
        .open(project.path(), id(&["Main"], &[], "rejected"))
        .unwrap();
    assert!(
        matches!(rejected.recovery, Some(RecoveredProof::Rejected { .. }))
            && rejected.attempt.is_none()
    );
    let closed_error = engine
        .open(project.path(), id(&["Main"], &[], "closed"))
        .unwrap_err();
    assert_eq!(closed_error.kind, ErrorKind::DeclarationChanged);
    assert!(!format!("{rejected:?}").contains(".rocq-engine"));
}

#[test]
fn source_contract_has_no_path_based_new_declaration_or_regex_identity_parser() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();
    let parser_source =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/source_index.rs")).unwrap();
    let repository =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/repository.rs")).unwrap();
    let new_decl = &repository[repository.find("pub struct NewDeclaration").unwrap()
        ..repository.find("/// Semantic source anchor").unwrap()];
    assert!(!new_decl.contains("PathBuf") && !new_decl.contains("transparent"));
    assert!(!source.contains("NewTheorem") && !source.contains("regex::"));
    assert!(
        parser_source.contains("fn parse_file(")
            && parser_source.contains("sentence_ranges(source)?")
    );
    assert!(
        !source.contains("identity: source["),
        "byte offsets may locate text but never define identity"
    );
}

#[test]
fn generated_and_vcs_trees_are_not_logical_project_sources() {
    let project = tempfile::tempdir().unwrap();
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
    let catalog = engine(state.path()).catalog(project.path()).unwrap();
    assert_eq!(
        catalog
            .declarations
            .iter()
            .map(|item| item.identity.constant.as_str())
            .collect::<Vec<_>>(),
        ["real"],
        "build products and VCS metadata must never become project declarations"
    );
}

#[test]
fn dune_theory_recursively_parses_its_own_stanza_and_explicit_modules() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("_build/default")).unwrap();
    fs::write(
        project.path().join("dune"),
        "; (rocq.theory (name Wrong.Library) (modules Ghost))\n\
         (rule (targets \"a literal (name Wrong.Library)\"))\n\
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
    let catalog = engine(state.path()).catalog(project.path()).unwrap();
    assert_eq!(
        catalog
            .declarations
            .iter()
            .map(|item| item.identity.clone())
            .collect::<Vec<_>>(),
        vec![
            id(&["Demo", "Library", "Main"], &[], "main"),
            id(&["Demo", "Library", "Nested"], &[], "nested"),
        ],
        "only modules belonging to the rocq.theory stanza may define this library"
    );
}

#[test]
fn coqproject_honors_quoted_paths_comments_and_include_directories() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir_all(project.path().join("source files")).unwrap();
    fs::create_dir_all(project.path().join("include files")).unwrap();
    fs::write(
        project.path().join("source files/Main.v"),
        "Theorem mapped : True. Admitted.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("_CoqProject"),
        "# A comment may contain a fake -Q nonexistent Wrong mapping.\n\
         -I \"include files\" # and this fake -R must be ignored too\n\
         -Q \"source files\" Quoted.Library\n",
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let catalog = engine(state.path()).catalog(project.path()).unwrap();
    assert_eq!(catalog.declarations.len(), 1);
    assert_eq!(
        catalog.declarations[0].identity,
        id(&["Quoted", "Library", "Main"], &[], "mapped")
    );
}

#[test]
fn parser_tracks_direct_proofs_unfinished_definitions_term_definitions_and_normalized_headers() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem direct : True.\nexact I.\nQed.\n\
         Definition pending : True.\nAdmitted.\n\
         Definition term (* ignored (* nested *) comment *)\n : True := I.\n\
         Theorem normalized\n (* comment *) :\n True.\nAdmitted.\n",
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    let catalog = engine.catalog(project.path()).unwrap();
    let by_name = |name: &str| {
        catalog
            .declarations
            .iter()
            .find(|item| item.identity.constant == name)
            .unwrap()
    };
    assert_eq!(by_name("direct").status, ProofLifecycle::Completed);
    assert_eq!(by_name("pending").status, ProofLifecycle::Open);
    assert_eq!(by_name("term").status, ProofLifecycle::Completed);
    assert_eq!(
        by_name("normalized").statement,
        "Theorem normalized : True",
        "comments and formatting must not affect the frozen declaration header"
    );
    let QueryResult::Text(direct_proof) = engine
        .query(
            project.path(),
            None,
            Query::Proof {
                name: "direct".into(),
            },
        )
        .unwrap()
    else {
        panic!("proof query must return source text");
    };
    assert!(direct_proof.contains("exact I.") && direct_proof.contains("Qed."));
}

#[test]
fn module_type_and_module_import_are_not_misparsed_as_module_scopes() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Module Type API.\nParameter promised : True.\nEnd API.\n\
         Module Implementation.\nTheorem inside : True. Admitted.\nEnd Implementation.\n\
         Module Import PublicAPI := Implementation.\n\
         Theorem top_level : True. Admitted.\n",
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let catalog = engine(state.path()).catalog(project.path()).unwrap();
    assert_eq!(catalog.declarations.len(), 2);
    assert_eq!(
        catalog
            .declarations
            .iter()
            .find(|item| item.identity.constant == "inside")
            .unwrap()
            .identity
            .modules,
        vec!["Implementation"]
    );
    assert!(
        catalog
            .declarations
            .iter()
            .any(|item| item.identity == id(&["Main"], &[], "top_level"))
    );
}
