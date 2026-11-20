// SharedArrayBuffer (ECMA-262 25.2). A SharedArrayBuffer is an ArrayBuffer cell whose
// slot 2 holds its Atomics waiter list (heap/typed.rs): with one agent its bytes are
// never shared, but views, Atomics and structuredClone see it as shared.
// @slim-omit: left out of the slim build (D2411).

/** The SharedArrayBuffer `this` of a prototype method. */
function ThisSharedArrayBuffer(v: Value): Obj {
  if (!AsBool(BufferInfo(v, 5))) {
    throw NewTypeError("SharedArrayBuffer expected");
  }
  return AsObj(v);
}

/** AllocateSharedArrayBuffer ( constructor, byteLength [ , maxByteLength ] ), 25.2.2.1. */
function AllocateSharedArrayBuffer(newTarget: Value, byteLength: f64, max: Value): Obj {
  if (max !== undefined && byteLength > AsNumber(max)) {
    throw NewRangeError("invalid array buffer max length");
  }
  const proto: Obj = GetPrototypeFromConstructor(newTarget, "SharedArrayBufferPrototype");
  const waiters: Obj = NewRecord(1);
  RecordSet(waiters, 0, undefined);
  return SharedArrayBufferCreate(proto, byteLength, max, waiters);
}

/**
 * SharedArrayBuffer ( length [ , options ] ), 25.2.3.1.
 * @constructor SharedArrayBuffer
 * @length 1
 */
function SharedArrayBufferConstructor(length: Value, options?: Value): Obj {
  if (new.target === undefined) {
    throw NewTypeError("must be called with new");
  }
  const byteLength: f64 = ToIndex(length);
  let max: Value = undefined;
  if (IsObject(options)) {
    const m: Value = Get(AsObj(options), "maxByteLength");
    if (m !== undefined) {
      max = ToIndex(m);
    }
  }
  return AllocateSharedArrayBuffer(new.target, byteLength, max);
}

/**
 * get SharedArrayBuffer [ %Symbol.species% ], 25.2.4.2.
 * @getter SharedArrayBuffer[Symbol.species]
 */
function SharedArrayBufferSpecies(this: Value): Value {
  return this;
}

/**
 * get SharedArrayBuffer.prototype.byteLength, 25.2.5.1.
 * @getter SharedArrayBuffer.prototype.byteLength
 */
function SharedArrayBufferPrototypeByteLength(this: Value): Value {
  return BufferInfo(ThisSharedArrayBuffer(this), 0);
}

/**
 * SharedArrayBuffer.prototype.grow ( newLength ), 25.2.5.3.
 * @builtin SharedArrayBuffer.prototype.grow
 */
function SharedArrayBufferPrototypeGrow(this: Value, newLength: Value): void {
  const o: Obj = ThisSharedArrayBuffer(this);
  if (!AsBool(BufferInfo(o, 2))) {
    throw NewTypeError("SharedArrayBuffer is not growable");
  }
  const newByteLength: f64 = ToIndex(newLength);
  if (newByteLength > AsNumber(BufferInfo(o, 1))) {
    throw NewRangeError("invalid array buffer length");
  }
  if (newByteLength < BufferLength(o)) {
    throw NewRangeError("SharedArrayBuffer cannot shrink");
  }
  BufferResize(o, newByteLength);
}

/**
 * get SharedArrayBuffer.prototype.growable, 25.2.5.4.
 * @getter SharedArrayBuffer.prototype.growable
 */
function SharedArrayBufferPrototypeGrowable(this: Value): Value {
  return BufferInfo(ThisSharedArrayBuffer(this), 2);
}

/**
 * get SharedArrayBuffer.prototype.maxByteLength, 25.2.5.5.
 * @getter SharedArrayBuffer.prototype.maxByteLength
 */
function SharedArrayBufferPrototypeMaxByteLength(this: Value): Value {
  return BufferInfo(ThisSharedArrayBuffer(this), 1);
}

/**
 * SharedArrayBuffer.prototype.slice ( start, end ), 25.2.5.6.
 * @builtin SharedArrayBuffer.prototype.slice
 */
function SharedArrayBufferPrototypeSlice(this: Value, start: Value, end: Value): Obj {
  const o: Obj = ThisSharedArrayBuffer(this);
  const len: f64 = BufferLength(o);
  const first: f64 = RelativeIndex(start, len, 0);
  const fin: f64 = RelativeIndex(end, len, len);
  const newLen: f64 = fin > first ? fin - first : 0;
  const ctor: Value = SpeciesConstructor(o, RealmConstructor("SharedArrayBuffer"));
  const created: Obj = Construct(ctor, newLen);
  ThisSharedArrayBuffer(created);
  if (created === o) {
    throw NewTypeError("cannot use identical SharedArrayBuffer");
  }
  if (BufferLength(created) < newLen) {
    throw NewTypeError("new SharedArrayBuffer is too small");
  }
  if (newLen > 0) {
    BufferCopy(o, first, created, 0, newLen);
  }
  return created;
}

/**
 * SharedArrayBuffer.prototype [ %Symbol.toStringTag% ], 25.2.5.7.
 * @value SharedArrayBuffer.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function SharedArrayBufferPrototypeToStringTag(): Str {
  return "SharedArrayBuffer";
}
