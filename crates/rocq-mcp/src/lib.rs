//! Rocq MCP server. Transport and protocol lifecycle are delegated to `rmcp`.

mod adapter;
mod checkpoint;
mod schema;
mod server;
mod validation;

pub use schema::tool_definitions;
pub use server::{RocqServer, ServerRuntime};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{dispatch, pet_release_error, public_error};
    use rocq_engine::{
        DeclarationIdentity, Engine, EngineConfig, Error, ErrorKind, FileId, pet::PetError,
    };
    use serde_json::json;
    use std::{fs, sync::Arc};

    fn dune_project(theory: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join("dune-project"),
            "(lang dune 3.22)\n(using rocq 0.12)\n",
        )
        .unwrap();
        fs::write(
            project.path().join("dune"),
            format!("(rocq.theory (name {theory}))\n"),
        )
        .unwrap();
        for (file, source) in files {
            fs::write(project.path().join(file), source).unwrap();
        }
        project
    }

    fn runtime() -> Arc<ServerRuntime> {
        Arc::new(ServerRuntime::new(Arc::new(
            Engine::new(EngineConfig::default()).unwrap(),
        )))
    }

    fn attach(runtime: &Arc<ServerRuntime>, project: &tempfile::TempDir) -> RocqServer {
        let server = runtime.connection();
        dispatch(
            runtime,
            &server.session,
            "start",
            json!({"project_path": project.path()}),
        )
        .unwrap();
        server
    }

    #[test]
    fn official_adapter_exposes_discovery_tools() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 10);
        for tool in tools {
            let value = serde_json::to_value(tool).expect("tool is serializable");
            assert_eq!(value["inputSchema"]["additionalProperties"], false);
            let schema = value["inputSchema"].to_string();
            assert!(!schema.contains("cursor"));
            assert!(!schema.contains("workspace"));
            assert!(!schema.contains("pet_pid"));
            if !matches!(tool.name.as_ref(), "start" | "list_files" | "list_decls") {
                assert!(!schema.contains("project_path"));
            }
        }
    }

    #[test]
    fn query_schema_exposes_every_kind_without_union_flattening() {
        let query = tool_definitions()
            .iter()
            .find(|tool| tool.name == "query")
            .expect("query tool exists");
        let value = serde_json::to_value(query).expect("tool is serializable");
        assert!(value["inputSchema"].get("oneOf").is_none());
        assert_eq!(
            value["inputSchema"]["properties"]["kind"]["enum"],
            json!([
                "goals",
                "search",
                "statement",
                "proof",
                "definition",
                "assumptions",
                "dependencies",
                "type",
                "notations"
            ])
        );
        for field in ["target", "expression", "pattern"] {
            assert!(value["inputSchema"]["properties"].get(field).is_some());
        }
        assert!(
            value["description"]
                .as_str()
                .unwrap()
                .contains("require `target`")
        );
        assert!(
            value["description"]
                .as_str()
                .unwrap()
                .contains("require `expression`")
        );
    }

    #[test]
    fn every_tool_description_comes_from_its_handbook_section() {
        for tool in tool_definitions() {
            let heading = format!("## `{}`\n", tool.name);
            let handbook = include_str!("../COMMANDS.md");
            let tail = handbook
                .split_once(&heading)
                .expect("tool has handbook section")
                .1;
            let section = tail.split("\n## ").next().unwrap().trim();
            assert_eq!(tool.description.as_deref(), Some(section));
        }
    }

    #[test]
    fn logical_identity_projection_never_contains_physical_paths() {
        let identity = DeclarationIdentity {
            file: FileId("Main.v".into()),
            qualified_path: vec!["Demo".into(), "plus_comm".into()],
        };
        assert_eq!(identity.qualified_name(), "Demo.plus_comm");
    }

    #[test]
    fn tool_annotations_mark_queries_read_only() {
        for tool in tool_definitions() {
            let value = serde_json::to_value(tool).expect("tool is serializable");
            let read_only = value["annotations"]["readOnlyHint"].as_bool();
            if matches!(
                tool.name.as_ref(),
                "query" | "try" | "list_files" | "list_decls"
            ) {
                assert_eq!(read_only, Some(true));
            } else {
                assert_ne!(read_only, Some(true));
            }
        }
    }

    #[test]
    fn engine_errors_are_transparently_and_exhaustively_serialized() {
        let cases = [
            (ErrorKind::InvalidRequest, "invalid_request"),
            (ErrorKind::InvalidConfiguration, "invalid_configuration"),
            (ErrorKind::InvalidDeclaration, "invalid_declaration"),
            (ErrorKind::NotFound, "not_found"),
            (ErrorKind::Ambiguous, "ambiguous"),
            (ErrorKind::DeclarationChanged, "declaration_changed"),
            (ErrorKind::ProofStepFailed, "proof_step_failed"),
            (ErrorKind::ProofTimeout, "proof_timeout"),
            (ErrorKind::ProjectTimeout, "project_timeout"),
            (ErrorKind::BuildTimeout, "build_timeout"),
            (
                ErrorKind::AxiomDependencyOutOfScope,
                "axiom_dependency_out_of_scope",
            ),
            (ErrorKind::UnfinishedDependency, "unfinished_dependency"),
        ];
        for (kind, wire) in cases {
            let projected = public_error(&Error::new(kind, "detail"));
            assert_eq!(projected, json!({"kind":wire,"message":"detail"}));
        }
    }

    #[test]
    fn only_pet_transport_loss_maps_release_to_proof_timeout() {
        for error in [
            PetError::ProcessLost("closed".into()),
            PetError::Protocol("invalid response".into()),
            PetError::OutputOverflow,
        ] {
            assert_eq!(pet_release_error(error).kind, ErrorKind::ProofTimeout);
        }
        for error in [
            PetError::Invalid("bad state".into()),
            PetError::Environment("bad workspace".into()),
            PetError::Remote {
                code: -32000,
                message: "release rejected".into(),
            },
        ] {
            assert_eq!(
                pet_release_error(error).kind,
                ErrorKind::InvalidConfiguration
            );
        }
    }

    #[test]
    fn rewind_schema_exposes_optional_steps_and_checkpoint_only() {
        let rewind = tool_definitions()
            .iter()
            .find(|tool| tool.name == "rewind")
            .expect("rewind tool exists");
        let value = serde_json::to_value(rewind).unwrap();
        let schema = &value["inputSchema"];
        assert_eq!(schema["required"], json!([]));
        assert_eq!(schema["properties"]["steps"]["minimum"], 1);
        assert_eq!(schema["properties"]["checkpoint"]["minimum"], 1);
        assert!(schema["properties"].get("to").is_none());
    }

    #[test]
    fn pet_restart_invalidates_and_replays_two_retained_branches() {
        use crate::checkpoint::CheckpointId;

        let project = dune_project(
            "Demo",
            &[(
                "A.v",
                "Theorem t : forall A B C : Prop, A -> B -> C -> A. Admitted.\n",
            )],
        );
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let session = Arc::clone(&server.session);
        let id = json!({"file":"A.v","qualified_path":["Demo","A","t"]});
        let root =
            dispatch(&runtime, &session, "prove", json!({"declaration":id})).unwrap()["checkpoint"]
                .as_u64()
                .unwrap();
        let first = dispatch(
            &runtime,
            &session,
            "check",
            json!({"attempts":["intros A B."]}),
        )
        .unwrap()["state"]["checkpoint"]
            .as_u64()
            .unwrap();
        dispatch(&runtime, &session, "rewind", json!({"checkpoint":root})).unwrap();
        let second = dispatch(
            &runtime,
            &session,
            "check",
            json!({"attempts":["intros A B C."]}),
        )
        .unwrap()["state"]["checkpoint"]
            .as_u64()
            .unwrap();
        let attached = session.project().unwrap();
        runtime.restart_pet_for_test(&attached);

        let first_view =
            dispatch(&runtime, &session, "rewind", json!({"checkpoint":first})).unwrap();
        assert_eq!(first_view["state"]["checkpoint"], first);
        assert!(
            first_view["state"]["goals"]
                .as_str()
                .unwrap()
                .contains("C : Prop")
        );
        let second_view =
            dispatch(&runtime, &session, "rewind", json!({"checkpoint":second})).unwrap();
        assert_eq!(second_view["state"]["checkpoint"], second);
        let second_goals = second_view["state"]["goals"].as_str().unwrap();
        assert!(second_goals.contains("A B C : Prop"), "{second_goals}");
        assert!(second_goals.contains("A -> B -> C -> A"), "{second_goals}");
        assert!(
            session
                .selection
                .lock()
                .unwrap()
                .checkpoints
                .lookup(CheckpointId::from_u64(first))
                .unwrap()
                .pet_state
                .is_some()
        );
    }

    #[test]
    fn temporary_and_rejected_operations_do_not_leak_pet_states() {
        let project = dune_project(
            "Demo",
            &[(
                "A.v",
                "Theorem t : forall A B C : Prop, A -> B -> C -> A. Admitted.\n",
            )],
        );
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let session = Arc::clone(&server.session);
        let id = json!({"file":"A.v","qualified_path":["Demo","A","t"]});
        let attached = session.project().unwrap();

        for _ in 0..10 {
            dispatch(
                &runtime,
                &session,
                "prove",
                json!({"declaration":id.clone()}),
            )
            .unwrap();
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 1);

            let tried = dispatch(
                &runtime,
                &session,
                "try",
                json!({"attempts":[
                    "intros A B C HA HB HC. exact HA.",
                    "fail 0."
                ]}),
            )
            .unwrap();
            assert_eq!(tried["attempts"].as_array().unwrap().len(), 2);
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 1);

            let rejected = dispatch(
                &runtime,
                &session,
                "check",
                json!({"attempts":["intros A. ThisCommandMustNotExist."]}),
            )
            .unwrap();
            assert!(rejected["selected"].is_null());
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 1);

            dispatch(
                &runtime,
                &session,
                "check",
                json!({"attempts":["intros A B."]}),
            )
            .unwrap();
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 2);
            dispatch(
                &runtime,
                &session,
                "abandon",
                json!({"declaration":id.clone()}),
            )
            .unwrap();
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 0);
        }
    }

    #[test]
    fn project_registry_shares_one_actor_per_canonical_project() {
        let first = dune_project("First", &[("A.v", "Theorem t : True. Admitted.\n")]);
        let second = dune_project("Second", &[("B.v", "Theorem t : True. Admitted.\n")]);
        let runtime = runtime();
        let first_a = attach(&runtime, &first);
        let first_b = attach(&runtime, &first);
        let second_a = attach(&runtime, &second);
        let first_runtime = first_a.session.project().unwrap();
        let shared_runtime = first_b.session.project().unwrap();
        let independent_runtime = second_a.session.project().unwrap();
        assert!(Arc::ptr_eq(&first_runtime, &shared_runtime));
        assert!(!Arc::ptr_eq(&first_runtime, &independent_runtime));
    }

    #[test]
    fn repeated_start_installs_new_dune_view_and_invalidates_shared_states() {
        let project = dune_project(
            "Demo",
            &[
                ("A.v", "Theorem a : True. Admitted.\n"),
                ("B.v", "Theorem b : True. Admitted.\n"),
            ],
        );
        fs::write(
            project.path().join("dune"),
            "(rocq.theory (name Demo) (modules A))\n",
        )
        .unwrap();
        let runtime = runtime();
        let first = attach(&runtime, &project);
        let second = attach(&runtime, &project);
        dispatch(
            &runtime,
            &second.session,
            "prove",
            json!({"declaration":{"file":"A.v","qualified_path":["Demo","A","a"]}}),
        )
        .unwrap();

        fs::write(
            project.path().join("dune"),
            "(rocq.theory (name Demo) (modules B))\n",
        )
        .unwrap();
        dispatch(
            &runtime,
            &first.session,
            "start",
            json!({"project_path":project.path()}),
        )
        .unwrap();

        let files = dispatch(&runtime, &first.session, "list_files", json!({})).unwrap();
        assert_eq!(files, json!({"files":["B.v"]}));
        assert!(
            second
                .session
                .selection
                .lock()
                .unwrap()
                .checkpoints
                .proof
                .as_ref()
                .unwrap()
                .checkpoints
                .values()
                .all(|checkpoint| checkpoint.pet_state.is_none())
        );
        let error =
            dispatch(&runtime, &second.session, "query", json!({"kind":"goals"})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::DeclarationChanged);
    }

    #[test]
    fn operation_admission_detects_dune_context_change_before_pet_use() {
        let project = dune_project("Demo", &[("A.v", "Theorem t : True. Admitted.\n")]);
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let identity = json!({"file":"A.v","qualified_path":["Demo","A","t"]});
        dispatch(
            &runtime,
            &server.session,
            "prove",
            json!({"declaration":identity}),
        )
        .unwrap();

        fs::write(
            project.path().join("dune"),
            "(rocq.theory (name Changed))\n",
        )
        .unwrap();
        let error = dispatch(
            &runtime,
            &server.session,
            "try",
            json!({"attempts":["exact I."]}),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::DeclarationChanged);
        assert!(
            server
                .session
                .selection
                .lock()
                .unwrap()
                .checkpoints
                .proof
                .as_ref()
                .unwrap()
                .checkpoints
                .values()
                .all(|checkpoint| checkpoint.pet_state.is_none())
        );
        assert!(
            fs::read_to_string(project.path().join("A.v"))
                .unwrap()
                .contains("Admitted")
        );
    }

    #[test]
    fn pet_loss_invalidates_only_sessions_in_the_affected_project() {
        let first = dune_project(
            "First",
            &[("A.v", "Theorem t : forall P : Prop, P -> P. Admitted.\n")],
        );
        let second = dune_project(
            "Second",
            &[("B.v", "Theorem t : forall P : Prop, P -> P. Admitted.\n")],
        );
        let runtime = runtime();
        let first_a = attach(&runtime, &first);
        let first_b = attach(&runtime, &first);
        let second_a = attach(&runtime, &second);
        let first_id = json!({"file":"A.v","qualified_path":["First","A","t"]});
        let second_id = json!({"file":"B.v","qualified_path":["Second","B","t"]});
        for server in [&first_a, &first_b] {
            dispatch(
                &runtime,
                &server.session,
                "prove",
                json!({"declaration":first_id.clone()}),
            )
            .unwrap();
        }
        dispatch(
            &runtime,
            &second_a.session,
            "prove",
            json!({"declaration":second_id}),
        )
        .unwrap();

        let affected = first_a.session.project().unwrap();
        runtime.restart_pet_for_test(&affected);
        for server in [&first_a, &first_b] {
            let selection = server.session.selection.lock().unwrap();
            assert!(
                selection
                    .checkpoints
                    .proof
                    .as_ref()
                    .unwrap()
                    .checkpoints
                    .values()
                    .all(|checkpoint| checkpoint.pet_state.is_none())
            );
        }
        {
            let selection = second_a.session.selection.lock().unwrap();
            assert!(
                selection
                    .checkpoints
                    .current()
                    .unwrap()
                    .1
                    .pet_state
                    .is_some()
            );
        }
        dispatch(
            &runtime,
            &second_a.session,
            "query",
            json!({"kind":"goals"}),
        )
        .unwrap();
        dispatch(&runtime, &first_a.session, "query", json!({"kind":"goals"})).unwrap();
    }

    #[test]
    fn publication_invalidates_and_replays_other_same_project_proofs() {
        let project = dune_project(
            "Demo",
            &[
                ("A.v", "Theorem a : forall P : Prop, P -> P. Admitted.\n"),
                ("B.v", "Theorem b : forall P : Prop, P -> P. Admitted.\n"),
            ],
        );
        let runtime = runtime();
        let first = attach(&runtime, &project);
        let second = attach(&runtime, &project);
        let first_id = json!({"file":"A.v","qualified_path":["Demo","A","a"]});
        let second_id = json!({"file":"B.v","qualified_path":["Demo","B","b"]});
        for (server, id) in [(&first, &first_id), (&second, &second_id)] {
            dispatch(
                &runtime,
                &server.session,
                "prove",
                json!({"declaration":id.clone()}),
            )
            .unwrap();
            dispatch(
                &runtime,
                &server.session,
                "check",
                json!({"attempts":["intros P H."]}),
            )
            .unwrap();
        }

        let first_closed = dispatch(
            &runtime,
            &first.session,
            "check",
            json!({"attempts":["exact H."]}),
        )
        .unwrap();
        assert_eq!(first_closed["state"]["status"], "Completed");
        {
            let selection = second.session.selection.lock().unwrap();
            assert!(
                selection
                    .checkpoints
                    .proof
                    .as_ref()
                    .unwrap()
                    .checkpoints
                    .values()
                    .all(|checkpoint| checkpoint.pet_state.is_none())
            );
        }
        let second_closed = dispatch(
            &runtime,
            &second.session,
            "check",
            json!({"attempts":["exact H."]}),
        )
        .unwrap();
        assert_eq!(second_closed["state"]["status"], "Completed");
        assert!(
            !fs::read_to_string(project.path().join("A.v"))
                .unwrap()
                .contains("Admitted")
        );
        assert!(
            !fs::read_to_string(project.path().join("B.v"))
                .unwrap()
                .contains("Admitted")
        );
    }

    #[test]
    fn failed_dune_refresh_retains_replayable_checkpoints() {
        let project = dune_project("Demo", &[("A.v", "Theorem t : True. Admitted.\n")]);
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let id = json!({"file":"A.v","qualified_path":["Demo","A","t"]});
        let root = dispatch(
            &runtime,
            &server.session,
            "prove",
            json!({"declaration":id.clone()}),
        )
        .unwrap()["checkpoint"]
            .as_u64()
            .unwrap();
        fs::write(
            project.path().join("dune"),
            "(this is not a valid dune stanza)\n",
        )
        .unwrap();
        let failed = dispatch(
            &runtime,
            &server.session,
            "check",
            json!({"attempts":["exact I."]}),
        )
        .unwrap_err();
        assert_eq!(failed.kind, ErrorKind::InvalidConfiguration);
        assert!(
            fs::read_to_string(project.path().join("A.v"))
                .unwrap()
                .contains("Admitted")
        );
        fs::write(project.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
        let rewound = dispatch(
            &runtime,
            &server.session,
            "rewind",
            json!({"checkpoint":root}),
        )
        .unwrap();
        assert_eq!(rewound["state"]["checkpoint"], root);
        dispatch(
            &runtime,
            &server.session,
            "abandon",
            json!({"declaration":id}),
        )
        .unwrap();
    }
}
