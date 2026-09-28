//! The ten public MCP operations mapped directly to Dune, PET, and writeback.

use crate::{
    checkpoint::{CheckpointError, CheckpointId},
    server::{ProjectRuntime, Selection, ServerRuntime, SessionCell},
    validation::{reject_unknown, required_string},
};
use rocq_engine::{
    DeclarationIdentity, DeclarationInfo, DeclarationKind, Engine, Error, ErrorKind, FileId,
    OpenResult, OpenedProof, PetQuery, PetStateId, ProofState, validate_fragments,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// Maximum raw UTF-8 bytes returned by one semantic text query response.
/// PET still owns the complete result; this limit bounds only MCP transport.
const QUERY_PAGE_BYTES: usize = 32 * 1024;

pub(crate) fn public_error(error: &Error) -> Value {
    let kind = match error.kind {
        ErrorKind::InvalidRequest => "invalid_request",
        ErrorKind::InvalidConfiguration => "invalid_configuration",
        ErrorKind::InvalidDeclaration => "invalid_declaration",
        ErrorKind::NotFound => "not_found",
        ErrorKind::Ambiguous => "ambiguous",
        ErrorKind::DeclarationChanged => "declaration_changed",
        ErrorKind::ProofStepFailed => "proof_step_failed",
        ErrorKind::PetLost => "pet_lost",
        ErrorKind::QueryFailed => "query_failed",
        ErrorKind::PetFailure => "pet_failure",
        ErrorKind::ProjectTimeout => "project_timeout",
        ErrorKind::BuildTimeout => "build_timeout",
        ErrorKind::AxiomDependencyOutOfScope => "axiom_dependency_out_of_scope",
        ErrorKind::UnfinishedDependency => "unfinished_dependency",
    };
    json!({"kind": kind, "message": error.message})
}

fn state_json(state: &ProofState, checkpoint: Option<CheckpointId>) -> Value {
    let mut value = json!({
        "target": declaration_identity_json(&state.theorem.identity),
        "status": format!("{:?}", state.lifecycle),
    });
    // Design note: an empty goal rendering carries no information after PET
    // has reported completion (or for a hypothetical solved `try` result).
    // Keep the wire state sparse; `try.solved` remains the explicit marker for
    // a hypothetical terminal result because its status intentionally stays
    // `Open` until `check` publishes it.
    if !state.goals.is_empty() {
        value["goals"] = json!(state.goals);
    }
    if let Some(checkpoint) = checkpoint {
        value["checkpoint"] = json!(checkpoint.get());
    }
    value
}

/// Project one canonical declaration identity into the reusable public wire
/// shape. Proof-state responses deliberately contain no second, lossy name.
fn declaration_identity_json(identity: &DeclarationIdentity) -> Value {
    json!({
        "file": identity.file.0,
        "qualified_path": identity.qualified_path,
    })
}

fn declaration_json(declaration: &DeclarationInfo) -> Value {
    json!({
        "id": declaration_identity_json(&declaration.identity),
        "statement": declaration.statement,
        "kind": format!("{:?}", declaration.kind),
    })
}

fn declaration_id(value: &Value, field: &str) -> Result<DeclarationIdentity, Error> {
    let object = value.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidRequest,
            format!("{field} must be an object"),
        )
    })?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "file" | "qualified_path"))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            format!("{field} has an unknown field"),
        ));
    }
    let file = object
        .get("file")
        .and_then(Value::as_str)
        .filter(|file| !file.is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidRequest,
                format!("{field}.file is required"),
            )
        })?;
    let qualified_path = object
        .get("qualified_path")
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidRequest,
                format!("{field}.qualified_path is required"),
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
                        "qualified_path must contain non-empty strings",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DeclarationIdentity {
        file: FileId(file.replace('\\', "/")),
        qualified_path,
    })
}

fn attempts(args: &Value) -> Result<Vec<String>, Error> {
    let values = args
        .get("attempts")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "attempts must be an array"))?;
    let fragments = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "attempt must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_fragments(&fragments)?;
    Ok(fragments)
}

