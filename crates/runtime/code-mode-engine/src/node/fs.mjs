import process from "node:process";
import { Buffer } from "node:buffer";
import { resolve as resolvePath } from "node:path";
import { connected, ask } from "node:internal/workspace";

const S_IFMT = 0o170000;
const S_IFDIR = 0o040000;
const S_IFREG = 0o100000;
const constants = {
  F_OK: 0,
  R_OK: 4,
  W_OK: 2,
  X_OK: 1,
  COPYFILE_EXCL: 1,
  COPYFILE_FICLONE: 2,
  COPYFILE_FICLONE_FORCE: 4,
  O_RDONLY: 0,
  O_WRONLY: 1,
  O_RDWR: 2,
  O_CREAT: 64,
  O_EXCL: 128,
  O_TRUNC: 512,
  O_APPEND: 1024,
  S_IFMT,
  S_IFREG,
  S_IFDIR,
  UV_DIRENT_FILE: 1,
  UV_DIRENT_DIR: 2,
};
const ERRNO = { ENOENT: -2, EBADF: -9, EEXIST: -17, ENOTDIR: -20, EISDIR: -21, EINVAL: -22, ENOSYS: -38, ENOTEMPTY: -39 };
const DESCRIPTIONS = {
  ENOENT: "no such file or directory",
  EBADF: "bad file descriptor",
  EEXIST: "file already exists",
  ENOTDIR: "not a directory",
  EISDIR: "illegal operation on a directory",
  EINVAL: "invalid argument",
  ENOSYS: "function not implemented",
  ENOTEMPTY: "directory not empty",
};

let inodes = 0;
const descriptors = new Map();
let nextDescriptor = 3;

function now() {
  return Date.now();
}

function directory(mode = 0o777, time = now()) {
  return { kind: "dir", entries: new Map(), mode: S_IFDIR | (mode & 0o7777 & ~0o022), ino: ++inodes, nlink: 2, atime: time, mtime: time, ctime: time, birthtime: time };
}

function file(data, mode = 0o666) {
  const time = now();
  return { kind: "file", data, mode: S_IFREG | (mode & 0o7777 & ~0o022), ino: ++inodes, nlink: 1, atime: time, mtime: time, ctime: time, birthtime: time };
}

const root = directory(0o755, 0);
root.entries.set("tmp", directory(0o1777, 0));
root.entries.set("work", directory(0o755, 0));

// With a connected workspace the tree mirrors it: directories fault their entries in from the
// workspace as they are looked up or listed, files load their bytes when first read, and every
// change made through this module is written back (see mirrored below).
function under(path, name) {
  return path === "/" ? "/" + name : path + "/" + name;
}

function remoteFile(path, data) {
  const node = file(undefined);
  delete node.data;
  if (data !== undefined) node.data = data;
  else {
    Object.defineProperty(node, "data", {
      configurable: true,
      enumerable: true,
      get() {
        const answer = ask({ op: "read", path });
        const bytes = answer.kind === "file" ? new Uint8Array(Buffer.from(answer.data ?? "", "base64")) : new Uint8Array(0);
        Object.defineProperty(node, "data", { value: bytes, writable: true, configurable: true, enumerable: true });
        return bytes;
      },
      set(value) {
        Object.defineProperty(node, "data", { value, writable: true, configurable: true, enumerable: true });
      },
    });
  }
  return node;
}

function remoteDirectory(path) {
  const node = directory(0o755);
  node.entries = new RemoteEntries(path);
  return node;
}

class RemoteEntries extends Map {
  #path;
  #listed = false;
  #gone = new Set();

  constructor(path) {
    super();
    this.#path = path;
  }

