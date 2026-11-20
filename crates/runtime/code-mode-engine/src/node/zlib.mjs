import { Buffer } from "node:buffer";

const LENGTH_BASE = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CODE_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
const WINDOW = 32768;

const constants = {
  Z_NO_FLUSH: 0,
  Z_PARTIAL_FLUSH: 1,
  Z_SYNC_FLUSH: 2,
  Z_FULL_FLUSH: 3,
  Z_FINISH: 4,
  Z_BLOCK: 5,
  Z_OK: 0,
  Z_STREAM_END: 1,
  Z_NEED_DICT: 2,
  Z_ERRNO: -1,
  Z_STREAM_ERROR: -2,
  Z_DATA_ERROR: -3,
  Z_MEM_ERROR: -4,
  Z_BUF_ERROR: -5,
  Z_VERSION_ERROR: -6,
  Z_NO_COMPRESSION: 0,
  Z_BEST_SPEED: 1,
  Z_BEST_COMPRESSION: 9,
  Z_DEFAULT_COMPRESSION: -1,
  Z_FILTERED: 1,
  Z_HUFFMAN_ONLY: 2,
  Z_RLE: 3,
  Z_FIXED: 4,
  Z_DEFAULT_STRATEGY: 0,
  ZLIB_VERNUM: 4865,
  DEFLATE: 1,
  INFLATE: 2,
  GZIP: 3,
  GUNZIP: 4,
  DEFLATERAW: 5,
  INFLATERAW: 6,
  UNZIP: 7,
  Z_MIN_WINDOWBITS: 8,
  Z_MAX_WINDOWBITS: 15,
  Z_DEFAULT_WINDOWBITS: 15,
  Z_MIN_LEVEL: -1,
  Z_MAX_LEVEL: 9,
  Z_DEFAULT_LEVEL: -1,
};

function dataError(message) {
  const error = new Error(message);
  error.errno = -3;
  error.code = "Z_DATA_ERROR";
  return error;
}

function bufError() {
  const error = new Error("unexpected end of file");
  error.errno = -5;
  error.code = "Z_BUF_ERROR";
  return error;
}

const CRC_TABLE = new Int32Array(256);
for (let n = 0; n < 256; n++) {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  CRC_TABLE[n] = c;
}

function crc32(data, value = 0) {
  const bytes = typeof data === "string" ? Buffer.from(data) : data;
  let crc = ~value;
  for (let i = 0; i < bytes.length; i++) crc = CRC_TABLE[(crc ^ bytes[i]) & 255] ^ (crc >>> 8);
  return ~crc >>> 0;
}

function adler32(bytes) {
  let a = 1;
  let b = 0;
  for (let i = 0; i < bytes.length; i++) {
    a = (a + bytes[i]) % 65521;
    b = (b + a) % 65521;
  }
  return ((b << 16) | a) >>> 0;
}

function table(lengths) {
  const counts = new Uint16Array(16);
  for (const length of lengths) counts[length]++;
  counts[0] = 0;
  const offsets = new Uint16Array(16);
  for (let i = 1; i < 16; i++) offsets[i] = offsets[i - 1] + counts[i - 1];
  const symbols = new Uint16Array(lengths.length);
  for (let i = 0; i < lengths.length; i++) if (lengths[i]) symbols[offsets[lengths[i]]++] = i;
  return { counts, symbols };
}

class Reader {
  constructor(bytes, at) {
    this.bytes = bytes;
    this.at = at;
    this.bit = 0;
    this.bits = 0;
  }

  need(count) {
    while (this.bits < count) {
      if (this.at >= this.bytes.length) throw bufError();
      this.bit |= this.bytes[this.at++] << this.bits;
      this.bits += 8;
    }
  }

  read(count) {
    if (count === 0) return 0;
    this.need(count);
    const value = this.bit & ((1 << count) - 1);
    this.bit >>>= count;
    this.bits -= count;
    return value;
  }

  decode(huffman) {
    let code = 0;
    let first = 0;
    let index = 0;
    for (let length = 1; length < 16; length++) {
      code |= this.read(1);
      const count = huffman.counts[length];
      if (code - count < first) return huffman.symbols[index + (code - first)];
      index += count;
      first += count;
      first <<= 1;
      code <<= 1;
    }
    throw dataError("invalid code");
  }

  align() {
    this.bit = 0;
    this.bits = 0;
  }
}

let fixedTables;

