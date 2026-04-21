//! demo-echo — reference WASM plugin.
//!
//! On every `on_message`, re-publishes the payload on a `<subject>.echo`
//! subject. Exists to exercise the plugin host's load / capability
//! enforcement / host-call round-trip end-to-end.

#![forbid(unsafe_code)]

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{bus, log};

struct Component;

impl Guest for Component {
    fn init() -> Result<(), PluginError> {
        log::emit(log::Level::Info, "demo-echo", "init ok");
        Ok(())
    }

    fn on_message(
        subject: String,
        iot_type: String,
        payload: Payload,
    ) -> Result<(), PluginError> {
        log::emit(
            log::Level::Debug,
            "demo-echo",
            &format!("received {iot_type} on {subject} ({} bytes)", payload.len()),
        );
        let echo_subject = format!("{subject}.echo");
        bus::publish(&echo_subject, &iot_type, &payload)
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
