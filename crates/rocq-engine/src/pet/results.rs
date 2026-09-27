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
            if pair.len() != 2 || pair[0].as_i64().is_none() {
                return Err(PetError::Protocol("PET feedback level is invalid".into()));
            }
            pair[1]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| PetError::Protocol("PET feedback message is invalid".into()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.join("\n"))
}

pub(super) fn parse_goals(value: &Value, proof_mode: bool) -> Result<PetGoals, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET goals result is not an object".into()))?;
    Ok(PetGoals {
        focused: parse_goal_collection(object.get("goals"))?,
        unfocused: parse_goal_collection(object.get("stack"))?,
        shelved: parse_goal_collection(object.get("shelf"))?,
        given_up: parse_goal_collection(object.get("given_up"))?,
        proof_mode,
    })
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
    let mut output = Vec::new();
    // PET represents unfocused goals as arbitrarily nested proof-stack
    // groups. Flatten only arrays; every leaf must still satisfy the full
    // structured goal schema (never silently discard malformed data).
    fn append(value: &Value, output: &mut Vec<PetGoal>) -> Result<(), PetError> {
        if let Some(values) = value.as_array() {
            for value in values {
                append(value, output)?;
            }
            return Ok(());
        }
        output.push(parse_goal(value)?);
        Ok(())
    }
    for value in values {
        append(value, &mut output)?;
    }
    Ok(output)
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
