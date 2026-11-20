// WebAssembly for djs: a binary decoder, a validator and a compiler from wasm functions to
// JS source, plus the JS API (WebAssembly.Module, Instance, Memory, Table, Global, ...).
// scripts/wasm-ts.py embeds this file in builtins/webassembly.ts, which runs it once when
// the realm is made, late in the prelude: it is the body of a function of WebAssembly, the
// namespace object, which it fills in. Its top level avoids for-of and destructuring, whose
// slow paths call intrinsics the prelude installs after it.
// Compiled modules run as djs bytecode, so imports, fuel, the GC and snapshots need nothing
// special. Supported: the MVP, multi-value, sign extension, non-trapping float-to-int,
// bulk memory, reference types, mutable globals, extended constant expressions and tail
// calls (as plain calls). Not supported: SIMD, threads, exceptions, memory64, GC.
"use strict";
const $BigInt = BigInt, $Number = Number, $Math = Math, $Object = Object, $String = String;
const $Uint8Array = Uint8Array, $DataView = DataView, $ArrayBuffer = ArrayBuffer;
const $Promise = Promise, $WeakMap = WeakMap, $Error = Error, $TypeError = TypeError;
const $RangeError = RangeError, $Function = Function, $Symbol = Symbol, $Array = Array;
const asIntN = $BigInt.asIntN, asUintN = $BigInt.asUintN, fround = $Math.fround;
const defineProperty = $Object.defineProperty, freeze = $Object.freeze;
const isView = $ArrayBuffer.isView;
const abByteLength = $Object.getOwnPropertyDescriptor($ArrayBuffer.prototype, "byteLength").get;
const abTransfer = $ArrayBuffer.prototype.transfer;
const apply = Reflect.apply;
const PAGE = 65536;
const MAX_PAGES = 65536;
const MAX_TABLE = 10000000;

// ---- Errors -------------------------------------------------------------------------

// NativeError-like constructors, without class heritage (see the top).
function errorClass(name) {
  const C = function (message) {
    return Reflect.construct($Error, [message, arguments[1]], new.target === undefined ? C : new.target);
  };
  defineProperty(C, "name", { value: name });
  defineProperty(C, "length", { value: 1 });
  $Object.setPrototypeOf(C, $Error);
  C.prototype = $Object.create($Error.prototype, {
    constructor: { value: C, writable: true, configurable: true },
    name: { value: name, writable: true, configurable: true },
    message: { value: "", writable: true, configurable: true },
  });
  defineProperty(C, "prototype", { writable: false });
  return C;
}
const CompileError = errorClass("CompileError");
const LinkError = errorClass("LinkError");
const RuntimeError = errorClass("RuntimeError");

function trap(msg) {
  throw new RuntimeError(msg);
}

// ---- Bytes --------------------------------------------------------------------------

/** A copy of a BufferSource's bytes, or TypeError. */
function copyBytes(src, api) {
  if (isView(src)) {
    const u = new $Uint8Array(src.buffer, src.byteOffset, src.byteLength);
    return u.slice();
  }
  let len;
  try {
    len = apply(abByteLength, src, []);
  } catch (e) {
    throw new $TypeError(api + "Argument 0 must be a buffer source");
  }
  return new $Uint8Array(src, 0, len).slice();
}

// ---- Decoder ------------------------------------------------------------------------

const I32 = 0x7f, I64 = 0x7e, F32 = 0x7d, F64 = 0x7c, V128 = 0x7b, FUNCREF = 0x70, EXTERNREF = 0x6f;
const TYPE_NAMES = { 0x7f: "i32", 0x7e: "i64", 0x7d: "f32", 0x7c: "f64", 0x70: "funcref", 0x6f: "externref" };

class Reader {
  constructor(bytes, pos, end) {
    this.b = bytes;
    this.p = pos;
    this.end = end;
  }
  fail(msg, at) {
    throw new CompileError(apiName + msg + " @+" + (at === undefined ? this.p : at));
  }
  eof() {
    return this.p >= this.end;
  }
  u8() {
    if (this.p >= this.end) this.fail("unexpected end of section or function");
    return this.b[this.p++];
  }
  u32() {
    let r = 0, shift = 0;
    const at = this.p;
    for (let i = 0; i < 5; i++) {
      const b = this.u8();
      if (i === 4 && (b & 0x70) !== 0) this.fail("extra bits in varint", at);
      r += (b & 0x7f) * 2 ** shift;
      if ((b & 0x80) === 0) return r;
      shift += 7;
    }
    this.fail("length overflow while decoding varint", at);
  }
  s32() {
    let r = 0, shift = 0, b;
    const at = this.p;
    for (let i = 0; ; i++) {
      if (i === 5) this.fail("length overflow while decoding varint", at);
      b = this.u8();
      r |= (b & 0x7f) << shift;
      shift += 7;
      if ((b & 0x80) === 0) {
        if (i === 4) {
          const top = b & 0x70;
          const sign = (b & 0x08) !== 0;
          if (top !== (sign ? 0x70 : 0)) this.fail("extra bits in varint", at);
          return r | 0;
        }
        if (b & 0x40) r |= -1 << shift;
        return r | 0;
      }
    }
  }
  s33() {
    let r = 0, shift = 0, b;
    const at = this.p;
    for (let i = 0; ; i++) {
      if (i === 5) this.fail("length overflow while decoding varint", at);
      b = this.u8();
      r += (b & 0x7f) * 2 ** shift;
      shift += 7;
      if ((b & 0x80) === 0) break;
    }
    if (b & 0x40) r -= 2 ** shift;
    return r;
  }
  s64() {
    let r = 0n, shift = 0n, b;
    const at = this.p;
    for (let i = 0; ; i++) {
      if (i === 10) this.fail("length overflow while decoding varint", at);
      b = this.u8();
      r |= $BigInt(b & 0x7f) << shift;
      shift += 7n;
      if ((b & 0x80) === 0) {
        if (i === 9 && b !== 0 && b !== 0x7f) this.fail("extra bits in varint", at);
        if (b & 0x40) r -= 1n << shift;
        break;
      }
    }
    return asIntN(64, r);
  }
  f32() {
    if (this.p + 4 > this.end) this.fail("unexpected end");
    const v = new $DataView(this.b.buffer, this.b.byteOffset + this.p, 4).getFloat32(0, true);
    this.p += 4;
    return v;
  }
  f64() {
    if (this.p + 8 > this.end) this.fail("unexpected end");
    const v = new $DataView(this.b.buffer, this.b.byteOffset + this.p, 8).getFloat64(0, true);
    this.p += 8;
    return v;
  }
  bytes(n) {
    if (this.p + n > this.end) this.fail("length out of bounds");
    const v = this.b.subarray(this.p, this.p + n);
    this.p += n;
    return v;
  }
  name() {
    const n = this.u32();
    const at = this.p;
    return utf8(this.bytes(n), this, at);
  }
}

function utf8(b, r, at) {
  let s = "";
  for (let i = 0; i < b.length; ) {
    const c = b[i];
    let cp, n;
    if (c < 0x80) { cp = c; n = 1; }
    else if (c >= 0xc2 && c < 0xe0) { cp = c & 0x1f; n = 2; }
    else if (c >= 0xe0 && c < 0xf0) { cp = c & 0x0f; n = 3; }
    else if (c >= 0xf0 && c < 0xf5) { cp = c & 0x07; n = 4; }
    else r.fail("no valid UTF-8 string", at);
    if (i + n > b.length) r.fail("no valid UTF-8 string", at);
    for (let k = 1; k < n; k++) {
      const d = b[i + k];
      if ((d & 0xc0) !== 0x80) r.fail("no valid UTF-8 string", at);
      cp = (cp << 6) | (d & 0x3f);
    }
    if ((n === 3 && (cp < 0x800 || (cp >= 0xd800 && cp < 0xe000))) || (n === 4 && (cp < 0x10000 || cp > 0x10ffff)))
      r.fail("no valid UTF-8 string", at);
    s += $String.fromCodePoint(cp);
    i += n;
  }
  return s;
}

function valType(r) {
  const at = r.p;
  const t = r.u8();
  if (t === I32 || t === I64 || t === F32 || t === F64 || t === FUNCREF || t === EXTERNREF) return t;
  if (t === V128) r.fail("Wasm SIMD unsupported", at);
  r.fail("invalid value type 0x" + t.toString(16), at);
}
function refType(r) {
  const at = r.p;
  const t = r.u8();
  if (t === FUNCREF || t === EXTERNREF) return t;
  r.fail("invalid reference type", at);
}
function limits(r, max, what) {
  const at = r.p;
  const flags = r.u8();
  const unit = what === "memory" ? " pages" : " elements";
  if (flags > 3 || (flags & 2 && what !== "memory")) r.fail("invalid " + what + " limits flags 0x" + flags.toString(16), at);
  if (flags & 2) r.fail("shared memory is not supported", at);
  const min = r.u32();
  if (min > max) r.fail("initial " + what + " size (" + min + unit + ") is larger than implementation limit (" + max + unit + ")", at + 1);
  let mx = null;
  if (flags & 1) {
    const mat = r.p;
    mx = r.u32();
    if (mx > max) r.fail("maximum " + what + " size (" + mx + unit + ") is larger than implementation limit (" + max + unit + ")", mat);
    if (mx < min) r.fail("maximum " + what + " size (" + mx + unit + ") is smaller than initial size (" + min + unit + ")", mat);
  }
  return { min, max: mx };
}

const SECTION_ORDER = { 1: 1, 2: 2, 3: 3, 4: 4, 5: 5, 13: 6, 6: 7, 7: 8, 8: 9, 9: 10, 12: 11, 10: 12, 11: 13 };
const SECTION_NAMES = { 1: "Type", 2: "Import", 3: "Function", 4: "Table", 5: "Memory", 6: "Global", 7: "Export", 8: "Start", 9: "Element", 10: "Code", 11: "Data", 12: "DataCount", 13: "Tag" };

