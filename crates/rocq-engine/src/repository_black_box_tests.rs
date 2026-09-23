//! Internal durable-candidate repository tests. The repository is deliberately
//! absent from the public engine API; restarts still reopen its on-disk records.

use crate::repository::{ProofPhase, ProofRepository, RepositoryError};
use rocq_engine::{
    CandidateRejection, CanonicalTactic, ClosedProof, DeclarationIdentity, DeclarationKind,
    Diagnostic, FileReplacement, LexicalScope, LogicalLibrary, OpenDeclaration, RejectionPhase,
    SolvedCandidate, SourceAnchor, TrustAuditResult, TrustBaseline,
};
use sha2::Digest;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};
use uuid::Uuid;

fn identity(library: &[&str], modules: &[&str], constant: &str) -> DeclarationIdentity {
    DeclarationIdentity {
        library: LogicalLibrary(library.iter().map(|x| (*x).into()).collect()),
        modules: modules.iter().map(|x| (*x).into()).collect(),
        constant: constant.into(),
    }
}
fn candidate(id: DeclarationIdentity, commands: &[&str]) -> SolvedCandidate {
    let baseline = TrustBaseline::new(
        vec![("A".into(), "True".into())],
        vec![(LogicalLibrary(vec!["Ext".into()]), [4; 32])],
        vec!["old_admission".into()],
        [5; 32],
    )
    .unwrap();
    SolvedCandidate::new(
        OpenDeclaration {
            kind: DeclarationKind::Theorem,
            anchor: SourceAnchor {
                source_digest: [1; 32],
                normalized_statement: "True".into(),
                // Design note: module scopes are part of the durable declaration
                // address, so valid fixture candidates must carry them too.
                context: id
                    .modules
                    .iter()
                    .cloned()
                    .map(LexicalScope::Module)
                    .collect(),
                old_body_digest: [2; 32],
            },
            identity: id,
        },
        commands
            .iter()
            .map(|x| CanonicalTactic((*x).into()))
            .collect(),
        baseline,
    )
    .unwrap()
}

fn declaration(identity: DeclarationIdentity, context: Vec<LexicalScope>) -> OpenDeclaration {
    OpenDeclaration {
        kind: DeclarationKind::Theorem,
        identity,
        anchor: SourceAnchor {
            source_digest: [1; 32],
            normalized_statement: "True".into(),
            context,
            old_body_digest: [2; 32],
        },
    }
}

fn checksum_valid_rewrite(path: &std::path::Path, phase: ProofPhase) {
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let id: Uuid = serde_json::from_value(record["id"].clone()).unwrap();
    let checksum: [u8; 32] =
        sha2::Sha256::digest(serde_json::to_vec(&(1u32, id, &phase)).unwrap()).into();
    record["phase"] = serde_json::to_value(phase).unwrap();
    record["checksum"] = serde_json::to_value(checksum).unwrap();
    fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
}
fn closed(c: SolvedCandidate) -> ClosedProof {
    ClosedProof::new(
        c,
        TrustAuditResult::ClosedUnderFrozenBaseline,
        vec![
            FileReplacement::with_original(
                PathBuf::from("theories/T.v"),
                Some(vec![7]),
                b"Theorem t : True. Proof. exact I. Qed.\n".to_vec(),
            )
            .unwrap(),
        ],
    )
    .unwrap()
}
fn rejection(c: SolvedCandidate, message: &str) -> CandidateRejection {
    CandidateRejection::new(
        c,
        RejectionPhase::TrustAudit,
        vec![Diagnostic::new("axiom_dependency_out_of_scope", message).unwrap()],
    )
    .unwrap()
}

fn records(project: &std::path::Path) -> Vec<PathBuf> {
    fs::read_dir(project.join("_build/.rocq-engine/proofs"))
        .unwrap()
        .map(|x| x.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}

#[test]
fn structured_identity_never_flattens_library_module_boundaries() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let left = identity(&["A", "B"], &[], "t");
    let right = identity(&["A"], &["B"], "t");
    repo.persist(candidate(left.clone(), &["exact I."]))
        .unwrap();
    repo.persist(candidate(right.clone(), &["exact I."]))
        .unwrap();
    let scan = repo.scan().unwrap();
    assert_eq!(scan.len(), 2);
    assert!(scan.contains_key(&left) && scan.contains_key(&right));
}

