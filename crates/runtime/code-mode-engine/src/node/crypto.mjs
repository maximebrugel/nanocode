import { Buffer } from "node:buffer";

const ALGORITHMS = {
  md5: { native: null, block: 64, size: 16 },
  sha1: { native: "SHA-1", block: 64, size: 20 },
  sha256: { native: "SHA-256", block: 64, size: 32 },
  sha384: { native: "SHA-384", block: 128, size: 48 },
  sha512: { native: "SHA-512", block: 128, size: 64 },
};
const ALIASES = { "sha-1": "sha1", "sha-256": "sha256", "sha-384": "sha384", "sha-512": "sha512", rsa_md5: "md5", "rsa-sha1": "sha1", "rsa-sha256": "sha256", "rsa-sha384": "sha384", "rsa-sha512": "sha512" };

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function algorithmOf(name) {
  const key = String(name).toLowerCase();
  const algorithm = ALGORITHMS[ALIASES[key] ?? key];
  if (algorithm === undefined) throw codedError(Error, "ERR_OSSL_EVP_UNSUPPORTED", "Digest method not supported");
  return algorithm;
}

function bytesOf(data, encoding, name = "data") {
  if (typeof data === "string") return Buffer.from(data, encoding);
  if (data instanceof Uint8Array) return data;
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"" + name + "\" argument must be of type string or an instance of Buffer, TypedArray, or DataView. Received " + (data === null ? "null" : typeof data));
}

function md5(bytes) {
  const s = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
  const k = new Int32Array(64);
  for (let i = 0; i < 64; i++) k[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 2 ** 32) | 0;
  const length = bytes.length;
  const padded = new Uint8Array((((length + 8) >> 6) + 1) * 64);
  padded.set(bytes);
  padded[length] = 0x80;
  const bits = new DataView(padded.buffer);
  bits.setUint32(padded.length - 8, (length * 8) >>> 0, true);
  bits.setUint32(padded.length - 4, Math.floor(length / 2 ** 29), true);
  let a0 = 0x67452301;
  let b0 = 0xefcdab89 | 0;
  let c0 = 0x98badcfe | 0;
  let d0 = 0x10325476;
  const m = new Int32Array(16);
  for (let block = 0; block < padded.length; block += 64) {
    for (let i = 0; i < 16; i++) m[i] = bits.getInt32(block + i * 4, true);
    let a = a0;
    let b = b0;
    let c = c0;
    let d = d0;
    for (let i = 0; i < 64; i++) {
      let f;
      let g;
      if (i < 16) {
        f = (b & c) | (~b & d);
        g = i;
      } else if (i < 32) {
        f = (d & b) | (~d & c);
        g = (5 * i + 1) % 16;
      } else if (i < 48) {
        f = b ^ c ^ d;
        g = (3 * i + 5) % 16;
      } else {
        f = c ^ (b | ~d);
        g = (7 * i) % 16;
      }
      const sum = (a + f + k[i] + m[g]) | 0;
      a = d;
      d = c;
      c = b;
      b = (b + ((sum << s[i]) | (sum >>> (32 - s[i])))) | 0;
    }
    a0 = (a0 + a) | 0;
    b0 = (b0 + b) | 0;
    c0 = (c0 + c) | 0;
    d0 = (d0 + d) | 0;
  }
  const out = new Uint8Array(16);
  const view = new DataView(out.buffer);
  view.setInt32(0, a0, true);
  view.setInt32(4, b0, true);
  view.setInt32(8, c0, true);
  view.setInt32(12, d0, true);
  return out;
}

function digestBytes(algorithm, bytes) {
  if (algorithm.native === null) return md5(bytes);
  const native = globalThis.__nanocodeBytes;
  if (native === undefined) throw new Error("hashing is not available in this runtime");
  const copy = Uint8Array.from(bytes);
  return new Uint8Array(native.digest(algorithm.native, copy.buffer, 0, copy.length));
}

function join(chunks) {
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    out.set(chunk, at);
    at += chunk.length;
  }
  return out;
}

function output(bytes, encoding) {
  const buffer = Buffer.from(bytes);
  return encoding === undefined || encoding === "buffer" ? buffer : buffer.toString(encoding);
}

class Hash {
  #algorithm;
  #chunks = [];
  #done = false;

  constructor(algorithm, options) {
    this.#algorithm = typeof algorithm === "string" ? algorithmOf(algorithm) : algorithm;
    if (options?.outputLength !== undefined && options.outputLength !== this.#algorithm.size) {
      throw codedError(Error, "ERR_OSSL_EVP_NOT_XOF_OR_INVALID_LENGTH", "Invalid XOF digest length");
    }
  }

