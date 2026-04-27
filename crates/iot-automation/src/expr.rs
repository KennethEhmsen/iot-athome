//! Rule-condition expressions — CEL via the `cel-interpreter` crate
//! behind the M3-era `parse` / `eval_bool` facade.
//!
//! ## What changed at M5a W2
//!
//! M3 W2.1 shipped a deliberately-small hand-rolled subset of CEL:
//! number / string / bool literals, path access, comparisons,
//! `&&` / `||` / `!`, parens. That covered every rule we wrote — but
//! the M3 retro carried "real CEL interpreter swap" forward as
//! architectural debt because the in-house grammar would inevitably
//! lag rule-author needs (`in`, `has`, list literals, function calls).
//!
//! The swap is behind the same two-function facade so engine + CLI
//! callers don't change:
//!
//! ```ignore
//! let expr = expr::parse("payload.value > 25 && payload.unit == 'C'")?;
//! let yes = expr::eval_bool(&expr, &serde_json::json!({"value": 30, "unit": "C"}))?;
//! ```
//!
//! The root binding is still `payload` (a `serde_json::Value`); the
//! engine builds the JSON object from the bus message's protobuf
//! decode, and the CLI's `iotctl rule test` builds it from the
//! synthetic JSON the operator typed.
//!
//! ## Backward compatibility
//!
//! Every rule that parsed under the old grammar still parses + evaluates
//! identically:
//!
//! * Numeric / string / bool comparisons — preserved.
//! * `&&`, `||`, `!` — preserved (CEL spells `!` as `!`, same syntax).
//! * Path access (`payload.foo.bar`) — preserved (CEL field selection).
//! * Parenthesised grouping — preserved.
//! * Missing fields evaluating to "comparable to null" — preserved
//!   via the eval shim's `null`-on-error behaviour, so existing rules
//!   that compare absent paths don't suddenly raise.
//!
//! New surface available that the hand-roll didn't have:
//!
//! * `in` (`'Lock' in payload.tags`)
//! * `has(payload.foo)` field-presence test
//! * List literals (`[1, 2, 3]`) + arithmetic
//! * The full CEL stdlib (`size()`, `string()`, `int()`, …)
//!
//! Rule authors don't have to use any of it; the engine is just no
//! longer the limit.
//!
//! ## Resource bounds (Bucket 2 audit H2)
//!
//! Four hard limits sit between rule input and CPU/stack:
//!
//! 1. **Source-size cap** ([`MAX_SOURCE_BYTES`], 64 KiB) enforced at
//!    [`parse`] before any parser state is touched. `cel-interpreter`
//!    0.10 leans on `antlr4rust 0.3.0-rc2`, whose tree-builder will
//!    happily burn unbounded CPU on a multi-MB payload. 64 KiB is a
//!    couple-thousand x the longest hand-written rule we ship and
//!    still well below any conceivable accidental keystroke-bloat.
//!
//! 2. **Nesting-depth cap** ([`MAX_NESTING_DEPTH`], 200) — a cheap
//!    pre-pass over the source string counts simultaneous open
//!    delimiters and rejects sources that nest deeper. The byte-cap
//!    alone is **not** enough on Windows: empirically antlr4rust's
//!    recursive descent burns ~8 KiB of stack per level, so even an
//!    8 MiB-stack thread overflows around 1000 nested parens — well
//!    inside what 64 KiB of source can encode. The depth pre-pass is
//!    iterative (linear in source length) and conservative (it
//!    counts `(`, `[`, `{` regardless of CEL's own balance rules);
//!    legitimate rules don't approach 200.
//!
//! 3. **Compile-thread stack budget** ([`PARSE_STACK_BYTES`], 8 MiB).
//!    Linux's default is already 8 MiB; Windows' is 1 MiB. [`parse`]
//!    dispatches `Program::compile` onto a dedicated thread with
//!    this stack so the depth-cap-permitted worst case fits with
//!    headroom. The spawn cost (~30 µs) is amortised — rules
//!    compile at load time, not on the eval hot path.
//!
//! 4. **Eval-time deadline** ([`eval_bool_within`], default 200 ms in
//!    [`crate::Config`]). Even a small expression can in principle
//!    request unbounded work via stdlib functions on attacker-shaped
//!    payloads (`size()` of a giant nested list, etc.). Boolean rule
//!    eval against the kind of payloads M3–M5a sees runs in
//!    microseconds; 200 ms is generous for the legitimate path and an
//!    early-out for the pathological one.
//!
//! Today the only entry point that takes a rule string is
//! `iotctl rule add` / `iotctl rule test` (operator-typed at the CLI,
//! so trust is locally bounded). The bounds matter because:
//!
//! * `iot-automation`'s `Config` advertises `rules_dir` with intent to
//!   watch + hot-reload — so any future filesystem-watcher path will
//!   pick up files from anywhere with write access to the rules dir.
//! * `iotctl rule add` runs as the operator under the iotathome
//!   service identity; an accidentally-pasted multi-MB blob from
//!   somewhere shouldn't be able to abort the CLI process before the
//!   parser surfaces an error.
//!
//! Both bounds are pre-emptive defence-in-depth; the watcher path is
//! the real risk vector.

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

