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

/// Read an optional non-empty string without treating a wrong JSON type as
/// absence.
pub(crate) fn optional_string(value: &Value, field: &str) -> Result<Option<String>, Error> {
    match value.get(field) {
        None => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(Error::new(
            ErrorKind::InvalidRequest,
            format!("{field} must be a non-empty string"),
        )),
    }
}

/// Read a bounded optional unsigned integer. The explicit public bounds are
/// enforced here rather than relying on clients to honor the advertised schema.
pub(crate) fn optional_usize(
    value: &Value,
    field: &str,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize, Error> {
    let Some(raw) = value.get(field) else {
        return Ok(default);
    };
    let number = raw.as_u64().and_then(|number| usize::try_from(number).ok());
    match number.filter(|number| (minimum..=maximum).contains(number)) {
        Some(number) => Ok(number),
        None => Err(Error::new(
            ErrorKind::InvalidRequest,
            format!("{field} must be an integer from {minimum} to {maximum}"),
        )),
    }
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
