const kMaxLength = 2 ** 32;
const kStringMaxLength = 2 ** 29 - 24;
const BASE64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const BASE64_VALUES = new Int16Array(128).fill(-1);
for (let i = 0; i < 64; i++) BASE64_VALUES[BASE64.charCodeAt(i)] = i;
BASE64_VALUES[45] = 62;
BASE64_VALUES[95] = 63;

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function received(value) {
  if (value === null) return "null";
  if (value === undefined) return "undefined";
  if (typeof value === "function") return "function " + value.name;
  if (typeof value === "object") return "an instance of " + (value.constructor?.name ?? "Object");
  return "type " + typeof value + " (" + String(value) + ")";
}

function normalizeEncoding(encoding) {
  if (encoding === undefined || encoding === null || encoding === "") return "utf8";
  switch (String(encoding).toLowerCase()) {
    case "utf8":
    case "utf-8":
      return "utf8";
    case "hex":
      return "hex";
    case "base64":
      return "base64";
    case "base64url":
      return "base64url";
    case "latin1":
    case "binary":
      return "latin1";
    case "ascii":
      return "ascii";
    case "ucs2":
    case "ucs-2":
    case "utf16le":
    case "utf-16le":
      return "utf16le";
    default:
      return undefined;
  }
}

function encodingOf(encoding) {
  const normalized = normalizeEncoding(encoding);
  if (normalized === undefined) throw codedError(TypeError, "ERR_UNKNOWN_ENCODING", "Unknown encoding: " + encoding);
  return normalized;
}

function utf8Encode(string) {
  const out = new Uint8Array(utf8Length(string));
  let at = 0;
  for (let i = 0; i < string.length; i++) {
    let code = string.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < string.length) {
      const next = string.charCodeAt(i + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        code = 0x10000 + ((code - 0xd800) << 10) + (next - 0xdc00);
        i++;
      }
    }
    if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd;
    if (code < 0x80) {
      out[at++] = code;
    } else if (code < 0x800) {
      out[at++] = 0xc0 | (code >> 6);
      out[at++] = 0x80 | (code & 63);
    } else if (code < 0x10000) {
      out[at++] = 0xe0 | (code >> 12);
      out[at++] = 0x80 | ((code >> 6) & 63);
      out[at++] = 0x80 | (code & 63);
    } else {
      out[at++] = 0xf0 | (code >> 18);
      out[at++] = 0x80 | ((code >> 12) & 63);
      out[at++] = 0x80 | ((code >> 6) & 63);
      out[at++] = 0x80 | (code & 63);
    }
  }
  return out;
}

function utf8Length(string) {
  let length = 0;
  for (let i = 0; i < string.length; i++) {
    const code = string.charCodeAt(i);
    if (code < 0x80) length += 1;
    else if (code < 0x800) length += 2;
    else if (code >= 0xd800 && code <= 0xdbff && i + 1 < string.length && string.charCodeAt(i + 1) >= 0xdc00 && string.charCodeAt(i + 1) <= 0xdfff) {
      length += 4;
      i++;
    } else length += 3;
  }
  return length;
}

function utf8Decode(bytes, start, end) {
  const parts = [];
  let chunk = [];
  let i = start;
  while (i < end) {
    const byte = bytes[i];
    let code = 0xfffd;
    let size = 1;
    if (byte < 0x80) {
      code = byte;
    } else if (byte >= 0xc2 && byte <= 0xf4) {
      const need = byte < 0xe0 ? 1 : byte < 0xf0 ? 2 : 3;
      let lower = 0x80;
      let upper = 0xbf;
      if (byte === 0xe0) lower = 0xa0;
      else if (byte === 0xed) upper = 0x9f;
      else if (byte === 0xf0) lower = 0x90;
      else if (byte === 0xf4) upper = 0x8f;
      let value = byte & (need === 1 ? 0x1f : need === 2 ? 0x0f : 0x07);
      let seen = 0;
      while (seen < need) {
        const next = bytes[i + 1 + seen];
        if (i + 1 + seen >= end || next < lower || next > upper) break;
        value = (value << 6) | (next & 0x3f);
        lower = 0x80;
        upper = 0xbf;
        seen++;
      }
      if (seen === need) code = value;
      size = 1 + seen;
    }
    if (code > 0xffff) {
      code -= 0x10000;
      chunk.push(0xd800 + (code >> 10), 0xdc00 + (code & 0x3ff));
    } else {
      chunk.push(code);
    }
    i += size;
    if (chunk.length >= 8192) {
      parts.push(String.fromCharCode.apply(null, chunk));
      chunk = [];
    }
  }
  parts.push(String.fromCharCode.apply(null, chunk));
  return parts.join("");
}