function decode(bytes) {
  const r = new Reader(bytes, 0, bytes.length);
  if (bytes.length === 0) r.fail("BufferSource argument is empty", 0);
  if (bytes.length < 4) r.fail("expected 4 bytes, fell off end", 0);
  if (bytes[0] !== 0 || bytes[1] !== 0x61 || bytes[2] !== 0x73 || bytes[3] !== 0x6d)
    r.fail("expected magic word 00 61 73 6d, found " + hex4(bytes, 0), 0);
  if (bytes.length < 8) r.fail("expected 4 bytes, fell off end", 4);
  if (bytes[4] !== 1 || bytes[5] !== 0 || bytes[6] !== 0 || bytes[7] !== 0)
    r.fail("expected version 01 00 00 00, found " + hex4(bytes, 4), 4);
  r.p = 8;
  const M = {
    types: [], imports: [], funcs: [], tables: [], mems: [], globals: [], exports: [],
    start: null, elems: [], dataCount: null, codes: [], datas: [], customs: [],
    nImportFuncs: 0, nImportTables: 0, nImportMems: 0, nImportGlobals: 0, declared: null,
  };
  let last = 0;
  let nFuncDecls = 0;
  while (!r.eof()) {
    const at = r.p;
    const id = r.u8();
    const size = r.u32();
    const start = r.p;
    const end = start + size;
    if (end > bytes.length) r.fail("section (code " + id + ", \"" + (SECTION_NAMES[id] || "Unknown") + "\") extends past end of the module (length " + size + ", remaining bytes " + (bytes.length - start) + ")", start);
    const s = new Reader(bytes, start, end);
    if (id === 0) {
      const name = s.name();
      M.customs.push({ name, bytes: bytes.slice(s.p, end) });
      r.p = end;
      continue;
    }
    const ord = SECTION_ORDER[id];
    if (ord === undefined) r.fail("unknown section code #0x" + id.toString(16), at);
    if (ord <= last) r.fail("unexpected section <" + SECTION_NAMES[id] + ">", at);
    last = ord;
    switch (id) {
      case 1: {
        const n = s.u32();
        for (let i = 0; i < n; i++) {
          const fat = s.p;
          const form = s.u8();
          if (form !== 0x60) s.fail("invalid function type form", fat);
          const params = [], results = [];
          for (let k = s.u32(); k > 0; k--) params.push(valType(s));
          for (let k = s.u32(); k > 0; k--) results.push(valType(s));
          M.types.push({ params, results, sig: sigOf(params, results) });
        }
        break;
      }
      case 2: {
        const n = s.u32();
        for (let i = 0; i < n; i++) {
          const module = s.name();
          const name = s.name();
          const kat = s.p;
          const kind = s.u8();
          const imp = { module, name, kind, index: 0 };
          if (kind === 0) {
            const t = s.u32();
            if (t >= M.types.length) s.fail("signature index " + t + " out of bounds (" + M.types.length + " signatures)", kat + 1);
            imp.index = M.funcs.length;
            M.funcs.push(t);
            M.nImportFuncs++;
          } else if (kind === 1) {
            const elem = refType(s);
            const l = limits(s, MAX_TABLE, "table");
            imp.index = M.tables.length;
            M.tables.push({ elem, min: l.min, max: l.max });
            M.nImportTables++;
          } else if (kind === 2) {
            const l = limits(s, MAX_PAGES, "memory");
            imp.index = M.mems.length;
            M.mems.push(l);
            M.nImportMems++;
          } else if (kind === 3) {
            const type = valType(s);
            const m = s.u8();
            if (m > 1) s.fail("invalid global mutability", s.p - 1);
            imp.index = M.globals.length;
            M.globals.push({ type, mut: m === 1, init: null });
            M.nImportGlobals++;
          } else if (kind === 4) {
            s.fail("exception handling is not supported", kat);
          } else {
            s.fail("unknown import kind 0x" + kind.toString(16), kat);
          }
          M.imports.push(imp);
        }
        break;
      }
      case 3: {
        const n = s.u32();
        nFuncDecls = n;
        for (let i = 0; i < n; i++) {
          const at2 = s.p;
          const t = s.u32();
          if (t >= M.types.length) s.fail("signature index " + t + " out of bounds (" + M.types.length + " signatures)", at2);
          M.funcs.push(t);
        }
        break;
      }
      case 4: {
        const n = s.u32();
        for (let i = 0; i < n; i++) {
          const elem = refType(s);
          const l = limits(s, MAX_TABLE, "table");
          M.tables.push({ elem, min: l.min, max: l.max });
        }
        break;
      }
      case 5: {
        const n = s.u32();
        for (let i = 0; i < n; i++) M.mems.push(limits(s, MAX_PAGES, "memory"));
        if (M.mems.length > 1) s.fail("At most one memory is supported (declared " + M.mems.length + ")", start);
        break;
      }
      case 6: {
        const n = s.u32();
        for (let i = 0; i < n; i++) {
          const type = valType(s);
          const m = s.u8();
          if (m > 1) s.fail("invalid global mutability", s.p - 1);
          const init = constExpr(s, M, type, M.globals.length);
          M.globals.push({ type, mut: m === 1, init });
        }
        break;
      }
      case 7: {
        const n = s.u32();
        const seen = new Set();
        for (let i = 0; i < n; i++) {
          const nat = s.p;
          const name = s.name();
          const kat = s.p;
          const kind = s.u8();
          const index = s.u32();
          const limit = [M.funcs.length, M.tables.length, M.mems.length, M.globals.length][kind];
          if (limit === undefined) s.fail("invalid export kind 0x" + kind.toString(16), kat);
          if (index >= limit) s.fail("invalid " + kindName(kind) + " index: " + index, kat + 1);
          if (seen.has(name)) s.fail("Duplicate export name '" + name + "'", nat);
          seen.add(name);
          M.exports.push({ name, kind, index });
          if (kind === 0) declare(M, index);
        }
        break;
      }
      case 8: {
        const at2 = s.p;
        const fx = s.u32();
        if (fx >= M.funcs.length) s.fail("function index " + fx + " out of bounds", at2);
        const t = M.types[M.funcs[fx]];
        if (t.params.length || t.results.length) s.fail("invalid start function: non-zero parameter or return count", at2);
        M.start = fx;
        break;
      }
      case 9: {
        const n = s.u32();
        for (let i = 0; i < n; i++) M.elems.push(elemSegment(s, M));
        break;
      }
      case 12:
        M.dataCount = s.u32();
        break;
      case 10: {
        const n = s.u32();
        if (n !== nFuncDecls) s.fail("function body count " + n + " mismatch (" + nFuncDecls + " expected)", start);
        for (let i = 0; i < n; i++) {
          const sz = s.u32();
          const bs = s.p;
          if (bs + sz > end) s.fail("function body extends beyond end of code section", bs);
          const fr = new Reader(bytes, bs, bs + sz);
          const locals = [];
          let total = 0;
          for (let k = fr.u32(); k > 0; k--) {
            const at3 = fr.p;
            const cnt = fr.u32();
            total += cnt;
            if (total > 50000) fr.fail("local count too large", at3);
            const t = valType(fr);
            for (let j = 0; j < cnt; j++) locals.push(t);
          }
          M.codes.push({ locals, start: fr.p, end: bs + sz });
          s.p = bs + sz;
        }
        break;
      }
      case 11: {
        const n = s.u32();
        if (M.dataCount !== null && n !== M.dataCount) s.fail("data segments count " + n + " mismatch (" + M.dataCount + " expected)", start);
        for (let i = 0; i < n; i++) {
          const fat = s.p;
          const flags = s.u32();
          let mem = 0, offset = null, mode = "active";
          if (flags === 1) mode = "passive";
          else if (flags === 2) mem = s.u32();
          else if (flags !== 0) s.fail("illegal flag value " + flags, fat);
          if (mode === "active") {
            if (mem >= M.mems.length) s.fail("invalid memory index " + mem + " for data section", fat);
            offset = constExpr(s, M, I32, M.globals.length);
          }
          const len = s.u32();
          M.datas.push({ mode, mem, offset, bytes: s.bytes(len) });
        }
        break;
      }
      case 13:
        s.fail("exception handling is not supported", at);
    }
    if (s.p !== end) s.fail("section was " + (s.p < end ? "longer" : "shorter") + " than expected size (" + size + " bytes expected, " + (s.p - start) + " decoded)", s.p);
    r.p = end;
  }
  if (nFuncDecls !== M.codes.length) r.fail("function count is " + nFuncDecls + ", but code section is absent", r.p);
  if (M.dataCount !== null && M.datas.length !== M.dataCount) r.fail("data segments count 0 mismatch (" + M.dataCount + " expected)", r.p);
  return M;
}

function hex4(b, at) {
  let s = "";
  for (let i = 0; i < 4; i++) s += (i ? " " : "") + ((b[at + i] | 0) < 16 ? "0" : "") + (b[at + i] | 0).toString(16);
  return s;
}
function sigOf(params, results) {
  let s = "";
  for (const t of params) s += TYPE_NAMES[t][0] + (t === EXTERNREF ? "x" : "");
  s += ":";
  for (const t of results) s += TYPE_NAMES[t][0] + (t === EXTERNREF ? "x" : "");
  return s;
}
function declare(M, fx) {
  if (M.declared === null) M.declared = new Set();
  M.declared.add(fx);
}
function kindName(k) {
  return ["function", "table", "memory", "global"][k];
}
const OP_NAMES = "unreachable nop block loop if else try catch throw rethrow throw_ref end br br_if br_table return call call_indirect return_call return_call_indirect".split(" ");
const OP_NAMES2 = { 0x1a: "drop", 0x1b: "select", 0x1c: "select", 0x20: "local.get", 0x21: "local.set", 0x22: "local.tee", 0x23: "global.get", 0x24: "global.set", 0x25: "table.get", 0x26: "table.set", 0x3f: "memory.size", 0x40: "memory.grow", 0xd1: "ref.is_null" };
const NUM_NAMES = "i32.eqz i32.eq i32.ne i32.lt_s i32.lt_u i32.gt_s i32.gt_u i32.le_s i32.le_u i32.ge_s i32.ge_u i64.eqz i64.eq i64.ne i64.lt_s i64.lt_u i64.gt_s i64.gt_u i64.le_s i64.le_u i64.ge_s i64.ge_u f32.eq f32.ne f32.lt f32.gt f32.le f32.ge f64.eq f64.ne f64.lt f64.gt f64.le f64.ge i32.clz i32.ctz i32.popcnt i32.add i32.sub i32.mul i32.div_s i32.div_u i32.rem_s i32.rem_u i32.and i32.or i32.xor i32.shl i32.shr_s i32.shr_u i32.rotl i32.rotr i64.clz i64.ctz i64.popcnt i64.add i64.sub i64.mul i64.div_s i64.div_u i64.rem_s i64.rem_u i64.and i64.or i64.xor i64.shl i64.shr_s i64.shr_u i64.rotl i64.rotr f32.abs f32.neg f32.ceil f32.floor f32.trunc f32.nearest f32.sqrt f32.add f32.sub f32.mul f32.div f32.min f32.max f32.copysign f64.abs f64.neg f64.ceil f64.floor f64.trunc f64.nearest f64.sqrt f64.add f64.sub f64.mul f64.div f64.min f64.max f64.copysign i32.wrap_i64 i32.trunc_f32_s i32.trunc_f32_u i32.trunc_f64_s i32.trunc_f64_u i64.extend_i32_s i64.extend_i32_u i64.trunc_f32_s i64.trunc_f32_u i64.trunc_f64_s i64.trunc_f64_u f32.convert_i32_s f32.convert_i32_u f32.convert_i64_s f32.convert_i64_u f32.demote_f64 f64.convert_i32_s f64.convert_i32_u f64.convert_i64_s f64.convert_i64_u f64.promote_f32 i32.reinterpret_f32 i64.reinterpret_f64 f32.reinterpret_i32 f64.reinterpret_i64 i32.extend8_s i32.extend16_s i64.extend8_s i64.extend16_s i64.extend32_s".split(" ");
const MEM_NAMES = "i32.load i64.load f32.load f64.load i32.load8_s i32.load8_u i32.load16_s i32.load16_u i64.load8_s i64.load8_u i64.load16_s i64.load16_u i64.load32_s i64.load32_u i32.store i64.store f32.store f64.store i32.store8 i32.store16 i64.store8 i64.store16 i64.store32".split(" ");
function opName(op) {
  if (op < OP_NAMES.length) return OP_NAMES[op];
  if (OP_NAMES2[op] !== undefined) return OP_NAMES2[op];
  if (op >= 0x28 && op <= 0x3e) return MEM_NAMES[op - 0x28];
  if (op >= 0x45 && op <= 0xc4) return NUM_NAMES[op - 0x45];
  return "0x" + op.toString(16);
}

