//! Loops compiled once to flat code.
//!
//! A while, until or for ((...)) loop is compiled, the first time it runs, into a flat
//! list of instructions with jumps: its conditions and bodies, if, { ... }, && / || and
//! nested loops become control flow, and the commands whose effect the compiler
//! understands become native operations on integer variable slots: (( expr )),
//! name=$(( expr )) / name=N / name=$other, [ a -lt b ] / test, [[ a -lt b ]],
//! true / false / :, break / continue. Every other command is run by the interpreter,
//! as run_list would run it.
//!
//! The variables native operations touch are resolved to slots at compile time. While
//! the loop runs, a slot holds the variable's integer value, read once when the loop
//! starts; native operations read and write slots and nothing else. Before anything
//! else runs (an interpreted command, errexit, a signal, a yield to the scheduler) the
//! changed slots are written back to the variables, with $_, PIPESTATUS and the line
//! the native operations left; after it, the slots are read again. A slot whose
//! variable is not a plain decimal integer (an array, a reference, a computed or
//! read-only variable, text) is Other: an operation that touches it, or that would fail
//! (division by zero), is run by the interpreter instead, with no effect of its own left
//! behind. While anything that changes what a command does is on (traps, set -x, POSIX
//! mode, set -a, a disabled builtin, a function named like a builtin the loop uses),
//! every command is interpreted and only the control flow stays native.

use std::rc::Rc;

use psh_syntax::{
    AndOrOp, ArithExpr, ArithKind, BinOp, BinaryTest, CmdId, Command, CommandKind, CondKind,
    ListId, ParamName, ParamOp, ParamPrefix, Part, Span, UnaryOp, Word, WordPart,
    plain_decimal,
};

use crate::exec;
use crate::options::SetOpt;
use crate::sched::safepoint;
use crate::state::{Code, R, Unwind, Vp};
use crate::vars::Value;

const NONE: u16 = u16::MAX;

/// A compiled loop.
pub(crate) struct Prog {
    slots: Vec<Box<str>>,
    exprs: Vec<AExpr>,
    code: Vec<Ins>,
    loops: Vec<LoopInfo>,
    nregs: usize,
    /// The commands the native operations stand for: a function of that name makes
    /// them interpreted.
    names: Vec<&'static [u8]>,
    /// for name in words: name's slot, so that an integer item is assigned to the slot.
    for_slot: u16,
}

/// The items of a compiled for loop.
pub(crate) enum ForSrc {
    /// The expanded words and the next one.
    Words(Vec<Vec<u8>>, usize),
    /// A brace sequence {a..b[..step]} of plain integers, not expanded into words:
    /// the next value, how many are left, and the signed step.
    Range { next: i64, left: u64, step: i64 },
}

struct LoopInfo {
    cont: u32,
    exit: u32,
    /// Holds the loop's status (its body's last).
    reg: u16,
    /// no_errexit when the loop started.
    ne: u16,
    parent: u16,
    /// The loop is in its parent's condition.
    in_cond: bool,
}

/// Where an instruction's command is in the script, to interpret it.
#[derive(Clone, Copy)]
enum Src {
    /// Item idx of the list, as run_list runs it.
    Item { list: ListId, idx: u32 },
    /// Pipeline k (0 the first) of that item's and-or list.
    Pipe { list: ListId, idx: u32, k: u32 },
}

enum Ins {
    /// A command: a native operation or the interpreter.
    Stmt {
        op: Op,
        src: Src,
        cmd: Option<CmdId>,
        line: u32,
        lp: u16,
        in_cond: bool,
    },
    /// The end of a list item: $? is its status.
    Item,
    SetSt(i32),
    /// Saves no_errexit into a register and sets it (a condition, an and-or list).
    NoErrSave(u16),
    NoErrRestore(u16),
    Jz(u32),
    Jnz(u32),
    Jmp(u32),
    /// A nested loop starts.
    LoopEnter(u16),
    /// An iteration starts: a safepoint, and pending signals.
    LoopHead,
    BodyDone(u16),
    /// The loop ends with the status in its register.
    LoopExit(u16),
    /// The next word of for name in words into name, or the end of the loop.
    ForItem { cmd: CmdId },
    /// A clause of for ((init; cond; step)).
    ForClause {
        lp: u16,
        which: Clause,
        expr: Option<u32>,
        cmd: CmdId,
        line: u32,
        blank: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    Init,
    Cond,
    Step,
}

enum Op {
    Arith(u32),
    Assign(Box<[(u16, Val)]>),
    /// [ a OP b ] or test a OP b: the operands must be integers.
    Test { op: Cmp, a: Opnd, b: Opnd, under: Under },
    /// [[ a OP b ]].
    Cond { op: Cmp, a: Opnd, b: Opnd },
    Const { status: i32, under: Span },
    /// echo with literal words and integer variables.
    Echo { args: Box<[EArg]>, newline: bool, under: Under },
    Break(u32, Span),
    Continue(u32, Span),
    Slow,
}

#[derive(Clone, Copy)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy)]
enum Opnd {
    Const(i64),
    /// $name: the variable must hold an integer.
    Slot(u16),
    /// A bare name in [[ ]], evaluated arithmetically: unset is 0.
    Arith(u16),
}

#[derive(Clone, Copy)]
enum Under {
    Span(Span),
    Slot,
}

#[derive(Clone, Copy)]
enum EArg {
    Lit(Span),
    Slot(u16),
}

enum Val {
    Int(i64),
    Slot(u16),
    Arith(u32),
}

/// An arithmetic expression over slots.
enum AExpr {
    Num(i64),
    Load(u16),
    Unary(UnaryOp, u32),
    IncDec { slot: u16, inc: bool, post: bool },
    Binary(BinOp, u32, u32),
    Cond(u32, u32, u32),
    Assign { slot: u16, op: Option<BinOp>, value: u32 },
}

/// The compiled loop of cmd, compiled on first use (None: not worth it).
pub(crate) fn compiled(code: &Code, cmd: &Command) -> Option<Rc<Prog>> {
    let key = (cmd.span.start, cmd.span.end);
    if let Some(known) = code.flat.borrow().get(&key) {
        return known.clone();
    }
    let prog = Compiler::new(code).top(cmd).map(Rc::new);
    code.flat.borrow_mut().insert(key, prog.clone());
    prog
}

struct Compiler<'c> {
    code: &'c Code,
    slots: Vec<Box<str>>,
    exprs: Vec<AExpr>,
    ins: Vec<Ins>,
    loops: Vec<LoopInfo>,
    nregs: usize,
    natives: usize,
    names: Vec<&'static [u8]>,
    /// The innermost loop being compiled, and whether in its condition.
    lp: u16,
    in_cond: bool,
    for_slot: u16,
}

impl<'c> Compiler<'c> {
    fn new(code: &'c Code) -> Self {
        Compiler {
            code,
            slots: Vec::new(),
            exprs: Vec::new(),
            ins: Vec::new(),
            loops: Vec::new(),
            nregs: 0,
            natives: 0,
            names: Vec::new(),
            lp: NONE,
            in_cond: false,
            for_slot: NONE,
        }
    }

    /// The loop's own redirections are applied by the caller (with_redirects).
    fn top(mut self, cmd: &Command) -> Option<Prog> {
        match &cmd.kind {
            CommandKind::While(_) | CommandKind::ArithFor(_) | CommandKind::For(_) => {}
            _ => return None,
        }
        self.compile_loop(cmd, true)?;
        if self.natives == 0 || self.slots.len() >= NONE as usize || self.nregs >= NONE as usize {
            return None;
        }
        Some(Prog {
            slots: self.slots,
            exprs: self.exprs,
            code: self.ins,
            loops: self.loops,
            nregs: self.nregs,
            names: self.names,
            for_slot: self.for_slot,
        })
    }