function base64Decode(string) {
  const out = new Uint8Array((string.length * 3) >> 2);
  let at = 0;
  let acc = 0;
  let bits = 0;
  for (let i = 0; i < string.length; i++) {
    const code = string.charCodeAt(i);
    if (code === 61) break;
    const value = code < 128 ? BASE64_VALUES[code] : -1;
    if (value < 0) continue;
    acc = (acc << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[at++] = (acc >> bits) & 255;
    }
  }
  return out.subarray(0, at);
}

function base64Encode(bytes, start, end, url) {
  const table = url ? BASE64URL : BASE64;
  let out = "";
  let i = start;
  for (; i + 2 < end; i += 3) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
    out += table[n >> 18] + table[(n >> 12) & 63] + table[(n >> 6) & 63] + table[n & 63];
  }
  if (end - i === 1) {
    const n = bytes[i] << 16;
    out += table[n >> 18] + table[(n >> 12) & 63] + (url ? "" : "==");
  } else if (end - i === 2) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8);
    out += table[n >> 18] + table[(n >> 12) & 63] + table[(n >> 6) & 63] + (url ? "" : "=");
  }
  return out;
}

function hexDecode(string) {
  const out = new Uint8Array(string.length >> 1);
  let at = 0;
  for (; at < out.length; at++) {
    const pair = string.slice(at * 2, at * 2 + 2);
    if (!/^[0-9a-fA-F]{2}$/.test(pair)) break;
    out[at] = parseInt(pair, 16);
  }
  return out.subarray(0, at);
}

function encodeString(string, encoding) {
  switch (encoding) {
    case "utf8":
      return utf8Encode(string);
    case "hex":
      return hexDecode(string);
    case "base64":
    case "base64url":
      return base64Decode(string);
    case "latin1":
    case "ascii": {
      const out = new Uint8Array(string.length);
      for (let i = 0; i < string.length; i++) out[i] = string.charCodeAt(i) & 255;
      return out;
    }
    case "utf16le": {
      const out = new Uint8Array(string.length * 2);
      for (let i = 0; i < string.length; i++) {
        const code = string.charCodeAt(i);
        out[i * 2] = code & 255;
        out[i * 2 + 1] = code >> 8;
      }
      return out;
    }
  }
  throw codedError(TypeError, "ERR_UNKNOWN_ENCODING", "Unknown encoding: " + encoding);
}

function decodeBytes(bytes, encoding, start, end) {
  switch (encoding) {
    case "utf8":
      return utf8Decode(bytes, start, end);
    case "hex": {
      let out = "";
      for (let i = start; i < end; i++) out += (bytes[i] < 16 ? "0" : "") + bytes[i].toString(16);
      return out;
    }
    case "base64":
      return base64Encode(bytes, start, end, false);
    case "base64url":
      return base64Encode(bytes, start, end, true);
    case "latin1":
    case "ascii": {
      let out = "";
      const mask = encoding === "ascii" ? 127 : 255;
      for (let i = start; i < end; i += 8192) {
        const slice = Array.from(bytes.subarray(i, Math.min(end, i + 8192)), (byte) => byte & mask);
        out += String.fromCharCode.apply(null, slice);
      }
      return out;
    }
    case "utf16le": {
      let out = "";
      for (let i = start; i + 1 < end; i += 2) out += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
      return out;
    }
  }
  throw codedError(TypeError, "ERR_UNKNOWN_ENCODING", "Unknown encoding: " + encoding);
}

