//! Rocq MCP server. Transport and protocol lifecycle are delegated to `rmcp`.

mod adapter;
mod checkpoint;
mod schema;
mod server;
mod validation;

pub use schema::tool_definitions;
pub use server::RocqServer;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{dispatch, identity_name, public_error};
    use rocq_engine::{DeclarationIdentity, Engine, EngineConfig, Error, ErrorKind, FileId};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

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
        assert_eq!(identity_name(&identity), "Demo.plus_comm");
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
    fn rewind_without_a_selected_proof_is_rejected_before_engine_work() {
        let state = tempfile::tempdir().unwrap();
        let engine = Engine::new(EngineConfig {
            state_parent: state.path().to_owned(),
            ..Default::default()
        })
        .unwrap();
        let selection = Arc::new(Mutex::new(crate::server::Selection::default()));
        let error = dispatch(&engine, &selection, "rewind", json!({})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidRequest);
        assert_eq!(error.message, "call prove first");
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
}
