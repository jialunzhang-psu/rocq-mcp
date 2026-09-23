//! Rocq MCP server. Transport and protocol lifecycle are delegated to `rmcp`.

mod adapter;
mod schema;
mod server;
mod validation;

pub use schema::tool_definitions;
pub use server::RocqServer;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{identity_name, public_error};
    use rocq_engine::{DeclarationIdentity, Error, ErrorKind};
    use serde_json::json;

    #[test]
    fn official_adapter_exposes_exactly_six_tools() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 6);
        for tool in tools {
            let value = serde_json::to_value(tool).expect("tool is serializable");
            assert_eq!(value["inputSchema"]["additionalProperties"], false);
            let schema = value["inputSchema"].to_string();
            assert!(!schema.contains("cursor"));
            assert!(!schema.contains("workspace"));
            assert!(!schema.contains("pet_pid"));
            if tool.name != "start" {
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
        for field in ["target", "expression", "name_contains", "statement_pattern"] {
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
            library: rocq_engine::LogicalLibrary(vec!["Demo".into()]),
            modules: vec!["Arithmetic".into()],
            constant: "plus_comm".into(),
        };
        assert_eq!(identity_name(&identity), "Demo.Arithmetic.plus_comm");
    }

    #[test]
    fn tool_annotations_mark_queries_read_only() {
        for tool in tool_definitions() {
            let value = serde_json::to_value(tool).expect("tool is serializable");
            let read_only = value["annotations"]["readOnlyHint"].as_bool();
            if tool.name == "query" || tool.name == "check_multi" {
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
}