#[test]
fn solved_closed_rejected_delete_roundtrip_is_self_contained_and_uuid_stable() {
    let project = tempfile::tempdir().unwrap();
    let id = identity(&["L"], &["M"], "t");
    let c = candidate(id.clone(), &["intro H.", "exact H."]);
    let solved = ProofRepository::open(project.path())
        .unwrap()
        .persist(c.clone())
        .unwrap();
    let record_name = records(project.path())[0].file_name().unwrap().to_owned();
    drop(solved);
    let repo = ProofRepository::open(project.path()).unwrap();
    let recovered = repo.scan().unwrap();
    let pending = &recovered[&id];
    match &pending.phase {
        ProofPhase::Solved(saved) => assert_eq!(saved, &c),
        _ => panic!("must recover complete solved candidate"),
    }
    repo.promote(&id, closed(c.clone())).unwrap();
    assert_eq!(records(project.path())[0].file_name().unwrap(), record_name);
    assert_eq!(records(project.path()).len(), 1);
    assert!(matches!(
        repo.scan().unwrap()[&id].phase,
        ProofPhase::Closed(_)
    ));
    assert!(matches!(
        repo.reject(&id, rejection(c.clone(), "late rejection")),
        Err(RepositoryError::IllegalTransition)
    ));
    repo.acknowledge_closed(&id).unwrap();
    assert!(repo.scan().unwrap().is_empty());
    assert!(records(project.path()).is_empty());
}

#[test]
fn replacement_plan_and_canonical_commands_survive_restart_exactly() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let id = identity(&["L"], &[], "t");
    let c = candidate(id.clone(), &["idtac \"dot.\".", "exact I."]);
    repo.persist(c.clone()).unwrap();
    repo.promote(&id, closed(c)).unwrap();
    drop(repo);
    let phase = &ProofRepository::open(project.path())
        .unwrap()
        .scan()
        .unwrap()[&id]
        .phase;
    let ProofPhase::Closed(proof) = phase else {
        panic!("closed")
    };
    assert_eq!(proof.candidate.commands[0].0, "idtac \"dot.\".");
    assert_eq!(proof.replacements.len(), 1);
    let r = &proof.replacements[0];
    assert_eq!(r.relative(), PathBuf::from("theories/T.v"));
    assert_eq!(
        r.old_digest,
        Some(<[u8; 32]>::from(sha2::Sha256::digest([7])))
    );
    assert_eq!(
        r.new_digest(),
        <[u8; 32]>::from(sha2::Sha256::digest(&r.contents))
    );
    assert!(r.contents.ends_with(b"Qed.\n"));
}

#[test]
fn corruption_truncation_unknown_version_and_checksum_fail_closed() {
    for mutation in ["truncate", "version", "checksum"] {
        let project = tempfile::tempdir().unwrap();
        let repo = ProofRepository::open(project.path()).unwrap();
        repo.persist(candidate(identity(&["L"], &[], "t"), &["exact I."]))
            .unwrap();
        let path = records(project.path()).pop().unwrap();
        match mutation {
            "truncate" => fs::write(&path, b"{").unwrap(),
            "version" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["version"] = serde_json::json!(999);
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "checksum" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["checksum"][0] = serde_json::json!(255);
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            ProofRepository::open(project.path()).is_err(),
            "{mutation} must fail closed"
        );
    }
}

