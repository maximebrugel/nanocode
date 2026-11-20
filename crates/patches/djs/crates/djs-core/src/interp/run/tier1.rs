use super::hot::{ld, st, word};
use super::{Exec, fast, suspend};
use crate::bytecode::{self, OPS};
use crate::heap::cells::{FuncTemplate, ValueArray};
use crate::heap::{Atom, atoms};
use crate::interp::conv;
use crate::interp::frame::HEADER;
use crate::interp::realm::{self, Intrinsic};
use crate::value::Value;
use alloc::vec::Vec;
use core::cell::Cell;

type Scan = (u32, i64, Option<(u32, u32)>);

fn body(b: &[u8], head: u32, end: u32) -> Result<Scan, u8> {
    let (mut at, mut fuel, mut site, mut prev, mut call, mut field) = (head, 0, None, head, false, false);
    while at < end {
        let w = word(b, at);
        let op = w as u8;
        let offset = match op {
            fast::ITER_NEXT_LOC if site.is_none() => {
                site = Some((at, prev));
                (w >> 24) as u32 as i32
            }
            fast::GET_METHOD
                if site.is_none()
                    && word(b, at + u32::from(OPS[op as usize].size)) as u8 == fast::CALL0 =>
            {
                site = Some((at, prev));
                call = true;
                0
            }
            fast::CALL0 if call => 0,
            fast::GET_FIELD => {
                let key = Atom::from_id((w >> 8) as u32);
                if key != atoms::DONE && key != atoms::VALUE {
                    return Err(op);
                }
                field = true;
                0
            }
            fast::JUMP | fast::JUMP_IF_FALSE | fast::JUMP_IF_TRUE => (w >> 8) as u32 as i32,
            fast::JUMP8 | fast::JUMP_IF_FALSE8 | fast::JUMP_IF_TRUE8 => (w >> 8) as u8 as i8 as i32,
            fast::GET_LOC
            | fast::PUT_LOC
            | fast::SET_LOC
            | fast::GET_ARG
            | fast::GET_LOC0..=fast::GET_LOC3
            | fast::PUT_LOC0..=fast::PUT_LOC3
            | fast::SET_LOC0..=fast::SET_LOC3
            | fast::GET_ARG0..=fast::GET_ARG3
            | fast::GET_LOC8
            | fast::PUT_LOC8
            | fast::SET_LOC8
            | fast::GET_ARG8
            | fast::PUSH_I8
            | fast::PUSH_I32
            | fast::PUSH0..=fast::PUSH3
            | fast::PUSH_CONST
            | fast::PUSH_CONST8
            | fast::UNDEFINED
            | fast::NULL
            | fast::PUSH_TRUE
            | fast::PUSH_FALSE
            | fast::DROP
            | fast::DUP
            | fast::INC
            | fast::DEC
            | fast::INC_LOC
            | fast::DEC_LOC
            | fast::ADD
            | fast::SUB
            | fast::MUL
            | fast::DIV
            | fast::MOD
            | fast::BAND
            | fast::BOR
            | fast::BXOR
            | fast::SHL
            | fast::SAR
            | fast::LT
            | fast::LE
            | fast::GT
            | fast::GE
            | fast::STRICT_EQ
            | fast::STRICT_NE
            | fast::NOT
            | fast::IS_UNDEFINED
            | fast::IS_NULLISH
            | fast::GET_ELEM
            | fast::PUT_ELEM
            | fast::ADD_LOC8
            | fast::INC_LT
            | fast::INC_LT_LOC => 0,
            _ => return Err(op),
        };
        let info = &OPS[op as usize];
        fuel += info.cost.fuel() as i64;
        prev = at;
        at += info.size as u32;
        let to = at as i64 + offset as i64;
        if to < at as i64 {
            return if to == head as i64 {
                match (field, call) {
                    (true, false) => Err(fast::GET_FIELD),
                    _ => Ok((at, fuel, site)),
                }
            } else {
                Err(op)
            };
        }
    }
    Err(0)
}

#[derive(Clone, Copy)]
enum Cmp {
    Ge,
    Gt,
    Le,
    Lt,
}

impl Cmp {
    fn mask(self, when: bool) -> u8 {
        let m = match self {
            Cmp::Ge => 0b0110,
            Cmp::Gt => 0b0100,
            Cmp::Le => 0b0011,
            Cmp::Lt => 0b0001,
        };
        if when { m } else { !m & 0b1111 }
    }
}

#[inline(always)]
fn order_int(x: i32, y: i32) -> u32 {
    u32::from(x > y) * 2 + u32::from(x == y)
}

#[inline(always)]
fn order_float(x: f64, y: f64) -> u32 {
    if x < y {
        0
    } else if x == y {
        1
    } else if x > y {
        2
    } else {
        3
    }
}

#[inline(always)]
fn number(n: f64) -> Value {
    if n.abs() > f64::from(i32::MAX) {
        Value::from_raw(n.to_bits())
    } else {
        conv::number(n)
    }
}

#[inline(always)]
fn add(p: Value, q: Value) -> Option<Value> {
    match (p.as_int(), q.as_int()) {
        (Some(x), Some(y)) => Some(match x.checked_add(y) {
            Some(z) => Value::int(z),
            None => Value::from_raw((f64::from(x) + f64::from(y)).to_bits()),
        }),
        _ => Some(number(p.as_number()? + q.as_number()?)),
    }
}

#[inline(always)]
fn order(a: Value, c: Value) -> Option<u32> {
    if let (Some(x), Some(y)) = (a.as_int(), c.as_int()) {
        return Some(order_int(x, y));
    }
    Some(order_float(a.as_number()?, c.as_number()?))
}