function byteLength(value, encoding) {
  if (typeof value !== "string") {
    if (ArrayBuffer.isView(value) || value instanceof ArrayBuffer) return value.byteLength;
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received " + received(value));
  }
  const normalized = normalizeEncoding(encoding) ?? "utf8";
  switch (normalized) {
    case "utf8":
      return utf8Length(value);
    case "hex":
      return value.length >>> 1;
    case "base64":
    case "base64url": {
      let length = value.length;
      if (length > 0 && value.charCodeAt(length - 1) === 61) length--;
      if (length > 1 && value.charCodeAt(length - 1) === 61) length--;
      return (length * 3) >>> 2;
    }
    case "utf16le":
      return value.length * 2;
    default:
      return value.length;
  }
}

function clampIndex(value, length, fallback) {
  if (value === undefined) return fallback;
  let index = Math.trunc(Number(value));
  if (Number.isNaN(index)) index = 0;
  if (index < 0) index = Math.max(length + index, 0);
  return Math.min(index, length);
}

function checkOffset(buffer, offset, size) {
  if (offset === undefined) offset = 0;
  if (typeof offset !== "number") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"offset\" argument must be of type number. Received " + received(offset));
  if (!Number.isInteger(offset)) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"offset\" is out of range. It must be an integer. Received " + offset);
  if (offset < 0 || offset + size > buffer.length) {
    if (buffer.length - size < 0) throw codedError(RangeError, "ERR_BUFFER_OUT_OF_BOUNDS", "Attempt to access memory outside buffer bounds");
    throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"offset\" is out of range. It must be >= 0 and <= " + (buffer.length - size) + ". Received " + offset);
  }
  return offset;
}

function checkValue(value, min, max, size) {
  if (typeof value === "bigint" ? value < min || value > max : !(value >= min && value <= max)) {
    const range = size > 4 ? ">= " + min + "n and <= " + max + "n" : min === 0 ? ">= 0 and <= " + max : ">= " + min + " and <= " + max;
    throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"value\" is out of range. It must be " + range + ". Received " + String(value) + (typeof value === "bigint" ? "n" : ""));
  }
}

function view(buffer) {
  return new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
}

function fromObject(value) {
  if (value.type === "Buffer" && Array.isArray(value.data)) return value.data;
  if (typeof value.length === "number" || ArrayBuffer.isView(value)) return value;
  return undefined;
}

class Buffer extends Uint8Array {
  constructor(value, encodingOrOffset, length) {
    if (typeof value === "number" || value === undefined) {
      super(value ?? 0);
    } else if (value instanceof ArrayBuffer) {
      super(value, encodingOrOffset ?? 0, length ?? value.byteLength - (encodingOrOffset ?? 0));
    } else {
      return Buffer.from(value, encodingOrOffset, length);
    }
  }

  static from(value, encodingOrOffset, length) {
    if (typeof value === "string") {
      const bytes = encodeString(value, encodingOf(encodingOrOffset));
      const out = new Buffer(bytes.length);
      out.set(bytes);
      return out;
    }
    if (value instanceof ArrayBuffer || (typeof SharedArrayBuffer === "function" && value instanceof SharedArrayBuffer)) {
      const offset = encodingOrOffset === undefined ? 0 : Math.trunc(Number(encodingOrOffset)) || 0;
      if (offset > value.byteLength) throw codedError(RangeError, "ERR_BUFFER_OUT_OF_BOUNDS", "\"offset\" is outside of buffer bounds");
      const size = length === undefined ? value.byteLength - offset : Math.trunc(Number(length)) || 0;
      if (offset + size > value.byteLength) throw codedError(RangeError, "ERR_BUFFER_OUT_OF_BOUNDS", "\"length\" is outside of buffer bounds");
      return new Buffer(value, offset, size);
    }
    if (value !== null && typeof value === "object") {
      const primitive = typeof value[Symbol.toPrimitive] === "function" ? value[Symbol.toPrimitive]("string") : value.valueOf();
      if (primitive !== value && primitive !== null && (typeof primitive === "string" || typeof primitive === "object")) {
        return Buffer.from(primitive, encodingOrOffset, length);
      }
      const source = fromObject(value);
      if (source !== undefined) {
        const out = new Buffer(source.length >>> 0);
        for (let i = 0; i < out.length; i++) out[i] = source[i];
        return out;
      }
    }
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object. Received " + received(value));
  }