#[test]
fn duplicate_identity_recovery_fails_closed_but_distinct_identities_recover() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let same = identity(&["L"], &[], "t");
    repo.persist(candidate(same.clone(), &["exact I."]))
        .unwrap();
    assert!(matches!(
        repo.persist(candidate(same.clone(), &["idtac.", "exact I."])),
        Err(RepositoryError::Duplicate)
    ));
    // A torn/hostile duplicate is also rejected on startup: independently make
    // a checksum-valid UUID record containing the same semantic identity.
    let original = records(project.path()).pop().unwrap();
    let original_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&original).unwrap()).unwrap();
    let phase: ProofPhase = serde_json::from_value(original_value["phase"].clone()).unwrap();
    let id = Uuid::now_v7();
    let checksum: [u8; 32] =
        sha2::Sha256::digest(serde_json::to_vec(&(1u32, id, &phase)).unwrap()).into();
    let duplicate = serde_json::json!({"version":1,"id":id,"phase":phase,"checksum":checksum});
    fs::write(
        original.with_file_name(format!("{id}.json")),
        serde_json::to_vec(&duplicate).unwrap(),
    )
    .unwrap();
    assert!(
        matches!(
            ProofRepository::open(project.path()),
            Err(RepositoryError::Duplicate)
        ),
        "two valid durable candidates may not bind one identity"
    );
}

#[test]
fn repository_recovery_view_does_not_expose_private_storage_paths() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    repo.persist(candidate(identity(&["L"], &[], "t"), &["exact I."]))
        .unwrap();
    let recovered = repo.scan().unwrap();
    let diagnostic = format!("{recovered:?}");
    assert!(
        !diagnostic.contains(project.path().to_string_lossy().as_ref()),
        "semantic recovery view must not expose private proof-record path"
    );
}

#[test]
fn persist_is_linearized_duplicate_rejected_and_distinct_concurrent_records_survive() {
    let project = tempfile::tempdir().unwrap();
    let repo = Arc::new(ProofRepository::open(project.path()).unwrap());
    let gate = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for command in ["exact I.", "idtac."] {
        let repo = repo.clone();
        let gate = gate.clone();
        workers.push(thread::spawn(move || {
            gate.wait();
            repo.persist(candidate(identity(&["L"], &[], "t"), &[command]))
        }));
    }
    let results: Vec<_> = workers.into_iter().map(|x| x.join().unwrap()).collect();
    assert_eq!(
        results.iter().filter(|x| x.is_ok()).count(),
        1,
        "same identity has exactly one linearized durable candidate"
    );
    assert_eq!(
        results
            .iter()
            .filter(|x| matches!(x, Err(RepositoryError::Duplicate)))
            .count(),
        1
    );
    let gate = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for name in ["u", "v"] {
        let repo = repo.clone();
        let gate = gate.clone();
        workers.push(thread::spawn(move || {
            gate.wait();
            repo.persist(candidate(identity(&["L"], &[], name), &["exact I."]))
        }));
    }
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert_eq!(
        repo.scan().unwrap().len(),
        3,
        "different identities are never lost under concurrent persist"
    );
}

#[test]
fn transition_machine_rejects_substitution_and_illegal_delete_or_repromotion() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let id = identity(&["L"], &[], "t");
    let c = candidate(id.clone(), &["exact I."]);
    repo.persist(c.clone()).unwrap();
    let substituted = candidate(id.clone(), &["idtac.", "exact I."]);
    assert!(matches!(
        repo.promote(&id, closed(substituted.clone())),
        Err(RepositoryError::Invalid)
    ));
    assert!(matches!(
        repo.reject(&id, rejection(substituted, "wrong candidate")),
        Err(RepositoryError::Invalid)
    ));
    assert!(matches!(
        repo.acknowledge_closed(&id),
        Err(RepositoryError::IllegalTransition)
    ));
    repo.reject(&id, rejection(c.clone(), "rejected")).unwrap();
    assert!(matches!(
        repo.promote(&id, closed(c.clone())),
        Err(RepositoryError::IllegalTransition)
    ));
    assert!(matches!(
        repo.reject(&id, rejection(c, "again")),
        Err(RepositoryError::IllegalTransition)
    ));
    assert!(matches!(
        repo.acknowledge_closed(&id),
        Err(RepositoryError::IllegalTransition)
    ));
}

