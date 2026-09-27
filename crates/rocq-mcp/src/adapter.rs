//! JSON-to-engine conversion for the ten public tools.

use crate::{
    checkpoint::{CheckpointError, CheckpointId},
    server::Selection,
    validation::{reject_unknown, required_string},
};
use rocq_engine::{
    DeclarationIdentity, DeclarationKind, Engine, Error, ErrorKind, FileId, LogicalLibrary,
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
    i.qualified_name()
}

/// Project internal proof state and an optional request boundary onto JSON.
fn state_json(s: &rocq_engine::ProofState, checkpoint: Option<CheckpointId>) -> Value {
    let mut value = json!({
        "theorem": identity_name(&s.theorem.identity),
        "statement": s.theorem.statement,
        "status": format!("{:?}", s.lifecycle),
        "goals": s.goals,
    });
    if let Some(checkpoint) = checkpoint {
        value
            .as_object_mut()
            .expect("proof state projection is an object")
            .insert("checkpoint".into(), json!(checkpoint.get()));
    }
    value
}

fn checkpoint_error(error: CheckpointError) -> Error {
    match error {
        CheckpointError::NoActiveProof => Error::new(ErrorKind::InvalidRequest, "call prove first"),
        CheckpointError::Unknown => Error::new(
            ErrorKind::InvalidRequest,
            "checkpoint is not active for the selected proof",
        ),
        CheckpointError::TooFar => Error::new(
            ErrorKind::InvalidRequest,
            "rewind exceeds available check history",
        ),
        CheckpointError::Exhausted => Error::new(
            ErrorKind::InvalidConfiguration,
            "checkpoint identifier space is exhausted",
        ),
    }
}

fn declaration_json(d: &rocq_engine::DeclarationInfo) -> Value {
    json!({
        "id": {
            "file": d.identity.file.0,
            "qualified_path": d.identity.qualified_path,
        },
        "name": identity_name(&d.identity),
        "statement": d.statement,
        "kind": format!("{:?}", d.kind),
    })
}

fn declaration_id(value: &Value) -> Result<DeclarationIdentity, Error> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "declaration must be an object"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "file" | "qualified_path"))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "declaration has an unknown field",
        ));
    }
    let file = object
        .get("file")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "declaration.file is required"))?;
    let qualified_path = object
        .get("qualified_path")
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidRequest,
                "declaration.qualified_path is required",
            )
        })?
        .iter()
        .map(|part| {
            part.as_str()
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidRequest,
                        "declaration.qualified_path must contain strings",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DeclarationIdentity {
        file: FileId(file.replace('\\', "/")),
        qualified_path,
    })
}

