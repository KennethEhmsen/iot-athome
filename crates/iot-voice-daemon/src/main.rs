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
    EnergyVadWakeDetector, IntentParser, LogIntentSink, Pipeline, PiperBinarySynthesizer,
    RuleIntentParser, SpeechRecognizer, StubAudioSource, StubSpeechRecognizer, StubWakeDetector,
    Synthesizer, WakeDetector,
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
    /// With `--use-mic` (requires the `mic` cargo feature, which
    /// pulls in `iot-voice/cpal`): real audio capture from the
    /// platform's default input device. Without `--use-mic`: a
    /// stub audio source that yields no frames — useful only for
    /// confirming the daemon launches + the bus connects.
    ///
    /// Real wake-word + STT impls land in subsequent commits;
    /// without them, even `--use-mic` produces no usable intents
    /// (the stub wake fires on amplitude, the stub STT returns a
    /// fixed phrase, both serve mostly as smoke-tests).
    Listen {
        /// Capture from the default microphone instead of the
        /// stub. Refused at runtime if the binary wasn't built
        /// with `--features mic`.
        #[arg(long)]
        use_mic: bool,
        /// Path to a ggml-format whisper.cpp model file. When
        /// set, real STT replaces the stub recogniser. Refused
        /// at runtime if the binary wasn't built with
        /// `--features stt-whisper`. Common path is
        /// `~/.iot-athome/models/ggml-base.en.bin` — see
        /// `iot-voice/src/whisper.rs` module docs for download
        /// instructions.
        #[arg(long, env = "IOT_VOICE_STT_MODEL")]
        stt_model: Option<std::path::PathBuf>,
        /// Path to a rustpotter `.rpw` wake-word model. When
        /// set, phrase-specific wake replaces the always-on
        /// energy-VAD detector. Refused at runtime if the binary
        /// wasn't built with `--features wake-phrase`. See
        /// `iot-voice/src/rustpotter_wake.rs` for model-training
        /// instructions.
        #[arg(long, env = "IOT_VOICE_WAKE_MODEL")]
        wake_model: Option<std::path::PathBuf>,
        /// Skip the NATS bus handshake; pipe transcribed
        /// intents to `tracing::info!` instead of publishing.
        /// Use this to smoke-test mic + wake + STT without
        /// having to bring up the dev stack first. Every
        /// recognised intent shows in the daemon's stdout.
        /// Unrecognised transcriptions still log at info!
        /// level so the operator sees what whisper heard.
        #[arg(long)]
        dry_run: bool,
    },
    /// Synthesise `<text>` to a WAV file via the piper TTS
    /// binary, write to stdout (or `--output <path>`).
    ///
    /// Useful as the operator's response-path smoke-test:
    /// confirms the piper binary + voice model are wired up
    /// correctly, before the rule engine starts firing
    /// acknowledgements at synthesis time. M5b W4d.
    Say {
        /// Text to synthesise. Multi-word phrases need quoting:
        /// `iot-voice say "the kitchen light is on"`.
        text: String,
        /// Path to a piper `.onnx` voice model. The matching
        /// `<model>.onnx.json` config file must sit next to it
        /// (piper requires both).
        #[arg(long, env = "IOT_VOICE_TTS_MODEL")]
        tts_model: std::path::PathBuf,
        /// Write the WAV bytes here. Defaults to `synthesised.wav`
        /// in the current directory; pass `-` for stdout (e.g.
        /// `iot-voice say --output - "hello" | aplay`).
        #[arg(long, default_value = "synthesised.wav")]
        output: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = iot_observability::init(&iot_observability::Config {
        service_name: "iot-voice".into(),
        service_version: env!("CARGO_PKG_VERSION").into(),
        otlp_endpoint: std::env::var("IOT_OTLP_ENDPOINT").ok(),
    });

    let cli = Cli::parse();

    // `say` is bus-free — TTS is purely local. Skip the
    // mTLS/NATS handshake so the operator can smoke-test piper
    // without the dev stack running.
    if let Command::Say {
        text,
        tts_model,
        output,
    } = &cli.command
    {
        return cmd_say(text, tts_model, output).await;
    }

    // `listen --dry-run` is also bus-free — same reasoning:
    // smoke-test mic + wake + STT without the dev stack.
    // Match before the Bus::connect so a missing broker
    // doesn't refuse the dry-run.
    if let Command::Listen {
        use_mic,
        stt_model,
        wake_model,
        dry_run: true,
    } = &cli.command
    {
        return cmd_listen_dry_run(*use_mic, stt_model.clone(), wake_model.clone()).await;
    }

    let cfg = BusConfig::from_env(cli.publisher.clone());
    let bus = Bus::connect(cfg)
        .await
        .context("bus connect (mTLS / NATS creds)")?;

    match cli.command {
        Command::Send { text } => cmd_send(&bus, &text).await,
        Command::Listen {
            use_mic,
            stt_model,
            wake_model,
            dry_run: false,
        } => cmd_listen(bus, use_mic, stt_model, wake_model).await,
        Command::Listen { dry_run: true, .. } | Command::Say { .. } => {
            unreachable!("handled before bus connect")
        }
    }
}

