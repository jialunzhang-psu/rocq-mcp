//! The ten public MCP operations mapped directly to Dune, PET, and writeback.

use crate::{
    checkpoint::{CheckpointError, CheckpointId},
    server::{ProjectRuntime, Selection, ServerRuntime, SessionCell},
    validation::{reject_unknown, required_string},
};
use rocq_engine::{
    DeclarationIdentity, DeclarationInfo, DeclarationKind, Engine, Error, ErrorKind, FileId,
    GoalScope, OpenResult, OpenedProof, PetQuery, PetStateId, ProofState, pet, validate_fragments,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};

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
        ErrorKind::ProofStepTimeout => "proof_step_timeout",
        ErrorKind::RequestCancelled => "request_cancelled",
        ErrorKind::PetLost => "pet_lost",
        ErrorKind::QueryFailed => "query_failed",
        ErrorKind::PetFailure => "pet_failure",
        ErrorKind::ProjectTimeout => "project_timeout",
        ErrorKind::BuildTimeout => "build_timeout",
        ErrorKind::AxiomDependencyOutOfScope => "axiom_dependency_out_of_scope",
        ErrorKind::UnfinishedDependency => "unfinished_dependency",
    };
    let mut value = json!({"kind": kind, "message": public_message(error)});
    if let Some(diagnostic) = &error.diagnostic {
        value["diagnostic"] = json!({
            "byte_range": {
                "start": diagnostic.byte_start,
                "end": diagnostic.byte_end,
            }
        });
    }
    value
}

/// Add a concrete recovery step for infrastructure and lifecycle failures.
///
/// Rocq diagnostics are deliberately left untouched: changing their wording
/// makes a tactic or query failure harder to repair.  The engine marks those
/// errors explicitly; this function never guesses from their text. The
/// remaining classes are wrapper-owned failures, so the response tells the
/// caller what to do next instead of exposing protocol vocabulary or numeric
/// codes.
fn public_message(error: &Error) -> String {
    // The engine marks semantic PET rejections explicitly. Do not infer
    // semantics from wording or from the presence of a source range: a
    // malformed infrastructure response may also carry metadata.
    if error.semantic {
        return error.message.clone();
    }
    let action = match error.kind {
        ErrorKind::InvalidConfiguration => {
            Some("check the project configuration or prover capabilities, then restart the server")
        }
        ErrorKind::NotFound => {
            Some("call list_files/list_decls or query(goals) to obtain a current identity or state")
        }
        ErrorKind::Ambiguous => Some("use the exact declaration identity returned by list_decls"),
        ErrorKind::DeclarationChanged => {
            Some("refresh the declaration with prove and retry the proof")
        }
        ErrorKind::ProofStepFailed => {
            Some("inspect query(goals) and submit a smaller valid fragment")
        }
        ErrorKind::ProofStepTimeout => {
            Some("retry with a larger timeout_ms or split the fragment into smaller steps")
        }
        ErrorKind::RequestCancelled => Some("retry the cancelled operation when ready"),
        ErrorKind::PetLost => {
            Some("retry the operation; any retained proof state will be replayed automatically")
        }
        ErrorKind::PetFailure => {
            Some("restart the server; if this repeats, rebuild its bundled prover")
        }
        ErrorKind::ProjectTimeout => Some("retry or increase ROCQ_COMMAND_TIMEOUT_SECS"),
        ErrorKind::BuildTimeout => {
            Some("retry with a larger ROCQ_COMMAND_TIMEOUT_SECS value or unset the limit")
        }
        ErrorKind::AxiomDependencyOutOfScope => {
            Some("inspect query(kind=assumptions) and replace the disallowed dependency")
        }
        ErrorKind::UnfinishedDependency => {
            Some("prove or replace the unfinished dependency, then retry")
        }
        ErrorKind::InvalidRequest => {
            Some("perform the stated prerequisite and correct the indicated argument, then retry")
        }
        ErrorKind::InvalidDeclaration => {
            Some("inspect the declaration/source diagnostic, correct the source, and retry")
        }
        ErrorKind::QueryFailed => {
            Some("retry with a valid query kind and target in the current Rocq context")
        }
    };
    match action {
        Some(action) => {
            let detail = error.message.trim().trim_end_matches('.');
            let detail = if detail.is_empty() {
                "operation failed"
            } else {
                detail
            };
            format!("{detail}. Next step: {action}.")
        }
        None => error.message.clone(),
    }
}

