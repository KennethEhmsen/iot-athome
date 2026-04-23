//! Per-plugin runtime task (M2 W4).
//!
//! Wasmtime's `Store<PluginState>` isn't `Sync`, and the export calls
//! (`call_init`, `call_on_message`, `call_on_mqtt_message`) take
//! `&mut store`. The clean pattern is: one tokio task per plugin, that
//! task owns the Store, and external actors (the supervisor, the MQTT
//! broker dispatcher, the bus router) talk to it over an mpsc channel.
//!
//! Ownership:
//! ```text
//!                         ┌─ PluginHandle::tx ─┐
//!   supervisor / broker → │                    │ → run_plugin_task()
//!                         └─ mpsc::Sender      │     owns Store+Plugin
//!                                              │     calls runtime.* exports
//!                                              ▼
//!                              Result<(), CrashReason>
//!                              ↑ returned to the supervisor so it can
//!                                consult the CrashTracker.
//! ```
//!
//! The supervisor (in `supervisor.rs`) awaits the `JoinHandle` each
//! task carries, classifies the outcome, and either restarts the task
//! (with a fresh Store — Wasmtime doesn't let you reuse a crashed one)
//! or writes the `.dead-lettered` marker.

use std::fmt;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wasmtime::Store;

use crate::component::{Plugin, PluginState};

/// What external actors ask a plugin task to do.
#[derive(Debug)]
pub enum PluginCommand {
    /// Deliver a NATS bus message matching the plugin's declared
    /// `bus.subscribe` patterns. Invokes `runtime.on-message`.
    OnBusMessage {
        subject: String,
        iot_type: String,
        payload: Vec<u8>,
    },
    /// Deliver an MQTT broker message matching a filter the plugin
    /// registered via `mqtt::subscribe`. Invokes `runtime.on-mqtt-message`.
    OnMqttMessage { topic: String, payload: Vec<u8> },
    /// Graceful stop. The task completes the in-flight export (if any),
    /// then exits `Ok(())` — the supervisor treats this as a clean
    /// shutdown, not a crash.
    Shutdown,
}

/// Why a plugin task exited abnormally. Fed into `CrashTracker::record`
/// by the supervisor. `Display` picks a short, grep-friendly string.
#[derive(Debug, Clone)]
pub enum CrashReason {
    /// `init()` returned an app-level `PluginError` (no trap, but the
    /// plugin refused to come up).
    InitFailed { code: String, message: String },
    /// `init()` trapped (WASM-level crash, e.g. divide-by-zero, fuel
    /// exhaustion, unreachable).
    InitTrapped(String),
    /// A handler (`on-message` / `on-mqtt-message`) trapped. App-level
    /// `PluginError` returns from handlers are *not* crashes — they're
    /// logged and the task stays up, matching the "capability.denied is
    /// a value, not a panic" contract from ADR-0003.
    HandlerTrapped {
        kind: &'static str, // "on_message" or "on_mqtt_message"
        message: String,
    },
}

impl fmt::Display for CrashReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitFailed { code, message } => {
                write!(f, "init-failed[{code}]: {message}")
            }
            Self::InitTrapped(m) => write!(f, "init-trapped: {m}"),
            Self::HandlerTrapped { kind, message } => {
                write!(f, "{kind}-trapped: {message}")
            }
        }
    }
}

/// Handle the supervisor keeps for each spawned plugin task. `tx` is
/// how the broker dispatcher / bus router feed commands to the task;
/// `join` is how the supervisor watches for clean exit vs. crash.
#[derive(Debug)]
pub struct PluginHandle {
    pub id: String,
    pub tx: mpsc::Sender<PluginCommand>,
    pub join: JoinHandle<Result<(), CrashReason>>,
}

/// Mailbox depth — enough to absorb a short burst from the broker
/// dispatcher without blocking its inbound loop. Tune based on real
/// plugin latency when we get profiling.
const MAILBOX_DEPTH: usize = 64;