/// `say <text>` — synthesise via piper, write WAV.
///
/// Bus-free: this is the local TTS smoke test, no broker needed.
/// The operator validates piper + voice-model wiring before the
/// rule engine starts firing acknowledgements.
async fn cmd_say(text: &str, tts_model: &std::path::Path, output: &str) -> Result<()> {
    use std::io::Write as _;
    let mut synth = PiperBinarySynthesizer::new(tts_model);
    let audio = synth
        .speak(text)
        .await
        .map_err(|e| anyhow::anyhow!("piper synth: {e}"))?;
    info!(
        text_len = text.len(),
        sample_rate = audio.sample_rate,
        duration_ms = audio.duration_ms,
        samples = audio.samples.len(),
        "synthesised; writing WAV"
    );

    // Re-encode the f32 samples back into a WAV envelope so the
    // operator can `aplay` / open in any audio tool. Round-trips
    // through f32 → i16 once.
    let wav_bytes = encode_wav(&audio.samples, audio.sample_rate);

    if output == "-" {
        std::io::stdout()
            .write_all(&wav_bytes)
            .context("write WAV to stdout")?;
    } else {
        std::fs::write(output, &wav_bytes).with_context(|| format!("write {output}"))?;
        println!(
            "synthesised {} samples @ {} Hz ({} ms) → {output}",
            audio.samples.len(),
            audio.sample_rate,
            audio.duration_ms,
        );
    }
    Ok(())
}