function fixed() {
  if (fixedTables === undefined) {
    const lengths = new Uint8Array(288);
    lengths.fill(8, 0, 144);
    lengths.fill(9, 144, 256);
    lengths.fill(7, 256, 280);
    lengths.fill(8, 280, 288);
    fixedTables = [table(lengths), table(new Uint8Array(30).fill(5))];
  }
  return fixedTables;
}

class Output {
  constructor() {
    this.bytes = new Uint8Array(1024);
    this.length = 0;
  }

  grow(extra) {
    if (this.length + extra <= this.bytes.length) return;
    let size = this.bytes.length * 2;
    while (size < this.length + extra) size *= 2;
    const next = new Uint8Array(size);
    next.set(this.bytes.subarray(0, this.length));
    this.bytes = next;
  }

  push(byte) {
    this.grow(1);
    this.bytes[this.length++] = byte;
  }

  result() {
    return this.bytes.slice(0, this.length);
  }
}

function inflateFrom(bytes, at) {
  const reader = new Reader(bytes, at);
  const out = new Output();
  let last = 0;
  while (!last) {
    last = reader.read(1);
    const type = reader.read(2);
    if (type === 0) {
      reader.align();
      if (reader.at + 4 > bytes.length) throw bufError();
      const length = bytes[reader.at] | (bytes[reader.at + 1] << 8);
      const check = bytes[reader.at + 2] | (bytes[reader.at + 3] << 8);
      if ((length ^ 0xffff) !== check) throw dataError("invalid stored block lengths");
      reader.at += 4;
      if (reader.at + length > bytes.length) throw bufError();
      out.grow(length);
      out.bytes.set(bytes.subarray(reader.at, reader.at + length), out.length);
      out.length += length;
      reader.at += length;
      continue;
    }
    let lit;
    let dist;
    if (type === 1) {
      [lit, dist] = fixed();
    } else if (type === 2) {
      const hlit = reader.read(5) + 257;
      const hdist = reader.read(5) + 1;
      const hclen = reader.read(4) + 4;
      const codeLengths = new Uint8Array(19);
      for (let i = 0; i < hclen; i++) codeLengths[CODE_ORDER[i]] = reader.read(3);
      const codes = table(codeLengths);
      const lengths = new Uint8Array(hlit + hdist);
      for (let i = 0; i < hlit + hdist;) {
        const symbol = reader.decode(codes);
        if (symbol < 16) {
          lengths[i++] = symbol;
        } else {
          let repeat;
          let value = 0;
          if (symbol === 16) {
            if (i === 0) throw dataError("invalid bit length repeat");
            value = lengths[i - 1];
            repeat = 3 + reader.read(2);
          } else if (symbol === 17) {
            repeat = 3 + reader.read(3);
          } else {
            repeat = 11 + reader.read(7);
          }
          if (i + repeat > hlit + hdist) throw dataError("invalid bit length repeat");
          lengths.fill(value, i, i + repeat);
          i += repeat;
        }
      }
      lit = table(lengths.subarray(0, hlit));
      dist = table(lengths.subarray(hlit));
    } else {
      throw dataError("invalid block type");
    }
    for (;;) {
      const symbol = reader.decode(lit);
      if (symbol < 256) {
        out.push(symbol);
      } else if (symbol === 256) {
        break;
      } else {
        const index = symbol - 257;
        if (index >= 29) throw dataError("invalid literal/length code");
        const length = LENGTH_BASE[index] + reader.read(LENGTH_EXTRA[index]);
        const code = reader.decode(dist);
        if (code >= 30) throw dataError("invalid distance code");
        const distance = DIST_BASE[code] + reader.read(DIST_EXTRA[code]);
        if (distance > out.length) throw dataError("invalid distance too far back");
        out.grow(length);
        for (let i = 0; i < length; i++) {
          out.bytes[out.length] = out.bytes[out.length - distance];
          out.length++;
        }
      }
    }
  }
  return { bytes: out.result(), end: reader.at };
}

class Writer {
  constructor() {
    this.out = new Output();
    this.bit = 0;
    this.bits = 0;
  }

  write(value, count) {
    this.bit |= value << this.bits;
    this.bits += count;
    while (this.bits >= 8) {
      this.out.push(this.bit & 255);
      this.bit >>>= 8;
      this.bits -= 8;
    }
  }

  code(code, length) {
    let reversed = 0;
    for (let i = 0; i < length; i++) reversed |= ((code >> i) & 1) << (length - 1 - i);
    this.write(reversed, length);
  }