/// Dispatch one request. Lock order is project operation barrier, then
/// connection selection. This lets publication invalidate other sessions
/// without racing a queued operation that still holds its session mutex.
pub(crate) fn dispatch(
    runtime: &Arc<ServerRuntime>,
    session: &Arc<SessionCell>,
    name: &str,
    args: Value,
) -> Result<Value, Error> {
    if name == "start" {
        return start(runtime, session, &args);
    }
    let project = session
        .project()
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call start first"))?;
    let _operation = project.lock();
    let layout_changed = runtime.engine.refresh_project(project.project())?;
    let mut selection = session
        .selection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !selection
        .project
        .as_ref()
        .is_some_and(|attached| Arc::ptr_eq(attached, &project))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "attached project changed before request admission",
        ));
    }
    if layout_changed {
        // Dune changed the environment that gives PET states their meaning.
        // Drop the child before any old state handle can be consumed; exact
        // proof text/checkpoint topology remains available for lazy replay.
        project.actor().restart();
        runtime.invalidate_project_states(&project, session, &mut selection);
    }
    let result = dispatch_attached(runtime, session, &project, &mut selection, name, &args);
    if result
        .as_ref()
        .is_err_and(|error| error.kind == ErrorKind::PetLost)
    {
        runtime.invalidate_project_states(&project, session, &mut selection);
    }
    result
}

fn start(
    runtime: &Arc<ServerRuntime>,
    session: &Arc<SessionCell>,
    args: &Value,
) -> Result<Value, Error> {
    reject_unknown(args, &["project_path"])?;
    let requested = PathBuf::from(required_string(args, "project_path")?);
    let attached = runtime.engine.attach(&requested)?;
    let next = runtime.project(attached);
    if let Some(previous) = session.project() {
        if Arc::ptr_eq(&previous, &next) {
            let _operation = previous.lock();
            let mut selection = session
                .selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Err(error) = retire_proof(&previous, &mut selection) {
                if error.kind == ErrorKind::PetLost {
                    runtime.invalidate_project_states(&previous, session, &mut selection);
                }
                return Err(error);
            }
            install_start_view(runtime, session, &next, &mut selection)?;
            return Ok(json!({}));
        }

        // Never hold two project operation barriers at once: sessions may
        // switch projects in opposite directions. Retire the previous proof,
        // detach, then admit the independently serialized target runtime.
        let _operation = previous.lock();
        let mut selection = session
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = retire_proof(&previous, &mut selection) {
            if error.kind == ErrorKind::PetLost {
                runtime.invalidate_project_states(&previous, session, &mut selection);
            }
            return Err(error);
        }
        selection.project = None;
    }

    let _operation = next.lock();
    let mut selection = session
        .selection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    install_start_view(runtime, session, &next, &mut selection)?;
    Ok(json!({}))
}

/// Admit one freshly attached Dune view under the target project's operation
/// barrier. A changed view is a project-wide PET epoch boundary, not merely a
/// replacement of the calling connection's cached project value.
fn install_start_view(
    runtime: &Arc<ServerRuntime>,
    session: &Arc<SessionCell>,
    project: &Arc<ProjectRuntime>,
    selection: &mut Selection,
) -> Result<(), Error> {
    // The initial attach identifies the runtime, but the view is queried
    // again inside its barrier so two racing `start` calls cannot install an
    // older Dune result after a newer one.
    let layout_changed = runtime.engine.refresh_project(project.project())?;
    selection.project = Some(Arc::clone(project));
    if layout_changed {
        project.actor().restart();
        runtime.invalidate_project_states(project, session, selection);
    }
    Ok(())
}

