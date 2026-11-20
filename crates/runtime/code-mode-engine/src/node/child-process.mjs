function unavailable(name) {
  return function () {
    const error = new Error("child_process." + name + " is not available: Code Mode cells cannot start processes. Run commands through a tool in tools instead.");
    error.code = "ENOSYS";
    error.errno = -38;
    error.syscall = name;
    throw error;
  };
}

const exec = unavailable("exec");
const execSync = unavailable("execSync");
const execFile = unavailable("execFile");
const execFileSync = unavailable("execFileSync");
const spawn = unavailable("spawn");
const spawnSync = unavailable("spawnSync");
const fork = unavailable("fork");

class ChildProcess {
  constructor() {
    unavailable("ChildProcess")();
  }
}

export default { exec, execSync, execFile, execFileSync, spawn, spawnSync, fork, ChildProcess };
export { exec, execSync, execFile, execFileSync, spawn, spawnSync, fork, ChildProcess };
