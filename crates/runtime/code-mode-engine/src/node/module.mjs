const builtinModules = ["assert", "assert/strict", "buffer", "child_process", "crypto", "events", "fs", "fs/promises", "module", "os", "path", "path/posix", "process", "querystring", "string_decoder", "timers", "timers/promises", "url", "util", "zlib"];

function isBuiltin(name) {
  const bare = String(name).startsWith("node:") ? String(name).slice(5) : String(name);
  return builtinModules.includes(bare);
}

function createRequire() {
  return globalThis.require;
}

const Module = { builtinModules, isBuiltin, createRequire };

export default Module;
export { builtinModules, isBuiltin, createRequire };