    fn reg(&mut self) -> u16 {
        self.nregs += 1;
        (self.nregs - 1) as u16
    }

    fn pc(&self) -> u32 {
        self.ins.len() as u32
    }

    fn slot(&mut self, name: &str) -> u16 {
        if let Some(i) = self.slots.iter().position(|s| &**s == name) {
            return i as u16;
        }
        self.slots.push(name.into());
        (self.slots.len() - 1) as u16
    }

    fn name(&mut self, n: &'static [u8]) {
        if !self.names.contains(&n) {
            self.names.push(n);
        }
    }

    fn expr0(&mut self) -> u32 {
        self.exprs.push(AExpr::Num(0));
        (self.exprs.len() - 1) as u32
    }

    /// A loop. top: the compiled loop itself (its caller counts it in vp.loops).
    fn compile_loop(&mut self, cmd: &Command, top: bool) -> Option<()> {
        let lp = self.loops.len() as u16;
        let (reg, ne) = (self.reg(), self.reg());
        self.loops.push(LoopInfo {
            cont: 0,
            exit: 0,
            reg,
            ne,
            parent: self.lp,
            in_cond: self.in_cond,
        });
        let (outer, outer_cond) = (self.lp, self.in_cond);
        self.lp = lp;
        self.in_cond = false;
        if !top {
            self.ins.push(Ins::LoopEnter(lp));
        }
        let mut exits = Vec::new();
        match &cmd.kind {
            CommandKind::While(w) => {
                let head = self.pc();
                self.loops[lp as usize].cont = head;
                self.ins.push(Ins::LoopHead);
                let r = self.reg();
                self.in_cond = true;
                self.ins.push(Ins::NoErrSave(r));
                self.list(w.cond)?;
                self.ins.push(Ins::NoErrRestore(r));
                self.in_cond = false;
                exits.push(self.pc());
                self.ins.push(if w.until { Ins::Jz(0) } else { Ins::Jnz(0) });
                self.list(w.body)?;
                self.ins.push(Ins::BodyDone(lp));
                self.ins.push(Ins::Jmp(head));
            }
            CommandKind::ArithFor(f) => {
                let line = self.code.line(cmd.span.start);
                let id = self.cmd_id(cmd)?;
                let init = self.clause(&f.init);
                self.ins.push(Ins::ForClause {
                    lp,
                    which: Clause::Init,
                    expr: init,
                    cmd: id,
                    line,
                    blank: false,
                });
                let head = self.pc();
                self.ins.push(Ins::LoopHead);
                let blank = self
                    .code
                    .ast
                    .bytes(f.cond.span)
                    .iter()
                    .all(u8::is_ascii_whitespace);
                let cond = self.clause(&f.cond);
                self.ins.push(Ins::ForClause {
                    lp,
                    which: Clause::Cond,
                    expr: cond,
                    cmd: id,
                    line,
                    blank,
                });
                self.list(f.body)?;
                self.ins.push(Ins::BodyDone(lp));
                self.loops[lp as usize].cont = self.pc();
                let step = self.clause(&f.step);
                self.ins.push(Ins::ForClause {
                    lp,
                    which: Clause::Step,
                    expr: step,
                    cmd: id,
                    line,
                    blank: false,
                });
                self.ins.push(Ins::Jmp(head));
            }
            // for name in words: only as the compiled loop itself, whose caller expands
            // the words (run_for).
            CommandKind::For(f) if top => {
                let id = self.cmd_id(cmd)?;
                if let Some(name) = psh_syntax::literal_text(&self.code.ast, &f.var)
                    && crate::state::is_name(name.as_bytes())
                {
                    self.for_slot = self.slot(&name);
                }
                let head = self.pc();
                self.loops[lp as usize].cont = head;
                self.ins.push(Ins::LoopHead);
                exits.push(self.pc());
                self.ins.push(Ins::ForItem { cmd: id });
                self.list(f.body)?;
                self.ins.push(Ins::BodyDone(lp));
                self.ins.push(Ins::Jmp(head));
            }
            _ => return None,
        }
        let exit = self.pc();
        for at in exits {
            self.patch(at, exit);
        }
        self.loops[lp as usize].exit = exit;
        self.ins.push(Ins::LoopExit(lp));
        self.lp = outer;
        self.in_cond = outer_cond;
        Some(())
    }

    fn cmd_id(&self, cmd: &Command) -> Option<CmdId> {
        self.code
            .ast
            .cmd_ids()
            .find(|&id| std::ptr::eq(self.code.ast.cmd(id), cmd))
    }

    fn patch(&mut self, at: u32, to: u32) {
        match &mut self.ins[at as usize] {
            Ins::Jz(t) | Ins::Jnz(t) | Ins::Jmp(t) => *t = to,
            Ins::ForItem { .. } => {}
            _ => unreachable!(),
        }
    }

    fn clause(&mut self, w: &Word) -> Option<u32> {
        let lit = crate::arith::literal(self.code, w)?;
        let expr = match lit.expr() {
            None => self.expr0(),
            Some(e) => self.arith(e, &lit.text)?,
        };
        self.natives += 1;
        Some(expr)
    }

    fn list(&mut self, id: ListId) -> Option<()> {
        let list = self.code.ast.list(id);
        for (idx, item) in list.items.iter().enumerate() {
            let src = Src::Item {
                list: id,
                idx: idx as u32,
            };
            let p = &item.first;
            let plain = |p: &psh_syntax::Pipeline| {
                p.stages.len() == 1 && !p.negated() && p.time.is_none()
            };
            if item.background {
                self.slow(src, None);
            } else if item.rest.is_empty() && plain(p) {
                self.command(p.stages[0].cmd, src)?;
            } else if plain(p) && item.rest.iter().all(|(_, p)| plain(p)) {
                self.andor(id, idx as u32, item);
            } else {
                self.slow(src, None);
            }
            self.ins.push(Ins::Item);
        }
        Some(())
    }

    fn andor(&mut self, list: ListId, idx: u32, item: &psh_syntax::AndOr) {
        let n = item.rest.len();
        let r = self.reg();
        let mut ends = Vec::new();
        self.ins.push(Ins::NoErrSave(r));
        self.pipe(item.first.stages[0].cmd, Src::Pipe { list, idx, k: 0 });
        self.ins.push(Ins::NoErrRestore(r));
        for (k, (op, p)) in item.rest.iter().enumerate() {
            ends.push(self.pc());
            self.ins.push(match op {
                AndOrOp::And => Ins::Jnz(0),
                AndOrOp::Or => Ins::Jz(0),
            });
            // $? between pipelines is the last one's status (run_andor).
            self.ins.push(Ins::Item);
            let last = k + 1 == n;
            if !last {
                self.ins.push(Ins::NoErrSave(r));
            }
            self.pipe(
                p.stages[0].cmd,
                Src::Pipe {
                    list,
                    idx,
                    k: k as u32 + 1,
                },
            );
            if !last {
                self.ins.push(Ins::NoErrRestore(r));
            }
        }
        // A pipeline that does not run leaves the status as it is, and the
        // next operator decides on that status: in a && b || c a failing a
        // skips b and still runs c.
        let end = self.pc();
        for (k, &at) in ends.iter().enumerate() {
            self.patch(at, ends.get(k + 1).copied().unwrap_or(end));
        }
    }

    /// A pipeline of an and-or list: native if it is a simple command we know.
    fn pipe(&mut self, id: CmdId, src: Src) {
        let cmd = self.code.ast.cmd(id);
        let op = match &cmd.kind {
            CommandKind::Simple(_) | CommandKind::Arith(_) | CommandKind::Cond(_)
                if cmd.redirects.is_empty() =>
            {
                self.native(cmd)
            }
            _ => None,
        };
        self.stmt(op.unwrap_or(Op::Slow), src, Some(id));
    }

