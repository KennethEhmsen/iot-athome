//! W3C Trace Context propagation (M3 W2.4).
//!
//! Format (<https://www.w3.org/TR/trace-context/#traceparent-header>):
//!
//! ```text
//!   traceparent: 00-<trace-id>-<parent-id>-<trace-flags>
//!     version   = "00" (the only version we emit / accept)
//!     trace-id  = 32 hex chars (16 bytes), lowercase, not all-zero
//!     parent-id = 16 hex chars (8 bytes), lowercase, not all-zero
//!     flags     = 2 hex chars (1 byte). Bit 0 = sampled.
//! ```
//!
//! This module carries the *format + generation* half — pure, testable,
//! no tokio runtime or span coupling. The integration that reads the
//! current tracing span's context + injects on every bus publish lands
//! in the follow-up commit (it needs `tracing-opentelemetry` and a
//! real `opentelemetry-sdk` Tracer; the M3 plan acknowledges this as
//! staged work).
//!
//! The intent is: every service that participates in a trace generates
//! one of these via [`TraceContext::new_root`] at the start of its
//! own work (or [`TraceContext::child_of`] when it has a parent it
//! received), serialises via [`TraceContext::to_header`], attaches as
//! a NATS header via the existing `iot_bus::TRACEPARENT` constant.
//! The receiving service parses via [`TraceContext::parse`] and
//! creates its own child.

use std::fmt;

/// Parsed W3C traceparent. 128-bit trace-id, 64-bit span-id, 8-bit
/// flags. Cheap to clone (it's 25 bytes of POD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub flags: u8,
}

/// Errors from parsing a traceparent header.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TraceparentError {
    #[error("wrong number of segments (expected 4, got {0})")]
    WrongSegmentCount(usize),
    #[error("unsupported version: {0}")]
    UnsupportedVersion(String),
    #[error("malformed trace-id (must be 32 lowercase hex chars, non-zero)")]
    BadTraceId,
    #[error("malformed span-id (must be 16 lowercase hex chars, non-zero)")]
    BadSpanId,
    #[error("malformed flags (must be 2 lowercase hex chars)")]
    BadFlags,
}

/// Default flags for newly-generated contexts: sampled (bit 0 set).
/// Downstream trace collectors drop contexts with sampled=0, so we
/// keep the default on — M3 doesn't yet have a sampler to flip it.
pub const FLAGS_SAMPLED: u8 = 0x01;

impl TraceContext {
    /// Fresh root context with random trace-id + span-id + sampled=1.
    ///
    /// # Panics
    /// Only if the OS RNG fails — a condition that's much more broken
    /// than a trace not being emitted. Tests + dev loops shouldn't hit
    /// this; in prod we'd rather know about it than fall back to a
    /// deterministic ID that silently kills uniqueness.
    #[must_use]
    pub fn new_root() -> Self {
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];
        fill_random(&mut trace_id);
        fill_random(&mut span_id);
        // W3C requires non-zero ids. getrandom gives full-entropy 16B /
        // 8B — the probability of all-zero is 2^-128 / 2^-64, i.e.
        // negligible. Guarding anyway because the spec is the spec.
        if trace_id == [0; 16] {
            trace_id[0] = 1;
        }
        if span_id == [0; 8] {
            span_id[0] = 1;
        }
        Self {
            trace_id,
            span_id,
            flags: FLAGS_SAMPLED,
        }
    }

    /// Derive a child context: same trace-id, fresh span-id, inherited
    /// flags.
    #[must_use]
    pub fn child_of(&self) -> Self {
        let mut span_id = [0u8; 8];
        fill_random(&mut span_id);
        if span_id == [0; 8] {
            span_id[0] = 1;
        }
        Self {
            trace_id: self.trace_id,
            span_id,
            flags: self.flags,
        }
    }

    /// Render as a W3C traceparent header value.
    #[must_use]
    pub fn to_header(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            hex_encode(&self.trace_id),
            hex_encode(&self.span_id),
            self.flags
        )
    }

    /// Parse a traceparent header value.
    ///
    /// # Errors
    /// One of the [`TraceparentError`] variants for each shape /
    /// content problem. Per the W3C spec we reject all-zero ids and
    /// any version byte != `00` (other versions are forward-compat
    /// but we don't speak them).
    pub fn parse(header: &str) -> Result<Self, TraceparentError> {
        let segs: Vec<&str> = header.trim().split('-').collect();
        if segs.len() != 4 {
            return Err(TraceparentError::WrongSegmentCount(segs.len()));
        }
        if segs[0] != "00" {
            return Err(TraceparentError::UnsupportedVersion(segs[0].to_owned()));
        }
        let trace_id: [u8; 16] = hex_decode_fixed(segs[1]).ok_or(TraceparentError::BadTraceId)?;
        if trace_id == [0; 16] {
            return Err(TraceparentError::BadTraceId);
        }
        let span_id: [u8; 8] = hex_decode_fixed(segs[2]).ok_or(TraceparentError::BadSpanId)?;
        if span_id == [0; 8] {
            return Err(TraceparentError::BadSpanId);
        }
        let flags_bytes: [u8; 1] = hex_decode_fixed(segs[3]).ok_or(TraceparentError::BadFlags)?;
        Ok(Self {
            trace_id,
            span_id,
            flags: flags_bytes[0],
        })
    }
}

