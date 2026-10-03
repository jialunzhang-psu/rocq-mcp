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
                "about",
                "print",
                "assumptions",
                "dependencies",
                "type",
                "notations",
                "locate_symbol",
                "progress"
            ])
        );
        for field in [
            "target",
            "at",
            "expression",
            "pattern",
            "scope",
            "goal_id",
            "offset",
            "symbol",
            "limit",
            "generation",
            "diff",
            "structured",
        ] {
            assert!(value["inputSchema"]["properties"].get(field).is_some());
        }
        assert_eq!(value["inputSchema"]["properties"]["offset"]["minimum"], 0);
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
    fn discovery_documents_entry_commit_and_persistence() {
        let description = |name: &str| {
            tool_definitions()
                .iter()
                .find(|tool| tool.name == name)
                .and_then(|tool| tool.description.as_deref())
                .unwrap()
        };
        assert!(description("prove").starts_with("Open, enter, and select"));
        assert!(description("prove").contains("before `try`, `check`"));
        assert!(description("try").starts_with("Speculatively test"));
        assert!(description("try").contains("never\ncommits or saves"));
        assert!(description("check").starts_with("Submit and commit"));
        assert!(description("check").contains("no separate save or publish call"));
        assert!(description("query").contains("A query never enters, commits, saves"));

        let instructions = crate::schema::server_instructions();
        assert!(instructions.contains("`list_decls` → `prove`"));
        assert!(instructions.contains("`invalid_request: call prove first`"));
        assert!(instructions.contains("uncursored `tools/list` returns exactly"));
        assert!(instructions.contains("`tools/list` result is authoritative"));
    }

    #[test]
    fn start_handbook_does_not_promise_remote_relative_paths() {
        let start = tool_definitions()
            .iter()
            .find(|tool| tool.name == "start")
            .and_then(|tool| tool.description.as_deref())
            .expect("start tool description exists");
        assert!(start.contains("/absolute/path/to/project"));
        assert!(start.contains("HTTP/Funnel"));
        assert!(start.contains("cheap layout probe"));
        assert!(!start.contains("\"./project\""));
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
            (ErrorKind::ProofStepTimeout, "proof_step_timeout"),
            (ErrorKind::RequestCancelled, "request_cancelled"),
            (ErrorKind::PetLost, "pet_lost"),
            (ErrorKind::QueryFailed, "query_failed"),
            (ErrorKind::QueryTimeout, "query_timeout"),
            (ErrorKind::PetFailure, "pet_failure"),
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
            assert_eq!(projected["kind"], wire);
            assert!(
                projected["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with("detail")),
                "{kind:?}: {projected}"
            );
            // Every wrapper-owned failure carries a concrete recovery action;
            // the semantic path is tested separately below and must not
            // acquire this prose.
            assert!(
                projected["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("Next step:")),
                "{kind:?}: {projected}"
            );
        }

        let resolution = Error::new(ErrorKind::InvalidDeclaration, "missing")
            .semantic()
            .with_resolution(json!({
                "kind": "missing_identifier",
                "suggested_imports": ["Require Import Demo.A."]
            }));
        assert_eq!(
            public_error(&resolution),
            json!({
                "kind": "invalid_declaration",
                "message": "missing",
                "resolution": {
                    "kind": "missing_identifier",
                    "suggested_imports": ["Require Import Demo.A."]
                }
            })
        );

        let semantic = Error::new(ErrorKind::QueryFailed, "Rocq: unknown reference").semantic();
        assert_eq!(
            public_error(&semantic),
            json!({"kind":"query_failed","message":"Rocq: unknown reference"})
        );
        for kind in [
            ErrorKind::InvalidDeclaration,
            ErrorKind::NotFound,
            ErrorKind::ProofStepFailed,
            ErrorKind::QueryFailed,
        ] {
            let diagnostic = format!("Rocq diagnostic for {kind:?}");
            let value = public_error(&Error::new(kind, &diagnostic).semantic());
            assert_eq!(value["message"], diagnostic, "{kind:?}: {value}");
            assert!(
                value["message"]
                    .as_str()
                    .unwrap()
                    .contains("Rocq diagnostic")
            );
        }
    }

    #[test]
    fn only_pet_transport_loss_maps_release_to_pet_lost() {
        for error in [
            PetError::ProcessLost("closed".into()),
            PetError::Protocol("invalid response".into()),
            PetError::OutputOverflow,
        ] {
            assert_eq!(pet_release_error(error).kind, ErrorKind::PetLost);
        }
        for error in [
            PetError::Invalid("bad state".into()),
            PetError::Environment("bad workspace".into()),
            PetError::Remote {
                code: -32601,
                kind: rocq_engine::pet::PetRemoteKind::MethodNotFound,
                message: "release rejected".into(),
                diagnostic: None,
            },
        ] {
            assert_eq!(
                pet_release_error(error).kind,
                ErrorKind::InvalidConfiguration
            );
        }
        assert_eq!(
            pet_release_error(PetError::Remote {
                code: -32004,
                kind: rocq_engine::pet::PetRemoteKind::Anomaly,
                message: "internal failure".into(),
                diagnostic: None,
            })
            .kind,
            ErrorKind::PetFailure
        );
        assert_eq!(
            pet_release_error(PetError::Cancelled).kind,
            ErrorKind::RequestCancelled
        );
        assert_eq!(
            pet_release_error(PetError::TimedOut { timeout_ms: 10 }).kind,
            ErrorKind::ProofStepTimeout
        );
    }

    #[test]
    fn nonsemantic_pet_failures_have_recovery_without_backend_identifiers() {
        for error in [
            PetError::Remote {
                code: -32601,
                kind: rocq_engine::pet::PetRemoteKind::MethodNotFound,
                message: "method petanque/legacy not found".into(),
                diagnostic: None,
            },
            PetError::Remote {
                code: -32004,
                kind: rocq_engine::pet::PetRemoteKind::Anomaly,
                message: "PET internal crash".into(),
                diagnostic: None,
            },
        ] {
            let value = public_error(&pet_release_error(error));
            let message = value["message"].as_str().unwrap();
            assert!(message.contains("Next step:"), "{value}");
            assert!(!message.contains("PET"), "{value}");
            assert!(!message.contains("petanque"), "{value}");
            assert!(!message.contains("-320"), "{value}");
        }

        let semantic = pet_release_error(PetError::Remote {
            code: -32008,
            kind: rocq_engine::pet::PetRemoteKind::ReferenceNotFound,
            message: "Reference_not_found: missing".into(),
            diagnostic: None,
        });
        assert_eq!(
            public_error(&semantic),
            json!({"kind":"invalid_configuration","message":"Reference_not_found: missing"})
        );
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
    fn proof_attempt_schemas_expose_one_optional_per_fragment_deadline() {
        for name in ["check", "try"] {
            let tool = tool_definitions()
                .iter()
                .find(|tool| tool.name == name)
                .unwrap();
            let schema = serde_json::to_value(tool).unwrap()["inputSchema"].clone();
            assert_eq!(schema["required"], json!(["attempts"]));
            assert_eq!(schema["properties"]["timeout_ms"]["type"], "integer");
            assert_eq!(schema["properties"]["timeout_ms"]["minimum"], 1);
            assert_eq!(schema["properties"]["trace"]["type"], "boolean");
            assert_eq!(schema["properties"]["structured"]["type"], "boolean");
            assert_eq!(schema["additionalProperties"], false);
        }
    }

    #[test]
    fn declare_schema_has_one_dune_owned_file_identity() {
        let declare = tool_definitions()
            .iter()
            .find(|tool| tool.name == "declare")
            .expect("declare tool exists");
        let schema = serde_json::to_value(declare).unwrap()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["name", "statement", "file"]));
        assert!(schema["properties"].get("library").is_none());
        assert!(
            declare
                .description
                .as_deref()
                .unwrap()
                .contains("callers do not repeat it")
        );
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
            dispatch(&runtime, &session, "prove", json!({"target":id})).unwrap()["checkpoint"]
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
        assert_eq!(first_view["checkpoint"], first);
        assert!(first_view["goals"].as_str().unwrap().contains("C : Prop"));
        let second_view =
            dispatch(&runtime, &session, "rewind", json!({"checkpoint":second})).unwrap();
        assert_eq!(second_view["checkpoint"], second);
        let second_goals = second_view["goals"].as_str().unwrap();
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
            dispatch(&runtime, &session, "prove", json!({"target":id.clone()})).unwrap();
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
            assert!(rejected.get("selected").is_none());
            assert!(rejected.get("error").is_none());
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 1);

            dispatch(
                &runtime,
                &session,
                "check",
                json!({"attempts":["intros A B."]}),
            )
            .unwrap();
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 2);
            dispatch(&runtime, &session, "abandon", json!({"target":id.clone()})).unwrap();
            assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 0);
        }
    }

    #[test]
    fn named_queries_use_their_target_document_without_disturbing_active_proof() {
        let project = dune_project(
            "Demo",
            &[
                (
                    "A.v",
                    "Theorem active : forall P : Prop, P -> P. Admitted.\n",
                ),
                ("B.v", "Theorem independent : True. Proof. exact I. Qed.\n"),
            ],
        );
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let active = json!({
            "file":"A.v",
            "qualified_path":["Demo","A","active"]
        });
        let independent = json!({
            "file":"B.v",
            "qualified_path":["Demo","B","independent"]
        });
        let opened = dispatch(
            &runtime,
            &server.session,
            "prove",
            json!({"target":active.clone()}),
        )
        .unwrap();
        let checkpoint = opened["checkpoint"].clone();
        let original_goals = opened["goals"].clone();
        let attached = server.session.project().unwrap();
        assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 1);

        // A.v deliberately does not Require B.v. Every named query must use
        // B's own post-declaration PET state rather than the retained A state.
        for kind in ["about", "print", "assumptions", "dependencies"] {
            let result = dispatch(
                &runtime,
                &server.session,
                "query",
                json!({"kind":kind,"target":independent.clone()}),
            )
            .unwrap();
            assert!(result["text"].is_string(), "{kind}: {result}");
            assert_eq!(
                attached.actor().diagnostic_state_count().unwrap(),
                1,
                "{kind} leaked its temporary target-document state"
            );
        }

        let missing = dispatch(
            &runtime,
            &server.session,
            "query",
            json!({
                "kind":"about",
                "target":{
                    "file":"B.v",
                    "qualified_path":["Demo","B","missing"]
                }
            }),
        )
        .unwrap_err();
        assert_eq!(missing.kind, ErrorKind::NotFound);
        assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 1);

        let after = dispatch(&runtime, &server.session, "query", json!({"kind":"goals"})).unwrap();
        assert_eq!(after["checkpoint"], checkpoint);
        assert_eq!(after["goals"], original_goals);
        dispatch(
            &runtime,
            &server.session,
            "abandon",
            json!({"target":active}),
        )
        .unwrap();
        assert_eq!(attached.actor().diagnostic_state_count().unwrap(), 0);
    }

    #[test]
    fn query_rebuilds_reverse_dependency_before_pet_consumes_it() {
        let project = dune_project(
            "Demo",
            &[
                (
                    "A.v",
                    "Definition base : nat := 0.\n\
                     Theorem base_zero : base = 0. Proof. reflexivity. Qed.\n",
                ),
                (
                    "B.v",
                    "From Demo Require Import A.\n\
                     Theorem consumer : base = 0. Proof. exact base_zero. Qed.\n",
                ),
            ],
        );
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let target = json!({
            "file":"B.v",
            "qualified_path":["Demo","B","consumer"]
        });
        let first = dispatch(
            &runtime,
            &server.session,
            "query",
            json!({"kind":"assumptions","target":target.clone()}),
        )
        .unwrap();
        assert!(first["text"].as_str().unwrap().contains("Closed"));

        // Change only the dependency source.  B's cached source and artifact
        // still look current to a wrapper-side fingerprint; the second query
        // must nevertheless ask Dune to rebuild B, then cross a PET epoch
        // before loading the new dependency assumptions.
        fs::write(
            project.path().join("A.v"),
            "Definition base : nat := 0.\n\
             Definition newly_added : nat := 1.\n\
             Theorem base_zero : base = 0. Proof. reflexivity. Qed.\n",
        )
        .unwrap();
        let second = dispatch(
            &runtime,
            &server.session,
            "query",
            json!({"kind":"assumptions","target":target}),
        )
        .unwrap();
        assert!(second["text"].as_str().unwrap().contains("Closed"));
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
            json!({"target":{"file":"A.v","qualified_path":["Demo","A","a"]}}),
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
            json!({"target":identity}),
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
                json!({"target":first_id.clone()}),
            )
            .unwrap();
        }
        dispatch(
            &runtime,
            &second_a.session,
            "prove",
            json!({"target":second_id}),
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
                json!({"target":id.clone()}),
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
    fn post_selection_trust_failure_uses_the_sparse_error_variant() {
        let project = dune_project(
            "Demo",
            &[(
                "A.v",
                "Theorem helper : True. Admitted.\nTheorem t : True. Admitted.\n",
            )],
        );
        let runtime = runtime();
        let server = attach(&runtime, &project);
        let target = json!({"file":"A.v","qualified_path":["Demo","A","t"]});
        dispatch(
            &runtime,
            &server.session,
            "prove",
            json!({"target":target.clone()}),
        )
        .unwrap();

        let result = dispatch(
            &runtime,
            &server.session,
            "check",
            json!({"attempts":["exact helper."]}),
        )
        .unwrap();
        assert_eq!(result["selected"], 0);
        assert_eq!(result["error"]["kind"], "unfinished_dependency");
        assert!(result.get("rejected").is_none());
        assert_eq!(result["state"]["target"], target);
        assert_eq!(result["state"]["status"], "Open");
        assert!(result["state"].get("checkpoint").is_some());
        assert_eq!(
            fs::read_to_string(project.path().join("A.v")).unwrap(),
            "Theorem helper : True. Admitted.\nTheorem t : True. Admitted.\n"
        );
        dispatch(
            &runtime,
            &server.session,
            "abandon",
            json!({"target":target}),
        )
        .unwrap();
    }

    #[test]
    fn completed_proofs_report_canonical_structured_dependencies() {
        let cases = [
            ("A.v", "short_dependency", "True"),
            (
                "B.v",
                "dependency_with_a_name_long_enough_to_wrap_human_readable_locate_output",
                "True",
            ),
            (
                "C.v",
                "dependency_with_multiline_type",
                "forall (P : Prop) (K : Type) (x y z : nat),\n  P -> P",
            ),
        ];
        let sources = cases
            .iter()
            .map(|(file, dependency, statement)| {
                let source = format!(
                    "Theorem {dependency} : {statement}.\n\
                     Admitted.\n\
                     Theorem target : {statement}.\n\
                     Proof. exact {dependency}. Qed.\n"
                );
                // Design note: the cases deliberately vary presentation width
                // while retaining each exact absolute identity for assertions.
                let unit = file.strip_suffix(".v").unwrap().to_owned();
                ((*file, unit, *dependency), source)
            })
            .collect::<Vec<_>>();
        let files = sources
            .iter()
            .map(|((file, _, _), source)| (*file, source.as_str()))
            .collect::<Vec<_>>();
        let project = dune_project("Demo", &files);
        let runtime = runtime();
        for ((file, unit, dependency), _) in &sources {
            let server = attach(&runtime, &project);
            let error = dispatch(
                &runtime,
                &server.session,
                "prove",
                json!({
                    "target": {
                        "file": file,
                        "qualified_path": ["Demo", unit, "target"]
                    }
                }),
            )
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::UnfinishedDependency);
            assert_eq!(
                error.message,
                format!("proof depends on unfinished declaration 'Demo.{unit}.{dependency}'")
            );
        }
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
            json!({"target":id.clone()}),
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
        assert_eq!(rewound["checkpoint"], root);
        dispatch(&runtime, &server.session, "abandon", json!({"target":id})).unwrap();
    }
}