#[test]
fn constructors_derive_unforgeable_identities_and_validate_replacement_paths() {
    let baseline = TrustBaseline::new(
        vec![
            ("b".into(), "T".into()),
            ("a".into(), "U".into()),
            ("a".into(), "U".into()),
        ],
        vec![],
        vec!["z".into(), "z".into(), "a".into()],
        [9; 32],
    )
    .unwrap();
    assert_eq!(
        baseline.explicit_axioms,
        vec![("a".into(), "U".into()), ("b".into(), "T".into())]
    );
    assert_eq!(baseline.forbidden_locals, vec!["a", "z"]);
    let c = candidate(identity(&["L"], &[], "t"), &["exact I."]);
    assert_ne!(c.content_identity(), [0; 32]);
    assert_ne!(c.baseline.identity(), [0; 32]);
    assert!(FileReplacement::with_original(PathBuf::from("../escape.v"), None, vec![]).is_err());
    assert!(FileReplacement::with_original(PathBuf::from("/absolute.v"), None, vec![]).is_err());
}

#[test]
fn malformed_filename_unknown_field_oversize_and_orphan_temp_fail_closed_or_clean() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    repo.persist(candidate(identity(&["L"], &[], "t"), &["exact I."]))
        .unwrap();
    let path = records(project.path()).pop().unwrap();
    let bytes = fs::read(&path).unwrap();
    let bad = path.with_file_name("not-a-uuid.json");
    fs::rename(&path, &bad).unwrap();
    assert!(
        ProofRepository::open(project.path()).is_err(),
        "filename must equal embedded UUID"
    );
    fs::remove_file(&bad).unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    repo.persist(candidate(identity(&["L"], &[], "u"), &["exact I."]))
        .unwrap();
    let path = records(project.path())
        .into_iter()
        .find(|p| fs::read(p).unwrap() == bytes)
        .unwrap_or_else(|| records(project.path())[0].clone());
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["future_field"] = serde_json::json!(true);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        ProofRepository::open(project.path()).is_err(),
        "unknown durable field fails closed"
    );
    let fresh = tempfile::tempdir().unwrap();
    let proofs = fresh.path().join("_build/.rocq-engine/proofs");
    fs::create_dir_all(&proofs).unwrap();
    fs::write(proofs.join(".orphan.tmp"), b"partial").unwrap();
    let repo = ProofRepository::open(fresh.path()).unwrap();
    assert!(repo.scan().unwrap().is_empty());
    assert!(!proofs.join(".orphan.tmp").exists());
    fs::write(
        proofs.join("00000000-0000-0000-0000-000000000000.json"),
        vec![0u8; 64 * 1024 * 1024 + 1],
    )
    .unwrap();
    assert!(matches!(
        ProofRepository::open(fresh.path()),
        Err(RepositoryError::Oversize)
    ));
}

#[test]
fn one_megabyte_replacement_roundtrips_and_over_sixteen_megabytes_is_typed_oversize() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let c = candidate(identity(&["L"], &[], "large"), &["exact I."]);
    let contents = vec![b'x'; 1024 * 1024];
    let proof = ClosedProof::new(
        c.clone(),
        TrustAuditResult::ClosedUnderFrozenBaseline,
        vec![
            FileReplacement::with_original(PathBuf::from("Large.v"), None, contents.clone())
                .unwrap(),
        ],
    )
    .unwrap();
    repo.persist(c.clone()).unwrap();
    repo.promote(&c.declaration.identity, proof).unwrap();
    drop(repo);
    let reopened = ProofRepository::open(project.path()).unwrap();
    let ProofPhase::Closed(proof) = &reopened.scan().unwrap()[&c.declaration.identity].phase else {
        panic!("closed")
    };
    assert_eq!(proof.replacements[0].contents, contents);
    assert_eq!(
        proof.replacements[0].new_digest(),
        <[u8; 32]>::from(sha2::Sha256::digest(&contents))
    );
    assert!(matches!(
        FileReplacement::with_original(
            PathBuf::from("TooLarge.v"),
            None,
            vec![0; 16 * 1024 * 1024 + 1]
        ),
        Err(RepositoryError::Oversize)
    ));
}

