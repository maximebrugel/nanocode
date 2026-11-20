// Atomics (ECMA-262 25.4) for one agent: the read-modify-write operations are the
// %AtomicOp leaf over the element's raw bits. No other agent can notify a waiter, so
// Atomics.wait only answers "not-equal" or "timed-out", and Atomics.waitAsync's waiters
// (a list in their SharedArrayBuffer's slot 2) settle by Atomics.notify or their timeout.
// @slim-omit: left out of the slim build (D2411).

/**
 * The Atomics object, 25.4: an ordinary object whose Symbol.toStringTag is "Atomics".
 * @value globalThis.Atomics
 * @attributes writable configurable
 */
function AtomicsObject(): Obj {
  return ObjectCreate(RealmIntrinsic("ObjectPrototype"));
}

/**
 * Atomics [ %Symbol.toStringTag% ], 25.4.17.
 * @value Atomics[Symbol.toStringTag]
 * @attributes configurable
 */
function AtomicsToStringTag(): Str {
  return "Atomics";
}

/**
 * ValidateIntegerTypedArray ( typedArray, waitable ), 25.4.3.1: Int32Array or
 * BigInt64Array when waitable, else any integer kind but Uint8Clamped.
 */
function ValidateIntegerTypedArray(v: Value, waitable: boolean): Obj {
  const o: Obj = ValidateTypedArray(v);
  const kind: i32 = KindOf(o);
  if (waitable) {
    if (kind !== 5 && kind !== 10) {
      throw NewTypeError("Atomics operation needs an Int32Array or BigInt64Array");
    }
  } else if (kind === 2 || kind === 7 || kind === 8 || kind === 9) {
    throw NewTypeError("Atomics operation needs an integer typed array");
  }
  return o;
}

/** ValidateAtomicAccess ( taRecord, requestIndex ), 25.4.3.2: the element index. */
function ValidateAtomicAccess(o: Obj, requestIndex: Value): f64 {
  const i: f64 = ToIndex(requestIndex);
  if (i >= TALength(o)) {
    throw NewRangeError("Invalid atomic access index");
  }
  return i;
}

/** The operand of an atomic operation on o: ToBigInt or ToIntegerOrInfinity. */
function AtomicOperand(o: Obj, v: Value): Value {
  if (KindOf(o) >= 10) {
    return ToBigInt(v);
  }
  return ToIntegerOrInfinity(v);
}

/** RevalidateAtomicAccess ( typedArray, byteIndexInBuffer ), 25.4.3.4. */
function RevalidateAtomicAccess(o: Obj): void {
  if (TAOutOfBounds(o)) {
    throw NewTypeError("ArrayBuffer is detached or resized");
  }
}

/** AtomicReadModifyWrite ( typedArray, index, value, op ), 25.4.3.17. */
function AtomicReadModifyWrite(ta: Value, index: Value, value: Value, op: i32): Value {
  const o: Obj = ValidateIntegerTypedArray(ta, false);
  const i: f64 = ValidateAtomicAccess(o, index);
  const v: Value = AtomicOperand(o, value);
  RevalidateAtomicAccess(o);
  return AtomicOp(o, i, op, v, undefined);
}

/**
 * Atomics.add ( typedArray, index, value ), 25.4.4.
 * @builtin Atomics.add
 */
function AtomicsAdd(ta: Value, index: Value, value: Value): Value {
  return AtomicReadModifyWrite(ta, index, value, 0);
}

/**
 * Atomics.and ( typedArray, index, value ), 25.4.5.
 * @builtin Atomics.and
 */
function AtomicsAnd(ta: Value, index: Value, value: Value): Value {
  return AtomicReadModifyWrite(ta, index, value, 1);
}

/**
 * Atomics.compareExchange ( typedArray, index, expectedValue, replacementValue ), 25.4.6.
 * @builtin Atomics.compareExchange
 */
function AtomicsCompareExchange(ta: Value, index: Value, expected: Value, replacement: Value): Value {
  const o: Obj = ValidateIntegerTypedArray(ta, false);
  const i: f64 = ValidateAtomicAccess(o, index);
  const e: Value = AtomicOperand(o, expected);
  const r: Value = AtomicOperand(o, replacement);
  RevalidateAtomicAccess(o);
  return AtomicOp(o, i, 6, e, r);
}

/**
 * Atomics.exchange ( typedArray, index, value ), 25.4.7.
 * @builtin Atomics.exchange
 */
function AtomicsExchange(ta: Value, index: Value, value: Value): Value {
  return AtomicReadModifyWrite(ta, index, value, 2);
}

