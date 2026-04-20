//! NATS subject builders.
//!
//! Per ADR-0004, subjects are constructed from stable IDs only — never from
//! user-facing labels. This module is the single source of truth for subject
//! shape; every publisher and every ACL generator calls through it.

use thiserror::Error;

/// Errors produced when a subject component is invalid.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubjectError {
    #[error("token is empty")]
    Empty,
    #[error("token '{0}' contains forbidden character")]
    BadChar(String),
}

/// Tokens used in subjects must be lowercase alphanumeric, `_`, or `-`.
/// No dots (delimiter), no spaces, no wildcards (`*`, `>`).
fn validate_token(t: &str) -> Result<(), SubjectError> {
    if t.is_empty() {
        return Err(SubjectError::Empty);
    }
    if !t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(SubjectError::BadChar(t.to_owned()));
    }
    Ok(())
}

/// `device.<plugin>.<device>.<entity>.state`
pub fn device_state(plugin: &str, device: &str, entity: &str) -> Result<String, SubjectError> {
    validate_token(plugin)?;
    validate_token(device)?;
    validate_token(entity)?;
    Ok(format!("device.{plugin}.{device}.{entity}.state"))
}

/// `device.<plugin>.<device>.<entity>.event`
pub fn device_event(plugin: &str, device: &str, entity: &str) -> Result<String, SubjectError> {
    validate_token(plugin)?;
    validate_token(device)?;
    validate_token(entity)?;
    Ok(format!("device.{plugin}.{device}.{entity}.event"))
}

/// `device.<plugin>.<device>.avail`
pub fn device_avail(plugin: &str, device: &str) -> Result<String, SubjectError> {
    validate_token(plugin)?;
    validate_token(device)?;
    Ok(format!("device.{plugin}.{device}.avail"))
}

/// `cmd.<plugin>.<device>.<entity>`
pub fn command(plugin: &str, device: &str, entity: &str) -> Result<String, SubjectError> {
    validate_token(plugin)?;
    validate_token(device)?;
    validate_token(entity)?;
    Ok(format!("cmd.{plugin}.{device}.{entity}"))
}

/// `automation.<rule>.fired`
pub fn automation_fired(rule: &str) -> Result<String, SubjectError> {
    validate_token(rule)?;
    Ok(format!("automation.{rule}.fired"))
}

/// `ml.<model>.prediction`
pub fn ml_prediction(model: &str) -> Result<String, SubjectError> {
    validate_token(model)?;
    Ok(format!("ml.{model}.prediction"))
}

/// `audit.<kind>`
pub fn audit(kind: &str) -> Result<String, SubjectError> {
    validate_token(kind)?;
    Ok(format!("audit.{kind}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_device_state_subject() {
        let s = device_state("zigbee", "01hxxabc", "temp").expect("valid");
        assert_eq!(s, "device.zigbee.01hxxabc.temp.state");
    }

    #[test]
    fn rejects_uppercase() {
        assert!(matches!(
            device_state("Zigbee", "a", "b"),
            Err(SubjectError::BadChar(_))
        ));
    }

    #[test]
    fn rejects_dots_in_token() {
        assert!(matches!(
            device_state("a.b", "a", "b"),
            Err(SubjectError::BadChar(_))
        ));
    }

    #[test]
    fn rejects_wildcards() {
        assert!(matches!(device_state("*", "a", "b"), Err(SubjectError::BadChar(_))));
        assert!(matches!(device_state(">", "a", "b"), Err(SubjectError::BadChar(_))));
    }
}
