//! Evaluation of arithmetic expressions over an environment of variables, as bash does
//! it: 64-bit wrapping math, short-circuit `&&` `||` `?:`, variable values evaluated as
//! arithmetic in turn (up to bash's recursion limit), and bash's runtime errors.
//!
//! The environment owns variable semantics (scalars, indexed and associative arrays,
//! negative indexes, `set -u`, readonly variables); the evaluator only asks it for values
//! and hands it new ones.

use alloc::string::String;
use alloc::vec::Vec;

use crate::arith::{
    ArithError, ArithErrorKind, ArithExpr, ArithKind, BinOp, LValue, UnaryOp, parse_arith,
};

/// Bash refuses to evaluate variable values nested this deep (`a=b b=c …`).
pub const ARITH_RECURSION_LIMIT: u32 = 1024;

/// A subscript, after the evaluator resolved it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithIndex<'a> {
    /// The subscript of an indexed array, evaluated (may be negative: from the end).
    Num(i64),
    /// The key of an associative array: the subscript text as written.
    Key(&'a [u8]),
    /// An indexed array's subscript that is empty (`a[]`, or `a[$k]` with `k` empty):
    /// read as element 0, but a store to it is a bad subscript that stores nothing.
    Empty,
}

/// A variable's value as the evaluator reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArithValue {
    /// Unset: counts as 0.
    Unset,
    /// A plain decimal number ([`plain_decimal`]).
    Num(i64),
    /// Any other text: evaluated as arithmetic in turn.
    Text(Vec<u8>),
}

/// What the evaluator needs from the shell.
pub trait ArithEnv {
    /// The shell's error type (unbound variable, readonly variable, …).
    type Error;

    /// Whether `name` is an associative array, whose subscripts are keys rather than
    /// arithmetic.
    fn is_assoc(&mut self, name: &[u8]) -> bool;

    /// The value of a variable (`index` `None`) or array element; `Ok(None)` when it is
    /// unset, which counts as 0. Under `set -u` return the "unbound variable" error.
    fn get(
        &mut self,
        name: &[u8],
        index: Option<ArithIndex<'_>>,
    ) -> Result<Option<Vec<u8>>, Self::Error>;

    /// What [`ArithEnv::get`] returns, read: an environment that can look at the value
    /// in place overrides this and copies only text that is not a plain number.
    fn load(
        &mut self,
        name: &[u8],
        index: Option<ArithIndex<'_>>,
    ) -> Result<ArithValue, Self::Error> {
        Ok(match self.get(name, index)? {
            None => ArithValue::Unset,
            Some(v) => match plain_decimal(&v) {
                Some(n) => ArithValue::Num(n),
                None => ArithValue::Text(v),
            },
        })
    }

    /// Assigns a value (the result of `=`, `+=`, `++`, …).
    fn set(
        &mut self,
        name: &[u8],
        index: Option<ArithIndex<'_>>,
        value: i64,
    ) -> Result<(), Self::Error>;

    /// The subscript of an indexed array with its expansions done (`$x`, `$(…)`,
    /// backquotes), asked only for text that holds a `$` or a backquote: bash expands a
    /// subscript it reaches inside a variable's value before it evaluates it.
    /// `Ok(None)` evaluates the text as it is.
    fn expand_subscript(&mut self, _sub: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(None)
    }
}

/// An evaluation failure: an arithmetic error in some text (the expression itself, a
/// variable's value or a subscript), or the environment's own error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError<E> {
    Arith { error: ArithError, text: Vec<u8> },
    Env(E),
}

impl<E> EvalError<E> {
    /// Bash's message for an arithmetic error (see [`ArithError::bash_text`]), without
    /// the caller's context prefix; `None` for environment errors.
    pub fn bash_text(&self) -> Option<String> {
        match self {
            EvalError::Arith { error, text } => Some(error.bash_text(text)),
            EvalError::Env(_) => None,
        }
    }
}

/// Parses and evaluates `text` (blank text is 0).
pub fn eval_arith<E: ArithEnv>(text: &[u8], env: &mut E) -> Result<i64, EvalError<E::Error>> {
    Evaluator {
        env,
        depth: 0,
        nest: 0,
    }
    .text(text)
}

/// Evaluates an expression parsed from `text` with [`parse_arith`].
pub fn eval_arith_expr<E: ArithEnv>(
    expr: &ArithExpr,
    text: &[u8],
    env: &mut E,
) -> Result<i64, EvalError<E::Error>> {
    Evaluator {
        env,
        depth: 0,
        nest: 0,
    }
    .expr(expr, text)
}

