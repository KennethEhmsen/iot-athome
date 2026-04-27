//! JetStream helpers (M3 W2.5).
//!
//! Two narrow utilities on top of `async_nats::jetstream`:
//!
//! 1. [`ensure_device_state_stream`] — idempotently creates the
//!    `DEVICE_STATE` stream that holds the last message per
//!    `device.*.*.*.state` subject. The panel survives reload because
//!    this stream replays last-known values on first connect.
//! 2. [`last_state`] — fetches the last retained message on a single
//!    subject. Callable from the gateway's WebSocket handler when a
//!    client subscribes: emit the replayed value immediately, then
//!    stream live updates from the core subscription.
//!
//! Commands (`cmd.>`) explicitly do NOT land on a stream — replaying
//! them would re-issue actions on reconnect, which is user-visible
//! foot-destruction. Only state.

use async_nats::jetstream::stream;

use crate::Bus;

/// Stream name + subject filter. Kept as constants so consumers
/// elsewhere (iot-gateway, iotctl diagnostics) agree on the naming.
pub const DEVICE_STATE_STREAM: &str = "DEVICE_STATE";
pub const DEVICE_STATE_SUBJECT: &str = "device.>";

/// Hard upper bound on replay-fan-out per call. A wildcard subject
/// like `device.>` can match thousands of subjects on a busy hub —
/// without this cap, a single panel reconnect could dump tens of
/// thousands of messages into a single fetch and stall the gateway
/// loop. The audit's M3 finding flagged the previous `min(1024)`
/// cap as best-case-only — `num_pending` is a server-reported value
/// that can race with new publishes between consumer-create and
/// fetch. This is the absolute ceiling, regardless of `num_pending`.
const REPLAY_HARD_CAP: u64 = 5000;

/// Fetched-batch size — the loop pulls at most this many messages
/// per `fetch().messages()` round, then yields back to the runtime
/// before the next batch. Sized to keep per-batch latency under
/// ~10 ms even on a slow disk-backed JetStream, so the gateway's
/// async loop stays responsive during a wide replay.
const REPLAY_BATCH_SIZE: usize = 500;

// Compile-time invariants on the replay-cap constants — see the
// inline rationale on REPLAY_HARD_CAP / REPLAY_BATCH_SIZE for what
// each band protects against. A `const { assert!(...) }` block
// catches a regression at compile time rather than during tests.
const _: () = {
    // Hard cap > batch size — otherwise we'd never run a 2nd batch.
    assert!(REPLAY_HARD_CAP > REPLAY_BATCH_SIZE as u64);
    // Batch size large enough to make progress per round-trip, small
    // enough to yield often.
    assert!(REPLAY_BATCH_SIZE >= 100);
    assert!(REPLAY_BATCH_SIZE <= 2000);
    // "Any reasonable home" ceiling for distinct device subjects
    // under `device.>`. If the cap looks low, the operator's real
    // workload deserves a paginated API, not a higher cap.
    assert!(REPLAY_HARD_CAP >= 1000);
    assert!(REPLAY_HARD_CAP <= 50_000);
};

impl Bus {
    /// Idempotently create (or upgrade) the `DEVICE_STATE` stream.
    /// Safe to call on every process start — `get_or_create_stream`
    /// is a no-op when the stream already exists with a matching
    /// config.
    ///
    /// # Errors
    /// Propagates `async_nats::jetstream` errors (network failure, or
    /// an existing stream whose config conflicts and can't be
    /// reconciled).
    pub async fn ensure_device_state_stream(&self) -> Result<(), JetstreamError> {
        let ctx = async_nats::jetstream::new(self.raw().clone());
        let config = stream::Config {
            name: DEVICE_STATE_STREAM.to_owned(),
            subjects: vec![DEVICE_STATE_SUBJECT.to_owned()],
            retention: stream::RetentionPolicy::Limits,
            // One message per subject — the point of the stream is
            // "what's the last known state" not "history of every
            // change" (that'd be TimescaleDB in M3 W3).
            max_messages_per_subject: 1,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::Old,
            ..stream::Config::default()
        };
        ctx.get_or_create_stream(config).await?;
        tracing::info!(
            stream = DEVICE_STATE_STREAM,
            filter = DEVICE_STATE_SUBJECT,
            "JetStream stream ensured"
        );
        Ok(())
    }