/**
 * A constant expression of type want, as a list of [op, imm] pairs evaluated at
 * instantiation. It may read the first nGlobals globals if immutable.
 */
function constExpr(s, M, want, nGlobals) {
  const code = [];
  const stack = [];
  for (;;) {
    const at = s.p;
    const op = s.u8();
    switch (op) {
      case 0x0b:
        if (stack.length !== 1 || stack[0] !== want)
          s.fail("type error in constant expression[0] (expected " + TYPE_NAMES[want] + ", got " + (stack.length ? TYPE_NAMES[stack[stack.length - 1]] : "<empty>") + ")", at);
        return code;
      case 0x41: code.push([op, s.s32()]); stack.push(I32); break;
      case 0x42: code.push([op, s.s64()]); stack.push(I64); break;
      case 0x43: code.push([op, s.f32()]); stack.push(F32); break;
      case 0x44: code.push([op, s.f64()]); stack.push(F64); break;
      case 0xd0: { const t = refType(s); code.push([op, t]); stack.push(t); break; }
      case 0xd2: {
        const fx = s.u32();
        if (fx >= M.funcs.length) s.fail("function index #" + fx + " is out of bounds", at + 1);
        declare(M, fx);
        code.push([op, fx]);
        stack.push(FUNCREF);
        break;
      }
      case 0x23: {
        const g = s.u32();
        if (g >= nGlobals) s.fail("Invalid global index: " + g, at + 1);
        if (M.globals[g].mut) s.fail("mutable globals cannot be used in constant expressions", at);
        code.push([op, g]);
        stack.push(M.globals[g].type);
        break;
      }
      case 0x6a: case 0x6b: case 0x6c: case 0x7c: case 0x7d: case 0x7e: {
        const t = op < 0x70 ? I32 : I64;
        if (stack.length < 2 || stack[stack.length - 1] !== t || stack[stack.length - 2] !== t) s.fail("type error in constant expression", at);
        stack.pop();
        code.push([op, 0]);
        break;
      }
      default:
        s.fail("opcode " + opName(op) + " is not allowed in constant expressions", at);
    }
  }
}

function elemSegment(s, M) {
  const at = s.p;
  const flags = s.u32();
  if (flags > 7) s.fail("illegal flag value " + flags, at);
  const passive = (flags & 1) !== 0;
  const explicit = (flags & 2) !== 0;
  const exprs = (flags & 4) !== 0;
  const seg = { mode: passive ? (explicit ? "declarative" : "passive") : "active", table: 0, offset: null, type: FUNCREF, inits: [] };
  if (!passive) {
    if (explicit) seg.table = s.u32();
    if (seg.table >= M.tables.length) s.fail("out of bounds table index " + seg.table, at);
    seg.offset = constExpr(s, M, I32, M.globals.length);
  }
  if (passive || explicit) {
    if (exprs) seg.type = refType(s);
    else {
      const k = s.u8();
      if (k !== 0) s.fail("illegal element kind 0x" + k.toString(16) + ". Must be 0x0", s.p - 1);
    }
  }
  const n = s.u32();
  for (let i = 0; i < n; i++) {
    if (exprs) seg.inits.push(constExpr(s, M, seg.type, M.globals.length));
    else {
      const fat = s.p;
      const fx = s.u32();
      if (fx >= M.funcs.length) s.fail("element function index " + fx + " out of bounds", fat);
      declare(M, fx);
      seg.inits.push([[0xd2, fx]]);
    }
  }
  if (seg.mode === "active" && M.tables[seg.table].elem !== seg.type)
    s.fail("Element segment of type " + TYPE_NAMES[seg.type] + " cannot be used for table of type " + TYPE_NAMES[M.tables[seg.table].elem], at);
  return seg;
}

// ---- Validation and code generation --------------------------------------------------

// Numeric operators: opcode -> [operand types, result type, JS template]. A and B are the
// operands' slots.
const NUM = {};
function num(op, ins, out, tpl) {
  NUM[op] = [ins, out, tpl];
}
{
  const i = I32, I = I64, f = F32, d = F64;
  num(0x45, [i], i, "(A===0?1:0)");
  num(0x46, [i, i], i, "(A===B?1:0)");
  num(0x47, [i, i], i, "(A!==B?1:0)");
  num(0x48, [i, i], i, "(A<B?1:0)");
  num(0x49, [i, i], i, "((A>>>0)<(B>>>0)?1:0)");
  num(0x4a, [i, i], i, "(A>B?1:0)");
  num(0x4b, [i, i], i, "((A>>>0)>(B>>>0)?1:0)");
  num(0x4c, [i, i], i, "(A<=B?1:0)");
  num(0x4d, [i, i], i, "((A>>>0)<=(B>>>0)?1:0)");
  num(0x4e, [i, i], i, "(A>=B?1:0)");
  num(0x4f, [i, i], i, "((A>>>0)>=(B>>>0)?1:0)");
  num(0x50, [I], i, "(A===0n?1:0)");
  num(0x51, [I, I], i, "(A===B?1:0)");
  num(0x52, [I, I], i, "(A!==B?1:0)");
  num(0x53, [I, I], i, "(A<B?1:0)");
  num(0x54, [I, I], i, "($u64(A)<$u64(B)?1:0)");
  num(0x55, [I, I], i, "(A>B?1:0)");
  num(0x56, [I, I], i, "($u64(A)>$u64(B)?1:0)");
  num(0x57, [I, I], i, "(A<=B?1:0)");
  num(0x58, [I, I], i, "($u64(A)<=$u64(B)?1:0)");
  num(0x59, [I, I], i, "(A>=B?1:0)");
  num(0x5a, [I, I], i, "($u64(A)>=$u64(B)?1:0)");
  for (let base = 0x5b; base <= 0x61; base += 6) {
    const t = base === 0x5b ? f : d;
    num(base, [t, t], i, "(A===B?1:0)");
    num(base + 1, [t, t], i, "(A!==B?1:0)");
    num(base + 2, [t, t], i, "(A<B?1:0)");
    num(base + 3, [t, t], i, "(A>B?1:0)");
    num(base + 4, [t, t], i, "(A<=B?1:0)");
    num(base + 5, [t, t], i, "(A>=B?1:0)");
  }
  num(0x67, [i], i, "$clz(A)");
  num(0x68, [i], i, "$ctz32(A)");
  num(0x69, [i], i, "$pop32(A)");
  num(0x6a, [i, i], i, "((A+B)|0)");
  num(0x6b, [i, i], i, "((A-B)|0)");
  num(0x6c, [i, i], i, "$imul(A,B)");
  num(0x6d, [i, i], i, "$divs32(A,B)");
  num(0x6e, [i, i], i, "$divu32(A,B)");
  num(0x6f, [i, i], i, "$rems32(A,B)");
  num(0x70, [i, i], i, "$remu32(A,B)");
  num(0x71, [i, i], i, "(A&B)");
  num(0x72, [i, i], i, "(A|B)");
  num(0x73, [i, i], i, "(A^B)");
  num(0x74, [i, i], i, "(A<<B)");
  num(0x75, [i, i], i, "(A>>B)");
  num(0x76, [i, i], i, "((A>>>B)|0)");
  num(0x77, [i, i], i, "((A<<B)|(A>>>(32-(B&31))))");
  num(0x78, [i, i], i, "((A>>>B)|(A<<(32-(B&31))))");
  num(0x79, [I], I, "$clz64(A)");
  num(0x7a, [I], I, "$ctz64(A)");
  num(0x7b, [I], I, "$pop64(A)");
  num(0x7c, [I, I], I, "$w(A+B)");
  num(0x7d, [I, I], I, "$w(A-B)");
  num(0x7e, [I, I], I, "$w(A*B)");
  num(0x7f, [I, I], I, "$divs64(A,B)");
  num(0x80, [I, I], I, "$divu64(A,B)");
  num(0x81, [I, I], I, "$rems64(A,B)");
  num(0x82, [I, I], I, "$remu64(A,B)");
  num(0x83, [I, I], I, "(A&B)");
  num(0x84, [I, I], I, "(A|B)");
  num(0x85, [I, I], I, "(A^B)");
  num(0x86, [I, I], I, "$w(A<<(B&63n))");
  num(0x87, [I, I], I, "(A>>(B&63n))");
  num(0x88, [I, I], I, "$w($u64(A)>>(B&63n))");
  num(0x89, [I, I], I, "$rotl64(A,B)");
  num(0x8a, [I, I], I, "$rotr64(A,B)");
  for (let base = 0x8b; base <= 0x99; base += 14) {
    const t = base === 0x8b ? f : d, R = base === 0x8b ? "$f" : "";
    num(base, [t], t, "$abs(A)");
    num(base + 1, [t], t, "(-A)");
    num(base + 2, [t], t, "$ceil(A)");
    num(base + 3, [t], t, "$floor(A)");
    num(base + 4, [t], t, "$trunc(A)");
    num(base + 5, [t], t, "$nearest(A)");
    num(base + 6, [t], t, R + "($sqrt(A))");
    num(base + 7, [t, t], t, R + "(A+B)");
    num(base + 8, [t, t], t, R + "(A-B)");
    num(base + 9, [t, t], t, R + "(A*B)");
    num(base + 10, [t, t], t, R + "(A/B)");
    num(base + 11, [t, t], t, "$min(A,B)");
    num(base + 12, [t, t], t, "$max(A,B)");
    num(base + 13, [t, t], t, "$cps(A,B)");
  }
  num(0xa7, [I], i, "$N($w32(A))");
  num(0xa8, [f], i, "$t32s(A)");
  num(0xa9, [f], i, "$t32u(A)");
  num(0xaa, [d], i, "$t32s(A)");
  num(0xab, [d], i, "$t32u(A)");
  num(0xac, [i], I, "$B(A)");
  num(0xad, [i], I, "$B(A>>>0)");
  num(0xae, [f], I, "$t64s(A)");
  num(0xaf, [f], I, "$t64u(A)");
  num(0xb0, [d], I, "$t64s(A)");
  num(0xb1, [d], I, "$t64u(A)");
  num(0xb2, [i], f, "$f(A)");
  num(0xb3, [i], f, "$f(A>>>0)");
  num(0xb4, [I], f, "$cvf32(A)");
  num(0xb5, [I], f, "$cvf32($u64(A))");
  num(0xb6, [d], f, "$f(A)");
  num(0xb7, [i], d, "(A)");
  num(0xb8, [i], d, "(A>>>0)");
  num(0xb9, [I], d, "$N(A)");
  num(0xba, [I], d, "$N($u64(A))");
  num(0xbb, [f], d, "(A)");
  num(0xbc, [f], i, "$ri32(A)");
  num(0xbd, [d], I, "$ri64(A)");
  num(0xbe, [i], f, "$rf32(A)");
  num(0xbf, [I], d, "$rf64(A)");
  num(0xc0, [i], i, "((A<<24)>>24)");
  num(0xc1, [i], i, "((A<<16)>>16)");
  num(0xc2, [I], I, "$sx8(A)");
  num(0xc3, [I], I, "$sx16(A)");
  num(0xc4, [I], I, "$w32(A)");
  // 0xfc 0..7, saturating truncation, keyed 0x100 + n.
  num(0x100, [f], i, "$ts32s(A)");
  num(0x101, [f], i, "$ts32u(A)");
  num(0x102, [d], i, "$ts32s(A)");
  num(0x103, [d], i, "$ts32u(A)");
  num(0x104, [f], I, "$ts64s(A)");
  num(0x105, [f], I, "$ts64u(A)");
  num(0x106, [d], I, "$ts64s(A)");
  num(0x107, [d], I, "$ts64u(A)");
}