fn dispatch_attached(
    runtime: &Arc<ServerRuntime>,
    session: &Arc<SessionCell>,
    project: &Arc<ProjectRuntime>,
    selection: &mut Selection,
    name: &str,
    args: &Value,
) -> Result<Value, Error> {
    let engine = runtime.engine.as_ref();
    match name {
        "list_files" => {
            reject_unknown(args, &[])?;
            let files = engine
                .list_files(project.project())?
                .into_iter()
                .map(|file| file.0)
                .collect::<Vec<_>>();
            Ok(json!({"files": files}))
        }
        "list_decls" => {
            reject_unknown(args, &["file"])?;
            let file = FileId(required_string(args, "file")?.replace('\\', "/"));
            let declarations = engine.list_decls(project.project(), project.actor(), &file)?;
            Ok(json!({
                "declarations": declarations.iter().map(declaration_json).collect::<Vec<_>>(),
            }))
        }
        "prove" => {
            reject_unknown(args, &["target"])?;
            let identity = declaration_id(
                args.get("target")
                    .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "target is required"))?,
                "target",
            )?;
            reject_active_proof(selection, "prove")?;
            match engine.open_declaration(project.project(), project.actor(), identity)? {
                OpenResult::Published(target) => {
                    runtime.invalidate_project_states(project, session, selection);
                    let state =
                        engine.validate_published(project.project(), project.actor(), &target)?;
                    Ok(state_json(&state, None))
                }
                OpenResult::Open(opened) => begin_proof(project, selection, opened),
            }
        }
        "declare" => {
            reject_unknown(args, &["name", "statement", "kind", "file"])?;
            let name = required_string(args, "name")?;
            let file = FileId(required_string(args, "file")?.replace('\\', "/"));
            let kind = declaration_kind(args.get("kind").and_then(Value::as_str))?;
            let statement = required_string(args, "statement")?;
            reject_active_proof(selection, "declare")?;
            let opened = engine.declare(
                project.project(),
                project.actor(),
                kind,
                file,
                &name,
                &statement,
            )?;
            begin_proof(project, selection, opened)
        }
        "abandon" => {
            reject_unknown(args, &["target"])?;
            let identity = declaration_id(
                args.get("target")
                    .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "target is required"))?,
                "target",
            )?;
            let proof = selection
                .checkpoints
                .proof
                .as_ref()
                .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?;
            if proof.target.identity() != &identity {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    "no active unpublished proof has that declaration",
                ));
            }
            retire_proof(project, selection)?;
            Ok(json!({}))
        }
        "check" => check(runtime, session, project, selection, args),
        "try" => try_fragments(engine, project, selection, args),
        "rewind" => rewind(engine, project, selection, args),
        "query" => query(engine, project, selection, args),
        _ => Err(Error::new(ErrorKind::InvalidRequest, "unknown tool")),
    }
}

fn begin_proof(
    project: &ProjectRuntime,
    selection: &mut Selection,
    opened: OpenedProof,
) -> Result<Value, Error> {
    let state = opened.state;
    let view = opened.view.clone();
    let checkpoint =
        match selection
            .checkpoints
            .begin(opened.target, opened.state, opened.finished, opened.view)
        {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                project
                    .actor()
                    .release_states(&[state])
                    .map_err(pet_release_error)?;
                return Err(checkpoint_error(error));
            }
        };
    Ok(state_json(&view, Some(checkpoint)))
}

/// Reject proof replacement without touching the active checkpoint graph or
/// its PET states. The caller must explicitly abandon the selected proof.
fn reject_active_proof(selection: &Selection, operation: &str) -> Result<(), Error> {
    if selection.checkpoints.proof.is_some() {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            format!("abandon the active proof before calling {operation}"),
        ));
    }
    Ok(())
}

fn check(
    runtime: &Arc<ServerRuntime>,
    session: &Arc<SessionCell>,
    project: &Arc<ProjectRuntime>,
    selection: &mut Selection,
    args: &Value,
) -> Result<Value, Error> {
    reject_unknown(args, &["attempts"])?;
    let fragments = attempts(args)?;
    let (base_id, base_state) = ensure_current_state(runtime.engine.as_ref(), project, selection)?;
    let target = selection
        .checkpoints
        .proof
        .as_ref()
        .expect("current state has a proof")
        .target
        .clone();
    let mut rejected = Vec::new();
    for (index, fragment) in fragments.iter().enumerate() {
        match runtime.engine.run(
            project.project(),
            project.actor(),
            &target,
            base_state,
            fragment,
        ) {
            Ok(step) => {
                let checkpoint = match selection.checkpoints.commit(
                    step.state,
                    fragment.clone(),
                    step.finished,
                    step.view.clone(),
                ) {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => {
                        project
                            .actor()
                            .release_states(&[step.state])
                            .map_err(pet_release_error)?;
                        return Err(checkpoint_error(error));
                    }
                };
                if !step.finished {
                    return Ok(selected_result(
                        index,
                        &step.view,
                        Some(checkpoint),
                        rejected,
                    ));
                }
                let path = selection
                    .checkpoints
                    .fragments(checkpoint)
                    .map_err(checkpoint_error)?;
                if let Err(error) = runtime.engine.close_proof(
                    project.project(),
                    project.actor(),
                    &target,
                    step.state,
                ) {
                    if error.kind == ErrorKind::PetLost {
                        runtime.invalidate_project_states(project, session, selection);
                    }
                    return Ok(selected_error(
                        index,
                        &step.view,
                        Some(checkpoint),
                        rejected,
                        &error,
                    ));
                }
                match runtime.engine.publish(
                    project.project(),
                    project.actor(),
                    &target,
                    &path,
                    || runtime.invalidate_project_states(project, session, selection),
                ) {
                    Ok(state) => {
                        selection.checkpoints.clear();
                        return Ok(selected_result(index, &state, None, rejected));
                    }
                    Err(error) => {
                        let checkpoint = if error.kind == ErrorKind::DeclarationChanged {
                            selection.checkpoints.clear();
                            None
                        } else {
                            Some(checkpoint)
                        };
                        return Ok(selected_error(
                            index, &step.view, checkpoint, rejected, &error,
                        ));
                    }
                }
            }
            Err(error) if error.kind == ErrorKind::ProofStepFailed => {
                rejected.push(public_error(&error));
            }
            Err(error) => return Err(error),
        }
    }
    let checkpoint = selection
        .checkpoints
        .lookup(base_id)
        .map_err(checkpoint_error)?;
    Ok(json!({
        "state": state_json(&checkpoint.view, Some(base_id)),
        "rejected": rejected,
    }))
}