/**
 * Atomics.isLockFree ( size ), 25.4.8.
 * @builtin Atomics.isLockFree
 */
function AtomicsIsLockFree(size: Value): boolean {
  const n: f64 = ToIntegerOrInfinity(size);
  return n === 1 || n === 2 || n === 4 || n === 8;
}

/**
 * Atomics.load ( typedArray, index ), 25.4.9.
 * @builtin Atomics.load
 */
function AtomicsLoad(ta: Value, index: Value): Value {
  const o: Obj = ValidateIntegerTypedArray(ta, false);
  const i: f64 = ValidateAtomicAccess(o, index);
  RevalidateAtomicAccess(o);
  return AtomicOp(o, i, 7, undefined, undefined);
}

/**
 * Atomics.or ( typedArray, index, value ), 25.4.10.
 * @builtin Atomics.or
 */
function AtomicsOr(ta: Value, index: Value, value: Value): Value {
  return AtomicReadModifyWrite(ta, index, value, 3);
}

/**
 * Atomics.store ( typedArray, index, value ), 25.4.11: the stored value (-0 as +0).
 * @builtin Atomics.store
 */
function AtomicsStore(ta: Value, index: Value, value: Value): Value {
  const o: Obj = ValidateIntegerTypedArray(ta, false);
  const i: f64 = ValidateAtomicAccess(o, index);
  let v: Value = AtomicOperand(o, value);
  if (KindOf(o) < 10) {
    v = AsNumber(v) + 0;
  }
  RevalidateAtomicAccess(o);
  AtomicOp(o, i, 2, v, undefined);
  return v;
}

/**
 * Atomics.sub ( typedArray, index, value ), 25.4.12.
 * @builtin Atomics.sub
 */
function AtomicsSub(ta: Value, index: Value, value: Value): Value {
  return AtomicReadModifyWrite(ta, index, value, 4);
}

/**
 * Atomics.xor ( typedArray, index, value ), 25.4.16.
 * @builtin Atomics.xor
 */
function AtomicsXor(ta: Value, index: Value, value: Value): Value {
  return AtomicReadModifyWrite(ta, index, value, 5);
}

/**
 * DoWait ( mode, typedArray, index, value, timeout ), 25.4.3.14, steps 1-13: a Record
 * [result, waiter list, index, timeout] where result is "not-equal", "timed-out", or
 * undefined when the agent would have to wait.
 */
function DoWaitCheck(ta: Value, index: Value, value: Value, timeout: Value): Obj {
  const o: Obj = ValidateIntegerTypedArray(ta, true);
  const buffer: Obj = AsObj(TypedArrayField(o, 1));
  if (!AsBool(BufferInfo(buffer, 5))) {
    throw NewTypeError("Atomics.wait needs a shared typed array");
  }
  const i: f64 = ValidateAtomicAccess(o, index);
  let v: Value = undefined;
  if (KindOf(o) === 10) {
    v = ToBigInt(value);
  } else {
    v = ToIntegerOrInfinity(value);
  }
  const q: f64 = ToNumber(timeout);
  let t: f64 = q;
  if (q !== q) {
    t = 1 / 0;
  } else if (q < 0) {
    t = 0;
  }
  const r: Obj = NewRecord(4);
  RecordSet(r, 1, BufferInfo(buffer, 6));
  RecordSet(r, 2, i);
  RecordSet(r, 3, t);
  RevalidateAtomicAccess(o);
  if (!AsBool(AtomicOp(o, i, 8, v, undefined))) {
    RecordSet(r, 0, "not-equal");
  } else if (t === 0) {
    RecordSet(r, 0, "timed-out");
  } else {
    RecordSet(r, 0, undefined);
  }
  return r;
}

/**
 * Atomics.wait ( typedArray, index, value, timeout ), 25.4.13. No other agent can
 * notify this one, so a finite wait times out at once, and an infinite one, which
 * could never end, is a TypeError (as when the agent cannot suspend).
 * @builtin Atomics.wait
 */
function AtomicsWait(ta: Value, index: Value, value: Value, timeout: Value): Str {
  const r: Obj = DoWaitCheck(ta, index, value, timeout);
  const result: Value = RecordGet(r, 0);
  if (result !== undefined) {
    return AsStr(result);
  }
  if (AsNumber(RecordGet(r, 3)) === 1 / 0) {
    throw NewTypeError("Atomics.wait cannot be called in this context");
  }
  return "timed-out";
}

/** A result object { async, value } of Atomics.waitAsync. */
function WaitAsyncResult(isAsync: boolean, value: Value): Obj {
  const o: Obj = ObjectCreate(RealmIntrinsic("ObjectPrototype"));
  CreateDataPropertyOrThrow(o, "async", isAsync);
  CreateDataPropertyOrThrow(o, "value", value);
  return o;
}

