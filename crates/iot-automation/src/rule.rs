//! Rule YAML loader (M3 W2.1).
//!
//! A rule file is the user-facing surface of the automation engine.
//! Shape, in YAML:
//!
//! ```yaml
//! id: kitchen-fan-hot
//! description: Turn kitchen fan on when kitchen temp > 25 °C.
//! triggers:
//!   - device.zigbee2mqtt.kitchen-temp.temperature.state
//! when: "payload.value > 25"
//! actions:
//!   - publish:
//!       subject: cmd.zigbee2mqtt.kitchen-fan.on
//!       iot_type: iot.device.v1.Command
//!       payload: {}
//! idempotency: "{{trigger}}:{{payload.at}}"
//! ```
//!
//! At load time the YAML parses + the `when` expression compiles to
//! a pre-bound `Expr`. Evaluation is a per-message function of
//! `(compiled_when, payload)` — the parser never runs on the hot path.
//!
//! This module covers the parse-and-validate surface only; the engine
//! loop that subscribes to triggers + fires actions lands in W2.2.

use serde::Deserialize;
use thiserror::Error;

use crate::expr::{self, Expr};

/// Errors from loading / compiling a rule file.
#[derive(Debug, Error)]
pub enum RuleError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("rule `{rule}` expression failed to parse: {source}")]
    Expr {
        rule: String,
        #[source]
        source: expr::ExprError,
    },
    #[error("rule `{rule}`: {msg}")]
    Validation { rule: String, msg: String },
}

/// Raw (YAML-shaped) rule; doesn't yet contain the compiled `when`
/// expression. Crosses the deserialisation boundary; converted into
/// a [`Rule`] via `compile`.
#[derive(Debug, Clone, Deserialize)]
pub struct RawRule {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// NATS subject patterns that wake this rule. Each inbound message
    /// matching any pattern runs the `when` expression.
    pub triggers: Vec<String>,
    /// The expression. Must evaluate to a boolean.
    pub when: String,
    pub actions: Vec<RawAction>,
    /// Idempotency template. Rendered per firing; duplicate keys
    /// inside the engine's rolling cache short-circuit.
    /// Unrendered form (e.g. `"{{trigger}}:{{payload.at}}"`) is fine
    /// to store verbatim — W2.2 fills this in.
    #[serde(default)]
    pub idempotency: String,
}

/// Action the rule emits when `when` passes.
#[derive(Debug, Clone, Deserialize)]
pub enum RawAction {
    /// Publish on the bus.
    #[serde(rename = "publish")]
    Publish {
        subject: String,
        #[serde(default = "default_iot_type")]
        iot_type: String,
        /// JSON payload. Gets serialised via the bus codec at emit
        /// time; `{}` is a valid empty-payload default.
        #[serde(default = "default_payload")]
        payload: serde_json::Value,
    },
    /// Structured log line — useful for dry-runs + test rules.
    #[serde(rename = "log")]
    Log { level: String, message: String },
}

fn default_iot_type() -> String {
    "application/json".into()
}

fn default_payload() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Fully-compiled, engine-ready rule. Holds the pre-parsed `when`
/// expression so the hot path doesn't re-parse on every trigger.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub when: Expr,
    pub actions: Vec<RawAction>,
    pub idempotency: String,
}

impl Rule {
    /// Parse + compile a single rule from YAML source.
    ///
    /// # Errors
    /// `Yaml` for malformed YAML; `Expr` if the `when` expression
    /// doesn't parse; `Validation` for structurally-bad rules
    /// (empty triggers list etc.).
    pub fn from_yaml(src: &str) -> Result<Self, RuleError> {
        let raw: RawRule = serde_yaml::from_str(src)?;
        Self::compile(raw)
    }

    /// Parse a file containing a single rule.
    ///
    /// # Errors
    /// Propagates `Io` on read failure + everything
    /// [`Self::from_yaml`] returns.
    pub fn from_file(path: &std::path::Path) -> Result<Self, RuleError> {
        let src = std::fs::read_to_string(path)?;
        Self::from_yaml(&src)
    }