fn selected_result(
    selected: usize,
    state: &ProofState,
    checkpoint: Option<CheckpointId>,
    rejected: Vec<Value>,
) -> Value {
    let mut result = json!({
        "selected": selected,
        "state": state_json(state, checkpoint),
    });
    if !rejected.is_empty() {
        result["rejected"] = json!(rejected);
    }
    result
}

fn selected_error(
    selected: usize,
    state: &ProofState,
    checkpoint: Option<CheckpointId>,
    rejected: Vec<Value>,
    error: &Error,
) -> Value {
    let mut result = json!({
        "selected": selected,
        "state": state_json(state, checkpoint),
        "error": public_error(error),
    });
    if !rejected.is_empty() {
        result["rejected"] = json!(rejected);
    }
    result
}

fn try_fragments(
    engine: &Engine,
    project: &ProjectRuntime,
    selection: &mut Selection,
    args: &Value,
) -> Result<Value, Error> {
    reject_unknown(args, &["attempts"])?;
    let fragments = attempts(args)?;
    let (_, base_state) = ensure_current_state(engine, project, selection)?;
    let target = selection
        .checkpoints
        .proof
        .as_ref()
        .expect("current state has a proof")
        .target
        .clone();
    let mut output = Vec::new();
    for fragment in fragments {
        match engine.run(
            project.project(),
            project.actor(),
            &target,
            base_state,
            &fragment,
        ) {
            Ok(step) => {
                let state = state_json(&step.view, None);
                project
                    .actor()
                    .release_states(&[step.state])
                    .map_err(pet_release_error)?;
                output.push(json!({"solved": step.finished, "state": state}));
            }
            Err(error) if error.kind == ErrorKind::ProofStepFailed => {
                output.push(json!({
                    "solved": false,
                    "error": public_error(&error),
                }));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(json!({"attempts": output}))
}

fn rewind(
    engine: &Engine,
    project: &ProjectRuntime,
    selection: &mut Selection,
    args: &Value,
) -> Result<Value, Error> {
    reject_unknown(args, &["steps", "checkpoint"])?;
    if args.get("steps").is_some() && args.get("checkpoint").is_some() {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "steps and checkpoint are mutually exclusive",
        ));
    }
    let target =
        if let Some(value) = args.get("checkpoint") {
            CheckpointId::from_u64(value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                Error::new(ErrorKind::InvalidRequest, "checkpoint must be positive")
            })?)
        } else if let Some(value) = args.get("steps") {
            let steps = value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidRequest,
                    "steps must be a positive integer",
                )
            })?;
            selection
                .checkpoints
                .steps_back(steps)
                .map_err(checkpoint_error)?
        } else {
            selection
                .checkpoints
                .steps_back(1)
                .map_err(checkpoint_error)?
        };
    let proof_target = selection
        .checkpoints
        .proof
        .as_ref()
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call prove first"))?
        .target
        .clone();
    engine.validate_target(project.project(), &proof_target)?;
    if selection
        .checkpoints
        .lookup(target)
        .map_err(checkpoint_error)?
        .pet_state
        .is_none()
    {
        replay_checkpoint(engine, project, selection, target)?;
    }
    selection
        .checkpoints
        .select(target)
        .map_err(checkpoint_error)?;
    let checkpoint = selection
        .checkpoints
        .lookup(target)
        .map_err(checkpoint_error)?;
    Ok(state_json(&checkpoint.view, Some(target)))
}