// Loads and stores: opcode -> [value type, bytes, natural alignment log2, JS].
const LOADS = {
  0x28: [I32, 4, 2, "$dv.getInt32(E,true)"],
  0x29: [I64, 8, 3, "$dv.getBigInt64(E,true)"],
  0x2a: [F32, 4, 2, "$dv.getFloat32(E,true)"],
  0x2b: [F64, 8, 3, "$dv.getFloat64(E,true)"],
  0x2c: [I32, 1, 0, "$dv.getInt8(E)"],
  0x2d: [I32, 1, 0, "$dv.getUint8(E)"],
  0x2e: [I32, 2, 1, "$dv.getInt16(E,true)"],
  0x2f: [I32, 2, 1, "$dv.getUint16(E,true)"],
  0x30: [I64, 1, 0, "$B($dv.getInt8(E))"],
  0x31: [I64, 1, 0, "$B($dv.getUint8(E))"],
  0x32: [I64, 2, 1, "$B($dv.getInt16(E,true))"],
  0x33: [I64, 2, 1, "$B($dv.getUint16(E,true))"],
  0x34: [I64, 4, 2, "$B($dv.getInt32(E,true))"],
  0x35: [I64, 4, 2, "$B($dv.getUint32(E,true))"],
};
const STORES = {
  0x36: [I32, 4, 2, "$dv.setInt32(E,V,true)"],
  0x37: [I64, 8, 3, "$dv.setBigInt64(E,V,true)"],
  0x38: [F32, 4, 2, "$dv.setFloat32(E,V,true)"],
  0x39: [F64, 8, 3, "$dv.setFloat64(E,V,true)"],
  0x3a: [I32, 1, 0, "$dv.setInt8(E,V)"],
  0x3b: [I32, 2, 1, "$dv.setInt16(E,V,true)"],
  0x3c: [I64, 1, 0, "$dv.setInt8(E,$N(V&255n))"],
  0x3d: [I64, 2, 1, "$dv.setInt16(E,$N(V&65535n),true)"],
  0x3e: [I64, 4, 2, "$dv.setUint32(E,$N(V&4294967295n),true)"],
};

function jsNum(v) {
  if (v !== v) return "(0/0)";
  if (v === 1 / 0) return "(1/0)";
  if (v === -1 / 0) return "(-1/0)";
  if (v === 0 && 1 / v < 0) return "(-0)";
  return v < 0 ? "(" + $String(v) + ")" : $String(v);
}
// Fills placeholders A and B (not in helper names such as $B).
function fill(tpl, a, b) {
  let s = "";
  for (let k = 0; k < tpl.length; k++) {
    const c = tpl[k];
    const name = k > 0 && tpl[k - 1] === "$";
    s += c === "A" && !name ? a : c === "B" && !name ? b : c;
  }
  return s;
}
function fillEV(tpl, e, v) {
  let s = "";
  for (let k = 0; k < tpl.length; k++) {
    const c = tpl[k];
    s += c === "E" ? e : c === "V" ? v : c;
  }
  return s;
}

