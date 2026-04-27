//! `iot-voice` — host-side voice daemon (M5b W3).
//!
//! Two subcommands:
//!
//! * `iot-voice send <text>` — feed `<text>` directly to the closed-
//!   domain parser, publish the resulting intent to NATS, exit. No
//!   audio / wake / STT involved. Useful for:
//!     - Operator smoke-test of the bus integration.
//!     - Rule-engine integration tests.
//!     - A pinned voice trigger from another shell command (cron
//!       firing "activate the night scene" at 22:00, etc.).
//!
//! * `iot-voice listen` — start the full pipeline. **Today** this
//!   wires up stub stages (deterministic, no audio actually
//!   captured) and exits cleanly when the source ends. **Tomorrow**
//!   (M5b W4+) it'll plug in `cpal` + `whisper-rs` + a real wake
//!   detector behind cargo features. The same daemon process,
//!   incrementally upgraded via dependency swaps.
//!
//! Per ADR-0015 §"Decision", this is the host-side daemon. Single
//! `[[bin]]` per the workspace pattern; supervisor (systemd / docker)
//! brings it up alongside `iot-registry` and `iot-gateway`.

#![forbid(unsafe_code)]

mod sink;

use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use iot_bus::{Bus, Config as BusConfig};
use iot_voice::{
    IntentParser, Pipeline, RuleIntentParser, StubAudioSource, StubSpeechRecognizer,
    StubWakeDetector,
};
use tracing::{info, warn};

use crate::sink::{dispatch_and_flush, NatsIntentSink};

#[derive(Debug, Parser)]
#[command(name = "iot-voice", version, about)]
struct Cli {
    /// Publisher identity reported on each NATS publish header.
    /// Defaults to `iot-voice`. Override per-host when running
    /// multiple voice daemons against the same broker.
    #[arg(long, env = "IOT_VOICE_PUBLISHER", default_value = "iot-voice")]
    publisher: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse `<text>` to an Intent and publish to NATS.
    ///
    /// Reads no audio. Useful as the operator's bus smoke-test
    /// path and as a Cron-friendly action trigger.
    Send {
        /// The text to parse. Must match the closed-domain
        /// grammar (`turn on the kitchen light`, `activate the
        /// movie scene`, …). Free-form requests need M5b W4+'s
        /// LLM-fallback path.
        text: String,
    },
    /// Run the full pipeline (audio → wake → stt → intent → sink).
    ///
    /// **Today**: stub stages — useful only for verifying the bus
    /// integration end-to-end. The daemon will yield no real
    /// intents because the stub audio source produces no frames.
    /// **Tomorrow** (M5b W4+): real audio + wake + STT plug in
    /// behind cargo features.
    Listen,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = iot_observability::init(&iot_observability::Config {
        service_name: "iot-voice".into(),
        service_version: env!("CARGO_PKG_VERSION").into(),
        otlp_endpoint: std::env::var("IOT_OTLP_ENDPOINT").ok(),
    });

    let cli = Cli::parse();
    let cfg = BusConfig::from_env(cli.publisher.clone());
    let bus = Bus::connect(cfg)
        .await
        .context("bus connect (mTLS / NATS creds)")?;

    match cli.command {
        Command::Send { text } => cmd_send(&bus, &text).await,
        Command::Listen => cmd_listen(bus).await,
    }
}

/// `send <text>` — parse, publish one intent, exit.
async fn cmd_send(bus: &Bus, text: &str) -> Result<()> {
    let parser = RuleIntentParser::new();
    let intent = match parser.parse(text).await {
        Ok(i) => i,
        Err(e) => {
            // The pipeline's `listen` mode swallows NoMatch — for
            // `send`, where the operator typed a specific phrase,
            // a non-match is a usage error and we fail loudly with
            // exit code != 0.
            anyhow::bail!("intent parse: {e}\n  closed-domain grammar didn't recognise the phrase. Try `turn on the <room> light` / `activate the <name> scene` / `what is the <sensor>`.");
        }
    };
    info!(
        domain = %intent.domain,
        verb = %intent.verb,
        confidence = intent.confidence,
        "publishing intent"
    );
    dispatch_and_flush(bus, &intent).await?;
    println!(
        "published {}.{}.{}",
        sink::NatsIntentSink::subject_for(&intent),
        intent.confidence,
        intent.raw,
    );
    Ok(())
}

/// `listen` — full pipeline with stub stages.
///
/// Today's stack (per ADR-0015 §"What this scaffold doesn't ship"):
///   audio:  StubAudioSource (yields 0 frames)
///   wake:   StubWakeDetector
///   stt:    StubSpeechRecognizer
///   parser: RuleIntentParser
///   sink:   NatsIntentSink (real broker publish)
///
/// The pipeline returns immediately because the stub audio source
/// is empty. The daemon prints a one-line "no real audio yet"
/// notice and exits cleanly, rather than spinning a no-op loop.
/// Real impls land in M5b W4+.
async fn cmd_listen(bus: Bus) -> Result<()> {
    warn!(
        "iot-voice listen: stub stages only — no real audio capture yet. \
         Use `iot-voice send <text>` for end-to-end bus testing today."
    );
    let audio = StubAudioSource::new(Vec::new());
    let wake = StubWakeDetector::default();
    let stt = StubSpeechRecognizer::new("(stub stt has no audio to transcribe)");
    let parser = RuleIntentParser::new();
    let sink = NatsIntentSink::new(bus);
    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
    pipeline.run().await.context("pipeline run")?;
    info!("pipeline ended (stub source exhausted)");
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    // The CLI surface tests live alongside the sink tests in
    // src/sink.rs (subject derivation, payload encoding). End-to-
    // end tests against a live NATS broker would belong in an
    // integration test under tests/, but require testcontainers +
    // a real broker — out of scope for this scaffold commit.
    //
    // What we do verify here: the `Command` enum derives correctly
    // so a typo in `clap`-attributes surfaces at unit-test time
    // rather than at first daemon-launch.
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_send_subcommand() {
        let m = Cli::command()
            .try_get_matches_from(vec!["iot-voice", "send", "turn on the kitchen light"])
            .expect("parse `send`");
        assert_eq!(m.subcommand_name(), Some("send"));
    }

    #[test]
    fn cli_parses_listen_subcommand() {
        let m = Cli::command()
            .try_get_matches_from(vec!["iot-voice", "listen"])
            .expect("parse `listen`");
        assert_eq!(m.subcommand_name(), Some("listen"));
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        Cli::command()
            .try_get_matches_from(vec!["iot-voice", "yelp"])
            .expect_err("expected unknown-subcommand failure");
    }
}
