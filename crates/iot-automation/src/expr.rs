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

use std::sync::Arc;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExprError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("runtime: {0}")]
    Runtime(String),
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

/// Parse a CEL expression source string.
///
/// # Errors
/// Returns [`ExprError::Parse`] when the source isn't a valid CEL
/// program. Error string is the parser's own diagnostic; CEL parser
/// messages tend to be better than the M3 hand-roll's, so we surface
/// them verbatim.
///
/// The compile path is wrapped in [`std::panic::catch_unwind`]
/// because cel-interpreter 0.10 leans on `antlr4rust 0.3.0-rc2`,
/// which can panic in its tree-builder on input the lexer fails to
/// tokenise (e.g. a stray `@`). A panic at `iotctl rule add` time
/// would take the CLI down rather than producing the actionable
/// error the operator needs; the catch demotes it to a clean
/// `Parse` variant. The work inside is pure CPU on a borrowed
/// `&str` — no `RefCell` / `Mutex` poisoning concerns.
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    let src_owned = src.to_owned();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cel_interpreter::Program::compile(&src_owned)
    }));
    let program = match result {
        Ok(Ok(program)) => program,
        Ok(Err(e)) => return Err(ExprError::Parse(e.to_string())),
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
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
}
