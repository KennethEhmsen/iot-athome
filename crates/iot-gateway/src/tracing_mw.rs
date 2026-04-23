//! Axum middleware that threads inbound W3C `traceparent` into the
//! task-local TraceContext for the handler's duration (M3 W2.6+).
//!
//! Effect: every REST / WS handler runs inside a
//! `iot_observability::traceparent::with_context(...)` scope. Bus
//! publishes from inside the handler automatically inherit the
//! traceparent header — the span tree for a single inbound request
//! lines up with every downstream bus message it spawns.
//!
//! Behaviour per request:
//!   * `traceparent` header present + valid → child_of parsed context
//!   * `traceparent` header absent or malformed → fresh root context
//!
//! Malformed headers fall through to a fresh root (rather than 400)
//! because an inbound caller sending garbage traceparent shouldn't
//! take down the panel's connection. The failure is debug-logged so
//! drift is visible.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use iot_observability::traceparent::{self, TraceContext};
use tracing::debug;

/// Middleware entry point.
///
/// Register via `axum::middleware::from_fn(traceparent_mw)` on
/// whichever sub-router should carry the trace context; the gateway
/// applies it to the whole app so `/stream`, `/healthz`, and
/// `/api/v1/*` all propagate.
pub async fn traceparent_mw(request: Request, next: Next) -> Response {
    let ctx = request
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .map_or_else(TraceContext::new_root, |header| {
            match TraceContext::parse(header) {
                Ok(parent) => parent.child_of(),
                Err(e) => {
                    debug!(error = %e, header, "inbound traceparent malformed; starting new root");
                    TraceContext::new_root()
                }
            }
        });
    traceparent::with_context(ctx, next.run(request)).await
}