struct Evaluator<'e, E> {
    env: &'e mut E,
    /// Nesting of variable values being evaluated (bash's recursion level).
    depth: u32,
    /// Nesting of `expr` calls across all texts: values nested deep inside nested
    /// values would otherwise overflow the stack before bash's limit is reached.
    nest: u32,
}

/// A bound on the evaluator's own recursion (1024 levels of `x=x` take one each); fits
/// a 1 MiB stack in release builds (tested). Values nested deep inside nested values
/// reach it before bash's level limit and get the same error.
const MAX_NEST: u32 = 2048;

fn err<E>(kind: ArithErrorKind, token: u32, text: &[u8]) -> EvalError<E> {
    EvalError::Arith {
        error: ArithError::new(kind, token, None),
        text: text.to_vec(),
    }
}

impl<E: ArithEnv> Evaluator<'_, E> {
    fn text(&mut self, text: &[u8]) -> Result<i64, EvalError<E::Error>> {
        match parse_arith(text) {
            Ok(Some(e)) => self.expr(&e, text),
            Ok(None) => Ok(0),
            Err(error) => Err(EvalError::Arith {
                error,
                text: text.to_vec(),
            }),
        }
    }

    fn index<'t>(
        &mut self,
        lv: &LValue,
        text: &'t [u8],
    ) -> Result<Option<ArithIndex<'t>>, EvalError<E::Error>> {
        let Some(sub) = lv.index else {
            return Ok(None);
        };
        let sub = &text[sub.range()];
        if self.env.is_assoc(&text[lv.name.range()]) {
            Ok(Some(ArithIndex::Key(sub)))
        } else if sub.iter().any(|&b| matches!(b, b'$' | b'`'))
            && let Some(expanded) = self.env.expand_subscript(sub).map_err(EvalError::Env)?
        {
            match expanded.iter().all(u8::is_ascii_whitespace) {
                true => Ok(Some(ArithIndex::Empty)),
                false => Ok(Some(ArithIndex::Num(self.text(&expanded)?))),
            }
        } else if sub.iter().all(u8::is_ascii_whitespace) {
            Ok(Some(ArithIndex::Empty))
        } else {
            Ok(Some(ArithIndex::Num(self.text(sub)?)))
        }
    }

    /// The value of a variable: its text, evaluated as arithmetic.
    fn load(&mut self, lv: &LValue, text: &[u8]) -> Result<i64, EvalError<E::Error>> {
        let index = self.index(lv, text)?;
        if self.depth + 1 >= ARITH_RECURSION_LIMIT {
            return Err(err(ArithErrorKind::RecursionLevel, lv.span.start, text));
        }
        let value = self
            .env
            .load(&text[lv.name.range()], index)
            .map_err(EvalError::Env)?;
        let value = match value {
            ArithValue::Unset => return Ok(0),
            // The common case: a plain decimal number needs no parse.
            ArithValue::Num(n) => return Ok(n),
            ArithValue::Text(value) => value,
        };
        self.depth += 1;
        let r = self.text(&value);
        self.depth -= 1;
        r
    }

    fn store(&mut self, lv: &LValue, text: &[u8], value: i64) -> Result<(), EvalError<E::Error>> {
        let index = self.index(lv, text)?;
        self.env
            .set(&text[lv.name.range()], index, value)
            .map_err(EvalError::Env)
    }

    fn expr(&mut self, e: &ArithExpr, text: &[u8]) -> Result<i64, EvalError<E::Error>> {
        if self.nest >= MAX_NEST {
            return Err(err(ArithErrorKind::RecursionLevel, e.span.start, text));
        }
        self.nest += 1;
        let r = self.expr_inner(e, text);
        self.nest -= 1;
        r
    }

    fn expr_inner(&mut self, e: &ArithExpr, text: &[u8]) -> Result<i64, EvalError<E::Error>> {
        Ok(match &e.kind {
            ArithKind::Num(n) => *n,
            ArithKind::Var(lv) => self.load(lv, text)?,
            ArithKind::Unary { op, expr } => {
                let v = self.expr(expr, text)?;
                match op {
                    UnaryOp::Plus => v,
                    UnaryOp::Minus => v.wrapping_neg(),
                    UnaryOp::Not => (v == 0) as i64,
                    UnaryOp::BitNot => !v,
                }
            }
            ArithKind::IncDec { target, inc, post } => {
                let old = self.load(target, text)?;
                let new = if *inc {
                    old.wrapping_add(1)
                } else {
                    old.wrapping_sub(1)
                };
                self.store(target, text, new)?;
                if *post { old } else { new }
            }
            ArithKind::Binary { op, lhs, rhs } => match op {
                BinOp::And | BinOp::Or => {
                    let a = self.expr(lhs, text)? != 0;
                    if a == (*op == BinOp::And) {
                        (self.expr(rhs, text)? != 0) as i64
                    } else {
                        self.skipped(rhs, text)?;
                        a as i64
                    }
                }
                BinOp::Comma => {
                    self.expr(lhs, text)?;
                    self.expr(rhs, text)?
                }
                _ => {
                    let a = self.expr(lhs, text)?;
                    let b = self.expr(rhs, text)?;
                    binary(*op, a, b, rhs, text)?
                }
            },
            ArithKind::Cond {
                cond,
                then,
                otherwise,
            } => {
                if self.expr(cond, text)? != 0 {
                    let v = self.expr(then, text)?;
                    self.skipped(otherwise, text)?;
                    v
                } else {
                    self.skipped(then, text)?;
                    self.expr(otherwise, text)?
                }
            }
            ArithKind::Assign { target, op, value } => {
                // `x op= v` reads x before evaluating v, as bash does (`x += x++`).
                let old = match op {
                    Some(_) => Some(self.load(target, text)?),
                    None => None,
                };
                let v = self.expr(value, text)?;
                let new = match (op, old) {
                    (Some(op), Some(old)) => binary(*op, old, v, value, text)?,
                    _ => v,
                };
                self.store(target, text, new)?;
                new
            }
            ArithKind::Fail { expr, error } => {
                self.expr(expr, text)?;
                return Err(EvalError::Arith {
                    error: error.clone(),
                    text: text.to_vec(),
                });
            }
        })
    }
}

