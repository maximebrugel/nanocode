import { Buffer } from "node:buffer";

function normalize(encoding) {
  const name = String(encoding ?? "utf8").toLowerCase();
  if (name === "utf8" || name === "utf-8") return "utf8";
  if (name === "ucs2" || name === "ucs-2" || name === "utf16le" || name === "utf-16le") return "utf16le";
  if (name === "base64" || name === "base64url" || name === "hex" || name === "ascii" || name === "latin1") return name;
  if (name === "binary") return "latin1";
  const error = new TypeError("Unknown encoding: " + encoding);
  error.code = "ERR_UNKNOWN_ENCODING";
  throw error;
}

function utf8Complete(bytes) {
  let end = bytes.length;
  for (let back = 1; back <= Math.min(3, bytes.length); back++) {
    const byte = bytes[bytes.length - back];
    if ((byte & 0xc0) === 0x80) continue;
    const need = byte >= 0xf0 ? 4 : byte >= 0xe0 ? 3 : byte >= 0xc0 ? 2 : 1;
    if (need > back) end = bytes.length - back;
    break;
  }
  return end;
}

class StringDecoder {
  constructor(encoding) {
    this.encoding = normalize(encoding);
    this.pending = new Uint8Array(0);
  }

  write(chunk) {
    if (typeof chunk === "string") return chunk;
    const bytes = new Uint8Array(this.pending.length + chunk.length);
    bytes.set(this.pending);
    bytes.set(chunk, this.pending.length);
    let end = bytes.length;
    if (this.encoding === "utf8") end = utf8Complete(bytes);
    else if (this.encoding === "utf16le") {
      end -= end % 2;
      if (end >= 2 && (bytes[end - 1] & 0xfc) === 0xd8) end -= 2;
    } else if (this.encoding === "base64" || this.encoding === "base64url") end -= end % 3;
    this.pending = bytes.slice(end);
    return Buffer.from(bytes.buffer, 0, end).toString(this.encoding);
  }

  end(chunk) {
    let out = chunk === undefined ? "" : this.write(chunk);
    if (this.pending.length) out += Buffer.from(this.pending).toString(this.encoding);
    this.pending = new Uint8Array(0);
    return out;
  }
}

export default { StringDecoder };
export { StringDecoder };