  static alloc(size, fill, encoding) {
    checkSize(size);
    const out = new Buffer(size);
    if (fill !== undefined && fill !== 0 && size > 0) out.fill(fill, 0, out.length, encoding);
    return out;
  }

  static allocUnsafe(size) {
    checkSize(size);
    return new Buffer(size);
  }

  static allocUnsafeSlow(size) {
    checkSize(size);
    return new Buffer(size);
  }

  static byteLength(value, encoding) {
    return byteLength(value, encoding);
  }

  static isBuffer(value) {
    return value instanceof Buffer;
  }

  static isEncoding(encoding) {
    return typeof encoding === "string" && encoding !== "" && normalizeEncoding(encoding) !== undefined;
  }

  static compare(a, b) {
    if (!(a instanceof Uint8Array)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"buf1\" argument must be an instance of Buffer or Uint8Array. Received " + received(a));
    if (!(b instanceof Uint8Array)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"buf2\" argument must be an instance of Buffer or Uint8Array. Received " + received(b));
    return compareBytes(a, 0, a.length, b, 0, b.length);
  }

  static concat(list, totalLength) {
    if (!Array.isArray(list)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"list\" argument must be an instance of Array. Received " + received(list));
    for (let i = 0; i < list.length; i++) {
      if (!(list[i] instanceof Uint8Array)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"list[" + i + "]\" argument must be an instance of Buffer or Uint8Array. Received " + received(list[i]));
    }
    const total = totalLength === undefined ? list.reduce((sum, item) => sum + item.length, 0) : totalLength;
    const out = Buffer.alloc(total);
    let at = 0;
    for (const item of list) {
      if (at >= total) break;
      const part = item.subarray(0, Math.min(item.length, total - at));
      out.set(part, at);
      at += part.length;
    }
    return out;
  }

  static copyBytesFrom(view, offset = 0, length) {
    const bytes = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
    const size = view.BYTES_PER_ELEMENT ?? 1;
    const end = length === undefined ? view.length : Math.min(view.length, offset + length);
    return Buffer.from(bytes.slice(offset * size, end * size));
  }

  get parent() {
    return this.buffer;
  }

  get offset() {
    return this.byteOffset;
  }

  toString(encoding, start, end) {
    if (arguments.length === 0) return utf8Decode(this, 0, this.length);
    const from = start === undefined ? 0 : Math.max(0, Math.trunc(Number(start)) || 0);
    const to = end === undefined ? this.length : Math.min(this.length, Math.trunc(Number(end)) || 0);
    if (from >= to) return "";
    return decodeBytes(this, encodingOf(encoding), from, to);
  }

  toLocaleString(encoding, start, end) {
    return this.toString(encoding, start, end);
  }

  toJSON() {
    return { type: "Buffer", data: Array.from(this) };
  }

  equals(other) {
    if (!(other instanceof Uint8Array)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"otherBuffer\" argument must be an instance of Buffer or Uint8Array. Received " + received(other));
    return compareBytes(this, 0, this.length, other, 0, other.length) === 0;
  }

  compare(target, targetStart = 0, targetEnd = target.length, sourceStart = 0, sourceEnd = this.length) {
    if (!(target instanceof Uint8Array)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"target\" argument must be an instance of Buffer or Uint8Array. Received " + received(target));
    return compareBytes(this, sourceStart, sourceEnd, target, targetStart, targetEnd);
  }

