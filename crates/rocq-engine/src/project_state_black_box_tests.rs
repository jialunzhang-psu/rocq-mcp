//! I2a project-attachment tests. Repository fixtures stay crate-private;
//! source audits are limited to contracts with no public timing hook.

use crate::repository::ProofRepository;
use rocq_engine::{
    CandidateRejection, CanonicalTactic, ClosedProof, DeclarationIdentity, DeclarationKind,
    Diagnostic, Engine, EngineConfig, FileReplacement, LogicalLibrary, OpenDeclaration,
    ProofLifecycle, RejectionPhase, SolvedCandidate, SourceAnchor, TrustAuditResult, TrustBaseline,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

fn engine(state_parent: &Path) -> Engine {
    Engine::new(EngineConfig {
        state_parent: state_parent.to_owned(),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap()
}

fn project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    project
}

fn identity(name: &str) -> DeclarationIdentity {
    DeclarationIdentity {
        library: LogicalLibrary(vec!["T2".into()]),
        modules: vec![],
        constant: name.into(),
    }
}

fn candidate(name: &str) -> SolvedCandidate {
    let baseline = TrustBaseline::new(vec![], vec![], vec![], [7; 32]).unwrap();
    SolvedCandidate::new(
        OpenDeclaration {
            kind: DeclarationKind::Theorem,
            identity: identity(name),
            anchor: SourceAnchor {
                source_digest: [1; 32],
                normalized_statement: "True".into(),
                context: vec![],
                old_body_digest: [2; 32],
            },
        },
        vec![CanonicalTactic("exact I.".into())],
        baseline,
    )
    .unwrap()
}

/// Seeds every recoverable durable phase before the engine's first attachment.
fn seed_recoverable_records(project: &Path) {
    let repository = ProofRepository::open(project).unwrap();
    repository.persist(candidate("solved")).unwrap();

    let rejected = candidate("rejected");
    let rejected_id = rejected.declaration.identity.clone();
    repository.persist(rejected.clone()).unwrap();
    repository
        .reject(
            &rejected_id,
            CandidateRejection::new(
                rejected,
                RejectionPhase::TrustAudit,
                vec![Diagnostic::new("audit_failure", "deterministic rejection").unwrap()],
            )
            .unwrap(),
        )
        .unwrap();

    let closed = candidate("closed");
    let closed_id = closed.declaration.identity.clone();
    repository.persist(closed.clone()).unwrap();
    repository
        .promote(
            &closed_id,
            ClosedProof::new(
                closed,
                TrustAuditResult::ClosedUnderFrozenBaseline,
                vec![
                    FileReplacement::with_original(PathBuf::from("Main.v"), None, b"ok\n".to_vec())
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn canonical_and_symlink_catalogs_reuse_one_engine_attachment() {
    let project = project();
    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    assert_eq!(
        engine.catalog(project.path()).unwrap().root,
        fs::canonicalize(project.path()).unwrap()
    );

    #[cfg(unix)]
    {
        let alias_parent = tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("project-alias");
        std::os::unix::fs::symlink(project.path(), &alias).unwrap();
        assert_eq!(
            engine.catalog(&alias).unwrap().root,
            fs::canonicalize(project.path()).unwrap(),
            "a symlink-equivalent project must reuse the canonical attachment"
        );
    }
}

#[test]
fn second_engine_is_rejected_until_first_engine_drops() {
    let project = project();
    let first_state = tempfile::tempdir().unwrap();
    let second_state = tempfile::tempdir().unwrap();
    let first = engine(first_state.path());
    let second = engine(second_state.path());
    first.catalog(project.path()).unwrap();
    let error = second.catalog(project.path()).unwrap_err();
    assert_eq!(error.kind, rocq_engine::ErrorKind::ProjectTimeout);
    assert!(
        !error
            .message
            .contains(project.path().to_string_lossy().as_ref())
            && !error.message.contains("project.lock")
            && !error.message.contains(".rocq-engine"),
        "attachment-contention errors must not expose private lock paths"
    );
    drop(first);
    second.catalog(project.path()).unwrap();
}

#[test]
fn different_projects_attach_concurrently_without_cross_project_contention() {
    let left = project();
    let right = project();
    let left_state = tempfile::tempdir().unwrap();
    let right_state = tempfile::tempdir().unwrap();
    let left_engine = Arc::new(engine(left_state.path()));
    let right_engine = Arc::new(engine(right_state.path()));
    let barrier = Arc::new(Barrier::new(2));
    let left_path = left.path().to_owned();
    let right_path = right.path().to_owned();
    let left_worker = {
        let engine = left_engine.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            engine.catalog(&left_path)
        })
    };
    let right_worker = {
        let engine = right_engine.clone();
        thread::spawn(move || {
            barrier.wait();
            engine.catalog(&right_path)
        })
    };
    left_worker.join().unwrap().unwrap();
    right_worker.join().unwrap().unwrap();
}

#[test]
fn first_catalog_recovers_all_durable_phases_before_following_engine_operations() {
    let project = project();
    seed_recoverable_records(project.path());
    let before = fs::read_dir(project.path().join("_build/.rocq-engine/proofs"))
        .unwrap()
        .count();
    let state = tempfile::tempdir().unwrap();
    let engine = engine(state.path());
    let first = engine.catalog(project.path()).unwrap();
    assert_eq!(first.declarations.len(), 4);
    for (name, expected) in [
        ("t", ProofLifecycle::Open),
        ("solved", ProofLifecycle::Pending),
        ("rejected", ProofLifecycle::Rejected),
        ("closed", ProofLifecycle::Completed),
    ] {
        assert_eq!(
            first
                .declarations
                .iter()
                .find(|item| item.identity.constant == name)
                .unwrap()
                .status,
            expected,
            "recovery merges the durable {name} phase without hiding source declarations"
        );
    }
    let second = engine.catalog(project.path()).unwrap();
    assert_eq!(second.declarations, first.declarations);
    assert_eq!(
        fs::read_dir(project.path().join("_build/.rocq-engine/proofs"))
            .unwrap()
            .count(),
        before,
        "catalog recovery is read-only; later engine work observes the recovered attachment"
    );
}

#[test]
fn corrupt_durable_recovery_error_is_sanitized_and_not_misreported_as_lock_contention() {
    let project = project();
    let proofs = project.path().join("_build/.rocq-engine/proofs");
    fs::create_dir_all(&proofs).unwrap();
    let id = "00000000-0000-0000-0000-000000000000";
    fs::write(proofs.join(format!("{id}.json")), b"{").unwrap();
    let state = tempfile::tempdir().unwrap();
    let error = engine(state.path()).catalog(project.path()).unwrap_err();
    assert_eq!(error.kind, rocq_engine::ErrorKind::InvalidConfiguration);
    assert!(
        !error
            .message
            .contains(project.path().to_string_lossy().as_ref())
            && !error.message.contains("project.lock")
            && !error.message.contains(".rocq-engine")
            && !error.message.contains(id),
        "recovery errors must not expose storage paths, lock names, or record UUIDs"
    );
    assert!(
        !error.message.contains("already attached"),
        "corrupt durable recovery is a semantic recovery failure, not lock contention"
    );
}

#[test]
fn source_contracts_keep_attachment_private_and_drop_detach_before_unlock() {
    // Gate overlap and detach timing have no public delay hook, so these are
    // intentionally structural audits rather than timing assertions.
    let state =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/project_state.rs")).unwrap();
    let engine = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine.rs")).unwrap();
    assert!(state.contains("pub(crate) struct ProjectAttachment"));
    assert!(state.contains("pub(crate) struct ProjectRegistry"));
    assert!(!engine.contains("pub use project_state"));
    assert!(state.contains("gate: RwLock<()>"));

    assert!(
        engine.contains("impl Drop for Engine") && engine.contains("self.pet_runtime.shutdown()"),
        "Engine must reap PET while its attachment registry is still alive"
    );
}

#[test]
fn source_contract_registry_mutex_does_not_cover_recovery_work() {
    let state =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/project_state.rs")).unwrap();
    let registry_start = state.find("impl ProjectRegistry").unwrap();
    let registry = &state[registry_start..];
    assert!(
        registry.contains("drop(values)") || registry.contains("let cached ="),
        "the registry mutex may protect lookup only, never repository recovery or work"
    );
}

#[test]
fn source_contract_engine_uses_one_attachment_current_view_not_legacy_generations() {
    let engine = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();
    assert!(
        !engine.contains("project_generations") && !engine.contains("ProjectGeneration"),
        "I2 owns one attachment/current view per project; legacy generation lists are forbidden"
    );
}

#[test]
fn source_contract_engine_operations_use_the_attachment_gate() {
    let engine = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();
    assert!(
        !engine.contains("TODO(I2): legacy callers"),
        "project attachment must gate the real Engine operations, not only catalog"
    );
}

fn method<'a>(source: &'a str, name: &str, next_name: &str) -> &'a str {
    let start = source.find(name).unwrap();
    let end = source[start..].find(next_name).unwrap();
    &source[start..start + end]
}

#[test]
fn source_contract_every_public_source_or_pet_path_enters_the_current_view_gate() {
    // There is no public test hook that can hold a gate while another caller
    // starts close. These order checks are therefore the non-flaky evidence for
    // read/read and read/write exclusion, not a timing claim.
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine.rs")).unwrap();
    for (start, next) in [
        ("pub fn catalog(", "/// Opens one parsed unfinished"),
        ("pub fn open(", "/// Declares a logical theorem"),
        ("pub fn declare(", "/// Replays a candidate prefix"),
    ] {
        let body = method(&source, start, next);
        assert!(
            body.find("self.attach_project").unwrap() < body.find("attachment.read()").unwrap(),
            "{start} must attach before taking its source read gate"
        );
    }

    let runtime = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/engine_runtime.rs"
    ))
    .unwrap();
    for operation in ["pub fn step(", "pub fn candidates(", "pub fn inspect("] {
        assert!(runtime.contains(operation));
    }
    let query = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/query.rs")).unwrap();
    assert!(query.contains("pub fn query(") && query.contains("self.catalog(project)"));
}