fn state_json(
    state: &ProofState,
    checkpoint: Option<CheckpointId>,
    retained_next_offset: Option<usize>,
) -> Value {
    let mut value = json!({
        "target": declaration_identity_json(&state.theorem.identity),
        "status": format!("{:?}", state.lifecycle),
    });
    // Design note: `Open` is a PET lifecycle fact, not a synonym for
    // “focused goals rendered to a non-empty string”.  Rocq can legally have
    // an open proof with an empty focused list and work parked on the shelf or
    // proof stack (`shelve.`, bullets, and focus commands).  Keep the field
    // stable, even when its value is the empty rendering, and expose the
    // structured counts/focus data PET supplied instead of making clients
    // probe with `idtac.`.
    if matches!(state.lifecycle, rocq_engine::ProofLifecycle::Open) {
        let page = match retained_next_offset {
            Some(next_offset) => json!({
                "text": &state.goals,
                "next_offset": next_offset,
            }),
            None => text_result(&state.goals, 0).expect("zero is a valid text offset"),
        };
        value["goals"] = page
            .get("text")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
        if let Some(next_offset) = page.get("next_offset") {
            // State responses are also used by check/try, where the caller
            // cannot supply an offset in the same request. A selected state
            // can resume through query(goals); a hypothetical `try` state is
            // deliberately released and therefore reports truncation without
            // inventing a durable server-side cursor.
            match checkpoint {
                Some(_) => value["goals_next_offset"] = next_offset.clone(),
                None => value["goals_truncated"] = json!(true),
            }
        }
        value["goal_counts"] = json!({
            "focused": state.goal_focus.focused.len(),
            "unfocused": state.goal_focus.unfocused_count(),
            "shelved": state.goal_focus.shelved.len(),
            "given_up": state.goal_focus.given_up.len(),
            "total": state.goal_focus.total_count(),
        });
        value["focus"] = focus_json(state, checkpoint.is_some());
    }
    if let Some(checkpoint) = checkpoint {
        value["checkpoint"] = json!(checkpoint.get());
    }
    value
}

fn focus_json(state: &ProofState, include_goal_ids: bool) -> Value {
    let focus = &state.goal_focus;
    let ids = |goals: &[Vec<Value>]| {
        goals
            .iter()
            .map(|goal| Value::Array(goal.clone()))
            .collect::<Vec<_>>()
    };
    let mut value = json!({
        "depth": focus.focus_depth(),
    });
    // Goal IDs can only be dereferenced while the exact selected checkpoint
    // remains addressable. `try` releases hypothetical PET states before it
    // returns, so exposing their IDs would invite guaranteed `not_found`
    // queries and needlessly inflate large alternative responses.
    if include_goal_ids {
        value["focused_goal_ids"] = json!(ids(&focus.focused));
        value["stack"] = json!(
            focus
                .stack
                .iter()
                .map(|frame| json!({
                    "left_goal_ids": ids(&frame.left),
                    "right_goal_ids": ids(&frame.right),
                }))
                .collect::<Vec<_>>()
        );
        value["shelved_goal_ids"] = json!(ids(&focus.shelved));
        value["given_up_goal_ids"] = json!(ids(&focus.given_up));
    }
    if let Some(next_bullet) = &focus.next_bullet {
        value["next_bullet"] = json!(next_bullet);
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

fn attempt_timeout(args: &Value) -> Result<Option<Duration>, Error> {
    args.get("timeout_ms")
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value > 0)
                .map(Duration::from_millis)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidRequest,
                        "timeout_ms must be a positive integer",
                    )
                })
        })
        .transpose()
}

fn reject_cancelled() -> Result<(), Error> {
    if pet::request_cancelled() {
        Err(Error::new(ErrorKind::RequestCancelled, "request cancelled"))
    } else {
        Ok(())
    }
}

/// Establish the request's linearization point immediately before a
/// wrapper-owned mutation. Cancellation that won first aborts the operation;
/// cancellation arriving later cannot interrupt the committed transaction.
fn commit_or_cancelled() -> Result<(), Error> {
    if pet::commit_request() {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::RequestCancelled, "request cancelled"))
    }
}