  copy(target, targetStart = 0, sourceStart = 0, sourceEnd = this.length) {
    if (sourceStart >= sourceEnd || targetStart >= target.length) return 0;
    const part = this.subarray(sourceStart, Math.min(sourceEnd, sourceStart + target.length - targetStart));
    Uint8Array.prototype.set.call(target, part, targetStart);
    return part.length;
  }

  subarray(start, end) {
    const from = clampIndex(start, this.length, 0);
    const to = Math.max(from, clampIndex(end, this.length, this.length));
    return new Buffer(this.buffer, this.byteOffset + from, to - from);
  }

  slice(start, end) {
    return this.subarray(start, end);
  }

  write(string, offset, length, encoding) {
    if (typeof offset === "string") {
      encoding = offset;
      offset = 0;
      length = this.length;
    } else if (typeof length === "string") {
      encoding = length;
      length = this.length - (offset ?? 0);
    }
    offset = offset === undefined ? 0 : offset;
    if (offset < 0 || offset > this.length) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"offset\" is out of range. It must be >= 0 && <= " + this.length + ". Received " + offset);
    const room = Math.min(this.length - offset, length === undefined ? Infinity : length);
    const kind = encodingOf(encoding);
    let bytes = encodeString(string, kind);
    if (bytes.length > room) {
      bytes = bytes.subarray(0, room);
      if (kind === "utf8") {
        let cut = room;
        while (cut > 0 && (bytes[cut - 1] & 0xc0) === 0x80) cut--;
        if (cut > 0) {
          const lead = bytes[cut - 1];
          const size = lead >= 0xf0 ? 4 : lead >= 0xe0 ? 3 : lead >= 0xc0 ? 2 : 1;
          if (cut - 1 + size > room) bytes = bytes.subarray(0, cut - 1);
        }
      }
    }
    this.set(bytes, offset);
    return bytes.length;
  }

  fill(value, offset, end, encoding) {
    if (typeof offset === "string") {
      encoding = offset;
      offset = 0;
      end = this.length;
    } else if (typeof end === "string") {
      encoding = end;
      end = this.length;
    }
    const from = offset === undefined ? 0 : offset;
    const to = end === undefined ? this.length : end;
    if (from < 0 || from > this.length) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"offset\" is out of range. It must be >= 0 && <= " + this.length + ". Received " + from);
    if (to < 0 || to > this.length) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"end\" is out of range. It must be >= 0 && <= " + this.length + ". Received " + to);
    if (from >= to) return this;
    if (typeof value === "number" || typeof value === "boolean") {
      Uint8Array.prototype.fill.call(this, Number(value) & 255, from, to);
      return this;
    }
    const pattern = typeof value === "string" ? encodeString(value, encodingOf(encoding)) : value instanceof Uint8Array ? value : Uint8Array.from(value ?? []);
    if (pattern.length === 0) {
      if (typeof value === "string" && value.length > 0) throw codedError(TypeError, "ERR_INVALID_ARG_VALUE", "The argument 'value' is invalid. Received '" + value + "'");
      Uint8Array.prototype.fill.call(this, 0, from, to);
      return this;
    }
    for (let i = from; i < to; i++) this[i] = pattern[(i - from) % pattern.length];
    return this;
  }

  indexOf(value, byteOffset, encoding) {
    return search(this, value, byteOffset, encoding, true);
  }

  lastIndexOf(value, byteOffset, encoding) {
    return search(this, value, byteOffset, encoding, false);
  }

  includes(value, byteOffset, encoding) {
    return this.indexOf(value, byteOffset, encoding) !== -1;
  }

  swap16() {
    return swap(this, 2);
  }

  swap32() {
    return swap(this, 4);
  }

  swap64() {
    return swap(this, 8);
  }

  readUIntLE(offset, size) {
    return readVariable(this, offset, size, true, false);
  }

  readUIntBE(offset, size) {
    return readVariable(this, offset, size, false, false);
  }

  readIntLE(offset, size) {
    return readVariable(this, offset, size, true, true);
  }

  readIntBE(offset, size) {
    return readVariable(this, offset, size, false, true);
  }

  writeUIntLE(value, offset, size) {
    return writeVariable(this, value, offset, size, true, false);
  }

  writeUIntBE(value, offset, size) {
    return writeVariable(this, value, offset, size, false, false);
  }

  writeIntLE(value, offset, size) {
    return writeVariable(this, value, offset, size, true, true);
  }

  writeIntBE(value, offset, size) {
    return writeVariable(this, value, offset, size, false, true);
  }
}

