class Timeout {
  #id;
  #repeat;
  #callback;
  #args;
  #delay;
  #active = true;

  constructor(callback, delay, args, repeat) {
    if (typeof callback !== "function") {
      const error = new TypeError("The \"callback\" argument must be of type function. Received " + (callback === null ? "null" : typeof callback));
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    this.#callback = callback;
    this.#args = args;
    this.#repeat = repeat;
    this.#delay = Number(delay) >= 1 && Number(delay) <= 2147483647 ? Number(delay) : 1;
    this.#schedule();
  }

  #schedule() {
    this.#id = globalThis.setTimeout(() => {
      if (!this.#active) return;
      if (this.#repeat) this.#schedule();
      else this.#active = false;
      this.#callback(...this.#args);
    }, this.#delay);
  }

  close() {
    this.#active = false;
    globalThis.clearTimeout(this.#id);
    return this;
  }

  refresh() {
    if (this.#active) {
      globalThis.clearTimeout(this.#id);
      this.#schedule();
    }
    return this;
  }

  ref() {
    return this;
  }

  unref() {
    return this;
  }

  hasRef() {
    return true;
  }

  [Symbol.toPrimitive]() {
    return this.#id;
  }
}

class Immediate {
  #id;
  #active = true;

  constructor(callback, args) {
    if (typeof callback !== "function") {
      const error = new TypeError("The \"callback\" argument must be of type function. Received " + (callback === null ? "null" : typeof callback));
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    this.#id = globalThis.setTimeout(() => {
      if (!this.#active) return;
      this.#active = false;
      callback(...args);
    }, 0);
  }

  close() {
    this.#active = false;
    globalThis.clearTimeout(this.#id);
    return this;
  }

  ref() {
    return this;
  }

  unref() {
    return this;
  }

  hasRef() {
    return true;
  }

  [Symbol.toPrimitive]() {
    return this.#id;
  }
}

function setTimeout(callback, delay, ...args) {
  return new Timeout(callback, delay, args, false);
}

function setInterval(callback, delay, ...args) {
  return new Timeout(callback, delay, args, true);
}

function setImmediate(callback, ...args) {
  return new Immediate(callback, args);
}

function clear(handle) {
  if (handle instanceof Timeout || handle instanceof Immediate) handle.close();
  else if (handle !== null && handle !== undefined) globalThis.clearTimeout(handle);
}

const clearTimeout = clear;
const clearInterval = clear;
const clearImmediate = clear;

export { Timeout, Immediate, setTimeout, setInterval, setImmediate, clearTimeout, clearInterval, clearImmediate };
export default { setTimeout, setInterval, setImmediate, clearTimeout, clearInterval, clearImmediate };
