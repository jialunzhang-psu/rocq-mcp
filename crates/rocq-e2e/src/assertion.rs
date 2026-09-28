use crate::{Result, TraceError};
use serde_json::Value;

/// Compare the complete public JSON value. Trace expectations are replayable
/// contracts, not partial snapshots, so missing or additional fields fail.
pub(crate) fn assert_output(line: usize, expected: &Value, actual: &Value) -> Result<()> {
    if output_matches(expected, actual) {
        return Ok(());
    }
    Err(TraceError::OutputMismatch {
        line,
        expected: expected.clone(),
        actual: actual.clone(),
    })
}

/// Match a complete transport value while allowing only the explicitly
/// unstable positive checkpoint integer.
fn output_matches(expected: &Value, actual: &Value) -> bool {
    if expected.as_object().is_some_and(|object| object.len() == 1)
        && expected.get("$checkpoint") == Some(&Value::Bool(true))
    {
        return actual.as_u64().is_some_and(|checkpoint| checkpoint > 0);
    }
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            expected.len() == actual.len()
                && expected.iter().all(|(key, value)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| output_matches(value, actual))
                })
        }
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| output_matches(expected, actual))
        }
        _ => expected == actual,
    }
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
    fn checkpoint_marker_accepts_only_positive_integers() {
        let marker = json!({"$checkpoint": true});
        assert_output(1, &marker, &json!(1)).unwrap();
        assert_output(1, &marker, &json!(0)).unwrap_err();
        assert_output(1, &marker, &json!("1")).unwrap_err();
    }
}