/// Encode mono `f32 [-1, 1]` PCM samples + sample rate into a
/// canonical RIFF/WAVE blob — inverse of the parser in
/// `iot_voice::piper_synth`. Lets `iot-voice say` write a file
/// any audio tool can open without depending on a Rust WAV crate.
///
/// `try_from` + `unwrap_or` for the size casts: a single TTS
/// utterance can't conceivably exceed the WAV-32-bit-size cap
/// (`u32::MAX` ≈ 4 GB ≈ 6 hours of 16-bit 22 kHz mono); on the
/// theoretical overflow we cap at `u32::MAX` so the WAV header
/// stays valid even if downstream readers see a truncated `data`
/// chunk. Better than a panic-on-overflow `as u32`.
fn encode_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_bytes = samples.len() * 2;
    let total_size = u32::try_from(36 + data_bytes).unwrap_or(u32::MAX);
    let data_size = u32::try_from(data_bytes).unwrap_or(u32::MAX);
    let byte_rate = sample_rate * 2;

    let mut out = Vec::with_capacity(44 + data_bytes);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&total_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits/sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    for s in samples {
        // f32 [-1, 1] → i16 [-32768, 32767]. clamp + scale, same
        // shape as the rustpotter wake adapter.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let q = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    out
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

/// `listen` — full pipeline.
///
/// Audio + STT are composed at runtime based on flags + features:
///
/// * `--use-mic` (requires `--features mic`): real default-input
///   capture via cpal. Loops forever, dropping frames into the
///   pipeline.
/// * `--stt-model <path>` (requires `--features stt-whisper`):
///   real Whisper STT loaded from the ggml-format model file.
///   Without this flag, the stub recogniser substitutes (returns
///   a placeholder phrase that the closed-domain parser rejects).
///
/// `--stt-model` without `--use-mic` is refused — a stub audio
/// source produces no frames, so loading a 140 MB model just to
/// transcribe nothing is operator-error and we say so.
async fn cmd_listen(
    bus: Bus,
    use_mic: bool,
    stt_model: Option<std::path::PathBuf>,
    wake_model: Option<std::path::PathBuf>,
) -> Result<()> {
    let sink: Arc<dyn iot_voice::IntentSink> = Arc::new(NatsIntentSink::new(bus));
    run_listen_inner(use_mic, stt_model, wake_model, sink).await
}

/// `listen --dry-run` — same pipeline shape as `cmd_listen` but
/// with [`LogIntentSink`] replacing the NATS publish path. No
/// bus connection; intents log to `tracing::info!` so the
/// operator can smoke-test the mic + wake + STT chain without
/// the dev stack running.
///
/// Useful when:
///
/// * The mic / model setup is being validated before the
///   broker is configured.
/// * The operator wants to see what whisper actually heard
///   without firing real automation rules.
/// * Pen-test / debugging — log-only output keeps the
///   `command.intent.>` subject quiet.
async fn cmd_listen_dry_run(
    use_mic: bool,
    stt_model: Option<std::path::PathBuf>,
    wake_model: Option<std::path::PathBuf>,
) -> Result<()> {
    info!("iot-voice listen --dry-run: bus skipped; intents log-only");
    let sink: Arc<dyn iot_voice::IntentSink> = Arc::new(LogIntentSink);
    run_listen_inner(use_mic, stt_model, wake_model, sink).await
}

/// Shared assembly of the listen pipeline. Both the bus-backed
/// (`cmd_listen`) and the dry-run (`cmd_listen_dry_run`) callers
/// reach this with their own `IntentSink`; the rest of the
/// pipeline (wake, STT, parser, pre-flight validation) is
/// identical.
async fn run_listen_inner(
    use_mic: bool,
    stt_model: Option<std::path::PathBuf>,
    wake_model: Option<std::path::PathBuf>,
    sink: Arc<dyn iot_voice::IntentSink>,
) -> Result<()> {
    let parser = RuleIntentParser::new();

    if stt_model.is_some() && !use_mic {
        anyhow::bail!(
            "--stt-model requires --use-mic; without real audio there's nothing to transcribe"
        );
    }
    if wake_model.is_some() && !use_mic {
        anyhow::bail!(
            "--wake-model requires --use-mic; phrase-specific wake needs real audio frames"
        );
    }

    let stt = build_stt(stt_model.as_deref())?;
    let wake = build_wake(use_mic, wake_model.as_deref())?;

    if use_mic {
        run_with_mic(wake, stt, parser, sink).await
    } else {
        warn!(
            "iot-voice listen: stub audio (no --use-mic). \
             Use `iot-voice send <text>` for end-to-end bus testing today, \
             or rebuild with `--features mic` and rerun with `--use-mic`."
        );
        let audio = StubAudioSource::new(Vec::new());
        let mut pipeline = Pipeline::new(audio, wake, stt, parser, sink);
        pipeline.run().await.context("pipeline run")?;
        info!("pipeline ended (stub source exhausted)");
        Ok(())
    }
}

/// Build the wake detector, picking rustpotter over energy-VAD
/// when a `--wake-model` is set + the `wake-phrase` feature is on.
///
/// Selection matrix:
///
/// | use_mic | wake_model | Detector |
/// |---------|-----------|----------|
/// | false   | n/a       | StubWakeDetector (amplitude) |
/// | true    | None      | EnergyVadWakeDetector (always-on) |
/// | true    | Some(_)   | RustpotterWakeDetector (phrase-specific) |
#[cfg(feature = "wake-phrase")]
fn build_wake(
    use_mic: bool,
    wake_model: Option<&std::path::Path>,
) -> Result<Box<dyn WakeDetector>> {
    if let Some(path) = wake_model {
        info!(model = %path.display(), "loading rustpotter wake-word model");
        let r = iot_voice::RustpotterWakeDetector::load(path)
            .map_err(|e| anyhow::anyhow!("load wake model: {e}"))?;
        return Ok(Box::new(r));
    }
    if use_mic {
        Ok(Box::new(EnergyVadWakeDetector::new()))
    } else {
        Ok(Box::new(StubWakeDetector::default()))
    }
}

#[cfg(not(feature = "wake-phrase"))]
fn build_wake(
    use_mic: bool,
    wake_model: Option<&std::path::Path>,
) -> Result<Box<dyn WakeDetector>> {
    if wake_model.is_some() {
        anyhow::bail!(
            "binary built without --features wake-phrase; \
             rebuild iot-voice-daemon with `cargo build -p iot-voice-daemon --features wake-phrase` \
             to enable --wake-model."
        );
    }
    if use_mic {
        Ok(Box::new(EnergyVadWakeDetector::new()))
    } else {
        Ok(Box::new(StubWakeDetector::default()))
    }
}

/// Build the recogniser, picking Whisper over the stub when
/// `model_path` is set + the `stt-whisper` feature is on.
#[cfg(feature = "stt-whisper")]
fn build_stt(model_path: Option<&std::path::Path>) -> Result<Box<dyn SpeechRecognizer>> {
    if let Some(path) = model_path {
        info!(model = %path.display(), "loading Whisper STT");
        let r = iot_voice::WhisperRecognizer::load(path)
            .map_err(|e| anyhow::anyhow!("load whisper model: {e}"))?;
        Ok(Box::new(r))
    } else {
        warn!(
            "--stt-model not set; STT will return placeholder text \
             that no rule will match. Pass --stt-model <path> for real transcription."
        );
        Ok(Box::new(StubSpeechRecognizer::new(
            "(stub stt — pass --stt-model for real transcription)",
        )))
    }
}

#[cfg(not(feature = "stt-whisper"))]
fn build_stt(model_path: Option<&std::path::Path>) -> Result<Box<dyn SpeechRecognizer>> {
    if model_path.is_some() {
        anyhow::bail!(
            "binary built without --features stt-whisper; \
             rebuild iot-voice-daemon with `cargo build -p iot-voice-daemon --features stt-whisper` \
             to enable --stt-model. Note: requires CMake + Clang on PATH."
        );
    }
    Ok(Box::new(StubSpeechRecognizer::new(
        "(stub stt has no audio to transcribe)",
    )))
}

#[cfg(feature = "mic")]
async fn run_with_mic(
    wake: Box<dyn WakeDetector>,
    stt: Box<dyn SpeechRecognizer>,
    parser: RuleIntentParser,
    sink: Arc<dyn iot_voice::IntentSink>,
) -> Result<()> {
    use iot_voice::CpalAudioSource;
    info!("starting cpal audio capture; speak after startup");
    let audio = CpalAudioSource::start().context("open default input device")?;
    let mut pipeline = Pipeline::new(audio, wake, stt, parser, sink);
    pipeline.run().await.context("pipeline run")?;
    info!("pipeline ended");
    Ok(())
}

#[cfg(not(feature = "mic"))]
// Same async signature as the `mic` arm so the call site is
// uniform — the await on the bail-fast path costs nothing.
#[allow(clippy::unused_async)]
async fn run_with_mic(
    _wake: Box<dyn WakeDetector>,
    _stt: Box<dyn SpeechRecognizer>,
    _parser: RuleIntentParser,
    _sink: Arc<dyn iot_voice::IntentSink>,
) -> Result<()> {
    anyhow::bail!(
        "binary built without --features mic; \
         rebuild iot-voice-daemon with `cargo build -p iot-voice-daemon --features mic` to enable --use-mic"
    )
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
    fn cli_parses_listen_with_use_mic() {
        let m = Cli::command()
            .try_get_matches_from(vec!["iot-voice", "listen", "--use-mic"])
            .expect("parse `listen --use-mic`");
        assert_eq!(m.subcommand_name(), Some("listen"));
        let listen_m = m.subcommand_matches("listen").expect("listen subcommand");
        assert!(listen_m.get_flag("use_mic"));
    }

    #[test]
    fn cli_parses_listen_with_dry_run() {
        let m = Cli::command()
            .try_get_matches_from(vec!["iot-voice", "listen", "--use-mic", "--dry-run"])
            .expect("parse `listen --use-mic --dry-run`");
        let listen_m = m.subcommand_matches("listen").expect("listen subcommand");
        assert!(listen_m.get_flag("use_mic"));
        assert!(listen_m.get_flag("dry_run"));
    }

    #[test]
    fn cli_parses_listen_with_stt_model() {
        let m = Cli::command()
            .try_get_matches_from(vec![
                "iot-voice",
                "listen",
                "--use-mic",
                "--stt-model",
                "/tmp/ggml-base.en.bin",
            ])
            .expect("parse `listen --use-mic --stt-model …`");
        let listen_m = m.subcommand_matches("listen").expect("listen subcommand");
        assert!(listen_m.get_flag("use_mic"));
        assert_eq!(
            listen_m
                .get_one::<std::path::PathBuf>("stt_model")
                .map(std::path::PathBuf::as_path),
            Some(std::path::Path::new("/tmp/ggml-base.en.bin")),
        );
    }

    #[test]
    fn cli_parses_say_subcommand() {
        let m = Cli::command()
            .try_get_matches_from(vec![
                "iot-voice",
                "say",
                "--tts-model",
                "/tmp/voice.onnx",
                "hello there",
            ])
            .expect("parse `say --tts-model … <text>`");
        let say_m = m.subcommand_matches("say").expect("say subcommand");
        assert_eq!(
            say_m.get_one::<String>("text").map(String::as_str),
            Some("hello there"),
        );
        assert_eq!(
            say_m
                .get_one::<std::path::PathBuf>("tts_model")
                .map(std::path::PathBuf::as_path),
            Some(std::path::Path::new("/tmp/voice.onnx")),
        );
    }

    #[test]
    fn cli_say_requires_tts_model() {
        // No --tts-model on `say`; clap should refuse since the
        // arg is required (no default_value, no `Option<…>`).
        Cli::command()
            .try_get_matches_from(vec!["iot-voice", "say", "hello"])
            .expect_err("expected --tts-model required");
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        Cli::command()
            .try_get_matches_from(vec!["iot-voice", "yelp"])
            .expect_err("expected unknown-subcommand failure");
    }
}