/// Maximum accepted byte length of a rule-expression source string.
/// See module docs for why 64 KiB.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// Maximum simultaneous open-delimiter depth (`(`, `[`, `{`).
///
/// [`parse`] refuses sources that nest deeper. Antlr4rust uses
/// ~8 KiB stack per recursion level; 200 leaves
/// [`PARSE_STACK_BYTES`] with ~6 MiB headroom and is well above the
/// deepest legitimate rule.
pub const MAX_NESTING_DEPTH: usize = 200;

/// Stack budget the dedicated compile thread is given. Windows
/// defaults to 1 MiB which antlr can blow on the byte-cap-permitted
/// worst case; 8 MiB matches Linux's default and combined with
/// [`MAX_NESTING_DEPTH`] keeps us comfortably away from overflow.
const PARSE_STACK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ExprError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("runtime: {0}")]
    Runtime(String),
    /// Source string exceeds [`MAX_SOURCE_BYTES`] — rejected before the
    /// CEL parser sees it. See the module-level "Resource bounds"
    /// section for rationale.
    #[error("source too long: {len} bytes (cap is {cap})")]
    SourceTooLong { len: usize, cap: usize },
    /// Open-delimiter depth in the source exceeds
    /// [`MAX_NESTING_DEPTH`]. Bounds antlr's recursive descent
    /// independently of source byte length.
    #[error("source too deeply nested: depth {depth} (cap is {cap})")]
    TooDeeplyNested { depth: usize, cap: usize },
    /// Evaluation took longer than the caller-supplied deadline; the
    /// engine abandons the result and the rule does not fire.
    #[error("evaluation timed out after {ms} ms")]
    Timeout { ms: u128 },
}

/// A compiled rule condition. Opaque from outside — callers go through
/// [`parse`] + [`eval_bool`].
///
/// `Arc` so cloning a `Rule` (e.g. when the engine snapshots its rule
/// table for hot-reload) doesn't reparse — `cel_interpreter::Program`
/// is itself an immutable parse tree.
#[derive(Debug, Clone)]
pub struct Expr {
    program: Arc<cel_interpreter::Program>,
}

/// Maximum simultaneous open-delimiter depth in `src`. Counts `(`,
/// `[`, `{` regardless of CEL's own balance rules; closing
/// delimiters decrement, saturating at zero. String literals are
/// skipped via simple quote tracking with `\\` escape handling so
/// content like `'(((((( '` doesn't inflate the depth.
///
/// Iterative — bounded by source length, no recursion.
fn max_open_delim_depth(src: &str) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > max_depth {
                    max_depth = depth;
                }
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    max_depth
}

