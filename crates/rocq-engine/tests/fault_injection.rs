//! Crash-window recovery tests. Run with
//! `cargo test -p rocq-engine --features fault-injection --test fault_injection`.

#![cfg(feature = "fault-injection")]

use rocq_engine::{
    DeclarationIdentity, DeclarationKind, Engine, EngineConfig, FileId, ProofLifecycle,
};
use std::{env, fs, path::PathBuf, process::Command, time::Duration};
use tempfile::TempDir;

fn identity() -> DeclarationIdentity {
    if env::var_os("ROCQ_ENGINE_FAULT_MULTI").is_some() {
        return DeclarationIdentity {
            file: FileId("Fresh.v".into()),
            qualified_path: vec!["Demo".into(), "Fresh".into(), "created".into()],
        };
    }
    DeclarationIdentity {
        file: FileId("Main.v".into()),
        qualified_path: vec!["Main".into(), "Main".into(), "created".into()],
    }
}
fn config(state: &std::path::Path) -> EngineConfig {
    EngineConfig {
        state_parent: state.to_owned(),
        operation_timeout: Duration::from_secs(20),
        close_timeout: Some(Duration::from_secs(20)),
        ..EngineConfig::default()
    }
}
fn child_mode() -> bool {
    env::var_os("ROCQ_ENGINE_FAULT_CHILD").is_some()
}

#[test]
fn every_publication_fault_point_recovers_idempotently() {
    if child_mode() {
        let project = PathBuf::from(env::var_os("ROCQ_ENGINE_FAULT_PROJECT").unwrap());
        let state = PathBuf::from(env::var_os("ROCQ_ENGINE_FAULT_STATE").unwrap());
        let engine = Engine::new(config(&state)).unwrap();
        let opened = engine
            .declare(
                &project,
                DeclarationKind::Theorem,
                identity(),
                "True".into(),
            )
            .unwrap();
        let attempt = opened.attempt.unwrap();
        let result = engine.check(attempt, &["exact I.".into()]);
        eprintln!("child step result: {result:?}");
        panic!("fault point did not abort");
    }
    for point in [
        "after_promote",
        "before_replace",
        "between_replacements",
        "after_replace",
        "after_final_build",
        "before_ack",
    ] {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let state = dir.path().join("state");
        let multi = point == "between_replacements";
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("dune-project"),
            "(lang dune 3.21)\n(using rocq 0.11)\n",
        )
        .unwrap();
        if multi {
            fs::create_dir_all(project.join("theories")).unwrap();
            fs::write(
                project.join("theories/dune"),
                "(rocq.theory (name Demo) (modules Existing))\n",
            )
            .unwrap();
            fs::write(
                project.join("theories/Existing.v"),
                "Definition old : True := I.\n",
            )
            .unwrap();
        } else {
            fs::write(project.join("dune"), "(rocq.theory (name Main))\n").unwrap();
            fs::write(project.join("Main.v"), "").unwrap();
        }
        let mut child = Command::new(env::current_exe().unwrap());
        child
            .env("ROCQ_ENGINE_FAULT_CHILD", "1")
            .env("ROCQ_ENGINE_FAULT_POINT", point)
            .env("ROCQ_ENGINE_FAULT_PROJECT", &project)
            .env("ROCQ_ENGINE_FAULT_STATE", &state)
            .args([
                "--exact",
                "every_publication_fault_point_recovers_idempotently",
            ]);
        if multi {
            child.env("ROCQ_ENGINE_FAULT_MULTI", "1");
        }
        assert!(
            !child.status().unwrap().success(),
            "fault point {point} did not terminate child"
        );
        let engine = Engine::new(config(&state)).unwrap();
        let target = if multi {
            DeclarationIdentity {
                file: FileId("Fresh.v".into()),
                qualified_path: vec!["Demo".into(), "Fresh".into(), "created".into()],
            }
        } else {
            identity()
        };
        let recovered = engine
            .open(&project, target)
            .unwrap_or_else(|error| panic!("{point}: recovery open failed: {error:?}"));
        assert_eq!(recovered.lifecycle, ProofLifecycle::Completed, "{point}");
        let source_path = if multi {
            project.join("theories/Fresh.v")
        } else {
            project.join("Main.v")
        };
        let source = fs::read_to_string(source_path).unwrap_or_else(|_| {
            if multi {
                fs::read_to_string(project.join("theories/Existing.v")).unwrap()
            } else {
                String::new()
            }
        });
        assert!(
            source.contains("Theorem created : True."),
            "{point}: {source}"
        );
        assert!(!source.contains("Admitted"), "{point}: {source}");
    }
}
