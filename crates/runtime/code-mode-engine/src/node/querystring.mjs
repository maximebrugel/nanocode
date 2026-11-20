import { Buffer } from "node:buffer";

function unescape(text) {
  try {
    return decodeURIComponent(text);
  } catch {
    return unescapeBuffer(text).toString();
  }
}

function unescapeBuffer(text) {
  const out = [];
  for (let i = 0; i < text.length; i++) {
    const hex = text.slice(i + 1, i + 3);
    if (text[i] === "%" && /^[0-9a-fA-F]{2}$/.test(hex)) {
      out.push(parseInt(hex, 16));
      i += 2;
    } else {
      out.push(...Buffer.from(text[i]));
    }
  }
  return Buffer.from(out);
}

function escape(text) {
  return encodeURIComponent(text);
}

function stringifyPrimitive(value) {
  if (typeof value === "string") return value;
  if ((typeof value === "number" && Number.isFinite(value)) || typeof value === "bigint" || typeof value === "boolean") return String(value);
  return "";
}

function stringify(object, sep = "&", eq = "=", options) {
  const encode = options?.encodeURIComponent ?? escape;
  if (object === null || typeof object !== "object") return "";
  const out = [];
  for (const key of Object.keys(object)) {
    const value = object[key];
    const name = encode(stringifyPrimitive(key)) + eq;
    if (Array.isArray(value)) for (const item of value) out.push(name + encode(stringifyPrimitive(item)));
    else out.push(name + encode(stringifyPrimitive(value)));
  }
  return out.join(sep);
}

function parse(text, sep = "&", eq = "=", options) {
  const out = Object.create(null);
  if (typeof text !== "string" || text.length === 0) return out;
  const decode = options?.decodeURIComponent ?? unescape;
  const maxKeys = options?.maxKeys ?? 1000;
  let pairs = text.split(sep || "&");
  if (maxKeys > 0) pairs = pairs.slice(0, maxKeys);
  for (const pair of pairs) {
    if (pair === "") continue;
    const at = pair.indexOf(eq || "=");
    const rawKey = at === -1 ? pair : pair.slice(0, at);
    const rawValue = at === -1 ? "" : pair.slice(at + (eq || "=").length);
    const key = decode(rawKey.replace(/\+/g, " "));
    const value = decode(rawValue.replace(/\+/g, " "));
    if (!(key in out)) out[key] = value;
    else if (Array.isArray(out[key])) out[key].push(value);
    else out[key] = [out[key], value];
  }
  return out;
}

const decode = parse;
const encode = stringify;

export default { parse, stringify, escape, unescape, unescapeBuffer, decode, encode };
export { parse, stringify, escape, unescape, unescapeBuffer, decode, encode };