#[test]
fn bounded_vectors_and_malformed_base64_fail_closed() {
    let baseline = TrustBaseline::new(vec![], vec![], vec![], [1; 32]).unwrap();
    let declaration = OpenDeclaration {
        kind: DeclarationKind::Theorem,
        identity: identity(&["L"], &[], "vectors"),
        anchor: SourceAnchor {
            source_digest: [1; 32],
            normalized_statement: "True".into(),
            context: vec![],
            old_body_digest: [2; 32],
        },
    };
    let commands = (0..65).map(|_| CanonicalTactic("idtac.".into())).collect();
    assert!(matches!(
        SolvedCandidate::new(declaration, commands, baseline),
        Err(RepositoryError::Oversize)
    ));
    let rejected_candidate = candidate(identity(&["L"], &[], "reject"), &["exact I."]);
    let diagnostics = (0..129)
        .map(|i| Diagnostic::new(format!("d{i}"), "bounded diagnostic").unwrap())
        .collect();
    assert!(matches!(
        CandidateRejection::new(
            rejected_candidate,
            RejectionPhase::NativeValidation,
            diagnostics
        ),
        Err(RepositoryError::Invalid)
    ));
}

#[test]
fn malformed_bounded_base64_contents_fails_recovery_before_use() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let c = candidate(identity(&["L"], &[], "base64-closed"), &["exact I."]);
    repo.persist(c.clone()).unwrap();
    let id = c.declaration.identity.clone();
    repo.promote(&id, closed(c)).unwrap();
    let path = records(project.path()).pop().unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["phase"]["Closed"]["replacements"][0]["contents"] = serde_json::json!("%%%not-base64%%%");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        matches!(
            ProofRepository::open(project.path()),
            Err(RepositoryError::Corrupt)
        ),
        "malformed base64 durable contents must fail closed"
    );
}

#[test]
fn constructors_reject_unsafe_paths_and_invalid_or_oversized_inputs_before_hashing() {
    let mut accepted = Vec::new();
    for path in ["", ".", "..", "../escape.v", "/absolute.v"] {
        if FileReplacement::with_original(PathBuf::from(path), None, vec![]).is_ok() {
            accepted.push(format!("replacement path {path:?}"));
        }
    }

    let baselines = vec![
        (
            "empty axiom name",
            TrustBaseline::new(vec![("".into(), "True".into())], vec![], vec![], [0; 32]),
        ),
        (
            "empty external logical library",
            TrustBaseline::new(
                vec![],
                vec![(LogicalLibrary(vec![]), [0; 32])],
                vec![],
                [0; 32],
            ),
        ),
        (
            "invalid forbidden local",
            TrustBaseline::new(vec![], vec![], vec!["bad local".into()], [0; 32]),
        ),
        (
            "oversized authorization string",
            TrustBaseline::new(
                vec![("A".into(), "x".repeat(64 * 1024 + 1))],
                vec![],
                vec![],
                [0; 32],
            ),
        ),
        (
            "oversized authorization count",
            TrustBaseline::new(
                (0..4097).map(|n| (format!("A{n}"), "T".into())).collect(),
                vec![],
                vec![],
                [0; 32],
            ),
        ),
    ];
    for (name, baseline) in baselines {
        if baseline.is_ok() {
            accepted.push(format!("baseline {name}"));
        }
    }

    let baseline = TrustBaseline::new(vec![], vec![], vec![], [0; 32]).unwrap();
    let invalid_identities = [
        identity(&[], &[], "t"),
        identity(&["."], &[], "t"),
        identity(&["L"], &[""], "t"),
        identity(&["L"], &["M/M"], "t"),
        identity(&["L"], &[], ""),
    ];
    for invalid in invalid_identities {
        if SolvedCandidate::new(declaration(invalid, vec![]), vec![], baseline.clone()).is_ok() {
            accepted.push("invalid declaration identity".into());
        }
    }
    if SolvedCandidate::new(
        declaration(
            identity(&["L"], &["M"], "t"),
            vec![LexicalScope::Module("different-module".into())],
        ),
        vec![],
        baseline.clone(),
    )
    .is_ok()
    {
        accepted.push("mismatched lexical module context".into());
    }
    let command_total = (0..64)
        .map(|_| CanonicalTactic("x".repeat(16 * 1024 + 1)))
        .collect();
    if SolvedCandidate::new(
        declaration(identity(&["L"], &[], "large-commands"), vec![]),
        command_total,
        baseline,
    )
    .is_ok()
    {
        accepted.push("oversized command total".into());
    }
    assert!(
        accepted.is_empty(),
        "constructors accepted invalid inputs before computing identities: {accepted:?}"
    );
}