  #fault(name) {
    if (this.#listed || typeof name !== "string" || super.has(name) || this.#gone.has(name)) return;
    const path = under(this.#path, name);
    const answer = ask({ op: "read", path });
    if (answer.kind === "file") super.set(name, remoteFile(path, new Uint8Array(Buffer.from(answer.data ?? "", "base64"))));
    else if (answer.kind === "dir") super.set(name, remoteDirectory(path));
  }

  #list() {
    if (this.#listed) return;
    this.#listed = true;
    for (const [name, kind] of ask({ op: "list", path: this.#path }).entries ?? []) {
      if (super.has(name) || this.#gone.has(name)) continue;
      const path = under(this.#path, name);
      super.set(name, kind === "dir" ? remoteDirectory(path) : remoteFile(path));
    }
  }

  get(name) {
    this.#fault(name);
    return super.get(name);
  }

  has(name) {
    this.#fault(name);
    return super.has(name);
  }

  set(name, value) {
    this.#gone.delete(name);
    return super.set(name, value);
  }

  delete(name) {
    this.#gone.add(name);
    return super.delete(name);
  }

  get size() {
    this.#list();
    return super.size;
  }

  keys() {
    this.#list();
    return super.keys();
  }

  values() {
    this.#list();
    return super.values();
  }

  entries() {
    this.#list();
    return super.entries();
  }

  forEach(callback, self) {
    this.#list();
    return super.forEach(callback, self);
  }

  [Symbol.iterator]() {
    return this.entries();
  }
}

let attached = false;

function attach() {
  if (attached || !connected()) return;
  attached = true;
  root.entries = new RemoteEntries("/");
}

function fsError(code, syscall, path, dest) {
  let message = code + ": " + DESCRIPTIONS[code] + ", " + syscall;
  if (path !== undefined) message += " '" + path + "'";
  if (dest !== undefined) message += " -> '" + dest + "'";
  const error = new Error(message);
  error.errno = ERRNO[code];
  error.code = code;
  error.syscall = syscall;
  if (path !== undefined) error.path = path;
  if (dest !== undefined) error.dest = dest;
  return error;
}

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function unsupported(name) {
  return function () {
    const error = new Error("ENOSYS: function not implemented, fs." + name + " is not supported by the in-memory file system of Code Mode cells");
    error.errno = ERRNO.ENOSYS;
    error.code = "ENOSYS";
    error.syscall = name;
    throw error;
  };
}

function toPath(value, name = "path") {
  if (typeof value === "string") {
    if (value.includes("\0")) throw codedError(TypeError, "ERR_INVALID_ARG_VALUE", "The argument '" + name + "' must be a string, Uint8Array, or URL without null bytes. Received " + JSON.stringify(value));
    return value;
  }
  if (value instanceof Uint8Array) return Buffer.from(value).toString();
  if (value !== null && typeof value === "object" && typeof value.href === "string" && typeof value.protocol === "string") {
    if (value.protocol !== "file:") throw codedError(TypeError, "ERR_INVALID_URL_SCHEME", "The URL must be of scheme file");
    return decodeURIComponent(value.pathname);
  }
  throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"" + name + "\" argument must be of type string or an instance of Buffer or URL. Received " + (value === null ? "null" : typeof value));
}

function parts(absolute) {
  return absolute.split("/").filter((part) => part !== "" && part !== ".");
}

function lookup(path, syscall) {
  attach();
  const shown = toPath(path);
  const absolute = resolvePath(shown);
  let node = root;
  for (const part of parts(absolute)) {
    if (node.kind !== "dir") throw fsError("ENOTDIR", syscall, shown);
    node = node.entries.get(part);
    if (node === undefined) throw fsError("ENOENT", syscall, shown);
  }
  if (node.kind !== "dir" && /\/\.?$/.test(shown)) throw fsError("ENOTDIR", syscall, shown);
  return node;
}

function find(path) {
  try {
    return lookup(path, "stat");
  } catch {
    return undefined;
  }
}

function parentOf(path, syscall) {
  attach();
  const shown = toPath(path);
  const names = parts(resolvePath(shown));
  const name = names.pop();
  let node = root;
  for (const part of names) {
    if (node.kind !== "dir") throw fsError("ENOTDIR", syscall, shown);
    node = node.entries.get(part);
    if (node === undefined) throw fsError("ENOENT", syscall, shown);
  }
  if (node.kind !== "dir") throw fsError("ENOTDIR", syscall, shown);
  return { parent: node, name, shown };
}

function touch(node) {
  const time = now();
  node.mtime = time;
  node.ctime = time;
}

function encodingOf(options, fallback) {
  if (typeof options === "string") return options;
  if (options !== null && typeof options === "object" && options.encoding !== undefined) return options.encoding;
  return fallback;
}

function bytesOf(data, encoding) {
  if (typeof data === "string") return Buffer.from(data, encoding ?? "utf8");
  if (data instanceof Uint8Array) return Uint8Array.from(data);
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength));
  throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"data\" argument must be of type string or an instance of Buffer, TypedArray, or DataView. Received " + (data === null ? "null" : typeof data));
}

function decode(bytes, encoding) {
  const buffer = Buffer.from(bytes);
  return encoding === undefined || encoding === null || encoding === "buffer" ? buffer : buffer.toString(encoding);
}

class Stats {
  constructor(node) {
    this.dev = 1;
    this.mode = node.mode;
    this.nlink = node.nlink;
    this.uid = 0;
    this.gid = 0;
    this.rdev = 0;
    this.blksize = 4096;
    this.ino = node.ino;
    this.size = node.kind === "file" ? node.data.length : 4096;
    this.blocks = Math.ceil(this.size / 512);
    this.atimeMs = node.atime;
    this.mtimeMs = node.mtime;
    this.ctimeMs = node.ctime;
    this.birthtimeMs = node.birthtime;
    this.atime = new Date(node.atime);
    this.mtime = new Date(node.mtime);
    this.ctime = new Date(node.ctime);
    this.birthtime = new Date(node.birthtime);
  }