    fn slow(&mut self, src: Src, cmd: Option<CmdId>) {
        self.stmt(Op::Slow, src, cmd);
    }

    fn stmt(&mut self, op: Op, src: Src, cmd: Option<CmdId>) {
        if !matches!(op, Op::Slow) {
            self.natives += 1;
        }
        let line = match cmd {
            Some(id) => {
                let c = self.code.ast.cmd(id);
                match &c.kind {
                    CommandKind::Simple(s) => exec::simple_line(self.code, c, s),
                    _ => self.code.line(c.span.start),
                }
            }
            None => 0,
        };
        self.ins.push(Ins::Stmt {
            op,
            src,
            cmd,
            line,
            lp: self.lp,
            in_cond: self.in_cond,
        });
    }

    /// A command that is a whole list item.
    fn command(&mut self, id: CmdId, src: Src) -> Option<()> {
        let cmd = self.code.ast.cmd(id);
        if !cmd.redirects.is_empty() {
            self.slow(src, Some(id));
            return Some(());
        }
        match &cmd.kind {
            CommandKind::While(_) | CommandKind::ArithFor(_) => {
                let start = self.ins.len();
                let saved = (self.slots.len(), self.exprs.len(), self.loops.len(), self.nregs);
                let (lp, in_cond, natives) = (self.lp, self.in_cond, self.natives);
                if self.compile_loop(cmd, false).is_none() {
                    self.ins.truncate(start);
                    self.slots.truncate(saved.0);
                    self.exprs.truncate(saved.1);
                    self.loops.truncate(saved.2);
                    self.nregs = saved.3;
                    self.lp = lp;
                    self.in_cond = in_cond;
                    self.natives = natives;
                    self.slow(src, Some(id));
                }
            }
            CommandKind::If(c) => {
                let mut ends = Vec::new();
                for br in &c.branches {
                    // An if condition is not the loop's own: continue there goes on
                    // with the loop.
                    let r = self.reg();
                    self.ins.push(Ins::NoErrSave(r));
                    self.list(br.cond)?;
                    self.ins.push(Ins::NoErrRestore(r));
                    let skip = self.pc();
                    self.ins.push(Ins::Jnz(0));
                    self.list(br.body)?;
                    ends.push(self.pc());
                    self.ins.push(Ins::Jmp(0));
                    let next = self.pc();
                    self.patch(skip, next);
                }
                match c.else_body {
                    Some(e) => self.list(e)?,
                    None => self.ins.push(Ins::SetSt(0)),
                }
                let end = self.pc();
                for at in ends {
                    self.patch(at, end);
                }
            }
            CommandKind::Brace(list) => self.list(*list)?,
            CommandKind::Simple(_) | CommandKind::Arith(_) | CommandKind::Cond(_) => {
                let op = self.native(cmd).unwrap_or(Op::Slow);
                self.stmt(op, src, Some(id));
            }
            _ => self.slow(src, Some(id)),
        }
        Some(())
    }

