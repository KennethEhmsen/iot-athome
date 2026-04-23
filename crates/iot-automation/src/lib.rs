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

use anyhow::Result;
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub rules_dir: Option<String>,
}

pub async fn run(_cfg: Config) -> Result<()> {
    info!("iot-automation starting (W2.1 parser/evaluator ready; engine loop lands W2.2)");
    tokio::signal::ctrl_c().await?;
    info!("iot-automation shutting down");
    Ok(())
}