/** Validates function fi's body and returns its JS source. */
function compileFunction(M, bytes, fi) {
  const code = M.codes[fi - M.nImportFuncs];
  const type = M.types[M.funcs[fi]];
  const r = new Reader(bytes, code.start, code.end);
  let curName = "";
  const fail = (msg, at) => {
    throw new CompileError(apiName + "Compiling function #" + fi + " failed: " + msg + " @+" + (at === undefined ? r.p : at));
  };
  const locals = type.params.concat(code.locals);
  const vs = []; // operand types; 0 is unknown (unreachable code)
  const ctrl = [];
  let out = "";
  let maxDepth = 0;
  let labels = 0;
  let usesTmp = false;
  const S = (n) => "s" + n;
  const top = () => ctrl[ctrl.length - 1];
  const live = () => {
    const c = top();
    return !c.unreachable && !c.dead;
  };
  const emit = (s) => {
    if (live()) out += s;
  };
  const push = (t) => {
    vs.push(t);
    if (vs.length > maxDepth) maxDepth = vs.length;
  };
  const pop = (want) => {
    const c = top();
    if (vs.length === c.height) {
      if (c.unreachable) return want || 0;
      fail("not enough arguments on the stack for " + curName + " (need 1, got 0)");
    }
    const t = vs.pop();
    if (want && t && t !== want) fail(curName + " expected type " + TYPE_NAMES[want] + ", found " + TYPE_NAMES[t]);
    return t || want || 0;
  };
  const popAll = (types) => {
    for (let k = types.length - 1; k >= 0; k--) pop(types[k]);
  };
  const pushAll = (types) => {
    for (const t of types) push(t);
  };
  const labelTypes = (c) => (c.kind === "loop" ? c.params : c.results);
  const pushCtrl = (kind, params, results) => {
    const parent = ctrl.length ? top() : null;
    const c = {
      kind, params, results,
      height: vs.length - params.length,
      unreachable: false,
      dead: parent !== null && (parent.unreachable || parent.dead),
      label: "L" + labels++,
    };
    ctrl.push(c);
    return c;
  };
  const setUnreachable = () => {
    const c = top();
    vs.length = c.height;
    c.unreachable = true;
  };
  const retCode = () => {
    const n = type.results.length;
    const from = vs.length - n;
    if (n === 0) return "return;";
    if (n === 1) return "return " + S(from) + ";";
    let s = "return [";
    for (let k = 0; k < n; k++) s += (k ? "," : "") + S(from + k);
    return s + "];";
  };
  // The JS for a branch to depth, with the label's values on top of the stack.
  const branch = (depth) => {
    const c = ctrl[ctrl.length - 1 - depth];
    if (c.kind === "func") return retCode();
    const n = labelTypes(c).length;
    const from = vs.length - n;
    let s = "";
    if (from !== c.height) for (let k = 0; k < n; k++) s += S(c.height + k) + "=" + S(from + k) + ";";
    return s + (c.kind === "loop" ? "continue " : "break ") + c.label + ";";
  };
  // Pops the label's types (checking them) and returns what was popped.
  const checkBranch = (depth, at) => {
    if (depth >= ctrl.length) fail("invalid branch depth: " + depth, at);
    const want = labelTypes(ctrl[ctrl.length - 1 - depth]);
    const got = [];
    for (let k = want.length - 1; k >= 0; k--) got.unshift(pop(want[k]));
    return got;
  };
  const blockType = () => {
    const at = r.p;
    const b = r.s33();
    if (b === -64) return [[], []];
    if (b < 0) {
      const t = b + 128;
      if (t === I32 || t === I64 || t === F32 || t === F64 || t === FUNCREF || t === EXTERNREF) return [[], [t]];
      if (t === V128) fail("Wasm SIMD unsupported", at);
      fail("invalid block type", at);
    }
    if (b >= M.types.length) fail("block type index " + b + " is not a signature definition", at);
    return [M.types[b].params, M.types[b].results];
  };
  const needMem = (at) => {
    if (M.mems.length === 0) fail("memory instruction with no memory", at);
  };
  const memarg = (natural, at) => {
    const aat = r.p;
    const align = r.u32();
    if (align >= 64) fail("multi-memory is not supported", aat);
    if (align > natural) fail("invalid alignment; expected maximum alignment is " + natural + ", actual alignment is " + align, aat);
    const off = r.u32();
    needMem(at);
    return off;
  };
  const memIndex = (at) => {
    const m = r.u8();
    if (m !== 0) fail("expected memory index 0, found " + m, at);
    needMem(at);
  };
  const tableIdx = (at) => {
    const t = r.u32();
    if (t >= M.tables.length) fail("table index " + t + " out of bounds (" + M.tables.length + " tables)", at);
    return t;
  };
  const elemIdx = (at) => {
    const e = r.u32();
    if (e >= M.elems.length) fail("invalid element segment index: " + e, at);
    return e;
  };
  const dataIdx = (at) => {
    if (M.dataCount === null) fail("data count section required", at);
    const e = r.u32();
    if (e >= M.dataCount) fail("invalid data segment index: " + e, at);
    return e;
  };
  const argList = (base, n) => {
    let args = "";
    for (let k = 0; k < n; k++) args += (k ? "," : "") + S(base + k);
    return args;
  };
  const call = (t, callee) => {
    popAll(t.params);
    const base = vs.length;
    const callExpr = callee + "(" + argList(base, t.params.length) + ")";
    if (t.results.length === 0) emit(callExpr + ";");
    else if (t.results.length === 1) emit(S(base) + "=" + callExpr + ";");
    else {
      usesTmp = true;
      let s = "$t=" + callExpr + ";";
      for (let k = 0; k < t.results.length; k++) s += S(base + k) + "=$t[" + k + "];";
      emit(s);
    }
    pushAll(t.results);
  };
  const tailCall = (t, callee, at) => {
    const rs = type.results;
    if (rs.length !== t.results.length || rs.some((x, k) => x !== t.results[k])) fail("tail call return types mismatch", at);
    popAll(t.params);
    emit("return " + callee + "(" + argList(vs.length, t.params.length) + ");");
    setUnreachable();
  };
  const fallthru = (c, at) => {
    popAll(c.results);
    if (vs.length !== c.height) fail("expected " + c.results.length + " elements on the stack for fallthru, found " + (vs.length - c.height + c.results.length), at);
  };

  pushCtrl("func", [], type.results);
  for (;;) {
    const at = r.p;
    if (r.eof()) fail("function body must end with \"end\" opcode", at);
    const op = r.u8();
    curName = opName(op);
    const simple = NUM[op];
    if (simple !== undefined) {
      const ins = simple[0];
      const dpt = vs.length;
      popAll(ins);
      if (live()) out += S(vs.length) + "=" + (ins.length === 2 ? fill(simple[2], S(dpt - 2), S(dpt - 1)) : fill(simple[2], S(dpt - 1), "")) + ";";
      push(simple[1]);
      continue;
    }
    const ld = LOADS[op];
    if (ld !== undefined) {
      const off = memarg(ld[2], at);
      pop(I32);
      const a = vs.length;
      emit(S(a) + "=" + fillEV(ld[3], "$ea(" + S(a) + "," + off + "," + ld[1] + ")", "") + ";");
      push(ld[0]);
      continue;
    }
    const st = STORES[op];
    if (st !== undefined) {
      const off = memarg(st[2], at);
      pop(st[0]);
      pop(I32);
      const a = vs.length;
      emit(fillEV(st[3], "$ea(" + S(a) + "," + off + "," + st[1] + ")", S(a + 1)) + ";");
      continue;
    }
    switch (op) {
      case 0x00:
        emit("$trap(\"unreachable\");");
        setUnreachable();
        break;
      case 0x01:
        break;
      case 0x02: case 0x03: {
        const [ps, rs] = blockType();
        popAll(ps);
        pushAll(ps);
        const c = pushCtrl(op === 0x02 ? "block" : "loop", ps, rs);
        if (!c.dead) out += c.label + (op === 0x02 ? ":{" : ":while(true){");
        break;
      }
      case 0x04: {
        const [ps, rs] = blockType();
        pop(I32);
        const cond = S(vs.length);
        popAll(ps);
        pushAll(ps);
        const c = pushCtrl("if", ps, rs);
        if (!c.dead) out += c.label + ":if(" + cond + "){";
        break;
      }
      case 0x05: {
        const c = top();
        if (c.kind !== "if") fail("else does not match an if", at);
        fallthru(c, at);
        c.kind = "else";
        c.unreachable = false;
        pushAll(c.params);
        if (!c.dead) out += "}else{";
        break;
      }
      case 0x0b: {
        const c = top();
        fallthru(c, at);
        if (c.kind === "if") {
          const ps = c.params, rs = c.results;
          if (ps.length !== rs.length || ps.some((x, k) => x !== rs[k])) fail("expected " + rs.length + " elements on the stack for fallthru, found " + ps.length, at);
        }
        if (c.kind === "func") {
          if (!c.unreachable) {
            pushAll(c.results);
            out += retCode();
          }
          if (r.p !== r.end) fail("trailing code after function end", r.p);
          const np = type.params.length;
          let head = "function f" + fi + "(";
          for (let k = 0; k < np; k++) head += (k ? "," : "") + "l" + k;
          head += "){";
          if (locals.length > np) {
            head += "let ";
            for (let k = np; k < locals.length; k++) {
              const t = locals[k];
              head += (k > np ? "," : "") + "l" + k + "=" + (t === I64 ? "0n" : t === FUNCREF || t === EXTERNREF ? "null" : "0");
            }
            head += ";";
          }
          if (maxDepth > 0 || usesTmp) {
            head += "let ";
            for (let k = 0; k < maxDepth; k++) head += (k ? "," : "") + S(k);
            if (usesTmp) head += (maxDepth ? "," : "") + "$t";
            head += ";";
          }
          return head + out + "}\n";
        }
        ctrl.pop();
        if (!c.dead) out += c.kind === "loop" ? "break;}" : "}";
        pushAll(c.results);
        break;
      }
      case 0x0c: {
        const depth = r.u32();
        checkBranch(depth, at);
        const c = ctrl[ctrl.length - 1 - depth];
        pushAll(labelTypes(c));
        emit(branch(depth));
        setUnreachable();
        break;
      }
      case 0x0d: {
        const depth = r.u32();
        pop(I32);
        const cond = S(vs.length);
        const got = checkBranch(depth, at);
        pushAll(got);
        emit("if(" + cond + "){" + branch(depth) + "}");
        break;
      }
      case 0x0e: {
        const n = r.u32();
        const depths = [];
        for (let k = 0; k <= n; k++) {
          const dd = r.u32();
          if (dd >= ctrl.length) fail("invalid branch depth: " + dd, at);
          depths.push(dd);
        }
        pop(I32);
        const idx = S(vs.length);
        const arity = labelTypes(ctrl[ctrl.length - 1 - depths[n]]).length;
        const save = vs.slice();
        for (const dd of depths) {
          if (labelTypes(ctrl[ctrl.length - 1 - dd]).length !== arity) fail("br_table: inconsistent arity", at);
          checkBranch(dd, at);
          vs.length = 0;
          for (const t of save) vs.push(t);
        }
        if (live()) {
          let s = "switch(" + idx + "){";
          for (let k = 0; k < n; k++) s += "case " + k + ":" + branch(depths[k]);
          s += "default:" + branch(depths[n]) + "}";
          out += s;
        }
        setUnreachable();
        break;
      }
      case 0x0f: {
        const got = [];
        for (let k = type.results.length - 1; k >= 0; k--) got.unshift(pop(type.results[k]));
        pushAll(got);
        emit(retCode());
        setUnreachable();
        break;
      }
      case 0x10: case 0x12: {
        const fx = r.u32();
        if (fx >= M.funcs.length) fail("function index #" + fx + " is out of bounds", at + 1);
        const t = M.types[M.funcs[fx]];
        if (op === 0x10) call(t, "f" + fx);
        else tailCall(t, "f" + fx, at);
        break;
      }
      case 0x11: case 0x13: {
        const tx = r.u32();
        if (tx >= M.types.length) fail("invalid signature index: " + tx, at + 1);
        const tb = tableIdx(at);
        if (M.tables[tb].elem !== FUNCREF) fail("call_indirect: immediate table #" + tb + " is not of a function type", at);
        pop(I32);
        const t = M.types[tx];
        const callee = "$ci($T" + tb + "," + S(vs.length) + ",\"" + t.sig + "\")";
        if (op === 0x11) call(t, callee);
        else tailCall(t, callee, at);
        break;
      }
      case 0x1a:
        pop(0);
        break;
      case 0x1b: case 0x1c: {
        let want = 0;
        if (op === 0x1c) {
          const n = r.u32();
          if (n !== 1) fail("invalid number of types for select", at);
          want = valType(r);
        }
        pop(I32);
        const t1 = pop(want);
        const t2 = pop(want || t1);
        const t = want || t1 || t2;
        if (op === 0x1b && (t === FUNCREF || t === EXTERNREF)) fail("select without type is only valid for value type inputs", at);
        if (op === 0x1b && t1 && t2 && t1 !== t2) fail("type mismatch in select", at);
        const b = vs.length;
        emit(S(b) + "=" + S(b + 2) + "?" + S(b) + ":" + S(b + 1) + ";");
        push(t);
        break;
      }
      case 0x20: case 0x21: case 0x22: {
        const x = r.u32();
        if (x >= locals.length) fail("invalid local index: " + x, at + 1);
        const t = locals[x];
        if (op === 0x20) {
          emit(S(vs.length) + "=l" + x + ";");
          push(t);
        } else {
          pop(t);
          emit("l" + x + "=" + S(vs.length) + ";");
          if (op === 0x22) push(t);
        }
        break;
      }
      case 0x23: case 0x24: {
        const x = r.u32();
        if (x >= M.globals.length) fail("Invalid global index: " + x, at + 1);
        const g = M.globals[x];
        if (op === 0x23) {
          emit(S(vs.length) + "=$g" + x + ".v;");
          push(g.type);
        } else {
          if (!g.mut) fail("immutable global #" + x + " cannot be assigned", at);
          pop(g.type);
          emit("$g" + x + ".v=" + S(vs.length) + ";");
        }
        break;
      }
      case 0x25: case 0x26: {
        const tb = tableIdx(at);
        const et = M.tables[tb].elem;
        if (op === 0x25) {
          pop(I32);
          emit(S(vs.length) + "=$tget($T" + tb + "," + S(vs.length) + ");");
          push(et);
        } else {
          pop(et);
          pop(I32);
          emit("$tset($T" + tb + "," + S(vs.length) + "," + S(vs.length + 1) + ");");
        }
        break;
      }
      case 0x3f: case 0x40: {
        memIndex(at);
        if (op === 0x3f) {
          emit(S(vs.length) + "=$ml/65536;");
        } else {
          pop(I32);
          emit(S(vs.length) + "=$grow(" + S(vs.length) + ");");
        }
        push(I32);
        break;
      }
      case 0x41:
        emit(S(vs.length) + "=" + jsNum(r.s32()) + ";");
        push(I32);
        break;
      case 0x42: {
        const v = r.s64();
        emit(S(vs.length) + "=" + (v < 0n ? "(" + v + "n)" : v + "n") + ";");
        push(I64);
        break;
      }
      case 0x43:
        emit(S(vs.length) + "=" + jsNum(r.f32()) + ";");
        push(F32);
        break;
      case 0x44:
        emit(S(vs.length) + "=" + jsNum(r.f64()) + ";");
        push(F64);
        break;
      case 0xd0: {
        const t = refType(r);
        emit(S(vs.length) + "=null;");
        push(t);
        break;
      }
      case 0xd1: {
        const t = pop(0);
        if (t && t !== FUNCREF && t !== EXTERNREF) fail("ref.is_null[0] expected reference type, found " + TYPE_NAMES[t], at);
        emit(S(vs.length) + "=" + S(vs.length) + "===null?1:0;");
        push(I32);
        break;
      }
      case 0xd2: {
        const fx = r.u32();
        if (fx >= M.funcs.length) fail("function index #" + fx + " is out of bounds", at + 1);
        if (!M.declared || !M.declared.has(fx)) fail("undeclared reference to function #" + fx, at + 1);
        emit(S(vs.length) + "=$rf(" + fx + ");");
        push(FUNCREF);
        break;
      }
      case 0xfc: {
        const sub = r.u32();
        curName = "0xfc" + sub;
        if (sub <= 7) {
          const [ins, outT, tpl] = NUM[0x100 + sub];
          popAll(ins);
          emit(S(vs.length) + "=" + fill(tpl, S(vs.length), "") + ";");
          push(outT);
          break;
        }
        switch (sub) {
          case 8: {
            const seg = dataIdx(at);
            memIndex(at);
            pop(I32); pop(I32); pop(I32);
            emit("$minit(" + seg + "," + argList(vs.length, 3) + ");");
            break;
          }
          case 9:
            emit("$ddrop(" + dataIdx(at) + ");");
            break;
          case 10:
            memIndex(at);
            memIndex(at);
            pop(I32); pop(I32); pop(I32);
            emit("$mcopy(" + argList(vs.length, 3) + ");");
            break;
          case 11:
            memIndex(at);
            pop(I32); pop(I32); pop(I32);
            emit("$mfill(" + argList(vs.length, 3) + ");");
            break;
          case 12: {
            const seg = elemIdx(at);
            const tb = tableIdx(at);
            if (M.elems[seg].type !== M.tables[tb].elem) fail("table.init: type mismatch", at);
            pop(I32); pop(I32); pop(I32);
            emit("$tinit($T" + tb + "," + seg + "," + argList(vs.length, 3) + ");");
            break;
          }
          case 13:
            emit("$edrop(" + elemIdx(at) + ");");
            break;
          case 14: {
            const t1 = tableIdx(at);
            const t2 = tableIdx(at);
            if (M.tables[t1].elem !== M.tables[t2].elem) fail("table.copy: type mismatch", at);
            pop(I32); pop(I32); pop(I32);
            emit("$tcopy($T" + t1 + ",$T" + t2 + "," + argList(vs.length, 3) + ");");
            break;
          }
          case 15: {
            const tb = tableIdx(at);
            pop(I32);
            pop(M.tables[tb].elem);
            const b = vs.length;
            emit(S(b) + "=$tgrow($T" + tb + "," + S(b) + "," + S(b + 1) + ");");
            push(I32);
            break;
          }
          case 16: {
            const tb = tableIdx(at);
            emit(S(vs.length) + "=$T" + tb + ".a.length;");
            push(I32);
            break;
          }
          case 17: {
            const tb = tableIdx(at);
            pop(I32);
            pop(M.tables[tb].elem);
            pop(I32);
            emit("$tfill($T" + tb + "," + argList(vs.length, 3) + ");");
            break;
          }
          default:
            fail("invalid numeric opcode: 0xfc" + sub.toString(16).padStart(2, "0"), at);
        }
        break;
      }
      case 0xfd:
        fail("Wasm SIMD unsupported", at);
      default:
        fail("invalid opcode " + opName(op), at);
    }
  }
}

// ---- Helpers the compiled code calls --------------------------------------------------

