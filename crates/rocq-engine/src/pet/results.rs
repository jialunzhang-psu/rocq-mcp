//! PET response decoding for run, feedback, and structured goal results.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct RunResult {
    pub(super) st: u64,
    pub(super) proof_finished: bool,
}

pub(super) fn parse_run_result(value: &Value) -> Result<RunResult, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET run result is not an object".into()))?;
    let st = object
        .get("st")
        .and_then(Value::as_u64)
        .ok_or_else(|| PetError::Protocol("PET run result has no state id".into()))?;
    let proof_finished = object
        .get("proof_finished")
        .and_then(Value::as_bool)
        .ok_or_else(|| PetError::Protocol("PET run result has no proof status".into()))?;
    Ok(RunResult { st, proof_finished })
}

pub(super) fn parse_feedback(value: &Value) -> Result<String, PetError> {
    // PET serializes Rocq's LSP diagnostic severity as an integer.  Severity
    // 4 is `hint`; Rocq uses it for loader/progress notices such as
    // "Fetching opaque proofs from disk ...", not for the result of the
    // vernacular query.  Keep the filtering typed at the protocol boundary
    // rather than matching a product-specific message string.  Notice,
    // warning, and error feedback remains visible to the caller.
    const HINT_SEVERITY: i64 = 4;
    let feedback = value
        .as_object()
        .and_then(|object| object.get("feedback"))
        .and_then(Value::as_array)
        .ok_or_else(|| PetError::Protocol("PET run result has no feedback".into()))?;
    feedback
        .iter()
        .map(|item| {
            // PET Run_result.feedback is a (level, message) pair. Return the
            // Rocq message, not the JSON serialization of the pair.
            let pair = item
                .as_array()
                .ok_or_else(|| PetError::Protocol("PET feedback item is not a pair".into()))?;
            let [level, message] = pair.as_slice() else {
                return Err(PetError::Protocol("PET feedback item is not a pair".into()));
            };
            let level = level
                .as_i64()
                .ok_or_else(|| PetError::Protocol("PET feedback level is invalid".into()))?;
            let message = message
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| PetError::Protocol("PET feedback message is invalid".into()))?;
            Ok((level, message))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| {
            items
                .into_iter()
                .filter_map(|(level, message)| (level != HINT_SEVERITY).then_some(message))
                .collect::<Vec<_>>()
                .join("\n")
        })
}

pub(super) fn parse_goals(value: &Value, proof_mode: bool) -> Result<PetGoals, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET goals result is not an object".into()))?;
    Ok(PetGoals {
        focused: parse_goal_collection(object.get("goals"))?,
        stack: parse_goal_stack(object.get("stack"))?,
        shelved: parse_goal_collection(object.get("shelf"))?,
        given_up: parse_goal_collection(object.get("given_up"))?,
        bullet: match object.get("bullet") {
            Some(Value::Null) | None => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => return Err(PetError::Protocol("PET goal bullet is invalid".into())),
        },
        proof_mode,
    })
}

/// Preserve PET's proof-stack frame boundaries.  A frame is a pair of goal
/// lists; flattening it loses the distinction Rocq uses for bullet/focus
/// validation and makes a valid next bullet impossible to report.
pub(super) fn parse_goal_stack(
    value: Option<&Value>,
) -> Result<Vec<super::PetGoalStackFrame>, PetError> {
    let Some(value) = value else {
        return Err(PetError::Protocol(
            "PET goals result omits the proof stack".into(),
        ));
    };
    let Some(frames) = value.as_array() else {
        return Err(PetError::Protocol("PET proof stack is not an array".into()));
    };
    frames
        .iter()
        .map(|frame| {
            let pair = frame
                .as_array()
                .ok_or_else(|| PetError::Protocol("PET proof-stack frame is not a pair".into()))?;
            if pair.len() != 2 {
                return Err(PetError::Protocol(
                    "PET proof-stack frame does not contain two goal lists".into(),
                ));
            }
            Ok(super::PetGoalStackFrame {
                left: parse_goal_collection(Some(&pair[0]))?,
                right: parse_goal_collection(Some(&pair[1]))?,
            })
        })
        .collect()
}

pub(super) fn parse_goal_collection(value: Option<&Value>) -> Result<Vec<PetGoal>, PetError> {
    let Some(value) = value else {
        return Err(PetError::Protocol(
            "PET goals result omits a goal collection".into(),
        ));
    };
    let Some(values) = value.as_array() else {
        return Err(PetError::Protocol(
            "PET goal collection is not an array".into(),
        ));
    };
    // PET's typed goal schema is exactly one flat goal list here; proof-stack
    // nesting is owned by `parse_goal_stack`. Accepting and flattening extra
    // arrays would erase protocol structure and hide an incompatible PET.
    values.iter().map(parse_goal).collect()
}