  update(data, encoding) {
    if (this.#done) throw codedError(Error, "ERR_CRYPTO_HASH_FINALIZED", "Digest already called");
    this.#chunks.push(Uint8Array.from(bytesOf(data, encoding)));
    return this;
  }

  digest(encoding) {
    if (this.#done) throw codedError(Error, "ERR_CRYPTO_HASH_FINALIZED", "Digest already called");
    this.#done = true;
    return output(digestBytes(this.#algorithm, join(this.#chunks)), encoding);
  }

  copy() {
    if (this.#done) throw codedError(Error, "ERR_CRYPTO_HASH_FINALIZED", "Digest already called");
    const copy = new Hash(this.#algorithm);
    for (const chunk of this.#chunks) copy.update(chunk);
    return copy;
  }
}

function hmacBytes(algorithm, key, message) {
  let block = key.length > algorithm.block ? digestBytes(algorithm, key) : key;
  const padded = new Uint8Array(algorithm.block);
  padded.set(block);
  const inner = new Uint8Array(algorithm.block + message.length);
  const outer = new Uint8Array(algorithm.block + algorithm.size);
  for (let i = 0; i < algorithm.block; i++) {
    inner[i] = padded[i] ^ 0x36;
    outer[i] = padded[i] ^ 0x5c;
  }
  inner.set(message, algorithm.block);
  outer.set(digestBytes(algorithm, inner), algorithm.block);
  return digestBytes(algorithm, outer);
}

class Hmac {
  #algorithm;
  #key;
  #chunks = [];
  #done = false;

  constructor(algorithm, key, options) {
    this.#algorithm = algorithmOf(algorithm);
    this.#key = Uint8Array.from(bytesOf(key?.export?.() ?? key, options?.encoding, "key"));
  }

  update(data, encoding) {
    if (this.#done) throw codedError(Error, "ERR_CRYPTO_HASH_FINALIZED", "Digest already called");
    this.#chunks.push(Uint8Array.from(bytesOf(data, encoding)));
    return this;
  }

  digest(encoding) {
    if (this.#done) return output(new Uint8Array(0), encoding);
    this.#done = true;
    return output(hmacBytes(this.#algorithm, this.#key, join(this.#chunks)), encoding);
  }
}

function createHash(algorithm, options) {
  return new Hash(algorithm, options);
}

function createHmac(algorithm, key, options) {
  return new Hmac(algorithm, key, options);
}

function hash(algorithm, data, outputEncoding = "hex") {
  return new Hash(algorithm).update(data).digest(outputEncoding);
}

function getHashes() {
  return Object.keys(ALGORITHMS);
}

function randomFill(bytes) {
  const native = globalThis.__nanocodeBytes;
  if (native === undefined) throw new Error("randomness is not available in this runtime");
  bytes.set(new Uint8Array(native.random(bytes.length)));
  return bytes;
}

function checkSize(size) {
  if (typeof size !== "number") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"size\" argument must be of type number. Received " + (size === null ? "null" : typeof size));
  if (!(size >= 0 && size <= 2 ** 31 - 1) || !Number.isInteger(size)) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"size\" is out of range. It must be >= 0 && <= 2147483647. Received " + size);
}

function randomBytes(size, callback) {
  checkSize(size);
  const bytes = randomFill(Buffer.alloc(size));
  if (typeof callback === "function") {
    queueMicrotask(() => callback(null, bytes));
    return undefined;
  }
  return bytes;
}

function randomFillSync(target, offset = 0, size) {
  const view = new Uint8Array(target.buffer ?? target, target.byteOffset ?? 0, target.byteLength);
  const end = size === undefined ? view.length : offset + size;
  randomFill(view.subarray(offset, end));
  return target;
}

function randomFillAsync(target, offset, size, callback) {
  if (typeof offset === "function") {
    callback = offset;
    offset = 0;
    size = undefined;
  } else if (typeof size === "function") {
    callback = size;
    size = undefined;
  }
  randomFillSync(target, offset, size);
  queueMicrotask(() => callback(null, target));
}

function randomUUID() {
  const bytes = randomFill(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Buffer.from(bytes).toString("hex");
  return hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-" + hex.slice(12, 16) + "-" + hex.slice(16, 20) + "-" + hex.slice(20);
}

function randomInt(min, max, callback) {
  if (max === undefined || typeof max === "function") {
    callback = max;
    max = min;
    min = 0;
  }
  if (!Number.isSafeInteger(min)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"min\" argument must be a safe integer. Received " + String(min));
  if (!Number.isSafeInteger(max)) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"max\" argument must be a safe integer. Received " + String(max));
  if (max <= min) throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"max\" is out of range. It must be greater than the value of \"min\" (" + min + "). Received " + max);
  const range = max - min;
  const limit = 2 ** 48 - (2 ** 48 % range);
  let value;
  do {
    const bytes = randomFill(new Uint8Array(6));
    value = bytes.reduce((sum, byte) => sum * 256 + byte, 0);
  } while (value >= limit);
  const result = min + (value % range);
  if (typeof callback === "function") {
    queueMicrotask(() => callback(null, result));
    return undefined;
  }
  return result;
}

function getRandomValues(array) {
  if (!ArrayBuffer.isView(array) || array instanceof Float32Array || array instanceof Float64Array || array instanceof DataView) {
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"typedArray\" argument must be an integer-type TypedArray");
  }
  randomFill(new Uint8Array(array.buffer, array.byteOffset, array.byteLength));
  return array;
}

function timingSafeEqual(a, b) {
  const left = bytesOf(a, undefined, "buf1");
  const right = bytesOf(b, undefined, "buf2");
  if (left.length !== right.length) throw codedError(RangeError, "ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH", "Input buffers must have the same byte length");
  let diff = 0;
  for (let i = 0; i < left.length; i++) diff |= left[i] ^ right[i];
  return diff === 0;
}

function pbkdf2Sync(password, salt, iterations, keylen, digest) {
  const algorithm = algorithmOf(digest);
  const key = Uint8Array.from(bytesOf(password, undefined, "password"));
  const saltBytes = bytesOf(salt, undefined, "salt");
  const out = new Uint8Array(keylen);
  for (let block = 1, at = 0; at < keylen; block++) {
    const first = new Uint8Array(saltBytes.length + 4);
    first.set(saltBytes);
    new DataView(first.buffer).setUint32(saltBytes.length, block);
    let u = hmacBytes(algorithm, key, first);
    const t = Uint8Array.from(u);
    for (let i = 1; i < iterations; i++) {
      u = hmacBytes(algorithm, key, u);
      for (let j = 0; j < t.length; j++) t[j] ^= u[j];
    }
    const part = t.subarray(0, Math.min(t.length, keylen - at));
    out.set(part, at);
    at += part.length;
  }
  return Buffer.from(out);
}

function pbkdf2(password, salt, iterations, keylen, digest, callback) {
  let result;
  let failure = null;
  try {
    result = pbkdf2Sync(password, salt, iterations, keylen, digest);
  } catch (error) {
    failure = error;
  }
  queueMicrotask(() => callback(failure, result));
}

function unavailable(name) {
  return function () {
    const error = new Error("crypto." + name + " is not available in Code Mode cells");
    error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
    throw error;
  };
}

const createCipheriv = unavailable("createCipheriv");
const createDecipheriv = unavailable("createDecipheriv");
const createSign = unavailable("createSign");
const createVerify = unavailable("createVerify");
const generateKeyPairSync = unavailable("generateKeyPairSync");
const scryptSync = unavailable("scryptSync");
const webcrypto = globalThis.crypto;
const subtle = globalThis.crypto?.subtle;
const constants = {};

const crypto = {
  createHash,
  createHmac,
  hash,
  getHashes,
  randomBytes,
  randomFillSync,
  randomFill: randomFillAsync,
  randomUUID,
  randomInt,
  getRandomValues,
  timingSafeEqual,
  pbkdf2,
  pbkdf2Sync,
  createCipheriv,
  createDecipheriv,
  createSign,
  createVerify,
  generateKeyPairSync,
  scryptSync,
  webcrypto,
  subtle,
  constants,
  Hash,
  Hmac,
};

export default crypto;
export {
  createHash,
  createHmac,
  hash,
  getHashes,
  randomBytes,
  randomFillSync,
  randomFillAsync as randomFill,
  randomUUID,
  randomInt,
  getRandomValues,
  timingSafeEqual,
  pbkdf2,
  pbkdf2Sync,
  createCipheriv,
  createDecipheriv,
  createSign,
  createVerify,
  generateKeyPairSync,
  scryptSync,
  webcrypto,
  subtle,
  constants,
  Hash,
  Hmac,
};
