import { EventEmitter } from "node:events";
import { statSync } from "node:fs";
import { Buffer } from "node:buffer";
import { workspace } from "node:internal/workspace";

const home = "/work";
let current = home;
let started;
let attached = false;

// A connected workspace supplies the starting directory and environment, the first time a cell
// looks at either.
function attach() {
  if (attached) return;
  const info = workspace();
  if (info === undefined) return;
  attached = true;
  if (typeof info.cwd === "string" && info.cwd.startsWith("/")) current = info.cwd;
  if (info.env !== null && typeof info.env === "object") {
    for (const [name, value] of Object.entries(info.env)) values[name] = String(value);
  }
}

const values = { HOME: home, PWD: home, TMPDIR: "/tmp", PATH: "" };
const environment = new Proxy(values, {
  get(target, name) {
    attach();
    return Reflect.get(target, name);
  },
  set(target, name, value) {
    attach();
    return Reflect.set(target, name, typeof name === "string" ? String(value) : value);
  },
  has(target, name) {
    attach();
    return Reflect.has(target, name);
  },
  deleteProperty(target, name) {
    attach();
    return Reflect.deleteProperty(target, name);
  },
  ownKeys(target) {
    attach();
    return Reflect.ownKeys(target);
  },
  getOwnPropertyDescriptor(target, name) {
    attach();
    return Reflect.getOwnPropertyDescriptor(target, name);
  },
  defineProperty(target, name, descriptor) {
    attach();
    return Reflect.defineProperty(target, name, descriptor);
  },
});

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function write(stream) {
  return function (chunk, encoding, callback) {
    if (typeof encoding === "function") callback = encoding;
    const text = typeof chunk === "string" ? chunk : Buffer.from(chunk.buffer ?? chunk, chunk.byteOffset, chunk.byteLength).toString(typeof encoding === "string" ? encoding : "utf8");
    const line = text.endsWith("\n") ? text.slice(0, -1) : text;
    if (typeof globalThis.text !== "function") throw new Error("process." + stream + ".write is only available while a cell runs");
    globalThis.text(line);
    if (typeof callback === "function") queueMicrotask(callback);
    return true;
  };
}

function stream(name, fd) {
  const out = new EventEmitter();
  out.fd = fd;
  out.isTTY = false;
  out.writable = true;
  out.write = write(name);
  out.end = (chunk, encoding, callback) => {
    if (chunk !== undefined && chunk !== null && typeof chunk !== "function") out.write(chunk, encoding);
    const done = [chunk, encoding, callback].find((item) => typeof item === "function");
    if (done) queueMicrotask(done);
    return out;
  };
  return out;
}

function highResolutionTime(previous) {
  const now = performance.now();
  let seconds = Math.floor(now / 1000);
  let nanos = Math.floor((now % 1000) * 1e6);
  if (previous !== undefined) {
    if (!Array.isArray(previous) || previous.length !== 2) {
      throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"time\" argument must be an instance of Array.");
    }
    seconds -= previous[0];
    nanos -= previous[1];
    if (nanos < 0) {
      seconds--;
      nanos += 1e9;
    }
  }
  return [seconds, nanos];
}
highResolutionTime.bigint = () => BigInt(Math.floor(performance.now() * 1e6));

class Process extends EventEmitter {}

const process = new Process();
Object.assign(process, {
  title: "node",
  version: "v24.0.0",
  versions: { node: "24.0.0" },
  release: { name: "node" },
  arch: "wasm32",
  platform: "linux",
  pid: 1,
  ppid: 0,
  argv: ["node"],
  argv0: "node",
  execArgv: [],
  execPath: "node",
  exitCode: undefined,
  env: environment,
  config: { variables: {} },
  features: {},
  stdout: stream("stdout", 1),
  stderr: stream("stderr", 2),
  cwd() {
    attach();
    return current;
  },
  chdir(directory) {
    attach();
    if (typeof directory !== "string") {
      throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"directory\" argument must be of type string. Received " + typeof directory);
    }
    const target = directory.startsWith("/") ? directory : current + "/" + directory;
    let stats;
    try {
      stats = statSync(target);
    } catch (error) {
      error.message = error.message.replace(/, stat /, ", chdir " + current + " -> ");
      error.syscall = "chdir";
      throw error;
    }
    if (!stats.isDirectory()) {
      const error = codedError(Error, "ENOTDIR", "ENOTDIR: not a directory, chdir " + current + " -> '" + directory + "'");
      error.errno = -20;
      error.syscall = "chdir";
      throw error;
    }
    const parts = [];
    for (const part of target.split("/")) {
      if (part === "" || part === ".") continue;
      if (part === "..") parts.pop();
      else parts.push(part);
    }
    current = "/" + parts.join("/");
  },
  nextTick(callback, ...args) {
    if (typeof callback !== "function") {
      throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"callback\" argument must be of type function. Received " + (callback === null ? "null" : typeof callback));
    }
    queueMicrotask(() => callback(...args));
  },
  hrtime: highResolutionTime,
  uptime() {
    started ??= performance.now();
    return (performance.now() - started) / 1000;
  },
  exit(code) {
    const status = code === undefined ? process.exitCode ?? 0 : Number(code);
    process.exitCode = status;
    process.emit("exit", status);
    if (status === 0 && typeof globalThis.exit === "function") globalThis.exit();
    throw new Error("process.exit(" + status + ")");
  },
  abort() {
    throw new Error("process.abort()");
  },
  kill(pid) {
    throw codedError(Error, "ENOSYS", "process.kill(" + pid + ") is not available: Code Mode cells cannot signal processes");
  },
  umask() {
    return 0o22;
  },
  emitWarning(warning, type, code) {
    const name = typeof type === "string" ? type : type?.type ?? warning?.name ?? "Warning";
    const message = typeof warning === "string" ? warning : warning?.message;
    const label = (code ?? type?.code) ? " [" + (code ?? type.code) + "]" : "";
    if (process.listenerCount("warning") > 0) {
      const error = warning instanceof Error ? warning : Object.assign(new Error(message), { name });
      process.emit("warning", error);
      return;
    }
    process.stderr.write("(node:1) " + (label ? label.trim() + " " : "") + name + ": " + message + "\n");
  },
  memoryUsage() {
    throw codedError(Error, "ENOSYS", "process.memoryUsage() is not available in Code Mode cells");
  },
  cpuUsage() {
    throw codedError(Error, "ENOSYS", "process.cpuUsage() is not available in Code Mode cells");
  },
  binding(name) {
    throw codedError(Error, "ERR_INVALID_MODULE", "No such module: " + name);
  },
});
process.memoryUsage.rss = process.memoryUsage;

export default process;
export const { env, argv, argv0, execArgv, execPath, platform, arch, pid, ppid, version, versions, release, stdout, stderr, cwd, chdir, nextTick, hrtime, uptime, exit, emitWarning, umask } = process;