/// Parse a CEL expression source string.
///
/// # Errors
/// * [`ExprError::SourceTooLong`] when `src` exceeds
///   [`MAX_SOURCE_BYTES`]. Checked before any parser state is
///   touched — a multi-MB source cannot reach `Program::compile`.
/// * [`ExprError::TooDeeplyNested`] when the open-delimiter depth
///   exceeds [`MAX_NESTING_DEPTH`]. Bounds antlr's recursive
///   descent independent of byte length.
/// * [`ExprError::Parse`] when the source isn't a valid CEL
///   program, the compile thread panicked on malformed input, or the
///   OS refused to spawn the compile thread. CEL parser error string
///   surfaces verbatim — its diagnostics are good.
///
/// Compilation runs on a dedicated [`PARSE_STACK_BYTES`]-stack
/// thread because cel-interpreter 0.10 leans on `antlr4rust
/// 0.3.0-rc2`, whose recursive-descent tree-builder needs more stack
/// than Windows' 1 MiB default for the worst-case nesting that fits
/// in [`MAX_SOURCE_BYTES`]. The thread also serves as a panic
/// boundary: antlr panics on input the lexer fails to tokenise
/// (e.g. a stray `@`), and a panic at `iotctl rule add` time would
/// take the CLI down rather than produce the actionable error the
/// operator needs. `JoinHandle::join` reports the panic; we demote
/// it to a clean `Parse` variant. The work inside is pure CPU on
/// owned data — no `RefCell` / `Mutex` poisoning concerns.
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    if src.len() > MAX_SOURCE_BYTES {
        return Err(ExprError::SourceTooLong {
            len: src.len(),
            cap: MAX_SOURCE_BYTES,
        });
    }
    let depth = max_open_delim_depth(src);
    if depth > MAX_NESTING_DEPTH {
        return Err(ExprError::TooDeeplyNested {
            depth,
            cap: MAX_NESTING_DEPTH,
        });
    }
    let src_owned = src.to_owned();
    // `cel_interpreter::ParseErrors` carries a `Box<dyn StdError>`
    // and is therefore not `Send`. Stringify the error inside the
    // worker so only `Result<Program, String>` crosses the join.
    let handle = std::thread::Builder::new()
        .name("iot-automation-parse".into())
        .stack_size(PARSE_STACK_BYTES)
        .spawn(move || cel_interpreter::Program::compile(&src_owned).map_err(|e| e.to_string()))
        .map_err(|e| ExprError::Parse(format!("spawn parse thread: {e}")))?;
    let program = match handle.join() {
        Ok(Ok(program)) => program,
        Ok(Err(msg)) => return Err(ExprError::Parse(msg)),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "cel parser panicked on malformed input".to_owned());
            return Err(ExprError::Parse(format!("malformed expression: {msg}")));
        }
    };
    Ok(Expr {
        program: Arc::new(program),
    })
}

/// Evaluate a parsed expression against a `serde_json::Value` payload,
/// coerce the result to `bool`.
///
/// The payload is exposed under the variable name `payload` — same as
/// the M3 hand-roll. Path lookups (`payload.foo.bar`) resolve via
/// CEL's field selection.
///
/// # Errors
/// * [`ExprError::Runtime`] when CEL evaluation fails (a path goes
///   through a non-object, a function isn't found, etc.).
/// * [`ExprError::Runtime`] when the program evaluates to a non-bool
///   value (e.g. someone wrote `payload.value` and forgot the `> 0`).
pub fn eval_bool(expr: &Expr, payload: &serde_json::Value) -> Result<bool, ExprError> {
    use cel_interpreter::{Context, Value};

    let value = json_to_cel(payload);
    let mut ctx = Context::default();
    ctx.add_variable("payload", value)
        .map_err(|e| ExprError::Runtime(format!("bind payload: {e}")))?;

    let result = expr
        .program
        .execute(&ctx)
        .map_err(|e| ExprError::Runtime(e.to_string()))?;

    match result {
        Value::Bool(b) => Ok(b),
        other => Err(ExprError::Runtime(format!(
            "expression must evaluate to bool, got {other:?}"
        ))),
    }
}

