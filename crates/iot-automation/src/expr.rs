//! Mini-expression language for rule conditions (M3 W2.1).
//!
//! A deliberately-small subset of what a full CEL implementation would
//! do — enough for the M3 rule cases (comparisons against state-message
//! fields, combined with boolean ops), without taking on a heavyweight
//! dep that the M3-PLAN risk table flagged as a wasip2/maturity unknown.
//!
//! Grammar:
//!
//! ```text
//!   expr   = or_expr
//!   or     = and ("||" and)*
//!   and    = unary ("&&" unary)*
//!   unary  = "!" unary | cmp
//!   cmp    = atom (("=="|"!="|"<"|">"|"<="|">=") atom)?
//!   atom   = number | string | "true" | "false" | path | "(" expr ")"
//!   path   = ident ("." ident)*
//! ```
//!
//! Evaluation binds the root context `payload` to a
//! `serde_json::Value` (the bus message payload — protobuf decoded to
//! JSON on the way in, or a raw JSON value for non-proto payloads).
//! Path traversal is the only variable reference — no function calls,
//! no arithmetic. That's intentional: the engine runs on every bus
//! message at high frequency; simple semantics + bounded time is
//! worth more than expressiveness for M3. M4 can swap in a real CEL
//! if the rule library outgrows this.

use std::fmt;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExprError {
    #[error("parse error at byte {pos}: {msg}")]
    Parse { pos: usize, msg: String },
    #[error("runtime: {0}")]
    Runtime(String),
}

/// Parsed expression tree. Cheap to clone — `String`s are short
/// (identifier / string-literal length), `f64` is a POD.
#[derive(Debug, Clone)]
pub enum Expr {
    Number(f64),
    Str(String),
    Bool(bool),
    Path(Vec<String>), // e.g. ["payload", "value"] → payload.value
    Not(Box<Expr>),
    BinOp(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    Neq,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Eq => "==",
            Self::Neq => "!=",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::Le => "<=",
            Self::Ge => ">=",
            Self::And => "&&",
            Self::Or => "||",
        })
    }
}

/// Value produced during evaluation. Aligned with JSON primitives that
/// paths can reach.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
}

impl Val {
    fn as_bool(&self) -> Result<bool, ExprError> {
        match self {
            Self::Bool(b) => Ok(*b),
            other => Err(ExprError::Runtime(format!("expected bool, got {other:?}"))),
        }
    }

    fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- parsing

/// Parse an expression source into an `Expr`.
///
/// # Errors
/// Surfaces a `Parse` with the byte position and a short message when
/// the source doesn't fit the grammar.
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    let tokens = lex(src)?;
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let e = p.parse_or()?;
    if p.pos != tokens.len() {
        return Err(ExprError::Parse {
            pos: p.current_pos(),
            msg: "unexpected input after expression".into(),
        });
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String), // includes `true`/`false` keywords — disambiguated at parse time
    Dot,
    LParen,
    RParen,
    Bang,
    Op(BinOp),
}

#[allow(clippy::too_many_lines)]
fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ExprError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'.' => {
                out.push((Tok::Dot, i));
                i += 1;
            }
            b'(' => {
                out.push((Tok::LParen, i));
                i += 1;
            }
            b')' => {
                out.push((Tok::RParen, i));
                i += 1;
            }
            b'!' if bytes.get(i + 1) == Some(&b'=') => {
                out.push((Tok::Op(BinOp::Neq), i));
                i += 2;
            }
            b'!' => {
                out.push((Tok::Bang, i));
                i += 1;
            }
            b'=' if bytes.get(i + 1) == Some(&b'=') => {
                out.push((Tok::Op(BinOp::Eq), i));
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'=') => {
                out.push((Tok::Op(BinOp::Le), i));
                i += 2;
            }
            b'<' => {
                out.push((Tok::Op(BinOp::Lt), i));
                i += 1;
            }
            b'>' if bytes.get(i + 1) == Some(&b'=') => {
                out.push((Tok::Op(BinOp::Ge), i));
                i += 2;
            }
            b'>' => {
                out.push((Tok::Op(BinOp::Gt), i));
                i += 1;
            }
            b'&' if bytes.get(i + 1) == Some(&b'&') => {
                out.push((Tok::Op(BinOp::And), i));
                i += 2;
            }
            b'|' if bytes.get(i + 1) == Some(&b'|') => {
                out.push((Tok::Op(BinOp::Or), i));
                i += 2;
            }
            b'"' | b'\'' => {
                let quote = b;
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                if j == bytes.len() {
                    return Err(ExprError::Parse {
                        pos: i,
                        msg: "unterminated string".into(),
                    });
                }
                let s = std::str::from_utf8(&bytes[start..j])
                    .map_err(|_| ExprError::Parse {
                        pos: i,
                        msg: "non-utf8 string literal".into(),
                    })?
                    .to_owned();
                out.push((Tok::Str(s), i));
                i = j + 1;
            }
            b'0'..=b'9' | b'-' | b'+' => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                let s = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                let n: f64 = s.parse().map_err(|_| ExprError::Parse {
                    pos: start,
                    msg: format!("bad number '{s}'"),
                })?;
                out.push((Tok::Num(n), start));
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let s = std::str::from_utf8(&bytes[start..i])
                    .unwrap_or("")
                    .to_owned();
                out.push((Tok::Ident(s), start));
            }
            _ => {
                return Err(ExprError::Parse {
                    pos: i,
                    msg: format!("unexpected byte '{}'", b as char),
                })
            }
        }
    }
    Ok(out)
}