  isFile() {
    return (this.mode & S_IFMT) === S_IFREG;
  }

  isDirectory() {
    return (this.mode & S_IFMT) === S_IFDIR;
  }

  isSymbolicLink() {
    return false;
  }

  isBlockDevice() {
    return false;
  }

  isCharacterDevice() {
    return false;
  }

  isFIFO() {
    return false;
  }

  isSocket() {
    return false;
  }
}

class Dirent {
  constructor(name, node, parentPath) {
    this.name = name;
    this.parentPath = parentPath;
    this.path = parentPath;
    Object.defineProperty(this, "kind", { value: node.kind, enumerable: false });
  }

  isFile() {
    return this.kind === "file";
  }

  isDirectory() {
    return this.kind === "dir";
  }

  isSymbolicLink() {
    return false;
  }

  isBlockDevice() {
    return false;
  }

  isCharacterDevice() {
    return false;
  }

  isFIFO() {
    return false;
  }

  isSocket() {
    return false;
  }
}

function existsSync(path) {
  try {
    return find(path) !== undefined;
  } catch {
    return false;
  }
}

function accessSync(path) {
  lookup(path, "access");
}

function statSync(path, options) {
  let node;
  try {
    node = lookup(path, "stat");
  } catch (error) {
    if (options?.throwIfNoEntry === false && (error.code === "ENOENT" || error.code === "ENOTDIR")) return undefined;
    throw error;
  }
  return new Stats(node);
}

function lstatSync(path, options) {
  let node;
  try {
    node = lookup(path, "lstat");
  } catch (error) {
    if (options?.throwIfNoEntry === false && (error.code === "ENOENT" || error.code === "ENOTDIR")) return undefined;
    throw error;
  }
  return new Stats(node);
}

function descriptor(fd, syscall) {
  const entry = descriptors.get(fd);
  if (entry === undefined) throw fsError("EBADF", syscall);
  return entry;
}

function fstatSync(fd) {
  return new Stats(descriptor(fd, "fstat").node);
}

function readFileSync(path, options) {
  const encoding = encodingOf(options);
  if (typeof path === "number") {
    const entry = descriptor(path, "read");
    const bytes = entry.node.data.subarray(entry.position);
    entry.position = entry.node.data.length;
    return decode(bytes, encoding);
  }
  const node = lookup(path, "open");
  if (node.kind === "dir") throw fsError("EISDIR", "read");
  node.atime = now();
  return decode(node.data, encoding);
}

function flagsOf(flag, fallback) {
  const value = flag ?? fallback;
  if (typeof value === "number") {
    return {
      read: (value & 3) !== 1,
      write: (value & 3) !== 0,
      create: (value & constants.O_CREAT) !== 0,
      exclusive: (value & constants.O_EXCL) !== 0,
      truncate: (value & constants.O_TRUNC) !== 0,
      append: (value & constants.O_APPEND) !== 0,
    };
  }
  const known = {
    r: [true, false, false, false, false, false],
    "r+": [true, true, false, false, false, false],
    rs: [true, false, false, false, false, false],
    "rs+": [true, true, false, false, false, false],
    w: [false, true, true, false, true, false],
    wx: [false, true, true, true, true, false],
    "w+": [true, true, true, false, true, false],
    "wx+": [true, true, true, true, true, false],
    a: [false, true, true, false, false, true],
    ax: [false, true, true, true, false, true],
    "a+": [true, true, true, false, false, true],
    "ax+": [true, true, true, true, false, true],
    as: [false, true, true, false, false, true],
    "as+": [true, true, true, false, false, true],
  }[String(value).replace("xs", "sx").replace(/^([rwa])s(x)?/, "$1$2")] ?? undefined;
  if (known === undefined) throw codedError(TypeError, "ERR_INVALID_ARG_VALUE", "The argument 'flags' is invalid. Received '" + value + "'");
  const [read, write, create, exclusive, truncate, append] = known;
  return { read, write, create, exclusive, truncate, append };
}

function openNode(path, flags, mode, syscall) {
  const { parent, name, shown } = parentOf(path, syscall);
  if (name === undefined) {
    if (flags.write) throw fsError("EISDIR", syscall, shown);
    return root;
  }
  let node = parent.entries.get(name);
  if (node !== undefined) {
    if (flags.create && flags.exclusive) throw fsError("EEXIST", syscall, shown);
    if (node.kind === "dir") {
      if (flags.write) throw fsError("EISDIR", syscall, shown);
      return node;
    }
    if (/\/\.?$/.test(shown)) throw fsError("ENOTDIR", syscall, shown);
    if (flags.truncate) {
      node.data = new Uint8Array(0);
      touch(node);
    }
    return node;
  }
  if (!flags.create) throw fsError("ENOENT", syscall, shown);
  if (shown.endsWith("/")) throw fsError("EISDIR", syscall, shown);
  node = file(new Uint8Array(0), typeof mode === "number" ? mode : 0o666);
  parent.entries.set(name, node);
  touch(parent);
  return node;
}

function writeFileSync(path, data, options) {
  const encoding = encodingOf(options, "utf8");
  const bytes = bytesOf(data, encoding);
  if (typeof path === "number") {
    writeAt(descriptor(path, "write"), bytes, null);
    return;
  }
  const flags = flagsOf(options?.flag, "w");
  const node = openNode(path, flags, options?.mode, "open");
  if (node.kind === "dir") throw fsError("EISDIR", "open", toPath(path));
  node.data = flags.append ? concat(node.data, bytes) : bytes;
  touch(node);
}

function appendFileSync(path, data, options) {
  const encoding = encodingOf(options, "utf8");
  const flag = typeof options === "object" && options?.flag !== undefined ? options.flag : "a";
  writeFileSync(path, bytesOf(data, encoding), { flag, mode: options?.mode });
}

function concat(a, b) {
  const out = new Uint8Array(a.length + b.length);
  out.set(a, 0);
  out.set(b, a.length);
  return out;
}

function mkdirSync(path, options) {
  const recursive = typeof options === "object" && options !== null && options.recursive === true;
  const mode = typeof options === "number" ? options : typeof options === "string" ? parseInt(options, 8) : options?.mode ?? 0o777;
  const shown = toPath(path);
  if (!recursive) {
    const { parent, name } = parentOf(path, "mkdir");
    if (name === undefined || parent.entries.has(name)) throw fsError("EEXIST", "mkdir", shown);
    parent.entries.set(name, directory(mode));
    parent.nlink++;
    touch(parent);
    return undefined;
  }
  attach();
  let node = root;
  let current = "";
  let first;
  for (const part of parts(resolvePath(shown))) {
    current += "/" + part;
    if (node.kind !== "dir") throw fsError("ENOTDIR", "mkdir", shown);
    let next = node.entries.get(part);
    if (next === undefined) {
      next = directory(mode);
      node.entries.set(part, next);
      node.nlink++;
      touch(node);
      first ??= current;
    }
    node = next;
  }
  if (node.kind !== "dir") throw fsError("EEXIST", "mkdir", shown);
  return first;
}

function mkdtempSync(prefix) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (;;) {
    let suffix = "";
    for (let i = 0; i < 6; i++) suffix += alphabet[Math.floor(Math.random() * alphabet.length)];
    const path = toPath(prefix, "prefix") + suffix;
    if (!existsSync(path)) {
      mkdirSync(path, 0o700);
      return path;
    }
  }
}

