//! MCP argument-shape validation. Domain validation remains in rocq-engine.
use rocq_engine::{Error, ErrorKind};
use serde_json::Value;

/// Read one required non-empty string argument.
pub(crate) fn required_string(value: &Value, field: &str) -> Result<String, Error> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidRequest,
                format!("{field} must be a non-empty string"),
            )
        })
}

/// Reject fields outside the selected tool/query variant. JSON Schema is a
/// client contract, not a substitute for server-side enforcement.
pub(crate) fn reject_unknown(value: &Value, allowed: &[&str]) -> Result<(), Error> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new(ErrorKind::InvalidRequest, "arguments must be an object"))?;
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            format!("unexpected field '{field}'"),
        ));
    }
    Ok(())
}
