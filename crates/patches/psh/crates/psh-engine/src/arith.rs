//! Arithmetic over the shell's variables: psh-syntax's evaluator (`eval_arith`) with
//! the vproc as its environment, arrays included.

use std::io::Write;
use std::rc::Rc;

use psh_syntax::{
    ArithEnv, ArithExpr, ArithIndex, ArithValue, EvalError, Part, Word, eval_arith,
    eval_arith_expr, parse_arith, plain_decimal,
};

use crate::state::{Code, Vp};
use crate::vars::Value;

struct Env<'a> {
    vp: &'a mut Vp,
}

impl Env<'_> {
    /// `set -u`: reading a variable that is not set is an error that ends the shell
    /// (`Vp::arith_unbound` tells the caller which kind of error it holds).
    fn unbound(&mut self, name: &[u8]) -> Result<(), String> {
        if !self.vp.opts.get(crate::options::SetOpt::Nounset) {
            return Ok(());
        }
        self.vp.arith_unbound = true;
        Err(format!(
            "{}: unbound variable",
            String::from_utf8_lossy(name)
        ))
    }
}

impl Env<'_> {
    fn unset_base(&mut self, name: &str, v: Option<Vec<u8>>) -> Result<Option<Vec<u8>>, String> {
        if v.is_none()
            && self.vp.var_ref(name).is_none_or(|var| {
                matches!(var.value, Value::Unset)
                    || (var.declared_only && var.get().is_none() && var.values().is_empty())
            })
        {
            self.unbound(name.as_bytes())?;
        }
        Ok(v)
    }
}

impl ArithEnv for Env<'_> {
    type Error = String;

    fn is_assoc(&mut self, name: &[u8]) -> bool {
        let name = String::from_utf8_lossy(name);
        self.vp.var_ref(&name).is_some_and(|v| v.is_assoc())
    }

    fn get(
        &mut self,
        name: &[u8],
        index: Option<ArithIndex<'_>>,
    ) -> Result<Option<Vec<u8>>, String> {
        let name = String::from_utf8_lossy(name).into_owned();
        match index {
            None => match self.vp.target(&name)? {
                (n, Some(sub)) => self.vp.get_elem(&n, &sub),
                (n, None) => {
                    if n == "BASHPID" && self.vp.bashpid.is_none() {
                        return Err(crate::expand::pid_error(self.vp, b"BASHPID"));
                    }
                    self.vp.refresh_dynamic(&n);
                    match self.vp.vars.value(&n).map(<[u8]>::to_vec) {
                        Some(value) => Ok(Some(value)),
                        None => self.unbound(n.as_bytes()).map(|()| None),
                    }
                }
            },
            Some(index) => {
                let v = match index {
                    ArithIndex::Key(k) => self.vp.get_elem(&name, &unquote(k)),
                    ArithIndex::Num(i) => self.vp.get_elem(&name, i.to_string().as_bytes()),
                    ArithIndex::Empty => {
                        self.vp.arith_notes.push(format!("{name}[]: bad array subscript"));
                        self.vp.get_elem(&name, b"0")
                    }
                };
                // Out of range below the start: 0, with bash's message.
                let v = match v {
                    Err(msg) if msg.ends_with(": bad array subscript") => {
                        self.vp.arith_notes.push(msg);
                        None
                    }
                    v => v?,
                };
                self.unset_base(&name, v)
            }
        }
    }

    /// A variable that is neither a reference nor computed is read where it is: a loop
    /// counter costs no copy of its name or its digits.
    fn load(&mut self, name: &[u8], index: Option<ArithIndex<'_>>) -> Result<ArithValue, String> {
        if index.is_none()
            && let Ok(n) = std::str::from_utf8(name)
            && n != "BASHPID"
            && let Some(var) = self.vp.vars.get(n)
            && !var.nameref
            && var.dynamic.is_none()
        {
            let value = var.get().map(|v| match plain_decimal(v) {
                Some(n) => ArithValue::Num(n),
                None => ArithValue::Text(v.to_vec()),
            });
            return match value {
                Some(value) => Ok(value),
                None => self.unbound(name).map(|()| ArithValue::Unset),
            };
        }
        Ok(match self.get(name, index)? {
            None => ArithValue::Unset,
            Some(v) => match plain_decimal(&v) {
                Some(n) => ArithValue::Num(n),
                None => ArithValue::Text(v),
            },
        })
    }

    fn set(
        &mut self,
        name: &[u8],
        index: Option<ArithIndex<'_>>,
        value: i64,
    ) -> Result<(), String> {
        if self.vp.arith_subst.mode != Mode::Off {
            self.record_write(name);
        }
        // A plain scalar takes the digits in the buffer it has: what `assign` does for
        // it, attribute by attribute, without a new name or value.
        if index.is_none()
            && let Ok(n) = std::str::from_utf8(name)
        {
            let allexport = self.vp.opts.get(crate::options::SetOpt::Allexport);
            if let Some(var) = self.vp.vars.get_mut(n)
                && !var.nameref
                && !var.readonly
                && var.dynamic.is_none()
                && let Value::Str(s) = &mut var.value
            {
                s.clear();
                let _ = write!(s, "{value}");
                var.declared_only = false;
                var.exported |= allexport;
                return Ok(());
            }
        }
        let name = String::from_utf8_lossy(name).into_owned();
        let v = value.to_string().into_bytes();
        match index {
            None => self.vp.assign(&name, v),
            Some(ArithIndex::Key(k)) => self.vp.assign_elem(&name, &unquote(k), v, false),
            Some(ArithIndex::Num(i)) => {
                self.vp
                    .assign_elem(&name, i.to_string().as_bytes(), v, false)
            }
            // bash reports it and goes on, with nothing stored.
            Some(ArithIndex::Empty) => {
                self.vp.arith_notes.push(format!("{name}[]: bad array subscript"));
                self.vp.arith_notes.push(format!("`{name}[]': not a valid identifier"));
                Ok(())
            }
        }
    }

    fn expand_subscript(&mut self, sub: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let s = &mut self.vp.arith_subst;
        if s.mode == Mode::Off {
            return Ok(None);
        }
        if s.pending.is_some() {
            return Err(String::new());
        }
        if let Some((text, value)) = s.done.get(s.next) {
            if text == sub {
                s.next += 1;
                return Ok(Some(value.clone()));
            }
            // The evaluation went another way than the last time: what was expanded
            // from here on belongs to that one.
            s.done.truncate(s.next);
        }
        // Writes made before this point that cannot be undone: no second run.
        if s.writes > 0 && s.journal.is_none() {
            return Ok(None);
        }
        s.pending = Some(sub.to_vec());
        if s.mode == Mode::Probe {
            let notes = s.notes_at;
            self.vp.arith_notes.truncate(notes);
        }
        Err(String::new())
    }
}