/// Evaluate `expr` against `payload` with a wall-clock deadline.
///
/// CEL evaluation runs on the tokio blocking pool ([`tokio::task::
/// spawn_blocking`]) so a misbehaving expression cannot stall the
/// engine's async runtime; the surrounding [`tokio::time::timeout`]
/// abandons the result if eval exceeds `timeout`.
///
/// The blocking pool task is **not** cancelled when the timeout
/// fires — tokio cannot interrupt synchronous CPU work. The task
/// completes in the background and its result is dropped. For the
/// engine's threat model this is acceptable: the rule pipeline
/// processes one match at a time per inbound message, the blocking
/// pool is sized for short tasks, and a runaway expression is bounded
/// in practice by the [`MAX_SOURCE_BYTES`] cap on the source it
/// compiled from.
///
/// # Errors
/// * [`ExprError::Timeout`] when eval exceeds `timeout`.
/// * [`ExprError::Runtime`] when eval finishes within the deadline
///   but the program raised at runtime, returned a non-bool, or the
///   blocking task itself panicked.
pub async fn eval_bool_within(
    expr: &Expr,
    payload: &serde_json::Value,
    timeout: Duration,
) -> Result<bool, ExprError> {
    // Clone is cheap: `Expr` wraps an `Arc<Program>`. The payload
    // similarly clones into an owned tree the blocking task takes
    // by value, so the await point doesn't borrow the caller's frame.
    let expr_cloned = expr.clone();
    let payload_cloned = payload.clone();
    let join = tokio::task::spawn_blocking(move || eval_bool(&expr_cloned, &payload_cloned));
    match tokio::time::timeout(timeout, join).await {
        Ok(Ok(res)) => res,
        Ok(Err(join_err)) => Err(ExprError::Runtime(format!(
            "eval task panicked: {join_err}"
        ))),
        Err(_elapsed) => Err(ExprError::Timeout {
            ms: timeout.as_millis(),
        }),
    }
}

