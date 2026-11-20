import process from "node:process";

const sep = "/";
const delimiter = ":";

function typeError(name, kind, value) {
  const error = new TypeError("The \"" + name + "\" argument must be of type " + kind + ". Received " + (value === null ? "null" : typeof value));
  error.code = "ERR_INVALID_ARG_TYPE";
  return error;
}

function check(value, name) {
  if (typeof value !== "string") throw typeError(name, "string", value);
}

function segments(path, absolute) {
  const out = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (out.length && out[out.length - 1] !== "..") out.pop();
      else if (!absolute) out.push("..");
    } else {
      out.push(part);
    }
  }
  return out;
}

function normalize(path) {
  check(path, "path");
  if (path === "") return ".";
  const absolute = path.startsWith("/");
  const trailing = path.endsWith("/");
  let body = segments(path, absolute).join("/");
  if (body === "" && !absolute) body = ".";
  if (body !== "" && trailing) body += "/";
  return absolute ? "/" + body : body;
}

function join(...parts) {
  for (const part of parts) check(part, "path");
  const joined = parts.filter((part) => part !== "").join("/");
  return joined === "" ? "." : normalize(joined);
}

function resolve(...parts) {
  let resolved = "";
  let absolute = false;
  for (let i = parts.length - 1; i >= -1 && !absolute; i--) {
    const part = i >= 0 ? parts[i] : process.cwd();
    check(part, "paths[" + i + "]");
    if (part === "") continue;
    resolved = part + "/" + resolved;
    absolute = part.startsWith("/");
  }
  const body = segments(resolved, absolute).join("/");
  return absolute ? "/" + body : body || ".";
}

function isAbsolute(path) {
  check(path, "path");
  return path.startsWith("/");
}

function relative(from, to) {
  check(from, "from");
  check(to, "to");
  if (from === to) return "";
  const a = segments(resolve(from), true);
  const b = segments(resolve(to), true);
  let common = 0;
  while (common < a.length && common < b.length && a[common] === b[common]) common++;
  return [...a.slice(common).map(() => ".."), ...b.slice(common)].join("/");
}

function dirname(path) {
  check(path, "path");
  if (path === "") return ".";
  const absolute = path.startsWith("/");
  let end = -1;
  let matched = true;
  for (let i = path.length - 1; i >= 1; i--) {
    if (path[i] === "/") {
      if (!matched) {
        end = i;
        break;
      }
    } else {
      matched = false;
    }
  }
  if (end === -1) return absolute ? "/" : ".";
  if (absolute && end === 1) return "//";
  return path.slice(0, end);
}

function basename(path, suffix) {
  check(path, "path");
  if (suffix !== undefined) check(suffix, "suffix");
  let end = path.length;
  while (end > 0 && path[end - 1] === "/") end--;
  const trimmed = path.slice(0, end);
  const base = trimmed.slice(trimmed.lastIndexOf("/") + 1);
  if (suffix !== undefined && path === suffix) return "";
  if (suffix !== undefined && end === 0 && path !== "" && suffix.length <= path.length) return path;
  if (suffix !== undefined && suffix !== base && base.endsWith(suffix)) {
    return base.slice(0, base.length - suffix.length);
  }
  return base;
}

function extname(path) {
  check(path, "path");
  const base = basename(path);
  const dot = base.lastIndexOf(".");
  if (dot <= 0) return "";
  if (dot === 1 && base === "..") return "";
  return base.slice(dot);
}

function parse(path) {
  check(path, "path");
  const root = path.startsWith("/") ? "/" : "";
  const base = basename(path);
  const ext = extname(path);
  let end = path.length;
  while (end > 1 && path[end - 1] === "/") end--;
  const trimmed = path.slice(0, end);
  const slash = trimmed.lastIndexOf("/");
  let dir = slash === -1 ? "" : trimmed.slice(0, slash);
  if (dir === "" && root) dir = "/";
  return { root, dir, base, ext, name: base.slice(0, base.length - ext.length) };
}

function format(object) {
  if (object === null || typeof object !== "object") throw typeError("pathObject", "object", object);
  const dir = object.dir || object.root;
  const ext = object.ext ? (object.ext.startsWith(".") ? object.ext : "." + object.ext) : "";
  const base = object.base || (object.name || "") + ext;
  if (!dir) return base;
  return dir === object.root ? dir + base : dir + "/" + base;
}

function toNamespacedPath(path) {
  return path;
}

function matchesGlob(path, pattern) {
  check(path, "path");
  check(pattern, "pattern");
  let source = "^";
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === "*") {
      if (pattern[i + 1] === "*") {
        source += ".*";
        i++;
        if (pattern[i + 1] === "/") i++;
      } else {
        source += "[^/]*";
      }
    } else if (c === "?") {
      source += "[^/]";
    } else {
      source += c.replace(/[.+^$(){}|[\]\\]/g, "\\$&");
    }
  }
  return new RegExp(source + "$").test(path);
}

const path = {
  sep,
  delimiter,
  normalize,
  join,
  resolve,
  isAbsolute,
  relative,
  dirname,
  basename,
  extname,
  parse,
  format,
  toNamespacedPath,
  matchesGlob,
};
path.posix = path;

export default path;
export const posix = path;
export {
  sep,
  delimiter,
  normalize,
  join,
  resolve,
  isAbsolute,
  relative,
  dirname,
  basename,
  extname,
  parse,
  format,
  toNamespacedPath,
  matchesGlob,
};
