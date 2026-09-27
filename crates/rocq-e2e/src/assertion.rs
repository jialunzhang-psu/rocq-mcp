use crate::{Result, TraceError};
use serde_json::Value;

/// Compare the complete public JSON value. Trace expectations are replayable
/// contracts, not partial snapshots, so missing or additional fields fail.
pub(crate) fn assert_output(line: usize, expected: &Value, actual: &Value) -> Result<()> {
    if output_matches(line, expected, actual) {
        return Ok(());
    }
    Err(TraceError::OutputMismatch {
        line,
        expected: expected.clone(),
        actual: actual.clone(),
    })
}

/// Match a complete transport value while allowing explicitly marked PET
/// pretty-printer leaves. Parent objects and arrays still require the exact
/// key set, order, and shape, so a marker nested under `error` cannot hide a
/// missing `state` or an unexpected response field.
fn output_matches(line: usize, expected: &Value, actual: &Value) -> bool {
    if expected.as_object().is_some_and(|object| object.len() == 1) {
        if let Some(about) = expected.get("$pet_about") {
            return assert_pet_about(line, about, actual).is_ok();
        }
        if let Some(print) = expected.get("$pet_print") {
            return assert_pet_print(line, print, actual).is_ok();
        }
        if let Some(search) = expected.get("$pet_search") {
            return assert_pet_search(line, search, actual).is_ok();
        }
        if expected.get("$checkpoint") == Some(&Value::Bool(true)) {
            return actual.as_u64().is_some_and(|checkpoint| checkpoint > 0);
        }
        if let Some(prefix) = expected.get("$pet_error_prefix").and_then(Value::as_str) {
            return actual.get("kind").and_then(Value::as_str) == Some("proof_step_failed")
                && actual
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.starts_with(prefix));
        }
    }
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            expected.len() == actual.len()
                && expected.iter().all(|(key, value)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| output_matches(line, value, actual))
                })
        }
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| output_matches(line, expected, actual))
        }
        _ => expected == actual,
    }
}

/// Validate PET's `Print` projection while allowing its pretty-printer's
/// whitespace to vary. The proof term and resulting type remain exact.
fn assert_pet_print(line: usize, expected: &Value, actual: &Value) -> Result<()> {
    let Some(expected) = expected.as_object() else {
        return Err(TraceError::OutputMismatch {
            line,
            expected: expected.clone(),
            actual: actual.clone(),
        });
    };
    let actual_text =
        actual
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceError::OutputMismatch {
                line,
                expected: Value::from(expected.clone()),
                actual: actual.clone(),
            })?;
    let name = expected.get("name").and_then(Value::as_str).unwrap_or("");
    let term = expected.get("term").and_then(Value::as_str).unwrap_or("");
    let ty = expected.get("type").and_then(Value::as_str).unwrap_or("");
    let first = format!("{name} = {term}");
    let stable = actual_text
        .lines()
        .next()
        .is_some_and(|line| line.trim() == first)
        && actual_text
            .lines()
            .any(|line| line.trim() == format!(": {ty}"));
    if stable {
        return Ok(());
    }
    Err(TraceError::OutputMismatch {
        line,
        expected: Value::from(expected.clone()),
        actual: actual.clone(),
    })
}

/// Validate a Search response through stable PET result anchors without
/// freezing the complete theorem inventory or pretty-printer whitespace.
/// The marker is deliberately explicit and still requires the exact
/// `{"text": ...}` response shape.
fn assert_pet_search(line: usize, expected: &Value, actual: &Value) -> Result<()> {
    let Some(expected) = expected.as_object() else {
        return Err(TraceError::OutputMismatch {
            line,
            expected: expected.clone(),
            actual: actual.clone(),
        });
    };
    let Some(needles) = expected.get("contains").and_then(Value::as_array) else {
        return Err(TraceError::OutputMismatch {
            line,
            expected: Value::Object(expected.clone()),
            actual: actual.clone(),
        });
    };
    let Some(text) = actual.get("text").and_then(Value::as_str) else {
        return Err(TraceError::OutputMismatch {
            line,
            expected: Value::Object(expected.clone()),
            actual: actual.clone(),
        });
    };
    let stable = !text.is_empty()
        && needles.iter().all(|needle| {
            needle
                .as_str()
                .is_some_and(|needle| !needle.is_empty() && text.contains(needle))
        });
    if stable {
        return Ok(());
    }
    Err(TraceError::OutputMismatch {
        line,
        expected: Value::Object(expected.clone()),
        actual: actual.clone(),
    })
}