fn range(mask: u8, n: i32) -> Option<(i32, u32)> {
    let (lo, hi) = match mask & 0b111 {
        0b001 => (i32::MIN, n.checked_sub(1)?),
        0b011 => (i32::MIN, n),
        0b100 => (n.checked_add(1)?, i32::MAX),
        0b110 => (n, i32::MAX),
        _ => return None,
    };
    Some((lo, hi.wrapping_sub(lo) as u32))
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum I {
    Add {
        a: u8,
        c: u8,
        d: u8,
    },
    BAnd {
        a: u8,
        c: u8,
        d: u8,
    },
    BOr {
        a: u8,
        c: u8,
        d: u8,
    },
    BXor {
        a: u8,
        c: u8,
        d: u8,
    },
    Branch {
        a: u8,
        c: u8,
        mask: u8,
        to: u16,
    },
    Compare {
        a: u8,
        c: u8,
        d: u8,
        mask: u8,
    },
    Div {
        a: u8,
        c: u8,
        d: u8,
    },
    Equal {
        a: u8,
        c: u8,
        d: u8,
        ne: bool,
    },
    GetElem {
        a: u8,
        c: u8,
        d: u8,
    },
    IsNullish {
        a: u8,
        d: u8,
    },
    IsUndefined {
        a: u8,
        d: u8,
    },
    Jump {
        to: u16,
    },
    Latch {
        delta: i32,
        lo: i32,
        span: u32,
        to: u16,
        x: u8,
    },
    JumpIf {
        a: u8,
        to: u16,
        when: bool,
    },
    Leave {
        exit: u16,
    },
    Mod {
        a: u8,
        c: u8,
        d: u8,
    },
    Mov {
        a: u8,
        d: u8,
    },
    Mul {
        a: u8,
        c: u8,
        d: u8,
    },
    Nop,
    Not {
        a: u8,
        d: u8,
    },
    PutElem {
        a: u8,
        after: u16,
        c: u8,
        v: u8,
    },
    Resume,
    Sar {
        a: u8,
        c: u8,
        d: u8,
    },
    Shl {
        a: u8,
        c: u8,
        d: u8,
    },
    Step {
        a: u8,
        d: u8,
        delta: i32,
    },
    StepBranch {
        c: u8,
        delta: i32,
        exit: u16,
        fuel: u32,
        mask: u8,
        to: u16,
        x: u8,
    },
    Sub {
        a: u8,
        c: u8,
        d: u8,
    },
}

impl I {
    fn reads(&self, x: u8) -> bool {
        match *self {
            I::Add { a, c, .. }
            | I::BAnd { a, c, .. }
            | I::BOr { a, c, .. }
            | I::BXor { a, c, .. }
            | I::Branch { a, c, .. }
            | I::Compare { a, c, .. }
            | I::Div { a, c, .. }
            | I::Equal { a, c, .. }
            | I::GetElem { a, c, .. }
            | I::Mod { a, c, .. }
            | I::Mul { a, c, .. }
            | I::Sar { a, c, .. }
            | I::Shl { a, c, .. }
            | I::Sub { a, c, .. }
            | I::StepBranch { x: a, c, .. } => a == x || c == x,
            I::IsNullish { a, .. }
            | I::IsUndefined { a, .. }
            | I::JumpIf { a, .. }
            | I::Latch { x: a, .. }
            | I::Mov { a, .. }
            | I::Not { a, .. }
            | I::Step { a, .. } => a == x,
            I::PutElem { a, c, v, .. } => a == x || c == x || v == x,
            I::Jump { .. } | I::Leave { .. } | I::Nop | I::Resume => false,
        }
    }

    fn dst(&mut self) -> Option<&mut u8> {
        match self {
            I::Add { d, .. }
            | I::BAnd { d, .. }
            | I::BOr { d, .. }
            | I::BXor { d, .. }
            | I::Compare { d, .. }
            | I::Div { d, .. }
            | I::Equal { d, .. }
            | I::GetElem { d, .. }
            | I::IsNullish { d, .. }
            | I::IsUndefined { d, .. }
            | I::Mod { d, .. }
            | I::Mov { d, .. }
            | I::Mul { d, .. }
            | I::Not { d, .. }
            | I::Sar { d, .. }
            | I::Shl { d, .. }
            | I::Step { d, .. }
            | I::Sub { d, .. } => Some(d),
            _ => None,
        }
    }

    fn target(&mut self) -> Option<&mut u16> {
        match self {
            I::Branch { to, .. }
            | I::Jump { to }
            | I::JumpIf { to, .. }
            | I::Latch { to, .. }
            | I::StepBranch { to, .. } => Some(to),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct Ins {
    exit: u16,
    fuel: u32,
    i: I,
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum K {
    Accumulate,
    Add,
    AddF,
    BAnd,
    BOr,
    BXor,
    Branch,
    Compare,
    Div,
    Equal,
    FoldAnd,
    FoldMod,
    FoldMul,
    FoldOr,
    FoldXor,
    GetElem,
    IsNullish,
    IsUndefined,
    Jump,
    JumpIf,
    Latch,
    Leave,
    Mod,
    Mov,
    Mul,
    Nop,
    Not,
    PutElem,
    Resume,
    Sar,
    Shl,
    Step,
    StepBranch,
    Sub,
}

#[derive(Clone)]
struct Op {
    a: u8,
    aux: u16,
    c: u8,
    chk: bool,
    d: u8,
    exit: u16,
    fuel: u32,
    k: Cell<K>,
    l: bool,
    lc: u8,
    lfuel: u32,
    lx: u8,
    m: bool,
    ma: u8,
    mask: u8,
    md: u8,
    mfuel: u32,
    n: i32,
    nx: u8,
    ra: u8,
    rd: u8,
    res: bool,
    rfuel: u32,
    span: u32,
    to: u16,
    w: bool,
    wa: u8,
    wd: u8,
    wfuel: u32,
}

fn lower(ins: &Ins) -> Op {
    let mut o = Op {
        a: 0,
        aux: 0,
        c: 0,
        chk: false,
        d: 0,
        exit: ins.exit,
        fuel: ins.fuel,
        k: Cell::new(K::Nop),
        l: false,
        lc: 0,
        lfuel: 0,
        lx: 0,
        m: false,
        ma: 0,
        mask: 0,
        md: 0,
        mfuel: 0,
        n: 0,
        nx: 0,
        ra: 0,
        rd: 0,
        res: false,
        rfuel: 0,
        span: 0,
        to: 0,
        w: false,
        wa: 0,
        wd: 0,
        wfuel: 0,
    };
    let mut abc = |k: K, a: u8, c: u8, d: u8| {
        o.k = Cell::new(k);
        (o.a, o.c, o.d) = (a, c, d);
    };
    match ins.i {
        I::Add { a, c, d } => abc(K::Add, a, c, d),
        I::BAnd { a, c, d } => abc(K::BAnd, a, c, d),
        I::BOr { a, c, d } => abc(K::BOr, a, c, d),
        I::BXor { a, c, d } => abc(K::BXor, a, c, d),
        I::Div { a, c, d } => abc(K::Div, a, c, d),
        I::GetElem { a, c, d } => abc(K::GetElem, a, c, d),
        I::Mod { a, c, d } => abc(K::Mod, a, c, d),
        I::Mul { a, c, d } => abc(K::Mul, a, c, d),
        I::Sar { a, c, d } => abc(K::Sar, a, c, d),
        I::Shl { a, c, d } => abc(K::Shl, a, c, d),
        I::Sub { a, c, d } => abc(K::Sub, a, c, d),
        I::IsNullish { a, d } => abc(K::IsNullish, a, 0, d),
        I::IsUndefined { a, d } => abc(K::IsUndefined, a, 0, d),
        I::Mov { a, d } => abc(K::Mov, a, 0, d),
        I::Not { a, d } => abc(K::Not, a, 0, d),
        I::Nop => abc(K::Nop, 0, 0, 0),
        I::Resume => abc(K::Resume, 0, 0, 0),
        I::PutElem { a, after, c, v } => {
            abc(K::PutElem, a, c, v);
            o.aux = after;
        }
        I::Step { a, d, delta } => abc(K::Step, a, delta as i8 as u8, d),
        I::Branch { a, c, mask, to } => {
            abc(K::Branch, a, c, 0);
            (o.mask, o.to) = (mask, to);
        }
        I::Compare { a, c, d, mask } => {
            abc(K::Compare, a, c, d);
            o.mask = mask;
        }
        I::Equal { a, c, d, ne } => {
            abc(K::Equal, a, c, d);
            o.mask = u8::from(ne);
        }
        I::Jump { to } => {
            abc(K::Jump, 0, 0, 0);
            o.to = to;
        }
        I::JumpIf { a, to, when } => {
            abc(K::JumpIf, a, 0, 0);
            (o.mask, o.to) = (u8::from(when), to);
        }
        I::Latch {
            delta,
            lo,
            span,
            to,
            x,
        } => {
            abc(K::Latch, x, delta as i8 as u8, 0);
            (o.n, o.span, o.to) = (lo, span, to);
        }
        I::Leave { exit } => {
            abc(K::Leave, 0, 0, 0);
            o.to = exit;
        }
        I::StepBranch {
            c,
            delta,
            exit,
            fuel,
            mask,
            to,
            x,
        } => {
            abc(K::StepBranch, x, c, delta as i8 as u8);
            (o.aux, o.mask, o.n, o.to) = (exit, mask, fuel as i32, to);
        }
    }
    o
}

#[inline(never)]
fn accumulate(r: &mut [Value; 256], o: &Op, a: usize, c: usize, fuel: &mut i64, pass: i64) -> Option<u16> {
    let (mut s, mut y, mut left) = (r[a], r[c].as_int().unwrap_or_default(), *fuel);
    let step = i32::from(o.lc as i8);
    let lfuel = i64::from(o.lfuel);
    let (lo, hi, at) = (i64::from(o.n), i64::from(o.n) + i64::from(o.span), i64::from(y));
    let room = match step.signum() {
        1 => (hi - at) / i64::from(step),
        -1 => (at - lo) / -i64::from(step),
        _ => i64::MAX,
    };
    let m = if lfuel > 0 && left > pass && hi <= i64::from(i32::MAX) && (lo..=hi).contains(&at) {
        room.min((left - pass - 1) / lfuel)
    } else {
        0
    };
    if m > 0 {
        let mut sf = match s.as_int() {
            Some(x) => f64::from(x),
            None => f64::from_bits(s.to_bits()),
        };
        let (mut yf, d) = (f64::from(y), f64::from(step));
        for _ in 0..m {
            sf += yf;
            yf += d;
        }
        s = number(sf);
        y = (at + m * i64::from(step)) as i32;
        left -= m * lfuel;
    }
    let out = loop {
        s = match s.as_int() {
            Some(x) => match x.checked_add(y) {
                Some(z) => Value::int(z),
                None => Value::from_raw((f64::from(x) + f64::from(y)).to_bits()),
            },
            None => number(f64::from_bits(s.to_bits()) + f64::from(y)),
        };
        let Some(n) = y.checked_add(step) else {
            left -= o.fuel as i64;
            break Some(o.aux);
        };
        y = n;
        left -= o.lfuel as i64;
        if n.wrapping_sub(o.n) as u32 > o.span {
            break None;
        }
        if left <= pass {
            break Some(0);
        }
    };
    r[a] = s;
    r[c] = Value::int(y);
    *fuel = left;
    out
}

#[inline(never)]
fn fold(
    r: &mut [Value; 256],
    o: &Op,
    q: &Op,
    fuel: &mut i64,
    pass: i64,
    op: impl Fn(i32, i32) -> Option<Value>,
) -> Result<Option<u16>, ()> {
    let (a, c, t, sx, x) = (o.a as usize, o.c as usize, o.d as usize, q.a as usize, q.lx as usize);
    let (Some(mut i), Some(y)) = (r[x].as_int(), r[c].as_int()) else {
        return Err(());
    };
    let inv = if a == x {
        None
    } else {
        Some(r[a].as_int().ok_or(())?)
    };
    let step = i32::from(q.lc as i8);
    let cost = i64::from(o.mfuel) + i64::from(q.lfuel);
    let (mut s, mut tv, mut left) = (r[sx], None, *fuel);
    let out = loop {
        let Some(v) = op(inv.unwrap_or(i), y) else {
            break Err(());
        };
        let Some(w) = add(s, v) else {
            break Err(());
        };
        (s, tv) = (w, Some(v));
        let Some(n) = i.checked_add(step) else {
            left -= i64::from(o.mfuel) + i64::from(q.fuel);
            break Ok(Some(q.aux));
        };
        i = n;
        left -= cost;
        if n.wrapping_sub(q.n) as u32 > q.span {
            break Ok(None);
        }
        if left <= pass {
            break Ok(Some(0));
        }
    };
    r[sx] = s;
    r[x] = Value::int(i);
    if let Some(v) = tv {
        r[t] = v;
    }
    *fuel = left;
    out
}

#[derive(Clone, Copy)]
struct Exit {
    at: u32,
    ip: u32,
    len: u32,
    rollback: bool,
}

#[derive(Clone, Copy)]
enum Src {
    Fresh(u8),
    Shadow(u32),
    Slot(u32),
    Value(Value),
}

enum Dest {
    Head,
    In,
    Out,
}

struct Frame {
    bp: u32,
    consts: u32,
    depth: u32,
    head: u32,
    lp: u32,
    sp: u32,
    stop: u32,
    vals: u32,
}

impl Frame {
    fn slot(&self, i: u32) -> u32 {
        self.vals + 8 * i
    }
}

struct Gen {
    at: u32,
    call: bool,
    frame: Frame,
    guard: [u32; 2],
    resume: u32,
    resume_pc: u32,
    yield_cost: u32,
    yield_ip: u32,
}

#[derive(Clone, Copy)]
struct Virtual {
    flag: u8,
    local: u8,
    obj: u8,
    slot: u32,
    value: u8,
}

#[derive(Clone, Default)]
struct Plan {
    barrier: usize,
    exits: Vec<Exit>,
    extra: i64,
    fixes: Vec<(usize, u32)>,
    fuel: u32,
    fuse: Option<usize>,
    init: Vec<Src>,
    labels: Vec<(u32, u32, Option<u16>)>,
    mark: Option<u16>,
    outs: Vec<(usize, u16)>,
    entry: Option<u32>,
    prog: Vec<Ins>,
    result: Option<Virtual>,
    rollback: Option<u16>,
    shadows: Vec<(u8, u8)>,
    stacks: Vec<u8>,
    stk: Vec<u8>,
    writes: Vec<(u8, u32)>,
}

impl Plan {
    fn reg(&mut self, src: Src) -> Option<u8> {
        let found = self.init.iter().position(|s| match (*s, src) {
            (Src::Shadow(x), Src::Shadow(y)) | (Src::Slot(x), Src::Slot(y)) => x == y,
            (Src::Fresh(x), Src::Fresh(y)) => x == y,
            (Src::Value(x), Src::Value(y)) => x.to_bits() == y.to_bits(),
            _ => false,
        });
        if let Some(g) = found {
            return u8::try_from(g).ok();
        }
        let g = u8::try_from(self.init.len()).ok()?;
        self.init.push(src);
        Some(g)
    }

    fn temp(&mut self, f: &Frame, j: usize) -> Option<u8> {
        self.reg(Src::Slot(f.slot(f.sp + j as u32)))
    }

    fn exit_at(&mut self, ip: u32) -> Option<u16> {
        let e = u16::try_from(self.exits.len()).ok()?;
        self.exits.push(Exit {
            at: self.stacks.len() as u32,
            ip,
            len: self.stk.len() as u32,
            rollback: false,
        });
        self.stacks.extend_from_slice(&self.stk);
        Some(e)
    }

    fn mark(&mut self, ip: u32) -> Option<()> {
        if self.mark.is_none() {
            self.mark = Some(match self.rollback {
                Some(e) => e,
                None => self.exit_at(ip)?,
            });
        }
        Some(())
    }

    fn emit(&mut self, i: I, cost: u32) -> Option<usize> {
        let exit = self.mark.take()?;
        self.prog.push(Ins {
            exit,
            fuel: core::mem::take(&mut self.fuel) + cost,
            i,
        });
        self.fuse = None;
        Some(self.prog.len() - 1)
    }

    fn settle(&mut self) {
        if let Some(exit) = self.mark.take() {
            self.prog.push(Ins {
                exit,
                fuel: core::mem::take(&mut self.fuel),
                i: I::Nop,
            });
        }
        self.fuse = None;
    }

    fn materialize(&mut self, f: &Frame, n: usize, only: Option<u8>) -> Option<()> {
        let mut settled = false;
        for j in 0..n {
            let t = self.temp(f, j)?;
            let g = self.stk[j];
            if g == t || only.is_some_and(|x| x != g) {
                continue;
            }
            if !settled {
                self.settle();
                settled = true;
            }
            self.prog.push(Ins {
                exit: 0,
                fuel: 0,
                i: I::Mov { a: g, d: t },
            });
            self.stk[j] = t;
        }
        Some(())
    }

    fn push(&mut self, ip: u32, cost: u32, src: Src) -> Option<()> {
        self.mark(ip)?;
        self.fuel += cost;
        let g = self.reg(src)?;
        self.stk.push(g);
        Some(())
    }

    fn binary(
        &mut self,
        f: &Frame,
        ip: u32,
        cost: u32,
        mk: impl Fn(u8, u8, u8) -> I,
    ) -> Option<()> {
        self.mark(ip)?;
        let c = self.stk.pop()?;
        let a = self.stk.pop()?;
        let d = self.temp(f, self.stk.len())?;
        let k = self.emit(mk(a, c, d), cost)?;
        self.stk.push(d);
        self.fuse = Some(k);
        Some(())
    }

    fn unary(&mut self, f: &Frame, ip: u32, cost: u32, mk: impl Fn(u8, u8) -> I) -> Option<()> {
        self.mark(ip)?;
        let a = self.stk.pop()?;
        let d = self.temp(f, self.stk.len())?;
        let k = self.emit(mk(a, d), cost)?;
        self.stk.push(d);
        self.fuse = Some(k);
        Some(())
    }

    fn written(&mut self, x: u8, slot: u32) {
        if !self.writes.iter().any(|w| w.0 == x) {
            self.writes.push((x, slot));
        }
    }

    fn step(&mut self, f: &Frame, ip: u32, cost: u32, slot: u32, delta: i32) -> Option<()> {
        if self.result.is_some_and(|v| v.slot == slot) {
            return None;
        }
        let x = self.reg(Src::Slot(slot))?;
        self.materialize(f, self.stk.len(), Some(x))?;
        self.mark(ip)?;
        self.emit(I::Step { a: x, d: x, delta }, cost)?;
        self.written(x, slot);
        Some(())
    }

    fn put(&mut self, f: &Frame, ip: u32, cost: u32, slot: u32, keep: bool) -> Option<()> {
        if self.result.is_some_and(|v| v.slot == slot) {
            return None;
        }
        let x = self.reg(Src::Slot(slot))?;
        let src = *self.stk.last()?;
        let n = self.stk.len() - 1;
        self.materialize(f, n, Some(x))?;
        let last = self.prog.len().checked_sub(1);
        let fused = !keep
            && self.mark.is_none()
            && self.fuse.is_some()
            && self.fuse == last
            && self.temp(f, n)? == src;
        if fused {
            let ins = self.prog.last_mut()?;
            let d = ins.i.dst()?;
            if *d != src {
                return None;
            }
            *d = x;
            ins.fuel += cost;
            self.stk.pop();
            self.fuse = None;
        } else {
            self.mark(ip)?;
            if !keep {
                self.stk.pop();
            }
            self.emit(I::Mov { a: src, d: x }, cost)?;
        }
        self.written(x, slot);
        Some(())
    }

    fn jump(
        &mut self,
        f: &Frame,
        ip: u32,
        cost: u32,
        target: u32,
        cond: Option<bool>,
    ) -> Option<bool> {
        let n = self.stk.len().checked_sub(usize::from(cond.is_some()))?;
        let dest = if target == f.head {
            Dest::Head
        } else if target >= f.stop {
            Dest::Out
        } else if target > ip {
            Dest::In
        } else {
            return None;
        };
        match dest {
            Dest::Head if n != f.depth as usize => return None,
            Dest::Head => self.materialize(f, n, None)?,
            Dest::In => {
                self.materialize(f, n, None)?;
                match self.labels.iter().find(|l| l.0 == target) {
                    Some(l) if l.1 != n as u32 => return None,
                    Some(_) => {}
                    None => self.labels.push((target, n as u32, None)),
                }
            }
            _ => {}
        }
        let k = match cond {
            Some(when) => {
                let top = *self.stk.last()?;
                let last = self.prog.len().checked_sub(1);
                let fusable = self.mark.is_none() && self.fuse.is_some() && self.fuse == last;
                match self.prog.last().map(|i| i.i) {
                    Some(I::Compare { a, c, d, mask }) if fusable && d == top => {
                        self.stk.pop();
                        let k = self.prog.len() - 1;
                        let mask = if when { mask } else { !mask & 0b1111 };
                        self.prog[k].i = I::Branch { a, c, mask, to: 0 };
                        self.prog[k].fuel += cost;
                        self.fuse = None;
                        match k.checked_sub(1).map(|s| (s, self.prog[s].i)) {
                            Some((
                                s,
                                I::Step {
                                    a: sa,
                                    d: sd,
                                    delta,
                                },
                            )) if s >= self.barrier && sa == sd && sd == a => {
                                let br = self.prog.pop()?;
                                let bound = match self.init[usize::from(c)] {
                                    Src::Value(v) => v.as_int().and_then(|n| range(mask, n)),
                                    Src::Fresh(_) | Src::Shadow(_) | Src::Slot(_) => None,
                                };
                                let step = &mut self.prog[s];
                                step.i = match bound {
                                    Some((lo, span)) => I::Latch {
                                        delta,
                                        lo,
                                        span,
                                        to: 0,
                                        x: a,
                                    },
                                    None => I::StepBranch {
                                        c,
                                        delta,
                                        exit: br.exit,
                                        fuel: step.fuel,
                                        mask,
                                        to: 0,
                                        x: a,
                                    },
                                };
                                step.fuel += br.fuel;
                                s
                            }
                            _ => k,
                        }
                    }
                    _ => {
                        self.mark(ip)?;
                        self.stk.pop();
                        self.emit(
                            I::JumpIf {
                                a: top,
                                to: 0,
                                when,
                            },
                            cost,
                        )?
                    }
                }
            }
            None => {
                self.mark(ip)?;
                match dest {
                    Dest::Out => {
                        let exit = self.exit_at(target)?;
                        self.emit(I::Leave { exit }, cost)?;
                        return Some(false);
                    }
                    _ => self.emit(I::Jump { to: 0 }, cost)?,
                }
            }
        };
        match dest {
            Dest::Head => {}
            Dest::In => self.fixes.push((k, target)),
            Dest::Out => {
                let exit = self.exit_at(target)?;
                self.outs.push((k, exit));
            }
        }
        Some(cond.is_some())
    }

    fn resume(&mut self, b: &[u8], ip: u32, g: &Gen, shadows: &[u32]) -> Option<()> {
        self.mark(ip)?;
        let e = self.mark?;
        self.exits[usize::from(e)].rollback = true;
        self.fuel += g.resume;
        self.extra += i64::from(g.resume);
        self.emit(I::Resume, 0)?;
        for &x in shadows {
            let a = self.reg(Src::Slot(x))?;
            let d = self.reg(Src::Shadow(x))?;
            self.prog.push(Ins {
                exit: 0,
                fuel: 0,
                i: I::Mov { a, d },
            });
            self.shadows.push((a, d));
        }
        let saved = core::mem::take(&mut self.stk);
        self.rollback = Some(e);
        let v = self.trace(b, g)?;
        self.rollback = None;
        self.stk = saved;
        self.stk.push(v);
        Some(())
    }

    fn trace(&mut self, b: &[u8], g: &Gen) -> Option<u8> {
        let f = &g.frame;
        let (mut ip, mut back) = (g.resume_pc, false);
        loop {
            if ip == g.yield_ip {
                if self.stk.len() != 1 {
                    return None;
                }
                self.fuel += g.yield_cost;
                self.extra += i64::from(g.yield_cost);
                let last = self.prog.last_mut()?;
                last.fuel += core::mem::take(&mut self.fuel);
                self.mark = None;
                self.fuse = None;
                return self.stk.pop();
            }
            if ip >= f.stop {
                return None;
            }
            let w = word(b, ip);
            let op = w as u8;
            let info = &OPS[op as usize];
            let cost = info.cost.fuel();
            let next = ip + info.size as u32;
            self.extra += i64::from(cost);
            if op == fast::PUT_ELEM {
                return None;
            }
            if simple(self, b, f, ip)? {
                ip = next;
                continue;
            }
            let (offset, cond) = match op {
                fast::JUMP => ((w >> 8) as u32 as i32, None),
                fast::JUMP8 => ((w >> 8) as u8 as i8 as i32, None),
                fast::JUMP_IF_FALSE | fast::JUMP_IF_TRUE => {
                    ((w >> 8) as u32 as i32, Some(op == fast::JUMP_IF_TRUE))
                }
                fast::JUMP_IF_FALSE8 | fast::JUMP_IF_TRUE8 => {
                    ((w >> 8) as u8 as i8 as i32, Some(op == fast::JUMP_IF_TRUE8))
                }
                _ => return None,
            };
            let target = u32::try_from(next as i64 + offset as i64).ok()?;
            let backward = target < next;
            if backward {
                if back {
                    return None;
                }
                back = true;
            }
            match cond {
                None => {
                    self.mark(ip)?;
                    self.fuel += cost;
                    ip = target;
                }
                Some(when) => {
                    self.side(ip, cost, when != backward)?;
                    ip = if backward { target } else { next };
                }
            }
        }
    }

    fn side(&mut self, ip: u32, cost: u32, when: bool) -> Option<()> {
        let top = *self.stk.last()?;
        let last = self.prog.len().checked_sub(1);
        let fusable = self.mark.is_none() && self.fuse.is_some() && self.fuse == last;
        let k = match self.prog.last().map(|i| i.i) {
            Some(I::Compare { a, c, d, mask }) if fusable && d == top => {
                self.stk.pop();
                let k = self.prog.len() - 1;
                let mask = if when { mask } else { !mask & 0b1111 };
                self.prog[k].i = I::Branch { a, c, mask, to: 0 };
                self.prog[k].fuel += cost;
                self.fuse = None;
                match k.checked_sub(1).map(|s| (s, self.prog[s].i)) {
                    Some((
                        s,
                        I::Step {
                            a: sa,
                            d: sd,
                            delta,
                        },
                    )) if s >= self.barrier && sa == sd && sd == a => {
                        let br = self.prog.pop()?;
                        let step = &mut self.prog[s];
                        step.i = I::StepBranch {
                            c,
                            delta,
                            exit: br.exit,
                            fuel: step.fuel,
                            mask,
                            to: 0,
                            x: a,
                        };
                        step.fuel += br.fuel;
                        s
                    }
                    _ => k,
                }
            }
            _ => {
                self.mark(ip)?;
                self.stk.pop();
                self.emit(I::JumpIf { a: top, to: 0, when }, cost)?
            }
        };
        let e = self.rollback?;
        self.outs.push((k, e));
        Some(())
    }
}

fn simple(p: &mut Plan, b: &[u8], f: &Frame, ip: u32) -> Option<bool> {
    let w = word(b, ip);
    let op = w as u8;
    let info = &OPS[op as usize];
    let cost = info.cost.fuel();
    let next = ip + info.size as u32;
    let imm16 = (w >> 8) as u16 as u32;
    let imm32 = (w >> 8) as u32;
    let loc = |i: u32| Src::Slot(f.slot(f.lp + i));
    let arg = |i: u32| Src::Slot(f.slot(f.bp + 2 + i));
    let int = |i: i32| Src::Value(Value::int(i));
    match op {
            fast::GET_LOC => p.push(ip, cost, loc(imm16))?,
        fast::GET_LOC0..=fast::GET_LOC3 => {
            p.push(ip, cost, loc((op - fast::GET_LOC0) as u32))?
        }
        fast::GET_LOC8 | fast::ADD_LOC8 => p.push(ip, cost, loc(imm16 & 0xFF))?,
        fast::GET_ARG => p.push(ip, cost, arg(imm16))?,
        fast::GET_ARG0..=fast::GET_ARG3 => {
            p.push(ip, cost, arg((op - fast::GET_ARG0) as u32))?
        }
        fast::GET_ARG8 => p.push(ip, cost, arg(imm16 & 0xFF))?,
        fast::PUT_LOC => p.put(f, ip, cost, f.slot(f.lp + imm16), false)?,
        fast::PUT_LOC0..=fast::PUT_LOC3 => {
            p.put(f, ip, cost, f.slot(f.lp + (op - fast::PUT_LOC0) as u32), false)?
        }
        fast::PUT_LOC8 => p.put(f, ip, cost, f.slot(f.lp + (imm16 & 0xFF)), false)?,
        fast::SET_LOC => p.put(f, ip, cost, f.slot(f.lp + imm16), true)?,
        fast::SET_LOC0..=fast::SET_LOC3 => {
            p.put(f, ip, cost, f.slot(f.lp + (op - fast::SET_LOC0) as u32), true)?
        }
        fast::SET_LOC8 => p.put(f, ip, cost, f.slot(f.lp + (imm16 & 0xFF)), true)?,
        fast::PUSH_I8 => p.push(ip, cost, int(imm32 as u8 as i8 as i32))?,
        fast::PUSH_I32 => p.push(ip, cost, int(imm32 as i32))?,
        fast::PUSH0..=fast::PUSH3 => p.push(ip, cost, int((op - fast::PUSH0) as i32))?,
        fast::PUSH_CONST | fast::PUSH_CONST8 => {
            if f.consts == 0 {
            return None;
            }
            let i = if op == fast::PUSH_CONST {
            imm32
            } else {
            imm16 & 0xFF
            };
            p.push(
            ip,
            cost,
            Src::Value(ld(b, ValueArray::VALUES.at(f.consts, i))),
            )?
        }
        fast::UNDEFINED => p.push(ip, cost, Src::Value(Value::UNDEFINED))?,
        fast::NULL => p.push(ip, cost, Src::Value(Value::NULL))?,
        fast::PUSH_TRUE => p.push(ip, cost, Src::Value(Value::TRUE))?,
        fast::PUSH_FALSE => p.push(ip, cost, Src::Value(Value::FALSE))?,
        fast::DROP => {
            p.mark(ip)?;
            p.fuel += cost;
            p.stk.pop()?;
        }
        fast::DUP => {
            p.mark(ip)?;
            p.fuel += cost;
            let g = *p.stk.last()?;
            p.stk.push(g);
        }
        fast::INC | fast::DEC => {
            let delta = if op == fast::INC { 1 } else { -1 };
            p.unary(f, ip, cost, |a, d| I::Step { a, d, delta })?
        }
        fast::INC_LOC | fast::INC_LT | fast::INC_LT_LOC => {
            p.step(f, ip, cost, f.slot(f.lp + imm16), 1)?
        }
        fast::DEC_LOC => p.step(f, ip, cost, f.slot(f.lp + imm16), -1)?,
        fast::ADD => p.binary(f, ip, cost, |a, c, d| I::Add { a, c, d })?,
        fast::SUB => p.binary(f, ip, cost, |a, c, d| I::Sub { a, c, d })?,
        fast::MUL => p.binary(f, ip, cost, |a, c, d| I::Mul { a, c, d })?,
        fast::DIV => p.binary(f, ip, cost, |a, c, d| I::Div { a, c, d })?,
        fast::MOD => p.binary(f, ip, cost, |a, c, d| I::Mod { a, c, d })?,
        fast::BAND => p.binary(f, ip, cost, |a, c, d| I::BAnd { a, c, d })?,
        fast::BOR => p.binary(f, ip, cost, |a, c, d| I::BOr { a, c, d })?,
        fast::BXOR => p.binary(f, ip, cost, |a, c, d| I::BXor { a, c, d })?,
        fast::SHL => p.binary(f, ip, cost, |a, c, d| I::Shl { a, c, d })?,
        fast::SAR => p.binary(f, ip, cost, |a, c, d| I::Sar { a, c, d })?,
        fast::LT | fast::LE | fast::GT | fast::GE => {
            let cmp = match op {
            fast::LT => Cmp::Lt,
            fast::LE => Cmp::Le,
            fast::GT => Cmp::Gt,
            _ => Cmp::Ge,
            };
            p.binary(f, ip, cost, |a, c, d| I::Compare {
            a,
            c,
            d,
            mask: cmp.mask(true),
            })?
        }
        fast::STRICT_EQ | fast::STRICT_NE => {
            let ne = op == fast::STRICT_NE;
            p.binary(f, ip, cost, |a, c, d| I::Equal { a, c, d, ne })?
        }
        fast::GET_ELEM => p.binary(f, ip, cost, |a, c, d| I::GetElem { a, c, d })?,
        fast::PUT_ELEM => {
            p.mark(ip)?;
            let v = p.stk.pop()?;
            let c = p.stk.pop()?;
            let a = p.stk.pop()?;
            let k = p.emit(I::PutElem { a, after: 0, c, v }, cost)?;
            p.mark(next)?;
            let after = p.mark?;
            p.prog[k].i = I::PutElem { a, after, c, v };
        }
        fast::NOT => p.unary(f, ip, cost, |a, d| I::Not { a, d })?,
        fast::IS_UNDEFINED => p.unary(f, ip, cost, |a, d| I::IsUndefined { a, d })?,
        fast::IS_NULLISH => p.unary(f, ip, cost, |a, d| I::IsNullish { a, d })?,
        _ => return Some(false),
    }
    Some(true)
}

fn translate(b: &[u8], f: &Frame, fused: Option<&Gen>) -> Option<Plan> {
    let mut p = Plan::default();
    for j in 0..f.depth as usize {
        let t = p.temp(f, j)?;
        p.stk.push(t);
    }
    p.exit_at(f.head)?;
    let mut reach = true;
    let mut ip = f.head;
    while ip < f.stop {
        if let Some(l) = p.labels.iter().position(|l| l.0 == ip) {
            let depth = p.labels[l].1 as usize;
            if reach {
                p.materialize(f, p.stk.len(), None)?;
                if p.stk.len() != depth {
                    return None;
                }
            } else {
                p.stk.clear();
                for j in 0..depth {
                    let t = p.temp(f, j)?;
                    p.stk.push(t);
                }
            }
            p.settle();
            p.labels[l].2 = Some(u16::try_from(p.prog.len()).ok()?);
            p.barrier = p.prog.len();
            reach = true;
        } else if !reach {
            return None;
        }
        let w = word(b, ip);
        let op = w as u8;
        let info = &OPS[op as usize];
        let next = ip + info.size as u32;
        let imm16 = (w >> 8) as u16 as u32;
        let imm32 = (w >> 8) as u32;
        if simple(&mut p, b, f, ip)? {
            ip = next;
            continue;
        }
        let cost = info.cost.fuel();
        match op {
            fast::JUMP => {
                reach = p.jump(
                    f,
                    ip,
                    cost,
                    (next as i64 + imm32 as i32 as i64) as u32,
                    None,
                )?
            }
            fast::JUMP8 => {
                reach = p.jump(
                    f,
                    ip,
                    cost,
                    (next as i64 + imm16 as u8 as i8 as i64) as u32,
                    None,
                )?
            }
            fast::JUMP_IF_FALSE | fast::JUMP_IF_TRUE => {
                let target = (next as i64 + imm32 as i32 as i64) as u32;
                reach = p.jump(f, ip, cost, target, Some(op == fast::JUMP_IF_TRUE))?
            }
            fast::JUMP_IF_FALSE8 | fast::JUMP_IF_TRUE8 => {
                let target = (next as i64 + imm16 as u8 as i8 as i64) as u32;
                reach = p.jump(f, ip, cost, target, Some(op == fast::JUMP_IF_TRUE8))?
            }
            fast::GET_METHOD => {
                let g = fused.filter(|g| g.at == ip && g.call)?;
                let after = next + u32::from(OPS[fast::CALL0 as usize].size);
                let pw = word(b, after);
                let (loc, keep) = local_put(pw)?;
                let slot = f.slot(f.lp + loc);
                let it = p.reg(Src::Slot(g.guard[0]))?;
                let local = p.reg(Src::Slot(slot))?;
                if p.mark.is_none()
                    || p.stk.pop()? != it
                    || slot == g.guard[0]
                    || p.stk.contains(&local)
                    || !p.labels.is_empty()
                    || p.entry.is_some_and(|e| e != slot)
                    || p.writes.iter().any(|w| w.1 == slot)
                {
                    return None;
                }
                p.fuel += cost;
                let mut scratch = p.clone();
                let from = scratch.writes.len();
                scratch.resume(b, ip, g, &[])?;
                let shadows: Vec<u32> = scratch.writes[from..].iter().map(|w| w.1).collect();
                p.resume(b, ip, g, &shadows)?;
                let v = p.stk.pop()?;
                let obj = p.reg(Src::Fresh(0))?;
                let value = p.reg(Src::Fresh(1))?;
                let flag = p.reg(Src::Fresh(2))?;
                let yes = p.reg(Src::Value(Value::TRUE))?;
                let put = &OPS[pw as u8 as usize];
                p.prog.push(Ins {
                    exit: 0,
                    fuel: put.cost.fuel(),
                    i: I::Mov { a: v, d: value },
                });
                p.prog.push(Ins {
                    exit: 0,
                    fuel: 0,
                    i: I::Mov { a: yes, d: flag },
                });
                p.fuse = None;
                if keep {
                    p.stk.push(obj);
                }
                p.result = Some(Virtual {
                    flag,
                    local,
                    obj,
                    slot,
                    value,
                });
                ip = after + put.size as u32;
                continue;
            }
            fast::GET_FIELD => {
                let top = *p.stk.last()?;
                let v = match p.result {
                    Some(v) => v,
                    None => {
                        let slot = match p.init[usize::from(top)] {
                            Src::Slot(x) if x >= f.slot(f.lp) && x < f.slot(f.sp) => x,
                            _ => return None,
                        };
                        if p.entry.is_some_and(|e| e != slot) {
                            return None;
                        }
                        p.entry = Some(slot);
                        Virtual {
                            flag: 0,
                            local: top,
                            obj: top,
                            slot,
                            value: p.reg(Src::Fresh(1))?,
                        }
                    }
                };
                let key = Atom::from_id(imm32);
                let src = if top != v.obj && top != v.local {
                    return None;
                } else if key == atoms::DONE {
                    p.reg(Src::Value(Value::FALSE))?
                } else if key == atoms::VALUE {
                    v.value
                } else {
                    return None;
                };
                p.mark(ip)?;
                p.fuel += cost;
                p.stk.pop();
                p.stk.push(src);
            }
            fast::ITER_NEXT_LOC => {
                let g = fused.filter(|g| g.at == ip && !g.call)?;
                if next as i64 + (w >> 24) as u32 as i32 as i64 != f.head as i64 {
                    return None;
                }
                let mut scratch = p.clone();
                let from = scratch.writes.len();
                scratch.resume(b, ip, g, &[])?;
                let shadows: Vec<u32> = scratch.writes[from..].iter().map(|w| w.1).collect();
                p.resume(b, ip, g, &shadows)?;
                let n = p.stk.len();
                if n != f.depth as usize {
                    return None;
                }
                p.materialize(f, n, None)?;
                p.mark = Some(0);
                p.emit(I::Jump { to: 0 }, 0)?;
                reach = false;
            }
            _ => return None,
        }
        ip = next;
    }
    if reach {
        let exit = p.exit_at(f.stop)?;
        if p.mark.is_none() {
            p.mark = Some(exit);
        }
        p.emit(I::Leave { exit }, 0)?;
    }
    for k in 0..p.outs.len() {
        let (at, exit) = p.outs[k];
        let to = u16::try_from(p.prog.len()).ok()?;
        p.prog.push(Ins {
            exit,
            fuel: 0,
            i: I::Leave { exit },
        });
        *p.prog[at].i.target()? = to;
    }
    for k in 0..p.fixes.len() {
        let (at, ip) = p.fixes[k];
        let to = p.labels.iter().find(|l| l.0 == ip)?.2?;
        *p.prog[at].i.target()? = to;
    }
    u8::try_from(p.prog.len() - 1).ok()?;
    if p.entry.is_some() && p.result.is_none() {
        return None;
    }
    if let Some(v) = p.result
        && p.prog.iter().any(|i| i.i.reads(v.obj) || i.i.reads(v.local))
    {
        return None;
    }
    Some(p)
}

fn local_put(w: u64) -> Option<(u32, bool)> {
    let op = w as u8;
    let imm16 = (w >> 8) as u16 as u32;
    Some(match op {
        fast::PUT_LOC => (imm16, false),
        fast::PUT_LOC0..=fast::PUT_LOC3 => ((op - fast::PUT_LOC0) as u32, false),
        fast::PUT_LOC8 => (imm16 & 0xFF, false),
        fast::SET_LOC => (imm16, true),
        fast::SET_LOC0..=fast::SET_LOC3 => ((op - fast::SET_LOC0) as u32, true),
        fast::SET_LOC8 => (imm16 & 0xFF, true),
        _ => return None,
    })
}

impl Exec<'_> {
    pub(crate) fn tier1_nudged(&self) -> bool {
        self.tier1_nudge.is_some()
    }

    pub(crate) fn nudge_tier1(&mut self) {
        if self.tier1_nudge.is_none() && self.opts.hot_loop {
            self.tier1_nudge = Some(self.opts.hot_backedges);
            self.opts.hot_backedges = 1;
        }
    }

    #[inline(never)]
    pub(super) fn run_tier1(&mut self) {
        let nudged = self
            .tier1_nudge
            .take()
            .map(|n| core::mem::replace(&mut self.opts.hot_backedges, n));
        let start = self.fuel;
        match self.tier1() {
            Some(pass) => {
                let long = (start - self.fuel) / i64::from(self.opts.hot_backedges) >= pass;
                if long || self.fuel <= pass {
                    self.tier1_nudge = Some(self.opts.hot_backedges);
                    self.opts.hot_backedges = 1;
                }
            }
            None => {
                if let Some(b) = nudged {
                    if let Some(n) = self.tier1_nudge.take() {
                        self.opts.hot_backedges = n;
                    }
                    if b < self.opts.hot_backedges / 2 {
                        self.tier1_nudge = Some(self.opts.hot_backedges);
                        self.opts.hot_backedges = b * 2;
                    }
                }
            }
        }
    }

    fn tier1(&mut self) -> Option<i64> {
        if self.opts.gc_stress || self.heap.gc_pending() || self.pc >= self.code_len {
            return None;
        }
        let head = self.code + self.pc;
        let vals = ValueArray::VALUES.at(self.stack, 0);
        let b: &mut [u8] = self.heap.arena_mut().hot_bytes();
        let (stop, pass, site) = match body(b, head, self.code + self.code_len) {
            Ok(found) => found,
            Err(op) => {
                if op == bytecode::Op::Yield as u8 {
                    self.tier1_nudge = Some(self.opts.hot_backedges);
                    self.opts.hot_backedges = 1;
                }
                return None;
            }
        };
        if self.fuel <= pass {
            return None;
        }
        let locals = self.heap.arena().get(self.template, FuncTemplate::LOCALS) as u32;
        let depth = self.sp.checked_sub(self.lp + locals)?;
        let fused = match site {
            Some((at, prev)) => Some(self.gen_site(at, prev)?),
            None => None,
        };
        let b: &mut [u8] = self.heap.arena_mut().hot_bytes();
        let f = Frame {
            bp: self.bp,
            consts: self.consts,
            depth,
            head,
            lp: self.lp,
            sp: self.sp - depth,
            stop,
            vals,
        };
        let plan = translate(b, &f, fused.as_ref())?;
        if let Some(g) = &fused
            && plan.writes.iter().any(|w| g.guard.contains(&w.1))
        {
            return None;
        }
        let pass = pass + plan.extra;
        if self.fuel <= pass {
            return None;
        }
        let spare = match plan.result {
            Some(_) => {
                let proto = realm::intrinsic_obj(self.heap, Intrinsic::ObjectProto);
                match self.heap.new_iter_result(Some(proto), Value::UNDEFINED, false) {
                    Ok(o) if !self.heap.gc_pending() => Some(o),
                    _ => return None,
                }
            }
            None => None,
        };
        let entry = match (plan.result, plan.entry) {
            (Some(v), Some(slot)) => {
                let o = self.heap.arena().read::<Value>(slot).as_object();
                match o.and_then(|o| self.heap.iter_result_fields(o)) {
                    Some((value, done)) if done == Value::FALSE => Some((v.value, value)),
                    _ => return None,
                }
            }
            _ => None,
        };
        let b: &mut [u8] = self.heap.arena_mut().hot_bytes();
        let mut r = [Value::UNDEFINED; 256];
        for (k, s) in plan.init.iter().enumerate() {
            r[k] = match *s {
                Src::Shadow(x) | Src::Slot(x) => ld(b, x),
                Src::Fresh(_) => Value::UNDEFINED,
                Src::Value(v) => v,
            };
        }
        if let Some((g, value)) = entry {
            r[usize::from(g)] = value;
        }

        let nop = lower(&Ins {
            exit: 0,
            fuel: 0,
            i: I::Nop,
        });
        let mut prog: [Op; 256] = core::array::from_fn(|s| match plan.prog.get(s) {
            Some(ins) => lower(ins),
            None => nop.clone(),
        });
        for s in 1..plan.prog.len() {
            let (o, p) = (prog[s].clone(), &plan.prog[s]);
            let suffix = matches!(
                prog[s - 1].k.get(),
                K::Add
                    | K::AddF
                    | K::BAnd
                    | K::BOr
                    | K::BXor
                    | K::Div
                    | K::GetElem
                    | K::IsNullish
                    | K::IsUndefined
                    | K::Mod
                    | K::Mov
                    | K::Mul
                    | K::Not
                    | K::Sar
                    | K::Shl
                    | K::Step
                    | K::Sub
            );
            let label = plan.labels.iter().any(|l| l.2 == Some(s as u16));
            if matches!(o.k.get(), K::Latch) && o.to == 0 && suffix && !label {
                let q = &mut prog[s - 1];
                (q.l, q.lc, q.lfuel, q.lx) = (true, o.c, q.fuel + p.fuel, o.a);
                (q.aux, q.n, q.span) = (o.exit, o.n, o.span);
                if s == 1
                    && matches!(q.k.get(), K::Add)
                    && q.a == q.d
                    && q.a != q.c
                    && q.c == q.lx
                {
                    q.k.set(K::Accumulate);
                }
            }
        }
        let mut targets = [false; 256];
        for o in &prog[..plan.prog.len()] {
            if matches!(
                o.k.get(),
                K::Branch | K::Jump | K::JumpIf | K::Latch | K::StepBranch
            ) {
                targets[usize::from(o.to)] = true;
            }
        }
        for l in &plan.labels {
            if let Some(t) = l.2 {
                targets[usize::from(t)] = true;
            }
        }
        let mut s = 1;
        while s < plan.prog.len() {
            let o = prog[s].clone();
            let q = &mut prog[s - 1];
            if matches!(o.k.get(), K::Mov)
                && !targets[s]
                && !q.l
                && !matches!(q.k.get(), K::Jump | K::Leave)
            {
                (q.m, q.ma, q.md, q.mfuel) = (true, o.a, o.d, q.fuel + o.fuel);
                s += 2;
            } else {
                s += 1;
            }
        }
        for s in 0..plan.prog.len() {
            let q = &prog[s];
            let (mut nx, mut f) = (s + 1 + usize::from(q.m), if q.m { q.mfuel } else { q.fuel });
            let branch = matches!(
                q.k.get(),
                K::Branch | K::JumpIf | K::Latch | K::StepBranch
            );
            let mut chk = false;
            if let Some(j) = prog.get(nx)
                && matches!(j.k.get(), K::Jump)
                && (branch || j.to != 0)
            {
                (nx, f, chk) = (usize::from(j.to), f + j.fuel, j.to == 0);
            }
            let q = &mut prog[s];
            (q.chk, q.mfuel, q.nx) = (chk, f, nx as u8);
        }
        for s in 0..plan.prog.len() {
            let q = &prog[s];
            if !matches!(
                q.k.get(),
                K::Branch | K::JumpIf | K::Latch | K::StepBranch
            ) {
                continue;
            }
            let v = &prog[usize::from(q.nx)];
            if matches!(v.k.get(), K::Mov) && !v.l && !v.m {
                let (wa, wd, wfuel, nx) = (v.a, v.d, v.mfuel, v.nx);
                let q = &mut prog[s];
                (q.nx, q.w, q.wa, q.wd, q.wfuel) = (nx, true, wa, wd, wfuel);
            }
        }
        for s in 1..plan.prog.len().saturating_sub(2) {
            let (q, t) = (&prog[s], s + 2);
            if matches!(q.k.get(), K::Resume)
                && q.m
                && usize::from(q.nx) == t
                && matches!(prog[t].k.get(), K::StepBranch)
                && !targets[s]
                && !targets[t]
                && (s < 2 || !prog[s - 2].l)
            {
                let (ra, rd, rfuel) = (q.ma, q.md, q.mfuel);
                for o in &mut prog[..plan.prog.len()] {
                    if usize::from(o.nx) == s {
                        o.nx = t as u8;
                    }
                }
                let o = &mut prog[t];
                (o.ra, o.rd, o.res, o.rfuel) = (ra, rd, true, rfuel);
            }
        }
        if plan.prog.len() >= 3 {
            let (o, q) = (&prog[0], &prog[1]);
            let fk = match o.k.get() {
                K::BAnd => Some(K::FoldAnd),
                K::BOr => Some(K::FoldOr),
                K::BXor => Some(K::FoldXor),
                K::Mod => Some(K::FoldMod),
                K::Mul => Some(K::FoldMul),
                _ => None,
            };
            if let Some(fk) = fk
                && matches!(q.k.get(), K::Add)
                && q.l
                && !targets[1]
                && !o.m
                && !o.l
                && usize::from(o.nx) == 1
                && q.a == q.d
                && q.c == o.d
                && ![o.a, o.c, q.a, q.lx].contains(&o.d)
                && ![o.a, o.c, q.lx].contains(&q.a)
                && o.c != q.lx
            {
                o.k.set(fk);
            }
        }
        let mut fuel = self.fuel;
        let mut mark = fuel;
        let mut k = 0u8;
        let exit = 'run: loop {
            let o = &prog[usize::from(k)];
            let (a, c, d) = (o.a as usize, o.c as usize, o.d as usize);
            macro_rules! next {
                () => {{
                    if o.m {
                        r[usize::from(o.md)] = r[usize::from(o.ma)];
                    }
                    fuel -= o.mfuel as i64;
                    k = o.nx;
                }};
            }
            macro_rules! fall {
                () => {{
                    next!();
                    if o.chk && fuel <= pass {
                        break 'run 0;
                    }
                    if o.w {
                        r[usize::from(o.wd)] = r[usize::from(o.wa)];
                        fuel -= o.wfuel as i64;
                    }
                }};
            }
            macro_rules! latch {
                () => {{
                    if o.l {
                        let x = o.lx as usize;
                        let Some(i) = r[x].as_int().and_then(|i| i.checked_add(i32::from(o.lc as i8)))
                        else {
                            fuel -= o.fuel as i64;
                            break 'run o.aux;
                        };
                        r[x] = Value::int(i);
                        fuel -= o.lfuel as i64;
                        if i.wrapping_sub(o.n) as u32 <= o.span {
                            if fuel <= pass {
                                break 'run 0;
                            }
                            k = 0;
                        } else {
                            k += 2;
                        }
                    } else {
                        next!();
                    }
                }};
            }
            macro_rules! spin {
                ($body:block) => {{
                    let at = k;
                    loop {
                        $body
                        if k != at {
                            break;
                        }
                    }
                }};
            }
            macro_rules! goto {
                () => {{
                    fuel -= o.fuel as i64;
                    if o.to == 0 {
                        if fuel <= pass {
                            break 'run 0;
                        }
                    }
                    k = o.to as u8;
                }};
            }
            macro_rules! arith {
                ($int:expr, $float:expr) => {{
                    let (p, q) = (r[a], r[c]);
                    let int: Option<Value> = match (p.as_int(), q.as_int()) {
                        (Some(x), Some(y)) => ($int)(x, y),
                        _ => None,
                    };
                    let v = match int {
                        Some(v) => v,
                        None => match (p.as_number(), q.as_number()) {
                            (Some(x), Some(y)) => match ($float)(x, y) {
                                Some(v) => v,
                                None => break 'run o.exit,
                            },
                            _ => break 'run o.exit,
                        },
                    };
                    r[d] = v;
                    latch!();
                }};
            }
            macro_rules! no_float {
                () => {
                    |_: f64, _: f64| -> Option<Value> { None }
                };
            }
            match o.k.get() {
                K::Accumulate => {
                    if r[c].as_int().is_some() && (r[a].as_int().is_some() || r[a].tag().is_none()) {
                        match accumulate(&mut r, o, a, c, &mut fuel, pass) {
                            Some(e) => break 'run e,
                            None => k += 2,
                        }
                    } else {
                        o.k.set(K::Add);
                    }
                }
                K::FoldAnd | K::FoldMod | K::FoldMul | K::FoldOr | K::FoldXor => {
                    let q = &prog[1];
                    let kind = o.k.get();
                    let res = match kind {
                        K::FoldAnd => fold(&mut r, o, q, &mut fuel, pass, |x, y| Some(Value::int(x & y))),
                        K::FoldOr => fold(&mut r, o, q, &mut fuel, pass, |x, y| Some(Value::int(x | y))),
                        K::FoldXor => fold(&mut r, o, q, &mut fuel, pass, |x, y| Some(Value::int(x ^ y))),
                        K::FoldMod => fold(&mut r, o, q, &mut fuel, pass, |x, y| {
                            (x >= 0 && y != 0).then(|| Value::int(x % y))
                        }),
                        _ => fold(&mut r, o, q, &mut fuel, pass, |x, y| {
                            Some(match x.checked_mul(y).filter(|&v| v != 0) {
                                Some(v) => Value::int(v),
                                None => number(f64::from(x) * f64::from(y)),
                            })
                        }),
                    };
                    match res {
                        Ok(Some(e)) => break 'run e,
                        Ok(None) => k = 3,
                        Err(()) => o.k.set(match kind {
                            K::FoldAnd => K::BAnd,
                            K::FoldOr => K::BOr,
                            K::FoldXor => K::BXor,
                            K::FoldMod => K::Mod,
                            _ => K::Mul,
                        }),
                    }
                }
                K::Add => spin!({
                    let (p, q) = (r[a], r[c]);
                    let Some(v) = add(p, q) else {
                        break 'run o.exit;
                    };
                    r[d] = v;
                    if p.tag().is_none() && q.as_int().is_some() {
                        o.k.set(K::AddF);
                        latch!();
                        break;
                    }
                    latch!();
                }),
                K::AddF => spin!({
                    let (p, q) = (r[a], r[c]);
                    let v = match (p.tag(), q.as_int()) {
                        (None, Some(y)) => number(f64::from_bits(p.to_bits()) + f64::from(y)),
                        _ => {
                            o.k.set(K::Add);
                            let Some(v) = add(p, q) else {
                                break 'run o.exit;
                            };
                            r[d] = v;
                            latch!();
                            break;
                        }
                    };
                    r[d] = v;
                    latch!();
                }),
                K::Latch => {
                    let Some(i) = r[a]
                        .as_int()
                        .and_then(|i| i.checked_add(i32::from(o.c as i8)))
                    else {
                        break 'run o.exit;
                    };
                    r[a] = Value::int(i);
                    if i.wrapping_sub(o.n) as u32 <= o.span {
                        goto!()
                    } else {
                        fall!()
                    }
                }
                K::StepBranch => {
                    if o.res {
                        mark = fuel;
                        r[usize::from(o.rd)] = r[usize::from(o.ra)];
                        fuel -= o.rfuel as i64;
                    }
                    let Some(i) = r[a]
                        .as_int()
                        .and_then(|i| i.checked_add(i32::from(o.d as i8)))
                    else {
                        break 'run o.exit;
                    };
                    r[a] = Value::int(i);
                    let n = r[c];
                    let t = match n.as_int() {
                        Some(n) => order_int(i, n),
                        None => match n.as_number() {
                            Some(n) => order_float(f64::from(i), n),
                            None => {
                                fuel -= o.n as i64;
                                break 'run o.aux;
                            }
                        },
                    };
                    if o.mask >> t & 1 != 0 {
                        goto!()
                    } else {
                        fall!()
                    }
                }
                K::Branch => {
                    let Some(t) = order(r[a], r[c]) else {
                        break 'run o.exit;
                    };
                    if o.mask >> t & 1 != 0 {
                        goto!()
                    } else {
                        fall!()
                    }
                }
                K::Compare => {
                    let Some(t) = order(r[a], r[c]) else {
                        break 'run o.exit;
                    };
                    r[d] = Value::bool(o.mask >> t & 1 != 0);
                    next!();
                }
                K::GetElem => {
                    let (Some(obj), Some(i)) = (
                        r[a].as_object(),
                        r[c].as_int().and_then(|i| u32::try_from(i).ok()),
                    ) else {
                        break 'run o.exit;
                    };
                    let Some(v) = self.heap.get_element_fast(obj, i) else {
                        break 'run o.exit;
                    };
                    r[d] = v;
                    latch!();
                }
                K::PutElem => {
                    let (Some(obj), Some(i)) = (
                        r[a].as_object(),
                        r[c].as_int().and_then(|i| u32::try_from(i).ok()),
                    ) else {
                        break 'run o.exit;
                    };
                    if !matches!(self.heap.set_element_fast(obj, i, r[d]), Ok(true)) {
                        break 'run o.exit;
                    }
                    if self.heap.gc_pending() {
                        fuel -= o.fuel as i64;
                        break 'run o.aux;
                    }
                    next!();
                }
                K::Mov => {
                    r[d] = r[a];
                    latch!();
                }
                K::Step => {
                    let Some(i) = r[a]
                        .as_int()
                        .and_then(|i| i.checked_add(i32::from(o.c as i8)))
                    else {
                        break 'run o.exit;
                    };
                    r[d] = Value::int(i);
                    latch!();
                }
                K::Sub => arith!(
                    |x: i32, y: i32| x.checked_sub(y).map(Value::int),
                    |x: f64, y: f64| Some(number(x - y))
                ),
                K::Mul => arith!(
                    |x: i32, y: i32| x.checked_mul(y).filter(|&v| v != 0).map(Value::int),
                    |x: f64, y: f64| Some(number(x * y))
                ),
                K::Div => arith!(|_: i32, _: i32| None, |x: f64, y: f64| Some(number(x / y))),
                K::Mod => arith!(
                    |x: i32, y: i32| (x >= 0 && y != 0).then(|| Value::int(x % y)),
                    no_float!()
                ),
                K::BAnd => arith!(|x: i32, y: i32| Some(Value::int(x & y)), no_float!()),
                K::BOr => arith!(|x: i32, y: i32| Some(Value::int(x | y)), no_float!()),
                K::BXor => arith!(|x: i32, y: i32| Some(Value::int(x ^ y)), no_float!()),
                K::Shl => arith!(
                    |x: i32, y: i32| Some(Value::int(x.wrapping_shl(y as u32 & 31))),
                    no_float!()
                ),
                K::Sar => arith!(
                    |x: i32, y: i32| Some(Value::int(x >> (y as u32 & 31))),
                    no_float!()
                ),
                K::Equal => {
                    let (p, q) = (r[a], r[c]);
                    let eq = match (p.as_number(), q.as_number()) {
                        (Some(x), Some(y)) => x == y,
                        _ if p == q => true,
                        _ if (p.as_string().is_some() && q.as_string().is_some())
                            || (p.as_bigint().is_some() && q.as_bigint().is_some()) =>
                        {
                            break 'run o.exit;
                        }
                        _ => false,
                    };
                    r[d] = Value::bool(eq != (o.mask != 0));
                    next!();
                }
                K::Not => {
                    let v = r[a];
                    let truthy = if let Some(x) = v.as_bool() {
                        x
                    } else if let Some(n) = v.as_number() {
                        !(n == 0.0 || n.is_nan())
                    } else if v.is_undefined() || v.is_null() {
                        false
                    } else if v.as_object().is_some() {
                        true
                    } else {
                        break 'run o.exit;
                    };
                    r[d] = Value::bool(!truthy);
                    latch!();
                }
                K::IsUndefined => {
                    r[d] = Value::bool(r[a].is_undefined());
                    latch!();
                }
                K::IsNullish => {
                    r[d] = Value::bool(r[a].is_nullish());
                    latch!();
                }
                K::JumpIf => {
                    let Some(t) = r[a].as_bool() else {
                        break 'run o.exit;
                    };
                    if t == (o.mask != 0) { goto!() } else { fall!() }
                }
                K::Jump => goto!(),
                K::Nop => next!(),
                K::Resume => {
                    mark = fuel;
                    next!();
                }
                K::Leave => {
                    fuel -= o.fuel as i64;
                    break 'run o.to;
                }
            }
        };
        let made = match (plan.result, spare) {
            (Some(v), Some(o)) if r[usize::from(v.flag)] == Value::TRUE => {
                self.heap.set_own_data_value(o, atoms::VALUE, r[v.value as usize]);
                Some((v, Value::object(o)))
            }
            _ => None,
        };
        let b: &mut [u8] = self.heap.arena_mut().hot_bytes();
        let e = plan.exits[exit as usize];
        let list = &plan.stacks[e.at as usize..(e.at + e.len) as usize];
        for (j, &g) in list.iter().enumerate() {
            let x = match made {
                Some((v, o)) if g == v.obj || g == v.local => o,
                _ => r[g as usize],
            };
            st(b, vals + 8 * (f.sp + j as u32), x);
        }
        if let Some((v, o)) = made {
            st(b, v.slot, o);
        }
        for &(g, x) in &plan.writes {
            let s = match plan.shadows.iter().find(|s| s.0 == g) {
                Some(s) if e.rollback => s.1,
                _ => g,
            };
            st(b, x, r[s as usize]);
        }
        self.pc = e.ip - self.code;
        self.sp = f.sp + e.len;
        self.fuel = if e.rollback { mark } else { fuel };
        Some(pass)
    }

    fn gen_site(&self, at: u32, prev: u32) -> Option<Gen> {
        let b = self.heap.arena().bytes();
        let slot = |i: u32| ValueArray::VALUES.at(self.stack, self.lp + i);
        let w = word(b, at);
        let call = w as u8 == fast::GET_METHOD;
        let (guard, it, next) = if call {
            let p = word(b, prev);
            let loc = match p as u8 {
                fast::GET_LOC => (p >> 8) as u16 as u32,
                fast::GET_LOC8 => (p >> 8) as u8 as u32,
                op @ fast::GET_LOC0..=fast::GET_LOC3 => (op - fast::GET_LOC0) as u32,
                _ => return None,
            };
            if Atom::from_id((w >> 8) as u32) != atoms::NEXT {
                return None;
            }
            let it = ld(b, slot(loc));
            let next = self.ic_get(it, atoms::NEXT, (w >> 40) as u16 as u32)?;
            ([slot(loc); 2], it, next)
        } else {
            let loc = (w >> 8) as u16 as u32;
            let guard = [slot(loc), slot(loc + 1)];
            (guard, ld(b, guard[0]), ld(b, guard[1]))
        };
        let (arr, pc, hp_off) = suspend::suspended_frame(self.heap, it, next)?;
        if arr < self.heap.arena().tracked_end() {
            return None;
        }
        let t = self.template_of(self.heap.value_array_get(arr, 0).as_object()?);
        let a = self.heap.arena();
        let code = FuncTemplate::DATA.at(t, 0);
        let code_len = a.get(t, FuncTemplate::CODE_LEN);
        let len = self.heap.value_array_len(arr);
        let size = |op: bytecode::Op| u32::from(OPS[op as usize].size);
        let fuel = |op: bytecode::Op| OPS[op as usize].cost.fuel();
        let yield_at = pc.checked_sub(size(bytecode::Op::Yield))?;
        let offset = u32::try_from(i32::from(*b.get((code + pc + 1) as usize)? as i8)).ok()?;
        let jump = pc + size(bytecode::Op::JumpIfFalse8) + offset;
        let ok = b[(code + yield_at) as usize] == bytecode::Op::Yield as u8
            && b[(code + pc) as usize] == bytecode::Op::JumpIfFalse8 as u8
            && jump < code_len
            && b[(code + jump) as usize] == bytecode::Op::Drop as u8;
        if !ok {
            return None;
        }
        let enter = if call {
            bytecode::Op::Call
        } else {
            bytecode::Op::IterNextLoc
        };
        Some(Gen {
            at,
            call,
            frame: Frame {
                bp: 0,
                consts: a.get(t, FuncTemplate::CONSTS).0,
                depth: 0,
                head: code + jump + size(bytecode::Op::Drop),
                lp: hp_off + HEADER,
                sp: len,
                stop: code + code_len,
                vals: ValueArray::VALUES.at(arr, 0),
            },
            guard,
            resume: fuel(enter) + len / 8 + fuel(bytecode::Op::JumpIfFalse8) + fuel(bytecode::Op::Drop),
            resume_pc: code + jump + size(bytecode::Op::Drop),
            yield_cost: fuel(bytecode::Op::Yield) + len / 8,
            yield_ip: code + yield_at,
        })
    }
}
