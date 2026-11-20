import assert from "node:assert";
import assertStrict from "node:assert/strict";
import buffer, { Buffer } from "node:buffer";
import childProcess from "node:child_process";
import crypto from "node:crypto";
import events from "node:events";
import fs from "node:fs";
import fsPromises from "node:fs/promises";
import module from "node:module";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import querystring from "node:querystring";
import stringDecoder from "node:string_decoder";
import timers, { setInterval, clearInterval, setImmediate, clearImmediate } from "node:timers";
import timersPromises from "node:timers/promises";
import url from "node:url";
import util from "node:util";
import zlib from "node:zlib";

const modules = {
  assert,
  "assert/strict": assertStrict,
  buffer,
  child_process: childProcess,
  crypto,
  events,
  fs,
  "fs/promises": fsPromises,
  module,
  os,
  path,
  "path/posix": path,
  process,
  querystring,
  string_decoder: stringDecoder,
  timers,
  "timers/promises": timersPromises,
  url,
  util,
  zlib,
};

function bare(id) {
  if (typeof id !== "string") {
    const error = new TypeError("The \"id\" argument must be of type string. Received " + (id === null ? "null" : typeof id));
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  const name = id.startsWith("node:") ? id.slice(5) : id;
  if (!Object.prototype.hasOwnProperty.call(modules, name)) {
    const error = new Error(id.startsWith("node:") ? "No such built-in module: " + id : "Cannot find module '" + id + "'");
    error.code = id.startsWith("node:") ? "ERR_UNKNOWN_BUILTIN_MODULE" : "MODULE_NOT_FOUND";
    throw error;
  }
  return name;
}

function require(id) {
  return modules[bare(id)];
}
require.resolve = (id) => bare(id);
require.cache = Object.create(null);

const globals = { Buffer, process, global: globalThis, setInterval, clearInterval, setImmediate, clearImmediate, require };
for (const name of Object.keys(globals)) {
  Object.defineProperty(globalThis, name, { value: globals[name], writable: true, configurable: true, enumerable: false });
}