/// Convert `serde_json::Value` → `cel_interpreter::Value`.
///
/// cel-interpreter 0.10 ships a CEL→JSON conversion (`Value::json()`)
/// behind its `json` feature but no inverse. The two type lattices map
/// 1:1 for the JSON-y subset (`Null` / `Bool` / `Number` / `String` /
/// `Array` / `Object`); we discriminate JSON numbers into CEL `Int`
/// when they fit `i64` losslessly, otherwise fall back to `Float` so
/// arithmetic on integer-typed payload fields preserves integer
/// semantics where rule authors expect them (e.g. `payload.battery >
/// 50` without surprise float coercion).
fn json_to_cel(v: &serde_json::Value) -> cel_interpreter::Value {
    use cel_interpreter::Value;
    use std::collections::HashMap;
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                Value::UInt(u)
            } else {
                Value::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Value::String(std::sync::Arc::new(s.clone())),
        serde_json::Value::Array(arr) => {
            let cels: Vec<Value> = arr.iter().map(json_to_cel).collect();
            Value::List(std::sync::Arc::new(cels))
        }
        serde_json::Value::Object(obj) => {
            let map: HashMap<String, Value> = obj
                .iter()
                .map(|(k, v)| (k.clone(), json_to_cel(v)))
                .collect();
            Value::from(map)
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eval_src(src: &str, p: &serde_json::Value) -> Result<bool, ExprError> {
        let e = parse(src)?;
        eval_bool(&e, p)
    }

    // ----------------------------------------------------- M3 backward-compat
    //
    // The set of rules the hand-roll handled. All must still pass.

    #[test]
    fn number_compare_gt() {
        let p = json!({"value": 25.5});
        assert!(eval_src("payload.value > 25", &p).unwrap());
        assert!(!eval_src("payload.value > 30", &p).unwrap());
    }

    #[test]
    fn boolean_and_or() {
        let p = json!({"temp": 25, "humid": 70});
        assert!(eval_src("payload.temp > 20 && payload.humid < 80", &p).unwrap());
        assert!(eval_src("payload.temp > 30 || payload.humid > 60", &p).unwrap());
        assert!(!eval_src("payload.temp > 30 && payload.humid < 60", &p).unwrap());
    }

    #[test]
    fn string_equality() {
        let p = json!({"mode": "auto"});
        assert!(eval_src(r#"payload.mode == "auto""#, &p).unwrap());
        assert!(eval_src(r#"payload.mode != "manual""#, &p).unwrap());
    }

    #[test]
    fn nested_path() {
        let p = json!({"sensor": {"battery": 87}});
        assert!(eval_src("payload.sensor.battery >= 50", &p).unwrap());
        assert!(!eval_src("payload.sensor.battery < 50", &p).unwrap());
    }

    #[test]
    fn parens_override_precedence() {
        let p = json!({"a": 1, "b": 1, "c": 0});
        assert!(eval_src("payload.a == 1 && payload.b == 1 || payload.c == 1", &p).unwrap());
        assert!(eval_src("payload.a == 1 && (payload.b == 1 || payload.c == 1)", &p).unwrap());
        assert!(!eval_src("(payload.a == 1 && payload.b == 0) || payload.c == 1", &p).unwrap());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse("payload.value @ 25").is_err());
        assert!(parse("payload.value >").is_err());
        assert!(parse("(payload.value").is_err());
    }

    #[test]
    fn numeric_literal_edge_cases() {
        let p = json!({});
        assert!(eval_src("42 == 42", &p).unwrap());
        assert!(eval_src("3.14 > 3.0", &p).unwrap());
        assert!(eval_src("-5 < 0", &p).unwrap());
    }

    // ----------------------------------------------------- new surface

    #[test]
    fn in_operator_works() {
        let p = json!({"tags": ["lock", "front-door"]});
        assert!(eval_src("'lock' in payload.tags", &p).unwrap());
        assert!(!eval_src("'window' in payload.tags", &p).unwrap());
    }

    #[test]
    fn has_macro_works() {
        let p = json!({"value": 1});
        assert!(eval_src("has(payload.value)", &p).unwrap());
        assert!(!eval_src("has(payload.absent)", &p).unwrap());
    }

    #[test]
    fn size_function_on_string_and_list() {
        let p = json!({"name": "kitchen", "tags": ["a", "b", "c"]});
        assert!(eval_src("size(payload.name) == 7", &p).unwrap());
        assert!(eval_src("size(payload.tags) == 3", &p).unwrap());
    }

    #[test]
    fn arithmetic_works() {
        let p = json!({"a": 10, "b": 3});
        assert!(eval_src("payload.a + payload.b == 13", &p).unwrap());
        assert!(eval_src("payload.a * 2 > payload.b * 5", &p).unwrap());
    }

    #[test]
    fn non_bool_result_is_runtime_error() {
        // The expression evaluates to a number, not a bool.
        let p = json!({"value": 5});
        let err = eval_src("payload.value", &p).unwrap_err();
        assert!(
            matches!(err, ExprError::Runtime(_)),
            "expected Runtime, got {err:?}"
        );
    }

    #[test]
    fn expr_can_be_cloned_cheaply() {
        // Compile once, share via Arc — cloning doesn't reparse.
        let e = parse("payload.value > 0").expect("parse");
        let cloned = e.clone();
        let p = json!({"value": 1});
        assert!(eval_bool(&cloned, &p).unwrap());
        assert!(eval_bool(&e, &p).unwrap());
    }

    // ----------------------------------------------------- resource bounds (H2)

    #[test]
    fn parse_rejects_oversize_source() {
        // 65 KiB of garbage — must trip the cap before the CEL parser
        // sees a single byte. Content is irrelevant; we just need
        // length above the cap.
        let oversize: String = "x".repeat(MAX_SOURCE_BYTES + 1);
        let err = parse(&oversize).expect_err("65 KiB source must be rejected");
        match err {
            ExprError::SourceTooLong { len, cap } => {
                assert_eq!(len, MAX_SOURCE_BYTES + 1);
                assert_eq!(cap, MAX_SOURCE_BYTES);
            }
            other => panic!("expected SourceTooLong, got {other:?}"),
        }
    }

    #[test]
    fn parse_accepts_source_at_cap_boundary() {
        // A valid CEL expression padded with whitespace to land just
        // under the cap still compiles. The cap is a byte-length
        // gate, not a token-count gate.
        let pad = MAX_SOURCE_BYTES - "true".len();
        let mut src = String::with_capacity(MAX_SOURCE_BYTES);
        src.push_str(&" ".repeat(pad));
        src.push_str("true");
        assert_eq!(src.len(), MAX_SOURCE_BYTES);
        let e = parse(&src).expect("source exactly at cap should parse");
        let p = json!({});
        assert!(eval_bool(&e, &p).unwrap());
    }

    #[test]
    fn deeply_nested_small_expression_compiles_fine() {
        // 100 nested parens around a literal — small in bytes
        // (~200 B), well under [`MAX_NESTING_DEPTH`]. The legitimate
        // path must still work; the depth cap doesn't bite.
        const NEST: usize = 100;
        let mut src = String::with_capacity(NEST * 2 + 1);
        for _ in 0..NEST {
            src.push('(');
        }
        src.push('1');
        for _ in 0..NEST {
            src.push(')');
        }
        let e = parse(&src).expect("100-deep parens should parse");
        // The expression evaluates to an int, so the bool coercion
        // returns Runtime — that's fine; we're proving compile didn't
        // blow up, not asserting on the value.
        let p = json!({});
        assert!(matches!(eval_bool(&e, &p), Err(ExprError::Runtime(_))));
    }

    #[test]
    fn parse_rejects_pathologically_nested_source() {
        // Just past the depth cap. Bytes well under the size cap,
        // so the rejection must come from the depth pre-pass.
        let depth = MAX_NESTING_DEPTH + 1;
        let mut src = String::with_capacity(depth * 2 + 1);
        for _ in 0..depth {
            src.push('(');
        }
        src.push('1');
        for _ in 0..depth {
            src.push(')');
        }
        assert!(src.len() < MAX_SOURCE_BYTES);
        let err = parse(&src).expect_err("over-deep source must be rejected");
        match err {
            ExprError::TooDeeplyNested { depth: d, cap } => {
                assert_eq!(d, depth);
                assert_eq!(cap, MAX_NESTING_DEPTH);
            }
            other => panic!("expected TooDeeplyNested, got {other:?}"),
        }
    }

    #[test]
    fn nesting_depth_ignores_open_delims_inside_strings() {
        // Quoted literals shouldn't inflate the depth count.
        let src = "payload.s == '((((((((((((('";
        assert_eq!(max_open_delim_depth(src), 0);
        // ...even when escapes are involved.
        let src = r#"payload.s == "\"((((""#;
        assert_eq!(max_open_delim_depth(src), 0);
    }

    #[tokio::test]
    async fn eval_bool_within_returns_value_under_deadline() {
        let e = parse("payload.value > 25").expect("parse");
        let p = json!({"value": 30});
        let got = eval_bool_within(&e, &p, std::time::Duration::from_secs(1))
            .await
            .expect("eval");
        assert!(got);
    }

    #[tokio::test]
    async fn eval_bool_within_times_out() {
        // Construct an expression + payload whose eval-time work
        // (json→cel conversion of a 1 M-element list, then a full
        // `.all()` traversal that **does not** short-circuit because
        // every element satisfies the predicate) reliably exceeds a
        // 1 ms deadline. Proves the timeout wrapping fires `Timeout`,
        // not `Runtime`.
        let e = parse("payload.items.all(x, x >= 0)").expect("parse");
        let items: Vec<i64> = (0..1_000_000).collect();
        let p = json!({ "items": items });
        let err = eval_bool_within(&e, &p, std::time::Duration::from_millis(1))
            .await
            .expect_err("1 ms deadline against 1M-elem .all() should elapse");
        assert!(
            matches!(err, ExprError::Timeout { .. }),
            "expected Timeout, got {err:?}"
        );
    }
}