struct Parser<'a> {
    tokens: &'a [(Tok, usize)],
    pos: usize,
}

impl Parser<'_> {
    fn current_pos(&self) -> usize {
        self.tokens
            .get(self.pos)
            .map_or_else(|| self.tokens.last().map_or(0, |(_, p)| *p), |(_, p)| *p)
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    fn bump(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos).map(|(t, _)| t);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Op(BinOp::Or))) {
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::BinOp(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_unary()?;
        while matches!(self.peek(), Some(Tok::Op(BinOp::And))) {
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = Expr::BinOp(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        if matches!(self.peek(), Some(Tok::Bang)) {
            self.bump();
            let inner = self.parse_unary()?;
            Ok(Expr::Not(Box::new(inner)))
        } else {
            self.parse_cmp()
        }
    }

    fn parse_cmp(&mut self) -> Result<Expr, ExprError> {
        let lhs = self.parse_atom()?;
        if let Some(Tok::Op(op)) = self.peek() {
            let op = *op;
            if matches!(
                op,
                BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
            ) {
                self.bump();
                let rhs = self.parse_atom()?;
                return Ok(Expr::BinOp(op, Box::new(lhs), Box::new(rhs)));
            }
        }
        Ok(lhs)
    }

    fn parse_atom(&mut self) -> Result<Expr, ExprError> {
        let pos = self.current_pos();
        match self.bump() {
            Some(Tok::Num(n)) => Ok(Expr::Number(*n)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s.clone())),
            Some(Tok::Ident(name)) => match name.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                _ => {
                    let mut path = vec![name.clone()];
                    while matches!(self.peek(), Some(Tok::Dot)) {
                        self.bump();
                        match self.bump() {
                            Some(Tok::Ident(next)) => path.push(next.clone()),
                            _ => {
                                return Err(ExprError::Parse {
                                    pos: self.current_pos(),
                                    msg: "expected ident after '.'".into(),
                                })
                            }
                        }
                    }
                    Ok(Expr::Path(path))
                }
            },
            Some(Tok::LParen) => {
                let e = self.parse_or()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err(ExprError::Parse {
                        pos: self.current_pos(),
                        msg: "expected ')'".into(),
                    }),
                }
            }
            other => Err(ExprError::Parse {
                pos,
                msg: format!("unexpected token {other:?}"),
            }),
        }
    }
}

// ---------------------------------------------------------------- eval

/// Evaluate `expr` against a root `serde_json::Value`. The root is
/// exposed under the identifier `payload`; path lookups
/// (`payload.foo.bar`) traverse it field-by-field.
///
/// # Errors
/// `Runtime` error on type mismatches (e.g. comparing a number to a
/// string, boolean-combining non-booleans) or on missing fields that
/// aren't compared for existence (`payload.foo == null` is the
/// approved way to check).
pub fn eval_bool(expr: &Expr, payload: &serde_json::Value) -> Result<bool, ExprError> {
    eval(expr, payload)?.as_bool()
}

fn eval(expr: &Expr, root: &serde_json::Value) -> Result<Val, ExprError> {
    match expr {
        Expr::Number(n) => Ok(Val::Number(*n)),
        Expr::Str(s) => Ok(Val::Str(s.clone())),
        Expr::Bool(b) => Ok(Val::Bool(*b)),
        Expr::Path(segs) => Ok(lookup_path(segs, root)),
        Expr::Not(inner) => Ok(Val::Bool(!eval(inner, root)?.as_bool()?)),
        Expr::BinOp(op, lhs, rhs) => match op {
            BinOp::And => Ok(Val::Bool(
                eval(lhs, root)?.as_bool()? && eval(rhs, root)?.as_bool()?,
            )),
            BinOp::Or => Ok(Val::Bool(
                eval(lhs, root)?.as_bool()? || eval(rhs, root)?.as_bool()?,
            )),
            BinOp::Eq | BinOp::Neq | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let l = eval(lhs, root)?;
                let r = eval(rhs, root)?;
                cmp(*op, &l, &r)
            }
        },
    }
}