function readdirSync(path, options) {
  const shown = toPath(path);
  const node = lookup(path, "scandir");
  if (node.kind !== "dir") throw fsError("ENOTDIR", "scandir", shown);
  const encoding = encodingOf(options, "utf8");
  const types = typeof options === "object" && options?.withFileTypes === true;
  const recursive = typeof options === "object" && options?.recursive === true;
  const out = [];
  const visit = (dir, prefix, parentPath) => {
    for (const name of [...dir.entries.keys()].sort()) {
      const child = dir.entries.get(name);
      const relative = prefix ? prefix + "/" + name : name;
      if (types) out.push(new Dirent(name, child, parentPath));
      else out.push(encoding === "buffer" ? Buffer.from(relative) : relative);
      if (recursive && child.kind === "dir") visit(child, relative, parentPath + "/" + name);
    }
  };
  visit(node, "", shown);
  return out;
}

function rmdirSync(path, options) {
  const { parent, name, shown } = parentOf(path, "rmdir");
  const node = name === undefined ? root : parent.entries.get(name);
  if (node === undefined) throw fsError("ENOENT", "rmdir", shown);
  if (node.kind !== "dir") throw fsError("ENOTDIR", "rmdir", shown);
  if (name === undefined) throw fsError("EINVAL", "rmdir", shown);
  if (node.entries.size > 0 && options?.recursive !== true) throw fsError("ENOTEMPTY", "rmdir", shown);
  parent.entries.delete(name);
  parent.nlink--;
  touch(parent);
}