fn invalidates_pet_epoch(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::PetLost | ErrorKind::ProofStepTimeout | ErrorKind::RequestCancelled
    )
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
    reject_cancelled()?;
    if name == "start" {
        return start(runtime, session, &args);
    }
    let project = session
        .project()
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "call start first"))?;
    let _operation = project.lock();
    reject_cancelled()?;
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
    reject_cancelled()?;
    let result = dispatch_attached(runtime, session, &project, &mut selection, name, &args);
    if result
        .as_ref()
        .is_err_and(|error| invalidates_pet_epoch(error.kind))
    {
        if result
            .as_ref()
            .is_err_and(|error| error.kind == ErrorKind::RequestCancelled)
        {
            // A late cancellation can arrive after PET produced a response.
            // Force the epoch boundary even in that race so no unowned state
            // created by the cancelled request can survive.
            project.actor().restart();
        }
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
            reject_cancelled()?;
            commit_or_cancelled()?;
            let mut selection = session
                .selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let pet_epoch_lost = retire_proof(&previous, &mut selection)?;
            if pet_epoch_lost {
                runtime.invalidate_project_states(&previous, session, &mut selection);
            }
            install_start_view(runtime, session, &next, &mut selection)?;
            return Ok(json!({}));
        }

        // Never hold two project operation barriers at once: sessions may
        // switch projects in opposite directions. Retire the previous proof,
        // detach, then admit the independently serialized target runtime.
        let _operation = previous.lock();
        reject_cancelled()?;
        commit_or_cancelled()?;
        let mut selection = session
            .selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pet_epoch_lost = retire_proof(&previous, &mut selection)?;
        if pet_epoch_lost {
            runtime.invalidate_project_states(&previous, session, &mut selection);
        }
        selection.project = None;
    }

    let _operation = next.lock();
    reject_cancelled()?;
    commit_or_cancelled()?;
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
                    Ok(state_json(&state, None, None))
                }
                OpenResult::Open(opened) => begin_proof(project, selection, *opened),
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
            commit_or_cancelled()?;
            if retire_proof(project, selection)? {
                runtime.invalidate_project_states(project, session, selection);
            }
            Ok(json!({}))
        }
        "check" => check(runtime, session, project, selection, args),
        "try" => try_fragments(runtime, session, project, selection, args),
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
    let (view, goals_next_offset) = bounded_checkpoint_view(opened.view);
    commit_or_cancelled()?;
    let checkpoint = match selection.checkpoints.begin(
        opened.target,
        opened.state,
        opened.finished,
        view,
        goals_next_offset,
    ) {
        Ok(checkpoint) => checkpoint,
        Err(error) => {
            project
                .actor()
                .release_states(&[state])
                .map_err(pet_release_error)?;
            return Err(checkpoint_error(error));
        }
    };
    let checkpoint_view = selection
        .checkpoints
        .lookup(checkpoint)
        .map_err(checkpoint_error)?;
    Ok(state_json(
        &checkpoint_view.view,
        Some(checkpoint),
        checkpoint_view.goals_next_offset,
    ))
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
    reject_unknown(args, &["attempts", "timeout_ms"])?;
    let fragments = attempts(args)?;
    let timeout = attempt_timeout(args)?;
    let (base_id, _) = ensure_current_state(runtime.engine.as_ref(), project, selection)?;
    let target = selection
        .checkpoints
        .proof
        .as_ref()
        .expect("current state has a proof")
        .target
        .clone();
    let mut rejected = Vec::new();
    for (index, fragment) in fragments.iter().enumerate() {
        // A timed-out earlier alternative destroyed the PET epoch. Reacquire
        // the same selected checkpoint lazily before evaluating the next one.
        let (_, base_state) = ensure_current_state(runtime.engine.as_ref(), project, selection)?;
        match runtime.engine.run(
            project.project(),
            project.actor(),
            &target,
            base_state,
            fragment,
            timeout,
        ) {
            Ok(mut step) => {
                let (view, goals_next_offset) = bounded_checkpoint_view(step.view);
                step.view = view;
                // Validate the native terminator before committing a finished
                // fragment. Cancellation can therefore still win without
                // advancing the checkpoint; after commit, publication is an
                // indivisible source transaction and must reach rollback or
                // success even if a late notification arrives.
                let close_error = if step.finished {
                    runtime
                        .engine
                        .close_proof(project.project(), project.actor(), &target, step.state)
                        .err()
                } else {
                    None
                };
                if close_error
                    .as_ref()
                    .is_some_and(|error| error.kind == ErrorKind::RequestCancelled)
                {
                    return Err(close_error.unwrap());
                }
                commit_or_cancelled()?;
                let checkpoint = match selection.checkpoints.commit(
                    step.state,
                    fragment.clone(),
                    step.finished,
                    step.view.clone(),
                    goals_next_offset,
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
                if let Some(error) = close_error {
                    if invalidates_pet_epoch(error.kind) {
                        runtime.invalidate_project_states(project, session, selection);
                    }
                    return Ok(selected_error(
                        index,
                        &step.view,
                        Some(checkpoint),
                        goals_next_offset,
                        rejected,
                        &error,
                    ));
                }
                if !step.finished {
                    return Ok(selected_result(
                        index,
                        &step.view,
                        Some(checkpoint),
                        goals_next_offset,
                        rejected,
                    ));
                }
                let path = selection
                    .checkpoints
                    .fragments(checkpoint)
                    .map_err(checkpoint_error)?;
                match runtime.engine.publish(
                    project.project(),
                    project.actor(),
                    &target,
                    &path,
                    || runtime.invalidate_project_states(project, session, selection),
                ) {
                    Ok(state) => {
                        selection.checkpoints.clear();
                        return Ok(selected_result(index, &state, None, None, rejected));
                    }
                    Err(error) => {
                        let checkpoint = if error.kind == ErrorKind::DeclarationChanged {
                            selection.checkpoints.clear();
                            None
                        } else {
                            Some(checkpoint)
                        };
                        return Ok(selected_error(
                            index,
                            &step.view,
                            checkpoint,
                            goals_next_offset,
                            rejected,
                            &error,
                        ));
                    }
                }
            }
            Err(error) if error.kind == ErrorKind::ProofStepFailed => {
                rejected.push(public_error(&error));
            }
            Err(error) if error.kind == ErrorKind::ProofStepTimeout => {
                rejected.push(public_error(&error));
                runtime.invalidate_project_states(project, session, selection);
            }
            Err(error) => return Err(error),
        }
    }
    let checkpoint = selection
        .checkpoints
        .lookup(base_id)
        .map_err(checkpoint_error)?;
    Ok(json!({
        "state": state_json(
            &checkpoint.view,
            Some(base_id),
            checkpoint.goals_next_offset,
        ),
        "rejected": rejected,
    }))
}