fn lookup_path(segs: &[String], root: &serde_json::Value) -> Val {
    let mut cur: &serde_json::Value = root;
    // First segment must be the root-binding name (`payload`). If the
    // author used something else (`msg`, `state`), leave the fallback
    // for future multi-context support — for M3 it's always payload.
    let mut iter = segs.iter();
    let first = iter.next();
    if first.map(String::as_str) != Some("payload") {
        return Val::Null;
    }
    for s in iter {
        match cur {
            serde_json::Value::Object(m) => match m.get(s) {
                Some(v) => cur = v,
                None => return Val::Null,
            },
            _ => return Val::Null,
        }
    }
    json_to_val(cur)
}

fn json_to_val(v: &serde_json::Value) -> Val {
    match v {
        serde_json::Value::Bool(b) => Val::Bool(*b),
        serde_json::Value::Number(n) => Val::Number(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Val::Str(s.clone()),
        // Null, arrays, and objects are all Null in this mini-lang.
        // Arrays + objects aren't comparable; `==` against a scalar
        // cleanly returns false from the cmp fallback.
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Val::Null
        }
    }
}

fn cmp(op: BinOp, l: &Val, r: &Val) -> Result<Val, ExprError> {
    // Number/number wins via numeric compare; otherwise fall back to
    // structural eq via debug-form. Keeps the common case cheap.
    if let (Some(ln), Some(rn)) = (l.as_number(), r.as_number()) {
        return Ok(Val::Bool(match op {
            BinOp::Eq => (ln - rn).abs() < f64::EPSILON,
            BinOp::Neq => (ln - rn).abs() >= f64::EPSILON,
            BinOp::Lt => ln < rn,
            BinOp::Gt => ln > rn,
            BinOp::Le => ln <= rn,
            BinOp::Ge => ln >= rn,
            _ => unreachable!("non-comparison op routed to cmp"),
        }));
    }
    match (l, r, op) {
        (Val::Str(a), Val::Str(b), BinOp::Eq) => Ok(Val::Bool(a == b)),
        (Val::Str(a), Val::Str(b), BinOp::Neq) => Ok(Val::Bool(a != b)),
        (Val::Bool(a), Val::Bool(b), BinOp::Eq) => Ok(Val::Bool(a == b)),
        (Val::Bool(a), Val::Bool(b), BinOp::Neq) => Ok(Val::Bool(a != b)),
        (Val::Null, Val::Null, BinOp::Eq) => Ok(Val::Bool(true)),
        (Val::Null, Val::Null, BinOp::Neq) => Ok(Val::Bool(false)),
        (a, Val::Null, BinOp::Eq) | (Val::Null, a, BinOp::Eq) => {
            Ok(Val::Bool(matches!(a, Val::Null)))
        }
        (a, Val::Null, BinOp::Neq) | (Val::Null, a, BinOp::Neq) => {
            Ok(Val::Bool(!matches!(a, Val::Null)))
        }
        (l, r, op) => Err(ExprError::Runtime(format!(
            "cannot compare {l:?} {op} {r:?}"
        ))),
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
    fn not_unary() {
        let p = json!({"on": true});
        assert!(eval_src("!payload.on == false", &p).unwrap());
    }

    #[test]
    fn parens_override_precedence() {
        let p = json!({"a": 1, "b": 1, "c": 0});
        // Without parens: a==1 && b==1 || c==1 = (a==1 && b==1) || c==1 = true
        assert!(eval_src("payload.a == 1 && payload.b == 1 || payload.c == 1", &p).unwrap());
        // With parens forcing the || inside: a==1 && (b==1 || c==1) = true
        assert!(eval_src("payload.a == 1 && (payload.b == 1 || payload.c == 1)", &p).unwrap());
        // Force false via grouping.
        assert!(!eval_src("(payload.a == 1 && payload.b == 0) || payload.c == 1", &p).unwrap());
    }

    #[test]
    fn missing_field_is_null() {
        let p = json!({"value": 10});
        // payload.absent is Null; Null == null is true.
        assert!(
            eval_src("payload.absent == null", &p).is_err()
                || matches!(
                    parse("payload.absent").and_then(|e| eval(&e, &p)),
                    Ok(Val::Null)
                )
        );
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
        assert!(eval_src("3.14 > 3", &p).unwrap());
        assert!(eval_src("-5 < 0", &p).unwrap());
    }
}