#[test]
fn aggregate_replacement_limits_are_enforced_before_serialization_with_checked_addition() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let c = candidate(identity(&["L"], &[], "aggregate"), &["exact I."]);
    repo.persist(c.clone()).unwrap();

    // Four legal per-file payloads exactly reach the 64 MiB raw aggregate.
    // Their base64 record is larger than the on-disk cap, so serialization must
    // still return a typed bound error rather than write a partial record.
    let at_raw_cap = (0..4)
        .map(|n| {
            FileReplacement::with_original(
                PathBuf::from(format!("AtCap{n}.v")),
                None,
                vec![b'x'; 16 * 1024 * 1024],
            )
            .unwrap()
        })
        .collect();
    assert!(matches!(
        repo.promote(
            &c.declaration.identity.clone(),
            ClosedProof::new(
                c.clone(),
                TrustAuditResult::ClosedUnderFrozenBaseline,
                at_raw_cap
            )
            .unwrap(),
        ),
        Err(RepositoryError::Oversize)
    ));

    // This is strictly above the raw-record cap. The validator must calculate
    // this sum with checked arithmetic before considering record serialization.
    let above_raw_cap = (0..5)
        .map(|n| {
            FileReplacement::with_original(
                PathBuf::from(format!("AboveCap{n}.v")),
                None,
                vec![b'x'; 16 * 1024 * 1024],
            )
            .unwrap()
        })
        .collect();
    assert!(matches!(
        ClosedProof::new(
            c,
            TrustAuditResult::ClosedUnderFrozenBaseline,
            above_raw_cap,
        ),
        Err(RepositoryError::Oversize)
    ));
}

#[test]
fn checksum_valid_unknown_nested_fields_fail_closed() {
    for phase_name in ["Solved", "Closed", "Rejected"] {
        let project = tempfile::tempdir().unwrap();
        let repo = ProofRepository::open(project.path()).unwrap();
        let c = candidate(identity(&["L"], &[], phase_name), &["exact I."]);
        repo.persist(c.clone()).unwrap();
        match phase_name {
            "Closed" => {
                repo.promote(&c.declaration.identity.clone(), closed(c))
                    .unwrap();
            }
            "Rejected" => {
                repo.reject(
                    &c.declaration.identity.clone(),
                    rejection(c, "expected rejection"),
                )
                .unwrap();
            }
            "Solved" => {}
            _ => unreachable!(),
        }
        let path = records(project.path()).pop().unwrap();
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let candidate = if phase_name == "Solved" {
            &mut record["phase"]["Solved"]
        } else {
            &mut record["phase"][phase_name]["candidate"]
        };
        candidate["declaration"]["identity"]["unknown_nested_field"] =
            serde_json::json!("must not be silently discarded");
        // Keep the original checksum deliberately. If the unknown nested field
        // is ignored by serde, reserialization produces the original checksum,
        // proving that schema strictness (not checksum mismatch) rejects it.
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(
            matches!(
                ProofRepository::open(project.path()),
                Err(RepositoryError::Corrupt)
            ),
            "checksum-valid nested unknown field in {phase_name} must fail closed"
        );
    }
}

