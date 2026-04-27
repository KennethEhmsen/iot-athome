//! Automation engine.
//!
//! Compiles declarative rules (YAML) into ready-to-evaluate shapes,
//! evaluates their conditions through a small built-in expression
//! language (M3 W2.1 subset — comparisons + boolean combinators, no
//! function calls), and dispatches actions as idempotent bus publishes.
//!
//! M3 W2.1 ships the parser + compiler + evaluator. The engine loop
//! (bus subscribe → match trigger → evaluate `when` → emit actions)
//! arrives in W2.2; the `iotctl rule` CLI in W2.3.

#![forbid(unsafe_code)]

pub mod engine;
pub mod expr;
pub mod rule;

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use tracing::info;

/// Default per-rule eval deadline, milliseconds.
///
/// Boolean rule evaluation against M3–M5a payloads runs in
/// microseconds; the 200 ms budget is generous for legitimate paths
/// and an early-out for pathological CEL stdlib calls (e.g. `size()`
/// on attacker-shaped giant lists). See [`crate::expr`] module docs
/// for the full resource-bounds story.
pub const DEFAULT_EVAL_TIMEOUT_MS: u64 = 200;

const fn default_eval_timeout_ms() -> u64 {
    DEFAULT_EVAL_TIMEOUT_MS
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub rules_dir: Option<String>,
    /// Per-rule eval deadline, milliseconds. A `when` expression that
    /// runs longer than this returns `ExprError::Timeout` and the rule
    /// is treated as not-fired for that message.
    #[serde(default = "default_eval_timeout_ms")]
    pub eval_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rules_dir: None,
            eval_timeout_ms: DEFAULT_EVAL_TIMEOUT_MS,
        }
    }
}

impl Config {
    /// `eval_timeout_ms` as a `Duration`, ready to hand to
    /// [`expr::eval_bool_within`].
    #[must_use]
    pub fn eval_timeout(&self) -> Duration {
        Duration::from_millis(self.eval_timeout_ms)
    }
}

pub async fn run(_cfg: Config) -> Result<()> {
    info!("iot-automation starting (W2.1 parser/evaluator ready; engine loop lands W2.2)");
    tokio::signal::ctrl_c().await?;
    info!("iot-automation shutting down");
    Ok(())
}
