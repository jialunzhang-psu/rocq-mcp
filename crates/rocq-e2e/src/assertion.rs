use crate::{Result, TraceError};
use serde_json::Value;

/// Compare the complete public JSON value. Trace expectations are replayable
/// contracts, not partial snapshots, so missing or additional fields fail.
pub(crate) fn assert_output(line: usize, expected: &Value, actual: &Value) -> Result<()> {
    if expected == actual {
        return Ok(());
    }
    Err(TraceError::OutputMismatch {
        line,
        expected: expected.clone(),
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
}