    /// Fetch the last retained message on `subject` from the
    /// `DEVICE_STATE` stream. Returns `Ok(None)` when there is no
    /// message for that subject (fresh installs, post-purge).
    ///
    /// # Errors
    /// `NoStream` if the stream isn't present (run
    /// `ensure_device_state_stream` first). Other variants wrap
    /// transport / server errors.
    pub async fn last_state(&self, subject: &str) -> Result<Option<Vec<u8>>, JetstreamError> {
        let ctx = async_nats::jetstream::new(self.raw().clone());
        let s = ctx.get_stream(DEVICE_STATE_STREAM).await?;
        match s.get_last_raw_message_by_subject(subject).await {
            Ok(msg) => Ok(Some(msg.payload.to_vec())),
            // The crate surfaces "no message found" as an error kind —
            // translate to None so callers can cleanly distinguish
            // "brand-new subject, nothing to replay" from a real fault.
            Err(e) if is_no_message(&e) => Ok(None),
            Err(e) => Err(JetstreamError::LastMsg(e.to_string())),
        }
    }

    /// Replay the last retained message for every subject matching a
    /// NATS wildcard pattern (e.g. `device.>`, `device.zigbee2mqtt.*.*.state`).
    ///
    /// Backed by an ephemeral JetStream pull-consumer with
    /// `DeliverPolicy::LastPerSubject` and `filter_subject = pattern`.
    /// The consumer reads each subject's latest retained message
    /// exactly once, then signals "no more" via `num_pending == 0`
    /// — at which point the call returns. The consumer is dropped
    /// (and the server collects it) when the consumer handle goes
    /// out of scope at function end.
    ///
    /// Use this from the gateway WS handler when a panel client
    /// subscribes to a wildcard subject — every device's last-known
    /// state arrives before the live firehose kicks in.
    ///
    /// Each yielded entry is `(subject, payload)`. Order isn't
    /// guaranteed across subjects (JetStream replays in stream-time
    /// order, which is publish order, not subject order).
    ///
    /// # Errors
    /// `GetStream` if the stream isn't present. `LastMsg` wraps
    /// consumer-create + fetch-batch failures.
    pub async fn last_state_wildcard(
        &self,
        pattern: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, JetstreamError> {
        use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
        use futures::StreamExt as _;

        let ctx = async_nats::jetstream::new(self.raw().clone());
        let stream = ctx.get_stream(DEVICE_STATE_STREAM).await?;

        // Ephemeral consumer (no durable name): server GCs it after
        // inactivity. AckNone since we don't need redelivery for a
        // one-shot replay.
        let consumer = stream
            .create_consumer(pull::Config {
                deliver_policy: DeliverPolicy::LastPerSubject,
                filter_subject: pattern.to_owned(),
                ack_policy: AckPolicy::None,
                inactive_threshold: std::time::Duration::from_secs(30),
                ..pull::Config::default()
            })
            .await
            .map_err(|e| JetstreamError::LastMsg(format!("create consumer: {e}")))?;

        // Use the consumer's info for `num_pending` — that's the
        // count of last-per-subject messages waiting. Bound the
        // batch fetch by it; if zero, return early. `info()` takes
        // `&mut self`; clone first so the underlying `consumer`
        // remains usable for the subsequent `fetch()`.
        //
        // `num_pending` is informational, not a hard contract — new
        // publishes between this call and the fetch can arrive on
        // the same filter — so the loop below also enforces
        // `REPLAY_HARD_CAP` in the message-counting path. The
        // audit's M3 finding called out the previous single-batch
        // shape: a single `fetch().max_messages(1024)` could fan
        // out unbounded if `num_pending` was reported low at info()
        // time but the broker delivered more on the wire.
        let mut info_handle = consumer.clone();
        let pending = info_handle
            .info()
            .await
            .map_err(|e| JetstreamError::LastMsg(format!("consumer info: {e}")))?
            .num_pending;
        if pending == 0 {
            return Ok(Vec::new());
        }

        // Bounded chunked-fetch loop — each iteration pulls at most
        // REPLAY_BATCH_SIZE messages, yields to the runtime between
        // batches, and bails as soon as REPLAY_HARD_CAP is reached.
        // `target_total` is informational only; the real ceiling is
        // REPLAY_HARD_CAP applied to the running message counter.
        let target_total = pending.min(REPLAY_HARD_CAP);
        let mut out = Vec::with_capacity(usize::try_from(target_total).unwrap_or(0));
        let mut truncated = false;

        while (out.len() as u64) < target_total {
            // Don't ask the broker for more than we'd accept.
            let remaining = target_total - out.len() as u64;
            let this_batch = remaining.min(REPLAY_BATCH_SIZE as u64);
            let this_batch_usize = usize::try_from(this_batch).unwrap_or(REPLAY_BATCH_SIZE);

            let mut batch = consumer
                .fetch()
                .max_messages(this_batch_usize)
                .messages()
                .await
                .map_err(|e| JetstreamError::LastMsg(format!("fetch batch: {e}")))?;

            let mut got_any = false;
            while let Some(msg_res) = batch.next().await {
                match msg_res {
                    Ok(msg) => {
                        out.push((msg.subject.to_string(), msg.payload.to_vec()));
                        got_any = true;
                        if (out.len() as u64) >= REPLAY_HARD_CAP {
                            truncated = true;
                            break;
                        }
                    }
                    Err(e) => return Err(JetstreamError::LastMsg(format!("stream item: {e}"))),
                }
            }

            if truncated {
                break;
            }
            if !got_any {
                // Broker had nothing more on this batch — `num_pending`
                // overcounted (subject was deleted between info() and
                // fetch, or the stream was purged). Stop instead of
                // looping forever asking for messages that aren't
                // coming.
                break;
            }

            // Yield back to the runtime so the gateway's WS loop +
            // other consumers don't starve during a wide replay.
            tokio::task::yield_now().await;
        }

        if truncated {
            tracing::warn!(
                target: "iot_bus::jetstream",
                pattern = pattern,
                returned = out.len(),
                hard_cap = REPLAY_HARD_CAP,
                pending_at_create = pending,
                "wildcard replay truncated at hard cap; remaining subjects need a follow-up call"
            );
        }
        Ok(out)
    }
}

/// Public thin wrapper around `async_nats::jetstream` errors.
///
/// The crate's own error types are variant-heavy and exposing them
/// directly would pull `jetstream::context::ErrorKind` into every
/// downstream match.
#[derive(Debug, thiserror::Error)]
pub enum JetstreamError {
    #[error("get_or_create_stream: {0}")]
    CreateOrGet(String),
    #[error("get_stream: {0}")]
    GetStream(String),
    #[error("get_last_msg: {0}")]
    LastMsg(String),
}

impl From<async_nats::jetstream::context::CreateStreamError> for JetstreamError {
    fn from(e: async_nats::jetstream::context::CreateStreamError) -> Self {
        Self::CreateOrGet(e.to_string())
    }
}

impl From<async_nats::jetstream::context::GetStreamError> for JetstreamError {
    fn from(e: async_nats::jetstream::context::GetStreamError) -> Self {
        Self::GetStream(e.to_string())
    }
}

/// Heuristic for "no retained message for this subject". The crate's
/// error types bubble up the NATS server's `{ code: 404, err_code:
/// 10037 }` as a string; rather than depend on unstable error-enum
/// variants we match the error's Display form.
fn is_no_message(e: &impl std::fmt::Display) -> bool {
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("no message found") || msg.contains("10037")
}

/// Build a per-device state subject string from its parts.
///
/// Exposed as a helper so the gateway can convert a panel
/// subscription (integration + device ULID + entity) into the exact
/// retained-message key without duplicating the format string.
/// Expressed as a function rather than a const template so any
/// future subject-shape change happens here + the tests catch
/// regressions.
#[must_use]
pub fn device_state_subject(plugin: &str, device_id_lc: &str, entity: &str) -> String {
    format!("device.{plugin}.{device_id_lc}.{entity}.state")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    // The live stream-lifecycle tests require a NATS container; they
    // live in the integration suite (W2.5b). This module's unit tests
    // just cover the pure helpers.

    #[test]
    fn constants_match_nats_filter_shape() {
        // The subject filter ends with `>` (NATS multi-wildcard) so
        // the stream captures every subject under `device.`.
        assert!(DEVICE_STATE_SUBJECT.ends_with('>'));
    }

    #[test]
    fn device_state_subject_shape() {
        assert_eq!(
            device_state_subject("zigbee2mqtt", "01hxx", "temperature"),
            "device.zigbee2mqtt.01hxx.temperature.state"
        );
    }

    #[test]
    fn is_no_message_matches_nats_phrasing() {
        struct Stub(&'static str);
        impl std::fmt::Display for Stub {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        assert!(is_no_message(&Stub("no message found")));
        assert!(is_no_message(&Stub("server error 10037: no message")));
        assert!(!is_no_message(&Stub("stream not found")));
        assert!(!is_no_message(&Stub("connection refused")));
    }

    // The replay-cap invariants are pure constant arithmetic — they
    // live in a `const { assert!(...) }` block at module scope (just
    // below the const declarations themselves) so the checks run at
    // compile time, which is both stricter (clippy::items_after_test_module
    // friendly) and more useful as a regression guard.
}
