import { resolve } from "node:path";
import querystring from "node:querystring";

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function urlClass() {
  if (typeof globalThis.URL !== "function") throw new Error("URL is not available in this runtime");
  return globalThis.URL;
}

function fileURLToPath(value) {
  let href;
  if (typeof value === "string") href = value;
  else if (value !== null && typeof value === "object" && typeof value.href === "string") href = value.href;
  else throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"path\" argument must be of type string or an instance of URL. Received " + (value === null ? "null" : typeof value));
  const match = /^file:(?:\/\/([^/?#]*))?([^?#]*)/i.exec(href);
  if (match === null) throw codedError(TypeError, "ERR_INVALID_URL_SCHEME", "The URL must be of scheme file");
  const host = match[1] ?? "";
  if (host !== "" && host.toLowerCase() !== "localhost") {
    throw codedError(TypeError, "ERR_INVALID_FILE_URL_HOST", "File URL host must be \"localhost\" or empty on linux");
  }
  const pathname = match[2] || "/";
  if (/%2f/i.test(pathname)) throw codedError(TypeError, "ERR_INVALID_FILE_URL_PATH", "File URL path must not include encoded / characters");
  return decodeURIComponent(pathname);
}

function encodePath(path) {
  return path.replace(/[%\\\n\r\t#?]/g, (c) => "%" + c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0"));
}

function pathToFileURL(path) {
  let absolute = resolve(path);
  if (path.endsWith("/") && !absolute.endsWith("/")) absolute += "/";
  const URL = urlClass();
  return new URL("file://" + encodePath(absolute));
}

function Url() {
  this.protocol = null;
  this.slashes = null;
  this.auth = null;
  this.host = null;
  this.port = null;
  this.hostname = null;
  this.hash = null;
  this.search = null;
  this.query = null;
  this.pathname = null;
  this.path = null;
  this.href = null;
}

const SLASHED = new Set(["http:", "https:", "ftp:", "gopher:", "file:", "ws:", "wss:"]);

function parse(text, parseQueryString, slashesDenoteHost) {
  if (text instanceof Url) return text;
  if (typeof text !== "string") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"url\" argument must be of type string. Received " + (text === null ? "null" : typeof text));
  const out = new Url();
  let rest = text.trim();
  const hashAt = rest.indexOf("#");
  if (hashAt !== -1) {
    out.hash = rest.slice(hashAt);
    rest = rest.slice(0, hashAt);
  }
  const queryAt = rest.indexOf("?");
  if (queryAt !== -1) {
    out.search = rest.slice(queryAt);
    out.query = rest.slice(queryAt + 1);
    rest = rest.slice(0, queryAt);
  } else if (parseQueryString) {
    out.search = "";
    out.query = {};
  }
  if (parseQueryString && typeof out.query === "string") out.query = querystring.parse(out.query);
  const protocol = /^([a-z0-9.+-]+:)/i.exec(rest);
  if (protocol) {
    out.protocol = protocol[1].toLowerCase();
    rest = rest.slice(protocol[1].length);
  }
  const slashes = rest.startsWith("//") && (protocol !== null || slashesDenoteHost) && out.protocol !== "javascript:";
  if (slashes) {
    out.slashes = true;
    rest = rest.slice(2);
  }
  if (slashes || (out.protocol !== null && !SLASHED.has(out.protocol) && out.protocol !== "javascript:")) {
    const end = rest.search(/[/\\]/);
    let authority = end === -1 ? rest : rest.slice(0, end);
    rest = end === -1 ? "" : rest.slice(end);
    const at = authority.lastIndexOf("@");
    if (at !== -1) {
      out.auth = decodeURIComponent(authority.slice(0, at));
      authority = authority.slice(at + 1);
    }
    const port = /:(\d*)$/.exec(authority);
    if (port) {
      if (port[1] !== "") out.port = port[1];
      authority = authority.slice(0, port.index);
    }
    out.hostname = authority.toLowerCase();
    out.host = out.hostname + (out.port !== null ? ":" + out.port : "");
    if (rest === "" && SLASHED.has(out.protocol)) rest = "/";
  }
  const escape = (part) => part === null ? null : part.replace(/[ "'<>\x60\t\n\r{}|\\^]/g, (c) => "%" + c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0"));
  rest = escape(rest);
  out.hash = escape(out.hash);
  if (typeof out.search === "string") out.search = escape(out.search);
  if (typeof out.query === "string") out.query = escape(out.query);
  if (rest !== "") out.pathname = rest;
  else if (out.slashes && out.hostname) out.pathname = "/";
  const search = typeof out.search === "string" ? out.search : "";
  out.path = out.pathname === null && search === "" ? null : (out.pathname ?? "") + search;
  out.href = format(out);
  return out;
}

function format(value, options) {
  if (typeof value === "string") value = parse(value);
  if (typeof globalThis.URL === "function" && value instanceof globalThis.URL) {
    if (options === undefined) return value.href;
    const copy = new globalThis.URL(value.href);
    if (options.auth === false) {
      copy.username = "";
      copy.password = "";
    }
    if (options.fragment === false) copy.hash = "";
    if (options.search === false) copy.search = "";
    return copy.href;
  }
  const protocol = value.protocol ? (value.protocol.endsWith(":") ? value.protocol : value.protocol + ":") : "";
  let host = "";
  if (value.host) host = value.host;
  else if (value.hostname) host = value.hostname + (value.port ? ":" + value.port : "");
  if (host && value.auth) host = encodeURIComponent(value.auth).replace(/%3A/i, ":") + "@" + host;
  let search = value.search ?? "";
  if (!search && value.query && typeof value.query === "object") {
    const query = querystring.stringify(value.query);
    if (query) search = "?" + query;
  }
  if (search && !search.startsWith("?")) search = "?" + search;
  let hash = value.hash ?? "";
  if (hash && !hash.startsWith("#")) hash = "#" + hash;
  let pathname = value.pathname ?? "";
  const slashes = value.slashes || SLASHED.has(protocol);
  if (slashes && (value.slashes || host)) {
    if (pathname && !pathname.startsWith("/")) pathname = "/" + pathname;
    host = "//" + host;
  }
  return protocol + host + pathname.replace(/[?#]/g, encodeURIComponent) + search.replace("#", "%23") + hash;
}

function resolveUrl(from, to) {
  const URL = urlClass();
  const base = new URL(from, "resolve://");
  const resolved = new URL(to, base);
  if (base.protocol === "resolve:") {
    const { pathname, search, hash } = resolved;
    return pathname + search + hash;
  }
  return resolved.toString();
}

function domainToASCII(domain) {
  try {
    return new (urlClass())("http://" + domain).hostname;
  } catch {
    return "";
  }
}

function domainToUnicode(domain) {
  return domain;
}

function urlToHttpOptions(value) {
  return {
    protocol: value.protocol,
    hostname: value.hostname.startsWith("[") ? value.hostname.slice(1, -1) : value.hostname,
    hash: value.hash,
    search: value.search,
    pathname: value.pathname,
    path: value.pathname + value.search,
    href: value.href,
    ...(value.port !== "" ? { port: Number(value.port) } : {}),
    ...(value.username || value.password ? { auth: decodeURIComponent(value.username) + ":" + decodeURIComponent(value.password) } : {}),
  };
}

const URL = globalThis.URL;
const URLSearchParams = globalThis.URLSearchParams;

export default { URL, URLSearchParams, Url, parse, format, resolve: resolveUrl, fileURLToPath, pathToFileURL, domainToASCII, domainToUnicode, urlToHttpOptions };
export { URL, URLSearchParams, Url, parse, format, resolveUrl as resolve, fileURLToPath, pathToFileURL, domainToASCII, domainToUnicode, urlToHttpOptions };