function rmSync(path, options) {
  const shown = toPath(path);
  let parent;
  let name;
  try {
    ({ parent, name } = parentOf(path, "lstat"));
  } catch (error) {
    if (options?.force === true && (error.code === "ENOENT" || error.code === "ENOTDIR")) return;
    throw error;
  }
  const node = name === undefined ? root : parent.entries.get(name);
  if (node === undefined) {
    if (options?.force === true) return;
    throw fsError("ENOENT", "lstat", shown);
  }
  if (node.kind === "dir") {
    if (options?.recursive !== true) {
      const error = new Error("Path is a directory: rm returned EISDIR (is a directory) " + shown);
      error.name = "SystemError";
      error.code = "ERR_FS_EISDIR";
      error.errno = ERRNO.EISDIR;
      error.syscall = "rm";
      error.path = shown;
      throw error;
    }
    if (name === undefined) throw fsError("EINVAL", "rm", shown);
    parent.nlink--;
  }
  parent.entries.delete(name);
  touch(parent);
}

function unlinkSync(path) {
  const { parent, name, shown } = parentOf(path, "unlink");
  const node = name === undefined ? root : parent.entries.get(name);
  if (node === undefined) throw fsError("ENOENT", "unlink", shown);
  if (node.kind === "dir") throw fsError("EISDIR", "unlink", shown);
  parent.entries.delete(name);
  node.nlink--;
  touch(parent);
}

function contains(ancestor, node) {
  if (ancestor === node) return true;
  if (ancestor.kind !== "dir") return false;
  for (const child of ancestor.entries.values()) if (contains(child, node)) return true;
  return false;
}

function renameSync(from, to) {
  const source = parentOf(from, "rename");
  const target = parentOf(to, "rename");
  const node = source.name === undefined ? root : source.parent.entries.get(source.name);
  if (node === undefined) throw fsError("ENOENT", "rename", source.shown, target.shown);
  if (target.name === undefined) throw fsError("EISDIR", "rename", source.shown, target.shown);
  if (node.kind === "dir" && contains(node, target.parent)) throw fsError("EINVAL", "rename", source.shown, target.shown);
  const existing = target.parent.entries.get(target.name);
  if (existing !== undefined && existing !== node) {
    if (existing.kind === "dir" && node.kind !== "dir") throw fsError("EISDIR", "rename", source.shown, target.shown);
    if (existing.kind !== "dir" && node.kind === "dir") throw fsError("ENOTDIR", "rename", source.shown, target.shown);
    if (existing.kind === "dir" && existing.entries.size > 0) throw fsError("ENOTEMPTY", "rename", source.shown, target.shown);
  }
  source.parent.entries.delete(source.name);
  target.parent.entries.set(target.name, node);
  touch(source.parent);
  touch(target.parent);
  node.ctime = now();
}

function copyFileSync(from, to, mode = 0) {
  const shownFrom = toPath(from);
  const node = lookup(from, "copyfile");
  if (node.kind === "dir") throw fsError("EISDIR", "copyfile", shownFrom, toPath(to));
  const { parent, name, shown } = parentOf(to, "copyfile");
  const existing = parent.entries.get(name);
  if (existing !== undefined && (mode & constants.COPYFILE_EXCL)) throw fsError("EEXIST", "copyfile", shownFrom, shown);
  if (existing?.kind === "dir") throw fsError("EISDIR", "copyfile", shownFrom, shown);
  if (existing !== undefined) {
    existing.data = Uint8Array.from(node.data);
    touch(existing);
  } else {
    parent.entries.set(name, file(Uint8Array.from(node.data), node.mode));
    touch(parent);
  }
}