impl Env<'_> {
    /// Counts a write while subscripts may be expanded, and on a second run keeps the
    /// variable's value from before it, for [`expanding`] to put back.
    fn record_write(&mut self, name: &[u8]) {
        let s = &mut self.vp.arith_subst;
        s.writes += 1;
        if s.journal.is_none() {
            return;
        }
        let name = String::from_utf8_lossy(name);
        let target = self.vp.resolve(&name);
        let base = target.split('[').next().unwrap_or_default().to_string();
        let old = self.vp.vars.get(&base).cloned();
        if let Some(journal) = &mut self.vp.arith_subst.journal
            && !journal.iter().any(|(n, _)| *n == base)
        {
            journal.push((base, old));
        }
    }
}

/// Whether an evaluation may stop at a subscript that needs expanding (`a[$(cmd)]`
/// reached through a variable's value), which needs the async part of the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    /// No: the subscript is evaluated as it is (sync callers with no way back).
    #[default]
    Off,
    /// A fast path's evaluation: it stops, and the caller takes its slow path.
    Probe,
    /// Inside [`expanding`], which expands the subscript and runs it again.
    Active,
}

/// The state of [`expanding`] and [`probe`], on the vproc.
#[derive(Clone, Default)]
pub(crate) struct Subst {
    pub(crate) mode: Mode,
    /// Subscripts expanded so far, in the order the evaluation reaches them.
    done: Vec<(Vec<u8>, Vec<u8>)>,
    /// How many of `done` this run has used.
    next: usize,
    /// The subscript this run stopped at.
    pending: Option<Vec<u8>>,
    /// Variable writes in this run.
    writes: u32,
    /// On a second run: each written variable's value from before the run.
    journal: Option<Vec<(String, Option<crate::vars::Var>)>>,
    notes_at: usize,
}

/// Runs a fast path's evaluation `f`; `None` when it stopped at a subscript that needs
/// expanding, before any write, so the caller can take its slow path instead.
#[inline]
pub(crate) fn probe<T>(vp: &mut Vp, f: impl FnOnce(&mut Vp) -> T) -> Option<T> {
    if !vp.core.arith_subst_gate.get() || vp.arith_subst.mode != Mode::Off {
        return Some(f(vp));
    }
    vp.arith_subst.mode = Mode::Probe;
    vp.arith_subst.writes = 0;
    vp.arith_subst.notes_at = vp.arith_notes.len();
    let r = f(vp);
    let stopped = vp.arith_subst.pending.take().is_some();
    vp.arith_subst.mode = Mode::Off;
    (!stopped).then_some(r)
}