    fn native(&mut self, cmd: &Command) -> Option<Op> {
        match &cmd.kind {
            CommandKind::Arith(w) => {
                let lit = crate::arith::literal(self.code, w)?;
                let e = match lit.expr() {
                    Some(e) => self.arith(e, &lit.text)?,
                    None => self.expr0(),
                };
                Some(Op::Arith(e))
            }
            CommandKind::Cond(e) => {
                let CondKind::Binary { op, lhs, rhs, .. } = &e.kind else {
                    return None;
                };
                let op = cmp(*op)?;
                let a = self.cond_operand(lhs)?;
                let b = self.cond_operand(rhs)?;
                Some(Op::Cond { op, a, b })
            }
            CommandKind::Simple(s) => {
                if s.words.is_empty() {
                    if s.assigns.is_empty() {
                        return None;
                    }
                    let mut out = Vec::new();
                    for a in &s.assigns {
                        if a.index.is_some() || a.append {
                            return None;
                        }
                        let psh_syntax::AssignValue::Scalar(w) = &a.value else {
                            return None;
                        };
                        let name = self.code.ast.text(a.name);
                        if matches!(name, "BASH_XTRACEFD" | "IFS" | "_") {
                            return None;
                        }
                        let v = self.value(w)?;
                        let slot = self.slot(name);
                        out.push((slot, v));
                    }
                    return Some(Op::Assign(out.into_boxed_slice()));
                }
                if !s.assigns.is_empty() {
                    return None;
                }
                let code = self.code;
                let lit = |w: &Word| -> Option<&str> {
                    match w.parts.as_slice() {
                        [p] if matches!(p.kind, Part::Lit) => Some(code.ast.text(p.span)),
                        _ => None,
                    }
                };
                let name = lit(&s.words[0])?;
                let last = s.words.last().map_or(Span::new(0, 0), |w| w.span);
                match (name, s.words.len()) {
                    ("true" | ":" | "false", 1) => {
                        let n: &'static [u8] = match name {
                            "true" => b"true",
                            ":" => b":",
                            _ => b"false",
                        };
                        self.name(n);
                        Some(Op::Const {
                            status: i32::from(name == "false"),
                            under: last,
                        })
                    }
                    ("break" | "continue", 1 | 2) => {
                        let n = match s.words.get(1) {
                            None => 1,
                            Some(w) => {
                                let n = plain_decimal(lit(w)?.as_bytes())?;
                                if n < 1 {
                                    return None;
                                }
                                n as u32
                            }
                        };
                        if n as usize > self.depth() {
                            return None;
                        }
                        if name == "break" {
                            self.name(b"break");
                            Some(Op::Break(n, last))
                        } else {
                            self.name(b"continue");
                            Some(Op::Continue(n, last))
                        }
                    }
                    ("echo", _) => {
                        let mut words = &s.words[1..];
                        let mut newline = true;
                        if let Some(w) = words.first()
                            && lit(w) == Some("-n")
                        {
                            newline = false;
                            words = &words[1..];
                        }
                        let mut args = Vec::new();
                        for (k, w) in words.iter().enumerate() {
                            if let Some(t) = lit(w) {
                                // Glob characters, a tilde or (first) an option need the
                                // interpreter.
                                if t.bytes().any(|b| matches!(b, b'*' | b'?' | b'[' | b'~'))
                                    || (k == 0 && t.starts_with('-'))
                                {
                                    return None;
                                }
                                args.push(EArg::Lit(w.parts[0].span));
                            } else {
                                let slot = self.param(w)?;
                                if k == 0 {
                                    // A negative number would be read as an option only if
                                    // it were -n, -e or -E: digits never are.
                                }
                                args.push(EArg::Slot(slot));
                            }
                        }
                        let under = match args.last() {
                            Some(EArg::Slot(_)) => Under::Slot,
                            _ => Under::Span(last),
                        };
                        self.name(b"echo");
                        Some(Op::Echo {
                            args: args.into_boxed_slice(),
                            newline,
                            under,
                        })
                    }
                    ("[", 5) | ("test", 4) => {
                        let args = &s.words[1..];
                        if name == "[" && lit(&args[3])? != "]" {
                            return None;
                        }
                        let op = match lit(&args[1])? {
                            "-eq" => Cmp::Eq,
                            "-ne" => Cmp::Ne,
                            "-lt" => Cmp::Lt,
                            "-le" => Cmp::Le,
                            "-gt" => Cmp::Gt,
                            "-ge" => Cmp::Ge,
                            _ => return None,
                        };
                        let a = self.test_operand(&args[0])?;
                        let b = self.test_operand(&args[2])?;
                        let under = match (name, b) {
                            ("test", Opnd::Slot(_)) => Under::Slot,
                            _ => Under::Span(last),
                        };
                        self.name(if name == "[" { b"[" } else { b"test" });
                        Some(Op::Test { op, a, b, under })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// How many loops of this compiled code enclose the current point.
    fn depth(&self) -> usize {
        let mut n = 0;
        let mut lp = self.lp;
        while lp != NONE {
            n += 1;
            lp = self.loops[lp as usize].parent;
        }
        n
    }

    /// $name, braced, or in double quotes.
    fn param(&mut self, w: &Word) -> Option<u16> {
        let part = match w.parts.as_slice() {
            [p] => p,
            _ => return None,
        };
        let inner: &WordPart = match &part.kind {
            Part::Double(parts) => match parts.as_slice() {
                [p] => p,
                _ => return None,
            },
            _ => part,
        };
        let Part::Param(p) = &inner.kind else {
            return None;
        };
        if p.prefix != ParamPrefix::None
            || p.name != ParamName::Var
            || p.index.is_some()
            || !matches!(p.op, ParamOp::None)
        {
            return None;
        }
        let name = self.code.ast.text(p.name_span);
        if !crate::state::is_name(name.as_bytes()) {
            return None;
        }
        Some(self.slot(name))
    }

    fn int_lit(&self, w: &Word) -> Option<i64> {
        match w.parts.as_slice() {
            [p] if matches!(p.kind, Part::Lit) => slot_decimal(self.code.ast.bytes(p.span)),
            _ => None,
        }
    }

    fn test_operand(&mut self, w: &Word) -> Option<Opnd> {
        if let Some(n) = self.int_lit(w) {
            return Some(Opnd::Const(n));
        }
        self.param(w).map(Opnd::Slot)
    }

    fn cond_operand(&mut self, w: &Word) -> Option<Opnd> {
        if let Some(n) = self.int_lit(w) {
            return Some(Opnd::Const(n));
        }
        if let [p] = w.parts.as_slice()
            && matches!(p.kind, Part::Lit)
        {
            let name = self.code.ast.text(p.span);
            if crate::state::is_name(name.as_bytes()) {
                return Some(Opnd::Arith(self.slot(name)));
            }
            return None;
        }
        self.param(w).map(Opnd::Slot)
    }

    fn value(&mut self, w: &Word) -> Option<Val> {
        if let Some(n) = self.int_lit(w) {
            return Some(Val::Int(n));
        }
        if let [p] = w.parts.as_slice()
            && let Part::Arith(inner) = &p.kind
        {
            let lit = crate::arith::literal(self.code, inner)?;
            return Some(Val::Arith(match lit.expr() {
                Some(e) => self.arith(e, &lit.text)?,
                None => self.expr0(),
            }));
        }
        self.param(w).map(Val::Slot)
    }

    fn arith(&mut self, e: &ArithExpr, text: &[u8]) -> Option<u32> {
        let name = |lv: &psh_syntax::LValue| -> Option<String> {
            if lv.index.is_some() {
                return None;
            }
            std::str::from_utf8(&text[lv.name.range()]).ok().map(str::to_string)
        };
        let node = match &e.kind {
            ArithKind::Num(n) => AExpr::Num(*n),
            ArithKind::Var(lv) => AExpr::Load(self.slot(&name(lv)?)),
            ArithKind::Unary { op, expr } => AExpr::Unary(*op, self.arith(expr, text)?),
            ArithKind::IncDec { target, inc, post } => AExpr::IncDec {
                slot: self.slot(&name(target)?),
                inc: *inc,
                post: *post,
            },
            ArithKind::Binary { op, lhs, rhs } => {
                AExpr::Binary(*op, self.arith(lhs, text)?, self.arith(rhs, text)?)
            }
            ArithKind::Cond {
                cond,
                then,
                otherwise,
            } => AExpr::Cond(
                self.arith(cond, text)?,
                self.arith(then, text)?,
                self.arith(otherwise, text)?,
            ),
            ArithKind::Assign { target, op, value } => {
                let slot = self.slot(&name(target)?);
                AExpr::Assign {
                    slot,
                    op: *op,
                    value: self.arith(value, text)?,
                }
            }
            ArithKind::Fail { .. } => return None,
        };
        self.exprs.push(node);
        Some((self.exprs.len() - 1) as u32)
    }
}

fn cmp(op: BinaryTest) -> Option<Cmp> {
    Some(match op {
        BinaryTest::ArithmeticEqualTo => Cmp::Eq,
        BinaryTest::ArithmeticNotEqualTo => Cmp::Ne,
        BinaryTest::ArithmeticLessThan => Cmp::Lt,
        BinaryTest::ArithmeticLessThanOrEqualTo => Cmp::Le,
        BinaryTest::ArithmeticGreaterThan => Cmp::Gt,
        BinaryTest::ArithmeticGreaterThanOrEqualTo => Cmp::Ge,
        _ => return None,
    })
}

impl Cmp {
    fn eval(self, a: i64, b: i64) -> bool {
        match self {
            Cmp::Eq => a == b,
            Cmp::Ne => a != b,
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
        }
    }
}

fn slot_decimal(value: &[u8]) -> Option<i64> {
    let n = plain_decimal(value)?;
    (n != 0 || value == b"0").then_some(n)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SV {
    Int(i64),
    /// Not set: 0 in arithmetic.
    Unset,
    /// Anything else: only the interpreter touches it.
    Other,
}

#[derive(Clone, Copy)]
enum PendUnder {
    No,
    Empty,
    Span(Span),
    Int(i64),
}

/// The running state of a compiled loop.
struct Ex<'p> {
    prog: &'p Prog,
    slots: Vec<(SV, bool)>,
    regs: Vec<i32>,
    undo: Vec<(u16, SV, bool)>,
    nounset: bool,
    /// What the native operations since the last flush leave behind.
    under: PendUnder,
    pipe: Option<i32>,
    line: Option<u32>,
    simple: bool,
    buf: Vec<u8>,
    echo_last: Option<i64>,
    /// for name in words: the name, the words and the next one.
    items: Option<(&'p str, ForSrc)>,
}

impl Ex<'_> {
    /// The next item of a brace sequence straight into the loop variable's slot, while
    /// native operations run and the variable holds a plain integer or is unset (what
    /// reload found, so no nameref, readonly or other attribute stands in the way):
    /// Some(true) when assigned, Some(false) at the end, None for run_with's own way.
    fn for_fast(&mut self) -> Option<bool> {
        let slot = self.prog.for_slot;
        let Some((_, ForSrc::Range { next, left, step })) = &mut self.items else {
            return None;
        };
        if *left == 0 {
            return Some(false);
        }
        if slot == NONE || !matches!(self.slots[slot as usize].0, SV::Int(_) | SV::Unset) {
            return None;
        }
        *left -= 1;
        let n = *next;
        if *left > 0 {
            *next += *step;
        }
        self.slots[slot as usize] = (SV::Int(n), true);
        Some(true)
    }

    /// The line echo writes, into buf; None if a variable is not an integer.
    fn echo_line(&mut self, code: &Code, args: &[EArg], newline: bool) -> Option<()> {
        self.buf.clear();
        let mut lastv = None;
        for (k, a) in args.iter().enumerate() {
            if k > 0 {
                self.buf.push(b' ');
            }
            match a {
                EArg::Lit(sp) => self.buf.extend_from_slice(code.ast.bytes(*sp)),
                EArg::Slot(s) => {
                    let SV::Int(n) = self.slots[*s as usize].0 else {
                        return None;
                    };
                    let _ = std::io::Write::write_fmt(&mut self.buf, format_args!("{n}"));
                    lastv = Some(n);
                }
            }
        }
        if newline {
            self.buf.push(b'\n');
        }
        self.echo_last = lastv;
        Some(())
    }

    fn reload(&mut self, vp: &Vp) {
        for (i, name) in self.prog.slots.iter().enumerate() {
            let v = match vp.vars.get(name) {
                None => SV::Unset,
                Some(v) if v.nameref || v.readonly || v.dynamic.is_some() => SV::Other,
                Some(v) => match &v.value {
                    Value::Str(s) => slot_decimal(s).map_or(SV::Other, SV::Int),
                    Value::Unset if !v.declared_only => SV::Unset,
                    _ => SV::Other,
                },
            };
            self.slots[i] = (v, false);
        }
        self.nounset = vp.opts.get(SetOpt::Nounset);
    }

    fn flush(&mut self, vp: &mut Vp, code: &Code) {
        for (i, (v, dirty)) in self.slots.iter_mut().enumerate() {
            if !*dirty {
                continue;
            }
            *dirty = false;
            let SV::Int(n) = *v else { continue };
            let name = &*self.prog.slots[i];
            if let Some(var) = vp.vars.get_mut(name)
                && let Value::Str(s) = &mut var.value
            {
                s.clear();
                let _ = std::io::Write::write_fmt(s, format_args!("{n}"));
                var.declared_only = false;
                continue;
            }
            let _ = vp.assign_with(name, n.to_string().into_bytes(), false);
        }
        if self.simple {
            self.simple = false;
            vp.core.fg_signal.set(None);
        }
        match std::mem::replace(&mut self.under, PendUnder::No) {
            PendUnder::No => {}
            PendUnder::Empty => {
                exec::set_underscore(vp, Vec::new());
            }
            PendUnder::Span(sp) => {
                exec::set_underscore(vp, code.ast.bytes(sp).to_vec());
            }
            PendUnder::Int(n) => {
                exec::set_underscore(vp, n.to_string().into_bytes());
            }
        }
        if let Some(st) = self.pipe.take() {
            exec::set_pipestatus(vp, &[st]);
        }
        if let Some(line) = self.line.take() {
            vp.line = line;
        }
    }

    fn get(&self, s: u16) -> Option<i64> {
        match self.slots[s as usize].0 {
            SV::Int(n) => Some(n),
            SV::Unset if !self.nounset => Some(0),
            _ => None,
        }
    }

    fn put(&mut self, s: u16, n: i64) -> Option<()> {
        let (v, dirty) = self.slots[s as usize];
        if v == SV::Other {
            return None;
        }
        self.undo.push((s, v, dirty));
        self.slots[s as usize] = (SV::Int(n), true);
        Some(())
    }

    fn eval(&mut self, e: u32) -> Option<i64> {
        Some(match self.prog.exprs[e as usize] {
            AExpr::Num(n) => n,
            AExpr::Load(s) => self.get(s)?,
            AExpr::Unary(op, x) => {
                let v = self.eval(x)?;
                match op {
                    UnaryOp::Plus => v,
                    UnaryOp::Minus => v.wrapping_neg(),
                    UnaryOp::Not => (v == 0) as i64,
                    UnaryOp::BitNot => !v,
                }
            }
            AExpr::IncDec { slot, inc, post } => {
                let old = self.get(slot)?;
                let new = if inc {
                    old.wrapping_add(1)
                } else {
                    old.wrapping_sub(1)
                };
                self.put(slot, new)?;
                if post { old } else { new }
            }
            AExpr::Binary(op, a, b) => match op {
                BinOp::And => (self.eval(a)? != 0 && self.eval(b)? != 0) as i64,
                BinOp::Or => (self.eval(a)? != 0 || self.eval(b)? != 0) as i64,
                BinOp::Comma => {
                    self.eval(a)?;
                    self.eval(b)?
                }
                _ => {
                    let x = self.eval(a)?;
                    let y = self.eval(b)?;
                    binary(op, x, y)?
                }
            },
            AExpr::Cond(c, t, f) => {
                if self.eval(c)? != 0 {
                    self.eval(t)?
                } else {
                    self.eval(f)?
                }
            }
            AExpr::Assign { slot, op, value } => {
                let old = match op {
                    Some(_) => Some(self.get(slot)?),
                    None => None,
                };
                let v = self.eval(value)?;
                let new = match (op, old) {
                    (Some(op), Some(old)) => binary(op, old, v)?,
                    _ => v,
                };
                self.put(slot, new)?;
                new
            }
        })
    }

    /// A number or a variable that holds one.
    #[inline]
    fn leaf(&self, e: u32) -> Option<i64> {
        match self.prog.exprs[e as usize] {
            AExpr::Num(n) => Some(n),
            AExpr::Load(s) => self.get(s),
            _ => None,
        }
    }

    /// The shapes counting loops are made of, without eval's recursion and undo log:
    /// `a OP b` over numbers and integer variables, `i++`/`--i`, and `i = b`/`i += b` on
    /// an integer variable. None: not one of them, or one that would fail (eval then
    /// does it, and fails as before); nothing is written then.
    #[inline]
    fn quick(&mut self, e: u32) -> Option<i64> {
        match self.prog.exprs[e as usize] {
            AExpr::Binary(op, a, b)
                if !matches!(op, BinOp::And | BinOp::Or | BinOp::Comma) =>
            {
                let (x, y) = (self.leaf(a)?, self.leaf(b)?);
                binary(op, x, y)
            }
            AExpr::IncDec { slot, inc, post } => {
                let SV::Int(old) = self.slots[slot as usize].0 else {
                    return None;
                };
                let new = if inc { old.wrapping_add(1) } else { old.wrapping_sub(1) };
                self.slots[slot as usize] = (SV::Int(new), true);
                Some(if post { old } else { new })
            }
            AExpr::Assign { slot, op, value } => {
                let SV::Int(old) = self.slots[slot as usize].0 else {
                    return None;
                };
                let v = self.leaf(value)?;
                let new = match op {
                    Some(op) => binary(op, old, v)?,
                    None => v,
                };
                self.slots[slot as usize] = (SV::Int(new), true);
                Some(new)
            }
            AExpr::Num(n) => Some(n),
            AExpr::Load(s) => self.get(s),
            _ => None,
        }
    }

    /// Evaluates an expression; on failure nothing it wrote stays.
    fn arith(&mut self, e: u32) -> Option<i64> {
        let mark = self.undo.len();
        let r = self.eval(e);
        if r.is_none() {
            while self.undo.len() > mark {
                let (s, v, dirty) = self.undo.pop().unwrap();
                self.slots[s as usize] = (v, dirty);
            }
        }
        r
    }

    fn int(&self, o: Opnd) -> Option<i64> {
        match o {
            Opnd::Const(n) => Some(n),
            Opnd::Slot(s) => match self.slots[s as usize].0 {
                SV::Int(n) => Some(n),
                _ => None,
            },
            Opnd::Arith(s) => self.get(s),
        }
    }

    /// Runs a native operation: its status, or None to interpret the command.
    fn native(&mut self, op: &Op) -> Option<i32> {
        match op {
            Op::Arith(e) => {
                let v = match self.quick(*e) {
                    Some(v) => v,
                    None => {
                        self.undo.clear();
                        self.arith(*e)?
                    }
                };
                let st = i32::from(v == 0);
                self.pipe = Some(st);
                Some(st)
            }
            Op::Assign(list) => {
                self.undo.clear();
                for (slot, v) in list.iter() {
                    let n = match v {
                        Val::Int(n) => Some(*n),
                        Val::Slot(s) => match self.slots[*s as usize].0 {
                            SV::Int(n) => Some(n),
                            _ => None,
                        },
                        // Only a shape that writes nothing: a later failing item undoes
                        // this list's writes from the undo log.
                        Val::Arith(e) => match self.prog.exprs[*e as usize] {
                            AExpr::Binary(..) | AExpr::Num(_) | AExpr::Load(_) => {
                                match self.quick(*e) {
                                    Some(v) => Some(v),
                                    None => self.arith(*e),
                                }
                            }
                            _ => self.arith(*e),
                        },
                    };
                    if n.and_then(|n| self.put(*slot, n)).is_none() {
                        while let Some((s, v, dirty)) = self.undo.pop() {
                            self.slots[s as usize] = (v, dirty);
                        }
                        return None;
                    }
                }
                self.under = PendUnder::Empty;
                self.pipe = Some(0);
                self.simple = true;
                Some(0)
            }
            Op::Test { op, a, b, under } => {
                let x = self.int(*a)?;
                let y = self.int(*b)?;
                self.under = match under {
                    Under::Span(sp) => PendUnder::Span(*sp),
                    Under::Slot => PendUnder::Int(y),
                };
                let st = i32::from(!op.eval(x, y));
                self.pipe = Some(st);
                self.simple = true;
                Some(st)
            }
            Op::Cond { op, a, b } => {
                let x = self.int(*a)?;
                let y = self.int(*b)?;
                let st = i32::from(!op.eval(x, y));
                self.pipe = Some(st);
                Some(st)
            }
            Op::Const { status, under } => {
                self.under = PendUnder::Span(*under);
                self.pipe = Some(*status);
                self.simple = true;
                Some(*status)
            }
            Op::Break(..) | Op::Continue(..) | Op::Echo { .. } | Op::Slow => None,
        }
    }
}

fn binary(op: BinOp, a: i64, b: i64) -> Option<i64> {
    Some(match op {
        BinOp::Pow => {
            if b < 0 {
                return None;
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
                return None;
            }
            if op == BinOp::Div {
                a.wrapping_div(b)
            } else {
                a.wrapping_rem(b)
            }
        }
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
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

/// Whether native operations may run: nothing that changes what a command does is on.
fn native_ok(vp: &Vp, prog: &Prog) -> bool {
    if vp.traps.live()
        || vp.opts.get(SetOpt::Xtrace)
        || vp.opts.get(SetOpt::Posix)
        || vp.opts.get(SetOpt::Allexport)
        || !vp.disabled.is_empty()
    {
        return false;
    }
    if !vp.funcs.is_empty()
        && prog
            .names
            .iter()
            .any(|n| std::str::from_utf8(n).is_ok_and(|n| vp.funcs.contains_key(n)))
    {
        return false;
    }
    if vp.vars.get("_").is_some_and(|v| v.readonly) {
        return false;
    }
    // An unquoted $i of digits is one field unless IFS splits digits or '-'.
    !vp
        .vars
        .value("IFS")
        .is_some_and(|ifs| ifs.iter().any(|b| b.is_ascii_digit() || *b == b'-'))
}

/// What to do after an unwind reached a compiled loop.
enum Landing {
    Jump(u32),
    Return(R),
}

/// Runs a compiled loop (the caller counts it in vp.loops).
pub(crate) async fn run(vp: &mut Vp, code: &Rc<Code>, prog: &Prog) -> R {
    run_with(vp, code, prog, None).await
}

/// Runs a compiled for name in words loop over its expanded words (the caller counts
/// it in vp.loops).
pub(crate) async fn run_for(
    vp: &mut Vp,
    code: &Rc<Code>,
    prog: &Prog,
    name: &str,
    items: Vec<Vec<u8>>,
) -> R {
    run_with(vp, code, prog, Some((name, ForSrc::Words(items, 0)))).await
}

/// Runs a compiled for loop over a brace sequence of integers without expanding it
/// (exec::run_for checks that it is one; the caller counts it in vp.loops).
pub(crate) async fn run_for_range(
    vp: &mut Vp,
    code: &Rc<Code>,
    prog: &Prog,
    name: &str,
    range: ForSrc,
) -> R {
    run_with(vp, code, prog, Some((name, range))).await
}

async fn run_with<'p>(
    vp: &mut Vp,
    code: &Rc<Code>,
    prog: &'p Prog,
    items: Option<(&'p str, ForSrc)>,
) -> R {
    let mut ex = Ex {
        prog,
        slots: vec![(SV::Other, false); prog.slots.len()],
        regs: vec![0; prog.nregs],
        undo: Vec::new(),
        nounset: false,
        under: PendUnder::No,
        pipe: None,
        line: None,
        simple: false,
        buf: Vec::new(),
        echo_last: None,
        items,
    };
    ex.reload(vp);
    let mut native = native_ok(vp, prog);
    let top = &prog.loops[0];
    ex.regs[top.reg as usize] = 0;
    ex.regs[top.ne as usize] = i32::from(vp.no_errexit);
    let mut st: i32 = vp.status;
    let mut pc: u32 = 0;
    // The budget was taken for the instruction at pc already (after a yield).
    let mut passed = false;
    loop {
        // Native stretches run synchronously, outside this future (spin).
        if native && !passed && !vp.opts.get(SetOpt::Errexit) {
            pc = spin(vp, &mut ex, pc, &mut st);
        }
        match &prog.code[pc as usize] {
            Ins::Stmt {
                op,
                src,
                cmd,
                line,
                lp,
                in_cond,
            } => {
                if !passed && !vp.core.sched.pass() {
                    ex.flush(vp, code);
                    safepoint(&vp.core.sched).await;
                    ex.reload(vp);
                    native = native_ok(vp, prog);
                    passed = true;
                    continue;
                }
                passed = false;
                if native {
                    if let Op::Break(n, under) | Op::Continue(n, under) = op {
                        ex.under = PendUnder::Span(*under);
                        ex.simple = true;
                        ex.line = Some(*line);
                        let u = match op {
                            Op::Break(..) => Unwind::Break(*n),
                            _ => Unwind::Continue(*n),
                        };
                        // `break` and `continue` succeed, as their builtins do.
                        vp.status = 0;
                        match land(vp, &mut ex, *lp, *in_cond, u) {
                            Landing::Jump(to) => {
                                pc = to;
                                continue;
                            }
                            Landing::Return(r) => {
                                ex.flush(vp, code);
                                return r;
                            }
                        }
                    }
                    let mut done = ex.native(op).map(Ok);
                    if let Op::Echo {
                        args,
                        newline,
                        under,
                    } = op
                        && !vp.opts.shopt(crate::options::Shopt::XpgEcho)
                        && ex.echo_line(code, args, *newline).is_some()
                    {
                        vp.line = *line;
                        let r = crate::builtins::write_fd(vp, 1, "echo", &ex.buf).await;
                        ex.under = match under {
                            Under::Span(sp) => PendUnder::Span(*sp),
                            Under::Slot => PendUnder::Int(ex.echo_last.unwrap_or(0)),
                        };
                        ex.simple = true;
                        done = Some(r.map(|ok| {
                            let st = i32::from(!ok);
                            ex.pipe = Some(st);
                            st
                        }));
                    }
                    if let Some(Err(u)) = done {
                        match land(vp, &mut ex, *lp, *in_cond, u) {
                            Landing::Jump(to) => {
                                pc = to;
                                continue;
                            }
                            Landing::Return(r) => {
                                ex.flush(vp, code);
                                return r;
                            }
                        }
                    }
                    if let Some(Ok(s)) = done {
                        st = s;
                        ex.line = Some(*line);
                        if s != 0
                            && !vp.no_errexit
                            && vp.opts.get(SetOpt::Errexit)
                            && let Some(id) = cmd
                        {
                            ex.flush(vp, code);
                            let c = code.ast.cmd(*id);
                            let r = match exec::after_errexit(vp, code, c, s, true) {
                                Some(f) => f.await,
                                None => Ok(s),
                            };
                            ex.reload(vp);
                            native = native_ok(vp, prog);
                            match r {
                                Ok(s) => st = s,
                                Err(u) => match land(vp, &mut ex, *lp, *in_cond, u) {
                                    Landing::Jump(to) => {
                                        pc = to;
                                        continue;
                                    }
                                    Landing::Return(r) => {
                                        ex.flush(vp, code);
                                        return r;
                                    }
                                },
                            }
                        }
                        pc += 1;
                        continue;
                    }
                }
                ex.flush(vp, code);
                let r = interpret(vp, code, *src).await;
                ex.reload(vp);
                native = native_ok(vp, prog);
                match r {
                    Ok(s) => {
                        st = s;
                        pc += 1;
                    }
                    Err(u) => match land(vp, &mut ex, *lp, *in_cond, u) {
                        Landing::Jump(to) => pc = to,
                        Landing::Return(r) => {
                            ex.flush(vp, code);
                            return r;
                        }
                    },
                }
            }
            Ins::Item => {
                vp.status = st;
                pc += 1;
            }
            Ins::SetSt(s) => {
                st = *s;
                pc += 1;
            }
            Ins::NoErrSave(r) => {
                ex.regs[*r as usize] = i32::from(vp.no_errexit);
                vp.no_errexit = true;
                pc += 1;
            }
            Ins::NoErrRestore(r) => {
                vp.no_errexit = ex.regs[*r as usize] != 0;
                pc += 1;
            }
            Ins::Jz(t) => pc = if st == 0 { *t } else { pc + 1 },
            Ins::Jnz(t) => pc = if st != 0 { *t } else { pc + 1 },
            Ins::Jmp(t) => pc = *t,
            Ins::LoopEnter(lp) => {
                let l = &prog.loops[*lp as usize];
                vp.loops += 1;
                ex.regs[l.reg as usize] = 0;
                ex.regs[l.ne as usize] = i32::from(vp.no_errexit);
                pc += 1;
            }
            Ins::LoopHead => {
                if !passed && !vp.core.sched.pass() {
                    ex.flush(vp, code);
                    safepoint(&vp.core.sched).await;
                    ex.reload(vp);
                    native = native_ok(vp, prog);
                    passed = true;
                    continue;
                }
                passed = false;
                if let Some(sig) = vp.job.as_ref().and_then(|j| j.killed()) {
                    ex.flush(vp, code);
                    return Err(Unwind::Exit(128 + sig));
                }
                if (vp.top && vp.sigs.peek(&vp.core) != 0) || vp.own_sigs.get() != 0 {
                    ex.flush(vp, code);
                    if let Err(u) = crate::builtins::trap::take_signals(vp).await {
                        vp.no_errexit = ex.regs[top.ne as usize] != 0;
                        return Err(u);
                    }
                    ex.reload(vp);
                    native = native_ok(vp, prog);
                }
                pc += 1;
            }
            Ins::ForItem { cmd } => {
                let exit = prog.loops[0].exit;
                if native {
                    match ex.for_fast() {
                        Some(true) => {
                            pc += 1;
                            continue;
                        }
                        Some(false) => {
                            pc = exit;
                            continue;
                        }
                        None => {}
                    }
                }
                let Some((name, src)) = &mut ex.items else {
                    unreachable!()
                };
                let item = match src {
                    ForSrc::Words(words, next) => {
                        let Some(item) = words.get_mut(*next) else {
                            pc = exit;
                            continue;
                        };
                        *next += 1;
                        std::mem::take(item)
                    }
                    ForSrc::Range { next, left, step } => {
                        if *left == 0 {
                            pc = exit;
                            continue;
                        }
                        *left -= 1;
                        let n = *next;
                        if *left > 0 {
                            *next += *step;
                        }
                        n.to_string().into_bytes()
                    }
                };
                let name: &str = name;
                if !native {
                    ex.flush(vp, code);
                    let c = code.ast.cmd(*cmd);
                    match exec::for_hooks(vp, code, c).await {
                        Ok(true) => {
                            ex.reload(vp);
                            native = native_ok(vp, prog);
                            pc = prog.loops[0].cont;
                            continue;
                        }
                        Ok(false) => {}
                        Err(u) => {
                            vp.no_errexit = ex.regs[top.ne as usize] != 0;
                            return Err(u);
                        }
                    }
                }
                let r = if vp.vars.get(name).is_some_and(|v| v.nameref) {
                    vp.vars.entry(name).set(item);
                    Ok(())
                } else {
                    vp.assign(name, item)
                };
                if let Err(msg) = r {
                    ex.flush(vp, code);
                    vp.error(&msg).await;
                    return Ok(1);
                }
                // The loop variable's slot, if native operations use it.
                if let Some(i) = prog.slots.iter().position(|s| &**s == name) {
                    let v = match vp.vars.get(name) {
                        Some(v) if !v.nameref && !v.readonly && v.dynamic.is_none() => {
                            match &v.value {
                                Value::Str(s) => slot_decimal(s).map_or(SV::Other, SV::Int),
                                _ => SV::Other,
                            }
                        }
                        _ => SV::Other,
                    };
                    ex.slots[i] = (v, false);
                }
                pc += 1;
            }
            Ins::BodyDone(lp) => {
                ex.regs[prog.loops[*lp as usize].reg as usize] = st;
                pc += 1;
            }
            Ins::LoopExit(lp) => {
                st = ex.regs[prog.loops[*lp as usize].reg as usize];
                if *lp == 0 {
                    ex.flush(vp, code);
                    return Ok(st);
                }
                vp.loops -= 1;
                pc += 1;
            }
            Ins::ForClause {
                lp,
                which,
                expr,
                cmd,
                line,
                blank,
            } => {
                let l = &prog.loops[*lp as usize];
                ex.undo.clear();
                let value = match expr {
                    Some(e) if native => ex.arith(*e),
                    _ => None,
                };
                let value = match value {
                    Some(v) => {
                        ex.line = Some(*line);
                        Some(v)
                    }
                    None => {
                        ex.flush(vp, code);
                        let c = code.ast.cmd(*cmd);
                        let CommandKind::ArithFor(f) = &c.kind else {
                            unreachable!()
                        };
                        let w = match which {
                            Clause::Init => &f.init,
                            Clause::Cond => &f.cond,
                            Clause::Step => &f.step,
                        };
                        vp.line = *line;
                        let r = exec::arith_clause(vp, code, w).await;
                        ex.reload(vp);
                        native = native_ok(vp, prog);
                        match r {
                            Ok(v) => v,
                            Err(u) => match land(vp, &mut ex, *lp, false, u) {
                                Landing::Jump(to) => {
                                    pc = to;
                                    continue;
                                }
                                Landing::Return(r) => {
                                    ex.flush(vp, code);
                                    return r;
                                }
                            },
                        }
                    }
                };
                match value {
                    None => {
                        ex.regs[l.reg as usize] = 1;
                        pc = l.exit;
                    }
                    Some(0) if *which == Clause::Cond && !*blank => pc = l.exit,
                    Some(_) => pc += 1,
                }
            }
        }
    }
}

/// Runs native operations and control flow from pc synchronously, as run_with would,
/// while nothing needs the interpreter, an await, an unwind or errexit (the caller
/// checks that errexit is off; nothing here can turn it on). Returns the pc of the
/// first instruction it leaves to run_with, which runs it as before: a command to
/// interpret or a native one that declined, break or continue, echo, the next word of a
/// for loop, the top loop's end, a spent scheduling budget or a pending signal (both
/// seen at the same checks run_with makes, before anything at that pc ran).
fn spin(vp: &mut Vp, ex: &mut Ex<'_>, mut pc: u32, st: &mut i32) -> u32 {
    let prog = ex.prog;
    loop {
        match &prog.code[pc as usize] {
            Ins::Stmt { op, line, .. } => {
                if matches!(op, Op::Break(..) | Op::Continue(..) | Op::Echo { .. } | Op::Slow) {
                    return pc;
                }
                if !vp.core.sched.pass() {
                    return pc;
                }
                let Some(s) = ex.native(op) else {
                    // run_with interprets it; the budget for it was taken here.
                    vp.core.sched.refund();
                    return pc;
                };
                *st = s;
                ex.line = Some(*line);
                pc += 1;
            }
            Ins::Item => {
                vp.status = *st;
                pc += 1;
            }
            Ins::SetSt(s) => {
                *st = *s;
                pc += 1;
            }
            Ins::NoErrSave(r) => {
                ex.regs[*r as usize] = i32::from(vp.no_errexit);
                vp.no_errexit = true;
                pc += 1;
            }
            Ins::NoErrRestore(r) => {
                vp.no_errexit = ex.regs[*r as usize] != 0;
                pc += 1;
            }
            Ins::Jz(t) => pc = if *st == 0 { *t } else { pc + 1 },
            Ins::Jnz(t) => pc = if *st != 0 { *t } else { pc + 1 },
            Ins::Jmp(t) => pc = *t,
            Ins::LoopEnter(lp) => {
                let l = &prog.loops[*lp as usize];
                vp.loops += 1;
                ex.regs[l.reg as usize] = 0;
                ex.regs[l.ne as usize] = i32::from(vp.no_errexit);
                pc += 1;
            }
            Ins::LoopHead => {
                if !vp.core.sched.pass() {
                    return pc;
                }
                if vp.job.as_ref().is_some_and(|j| j.killed().is_some())
                    || (vp.top && vp.sigs.peek(&vp.core) != 0)
                    || vp.own_sigs.get() != 0
                {
                    vp.core.sched.refund();
                    return pc;
                }
                pc += 1;
            }
            Ins::BodyDone(lp) => {
                ex.regs[prog.loops[*lp as usize].reg as usize] = *st;
                pc += 1;
            }
            Ins::LoopExit(lp) => {
                if *lp == 0 {
                    return pc;
                }
                *st = ex.regs[prog.loops[*lp as usize].reg as usize];
                vp.loops -= 1;
                pc += 1;
            }
            Ins::ForClause {
                lp,
                which,
                expr: Some(e),
                line,
                blank,
                ..
            } => {
                ex.undo.clear();
                let Some(v) = ex.arith(*e) else {
                    return pc;
                };
                ex.line = Some(*line);
                let l = &prog.loops[*lp as usize];
                if v == 0 && *which == Clause::Cond && !*blank {
                    pc = l.exit;
                } else {
                    pc += 1;
                }
            }
            Ins::ForItem { .. } => match ex.for_fast() {
                Some(true) => pc += 1,
                Some(false) => pc = prog.loops[0].exit,
                None => return pc,
            },
            Ins::ForClause { .. } => return pc,
        }
    }
}

/// An unwind (break, continue, or anything else) from a command in loop lp: where to go
/// on, as loop_unwind decides at each level.
fn land(vp: &mut Vp, ex: &mut Ex<'_>, mut lp: u16, mut in_cond: bool, mut u: Unwind) -> Landing {
    loop {
        let l = &ex.prog.loops[lp as usize];
        let status = if in_cond {
            ex.regs[l.reg as usize]
        } else {
            vp.status
        };
        let r = if in_cond && u == Unwind::Continue(1) {
            Some(Ok(status))
        } else {
            exec::loop_unwind(u, status)
        };
        vp.no_errexit = ex.regs[l.ne as usize] != 0;
        match r {
            None => {
                ex.regs[l.reg as usize] = status;
                return Landing::Jump(l.cont);
            }
            Some(Ok(s)) => {
                ex.regs[l.reg as usize] = s;
                return Landing::Jump(l.exit);
            }
            Some(Err(next)) => {
                if l.parent == NONE {
                    return Landing::Return(Err(next));
                }
                vp.loops -= 1;
                u = next;
                in_cond = l.in_cond;
                lp = l.parent;
            }
        }
    }
}

/// Runs a command the way run_list (or the and-or list) would.
async fn interpret(vp: &mut Vp, code: &Rc<Code>, src: Src) -> R {
    match src {
        Src::Item { list, idx } => exec::run_item(vp, code, list, idx as usize).await,
        Src::Pipe { list, idx, k } => {
            let item = &code.ast.list(list).items[idx as usize];
            let p = match k {
                0 => &item.first,
                k => &item.rest[k as usize - 1].1,
            };
            exec::andor_pipeline(vp, code, p).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::{Script, Shell, Step, testhost::TestHost};

    fn check(source: &str, expected: &[u8]) {
        for latency in [false, true] {
            for budget in [1, 1000] {
                let host = Rc::new(TestHost::new());
                host.set_latency(latency);
                let mut shell = Shell::new(host.config());
                shell.start(Script::command(source));
                let status = loop {
                    match shell.step(budget) {
                        Step::Done(status) => break status,
                        Step::Yield | Step::Blocked => {}
                        Step::Idle => panic!("{source}: idle before completion"),
                    }
                };
                assert_eq!(status, 0, "{source}: latency={latency}, budget={budget}");
                assert_eq!(host.stderr(), b"", "{source}");
                assert_eq!(host.stdout(), expected, "{source}");
                assert_eq!(shell.step(budget), Step::Idle, "{source}");
            }
        }
    }

    #[test]
    fn flat_loops_preserve_negative_zero_text() {
        for source in [
            "x=-0; for ((i=0;i<1;i++)); do echo \"$x\"; done",
            "x=-0; for ((i=0;i<1;i++)); do echo $x; done",
            "x=-0; for ((i=0;i<1;i++)); do y=$x; done; echo \"$y\"",
            "for ((i=0;i<1;i++)); do x=-0; echo \"$x\"; done",
            "for x in -0; do echo \"$x\"; done",
            "for x in -0; do y=$x; done; echo \"$y\"",
            "x=-0; i=0; while ((i++<1)); do echo \"$x\"; done",
            "x=-0; i=0; until ((i++>=1)); do echo \"$x\"; done",
            "x=-0; for ((i=0;i<2;i++)); do ((i)) && x=-0; done; echo \"$x\"",
        ] {
            check(source, b"-0\n");
        }
    }

    #[test]
    fn flat_and_or_lists_try_each_operator_on_the_last_status() {
        for (source, expected) in [
            (
                "for x in a b; do [ x = y ] && echo yes || echo no; done",
                &b"no
no
"[..],
            ),
            (
                "i=0; while [ $i -lt 2 ]; do [[ x == y ]] && echo yes || echo no; i=$((i+1)); done",
                &b"no
no
"[..],
            ),
            (
                "for x in a; do [ x = x ] && echo yes || echo no; done",
                &b"yes
"[..],
            ),
            (
                "for x in a; do [ x = y ] || [ x = z ] && echo yes || echo no; done",
                &b"no
"[..],
            ),
            (
                "for x in a; do [ x = x ] || echo skipped && echo ran; done",
                &b"ran
"[..],
            ),
        ] {
            check(source, expected);
        }
    }

    #[test]
    fn flat_loops_normalize_negative_zero_after_arithmetic() {
        for (source, expected) in [
            (
                "x=-0; for ((i=0;i<1;i++)); do ((x+=0)); echo \"$x\"; done",
                &b"0\n"[..],
            ),
            (
                "x=-0; for ((i=0;i<1;i++)); do ((x++)); echo \"$x\"; done",
                &b"1\n"[..],
            ),
            ("for x in -0; do ((x+=0)); echo \"$x\"; done", &b"0\n"[..]),
        ] {
            check(source, expected);
        }
    }
}
