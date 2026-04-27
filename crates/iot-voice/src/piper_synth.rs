//! Shell-out [`Synthesizer`] backed by the `piper` TTS binary
//! (M5b W4d).
//!
//! Piper (rhasspy/piper) is the de-facto local TTS for the
//! Home-Assistant-class smart-home stack. The upstream
//! distribution ships pre-built binaries for Linux + macOS +
//! Windows, plus a wide library of voice models in `.onnx`
//! format. Quality is comparable to commercial cloud TTS at
//! ~80 ms synthesis on a Pi 5.
//!
//! ## Why shell-out, not a Rust binding
//!
//! `piper-rs` and `piper-tts` exist on crates.io as Rust
//! bindings, but both wrap the C++ library via CMake + ONNX
//! Runtime — the same heavyweight build complexity that gates
//! `whisper-rs` (M5b W4c) behind a `stt-whisper` feature. For
//! TTS the latency cost of fork/exec (~30 ms on Linux, ~80 ms
//! on Windows) is acceptable on the response path: the operator
//! has already issued a command, the action has fired, and the
//! voice acknowledgement is the *last* step. Detection-path
//! latency (W4a/b/c) is what we optimised for.
//!
//! Shell-out has the additional virtue that operators can swap
//! the Piper binary out for `espeak-ng`, `coqui-tts`, or any
//! other CLI that consumes text-on-stdin and emits WAV-on-stdout
//! — the synthesiser doesn't care about the binary identity, only
//! its I/O protocol.
//!
//! ## Operator setup
//!
//! 1. Download piper binary + a voice model from
//!    <https://github.com/rhasspy/piper/releases>. Voice models
//!    are at <https://huggingface.co/rhasspy/piper-voices/tree/main>.
//!    `en_GB-alba-medium.onnx` is a reasonable starting point
//!    (~60 MB).
//! 2. Place the binary on `PATH`. On Windows: drop
//!    `piper.exe` into `C:\Users\<user>\.iot-athome\bin\` and
//!    add to `PATH`. On Linux: `/usr/local/bin/piper`.
//! 3. Place the model files at
//!    `~/.iot-athome/models/voices/<voice>.onnx` and
//!    `~/.iot-athome/models/voices/<voice>.onnx.json`
//!    (piper requires both — the JSON is config metadata).
//! 4. The daemon's `--tts-model <path>` flag points at the
//!    `.onnx` file.
//!
//! ## Output format
//!
//! Piper writes WAV-PCM to stdout. The synthesiser parses the
//! header to extract sample rate + 16-bit-PCM payload, then
//! converts to `f32 [-1, 1]` for the trait contract. Sample
//! rate stays at whatever the model used (typically 22.05 kHz
//! for `medium` voices); the daemon's playback path resamples
//! to the platform output device's preferred rate.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tracing::{debug, info};

use crate::synth::{SynthError, SynthesisedAudio, Synthesizer};

/// Piper binary name. Operator drops it on `PATH`; we don't
/// hard-code an absolute path so the same daemon binary works
/// across distros.
const PIPER_BIN: &str = "piper";

/// `Synthesizer` impl that shells out to `piper` per call.
#[derive(Debug, Clone)]
pub struct PiperBinarySynthesizer {
    voice_model: PathBuf,
}

impl PiperBinarySynthesizer {
    /// Build with the path to a `.onnx` voice model.
    ///
    /// Doesn't actually exec piper — that happens on each
    /// `speak()` call. The model path is validated lazily; a
    /// missing file or malformed model surfaces as a
    /// [`SynthError::Backend`] from the first synthesis.
    #[must_use]
    pub fn new(voice_model: impl Into<PathBuf>) -> Self {
        Self {
            voice_model: voice_model.into(),
        }
    }

    /// Path to the voice model, exposed for diagnostics.
    #[must_use]
    pub fn voice_model(&self) -> &Path {
        &self.voice_model
    }
}

#[async_trait]
impl Synthesizer for PiperBinarySynthesizer {
    async fn speak(&mut self, text: &str) -> Result<SynthesisedAudio, SynthError> {
        let voice_model = self.voice_model.clone();
        let text_owned = text.to_owned();
        // Shell-out + WAV parse is a blocking-ish op (the
        // exec + read is sub-second but still synchronous).
        // Spawn onto the blocking pool to keep the async runtime
        // free; same pattern as the WhisperRecognizer.
        tokio::task::spawn_blocking(move || run_piper(&voice_model, &text_owned))
            .await
            .map_err(|e| SynthError::Backend(format!("spawn_blocking join: {e}")))?
    }
}