  flush() {
    if (this.bits > 0) this.out.push(this.bit & 255);
    this.bit = 0;
    this.bits = 0;
    return this.out.result();
  }
}

function literal(writer, symbol) {
  if (symbol < 144) writer.code(0x30 + symbol, 8);
  else if (symbol < 256) writer.code(0x190 + symbol - 144, 9);
  else if (symbol < 280) writer.code(symbol - 256, 7);
  else writer.code(0xc0 + symbol - 280, 8);
}

function match(writer, length, distance) {
  let index = 28;
  while (LENGTH_BASE[index] > length) index--;
  literal(writer, 257 + index);
  writer.write(length - LENGTH_BASE[index], LENGTH_EXTRA[index]);
  let code = 29;
  while (DIST_BASE[code] > distance) code--;
  writer.code(code, 5);
  writer.write(distance - DIST_BASE[code], DIST_EXTRA[code]);
}

function stored(bytes) {
  const out = new Output();
  let at = 0;
  do {
    const length = Math.min(65535, bytes.length - at);
    const last = at + length >= bytes.length ? 1 : 0;
    out.push(last);
    out.push(length & 255);
    out.push(length >> 8);
    out.push(~length & 255);
    out.push((~length >> 8) & 255);
    out.grow(length);
    out.bytes.set(bytes.subarray(at, at + length), out.length);
    out.length += length;
    at += length;
  } while (at < bytes.length);
  return out.result();
}

function deflateRawBytes(bytes, level) {
  if (level === 0) return stored(bytes);
  const chain = level >= 6 ? 128 : 16;
  const writer = new Writer();
  writer.write(1, 1);
  writer.write(1, 2);
  const head = new Int32Array(65536).fill(-1);
  const previous = new Int32Array(WINDOW);
  const hash = (i) => ((bytes[i] << 8) ^ (bytes[i + 1] << 4) ^ bytes[i + 2]) & 65535;
  const insert = (i) => {
    if (i + 2 >= bytes.length) return;
    const h = hash(i);
    previous[i & (WINDOW - 1)] = head[h];
    head[h] = i;
  };
  let i = 0;
  while (i < bytes.length) {
    let bestLength = 0;
    let bestDistance = 0;
    if (i + 2 < bytes.length) {
      let candidate = head[hash(i)];
      let tries = chain;
      const limit = Math.min(258, bytes.length - i);
      while (candidate >= 0 && i - candidate <= WINDOW && tries-- > 0) {
        let length = 0;
        while (length < limit && bytes[candidate + length] === bytes[i + length]) length++;
        if (length > bestLength) {
          bestLength = length;
          bestDistance = i - candidate;
          if (length === limit) break;
        }
        const next = previous[candidate & (WINDOW - 1)];
        if (next >= candidate) break;
        candidate = next;
      }
    }
    if (bestLength >= 3) {
      match(writer, bestLength, bestDistance);
      for (let j = 0; j < bestLength; j++) insert(i + j);
      i += bestLength;
    } else {
      literal(writer, bytes[i]);
      insert(i);
      i++;
    }
  }
  literal(writer, 256);
  const compressed = writer.flush();
  return compressed.length > bytes.length + 5 * Math.ceil(bytes.length / 65535 || 1) ? stored(bytes) : compressed;
}

function input(data) {
  if (typeof data === "string") return Buffer.from(data);
  if (data instanceof Uint8Array) return data;
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  const error = new TypeError("The \"buffer\" argument must be of type string or an instance of Buffer, TypedArray, DataView, or ArrayBuffer. Received " + (data === null ? "null" : typeof data));
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
}

function levelOf(options) {
  const level = options?.level ?? -1;
  return level === -1 ? 6 : level;
}

function deflateRawSync(data, options) {
  return Buffer.from(deflateRawBytes(input(data), levelOf(options)));
}

function inflateRawSync(data) {
  return Buffer.from(inflateFrom(input(data), 0).bytes);
}

function deflateSync(data, options) {
  const bytes = input(data);
  const level = levelOf(options);
  const body = deflateRawBytes(bytes, level);
  const out = new Uint8Array(body.length + 6);
  out[0] = 0x78;
  out[1] = level === 0 || level === 1 ? 0x01 : level < 6 ? 0x5e : level === 6 ? 0x9c : 0xda;
  out.set(body, 2);
  new DataView(out.buffer).setUint32(body.length + 2, adler32(bytes));
  return Buffer.from(out);
}