    /// Compile a pre-deserialised `RawRule`.
    ///
    /// # Errors
    /// Same as [`Self::from_yaml`] minus the YAML step.
    pub fn compile(raw: RawRule) -> Result<Self, RuleError> {
        if raw.id.is_empty() {
            return Err(RuleError::Validation {
                rule: raw.id,
                msg: "id must not be empty".into(),
            });
        }
        if raw.triggers.is_empty() {
            return Err(RuleError::Validation {
                rule: raw.id,
                msg: "rule must declare at least one trigger".into(),
            });
        }
        if raw.actions.is_empty() {
            return Err(RuleError::Validation {
                rule: raw.id,
                msg: "rule must declare at least one action".into(),
            });
        }
        let when = expr::parse(&raw.when).map_err(|e| RuleError::Expr {
            rule: raw.id.clone(),
            source: e,
        })?;

        Ok(Self {
            id: raw.id,
            description: raw.description,
            triggers: raw.triggers,
            when,
            actions: raw.actions,
            idempotency: raw.idempotency,
        })
    }

    /// Does this rule's trigger set match `subject`?
    ///
    /// M3 W2.1 keeps trigger matching to exact equality. Wildcard
    /// support (`device.zigbee2mqtt.*.temperature.state`) lands with
    /// the engine loop in W2.2 where there's a full router anyway.
    #[must_use]
    pub fn triggers_on(&self, subject: &str) -> bool {
        self.triggers.iter().any(|t| t == subject)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    const KITCHEN_FAN: &str = r#"
id: kitchen-fan-hot
description: Turn kitchen fan on when kitchen temp > 25 C.
triggers:
  - device.zigbee2mqtt.kitchen-temp.temperature.state
when: "payload.value > 25"
actions:
  - !publish
    subject: cmd.zigbee2mqtt.kitchen-fan.on
    iot_type: iot.device.v1.Command
"#;

    #[test]
    fn parses_full_rule() {
        let rule = Rule::from_yaml(KITCHEN_FAN).expect("parse");
        assert_eq!(rule.id, "kitchen-fan-hot");
        assert_eq!(rule.triggers.len(), 1);
        assert_eq!(rule.actions.len(), 1);
        match &rule.actions[0] {
            RawAction::Publish {
                subject, iot_type, ..
            } => {
                assert_eq!(subject, "cmd.zigbee2mqtt.kitchen-fan.on");
                assert_eq!(iot_type, "iot.device.v1.Command");
            }
            RawAction::Log { .. } => panic!("expected Publish action, got Log"),
        }
    }

    #[test]
    fn compiled_when_evaluates_correctly() {
        let rule = Rule::from_yaml(KITCHEN_FAN).expect("parse");
        let hot = serde_json::json!({"value": 28.0});
        let cold = serde_json::json!({"value": 18.0});
        assert!(expr::eval_bool(&rule.when, &hot).unwrap());
        assert!(!expr::eval_bool(&rule.when, &cold).unwrap());
    }

    #[test]
    fn triggers_on_exact_subject() {
        let rule = Rule::from_yaml(KITCHEN_FAN).expect("parse");
        assert!(rule.triggers_on("device.zigbee2mqtt.kitchen-temp.temperature.state"));
        assert!(!rule.triggers_on("device.zigbee2mqtt.livingroom-temp.temperature.state"));
    }

    #[test]
    fn rejects_empty_triggers() {
        let src = r#"
id: x
triggers: []
when: "true"
actions:
  - !log
    level: info
    message: hi
"#;
        let err = Rule::from_yaml(src).expect_err("should fail");
        assert!(matches!(err, RuleError::Validation { .. }));
    }

    #[test]
    fn rejects_bad_expression() {
        let src = r#"
id: x
triggers:
  - t
when: "payload.value @ 10"
actions:
  - !log
    level: info
    message: hi
"#;
        let err = Rule::from_yaml(src).expect_err("should fail");
        assert!(matches!(err, RuleError::Expr { .. }));
    }
}