/// Spawn a tokio task that owns `store` + `plugin` and runs the command
/// loop. Returns immediately with a handle; the caller (supervisor)
/// awaits `handle.join` to learn the outcome.
///
/// Side effect: injects the newly-minted `tx` into `store.data_mut()`
/// so the plugin's in-task `mqtt::Host::subscribe` impl can register
/// itself with the shared router (which needs a way to deliver back
/// to exactly this task).
pub fn spawn_plugin_task(
    id: String,
    mut store: Store<PluginState>,
    plugin: Plugin,
) -> PluginHandle {
    let (tx, rx) = mpsc::channel(MAILBOX_DEPTH);
    // Give the plugin access to its own mailbox before init() can
    // possibly call mqtt::subscribe.
    store.data_mut().self_tx = Some(tx.clone());

    let id_for_task = id.clone();
    let join = tokio::spawn(async move { run_plugin_task(id_for_task, store, plugin, rx).await });
    PluginHandle { id, tx, join }
}

/// The task body. Calls `init`, then loops on `rx` until shutdown or a
/// trap. A trap from any export short-circuits the loop with `Err`; an
/// app-level `PluginError` from a handler is logged and we keep going.
async fn run_plugin_task(
    id: String,
    mut store: Store<PluginState>,
    plugin: Plugin,
    mut rx: mpsc::Receiver<PluginCommand>,
) -> Result<(), CrashReason> {
    // 1. init — this is the primary "did the plugin come up?" signal.
    match plugin.iot_plugin_host_runtime().call_init(&mut store).await {
        Ok(Ok(())) => {
            tracing::info!(plugin = %id, "init ok");
        }
        Ok(Err(e)) => {
            return Err(CrashReason::InitFailed {
                code: e.code,
                message: e.message,
            });
        }
        Err(trap) => {
            return Err(CrashReason::InitTrapped(trap.to_string()));
        }
    }

    // 2. command loop. Receiver closing means every sender was dropped
    // — treat as a clean shutdown (the supervisor itself dropped the
    // tx before awaiting the join).
    while let Some(cmd) = rx.recv().await {
        match cmd {
            PluginCommand::Shutdown => {
                tracing::info!(plugin = %id, "shutdown command");
                break;
            }
            PluginCommand::OnBusMessage {
                subject,
                iot_type,
                payload,
            } => {
                let outcome = plugin
                    .iot_plugin_host_runtime()
                    .call_on_message(&mut store, &subject, &iot_type, &payload)
                    .await;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(
                            plugin = %id, subject, code = %e.code, message = %e.message,
                            "on_message returned app-level error"
                        );
                    }
                    Err(trap) => {
                        return Err(CrashReason::HandlerTrapped {
                            kind: "on_message",
                            message: trap.to_string(),
                        });
                    }
                }
            }
            PluginCommand::OnMqttMessage { topic, payload } => {
                let outcome = plugin
                    .iot_plugin_host_runtime()
                    .call_on_mqtt_message(&mut store, &topic, &payload)
                    .await;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(
                            plugin = %id, topic, code = %e.code, message = %e.message,
                            "on_mqtt_message returned app-level error"
                        );
                    }
                    Err(trap) => {
                        return Err(CrashReason::HandlerTrapped {
                            kind: "on_mqtt_message",
                            message: trap.to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn crash_reason_display_is_grep_friendly() {
        let r = CrashReason::InitFailed {
            code: "foo.bar".into(),
            message: "nope".into(),
        };
        assert_eq!(r.to_string(), "init-failed[foo.bar]: nope");

        let r = CrashReason::InitTrapped("divide by zero".into());
        assert_eq!(r.to_string(), "init-trapped: divide by zero");

        let r = CrashReason::HandlerTrapped {
            kind: "on_message",
            message: "unreachable".into(),
        };
        assert_eq!(r.to_string(), "on_message-trapped: unreachable");
    }
}
