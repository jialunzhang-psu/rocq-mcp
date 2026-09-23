//! JSON-to-engine conversion for the six public tools.

use crate::{
    server::Selection,
    validation::{optional_string, optional_usize, reject_unknown, required_string},
};
use rocq_engine::{
    DeclarationIdentity, DeclarationKind, Engine, Error, ErrorKind, LogicalLibrary, NewDeclaration,
    Query, QueryResult,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// Project the engine's already-public user error into MCP JSON. Private
/// engine faults have already been normalized by the Engine boundary and can
/// never be classified or renamed here.
pub(crate) fn public_error(error: &Error) -> Value {
    let (kind, message) = match error.kind {
        ErrorKind::InvalidRequest => ("invalid_request", error.message.as_str()),
        ErrorKind::InvalidConfiguration => ("invalid_configuration", error.message.as_str()),
        ErrorKind::InvalidDeclaration => ("invalid_declaration", error.message.as_str()),
        ErrorKind::NotFound => ("not_found", error.message.as_str()),
        ErrorKind::Ambiguous => ("ambiguous", error.message.as_str()),
        ErrorKind::DeclarationChanged => ("declaration_changed", error.message.as_str()),
        ErrorKind::ProofStepFailed => ("proof_step_failed", error.message.as_str()),
        ErrorKind::ProofTimeout => ("proof_timeout", error.message.as_str()),
        ErrorKind::ProjectTimeout => ("project_timeout", error.message.as_str()),
        ErrorKind::BuildTimeout => ("build_timeout", error.message.as_str()),
        ErrorKind::AxiomDependencyOutOfScope => {
            ("axiom_dependency_out_of_scope", error.message.as_str())
        }
        ErrorKind::UnfinishedDependency => ("unfinished_dependency", error.message.as_str()),
    };
    json!({"kind":kind,"message":message})
}
/// Return the selected project or the user-facing missing-start error.
fn project(s: &Selection) -> Result<PathBuf, Error> {
    s.project
        .clone()
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call start first"))
}
pub(crate) fn identity_name(i: &DeclarationIdentity) -> String {
    let mut p = i.library.0.clone();
    p.extend(i.modules.clone());
    p.push(i.constant.clone());
    p.join(".")
}

/// Project internal proof state onto the four public JSON fields.
fn state_json(s: &rocq_engine::ProofState) -> Value {
    json!({
        "theorem": identity_name(&s.theorem.identity),
        "statement": s.theorem.statement,
        "status": format!("{:?}", s.lifecycle),
        "goals": s.goals,
    })
}
/// Dispatch one validated MCP tool call while updating its connection selection.
/// Engine errors are returned unchanged for one-to-one JSON projection.
pub(crate) fn dispatch(
    e: &Engine,
    cell: &Arc<Mutex<Selection>>,
    name: &str,
    a: Value,
) -> Result<Value, Error> {
    // Design note: this is the only MCP-to-engine business boundary.  All
    // proof semantics, persistence, PET state and publication remain in Engine.
    // A previous worker panic must not make the connection permanently
    // unusable; retain the state protected by the poisoned mutex.
    let mut s = cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match name {
        "start" => {
            reject_unknown(&a, &["project_path"])?;
            let p = std::fs::canonicalize(required_string(&a, "project_path")?).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "project path is unavailable",
                )
            })?;
            let c = e.catalog(&p)?;
            s.project = Some(p);
            s.attempt = None;
            let declarations = c
                .declarations
                .iter()
                .map(|d| {
                    json!({
                        "name": identity_name(&d.identity),
                        "statement": d.statement,
                        "status": format!("{:?}", d.status),
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({"declarations": declarations}))
        }
        "prove" => {
            reject_unknown(&a, &["theorem"])?;
            let p = project(&s)?;
            let n = required_string(&a, "theorem")?;
            // Design note: name validation and suffix resolution are engine
            // semantics; MCP only translates the request and response.
            let st = e.open_named(&p, &n)?;
            s.attempt = st.attempt;
            Ok(state_json(&st))
        }
        "declare" => {
            reject_unknown(&a, &["name", "statement", "kind", "library"])?;
            let p = project(&s)?;
            let n = required_string(&a, "name")?;
            let c = e.catalog(&p)?;
            let base = c.declarations.first().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "project has no logical library",
                )
            })?;
            let parts = n.split('.').map(str::to_owned).collect::<Vec<_>>();
            let Some(constant) = parts.last().cloned() else {
                return Err(Error::new(
                    ErrorKind::InvalidDeclaration,
                    "declaration name is empty",
                ));
            };
            // Design note: a logical library is explicit when creating a new
            // compilation unit; no filesystem path or module-boundary guess
            // crosses the public MCP boundary.
            let library = match a.get("library") {
                None => base.identity.library.clone(),
                Some(_) => LogicalLibrary(
                    required_string(&a, "library")?
                        .split('.')
                        .map(str::to_owned)
                        .collect(),
                ),
            };
            let identity = DeclarationIdentity {
                library,
                modules: parts[..parts.len() - 1].to_vec(),
                constant,
            };
            let kind_text = match a.get("kind") {
                None => "Theorem",
                Some(Value::String(value)) if !value.is_empty() => value,
                Some(_) => {
                    return Err(Error::new(
                        ErrorKind::InvalidDeclaration,
                        "declaration kind must be a non-empty string",
                    ));
                }
            };
            let kind = match kind_text.to_ascii_lowercase().as_str() {
                "definition" => DeclarationKind::Definition,
                "lemma" => DeclarationKind::Lemma,
                "theorem" => DeclarationKind::Theorem,
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidDeclaration,
                        format!("unsupported declaration kind '{kind_text}'"),
                    ));
                }
            };
            let st = e.declare(
                &p,
                NewDeclaration {
                    kind,
                    identity: identity.clone(),
                    context: vec![],
                    statement: required_string(&a, "statement")?,
                },
            )?;
            s.attempt = st.attempt;
            Ok(state_json(&st))
        }
        "check" => {
            reject_unknown(&a, &["commands"])?;
            let at = s
                .attempt
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?;
            let r = e.step(at, &required_string(&a, "commands")?)?;
            s.attempt = r.state.attempt;
            Ok(json!({
                "state": state_json(&r.state),
                "error": r.error.as_ref().map(public_error),
            }))
        }
        "check_multi" => {
            reject_unknown(&a, &["candidates"])?;
            let at = s
                .attempt
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?;
            let cs = a
                .get("candidates")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidRequest, "candidates must be an array")
                })?
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_owned).ok_or_else(|| {
                        Error::new(ErrorKind::InvalidRequest, "candidate must be a string")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let results = e.candidates(at, &cs)?;
            let candidates = results
                .into_iter()
                .map(|r| {
                    json!({
                        "solved": r.solved,
                        "state": r.state.map(|state| state_json(&state)),
                        "error": r.error.as_ref().map(public_error),
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({"candidates": candidates}))
        }
        "query" => {
            let p = project(&s)?;
            // Design note: query variants live directly in the tool arguments;
            // an extra `request` envelope carries no user-visible semantics.
            let q = &a;
            let kind = q
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "kind is required"))?;
            let fields: &[&str] = match kind {
                "goals" => &["kind"],
                "search" => &[
                    "kind",
                    "name_contains",
                    "statement_pattern",
                    "status",
                    "offset",
                    "limit",
                ],
                "statement" | "proof" | "definition" | "assumptions" | "dependencies" => {
                    &["kind", "target"]
                }
                "type" | "notations" => &["kind", "expression"],
                _ => &["kind"],
            };
            reject_unknown(q, fields)?;
            let query = match kind {
                "goals" => Query::Goals,
                "statement" => Query::Statement {
                    name: required_string(q, "target")?,
                },
                "proof" => Query::Proof {
                    name: required_string(q, "target")?,
                },
                "definition" => Query::Definition {
                    name: required_string(q, "target")?,
                },
                "assumptions" => Query::Assumptions {
                    name: required_string(q, "target")?,
                },
                "dependencies" => Query::Dependencies {
                    name: required_string(q, "target")?,
                },
                "type" => Query::ExpressionType {
                    expression: required_string(q, "expression")?,
                },
                "notations" => Query::Notation {
                    expression: required_string(q, "expression")?,
                },
                "search" => Query::Search {
                    name: optional_string(q, "name_contains")?,
                    statement: optional_string(q, "statement_pattern")?,
                    status: match q.get("status") {
                        None => None,
                        Some(Value::String(value)) if value == "Open" => {
                            Some(rocq_engine::ProofLifecycle::Open)
                        }
                        Some(Value::String(value)) if value == "Completed" => {
                            Some(rocq_engine::ProofLifecycle::Completed)
                        }
                        Some(Value::String(value)) if value == "Pending" => {
                            Some(rocq_engine::ProofLifecycle::Pending)
                        }
                        Some(Value::String(value)) if value == "Rejected" => {
                            Some(rocq_engine::ProofLifecycle::Rejected)
                        }
                        Some(_) => {
                            return Err(Error::new(
                                ErrorKind::InvalidRequest,
                                "invalid search status",
                            ));
                        }
                    },
                    offset: optional_usize(q, "offset", 0, 0, usize::MAX)?,
                    limit: optional_usize(q, "limit", 20, 1, 100)?,
                },
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "unsupported query kind",
                    ));
                }
            };
            match e.query(&p, s.attempt, query)? {
                QueryResult::Text(t) => Ok(json!({"text":t})),
                QueryResult::State(st) => Ok(state_json(&st)),
            }
        }
        _ => Err(Error::new(ErrorKind::InvalidRequest, "unknown tool")),
    }
}