const scratch = new $DataView(new $ArrayBuffer(8));
function ctz32(a) {
  return a === 0 ? 32 : 31 - $Math.clz32(a & -a);
}
function pop32(a) {
  a = a - ((a >>> 1) & 0x55555555);
  a = (a & 0x33333333) + ((a >>> 2) & 0x33333333);
  return $Math.imul((a + (a >>> 4)) & 0x0f0f0f0f, 0x01010101) >>> 24;
}
function hi32(a) {
  return $Number(asIntN(32, a >> 32n));
}
function lo32(a) {
  return $Number(asIntN(32, a));
}
function checkTable(t, i, n) {
  if (i + n > t.a.length) trap("table index is out of bounds");
}
const H = {
  $trap: trap,
  $imul: $Math.imul,
  $clz: $Math.clz32,
  $ctz32: ctz32,
  $pop32: pop32,
  $divs32: (a, b) => {
    if (b === 0) trap("divide by zero");
    if (a === -2147483648 && b === -1) trap("divide result unrepresentable");
    return (a / b) | 0;
  },
  $divu32: (a, b) => {
    if (b === 0) trap("divide by zero");
    return ((a >>> 0) / (b >>> 0)) | 0;
  },
  $rems32: (a, b) => {
    if (b === 0) trap("remainder by zero");
    return (a % b) | 0;
  },
  $remu32: (a, b) => {
    if (b === 0) trap("remainder by zero");
    return ((a >>> 0) % (b >>> 0)) | 0;
  },
  $w: (a) => asIntN(64, a),
  $w32: (a) => asIntN(32, a),
  $sx8: (a) => asIntN(8, a),
  $sx16: (a) => asIntN(16, a),
  $u64: (a) => asUintN(64, a),
  $clz64: (a) => {
    const hi = hi32(a);
    return $BigInt(hi !== 0 ? $Math.clz32(hi) : 32 + $Math.clz32(lo32(a)));
  },
  $ctz64: (a) => {
    const lo = lo32(a);
    return $BigInt(lo !== 0 ? ctz32(lo) : 32 + ctz32(hi32(a)));
  },
  $pop64: (a) => $BigInt(pop32(hi32(a)) + pop32(lo32(a))),
  $divs64: (a, b) => {
    if (b === 0n) trap("divide by zero");
    if (a === -9223372036854775808n && b === -1n) trap("divide result unrepresentable");
    return a / b;
  },
  $divu64: (a, b) => {
    if (b === 0n) trap("divide by zero");
    return asIntN(64, asUintN(64, a) / asUintN(64, b));
  },
  $rems64: (a, b) => {
    if (b === 0n) trap("remainder by zero");
    return a % b;
  },
  $remu64: (a, b) => {
    if (b === 0n) trap("remainder by zero");
    return asIntN(64, asUintN(64, a) % asUintN(64, b));
  },
  $rotl64: (a, b) => {
    const k = b & 63n, u = asUintN(64, a);
    return asIntN(64, (u << k) | (u >> ((64n - k) & 63n)));
  },
  $rotr64: (a, b) => {
    const k = b & 63n, u = asUintN(64, a);
    return asIntN(64, (u >> k) | (u << ((64n - k) & 63n)));
  },
  $abs: $Math.abs,
  $ceil: $Math.ceil,
  $floor: $Math.floor,
  $trunc: $Math.trunc,
  $sqrt: $Math.sqrt,
  $nearest: (a) => {
    if (a !== a || a === 0 || a === 1 / 0 || a === -1 / 0) return a;
    const r = $Math.round(a);
    // Math.round rounds halves up; wasm rounds them to even.
    const res = r - a === 0.5 && r % 2 !== 0 ? r - 1 : r;
    return res === 0 && a < 0 ? -0 : res;
  },
  $f: fround,
  $min: (a, b) => (a !== a || b !== b ? 0 / 0 : $Math.min(a, b)),
  $max: (a, b) => (a !== a || b !== b ? 0 / 0 : $Math.max(a, b)),
  $cps: (a, b) => {
    scratch.setFloat64(0, b);
    const neg = (scratch.getUint8(0) & 0x80) !== 0;
    scratch.setFloat64(0, a);
    const aneg = (scratch.getUint8(0) & 0x80) !== 0;
    return neg === aneg ? a : -a;
  },
  $t32s: (a) => {
    if (!(a > -2147483649 && a < 2147483648)) trap("float unrepresentable in integer range");
    return $Math.trunc(a) | 0;
  },
  $t32u: (a) => {
    if (!(a > -1 && a < 4294967296)) trap("float unrepresentable in integer range");
    return $Math.trunc(a) | 0;
  },
  $t64s: (a) => {
    if (!(a >= -9223372036854775808 && a < 9223372036854775808)) trap("float unrepresentable in integer range");
    return $BigInt($Math.trunc(a));
  },
  $t64u: (a) => {
    if (!(a > -1 && a < 18446744073709551616)) trap("float unrepresentable in integer range");
    return asIntN(64, $BigInt($Math.trunc(a)));
  },
  $ts32s: (a) => (a !== a ? 0 : a <= -2147483648 ? -2147483648 : a >= 2147483647 ? 2147483647 : $Math.trunc(a) | 0),
  $ts32u: (a) => (a !== a || a <= 0 ? 0 : a >= 4294967295 ? -1 : $Math.trunc(a) | 0),
  $ts64s: (a) => (a !== a ? 0n : a <= -9223372036854775808 ? -9223372036854775808n : a >= 9223372036854775807 ? 9223372036854775807n : $BigInt($Math.trunc(a))),
  $ts64u: (a) => (a !== a || a <= 0 ? 0n : a >= 18446744073709551615 ? -1n : asIntN(64, $BigInt($Math.trunc(a)))),
  $cvf32: (a) => {
    // i64 -> f32 without double rounding: fold the bits below f64's mantissa into a sticky bit.
    const neg = a < 0n;
    let u = neg ? -a : a;
    if (u >= 1n << 53n) {
      let shift = 0n;
      while (u >> shift >= 1n << 53n) shift++;
      const low = u & ((1n << shift) - 1n);
      u = ((u >> shift) | (low !== 0n ? 1n : 0n)) << shift;
    }
    const v = fround($Number(u));
    return neg ? -v : v;
  },
  $B: $BigInt,
  $N: $Number,
  $ri32: (a) => {
    scratch.setFloat32(0, a, true);
    return scratch.getInt32(0, true);
  },
  $ri64: (a) => {
    scratch.setFloat64(0, a, true);
    return scratch.getBigInt64(0, true);
  },
  $rf32: (a) => {
    scratch.setInt32(0, a, true);
    return scratch.getFloat32(0, true);
  },
  $rf64: (a) => {
    scratch.setBigInt64(0, a, true);
    return scratch.getFloat64(0, true);
  },
  $tget: (t, i) => {
    checkTable(t, i >>> 0, 1);
    return t.a[i >>> 0];
  },
  $tset: (t, i, v) => {
    checkTable(t, i >>> 0, 1);
    t.a[i >>> 0] = v;
  },
  $tgrow: (t, v, n) => tableGrow(t, n >>> 0, v),
  $tfill: (t, i, v, n) => {
    i >>>= 0;
    n >>>= 0;
    checkTable(t, i, n);
    for (let k = 0; k < n; k++) t.a[i + k] = v;
  },
  $tcopy: (dt, st, di, si, n) => {
    di >>>= 0;
    si >>>= 0;
    n >>>= 0;
    checkTable(dt, di, n);
    checkTable(st, si, n);
    if (di <= si) for (let k = 0; k < n; k++) dt.a[di + k] = st.a[si + k];
    else for (let k = n - 1; k >= 0; k--) dt.a[di + k] = st.a[si + k];
  },
  $ci: (t, i, sig) => {
    checkTable(t, i >>> 0, 1);
    const fn = t.a[i >>> 0];
    if (fn === null) trap("null function or function signature mismatch");
    const info = funcInfo.get(fn);
    if (info.sig !== sig) trap("null function or function signature mismatch");
    return info.fn;
  },
};
const HELPER_NAMES = $Object.keys(H);

// ---- Runtime state ------------------------------------------------------------------

// Exported functions: JS function -> { fn: the raw function, sig, type }.
const funcInfo = new $WeakMap();

function makeMemoryCell(min, max, api) {
  let buf;
  try {
    buf = new $ArrayBuffer(min * PAGE);
  } catch (e) {
    throw new $RangeError(api + "could not allocate memory");
  }
  return { buf, dv: new $DataView(buf), u8: new $Uint8Array(buf), len: min * PAGE, max, listeners: [], obj: null };
}
function memoryGrow(m, delta) {
  const old = m.len / PAGE;
  const limit = m.max === null ? MAX_PAGES : m.max;
  if (delta > limit - old) return -1;
  const newLen = (old + delta) * PAGE;
  let nb;
  try {
    if (abTransfer !== undefined) nb = apply(abTransfer, m.buf, [newLen]);
    else {
      nb = new $ArrayBuffer(newLen);
      new $Uint8Array(nb).set(m.u8);
    }
  } catch (e) {
    return -1;
  }
  m.buf = nb;
  m.len = newLen;
  m.dv = new $DataView(nb);
  m.u8 = new $Uint8Array(nb);
  for (const l of m.listeners) l();
  return old;
}
function tableGrow(t, n, v) {
  const old = t.a.length;
  const limit = t.max === null ? MAX_TABLE : t.max;
  if (n > limit - old) return -1;
  for (let k = 0; k < n; k++) t.a.push(v);
  return old;
}
function defaultValue(t) {
  return t === I64 ? 0n : t === FUNCREF || t === EXTERNREF ? null : 0;
}
/** ToWebAssemblyValue ( v, type ). Raw values are JS values: i64 is a BigInt. */
function toWasm(v, t) {
  switch (t) {
    case I32: return v | 0;
    case I64: return asIntN(64, v);
    case F32: return fround(+v);
    case F64: return +v;
    case EXTERNREF: return v;
    default:
      if (v === null || (typeof v === "function" && funcInfo.has(v))) return v;
      throw new $TypeError("type incompatibility when transforming from/to JS");
  }
}

/** The exported function for raw function raw of type type, named index. */
function exportFunction(raw, type, index) {
  const ps = type.params;
  const n = ps.length;
  const multi = type.results.length > 1;
  const w = {
    [""](...args) {
      const a = new $Array(n);
      for (let k = 0; k < n; k++) a[k] = toWasm(args[k], ps[k]);
      const r = apply(raw, undefined, a);
      return multi ? r.slice() : r;
    },
  }[""];
  defineProperty(w, "length", { value: n });
  defineProperty(w, "name", { value: $String(index) });
  funcInfo.set(w, { fn: raw, sig: type.sig, type });
  return w;
}
/** The raw function calling host function fn as type type. */
function importFunction(fn, type) {
  const rs = type.results;
  if (rs.length === 0) {
    return (...args) => {
      apply(fn, undefined, args);
    };
  }
  if (rs.length === 1) {
    const t = rs[0];
    return (...args) => toWasm(apply(fn, undefined, args), t);
  }
  return (...args) => {
    const v = apply(fn, undefined, args);
    if (v === null || (typeof v !== "object" && typeof v !== "function")) throw new $TypeError("multi-return value must be iterable");
    const list = [...v];
    if (list.length !== rs.length) throw new $TypeError("multi-return length mismatch");
    for (let k = 0; k < rs.length; k++) list[k] = toWasm(list[k], rs[k]);
    return list;
  };
}

// ---- Compilation and instantiation --------------------------------------------------