pub(super) fn parse_goal(value: &Value) -> Result<PetGoal, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET goal is not an object".into()))?;
    let info = object
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| PetError::Protocol("PET goal has no info".into()))?;
    let evar = info
        .get("evar")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| PetError::Protocol("PET goal has no evar".into()))?;
    if evar.is_empty() {
        return Err(PetError::Protocol("PET goal has an empty evar id".into()));
    }
    let name = match info.get("name") {
        Some(Value::Null) | None => None,
        Some(Value::String(name)) => Some(name.clone()),
        _ => return Err(PetError::Protocol("PET goal name is invalid".into())),
    };
    let hypotheses = object
        .get("hyps")
        .and_then(Value::as_array)
        .ok_or_else(|| PetError::Protocol("PET goal has no hypotheses".into()))?
        .iter()
        .map(parse_hypothesis)
        .collect::<Result<Vec<_>, _>>()?;
    let ty = object
        .get("ty")
        .and_then(Value::as_str)
        .ok_or_else(|| PetError::Protocol("PET goal has no type".into()))?
        .to_owned();
    Ok(PetGoal {
        evar,
        name,
        hypotheses,
        ty,
    })
}

pub(super) fn parse_hypothesis(value: &Value) -> Result<PetHypothesis, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET hypothesis is not an object".into()))?;
    let names = object
        .get("names")
        .and_then(Value::as_array)
        .ok_or_else(|| PetError::Protocol("PET hypothesis has no names".into()))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| PetError::Protocol("PET hypothesis name is invalid".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let definition = match object.get("def") {
        Some(Value::Null) | None => None,
        Some(Value::String(value)) => Some(value.clone()),
        _ => {
            return Err(PetError::Protocol(
                "PET hypothesis definition is invalid".into(),
            ));
        }
    };
    let ty = object
        .get("ty")
        .and_then(Value::as_str)
        .ok_or_else(|| PetError::Protocol("PET hypothesis has no type".into()))?
        .to_owned();
    Ok(PetHypothesis {
        names,
        definition,
        ty,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(id: u64, ty: &str) -> Value {
        json!({
            "info": {"evar": ["Ser_Evar", id], "name": null},
            "hyps": [],
            "ty": ty,
        })
    }

    #[test]
    fn query_feedback_omits_loader_hints_but_keeps_semantic_output() {
        let value = json!({
            "feedback": [
                [4, "Fetching opaque proofs from disk for Demo.Dependency"],
                [3, "Closed under the global context"],
                [2, "a warning from the queried command"]
            ]
        });
        assert_eq!(
            parse_feedback(&value).unwrap(),
            "Closed under the global context\na warning from the queried command"
        );
    }

    #[test]
    fn goals_parser_preserves_shelf_stack_bullet_and_evar_identity() {
        let value = json!({
            "goals": [],
            "stack": [[[], [goal(2, "right")]]],
            "shelf": [goal(3, "shelved")],
            "given_up": [],
            "bullet": "Focus next goal with bullet -.",
        });
        let goals = parse_goals(&value, true).unwrap();
        assert!(goals.focused.is_empty());
        assert_eq!(goals.stack.len(), 1);
        assert_eq!(goals.stack[0].right[0].ty, "right");
        assert_eq!(goals.shelved[0].evar, vec![json!("Ser_Evar"), json!(3)]);
        assert_eq!(
            goals.bullet.as_deref(),
            Some("Focus next goal with bullet -.")
        );
        let focus = goals.focus();
        assert_eq!(focus.focus_depth(), 1);
        assert_eq!(focus.total_count(), 2);
        assert_eq!(focus.stack[0].right[0], vec![json!("Ser_Evar"), json!(2)]);
    }

    #[test]
    fn goals_parser_fails_closed_on_malformed_focus_metadata() {
        let base = json!({
            "goals": [goal(1, "focused")],
            "stack": [],
            "shelf": [],
            "given_up": [],
            "bullet": null,
        });
        for malformed in [
            json!({"goals": [], "shelf": [], "given_up": [], "bullet": null}),
            json!({"goals": [], "stack": [[[]]], "shelf": [], "given_up": [], "bullet": null}),
            json!({"goals": [], "stack": [], "shelf": [], "given_up": [], "bullet": 1}),
            json!({"goals": [[goal(1, "nested")]], "stack": [], "shelf": [], "given_up": [], "bullet": null}),
        ] {
            assert!(matches!(
                parse_goals(&malformed, true),
                Err(PetError::Protocol(_))
            ));
        }
        let mut empty_evar = base;
        empty_evar["goals"][0]["info"]["evar"] = json!([]);
        assert!(matches!(
            parse_goals(&empty_evar, true),
            Err(PetError::Protocol(_))
        ));
    }
}