/**
 * Atomics.waitAsync ( typedArray, index, value, timeout ), 25.4.14. A waiter is a
 * Record [index, promise, next, settled] on its buffer's list; a finite timeout uses
 * the host's setTimeout when it has one.
 * @builtin Atomics.waitAsync
 */
function AtomicsWaitAsync(ta: Value, index: Value, value: Value, timeout: Value): Obj {
  const r: Obj = DoWaitCheck(ta, index, value, timeout);
  const result: Value = RecordGet(r, 0);
  if (result !== undefined) {
    return WaitAsyncResult(false, result);
  }
  const list: Obj = AsObj(RecordGet(r, 1));
  const promise: Obj = PromiseCreate(RealmIntrinsic("PromisePrototype"));
  const waiter: Obj = NewRecord(4);
  RecordSet(waiter, 0, RecordGet(r, 2));
  RecordSet(waiter, 1, promise);
  RecordSet(waiter, 2, undefined);
  RecordSet(waiter, 3, false);
  AppendWaiter(list, waiter);
  const t: f64 = AsNumber(RecordGet(r, 3));
  if (t !== 1 / 0) {
    const setTimeout: Value = Get(GlobalObject(), "setTimeout");
    if (IsCallable(setTimeout)) {
      const data: Obj = NewRecord(2);
      RecordSet(data, 0, list);
      RecordSet(data, 1, waiter);
      Call(setTimeout, undefined, NewBuiltinClosure(27, data, 0), t);
    }
  }
  return WaitAsyncResult(true, promise);
}

/** Appends a waiter to the end of a waiter list. */
function AppendWaiter(list: Obj, waiter: Obj): void {
  let cur: Value = RecordGet(list, 0);
  if (cur === undefined) {
    RecordSet(list, 0, waiter);
    return;
  }
  while (RecordGet(AsObj(cur), 2) !== undefined) {
    cur = RecordGet(AsObj(cur), 2);
  }
  RecordSet(AsObj(cur), 2, waiter);
}

/** Removes a waiter from its list (nothing if it is not there). */
function RemoveWaiter(list: Obj, waiter: Obj): void {
  let prev: Value = undefined;
  let cur: Value = RecordGet(list, 0);
  while (cur !== undefined) {
    const next: Value = RecordGet(AsObj(cur), 2);
    if (cur === waiter) {
      if (prev === undefined) {
        RecordSet(list, 0, next);
      } else {
        RecordSet(AsObj(prev), 2, next);
      }
      return;
    }
    prev = cur;
    cur = next;
  }
}

/**
 * The timeout of an Atomics.waitAsync waiter: `this` is [list, waiter]; the waiter's
 * promise resolves to "timed-out" unless Atomics.notify settled it first.
 * @intrinsic
 */
function AtomicsWaitTimeout(this: Value): void {
  const data: Obj = AsObj(this);
  const waiter: Obj = AsObj(RecordGet(data, 1));
  if (AsBool(RecordGet(waiter, 3))) {
    return;
  }
  RecordSet(waiter, 3, true);
  RemoveWaiter(AsObj(RecordGet(data, 0)), waiter);
  ResolvePromise(AsObj(RecordGet(waiter, 1)), "timed-out");
}

/**
 * Atomics.notify ( typedArray, index, count ), 25.4.15: wakes up to count waiters on
 * the element, oldest first; 0 for a typed array that is not shared.
 * @builtin Atomics.notify
 */
function AtomicsNotify(ta: Value, index: Value, count: Value): f64 {
  const o: Obj = ValidateIntegerTypedArray(ta, true);
  const i: f64 = ValidateAtomicAccess(o, index);
  let c: f64 = 1 / 0;
  if (count !== undefined) {
    c = ToIntegerOrInfinity(count);
    if (c < 0) {
      c = 0;
    }
  }
  const buffer: Obj = AsObj(TypedArrayField(o, 1));
  if (!AsBool(BufferInfo(buffer, 5))) {
    return 0;
  }
  const list: Obj = AsObj(BufferInfo(buffer, 6));
  let n: f64 = 0;
  let cur: Value = RecordGet(list, 0);
  while (cur !== undefined && n < c) {
    const w: Obj = AsObj(cur);
    cur = RecordGet(w, 2);
    if (AsNumber(RecordGet(w, 0)) === i) {
      RemoveWaiter(list, w);
      RecordSet(w, 3, true);
      ResolvePromise(AsObj(RecordGet(w, 1)), "ok");
      n = n + 1;
    }
  }
  return n;
}