/// Runs `f`, whose arithmetic expands the subscripts it reaches in variables' values
/// as bash does (`x='a[$(echo 2)]'; echo $((x))`): an evaluation that reaches one stops
/// there, the subscript is expanded, and `f` runs again from the start with the
/// variables it wrote put back, taking the expansions made so far in order.
///
/// Called as `expanding!(vp, |vp| …)`, which awaits it only once the gate is on
/// (`gate`) and otherwise just runs `f`, so the callers' futures stay as they were.
macro_rules! expanding {
    ($vp:expr, $f:expr) => {{
        let vp: &mut crate::state::Vp = $vp;
        let mut f = crate::arith::closure($f);
        if vp.core.arith_subst_gate.get() {
            Box::pin(crate::arith::expanding_gated(vp, f)).await
        } else {
            Ok::<_, crate::state::Unwind>(f(vp))
        }
    }};
}
pub(crate) use expanding;

/// Gives `expanding!`'s closure its argument type.
pub(crate) fn closure<T, F: FnMut(&mut Vp) -> T>(f: F) -> F {
    f
}

pub(crate) async fn expanding_gated<T>(
    vp: &mut Vp,
    mut f: impl FnMut(&mut Vp) -> T,
) -> Result<T, crate::state::Unwind> {
    if vp.arith_subst.mode != Mode::Off {
        return Ok(f(vp));
    }
    let notes = vp.arith_notes.len();
    let mut st = Subst {
        mode: Mode::Active,
        ..Subst::default()
    };
    for _ in 0..1024 {
        st.next = 0;
        st.writes = 0;
        vp.arith_subst = st;
        let r = f(vp);
        st = std::mem::take(&mut vp.arith_subst);
        let Some(text) = st.pending.take() else {
            return Ok(r);
        };
        if let Some(journal) = st.journal.take() {
            for (name, old) in journal.into_iter().rev() {
                match old {
                    Some(var) => vp.vars.insert(&name, var),
                    None => {
                        vp.vars.remove(&name);
                    }
                }
            }
        }
        st.journal = Some(Vec::new());
        vp.arith_notes.truncate(notes);
        let value = expand_subscript(vp, &text).await?;
        st.done.push((text, value));
    }
    Ok(f(vp))
}

/// A subscript's expansions, as the word of an arithmetic command.
async fn expand_subscript(vp: &mut Vp, text: &[u8]) -> Result<Vec<u8>, crate::state::Unwind> {
    let mut src = b"((".to_vec();
    src.extend_from_slice(text);
    src.extend_from_slice(b"))");
    let Ok(ast) = psh_syntax::parse_bytes(&src, psh_syntax::Options::default()) else {
        return Ok(text.to_vec());
    };
    let word = ast
        .list(ast.root())
        .items
        .first()
        .and_then(|item| item.first.stages.first())
        .and_then(|stage| match &ast.cmd(stage.cmd).kind {
            psh_syntax::CommandKind::Arith(w) => Some(w.clone()),
            _ => None,
        });
    let Some(word) = word else {
        return Ok(text.to_vec());
    };
    let code = Rc::new(Code {
        lines: Rc::new(psh_syntax::LineIndex::from_bytes(&src)),
        ast: std::sync::Arc::new(ast),
        line_base: vp.line.saturating_sub(1),
        arith: Default::default(),
        flat: Default::default(),
    });
    let (value, _) = Box::pin(crate::expand::expand_single(
        vp,
        &code,
        &word,
        crate::expand::Ctx::Arith,
    ))
    .await?;
    Ok(value)
}

/// Turns on [`expanding`] and [`probe`] for the rest of the run once shell text
/// (a script chunk, `eval` or `source` text, a backquote body) could make a value
/// with a subscript to expand (`could_expand_subscript`); until then they run the
/// plain evaluator, so arithmetic pays one branch.
pub(crate) fn gate(core: &crate::state::Core, text: &[u8]) {
    if !core.arith_subst_gate.get() && could_expand_subscript(text) {
        core.arith_subst_gate.set(true);
    }
}

/// Whether shell text holds a literal `$` or backquote after a `[` in the same word
/// with no `]` between: one in single quotes (`'a[$(cmd)]'`, `$'…'`) or escaped
/// with a backslash (`a[\$i]`), which is how a value such as `a[$(cmd)]` is written.
/// An expanded `$` (`${a[$i]}`, `$((a[$i]))`, `[[ $i -lt 3 ]]`) does not count.
pub(crate) fn could_expand_subscript(text: &[u8]) -> bool {
    let (mut bracket, mut squote, mut dquote, mut i) = (false, false, false, 0);
    while i < text.len() {
        let b = text[i];
        if squote {
            match b {
                b'\'' => squote = false,
                b'$' | b'`' if bracket => return true,
                b'[' => bracket = true,
                b']' => bracket = false,
                _ => {}
            }
        } else {
            match b {
                b'\\' => {
                    if bracket && matches!(text.get(i + 1), Some(b'$' | b'`')) {
                        return true;
                    }
                    i += 1;
                }
                b'\'' if !dquote => squote = true,
                b'"' => dquote = !dquote,
                b'[' => bracket = true,
                b']' => bracket = false,
                b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'(' | b')' if !dquote => {
                    bracket = false
                }
                _ => {}
            }
        }
        i += 1;
    }
    false
}