// The API whose name prefixes compile errors, as V8 does.
let apiName = "WebAssembly.Module(): ";
function compileModule(bytes, api) {
  apiName = api;
  const M = decode(bytes);
  let src = "\"use strict\";const{" + HELPER_NAMES.join(",") + "}=$rt.h;";
  src += "let $dv=null,$ml=0;const $mem=$rt.mem;";
  src += "if($mem!==null){const $refresh=()=>{$dv=$mem.dv;$ml=$mem.len;};$mem.listeners.push($refresh);$refresh();}";
  src += "function $ea(a,o,n){const e=(a>>>0)+o;if(e+n>$ml)$trap(\"memory access out of bounds\");return e;}";
  src += "const{$grow,$minit,$ddrop,$mcopy,$mfill,$tinit,$edrop,$rf}=$rt;";
  for (let k = 0; k < M.globals.length; k++) src += "const $g" + k + "=$rt.g[" + k + "];";
  for (let k = 0; k < M.tables.length; k++) src += "const $T" + k + "=$rt.t[" + k + "];";
  for (let k = 0; k < M.nImportFuncs; k++) src += "const f" + k + "=$rt.f[" + k + "];";
  for (let k = M.nImportFuncs; k < M.funcs.length; k++) src += compileFunction(M, bytes, k);
  src += "return[";
  for (let k = 0; k < M.funcs.length; k++) src += (k ? "," : "") + "f" + k;
  src += "];";
  return { M, src, factory: null };
}
function factoryOf(rec) {
  if (rec.factory === null) {
    rec.factory = new $Function("$rt", rec.src);
    rec.src = null;
  }
  return rec.factory;
}

function evalConst(code, globals, refFunc) {
  const st = [];
  for (const [op, imm] of code) {
    switch (op) {
      case 0x41: case 0x42: case 0x43: case 0x44: st.push(imm); break;
      case 0xd0: st.push(null); break;
      case 0xd2: st.push(refFunc(imm)); break;
      case 0x23: st.push(globals[imm].v); break;
      case 0x6a: { const b = st.pop(), a = st.pop(); st.push((a + b) | 0); break; }
      case 0x6b: { const b = st.pop(), a = st.pop(); st.push((a - b) | 0); break; }
      case 0x6c: { const b = st.pop(), a = st.pop(); st.push($Math.imul(a, b)); break; }
      case 0x7c: { const b = st.pop(), a = st.pop(); st.push(asIntN(64, a + b)); break; }
      case 0x7d: { const b = st.pop(), a = st.pop(); st.push(asIntN(64, a - b)); break; }
      case 0x7e: { const b = st.pop(), a = st.pop(); st.push(asIntN(64, a * b)); break; }
    }
  }
  return st[0];
}

const emptyBytes = new $Uint8Array(0);

function instantiateModule(rec, importObject, api) {
  const M = rec.M;
  if (M.imports.length > 0 && !isObject(importObject)) throw new $TypeError(api + "Imports argument must be present and must be an object");
  const rawFuncs = new $Array(M.funcs.length);
  const wrappers = new $Array(M.funcs.length);
  const tables = [], globals = [];
  let mem = null;
  for (let k = 0; k < M.imports.length; k++) {
    const imp = M.imports[k];
    const where = "Import #" + k + " module=\"" + imp.module + "\"";
    const ns = importObject[imp.module];
    if (!isObject(ns)) throw new $TypeError(api + where + ": module is not an object or function");
    const v = ns[imp.name];
    const what = api + where + " function=\"" + imp.name + "\": ";
    if (imp.kind === 0) {
      const type = M.types[M.funcs[imp.index]];
      if (typeof v !== "function") throw new LinkError(what + "function import requires a callable");
      const info = funcInfo.get(v);
      if (info !== undefined) {
        if (info.sig !== type.sig) throw new LinkError(what + "imported function does not match the expected type");
        rawFuncs[imp.index] = info.fn;
        wrappers[imp.index] = v;
      } else {
        rawFuncs[imp.index] = importFunction(v, type);
      }
    } else if (imp.kind === 1) {
      const want = M.tables[imp.index];
      const t = isObject(v) ? tableCells.get(v) : undefined;
      if (t === undefined) throw new LinkError(what + "table import requires a WebAssembly.Table");
      if (t.a.length < want.min) throw new LinkError(what + "table import has " + t.a.length + " elements, need at least " + want.min);
      if (want.max !== null) {
        if (t.max === null) throw new LinkError(what + "table import has no maximum length, expected " + want.max);
        if (t.max > want.max) throw new LinkError(what + "table import has a larger maximum size " + t.max + " than the module's declared maximum " + want.max);
      }
      if (t.elem !== want.elem) throw new LinkError(what + "imported table does not match the expected type");
      tables.push(t);
    } else if (imp.kind === 2) {
      const want = M.mems[imp.index];
      const m = isObject(v) ? memoryCells.get(v) : undefined;
      if (m === undefined) throw new LinkError(what + "memory import must be a WebAssembly.Memory object");
      const pages = m.len / PAGE;
      if (pages < want.min) throw new LinkError(what + "memory import has " + pages + " pages which is smaller than the declared initial of " + want.min);
      if (want.max !== null) {
        if (m.max === null) throw new LinkError(what + "memory import has no maximum limit, expected at most " + want.max);
        if (m.max > want.max) throw new LinkError(what + "memory import has a larger maximum size " + m.max + " than the module's declared maximum " + want.max);
      }
      mem = m;
    } else {
      const want = M.globals[imp.index];
      const g = isObject(v) ? globalCells.get(v) : undefined;
      if (g !== undefined) {
        if (g.mut !== want.mut) throw new LinkError(what + "imported global does not match the expected mutability");
        if (g.type !== want.type) throw new LinkError(what + "imported global does not match the expected type");
        globals.push(g);
      } else {
        if (want.mut) throw new LinkError(what + "imported mutable global must be a WebAssembly.Global object");
        let val;
        if (want.type === I64) {
          if (typeof v !== "bigint") throw new LinkError(what + "global import must be a BigInt value");
          val = asIntN(64, v);
        } else if (want.type === EXTERNREF) {
          val = v;
        } else if (want.type === FUNCREF) {
          if (!(v === null || (typeof v === "function" && funcInfo.has(v)))) throw new LinkError(what + "imported funcref global must be null or a WebAssembly function");
          val = v;
        } else {
          if (typeof v !== "number") throw new LinkError(what + "global import must be a number, valid Wasm reference, or WebAssembly.Global object");
          val = toWasm(v, want.type);
        }
        globals.push({ type: want.type, mut: false, v: val, obj: null });
      }
    }
  }
  const refFunc = (fi) => {
    let w = wrappers[fi];
    if (w === undefined) {
      w = exportFunction(rawFuncs[fi], M.types[M.funcs[fi]], fi);
      wrappers[fi] = w;
    }
    return w;
  };
  for (let k = M.nImportTables; k < M.tables.length; k++) {
    const t = M.tables[k];
    tables.push({ a: new $Array(t.min).fill(null), elem: t.elem, max: t.max, obj: null });
  }
  for (let k = M.nImportMems; k < M.mems.length; k++) mem = makeMemoryCell(M.mems[k].min, M.mems[k].max, api);
  const factory = factoryOf(rec);
  const datas = M.datas.map((s) => s.bytes);
  let elems = null;
  const memOob = () => trap("memory access out of bounds");
  const rt = {
    h: H, mem, g: globals, t: tables, f: rawFuncs,
    $grow: (n) => memoryGrow(mem, n >>> 0),
    $minit: (seg, dst, src, n) => {
      const b = datas[seg];
      dst >>>= 0; src >>>= 0; n >>>= 0;
      if (src + n > b.length || dst + n > mem.len) memOob();
      mem.u8.set(b.subarray(src, src + n), dst);
    },
    $ddrop: (seg) => {
      datas[seg] = emptyBytes;
    },
    $mcopy: (dst, src, n) => {
      dst >>>= 0; src >>>= 0; n >>>= 0;
      if (src + n > mem.len || dst + n > mem.len) memOob();
      mem.u8.copyWithin(dst, src, src + n);
    },
    $mfill: (dst, v, n) => {
      dst >>>= 0; n >>>= 0;
      if (dst + n > mem.len) memOob();
      mem.u8.fill(v & 255, dst, dst + n);
    },
    $tinit: (t, seg, dst, src, n) => {
      const e = elems[seg];
      dst >>>= 0; src >>>= 0; n >>>= 0;
      if (src + n > e.length || dst + n > t.a.length) trap("table index is out of bounds");
      for (let k = 0; k < n; k++) t.a[dst + k] = e[src + k];
    },
    $edrop: (seg) => {
      elems[seg] = [];
    },
    $rf: refFunc,
  };
  // The raw functions exist before globals and segments are evaluated (ref.func).
  for (let k = M.nImportGlobals; k < M.globals.length; k++) globals.push({ type: M.globals[k].type, mut: M.globals[k].mut, v: undefined, obj: null });
  const made = factory(rt);
  for (let k = M.nImportFuncs; k < made.length; k++) rawFuncs[k] = made[k];
  for (let k = M.nImportGlobals; k < M.globals.length; k++) globals[k].v = evalConst(M.globals[k].init, globals, refFunc);
  elems = M.elems.map((s) => s.inits.map((e) => evalConst(e, globals, refFunc)));
  const exports = $Object.create(null);
  for (const e of M.exports) {
    let v;
    if (e.kind === 0) v = refFunc(e.index);
    else if (e.kind === 1) v = tableObject(tables[e.index]);
    else if (e.kind === 2) v = memoryObject(mem);
    else v = globalObject(globals[e.index]);
    exports[e.name] = v;
  }
  freeze(exports);
  for (let k = 0; k < M.elems.length; k++) {
    const s = M.elems[k];
    if (s.mode === "active") {
      const off = evalConst(s.offset, globals, refFunc) >>> 0;
      const t = tables[s.table];
      const e = elems[k];
      if (off + e.length > t.a.length) trap(api + "table index is out of bounds");
      for (let j = 0; j < e.length; j++) t.a[off + j] = e[j];
      elems[k] = [];
    } else if (s.mode === "declarative") elems[k] = [];
  }
  for (let k = 0; k < M.datas.length; k++) {
    const s = M.datas[k];
    if (s.mode !== "active") continue;
    const off = evalConst(s.offset, globals, refFunc) >>> 0;
    if (off + s.bytes.length > mem.len) trap(api + "data segment " + k + " is out of bounds (offset " + off + ", length " + s.bytes.length + ", memory size " + mem.len + ")");
    mem.u8.set(s.bytes, off);
    datas[k] = emptyBytes;
  }
  if (M.start !== null) rawFuncs[M.start]();
  return exports;
}

// ---- The JS API ---------------------------------------------------------------------

const memoryCells = new $WeakMap();
const tableCells = new $WeakMap();
const globalCells = new $WeakMap();
const moduleRecords = new $WeakMap();
const instanceExports = new $WeakMap();

