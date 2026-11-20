// The workspace a cell runs in, when its host connects one: a host's prelude defines
// globalThis.__nanocodeNodeWorkspace as a synchronous provider (usually a synchronous host tool
// call) that answers, for absolute paths:
//   { op: "info" }                -> { cwd, env }
//   { op: "read", path }          -> { kind: "file", data: base64 } | { kind: "dir" } | { kind: "none" }
//   { op: "list", path }          -> { entries: [[name, "file" | "dir"], ...] }
//   { op: "write", path, data }   -> {}  (data base64)
//   { op: "mkdir", path }         -> {}  (with parents)
//   { op: "remove", path }        -> {}  (recursive, missing is fine)
//   { op: "rename", from, to }    -> {}  (replaces to)
// node:fs then reads and writes that workspace, and process.env and process.cwd() start from
// its environment. Without one, node:fs is an in-memory tree private to the cell.
const provider = () => globalThis.__nanocodeNodeWorkspace;
let info;

export function connected() {
  return typeof provider() === "function";
}

export function ask(request) {
  const answer = provider()(request);
  return answer !== null && typeof answer === "object" ? answer : {};
}

export function workspace() {
  if (!connected()) return undefined;
  info ??= ask({ op: "info" });
  return info;
}