fn selected_result(
    selected: usize,
    state: &ProofState,
    checkpoint: Option<CheckpointId>,
    goals_next_offset: Option<usize>,
    rejected: Vec<Value>,
) -> Value {
    let mut result = json!({
        "selected": selected,
        "state": state_json(state, checkpoint, goals_next_offset),
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
    goals_next_offset: Option<usize>,
    rejected: Vec<Value>,
    error: &Error,
) -> Value {
    let mut result = json!({
        "selected": selected,
        "state": state_json(state, checkpoint, goals_next_offset),
        "error": public_error(error),
    });
    if !rejected.is_empty() {
        result["rejected"] = json!(rejected);
    }
    result
}

fn try_fragments(
    runtime: &Arc<ServerRuntime>,
    session: &Arc<SessionCell>,
    project: &Arc<ProjectRuntime>,
    selection: &mut Selection,
    args: &Value,
) -> Result<Value, Error> {
    reject_unknown(args, &["attempts", "timeout_ms"])?;
    let fragments = attempts(args)?;
    let timeout = attempt_timeout(args)?;
    let _ = ensure_current_state(runtime.engine.as_ref(), project, selection)?;
    let target = selection
        .checkpoints
        .proof
        .as_ref()
        .expect("current state has a proof")
        .target
        .clone();
    let mut output = Vec::new();
    for fragment in fragments {
        let (_, base_state) = ensure_current_state(runtime.engine.as_ref(), project, selection)?;
        match runtime.engine.run(
            project.project(),
            project.actor(),
            &target,
            base_state,
            &fragment,
            timeout,
        ) {
            Ok(step) => {
                reject_cancelled()?;
                let state = state_json(&step.view, None, None);
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
            Err(error) if error.kind == ErrorKind::ProofStepTimeout => {
                output.push(json!({
                    "solved": false,
                    "error": public_error(&error),
                }));
                runtime.invalidate_project_states(project, session, selection);
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
    commit_or_cancelled()?;
    selection
        .checkpoints
        .select(target)
        .map_err(checkpoint_error)?;
    let checkpoint = selection
        .checkpoints
        .lookup(target)
        .map_err(checkpoint_error)?;
    Ok(state_json(
        &checkpoint.view,
        Some(target),
        checkpoint.goals_next_offset,
    ))
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
            reject_unknown(args, &["kind", "scope", "goal_id", "offset"])?;
            let offset = query_offset(args)?;
            let goal_id = args.get("goal_id").map(|value| {
                value.as_array().ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidRequest,
                        "goal_id must be a non-empty evar array returned by query(goals)",
                    )
                })
            });
            let goal_id = match goal_id {
                None => None,
                Some(result) => {
                    let id = result?;
                    if id.is_empty() {
                        return Err(Error::new(
                            ErrorKind::InvalidRequest,
                            "goal_id must be a non-empty evar array returned by query(goals)",
                        ));
                    }
                    Some(id.as_slice())
                }
            };
            if goal_id.is_some() && args.get("scope").is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    "scope and goal_id are mutually exclusive",
                ));
            }
            // A PET evar id already identifies one goal across every current
            // collection. Scope is only a collection selector and must not be
            // repeated merely to retrieve a shelved or unfocused goal.
            let scope = if goal_id.is_some() {
                GoalScope::All
            } else {
                goal_scope(args)?
            };
            let (checkpoint, state) = ensure_current_state(engine, project, selection)?;
            let proof = selection.checkpoints.proof.as_ref().unwrap();
            let view = engine.goals(
                project.project(),
                project.actor(),
                &proof.target,
                state,
                scope,
                goal_id,
            )?;
            let page = text_result(&view.goals, offset)?;
            let mut result = state_json(&view, Some(checkpoint), None);
            result
                .as_object_mut()
                .expect("state is an object")
                .remove("goals_next_offset");
            result["goals"] = page
                .get("text")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new()));
            if let Some(next_offset) = page.get("next_offset") {
                result["next_offset"] = next_offset.clone();
            }
            Ok(result)
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
            // Design note: an exact match to the selected proof target may
            // use its retained state (which is the only context that can
            // represent an unpublished `declare`). Any other target must use
            // its own document position; an unrelated active proof must never
            // shadow that file. PET can keep both states in one process, and
            // `query_at` releases only its temporary state after materializing
            // the result; checkpoint-owned proof states remain untouched.
            if selection
                .checkpoints
                .proof
                .as_ref()
                .is_some_and(|proof| proof.target.identity() == &identity)
            {
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

fn goal_scope(args: &Value) -> Result<GoalScope, Error> {
    let scope = match args.get("scope") {
        None => return Ok(GoalScope::Focused),
        Some(Value::String(scope)) => scope.as_str(),
        Some(_) => {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "scope must be focused, unfocused, shelved, given_up, or all",
            ));
        }
    };
    match scope {
        "focused" => Ok(GoalScope::Focused),
        "unfocused" => Ok(GoalScope::Unfocused),
        "shelved" => Ok(GoalScope::Shelved),
        "given_up" => Ok(GoalScope::GivenUp),
        "all" => Ok(GoalScope::All),
        _ => Err(Error::new(
            ErrorKind::InvalidRequest,
            "scope must be focused, unfocused, shelved, given_up, or all",
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
fn text_result(text: impl AsRef<str>, offset: usize) -> Result<Value, Error> {
    let text = text.as_ref();
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

/// Bound the only pretty-printed goal text retained by a checkpoint.
///
/// PET remains the semantic owner and can regenerate the complete rendering
/// from the checkpoint's state handle. The returned continuation is a UTF-8
/// byte boundary accepted by `query(goals)`; no full contexts are copied into
/// every historical checkpoint.
fn bounded_checkpoint_view(mut state: ProofState) -> (ProofState, Option<usize>) {
    if state.goals.len() <= QUERY_PAGE_BYTES {
        return (state, None);
    }
    let mut end = QUERY_PAGE_BYTES;
    while end > 0 && !state.goals.is_char_boundary(end) {
        end -= 1;
    }
    state.goals.truncate(end);
    (state, Some(end))
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
    let mut staged = Vec::<(CheckpointId, PetStateId, ProofState, Option<usize>)>::new();
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
                "proof completion changed while restoring the initial checkpoint",
            ));
        }
        state = opened.state;
        let (view, goals_next_offset) = bounded_checkpoint_view(opened.view);
        staged.push((records[0].0, opened.state, view, goals_next_offset));
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
            None,
        ) {
            Ok(step) => step,
            Err(error) => {
                // Process-invalidating errors already discarded every staged
                // state. Preserve their primary classification rather than
                // replacing cancellation/transport loss with a failed release.
                if !invalidates_pet_epoch(error.kind) {
                    release_staged(project, &staged)?;
                }
                return Err(error);
            }
        };
        if step.finished != checkpoint.finished {
            let mut states = staged
                .iter()
                .map(|(_, state, _, _)| *state)
                .collect::<Vec<_>>();
            states.push(step.state);
            project
                .actor()
                .release_states(&states)
                .map_err(pet_release_error)?;
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "proof completion changed while restoring the checkpoint",
            ));
        }
        state = step.state;
        let (view, goals_next_offset) = bounded_checkpoint_view(step.view);
        staged.push((*checkpoint_id, step.state, view, goals_next_offset));
    }
    let mut replaced = Vec::new();
    if let Some(proof) = &mut selection.checkpoints.proof {
        for (checkpoint, state, view, goals_next_offset) in staged {
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
            node.goals_next_offset = goals_next_offset;
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
    staged: &[(CheckpointId, PetStateId, ProofState, Option<usize>)],
) -> Result<(), Error> {
    let states = staged
        .iter()
        .map(|(_, state, _, _)| *state)
        .collect::<Vec<_>>();
    project
        .actor()
        .release_states(&states)
        .map_err(pet_release_error)
}

/// Retire the selected proof and release its live PET IDs. The boolean result
/// is true only when release discovered that the complete PET epoch was
/// already lost; callers must then invalidate state IDs in sibling sessions.
/// A lost epoch still counts as successful retirement because none of its IDs
/// can remain allocated or be safely retried against a replacement process.
fn retire_proof(project: &ProjectRuntime, selection: &mut Selection) -> Result<bool, Error> {
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
            // state when its process owner was dropped.  Treat retirement as
            // successful in that epoch: retrying those integer IDs against a
            // replacement PET is unsound, and making `start`/`abandon` fail
            // leaves a stale selection that cannot be used or explicitly
            // discarded.  The project-wide invalidation still happens at the
            // caller, which owns the shared session registry.
            if error.is_transport_loss() {
                selection.checkpoints.clear();
                return Ok(true);
            }
            return Err(pet_release_error(error));
        }
        selection.checkpoints.clear();
    }
    Ok(false)
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
    // Design note: unexpected transport loss and intentional cancellation /
    // deadline boundaries have distinct public meanings even though all end
    // the PET epoch. A live semantic release rejection remains an
    // owner/protocol configuration failure.
    let kind = match &error {
        rocq_engine::pet::PetError::Cancelled => ErrorKind::RequestCancelled,
        rocq_engine::pet::PetError::TimedOut { .. } => ErrorKind::ProofStepTimeout,
        error if error.is_transport_loss() => ErrorKind::PetLost,
        error if error.is_internal_failure() => ErrorKind::PetFailure,
        _ => ErrorKind::InvalidConfiguration,
    };
    let projected = Error::new(kind, error.public_message());
    if error.is_semantic() {
        projected.semantic()
    } else {
        projected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_deadline_is_optional_and_strictly_positive() {
        assert_eq!(attempt_timeout(&json!({})).unwrap(), None);
        assert_eq!(
            attempt_timeout(&json!({"timeout_ms": 25})).unwrap(),
            Some(Duration::from_millis(25))
        );
        for value in [json!(0), json!(-1), json!(1.5), json!("25")] {
            let error = attempt_timeout(&json!({"timeout_ms": value})).unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidRequest);
        }
    }

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
        assert_eq!(text_result("ok", 0).unwrap(), json!({"text":"ok"}));
        assert_eq!(
            text_result("ok", 3).unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        for value in [json!(-1), json!(1.5), json!("1")] {
            assert_eq!(
                query_offset(&json!({"offset":value})).unwrap_err().kind,
                ErrorKind::InvalidRequest
            );
        }
    }

    #[test]
    fn goal_scope_defaults_to_focused_and_rejects_invalid_values() {
        assert_eq!(goal_scope(&json!({})).unwrap(), GoalScope::Focused);
        assert_eq!(
            goal_scope(&json!({"scope":"shelved"})).unwrap(),
            GoalScope::Shelved
        );
        for value in [json!(1), json!("unknown")] {
            assert_eq!(
                goal_scope(&json!({"scope":value})).unwrap_err().kind,
                ErrorKind::InvalidRequest
            );
        }
    }

    #[test]
    fn open_state_keeps_empty_focus_and_pet_focus_metadata() {
        let state = ProofState {
            theorem: DeclarationInfo {
                identity: DeclarationIdentity {
                    file: FileId("A.v".into()),
                    qualified_path: vec!["Demo".into(), "t".into()],
                },
                kind: DeclarationKind::Theorem,
                statement: "True".into(),
            },
            lifecycle: rocq_engine::ProofLifecycle::Open,
            goals: String::new(),
            goal_focus: rocq_engine::GoalFocus {
                focused: vec![],
                stack: vec![rocq_engine::GoalStackFrame {
                    left: vec![],
                    right: vec![vec![json!("Ser_Evar"), json!(7)]],
                }],
                shelved: vec![],
                given_up: vec![],
                next_bullet: Some("Focus next goal with bullet -.".into()),
            },
        };
        let value = state_json(&state, Some(CheckpointId::from_u64(3)), None);
        assert_eq!(value["status"], "Open");
        assert_eq!(value["goals"], "");
        assert_eq!(value["goal_counts"]["total"], 1);
        assert_eq!(value["focus"]["depth"], 1);
        assert_eq!(
            value["focus"]["next_bullet"],
            "Focus next goal with bullet -."
        );
        assert_eq!(value["focus"]["stack"][0]["right_goal_ids"][0][1], 7);
    }

    #[test]
    fn retained_and_hypothetical_goal_renderings_are_bounded_without_utf8_damage() {
        let state = ProofState {
            theorem: DeclarationInfo {
                identity: DeclarationIdentity {
                    file: FileId("A.v".into()),
                    qualified_path: vec!["Demo".into(), "t".into()],
                },
                kind: DeclarationKind::Theorem,
                statement: "True".into(),
            },
            lifecycle: rocq_engine::ProofLifecycle::Open,
            goals: format!("{}🦀tail", "a".repeat(QUERY_PAGE_BYTES - 1)),
            goal_focus: rocq_engine::GoalFocus::default(),
        };
        let hypothetical = state_json(&state, None, None);
        assert_eq!(hypothetical["goals_truncated"], true);
        assert!(hypothetical.get("goals_next_offset").is_none());
        assert_eq!(
            hypothetical["goals"].as_str().unwrap().len(),
            QUERY_PAGE_BYTES - 1
        );
        assert_eq!(hypothetical["focus"], json!({"depth":0}));

        let (retained, next_offset) = bounded_checkpoint_view(state);
        assert_eq!(retained.goals.len(), QUERY_PAGE_BYTES - 1);
        assert_eq!(next_offset, Some(QUERY_PAGE_BYTES - 1));
        let selected = state_json(&retained, Some(CheckpointId::from_u64(1)), next_offset);
        assert_eq!(selected["goals_next_offset"], QUERY_PAGE_BYTES - 1);
        assert!(selected.get("goals_truncated").is_none());
        assert!(selected["focus"].get("next_bullet").is_none());
    }

    #[test]
    fn pet_sentence_diagnostic_is_structured_on_the_wire() {
        let error = Error::new(ErrorKind::ProofStepFailed, "bad tactic")
            .semantic()
            .with_diagnostic(rocq_engine::ProofDiagnostic {
                byte_start: 12,
                byte_end: 20,
            });
        assert_eq!(
            public_error(&error),
            json!({
                "kind":"proof_step_failed",
                "message":"bad tactic",
                "diagnostic":{"byte_range":{"start":12,"end":20}}
            })
        );
    }
}