function isObject(v) {
  return v !== null && (typeof v === "object" || typeof v === "function");
}
function requireNew(nt, name) {
  if (nt === undefined) throw new $TypeError("WebAssembly." + name + " must be invoked with 'new'");
}
function brand(map, v, what) {
  const c = isObject(v) ? map.get(v) : undefined;
  if (c === undefined) throw new $TypeError("Receiver is not a " + what);
  return c;
}
function toU32(v, what) {
  const n = +v;
  if (n !== n || n === 1 / 0 || n === -1 / 0) throw new $TypeError(what + " must be convertible to a valid number");
  const t = $Math.trunc(n);
  if (t < 0 || t > 4294967295) throw new $TypeError(what + " must be in the unsigned long range");
  return t;
}
function descIndex(desc, name, api, max, required) {
  const v = desc[name];
  if (v === undefined) {
    if (required) throw new $TypeError(api + "Property '" + name + "' is required");
    return null;
  }
  const t = toU32(v, api + "Property '" + name + "'");
  if (t > max) throw new $RangeError(api + "Property '" + name + "': value " + t + " is above the upper bound " + max);
  return t;
}
function parseValType(s, api) {
  switch ($String(s)) {
    case "i32": return I32;
    case "i64": return I64;
    case "f32": return F32;
    case "f64": return F64;
    case "anyfunc": case "funcref": return FUNCREF;
    case "externref": return EXTERNREF;
  }
  throw new $TypeError(api + "Descriptor property 'value' must be a WebAssembly type");
}
function moduleRecord(m, api) {
  const rec = isObject(m) ? moduleRecords.get(m) : undefined;
  if (rec === undefined) throw new $TypeError(api + ": Argument 0 must be a WebAssembly.Module");
  return rec;
}

class Module {
  constructor(bytes) {
    requireNew(new.target, "Module");
    moduleRecords.set(this, compileModule(copyBytes(bytes, "WebAssembly.Module(): "), "WebAssembly.Module(): "));
  }
  static exports(m) {
    return moduleRecord(m, "WebAssembly.Module.exports()").M.exports.map((e) => ({ name: e.name, kind: kindName(e.kind) }));
  }
  static imports(m) {
    return moduleRecord(m, "WebAssembly.Module.imports()").M.imports.map((e) => ({ module: e.module, name: e.name, kind: kindName(e.kind) }));
  }
  static customSections(m, name) {
    const rec = moduleRecord(m, "WebAssembly.Module.customSections()");
    if (name === undefined) throw new $TypeError("WebAssembly.Module.customSections(): Argument 1 is required");
    const want = $String(name);
    const out = [];
    for (const c of rec.M.customs) if (c.name === want) out.push(c.bytes.slice().buffer);
    return out;
  }
}

class Instance {
  constructor(module, importObject) {
    requireNew(new.target, "Instance");
    const rec = moduleRecord(module, "WebAssembly.Instance()");
    instanceExports.set(this, instantiateModule(rec, importObject, "WebAssembly.Instance(): "));
  }
  get exports() {
    return brand(instanceExports, this, "WebAssembly.Instance");
  }
}

class Memory {
  constructor(desc) {
    requireNew(new.target, "Memory");
    const api = "WebAssembly.Memory(): ";
    if (!isObject(desc)) throw new $TypeError(api + "Argument 0 must be a memory descriptor");
    const initial = descIndex(desc, "initial", api, MAX_PAGES, true);
    const maximum = descIndex(desc, "maximum", api, MAX_PAGES, false);
    if (maximum !== null && maximum < initial) throw new $RangeError(api + "Property 'maximum': value " + maximum + " is below the lower bound " + initial);
    const m = makeMemoryCell(initial, maximum, api);
    m.obj = this;
    memoryCells.set(this, m);
  }
  grow(delta) {
    const m = brand(memoryCells, this, "WebAssembly.Memory");
    const r = memoryGrow(m, toU32(delta, "WebAssembly.Memory.grow(): Argument 0"));
    if (r < 0) throw new $RangeError("WebAssembly.Memory.grow(): Maximum memory size exceeded");
    return r;
  }
  get buffer() {
    return brand(memoryCells, this, "WebAssembly.Memory").buf;
  }
}
function memoryObject(m) {
  if (m.obj === null) {
    const o = $Object.create(Memory.prototype);
    m.obj = o;
    memoryCells.set(o, m);
  }
  return m.obj;
}

function tableName(t) {
  return t.elem === FUNCREF ? "funcref" : "externref";
}
class Table {
  constructor(desc, value) {
    requireNew(new.target, "Table");
    const api = "WebAssembly.Table(): ";
    if (!isObject(desc)) throw new $TypeError(api + "Argument 0 must be a table descriptor");
    const es = $String(desc.element);
    let elem;
    if (es === "anyfunc" || es === "funcref") elem = FUNCREF;
    else if (es === "externref") elem = EXTERNREF;
    else throw new $TypeError(api + "Descriptor property 'element' must be a WebAssembly reference type");
    const initial = descIndex(desc, "initial", api, MAX_TABLE, true);
    const maximum = descIndex(desc, "maximum", api, 4294967295, false);
    if (maximum !== null && maximum < initial) throw new $RangeError(api + "Property 'maximum': value " + maximum + " is below the lower bound " + initial);
    const init = arguments.length < 2 ? (elem === EXTERNREF ? undefined : null) : value === undefined && elem === FUNCREF ? null : toWasm(value, elem);
    tableCells.set(this, { a: new $Array(initial).fill(init), elem, max: maximum, obj: this });
  }
  get length() {
    return brand(tableCells, this, "WebAssembly.Table").a.length;
  }
  grow(delta, value) {
    const t = brand(tableCells, this, "WebAssembly.Table");
    const n = toU32(delta, "WebAssembly.Table.grow(): Argument 0");
    const init = arguments.length < 2 ? (t.elem === EXTERNREF ? undefined : null) : toWasm(value, t.elem);
    const r = tableGrow(t, n, init);
    if (r < 0) throw new $RangeError("WebAssembly.Table.grow(): failed to grow table by " + n);
    return r;
  }
  get(index) {
    const t = brand(tableCells, this, "WebAssembly.Table");
    const i = toU32(index, "WebAssembly.Table.get(): Argument 0");
    if (i >= t.a.length) throw new $RangeError("WebAssembly.Table.get(): invalid address " + i + " in " + tableName(t) + " table of size " + t.a.length);
    return t.a[i];
  }
  set(index, value) {
    const t = brand(tableCells, this, "WebAssembly.Table");
    const i = toU32(index, "WebAssembly.Table.set(): Argument 0");
    if (i >= t.a.length) throw new $RangeError("WebAssembly.Table.set(): invalid address " + i + " in " + tableName(t) + " table of size " + t.a.length);
    t.a[i] = arguments.length < 2 ? (t.elem === EXTERNREF ? undefined : null) : toWasm(value, t.elem);
  }
}
function tableObject(t) {
  if (t.obj === null) {
    const o = $Object.create(Table.prototype);
    t.obj = o;
    tableCells.set(o, t);
  }
  return t.obj;
}

class Global {
  constructor(desc, value) {
    requireNew(new.target, "Global");
    const api = "WebAssembly.Global(): ";
    if (!isObject(desc)) throw new $TypeError(api + "Argument 0 must be a global descriptor");
    const mut = !!desc.mutable;
    const type = parseValType(desc.value, api);
    const v = value === undefined ? (type === EXTERNREF && arguments.length >= 2 ? undefined : defaultValue(type)) : toWasm(value, type);
    globalCells.set(this, { type, mut, v, obj: this });
  }
  valueOf() {
    return brand(globalCells, this, "WebAssembly.Global").v;
  }
  get value() {
    return brand(globalCells, this, "WebAssembly.Global").v;
  }
  set value(v) {
    const g = brand(globalCells, this, "WebAssembly.Global");
    if (!g.mut) throw new $TypeError("WebAssembly.Global.value: Can't set the value of an immutable global.");
    g.v = toWasm(v, g.type);
  }
}
function globalObject(g) {
  if (g.obj === null) {
    const o = $Object.create(Global.prototype);
    g.obj = o;
    globalCells.set(o, g);
  }
  return g.obj;
}

{
  const classes = [Module, "Module", Instance, "Instance", Memory, "Memory", Table, "Table", Global, "Global"];
  for (let k = 0; k < classes.length; k += 2) {
    defineProperty(classes[k].prototype, $Symbol.toStringTag, { value: "WebAssembly." + classes[k + 1], configurable: true });
    defineProperty(classes[k], "length", { value: 1 });
  }
}

function newModule(rec) {
  const m = $Object.create(Module.prototype);
  moduleRecords.set(m, rec);
  return m;
}
function newInstance(rec, importObject, api) {
  const inst = $Object.create(Instance.prototype);
  instanceExports.set(inst, instantiateModule(rec, importObject, api));
  return inst;
}
function checkImportsArg(importObject, api) {
  if (importObject !== undefined && !isObject(importObject)) throw new $TypeError(api + "Argument 1 must be an object");
}

function validate(bytes) {
  const b = copyBytes(bytes, "WebAssembly.validate(): ");
  apiName = "WebAssembly.validate(): ";
  try {
    const M = decode(b);
    for (let k = M.nImportFuncs; k < M.funcs.length; k++) compileFunction(M, b, k);
    return true;
  } catch (e) {
    if (e instanceof CompileError) return false;
    throw e;
  }
}
function compile(bytes) {
  return new $Promise((resolve) => {
    resolve(newModule(compileModule(copyBytes(bytes, "WebAssembly.compile(): "), "WebAssembly.compile(): ")));
  });
}
function instantiate(source, importObject) {
  const api = "WebAssembly.instantiate(): ";
  return new $Promise((resolve) => {
    const rec = isObject(source) ? moduleRecords.get(source) : undefined;
    if (rec !== undefined) {
      checkImportsArg(importObject, api);
      resolve($Promise.resolve().then(() => newInstance(rec, importObject, api)));
      return;
    }
    const b = copyBytes(source, api);
    checkImportsArg(importObject, api);
    const r = compileModule(b, api);
    const module = newModule(r);
    resolve($Promise.resolve().then(() => ({ module, instance: newInstance(r, importObject, api) })));
  });
}
async function responseBytes(source, api) {
  const resp = await source;
  if (!isObject(resp) || typeof resp.arrayBuffer !== "function") throw new $TypeError(api + "Argument 0 must be provided and must be a Response");
  const headers = resp.headers;
  if (isObject(headers) && typeof headers.get === "function") {
    const ct = headers.get("Content-Type");
    if (ct === null || ct === undefined || $String(ct).toLowerCase().split(";")[0].trim() !== "application/wasm")
      throw new $TypeError(api + "Incorrect response MIME type. Expected 'application/wasm'.");
  }
  if (resp.ok === false) throw new $TypeError(api + "HTTP status code is not ok");
  return new $Uint8Array(await resp.arrayBuffer());
}
async function compileStreaming(source) {
  return newModule(compileModule(await responseBytes(source, "WebAssembly.compileStreaming(): "), "WebAssembly.compileStreaming(): "));
}
async function instantiateStreaming(source, importObject) {
  const api = "WebAssembly.instantiateStreaming(): ";
  const b = await responseBytes(source, api);
  checkImportsArg(importObject, api);
  const r = compileModule(b, api);
  const module = newModule(r);
  await undefined;
  return { module, instance: newInstance(r, importObject, api) };
}

{
  const fns = [compile, validate, instantiate, compileStreaming, instantiateStreaming];
  for (let k = 0; k < fns.length; k++) {
    defineProperty(fns[k], "length", { value: 1 });
    defineProperty(WebAssembly, fns[k].name, { value: fns[k], writable: true, enumerable: true, configurable: true });
  }
  const ctors = [Module, Instance, Table, Memory, Global, CompileError, LinkError, RuntimeError];
  for (let k = 0; k < ctors.length; k++) defineProperty(WebAssembly, ctors[k].name, { value: ctors[k], writable: true, configurable: true });
  defineProperty(WebAssembly, $Symbol.toStringTag, { value: "WebAssembly", configurable: true });
}
return WebAssembly;