impl fmt::Display for TraceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_header())
    }
}

// ------------------------------------------------------ hex / randomness

fn hex_encode(bytes: &[u8]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(CHARS[(b >> 4) as usize] as char);
        out.push(CHARS[(b & 0xF) as usize] as char);
    }
    out
}

fn hex_decode_fixed<const N: usize>(src: &str) -> Option<[u8; N]> {
    if src.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = decode_nib(src.as_bytes()[i * 2])?;
        let lo = decode_nib(src.as_bytes()[i * 2 + 1])?;
        *o = (hi << 4) | lo;
    }
    Some(out)
}

fn decode_nib(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None, // uppercase hex isn't spec-compliant
    }
}

fn fill_random(buf: &mut [u8]) {
    // Panic-on-fail is intentional (see `new_root` docs).
    #[allow(clippy::expect_used)]
    getrandom::getrandom(buf).expect("OS RNG");
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let tc = TraceContext::new_root();
        let header = tc.to_header();
        let parsed = TraceContext::parse(&header).unwrap();
        assert_eq!(parsed, tc);
        // Sanity on the on-wire shape.
        assert!(header.starts_with("00-"));
        assert_eq!(header.len(), 2 + 1 + 32 + 1 + 16 + 1 + 2);
    }

    #[test]
    fn child_inherits_trace_id_but_not_span_id() {
        let parent = TraceContext::new_root();
        let child = parent.child_of();
        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
        assert_eq!(child.flags, parent.flags);
    }

    #[test]
    fn rejects_wrong_version() {
        let bad = "01-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01";
        assert_eq!(
            TraceContext::parse(bad),
            Err(TraceparentError::UnsupportedVersion("01".into()))
        );
    }

    #[test]
    fn rejects_wrong_segment_count() {
        assert!(matches!(
            TraceContext::parse("00-abc-def"),
            Err(TraceparentError::WrongSegmentCount(3))
        ));
    }

    #[test]
    fn rejects_zero_ids() {
        let zero_trace = "00-00000000000000000000000000000000-bbbbbbbbbbbbbbbb-01";
        assert_eq!(
            TraceContext::parse(zero_trace),
            Err(TraceparentError::BadTraceId)
        );
        let zero_span = "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-0000000000000000-01";
        assert_eq!(
            TraceContext::parse(zero_span),
            Err(TraceparentError::BadSpanId)
        );
    }

    #[test]
    fn rejects_uppercase_hex() {
        // Spec says lowercase only.
        let upper = "00-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA-bbbbbbbbbbbbbbbb-01";
        assert_eq!(
            TraceContext::parse(upper),
            Err(TraceparentError::BadTraceId)
        );
    }

    #[test]
    fn rejects_bad_length() {
        // 31 chars of trace-id — one short.
        let short = "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01";
        assert_eq!(
            TraceContext::parse(short),
            Err(TraceparentError::BadTraceId)
        );
    }

    #[test]
    fn flags_roundtrip() {
        let mut tc = TraceContext::new_root();
        tc.flags = 0x00; // not-sampled
        let header = tc.to_header();
        assert!(header.ends_with("-00"));
        assert_eq!(TraceContext::parse(&header).unwrap().flags, 0x00);

        tc.flags = 0xff;
        let header = tc.to_header();
        assert!(header.ends_with("-ff"));
        assert_eq!(TraceContext::parse(&header).unwrap().flags, 0xff);
    }

    #[test]
    fn two_roots_are_unique() {
        // With 128 bits of randomness the probability of collision
        // is negligible; this guards against an accidental
        // fixed-seed bug.
        let a = TraceContext::new_root();
        let b = TraceContext::new_root();
        assert_ne!(a.trace_id, b.trace_id);
        assert_ne!(a.span_id, b.span_id);
    }
}