fn query(
    engine: &Engine,
    project: &ProjectRuntime,
    selection: &mut Selection,
    args: &Value,
) -> Result<Value, Error> {
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "kind is required"))?;
    match kind {
        "goals" => {
            reject_unknown(args, &["kind"])?;
            let (checkpoint, state) = ensure_current_state(engine, project, selection)?;
            let proof = selection.checkpoints.proof.as_ref().unwrap();
            let view = engine.goals(project.project(), project.actor(), &proof.target, state)?;
            Ok(state_json(&view, Some(checkpoint)))
        }
        "about" | "print" | "assumptions" | "dependencies" => {
            reject_unknown(args, &["kind", "target", "offset"])?;
            let offset = query_offset(args)?;
            let identity = declaration_id(
                args.get("target")
                    .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "target is required"))?,
                "target",
            )?;
            let name = identity.qualified_name();
            let query = match kind {
                "about" => PetQuery::About(name),
                "print" => PetQuery::Print(name),
                "assumptions" => PetQuery::Assumptions(name),
                "dependencies" => PetQuery::Dependencies(name),
                _ => unreachable!(),
            };
            // Design note: a selected proof supplies the authoritative PET
            // context. Reopening the declaration at its source position
            // would silently lose local hypotheses and make named queries
            // disagree with the other queries in the same proof.
            if selection.checkpoints.proof.is_some() {
                let (_, state) = ensure_current_state(engine, project, selection)?;
                let proof = selection.checkpoints.proof.as_ref().unwrap();
                return text_result(
                    engine.query_state(
                        project.project(),
                        project.actor(),
                        &proof.target,
                        state,
                        query,
                    )?,
                    offset,
                );
            }
            text_result(
                engine.query_at(project.project(), project.actor(), &identity, query)?,
                offset,
            )
        }
        "search" | "type" | "notations" => {
            let allowed = if kind == "search" {
                &["kind", "pattern", "at", "offset"][..]
            } else {
                &["kind", "expression", "at", "offset"][..]
            };
            reject_unknown(args, allowed)?;
            let offset = query_offset(args)?;
            let query = match kind {
                "search" => PetQuery::Search(required_string(args, "pattern")?),
                "type" => PetQuery::ExpressionType(required_string(args, "expression")?),
                "notations" => PetQuery::Notation(required_string(args, "expression")?),
                _ => unreachable!(),
            };
            // Design note: `at` is an explicit context selector, so it must
            // override rather than be shadowed by an active proof.
            if let Some(at) = args.get("at") {
                let identity = declaration_id(at, "at")?;
                return text_result(
                    engine.query_at(project.project(), project.actor(), &identity, query)?,
                    offset,
                );
            }
            if selection.checkpoints.proof.is_some() {
                let (_, state) = ensure_current_state(engine, project, selection)?;
                let proof = selection.checkpoints.proof.as_ref().unwrap();
                return text_result(
                    engine.query_state(
                        project.project(),
                        project.actor(),
                        &proof.target,
                        state,
                        query,
                    )?,
                    offset,
                );
            }
            Err(Error::new(
                ErrorKind::InvalidRequest,
                "at is required when no proof is selected",
            ))
        }
        _ => Err(Error::new(
            ErrorKind::InvalidRequest,
            "unsupported query kind",
        )),
    }
}

/// Parse the optional stateless text continuation offset. The value counts
/// UTF-8 bytes and is validated against the materialized PET result by
/// `text_result`; this function has no side effects.
fn query_offset(args: &Value) -> Result<usize, Error> {
    let Some(value) = args.get("offset") else {
        return Ok(0);
    };
    let offset = value.as_u64().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidRequest,
            "offset must be a non-negative integer",
        )
    })?;
    usize::try_from(offset)
        .map_err(|_| Error::new(ErrorKind::InvalidRequest, "offset is too large"))
}