function cpSync(from, to, options) {
  const node = lookup(from, "cp");
  if (node.kind === "dir") {
    if (options?.recursive !== true) {
      const error = new Error("Recursive option is required to copy a directory: " + toPath(from));
      error.name = "SystemError";
      error.code = "ERR_FS_EISDIR";
      throw error;
    }
    if (!existsSync(to)) mkdirSync(to, { recursive: true });
    for (const name of node.entries.keys()) cpSync(toPath(from) + "/" + name, toPath(to) + "/" + name, options);
    return;
  }
  if (existsSync(to) && options?.force === false) {
    if (options?.errorOnExist) throw fsError("EEXIST", "cp", toPath(to));
    return;
  }
  copyFileSync(from, to);
}

function truncateSync(path, length = 0) {
  if (typeof path === "number") return ftruncateSync(path, length);
  const node = lookup(path, "open");
  if (node.kind === "dir") throw fsError("EISDIR", "open", toPath(path));
  resize(node, length);
}

function resize(node, length) {
  const out = new Uint8Array(length);
  out.set(node.data.subarray(0, length));
  node.data = out;
  touch(node);
}

function ftruncateSync(fd, length = 0) {
  resize(descriptor(fd, "ftruncate").node, length);
}

function timeOf(value) {
  if (value instanceof Date) return value.getTime();
  if (typeof value === "string") return Number(value) * 1000;
  return value * 1000;
}

function utimesSync(path, atime, mtime) {
  const node = lookup(path, "utime");
  node.atime = timeOf(atime);
  node.mtime = timeOf(mtime);
}

function futimesSync(fd, atime, mtime) {
  const node = descriptor(fd, "futime").node;
  node.atime = timeOf(atime);
  node.mtime = timeOf(mtime);
}

function chmodSync(path, mode) {
  const node = lookup(path, "chmod");
  node.mode = (node.mode & S_IFMT) | ((typeof mode === "string" ? parseInt(mode, 8) : mode) & 0o7777);
  node.ctime = now();
}

function fchmodSync(fd, mode) {
  const node = descriptor(fd, "fchmod").node;
  node.mode = (node.mode & S_IFMT) | ((typeof mode === "string" ? parseInt(mode, 8) : mode) & 0o7777);
}

function chownSync(path) {
  lookup(path, "chown");
}

function realpathSync(path) {
  lookup(path, "realpath");
  return resolvePath(toPath(path));
}
realpathSync.native = realpathSync;

function readlinkSync(path) {
  lookup(path, "readlink");
  throw fsError("EINVAL", "readlink", toPath(path));
}

function linkSync(existing, path) {
  const node = lookup(existing, "link");
  if (node.kind === "dir") throw Object.assign(fsError("EISDIR", "link", toPath(existing), toPath(path)), { code: "EPERM", errno: -1 });
  const { parent, name, shown } = parentOf(path, "link");
  if (parent.entries.has(name)) throw fsError("EEXIST", "link", toPath(existing), shown);
  parent.entries.set(name, node);
  node.nlink++;
  touch(parent);
}

function openSync(path, flags = "r", mode) {
  const parsed = flagsOf(flags, "r");
  const node = openNode(path, parsed, mode, "open");
  const fd = nextDescriptor++;
  descriptors.set(fd, { node, position: 0, flags: parsed, path: toPath(path), absolute: resolvePath(toPath(path)) });
  return fd;
}

function closeSync(fd) {
  descriptor(fd, "close");
  descriptors.delete(fd);
}

function fsyncSync(fd) {
  descriptor(fd, "fsync");
}

function readSync(fd, buffer, offsetOrOptions, length, position) {
  const entry = descriptor(fd, "read");
  let offset = offsetOrOptions;
  if (offsetOrOptions !== null && typeof offsetOrOptions === "object") {
    ({ offset = 0, length = buffer.byteLength - offset, position = null } = offsetOrOptions);
  }
  offset ??= 0;
  length ??= buffer.byteLength - offset;
  if (entry.node.kind === "dir") throw fsError("EISDIR", "read");
  const start = typeof position === "number" || typeof position === "bigint" ? Number(position) : entry.position;
  const bytes = entry.node.data.subarray(start, start + length);
  new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength).set(bytes, offset);
  if (!(typeof position === "number" || typeof position === "bigint")) entry.position += bytes.length;
  return bytes.length;
}

function writeAt(entry, bytes, position) {
  if (!entry.flags.write) throw fsError("EBADF", "write");
  const node = entry.node;
  const start = entry.flags.append ? node.data.length : typeof position === "number" ? position : entry.position;
  if (start + bytes.length > node.data.length) {
    const grown = new Uint8Array(start + bytes.length);
    grown.set(node.data);
    node.data = grown;
  }
  node.data.set(bytes, start);
  if (typeof position !== "number") entry.position = start + bytes.length;
  touch(node);
  return bytes.length;
}

