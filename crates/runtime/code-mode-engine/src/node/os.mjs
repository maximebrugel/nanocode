import process from "node:process";

const EOL = "\n";
const devNull = "/dev/null";
const constants = { signals: {}, errno: {}, priority: {} };

function platform() {
  return process.platform;
}

function arch() {
  return process.arch;
}

function type() {
  return "Linux";
}

function release() {
  return "";
}

function version() {
  return "";
}

function machine() {
  return process.arch;
}

function homedir() {
  return process.env.HOME;
}

function tmpdir() {
  return "/tmp";
}

function hostname() {
  return "localhost";
}

function endianness() {
  return new Uint8Array(new Uint16Array([1]).buffer)[0] === 1 ? "LE" : "BE";
}

function cpus() {
  return [];
}

function availableParallelism() {
  return 1;
}

function loadavg() {
  return [0, 0, 0];
}

function uptime() {
  return process.uptime();
}

function networkInterfaces() {
  return {};
}

function userInfo() {
  return { uid: -1, gid: -1, username: "", homedir: homedir(), shell: null };
}

function unavailable(name) {
  return function () {
    const error = new Error("os." + name + "() is not available in Code Mode cells");
    error.code = "ENOSYS";
    throw error;
  };
}

const totalmem = unavailable("totalmem");
const freemem = unavailable("freemem");
const getPriority = unavailable("getPriority");
const setPriority = unavailable("setPriority");

const os = { EOL, devNull, constants, platform, arch, type, release, version, machine, homedir, tmpdir, hostname, endianness, cpus, availableParallelism, loadavg, uptime, networkInterfaces, userInfo, totalmem, freemem, getPriority, setPriority };

export default os;
export { EOL, devNull, constants, platform, arch, type, release, version, machine, homedir, tmpdir, hostname, endianness, cpus, availableParallelism, loadavg, uptime, networkInterfaces, userInfo, totalmem, freemem, getPriority, setPriority };