fn unquote(k: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if !k.iter().any(|&b| matches!(b, b'\'' | b'"' | b'\\')) {
        return std::borrow::Cow::Borrowed(k);
    }
    let mut out = Vec::with_capacity(k.len());
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    while i < k.len() {
        let b = k[i];
        match b {
            b'\'' if !double => single = !single,
            b'"' if !single => double = !double,
            b'\\' if !single && i + 1 < k.len() => {
                i += 1;
                out.push(k[i]);
            }
            _ => out.push(b),
        }
        i += 1;
    }
    std::borrow::Cow::Owned(out)
}

pub(crate) async fn notes(vp: &mut Vp) {
    for msg in std::mem::take(&mut vp.arith_notes) {
        vp.error(&msg).await;
    }
}

/// Evaluates `text`; the error is bash's message without the `line N:` prefix.
pub(crate) fn eval(vp: &mut Vp, text: &[u8]) -> Result<i64, String> {
    vp.arith_unbound = false;
    vp.arith_syntax = false;
    match eval_arith(text, &mut Env { vp }) {
        Ok(v) => Ok(v),
        Err(EvalError::Env(msg)) => Err(msg),
        Err(e) => {
            vp.arith_syntax = true;
            Err(e.bash_text().unwrap_or_default())
        }
    }
}

/// An arithmetic word without expansions: its text, and the expression parsed from it
/// (`None` for blank text, which is 0).
pub(crate) struct Literal {
    pub text: Vec<u8>,
    expr: Option<ArithExpr>,
}

/// The parsed form of an arithmetic word that is literal text only, kept with the
/// script so a loop expands and parses it once. `None` sends the word down the usual
/// path: it has an expansion, a `~` (which may be a tilde prefix), or does not parse
/// (the error is reported there).
pub(crate) fn literal(code: &Code, word: &Word) -> Option<Rc<Literal>> {
    let key = (word.span.start, word.span.end);
    if let Some(known) = code.arith.borrow().get(&key) {
        return Some(known.clone());
    }
    if !word.parts.iter().all(|p| matches!(p.kind, Part::Lit)) {
        return None;
    }
    let mut text = Vec::new();
    for p in &word.parts {
        text.extend_from_slice(code.ast.bytes(p.span));
    }
    if text.contains(&b'~') {
        return None;
    }
    let expr = parse_arith(&text).ok()?;
    let lit = Rc::new(Literal { text, expr });
    code.arith.borrow_mut().insert(key, lit.clone());
    Some(lit)
}

impl Literal {
    /// The parsed expression (`None` for blank text).
    pub(crate) fn expr(&self) -> Option<&ArithExpr> {
        self.expr.as_ref()
    }
}

/// Evaluates a [`Literal`], as [`eval`] evaluates its text.
pub(crate) fn eval_literal(vp: &mut Vp, lit: &Literal) -> Result<i64, String> {
    vp.arith_unbound = false;
    let Some(expr) = &lit.expr else {
        return Ok(0);
    };
    match eval_arith_expr(expr, &lit.text, &mut Env { vp }) {
        Ok(v) => Ok(v),
        Err(EvalError::Env(msg)) => Err(msg),
        Err(e) => Err(e.bash_text().unwrap_or_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::could_expand_subscript as could;

    #[test]
    fn plain_arithmetic_keeps_the_gate_off() {
        for s in [
            "i=0; while [ $i -lt 100000 ]; do i=$((i+1)); done",
            "i=0; while ((i<100000)); do ((i++)); done",
            "i=0; while [[ $i -lt 100000 ]]; do i=$((i+1)); done",
            "a=(1 2 3); echo \"${a[$i]}\" $((a[$i] + 1)); a[$i]=2; x=\"${a[${#a[@]}-1]}\"",
            "echo '[ok]' '$HOME' \"[$x]\"; echo \\$5 [ \\$x",
        ] {
            assert!(!could(s.as_bytes()), "{s}");
        }
    }

    #[test]
    fn a_written_subscript_turns_it_on() {
        for s in [
            "x='a[$(echo 2)]'; echo $((x))",
            "x='a[`echo 2`]'",
            "expr='a[ $(echo 2) ]'",
            "x=a[\\$i]",
            "x=\"a[\\$(echo 2)]\"",
            "x=$'a[$i]'",
        ] {
            assert!(could(s.as_bytes()), "{s}");
        }
    }
}