/// Exec `piper --model <model> -f -`, write `text` to stdin,
/// read WAV-PCM from stdout, parse + return as
/// [`SynthesisedAudio`].
fn run_piper(voice_model: &Path, text: &str) -> Result<SynthesisedAudio, SynthError> {
    info!(
        target: "iot_voice::piper_synth",
        model = %voice_model.display(),
        text_len = text.len(),
        "synthesising"
    );

    let mut child = std::process::Command::new(PIPER_BIN)
        .arg("--model")
        .arg(voice_model)
        // `-f -` would write to a file `-` (literally); piper's
        // convention is `--output_raw` for stdout audio, but the
        // CLI's WAV-on-stdout default is the most portable shape
        // across piper versions. We simply read stdout below.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SynthError::Backend(format!("spawn `{PIPER_BIN}`: {e}; is the binary on PATH?")))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| SynthError::Backend("piper child stdin unavailable".into()))?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| SynthError::Backend(format!("write stdin: {e}")))?;
        stdin
            .write_all(b"\n")
            .map_err(|e| SynthError::Backend(format!("write stdin newline: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| SynthError::Backend(format!("wait piper: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SynthError::Backend(format!(
            "piper exited with {}: stderr={stderr}",
            output.status.code().unwrap_or(-1)
        )));
    }
    debug!(
        target: "iot_voice::piper_synth",
        wav_bytes = output.stdout.len(),
        "piper synthesis complete"
    );
    parse_wav(&output.stdout)
}

/// Parse piper's WAV-on-stdout output into `SynthesisedAudio`.
///
/// Piper emits canonical RIFF/WAVE PCM-16. We tolerate the
/// minimum-viable subset:
///
/// * RIFF "WAVE" header with a `fmt ` chunk + `data` chunk.
/// * Format tag = 1 (PCM).
/// * Channels = 1 (mono).
/// * Bits per sample = 16.
///
/// Any deviation is a clean error rather than a silent garbage
/// playback.
fn parse_wav(bytes: &[u8]) -> Result<SynthesisedAudio, SynthError> {
    if bytes.len() < 44 {
        return Err(SynthError::Backend(format!(
            "WAV too short: {} bytes (need ≥ 44)",
            bytes.len()
        )));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(SynthError::Backend("missing RIFF/WAVE magic".into()));
    }

    // Walk chunks looking for `fmt ` then `data`. Piper writes
    // them in that order in practice, but the WAV spec doesn't
    // require it; iterate to be safe.
    let mut sample_rate: Option<u32> = None;
    let mut channels: Option<u16> = None;
    let mut bits: Option<u16> = None;
    let mut data: Option<&[u8]> = None;
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => {
                if size < 16 {
                    return Err(SynthError::Backend(format!("fmt chunk too small: {size}")));
                }
                let fmt = &bytes[body_start..body_end];
                let fmt_tag = u16::from_le_bytes([fmt[0], fmt[1]]);
                if fmt_tag != 1 {
                    return Err(SynthError::Backend(format!(
                        "unsupported WAV format tag {fmt_tag}; expected 1 (PCM)"
                    )));
                }
                channels = Some(u16::from_le_bytes([fmt[2], fmt[3]]));
                sample_rate = Some(u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]));
                bits = Some(u16::from_le_bytes([fmt[14], fmt[15]]));
            }
            b"data" => {
                data = Some(&bytes[body_start..body_end]);
            }
            _ => { /* skip LIST, JUNK, etc. */ }
        }
        pos = body_end + (size & 1); // RIFF chunks pad to even boundary
    }

    let sample_rate =
        sample_rate.ok_or_else(|| SynthError::Backend("WAV missing fmt chunk".into()))?;
    let channels = channels.ok_or_else(|| SynthError::Backend("WAV missing channels".into()))?;
    let bits = bits.ok_or_else(|| SynthError::Backend("WAV missing bits-per-sample".into()))?;
    let data = data.ok_or_else(|| SynthError::Backend("WAV missing data chunk".into()))?;

    if channels != 1 {
        return Err(SynthError::Backend(format!(
            "expected mono, got {channels} channels"
        )));
    }
    if bits != 16 {
        return Err(SynthError::Backend(format!(
            "expected 16-bit PCM, got {bits}"
        )));
    }

    // Convert i16 LE → f32 [-1, 1].
    let sample_count = data.len() / 2;
    let mut samples = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let lo = data[i * 2];
        let hi = data[i * 2 + 1];
        let s = i16::from_le_bytes([lo, hi]);
        samples.push(f32::from(s) / f32::from(i16::MAX));
    }
    let duration_ms =
        u32::try_from(sample_count * 1000 / sample_rate.max(1) as usize).unwrap_or(u32::MAX);
    Ok(SynthesisedAudio {
        samples,
        sample_rate,
        duration_ms,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Build a minimal valid WAV blob in-memory: RIFF header,
    /// fmt chunk (PCM, 1 channel, sample_rate, 16-bit), data
    /// chunk with `samples` i16-LE.
    fn make_wav(sample_rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        let data_bytes = samples.len() * 2;
        let total_size = u32::try_from(36 + data_bytes).expect("test fixture < 4 GB");
        let data_size = u32::try_from(data_bytes).expect("test fixture < 4 GB");
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&total_size.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits/sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_size.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn parse_wav_round_trips_i16_to_f32() {
        let blob = make_wav(22_050, &[0, i16::MAX, i16::MIN, -16384, 16384]);
        let audio = parse_wav(&blob).expect("parse");
        assert_eq!(audio.sample_rate, 22_050);
        assert_eq!(audio.samples.len(), 5);
        // 0 → 0.0
        assert!(audio.samples[0].abs() < f32::EPSILON);
        // i16::MAX → ~1.0
        assert!((audio.samples[1] - 1.0).abs() < 0.001);
        // i16::MIN → ~-1.0 (slight offset because i16 range is asymmetric)
        assert!(audio.samples[2] < -0.99);
        // duration_ms = 5 samples * 1000 / 22050 ≈ 0
        assert!(audio.duration_ms < 5);
    }

    #[test]
    fn parse_wav_rejects_short_input() {
        let err = parse_wav(b"RIFF").unwrap_err();
        assert!(matches!(err, SynthError::Backend(_)), "{err:?}");
    }

    #[test]
    fn parse_wav_rejects_missing_magic() {
        // Looks RIFF-shaped but isn't.
        let mut blob = vec![0u8; 64];
        blob[0..4].copy_from_slice(b"OGGS");
        let err = parse_wav(&blob).unwrap_err();
        assert!(matches!(err, SynthError::Backend(_)));
    }

    #[test]
    fn parse_wav_rejects_stereo() {
        // Construct a WAV with channels=2.
        let mut blob = Vec::new();
        blob.extend_from_slice(b"RIFF");
        blob.extend_from_slice(&36u32.to_le_bytes());
        blob.extend_from_slice(b"WAVE");
        blob.extend_from_slice(b"fmt ");
        blob.extend_from_slice(&16u32.to_le_bytes());
        blob.extend_from_slice(&1u16.to_le_bytes()); // PCM
        blob.extend_from_slice(&2u16.to_le_bytes()); // STEREO
        blob.extend_from_slice(&22_050u32.to_le_bytes());
        blob.extend_from_slice(&88_200u32.to_le_bytes());
        blob.extend_from_slice(&4u16.to_le_bytes());
        blob.extend_from_slice(&16u16.to_le_bytes());
        blob.extend_from_slice(b"data");
        blob.extend_from_slice(&0u32.to_le_bytes());
        let err = parse_wav(&blob).unwrap_err();
        let SynthError::Backend(msg) = err;
        assert!(msg.contains("mono"), "{msg}");
    }

    #[test]
    fn parse_wav_rejects_non_pcm_format() {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"RIFF");
        blob.extend_from_slice(&36u32.to_le_bytes());
        blob.extend_from_slice(b"WAVE");
        blob.extend_from_slice(b"fmt ");
        blob.extend_from_slice(&16u32.to_le_bytes());
        blob.extend_from_slice(&3u16.to_le_bytes()); // IEEE float, not PCM
        blob.extend_from_slice(&1u16.to_le_bytes());
        blob.extend_from_slice(&22_050u32.to_le_bytes());
        blob.extend_from_slice(&88_200u32.to_le_bytes());
        blob.extend_from_slice(&4u16.to_le_bytes());
        blob.extend_from_slice(&32u16.to_le_bytes());
        blob.extend_from_slice(b"data");
        blob.extend_from_slice(&0u32.to_le_bytes());
        let err = parse_wav(&blob).unwrap_err();
        let SynthError::Backend(msg) = err;
        assert!(msg.contains("PCM"), "{msg}");
    }
}
