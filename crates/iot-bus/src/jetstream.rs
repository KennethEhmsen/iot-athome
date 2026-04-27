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
    /// The call drains the consumer in batches of [`WILDCARD_BATCH_SIZE`]
    /// until `num_pending == 0`, so the returned vec contains every
    /// matching subject's latest retained message regardless of how
    /// many devices the home has.
    ///
    /// **Soft cap.** A misbehaving stream (or a home with truly huge
    /// device counts) is bounded by [`WILDCARD_MAX_BATCHES`] iterations
    /// — at most `WILDCARD_BATCH_SIZE * WILDCARD_MAX_BATCHES`
    /// (= 10 240) subjects per call. Above that we warn-log and
    /// return what we got: past 10 K, the panel UI is the bottleneck
    /// long before the storage layer is. Callers can re-call to drain
    /// the rest if they really need to.
    ///
    /// **Backpressure note.** The gateway WS handler streams these
    /// out one-by-one to the client. If the panel reads slower than
    /// the replay produces, the WS write buffer grows; that's a
    /// separate concern for the WS handler to manage (M6).
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

        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        let mut drained = false;
        for _ in 0..WILDCARD_MAX_BATCHES {
            // `info()` takes `&mut self`; clone first so the
            // underlying `consumer` remains usable for `fetch()`.
            let mut info_handle = consumer.clone();
            let pending = info_handle
                .info()
                .await
                .map_err(|e| JetstreamError::LastMsg(format!("consumer info: {e}")))?
                .num_pending;
            if pending == 0 {
                drained = true;
                break;
            }

            let batch_size = pending.min(WILDCARD_BATCH_SIZE as u64);
            let mut batch = consumer
                .fetch()
                .max_messages(usize::try_from(batch_size).unwrap_or(WILDCARD_BATCH_SIZE))
                .messages()
                .await
                .map_err(|e| JetstreamError::LastMsg(format!("fetch batch: {e}")))?;

            while let Some(msg_res) = batch.next().await {
                match msg_res {
                    Ok(msg) => out.push((msg.subject.to_string(), msg.payload.to_vec())),
                    Err(e) => return Err(JetstreamError::LastMsg(format!("stream item: {e}"))),
                }
            }
        }

        if !drained {
            tracing::warn!(
                pattern,
                returned = out.len(),
                cap_subjects = WILDCARD_BATCH_SIZE * WILDCARD_MAX_BATCHES,
                "last_state_wildcard hit soft cap; further subjects truncated — \
                 caller may re-invoke to drain the rest"
            );
        }

        // M6 will replace this with a Prometheus counter; for now a
        // structured debug log gives operators a per-call subject
        // count without a metrics dependency.
        tracing::debug!(
            metric = "iot_bus.wildcard_replay.subjects_returned",
            pattern,
            count = out.len(),
            drained,
            "wildcard replay returned"
        );

        Ok(out)
    }
}

/// Per-fetch batch ceiling for [`Bus::last_state_wildcard`].
///
/// 1024 is a round number well below the JetStream pull-consumer's
/// own per-fetch limit and large enough that O(100)-device homes
/// drain in a single batch.
pub const WILDCARD_BATCH_SIZE: usize = 1024;

/// Total batch budget for [`Bus::last_state_wildcard`].
///
/// `WILDCARD_BATCH_SIZE * WILDCARD_MAX_BATCHES` = 10 240 subjects
/// per call. Above that the panel UI is the bottleneck, not the
/// storage layer; we warn-log and let the caller re-invoke.
pub const WILDCARD_MAX_BATCHES: usize = 10;

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
}
