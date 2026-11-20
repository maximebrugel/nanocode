function abortError(signal) {
  const error = new Error("The operation was aborted", { cause: signal?.reason });
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
}

function setTimeout(delay, value, options) {
  const signal = options?.signal;
  if (signal?.aborted) return Promise.reject(abortError(signal));
  return new Promise((resolve, reject) => {
    const id = globalThis.setTimeout(() => resolve(value), delay);
    signal?.addEventListener?.("abort", () => {
      globalThis.clearTimeout(id);
      reject(abortError(signal));
    }, { once: true });
  });
}

function setImmediate(value, options) {
  return setTimeout(0, value, options);
}

async function* setInterval(delay, value, options) {
  const signal = options?.signal;
  for (;;) {
    await setTimeout(delay, undefined, { signal });
    yield value;
  }
}

const scheduler = {
  wait(delay, options) {
    return setTimeout(delay, undefined, options);
  },
  yield() {
    return setImmediate();
  },
};

export default { setTimeout, setImmediate, setInterval, scheduler };
export { setTimeout, setImmediate, setInterval, scheduler };