impl<E: ArithEnv> Evaluator<'_, E> {
    /// Walks a branch bash does not evaluate (the dead side of `&&`, `||` and `?:`),
    /// as bash's `noeval` does: variables read as 0, nothing is assigned and division
    /// by 0 is not an error, but a negative exponent still is (`1 ? 1 : 2 ** -3`).
    fn skipped(&mut self, e: &ArithExpr, text: &[u8]) -> Result<i64, EvalError<E::Error>> {
        if self.nest >= MAX_NEST {
            return Err(err(ArithErrorKind::RecursionLevel, e.span.start, text));
        }
        self.nest += 1;
        let r = self.skipped_inner(e, text);
        self.nest -= 1;
        r
    }

    fn skipped_inner(&mut self, e: &ArithExpr, text: &[u8]) -> Result<i64, EvalError<E::Error>> {
        let lenient = |op: BinOp, a: i64, b: i64, rhs: &ArithExpr| {
            let b = if matches!(op, BinOp::Div | BinOp::Rem) && b == 0 { 1 } else { b };
            binary(op, a, b, rhs, text)
        };
        Ok(match &e.kind {
            ArithKind::Num(n) => *n,
            ArithKind::Var(_) => 0,
            ArithKind::Unary { op, expr } => {
                let v = self.skipped(expr, text)?;
                match op {
                    UnaryOp::Plus => v,
                    UnaryOp::Minus => v.wrapping_neg(),
                    UnaryOp::Not => (v == 0) as i64,
                    UnaryOp::BitNot => !v,
                }
            }
            ArithKind::IncDec { inc, post, .. } => match (post, inc) {
                (true, _) => 0,
                (false, true) => 1,
                (false, false) => -1,
            },
            ArithKind::Binary { op, lhs, rhs } => {
                let a = self.skipped(lhs, text)?;
                let b = self.skipped(rhs, text)?;
                match op {
                    BinOp::And => (a != 0 && b != 0) as i64,
                    BinOp::Or => (a != 0 || b != 0) as i64,
                    BinOp::Comma => b,
                    _ => lenient(*op, a, b, rhs)?,
                }
            }
            ArithKind::Cond {
                cond,
                then,
                otherwise,
            } => {
                let c = self.skipped(cond, text)?;
                let t = self.skipped(then, text)?;
                let o = self.skipped(otherwise, text)?;
                if c != 0 { t } else { o }
            }
            ArithKind::Assign { op, value, .. } => {
                let v = self.skipped(value, text)?;
                match op {
                    Some(op) => lenient(*op, 0, v, value)?,
                    None => v,
                }
            }
            ArithKind::Fail { error, .. } => {
                return Err(EvalError::Arith {
                    error: error.clone(),
                    text: text.to_vec(),
                });
            }
        })
    }
}