function checkSize(size) {
  if (typeof size !== "number") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"size\" argument must be of type number. Received " + received(size));
  if (!(size >= 0 && size <= kMaxLength)) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"size\" is out of range. It must be >= 0 && <= " + kMaxLength + ". Received " + size);
}

function compareBytes(a, aStart, aEnd, b, bStart, bEnd) {
  const length = Math.min(aEnd - aStart, bEnd - bStart);
  for (let i = 0; i < length; i++) {
    const x = a[aStart + i];
    const y = b[bStart + i];
    if (x !== y) return x < y ? -1 : 1;
  }
  const left = aEnd - aStart;
  const right = bEnd - bStart;
  return left < right ? -1 : left > right ? 1 : 0;
}

function search(buffer, value, byteOffset, encoding, forward) {
  if (typeof byteOffset === "string") {
    encoding = byteOffset;
    byteOffset = undefined;
  }
  let offset = byteOffset === undefined || byteOffset === null ? (forward ? 0 : buffer.length) : Number(byteOffset);
  if (Number.isNaN(offset)) offset = forward ? 0 : buffer.length;
  if (offset < 0) offset = buffer.length + offset;
  if (typeof value === "number") {
    const byte = value & 255;
    return forward
      ? Uint8Array.prototype.indexOf.call(buffer, byte, Math.max(0, offset))
      : offset < 0 ? -1 : Uint8Array.prototype.lastIndexOf.call(buffer, byte, offset);
  }
  let needle;
  if (typeof value === "string") needle = encodeString(value, encodingOf(encoding));
  else if (value instanceof Uint8Array) needle = value;
  else throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"value\" argument must be one of type number or string or an instance of Buffer or Uint8Array. Received " + received(value));
  if (needle.length === 0) return forward ? Math.min(Math.max(0, offset), buffer.length) : Math.min(offset, buffer.length);
  const last = buffer.length - needle.length;
  if (forward) {
    for (let i = Math.max(0, offset); i <= last; i++) if (matches(buffer, needle, i)) return i;
  } else {
    for (let i = Math.min(offset, last); i >= 0; i--) if (matches(buffer, needle, i)) return i;
  }
  return -1;
}

function matches(buffer, needle, at) {
  for (let j = 0; j < needle.length; j++) if (buffer[at + j] !== needle[j]) return false;
  return true;
}

function swap(buffer, size) {
  if (buffer.length % size !== 0) throw codedError(RangeError, "ERR_INVALID_BUFFER_SIZE", "Buffer size must be a multiple of " + size * 8 + "-bits");
  for (let i = 0; i < buffer.length; i += size) buffer.subarray(i, i + size).reverse();
  return buffer;
}

function readVariable(buffer, offset, size, little, signed) {
  if (!Number.isInteger(size) || size < 1 || size > 6) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"byteLength\" is out of range. It must be >= 1 and <= 6. Received " + size);
  offset = checkOffset(buffer, offset, size);
  let value = 0;
  for (let i = 0; i < size; i++) value = value * 256 + buffer[offset + (little ? size - 1 - i : i)];
  if (signed && value >= 2 ** (size * 8 - 1)) value -= 2 ** (size * 8);
  return value;
}

function writeVariable(buffer, value, offset, size, little, signed) {
  if (!Number.isInteger(size) || size < 1 || size > 6) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"byteLength\" is out of range. It must be >= 1 and <= 6. Received " + size);
  const max = signed ? 2 ** (size * 8 - 1) - 1 : 2 ** (size * 8) - 1;
  const min = signed ? -(2 ** (size * 8 - 1)) : 0;
  checkValue(value, min, max, size);
  offset = checkOffset(buffer, offset, size);
  let rest = value < 0 ? value + 2 ** (size * 8) : value;
  for (let i = 0; i < size; i++) {
    buffer[offset + (little ? i : size - 1 - i)] = rest % 256;
    rest = Math.floor(rest / 256);
  }
  return offset + size;
}