/// Project a complete PET text result into one bounded MCP page.
///
/// `offset` is a UTF-8 byte boundary previously returned as `next_offset`.
/// Small first pages preserve the original `{text}` shape. Paged responses
/// add only `next_offset` when another page exists. Invalid or stale offsets
/// fail without retaining any wrapper-side query state. The request already
/// carries the current offset, so the response exposes only the continuation
/// token.
fn text_result(text: String, offset: usize) -> Result<Value, Error> {
    let length = text.len();
    if offset > length || !text.is_char_boundary(offset) || (offset == length && !text.is_empty()) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "offset is not a valid boundary in the query result",
        ));
    }
    if offset == 0 && length <= QUERY_PAGE_BYTES {
        return Ok(json!({"text": text}));
    }
    let mut end = offset.saturating_add(QUERY_PAGE_BYTES).min(length);
    while end > offset && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = json!({"text": &text[offset..end]});
    if end < length {
        result["next_offset"] = json!(end);
    }
    Ok(result)
}

fn ensure_current_state(
    engine: &Engine,
    project: &ProjectRuntime,
    selection: &mut Selection,
) -> Result<(CheckpointId, PetStateId), Error> {
    let (checkpoint, state) = {
        let (checkpoint, value) = selection.checkpoints.current().map_err(checkpoint_error)?;
        (checkpoint, value.pet_state)
    };
    let state = match state {
        Some(state) => state,
        None => replay_checkpoint(engine, project, selection, checkpoint)?,
    };
    Ok((checkpoint, state))
}

/// Lazily rebuild the missing suffix of one root-to-checkpoint path. Every
/// newly exported state is retained by its checkpoint; parent states are never
/// released merely because their child was replayed.
fn replay_checkpoint(
    engine: &Engine,
    project: &ProjectRuntime,
    selection: &mut Selection,
    target: CheckpointId,
) -> Result<PetStateId, Error> {
    let path = selection
        .checkpoints
        .path(target)
        .map_err(checkpoint_error)?;
    let records = path
        .iter()
        .map(|id| {
            selection
                .checkpoints
                .lookup(*id)
                .cloned()
                .map(|checkpoint| (*id, checkpoint))
                .map_err(checkpoint_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let missing = records
        .iter()
        .position(|(_, checkpoint)| checkpoint.pet_state.is_none());
    let Some(missing) = missing else {
        return Ok(records.last().unwrap().1.pet_state.unwrap());
    };
    let proof_target = selection
        .checkpoints
        .proof
        .as_ref()
        .expect("checkpoint path has a proof")
        .target
        .clone();
    let mut staged = Vec::<(CheckpointId, PetStateId, ProofState)>::new();
    let mut state;
    let start;
    if missing == 0 {
        let opened = engine.replay_root(project.project(), project.actor(), &proof_target)?;
        if opened.finished != records[0].1.finished {
            project
                .actor()
                .release_states(&[opened.state])
                .map_err(pet_release_error)?;
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "PET root completion changed during replay",
            ));
        }
        state = opened.state;
        staged.push((records[0].0, opened.state, opened.view));
        start = 1;
    } else {
        state = records[missing - 1].1.pet_state.unwrap();
        start = missing;
    }
    for (checkpoint_id, checkpoint) in records.iter().skip(start) {
        let input = checkpoint.accepted_input.as_deref().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "non-root checkpoint has no accepted input",
            )
        })?;
        let step = match engine.run(
            project.project(),
            project.actor(),
            &proof_target,
            state,
            input,
        ) {
            Ok(step) => step,
            Err(error) => {
                release_staged(project, &staged)?;
                return Err(error);
            }
        };
        if step.finished != checkpoint.finished {
            let mut states = staged
                .iter()
                .map(|(_, state, _)| *state)
                .collect::<Vec<_>>();
            states.push(step.state);
            project
                .actor()
                .release_states(&states)
                .map_err(pet_release_error)?;
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "PET completion diverged while replaying the checkpoint",
            ));
        }
        state = step.state;
        staged.push((*checkpoint_id, step.state, step.view));
    }
    let mut replaced = Vec::new();
    if let Some(proof) = &mut selection.checkpoints.proof {
        for (checkpoint, state, view) in staged {
            let node = proof.checkpoints.get_mut(&checkpoint).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "checkpoint disappeared during replay",
                )
            })?;
            if let Some(previous) = node.pet_state.replace(state) {
                replaced.push(previous);
            }
            node.view = view;
        }
    }
    project
        .actor()
        .release_states(&replaced)
        .map_err(pet_release_error)?;
    Ok(state)
}

