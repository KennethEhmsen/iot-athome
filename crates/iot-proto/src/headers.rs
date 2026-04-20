//! Canonical NATS header keys — mirror of `iot.bus.v1.BusHeaders` (ADR-0004, ADR-0009).

/// W3C Trace Context traceparent. Required on every message.
pub const TRACEPARENT: &str = "traceparent";

/// W3C Trace Context tracestate. Optional.
pub const TRACESTATE: &str = "tracestate";

/// Protobuf major schema version for the payload. Required.
pub const IOT_SCHEMA_VERSION: &str = "iot-schema-version";

/// Fully-qualified Protobuf type name of the payload. Required.
pub const IOT_TYPE: &str = "iot-type";

/// Publisher identity (plugin_id or service name). Optional.
pub const IOT_PUBLISHER: &str = "iot-publisher";

/// Payload content type. Defaults to `application/x-protobuf` when absent.
pub const CONTENT_TYPE: &str = "content-type";

/// Default content type value for Protobuf payloads.
pub const CONTENT_TYPE_PROTOBUF: &str = "application/x-protobuf";