/// Decode the shared ordered proof-fragment input used by `check` and `try`.
/// Count, sentence framing, and size limits are enforced by the engine so both
/// tools have exactly one semantic validation path.
fn proof_attempts(args: &Value) -> Result<Vec<String>, Error> {
    args.get("attempts")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "attempts must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "attempt must be a string"))
        })
        .collect()
}
/// Dispatch one validated MCP tool call while updating its connection selection.
/// Engine errors are returned unchanged for one-to-one JSON projection.
pub(crate) fn dispatch(
    e: &Engine,
    cell: &Arc<Mutex<Selection>>,
    name: &str,
    a: Value,
) -> Result<Value, Error> {
    // Design note: this is the only MCP-to-engine boundary. Proof semantics,
    // PET state, traces, and writeback remain below it in Engine.
    // A previous worker panic must not make the connection permanently
    // unusable; retain the state protected by the poisoned mutex.
    let mut s = cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match name {
        "start" => {
            reject_unknown(&a, &["project_path"])?;
            let requested = PathBuf::from(required_string(&a, "project_path")?);
            // Design note: attachment is an engine/Dune operation, not a
            // transport-side `canonicalize`. A successful response therefore
            // guarantees a real canonical Dune workspace without invoking PET.
            let project = e.attach(&requested)?;
            s.project = Some(project);
            s.checkpoints.clear_active();
            Ok(json!({"attached": true}))
        }
        "list_files" => {
            reject_unknown(&a, &[])?;
            let p = project(&s)?;
            let files = e
                .list_files(&p)?
                .into_iter()
                .map(|file| file.0)
                .collect::<Vec<_>>();
            Ok(json!({"files": files}))
        }
        "list_decls" => {
            reject_unknown(&a, &["file"])?;
            let p = project(&s)?;
            let file = required_string(&a, "file")?;
            let declarations = e.list_decls(&p, &rocq_engine::FileId(file.clone()))?;
            Ok(json!({
                "file": file,
                "declarations": declarations.iter().map(declaration_json).collect::<Vec<_>>()
            }))
        }
        "prove" => {
            reject_unknown(&a, &["declaration"])?;
            let p = project(&s)?;
            let identity = declaration_id(a.get("declaration").ok_or_else(|| {
                Error::new(ErrorKind::InvalidRequest, "declaration is required")
            })?)?;
            let st = e.open_declaration(&p, identity)?;
            let checkpoint = match st.attempt {
                Some(attempt) => Some(s.checkpoints.begin(attempt).map_err(checkpoint_error)?),
                None => {
                    s.checkpoints.clear_active();
                    None
                }
            };
            Ok(state_json(&st, checkpoint))
        }
        "abandon" => {
            reject_unknown(&a, &["declaration"])?;
            let p = project(&s)?;
            let identity = declaration_id(a.get("declaration").ok_or_else(|| {
                Error::new(ErrorKind::InvalidRequest, "declaration is required")
            })?)?;
            let abandoned = e.abandon(&p, identity)?;
            // The selected cursor may have belonged to the retired root.
            s.checkpoints.clear_active();
            Ok(json!({"abandoned":abandoned}))
        }
        "declare" => {
            reject_unknown(&a, &["name", "statement", "kind", "library", "file"])?;
            let p = project(&s)?;
            let n = required_string(&a, "name")?;
            let parts = n.split('.').map(str::to_owned).collect::<Vec<_>>();
            let Some(constant) = parts.last().cloned() else {
                return Err(Error::new(
                    ErrorKind::InvalidDeclaration,
                    "declaration name is empty",
                ));
            };
            // Dune must already own this compilation unit, so its logical
            // library is explicit and never inferred from declaration order.
            let library = LogicalLibrary(
                required_string(&a, "library")?
                    .split('.')
                    .map(str::to_owned)
                    .collect(),
            );
            let file = required_string(&a, "file")?.replace('\\', "/");
            let prefix = &parts[..parts.len() - 1];
            // Validate the caller's raw path before interpreting its library
            // prefix.  This keeps malformed components (for example `.foo`)
            // in the engine's generic identity category while preserving the
            // more specific wrong-library diagnostic for valid paths.
            rocq_engine::validate_identity(&DeclarationIdentity {
                file: FileId(file.clone()),
                qualified_path: parts.clone(),
            })?;
            if !prefix.is_empty() && !prefix.starts_with(library.0.as_slice()) {
                return Err(Error::new(
                    ErrorKind::InvalidDeclaration,
                    "declaration name must be local or qualified by its library and modules",
                ));
            }
            let modules = if prefix.starts_with(library.0.as_slice()) {
                prefix[library.0.len()..].to_vec()
            } else {
                prefix.to_vec()
            };
            let identity = DeclarationIdentity {
                file: FileId(file),
                qualified_path: library
                    .0
                    .iter()
                    .cloned()
                    .chain(modules)
                    .chain(std::iter::once(constant))
                    .collect(),
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
                kind,
                identity.clone(),
                required_string(&a, "statement")?,
            )?;
            let checkpoint = match st.attempt {
                Some(attempt) => Some(s.checkpoints.begin(attempt).map_err(checkpoint_error)?),
                None => {
                    s.checkpoints.clear_active();
                    None
                }
            };
            Ok(state_json(&st, checkpoint))
        }
        "check" => {
            reject_unknown(&a, &["attempts"])?;
            let (_, at) = s
                .checkpoints
                .current()
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?;
            let attempts = proof_attempts(&a)?;
            let r = e.check(at, &attempts)?;
            let checkpoint = match r.state.attempt {
                Some(attempt) => Some(s.checkpoints.commit(attempt).map_err(checkpoint_error)?),
                None => {
                    s.checkpoints.clear_active();
                    None
                }
            };
            Ok(json!({
                "selected": r.selected,
                "state": state_json(&r.state, checkpoint),
                "rejected": r.rejected.iter().map(public_error).collect::<Vec<_>>(),
                "error": r.error.as_ref().map(public_error),
            }))
        }
        "try" => {
            reject_unknown(&a, &["attempts"])?;
            let (_, at) = s
                .checkpoints
                .current()
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?;
            let attempts = proof_attempts(&a)?;
            let results = e.try_attempts(at, &attempts)?;
            let attempts = results
                .into_iter()
                .map(|r| {
                    json!({
                        "solved": r.solved,
                        "state": r.state.map(|state| state_json(&state, None)),
                        "error": r.error.as_ref().map(public_error),
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({"attempts": attempts}))
        }
        "rewind" => {
            reject_unknown(&a, &["steps", "checkpoint"])?;
            let steps = match a.get("steps") {
                None => None,
                Some(value) => {
                    Some(value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "steps must be a positive integer",
                        )
                    })?)
                }
            };
            let checkpoint = match a.get("checkpoint") {
                None => None,
                Some(value) => {
                    Some(value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "checkpoint must be a positive integer",
                        )
                    })?)
                }
            };
            if steps.is_some() && checkpoint.is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "steps and checkpoint are mutually exclusive",
                ));
            }
            let (_, current) = s
                .checkpoints
                .current()
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?;
            let (target_id, target) = match checkpoint {
                Some(id) => s
                    .checkpoints
                    .lookup(CheckpointId::from_u64(id))
                    .map_err(checkpoint_error)?,
                None => s
                    .checkpoints
                    .steps_back(steps.unwrap_or(1))
                    .map_err(checkpoint_error)?,
            };
            let state = e.checkout(current, target)?;
            s.checkpoints.select(target_id).map_err(checkpoint_error)?;
            Ok(json!({"state": state_json(&state, Some(target_id))}))
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
                "search" => &["kind", "pattern", "at"],
                "statement" | "proof" | "definition" | "assumptions" | "dependencies" => {
                    &["kind", "target"]
                }
                "type" | "notations" => &["kind", "expression", "at"],
                _ => &["kind"],
            };
            reject_unknown(q, fields)?;
            if kind == "goals" {
                let (checkpoint, attempt) = s.checkpoints.current().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidRequest, "goals requires attempt")
                })?;
                return Ok(state_json(&e.query_goals(&p, attempt)?, Some(checkpoint)));
            }
            let text = match kind {
                "statement" => e.query_statement(
                    &p,
                    &declaration_id(q.get("target").ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "target must be a non-empty string",
                        )
                    })?)?,
                )?,
                "proof" => e.query_proof(
                    &p,
                    &declaration_id(q.get("target").ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "target must be a non-empty string",
                        )
                    })?)?,
                )?,
                "definition" => e.query_definition(
                    &p,
                    &declaration_id(q.get("target").ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "target must be a non-empty string",
                        )
                    })?)?,
                )?,
                "assumptions" => e.query_assumptions(
                    &p,
                    &declaration_id(q.get("target").ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "target must be a non-empty string",
                        )
                    })?)?,
                )?,
                "dependencies" => e.query_dependencies(
                    &p,
                    &declaration_id(q.get("target").ok_or_else(|| {
                        Error::new(
                            ErrorKind::InvalidRequest,
                            "target must be a non-empty string",
                        )
                    })?)?,
                )?,
                "type" => {
                    // Validate the required expression before decoding an
                    // optional context. This keeps malformed requests
                    // deterministic: a missing/ill-typed expression is not
                    // masked by an unrelated malformed `at` value.
                    let expression = required_string(q, "expression")?;
                    let at = q.get("at").map(declaration_id).transpose()?;
                    let attempt = s.checkpoints.current().map(|(_, attempt)| attempt);
                    e.query_expression_type(&p, attempt, expression, at.as_ref())?
                }
                "notations" => {
                    let expression = required_string(q, "expression")?;
                    let at = q.get("at").map(declaration_id).transpose()?;
                    let attempt = s.checkpoints.current().map(|(_, attempt)| attempt);
                    e.query_notation(&p, attempt, expression, at.as_ref())?
                }
                "search" => {
                    let pattern = required_string(q, "pattern")?;
                    let at = q.get("at").map(declaration_id).transpose()?;
                    let attempt = s.checkpoints.current().map(|(_, attempt)| attempt);
                    e.query_search(&p, attempt, pattern, at.as_ref())?
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidRequest,
                        "unsupported query kind",
                    ));
                }
            };
            Ok(json!({"text": text}))
        }
        _ => Err(Error::new(ErrorKind::InvalidRequest, "unknown tool")),
    }
}