#[test]
fn checksum_valid_rejected_records_revalidate_vector_and_diagnostic_contracts() {
    for (name, diagnostics) in [
        (
            "too-many",
            serde_json::Value::Array(
                (0..129)
                    .map(|n| serde_json::json!({"code": format!("d{n}"), "message": "valid"}))
                    .collect(),
            ),
        ),
        (
            "invalid-diagnostic",
            serde_json::json!([{"code": "valid_code", "message": "has a control \u{0001}"}]),
        ),
    ] {
        let project = tempfile::tempdir().unwrap();
        let repo = ProofRepository::open(project.path()).unwrap();
        let c = candidate(identity(&["L"], &[], name), &["exact I."]);
        repo.persist(c).unwrap();
        let path = records(project.path()).pop().unwrap();
        let record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let phase_value = serde_json::json!({
            "Rejected": {
                "candidate": record["phase"]["Solved"].clone(),
                "phase": "TrustAudit",
                "diagnostics": diagnostics,
            }
        });
        let phase: ProofPhase = serde_json::from_value(phase_value).unwrap();
        checksum_valid_rewrite(&path, phase);
        assert!(
            matches!(
                ProofRepository::open(project.path()),
                Err(RepositoryError::Invalid)
            ),
            "checksum-valid rejected record {name} must fail semantic validation"
        );
    }
}

#[test]
fn diagnostic_text_allows_literal_path_separators_without_leaking_private_paths() {
    let diagnostic = Diagnostic::new("sanitized_code", r"literal /\ separator").unwrap();
    assert_eq!(diagnostic.message(), r"literal /\ separator");
}

fn repository_source() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/repository.rs")).unwrap()
}

#[test]
fn closed_proof_constructor_is_fallible() {
    // These are source-contract audits because the former is a public return
    // type and the latter two have no portable black-box crash oracle.
    let source = repository_source();
    let closed_start = source.find("impl ClosedProof {").unwrap();
    let closed_end = source[closed_start..]
        .find("/// Deterministic close stage")
        .unwrap();
    let closed = &source[closed_start..closed_start + closed_end];
    assert!(
        closed.contains(") -> Result<Self, RepositoryError>"),
        "ClosedProof::new must reject invalid replacement plans instead of creating invalid state"
    );
}

#[test]
fn closed_replacement_retains_original_bytes_for_crash_rollback() {
    let project = tempfile::tempdir().unwrap();
    let repo = ProofRepository::open(project.path()).unwrap();
    let candidate = candidate(identity(&["L"], &[], "rollback"), &["exact I."]);
    let id = candidate.declaration.identity.clone();
    repo.persist(candidate.clone()).unwrap();
    let replacement = FileReplacement::with_original(
        PathBuf::from("L.v"),
        Some(b"old source".to_vec()),
        b"new source".to_vec(),
    )
    .unwrap();
    repo.promote(
        &id,
        ClosedProof::new(
            candidate,
            TrustAuditResult::ClosedUnderFrozenBaseline,
            vec![replacement],
        )
        .unwrap(),
    )
    .unwrap();
    drop(repo);
    let recovered = ProofRepository::open(project.path())
        .unwrap()
        .scan()
        .unwrap();
    let ProofPhase::Closed(closed) = &recovered[&id].phase else {
        panic!("closed")
    };
    assert_eq!(
        closed.replacements[0].old_contents(),
        Some(b"old source".as_slice())
    );
}

#[test]
fn aggregate_replacement_validation_uses_checked_arithmetic() {
    let source = repository_source();
    let validation_start = source.find("fn validate_closed(").unwrap();
    let validation_end = source[validation_start..]
        .find("fn valid_identifier(")
        .unwrap();
    let validation = &source[validation_start..validation_start + validation_end];
    assert!(
        validation.contains("checked_add")
            || validation.contains("try_fold")
            || validation.contains("checked_total"),
        "aggregate replacement size must use checked arithmetic"
    );
}

#[test]
fn first_store_syncs_project_parent_directory() {
    let source = repository_source();
    let sync_start = source.find("fn sync_hierarchy(").unwrap();
    let sync_end = source[sync_start..].find("fn clean_temps(").unwrap();
    let sync = &source[sync_start..sync_start + sync_end];
    assert!(
        sync.contains(".and_then(Path::parent)\n            .and_then(Path::parent)"),
        "first-store hierarchy synchronization must include the project parent"
    );
}