function writeSync(fd, data, offsetOrPosition, lengthOrEncoding, position) {
  const entry = descriptor(fd, "write");
  if (typeof data === "string") {
    return writeAt(entry, Buffer.from(data, typeof lengthOrEncoding === "string" ? lengthOrEncoding : "utf8"), typeof offsetOrPosition === "number" ? offsetOrPosition : null);
  }
  const bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  const offset = offsetOrPosition ?? 0;
  const length = typeof lengthOrEncoding === "number" ? lengthOrEncoding : bytes.length - offset;
  return writeAt(entry, bytes.subarray(offset, offset + length), position);
}

// Writes a change made through this module back to the connected workspace. Calls made inside
// another fs call (cpSync copying files, rmSync removing them) leave it to the outermost one.
let depth = 0;

function mirrored(fn, after) {
  return function (...args) {
    if (!connected()) return fn.apply(this, args);
    attach();
    const before = after.length > 2 ? args.map((arg) => (typeof arg === "number" ? descriptors.get(arg) : undefined)) : undefined;
    depth++;
    let result;
    try {
      result = fn.apply(this, args);
    } finally {
      depth--;
    }
    if (depth === 0) after(args, result, before);
    return result;
  };
}

function mirror(path, deep = false) {
  const absolute = resolvePath(path);
  const node = find(absolute);
  if (node === undefined) ask({ op: "remove", path: absolute });
  else if (node.kind === "dir") {
    ask({ op: "mkdir", path: absolute });
    if (deep) for (const name of node.entries.keys()) mirror(under(absolute, name), true);
  } else ask({ op: "write", path: absolute, data: Buffer.from(node.data).toString("base64") });
}

function mirrorTarget(target, entry) {
  if (typeof target === "number") {
    entry ??= descriptors.get(target);
    if (entry?.flags.write) mirror(entry.absolute);
  } else mirror(toPath(target));
}

writeFileSync = mirrored(writeFileSync, ([target]) => mirrorTarget(target));
appendFileSync = mirrored(appendFileSync, ([target]) => mirrorTarget(target));
truncateSync = mirrored(truncateSync, ([target]) => mirrorTarget(target));
ftruncateSync = mirrored(ftruncateSync, ([fd]) => mirrorTarget(fd));
fsyncSync = mirrored(fsyncSync, ([fd]) => mirrorTarget(fd));
closeSync = mirrored(closeSync, ([fd], _, [entry]) => mirrorTarget(fd, entry));
openSync = mirrored(openSync, (_, fd) => mirrorTarget(fd));
mkdirSync = mirrored(mkdirSync, ([path]) => mirror(toPath(path)));
mkdtempSync = mirrored(mkdtempSync, (_, path) => mirror(path));
rmdirSync = mirrored(rmdirSync, ([path]) => mirror(toPath(path)));
rmSync = mirrored(rmSync, ([path]) => mirror(toPath(path)));
unlinkSync = mirrored(unlinkSync, ([path]) => mirror(toPath(path)));
renameSync = mirrored(renameSync, ([from, to]) => ask({ op: "rename", from: resolvePath(toPath(from)), to: resolvePath(toPath(to)) }));
copyFileSync = mirrored(copyFileSync, ([, to]) => mirror(toPath(to)));
cpSync = mirrored(cpSync, ([, to]) => mirror(toPath(to), true));
linkSync = mirrored(linkSync, ([, path]) => mirror(toPath(path)));

const sync = {
  existsSync,
  accessSync,
  statSync,
  lstatSync,
  fstatSync,
  readFileSync,
  writeFileSync,
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmdirSync,
  rmSync,
  unlinkSync,
  renameSync,
  copyFileSync,
  cpSync,
  truncateSync,
  ftruncateSync,
  utimesSync,
  lutimesSync: utimesSync,
  futimesSync,
  chmodSync,
  lchmodSync: chmodSync,
  fchmodSync,
  chownSync,
  lchownSync: chownSync,
  fchownSync: (fd) => void descriptor(fd, "fchown"),
  realpathSync,
  readlinkSync,
  linkSync,
  symlinkSync: unsupported("symlinkSync"),
  openSync,
  closeSync,
  fsyncSync,
  fdatasyncSync: fsyncSync,
  readSync,
  writeSync,
};