/// A binary operation on values. `rhs` locates the error token of runtime errors, as
/// bash reports it: the start of the divisor for `/` and `%`; for `**`, the token after
/// the exponent (its last token at the end of the text).
fn binary<E>(op: BinOp, a: i64, b: i64, rhs: &ArithExpr, text: &[u8]) -> Result<i64, EvalError<E>> {
    Ok(match op {
        BinOp::Pow => {
            if b < 0 {
                let mut next = rhs.span.end as usize;
                while text
                    .get(next)
                    .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n'))
                {
                    next += 1;
                }
                let token = if next < text.len() {
                    next as u32
                } else {
                    rhs.last
                };
                return Err(err(ArithErrorKind::NegativeExponent, token, text));
            }
            let (mut base, mut exp, mut acc) = (a, b as u64, 1i64);
            while exp > 0 {
                if exp & 1 == 1 {
                    acc = acc.wrapping_mul(base);
                }
                base = base.wrapping_mul(base);
                exp >>= 1;
            }
            acc
        }
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div | BinOp::Rem => {
            if b == 0 {
                return Err(err(ArithErrorKind::DivisionByZero, rhs.span.start, text));
            }
            if op == BinOp::Div {
                a.wrapping_div(b)
            } else {
                a.wrapping_rem(b)
            }
        }
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        // Shift counts are taken modulo 64.
        BinOp::Shl => a.wrapping_shl(b as u32),
        BinOp::Shr => a.wrapping_shr(b as u32),
        BinOp::Lt => (a < b) as i64,
        BinOp::Le => (a <= b) as i64,
        BinOp::Gt => (a > b) as i64,
        BinOp::Ge => (a >= b) as i64,
        BinOp::Eq => (a == b) as i64,
        BinOp::Ne => (a != b) as i64,
        BinOp::BitAnd => a & b,
        BinOp::BitXor => a ^ b,
        BinOp::BitOr => a | b,
        BinOp::And => (a != 0 && b != 0) as i64,
        BinOp::Or => (a != 0 || b != 0) as i64,
        BinOp::Comma => b,
    })
}

/// `[-]digits` without a leading zero (which would be octal) and short enough not to
/// wrap: parse it directly.
pub fn plain_decimal(v: &[u8]) -> Option<i64> {
    let (neg, digits) = match v.first() {
        Some(b'-') => (true, &v[1..]),
        _ => (false, v),
    };
    if digits.is_empty() || digits.len() > 18 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    let mut n: i64 = 0;
    for &d in digits {
        n = n * 10 + (d - b'0') as i64;
    }
    Some(if neg { -n } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Variables that are all unset, and assignments that fail the test.
    struct Empty;

    impl ArithEnv for Empty {
        type Error = ();
        fn is_assoc(&mut self, _: &[u8]) -> bool {
            false
        }
        fn get(&mut self, _: &[u8], _: Option<ArithIndex<'_>>) -> Result<Option<Vec<u8>>, ()> {
            Ok(None)
        }
        fn set(&mut self, _: &[u8], _: Option<ArithIndex<'_>>, _: i64) -> Result<(), ()> {
            Err(())
        }
    }

    fn eval(s: &str) -> Result<i64, EvalError<()>> {
        eval_arith(s.as_bytes(), &mut Empty)
    }

    #[test]
    fn a_dead_branch_assigns_nothing_and_divides_by_zero_freely() {
        assert_eq!(eval("1 ? 1 : (n = 2)"), Ok(1));
        assert_eq!(eval("0 && n++"), Ok(0));
        assert_eq!(eval("1 || 1 / 0"), Ok(1));
        assert_eq!(eval("0 ? 1 % 0 : 3"), Ok(3));
        assert_eq!(eval("1 ? 1 : 2 ** n"), Ok(1));
    }

    #[test]
    fn syntax_errors_read_as_bash_5_2_words_them() {
        let e = eval("1 +").unwrap_err();
        assert_eq!(
            e.bash_text().as_deref(),
            Some("1 +: syntax error: operand expected (error token is \"+\")")
        );
    }

    #[test]
    fn a_dead_branch_still_refuses_a_negative_exponent() {
        for s in ["1 ? 1 : 2 ** -3", "7 ? 1 : 2 ** --n", "0 && 2 ** -1", "1 || 0 ? 2 ** -1 : 0"] {
            let e = eval(s).unwrap_err();
            assert!(
                matches!(&e, EvalError::Arith { error, .. } if error.kind == ArithErrorKind::NegativeExponent),
                "{s}: {e:?}"
            );
        }
    }
}