fn release_staged(
    project: &ProjectRuntime,
    staged: &[(CheckpointId, PetStateId, ProofState)],
) -> Result<(), Error> {
    let states = staged
        .iter()
        .map(|(_, state, _)| *state)
        .collect::<Vec<_>>();
    project
        .actor()
        .release_states(&states)
        .map_err(pet_release_error)
}

fn retire_proof(project: &ProjectRuntime, selection: &mut Selection) -> Result<(), Error> {
    let states = selection.checkpoints.proof.as_ref().map(|proof| {
        proof
            .checkpoints
            .values()
            .filter_map(|checkpoint| checkpoint.pet_state)
            .collect::<Vec<_>>()
    });
    if let Some(states) = states {
        if let Err(error) = project.actor().release_states(&states) {
            // Design note: a lost PET has already discarded every exported
            // state when its process owner was dropped.  Keep the proof only
            // for a live-process protocol error; the caller will invalidate
            // all project sessions for a transport loss.
            if error.is_transport_loss() {
                selection.checkpoints.clear();
            }
            return Err(pet_release_error(error));
        }
        selection.checkpoints.clear();
    }
    Ok(())
}

fn declaration_kind(value: Option<&str>) -> Result<DeclarationKind, Error> {
    match value.unwrap_or("Theorem").to_ascii_lowercase().as_str() {
        "theorem" => Ok(DeclarationKind::Theorem),
        "lemma" => Ok(DeclarationKind::Lemma),
        "definition" => Ok(DeclarationKind::Definition),
        _ => Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "unsupported declaration kind",
        )),
    }
}

fn checkpoint_error(error: CheckpointError) -> Error {
    match error {
        CheckpointError::NoActiveProof => Error::new(ErrorKind::InvalidRequest, "call prove first"),
        CheckpointError::ActiveProof => Error::new(
            ErrorKind::InvalidConfiguration,
            "active proof was not retired before replacement",
        ),
        CheckpointError::Unknown => {
            Error::new(ErrorKind::InvalidRequest, "checkpoint is not active")
        }
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

pub(crate) fn pet_release_error(error: rocq_engine::pet::PetError) -> Error {
    // Design note: only transport loss invalidates every state ID in the
    // project epoch. A live PET rejecting a release is an owner/protocol
    // configuration failure; reporting it as transport loss would make the
    // dispatcher erase replayable checkpoints while retaining the child.
    let kind = if error.is_transport_loss() {
        ErrorKind::PetLost
    } else if error.is_internal_failure() {
        ErrorKind::PetFailure
    } else {
        ErrorKind::InvalidConfiguration
    };
    Error::new(kind, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_query_pages_are_bounded_and_resume_on_utf8_boundaries() {
        let text = format!("{}🦀tail", "a".repeat(QUERY_PAGE_BYTES - 1));
        let first = text_result(text.clone(), 0).unwrap();
        assert_eq!(first["text"].as_str().unwrap().len(), QUERY_PAGE_BYTES - 1);
        assert_eq!(first["next_offset"], QUERY_PAGE_BYTES - 1);
        assert!(first.get("offset").is_none());
        assert!(first.get("total_bytes").is_none());

        let offset = first["next_offset"].as_u64().unwrap() as usize;
        let second = text_result(text.clone(), offset).unwrap();
        assert_eq!(second["text"], "🦀tail");
        assert!(second.get("next_offset").is_none());
        assert!(second.get("offset").is_none());
        assert!(second.get("total_bytes").is_none());

        let middle_of_crab = offset + 1;
        assert_eq!(
            text_result(text, middle_of_crab).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }

    #[test]
    fn small_text_query_preserves_the_original_wire_shape() {
        assert_eq!(text_result("ok".into(), 0).unwrap(), json!({"text":"ok"}));
        assert_eq!(
            text_result("ok".into(), 3).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        for value in [json!(-1), json!(1.5), json!("1")] {
            assert_eq!(
                query_offset(&json!({"offset":value})).unwrap_err().kind,
                ErrorKind::InvalidRequest
            );
        }
    }
}