function callbackOf(args, name) {
  const callback = args.pop();
  if (typeof callback !== "function") {
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"cb\" argument must be of type function. Received " + (callback === null ? "null" : typeof callback));
  }
  return callback;
}

const callbacks = {};
for (const [name, fn] of Object.entries(sync)) {
  if (name === "existsSync") continue;
  const plain = name.slice(0, -4);
  callbacks[plain] = function (...args) {
    const callback = callbackOf(args, plain);
    let result;
    let failure = null;
    try {
      result = fn(...args);
    } catch (error) {
      failure = error;
    }
    queueMicrotask(() => (failure ? callback(failure) : plain === "read" || plain === "write" ? callback(null, result, args[1]) : callback(null, result)));
  };
}
callbacks.exists = function (path, callback) {
  const result = existsSync(path);
  queueMicrotask(() => callback(result));
};
callbacks.realpath.native = callbacks.realpath;

class FileHandle {
  #fd;

  constructor(fd) {
    this.#fd = fd;
  }

  get fd() {
    return this.#fd;
  }

  async readFile(options) {
    return readFileSync(this.#fd, options);
  }

  async writeFile(data, options) {
    return writeFileSync(this.#fd, data, options);
  }

  async appendFile(data, options) {
    return writeSync(this.#fd, bytesOf(data, encodingOf(options, "utf8")));
  }

  async read(buffer, offset, length, position) {
    buffer ??= Buffer.alloc(16384);
    const bytesRead = readSync(this.#fd, buffer, offset, length, position);
    return { bytesRead, buffer };
  }

  async write(data, ...rest) {
    const bytesWritten = writeSync(this.#fd, data, ...rest);
    return { bytesWritten, buffer: data };
  }

  async stat() {
    return fstatSync(this.#fd);
  }

  async truncate(length) {
    return ftruncateSync(this.#fd, length);
  }

  async sync() {
    return fsyncSync(this.#fd);
  }

  async close() {
    return closeSync(this.#fd);
  }

  async [Symbol.asyncDispose]() {
    if (descriptors.has(this.#fd)) closeSync(this.#fd);
  }
}

const promises = {};
for (const [name, fn] of Object.entries(sync)) {
  if (name === "existsSync" || name === "openSync" || name === "closeSync" || name === "readSync" || name === "writeSync" || name.startsWith("f")) continue;
  const plain = name.slice(0, -4);
  promises[plain] = async (...args) => fn(...args);
}
promises.open = async (path, flags, mode) => new FileHandle(openSync(path, flags, mode));
promises.constants = constants;

const createReadStream = unsupported("createReadStream");
const createWriteStream = unsupported("createWriteStream");
const watch = unsupported("watch");
const watchFile = unsupported("watchFile");
const unwatchFile = unsupported("unwatchFile");
const opendirSync = unsupported("opendirSync");
const statfsSync = unsupported("statfsSync");

const fs = {
  ...sync,
  ...callbacks,
  promises,
  constants,
  Stats,
  Dirent,
  createReadStream,
  createWriteStream,
  watch,
  watchFile,
  unwatchFile,
  opendirSync,
  statfsSync,
  F_OK: constants.F_OK,
  R_OK: constants.R_OK,
  W_OK: constants.W_OK,
  X_OK: constants.X_OK,
};

export default fs;
export { promises, constants, Stats, Dirent, FileHandle, createReadStream, createWriteStream, watch, watchFile, unwatchFile, opendirSync, statfsSync };
export { existsSync, accessSync, statSync, lstatSync, fstatSync, readFileSync, writeFileSync, appendFileSync, mkdirSync, mkdtempSync, readdirSync, rmdirSync, rmSync, unlinkSync, renameSync, copyFileSync, cpSync, truncateSync, ftruncateSync, utimesSync, futimesSync, chmodSync, fchmodSync, chownSync, realpathSync, readlinkSync, linkSync, openSync, closeSync, fsyncSync, readSync, writeSync };
export const { lutimesSync, lchmodSync, lchownSync, fchownSync, symlinkSync, fdatasyncSync } = sync;
export const { access, stat, lstat, fstat, readFile, writeFile, appendFile, mkdir, mkdtemp, readdir, rmdir, rm, unlink, rename, copyFile, cp, truncate, ftruncate, utimes, futimes, chmod, fchmod, chown, realpath, readlink, link, open, close, fsync, read, write, lutimes, lchmod, lchown, fchown, symlink, fdatasync, exists } = callbacks;