function inflateSync(data) {
  const bytes = input(data);
  if (bytes.length < 2) throw bufError();
  if ((bytes[0] & 15) !== 8 || ((bytes[0] << 8) | bytes[1]) % 31 !== 0) throw dataError("incorrect header check");
  if (bytes[1] & 0x20) throw dataError("Missing dictionary");
  const { bytes: out, end } = inflateFrom(bytes, 2);
  if (end + 4 > bytes.length) throw bufError();
  if (new DataView(bytes.buffer, bytes.byteOffset + end, 4).getUint32(0) !== adler32(out)) throw dataError("incorrect data check");
  return Buffer.from(out);
}

function gzipSync(data, options) {
  const bytes = input(data);
  const level = levelOf(options);
  const body = deflateRawBytes(bytes, level);
  const out = new Uint8Array(body.length + 18);
  out.set([0x1f, 0x8b, 8, 0, 0, 0, 0, 0, level === 9 ? 2 : level === 1 ? 4 : 0, 3]);
  out.set(body, 10);
  const view = new DataView(out.buffer);
  view.setUint32(body.length + 10, crc32(bytes), true);
  view.setUint32(body.length + 14, bytes.length >>> 0, true);
  return Buffer.from(out);
}

function gunzipSync(data) {
  const bytes = input(data);
  const parts = [];
  let at = 0;
  do {
    if (bytes.length - at < 10) throw bufError();
    if (bytes[at] !== 0x1f || bytes[at + 1] !== 0x8b) throw dataError("incorrect header check");
    if (bytes[at + 2] !== 8) throw dataError("unknown compression method");
    const flags = bytes[at + 3];
    let cursor = at + 10;
    if (flags & 4) cursor += 2 + (bytes[cursor] | (bytes[cursor + 1] << 8));
    if (flags & 8) while (bytes[cursor++] !== 0) if (cursor > bytes.length) throw bufError();
    if (flags & 16) while (bytes[cursor++] !== 0) if (cursor > bytes.length) throw bufError();
    if (flags & 2) cursor += 2;
    const { bytes: out, end } = inflateFrom(bytes, cursor);
    if (end + 8 > bytes.length) throw bufError();
    const view = new DataView(bytes.buffer, bytes.byteOffset + end, 8);
    if (view.getUint32(0, true) !== crc32(out)) throw dataError("incorrect data check");
    if (view.getUint32(4, true) !== out.length >>> 0) throw dataError("incorrect length check");
    parts.push(out);
    at = end + 8;
  } while (at < bytes.length && bytes[at] === 0x1f);
  return Buffer.concat(parts);
}

function unzipSync(data) {
  const bytes = input(data);
  return bytes[0] === 0x1f && bytes[1] === 0x8b ? gunzipSync(bytes) : inflateSync(bytes);
}

function asyncOf(fn) {
  return function (data, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    let result;
    let failure = null;
    try {
      result = fn(data, options);
    } catch (error) {
      failure = error;
    }
    queueMicrotask(() => callback(failure, result));
  };
}

function unavailable(name) {
  return function () {
    const error = new Error("zlib." + name + " is not available in Code Mode cells");
    error.code = "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM";
    throw error;
  };
}

const gzip = asyncOf(gzipSync);
const gunzip = asyncOf(gunzipSync);
const deflate = asyncOf(deflateSync);
const inflate = asyncOf(inflateSync);
const deflateRaw = asyncOf(deflateRawSync);
const inflateRaw = asyncOf(inflateRawSync);
const unzip = asyncOf(unzipSync);
const brotliCompressSync = unavailable("brotliCompressSync");
const brotliDecompressSync = unavailable("brotliDecompressSync");
const createGzip = unavailable("createGzip");
const createGunzip = unavailable("createGunzip");
const createDeflate = unavailable("createDeflate");
const createInflate = unavailable("createInflate");

const zlib = { constants, crc32, gzipSync, gunzipSync, deflateSync, inflateSync, deflateRawSync, inflateRawSync, unzipSync, gzip, gunzip, deflate, inflate, deflateRaw, inflateRaw, unzip, brotliCompressSync, brotliDecompressSync, createGzip, createGunzip, createDeflate, createInflate };

export default zlib;
export { constants, crc32, gzipSync, gunzipSync, deflateSync, inflateSync, deflateRawSync, inflateRawSync, unzipSync, gzip, gunzip, deflate, inflate, deflateRaw, inflateRaw, unzip, brotliCompressSync, brotliDecompressSync, createGzip, createGunzip, createDeflate, createInflate };