/// Validate PET's `About` response without freezing its disposable workspace
/// path. The marker is deliberately explicit: it is not a general prefix
/// comparison and still checks the stable declaration and location structure.
fn assert_pet_about(line: usize, expected: &Value, actual: &Value) -> Result<()> {
    let Some(expected) = expected.as_object() else {
        return Err(TraceError::OutputMismatch {
            line,
            expected: expected.clone(),
            actual: actual.clone(),
        });
    };
    let actual_text =
        actual
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| TraceError::OutputMismatch {
                line,
                expected: Value::from(expected.clone()),
                actual: actual.clone(),
            })?;
    let name = expected.get("name").and_then(Value::as_str).unwrap_or("");
    let statement = expected
        .get("statement")
        .and_then(Value::as_str)
        .unwrap_or("");
    let constant = expected
        .get("constant")
        .and_then(Value::as_str)
        .unwrap_or("");
    let header = format!("{name} : {statement}");
    let has_location =
        actual_text.contains("\nDeclared in\nFile ") || actual_text.contains("\nDeclared in File ");
    let stable = actual_text.starts_with(&format!("{header}\n\n"))
        && actual_text.contains(&format!("\nExpands to: Constant {constant}"))
        && has_location
        && actual_text.lines().any(|line| {
            (line.starts_with("File ") || line.starts_with("Declared in File "))
                && line.contains(", line ")
                && line.contains(", characters ")
        });
    if stable {
        return Ok(());
    }
    Err(TraceError::OutputMismatch {
        line,
        expected: Value::from(expected.clone()),
        actual: actual.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comparison_is_exact_and_source_located() {
        assert_output(4, &json!({"x": 1}), &json!({"x": 1})).unwrap();
        let error = assert_output(7, &json!({"x": 1}), &json!({"x": 1, "y": 2})).unwrap_err();
        assert!(error.to_string().contains("trace line 7"));
    }

    #[test]
    fn pet_about_marker_ignores_only_disposable_path() {
        let expected = json!({
            "$pet_about": {
                "name": "done",
                "statement": "True",
                "constant": "Main.done"
            }
        });
        let actual = json!({"text": "done : True\n\ndone is opaque\nExpands to: Constant Main.done\nDeclared in\nFile \"/tmp/random/Main.v\", line 4, characters 8-12"});
        assert_output(3, &expected, &actual).unwrap();
    }

    #[test]
    fn pet_print_marker_keeps_term_and_type() {
        let expected = json!({
            "$pet_print": {"name": "done", "term": "I", "type": "True"}
        });
        let actual = json!({"text": "done = I\n     : True"});
        assert_output(3, &expected, &actual).unwrap();
    }

    #[test]
    fn pet_search_marker_checks_stable_result_anchors() {
        let expected = json!({"$pet_search": {"contains": ["eq_refl:"]}});
        let actual = json!({"text": "eq_refl: forall {A : Type}, x = x"});
        assert_output(3, &expected, &actual).unwrap();
        assert_output(3, &expected, &json!({"text": "other"})).unwrap_err();
    }

    #[test]
    fn pet_error_marker_can_be_nested_without_weakening_parent_shape() {
        let expected = json!({
            "state": {"status": "Open"},
            "error": {"$pet_error_prefix": "PET rejected request"}
        });
        let actual = json!({
            "state": {"status": "Open"},
            "error": {
                "kind": "proof_step_failed",
                "message": "PET rejected request (-32003): native wording"
            }
        });
        assert_output(5, &expected, &actual).unwrap();
        assert_output(5, &expected, &json!({"error": actual["error"].clone()})).unwrap_err();
    }

    #[test]
    fn checkpoint_marker_accepts_only_positive_integers() {
        let marker = json!({"$checkpoint": true});
        assert_output(1, &marker, &json!(1)).unwrap();
        assert_output(1, &marker, &json!(0)).unwrap_err();
        assert_output(1, &marker, &json!("1")).unwrap_err();
    }
}