const FIXED = [
  ["UInt8", 1, "Uint8", 0, 255],
  ["Int8", 1, "Int8", -128, 127],
  ["UInt16", 2, "Uint16", 0, 65535],
  ["Int16", 2, "Int16", -32768, 32767],
  ["UInt32", 4, "Uint32", 0, 4294967295],
  ["Int32", 4, "Int32", -2147483648, 2147483647],
  ["Float", 4, "Float32", -Infinity, Infinity],
  ["Double", 8, "Float64", -Infinity, Infinity],
  ["BigUInt64", 8, "BigUint64", 0n, 2n ** 64n - 1n],
  ["BigInt64", 8, "BigInt64", -(2n ** 63n), 2n ** 63n - 1n],
];

for (const [name, size, kind, min, max] of FIXED) {
  const orders = size === 1 ? [["", false]] : [["LE", true], ["BE", false]];
  for (const [suffix, little] of orders) {
    const read = function (offset) {
      offset = checkOffset(this, offset, size);
      return view(this)["get" + kind](offset, little);
    };
    const write = function (value, offset) {
      if (kind.startsWith("Big")) {
        if (typeof value !== "bigint") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"value\" argument must be of type bigint. Received " + received(value));
        checkValue(value, min, max, size);
      } else if (!kind.startsWith("Float")) {
        checkValue(+value, min, max, size);
      }
      offset = checkOffset(this, offset, size);
      view(this)["set" + kind](offset, kind.startsWith("Big") ? value : +value, little);
      return offset + size;
    };
    for (const alias of new Set([name, name.replace("UInt", "Uint")])) {
      Object.defineProperty(Buffer.prototype, "read" + alias + suffix, { value: read, writable: true, configurable: true });
      Object.defineProperty(Buffer.prototype, "write" + alias + suffix, { value: write, writable: true, configurable: true });
    }
  }
}

for (const name of ["readUIntLE", "readUIntBE", "writeUIntLE", "writeUIntBE"]) {
  Object.defineProperty(Buffer.prototype, name.replace("UInt", "Uint"), { value: Buffer.prototype[name], writable: true, configurable: true });
}

Object.defineProperty(Buffer, Symbol.species, { get: () => Buffer, configurable: true });
Buffer.poolSize = 8192;

const SlowBuffer = function SlowBuffer(size) {
  return Buffer.allocUnsafeSlow(size);
};

function isUtf8(input) {
  const bytes = input instanceof ArrayBuffer ? new Uint8Array(input) : new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  return utf8Encode(utf8Decode(bytes, 0, bytes.length)).length === bytes.length && !utf8Decode(bytes, 0, bytes.length).includes("\ufffd") || bytes.every((byte) => byte < 0x80);
}

function isAscii(input) {
  const bytes = input instanceof ArrayBuffer ? new Uint8Array(input) : new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  return bytes.every((byte) => byte < 0x80);
}

function transcode(source, from, to) {
  return Buffer.from(Buffer.from(source).toString(encodingOf(from)), encodingOf(to));
}

const constants = { MAX_LENGTH: kMaxLength, MAX_STRING_LENGTH: kStringMaxLength };
const INSPECT_MAX_BYTES = 50;
const atob = (...args) => globalThis.atob(...args);
const btoa = (...args) => globalThis.btoa(...args);
const Blob = globalThis.Blob;
const File = globalThis.File;

export { Buffer, SlowBuffer, kMaxLength, kStringMaxLength, constants, INSPECT_MAX_BYTES, isUtf8, isAscii, transcode, atob, btoa, Blob, File };
export default { Buffer, SlowBuffer, kMaxLength, kStringMaxLength, constants, INSPECT_MAX_BYTES, isUtf8, isAscii, transcode, atob, btoa, Blob, File };
