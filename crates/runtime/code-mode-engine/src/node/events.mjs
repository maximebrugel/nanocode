const kCapture = Symbol("kCapture");
const errorMonitor = Symbol("events.errorMonitor");
const captureRejectionSymbol = Symbol.for("nodejs.rejection");
let defaultMaxListeners = 10;

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function checkListener(listener) {
  if (typeof listener !== "function") {
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"listener\" argument must be of type function. Received " + (listener === null ? "null" : typeof listener));
  }
}

function eventsOf(emitter) {
  if (!Object.prototype.hasOwnProperty.call(emitter, "_events") || emitter._events === undefined) {
    Object.defineProperty(emitter, "_events", { value: Object.create(null), writable: true, configurable: true, enumerable: false });
    Object.defineProperty(emitter, "_eventsCount", { value: 0, writable: true, configurable: true, enumerable: false });
  }
  return emitter._events;
}

function addListener(emitter, name, listener, prepend) {
  checkListener(listener);
  const events = eventsOf(emitter);
  if (events.newListener !== undefined) emitter.emit("newListener", name, listener.listener ?? listener);
  const list = events[name] ?? (emitter._eventsCount++, (events[name] = []));
  if (prepend) list.unshift(listener);
  else list.push(listener);
  const max = emitter.getMaxListeners();
  if (max > 0 && list.length > max && !list.warned) {
    list.warned = true;
    const warning = new Error("Possible EventEmitter memory leak detected. " + list.length + " " + String(name) + " listeners added to " + (emitter.constructor?.name ?? "EventEmitter") + ". MaxListeners is " + max + ". Use emitter.setMaxListeners() to increase limit");
    warning.name = "MaxListenersExceededWarning";
    globalThis.process?.emitWarning?.(warning);
  }
  return emitter;
}

function onceWrapper(emitter, name, listener) {
  const wrapper = function (...args) {
    emitter.removeListener(name, wrapper);
    return listener.apply(this, args);
  };
  wrapper.listener = listener;
  return wrapper;
}

class EventEmitter {
  constructor(options) {
    eventsOf(this);
    Object.defineProperty(this, "_maxListeners", { value: undefined, writable: true, configurable: true, enumerable: false });
    if (options?.captureRejections) this[kCapture] = true;
  }

  static get defaultMaxListeners() {
    return defaultMaxListeners;
  }

  static set defaultMaxListeners(value) {
    if (typeof value !== "number" || value < 0 || Number.isNaN(value)) {
      throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"defaultMaxListeners\" is out of range. It must be a non-negative number. Received " + String(value));
    }
    defaultMaxListeners = value;
  }

  setMaxListeners(count) {
    if (typeof count !== "number" || count < 0 || Number.isNaN(count)) {
      throw codedError(RangeError, "ERR_OUT_OF_RANGE", "The value of \"n\" is out of range. It must be a non-negative number. Received " + String(count));
    }
    this._maxListeners = count;
    return this;
  }

  getMaxListeners() {
    return this._maxListeners === undefined ? defaultMaxListeners : this._maxListeners;
  }

  emit(name, ...args) {
    const events = eventsOf(this);
    if (name === "error") {
      if (events[errorMonitor] !== undefined) this.emit(errorMonitor, ...args);
      if (events.error === undefined) {
        const error = args[0];
        if (error instanceof Error) throw error;
        const wrapped = new Error("Unhandled error." + (error === undefined ? "" : " (" + String(error) + ")"));
        wrapped.code = "ERR_UNHANDLED_ERROR";
        wrapped.context = error;
        throw wrapped;
      }
    }
    const list = events[name];
    if (list === undefined) return false;
    for (const listener of [...list]) {
      const result = listener.apply(this, args);
      if (this[kCapture] && result && typeof result.then === "function") {
        result.then(undefined, (error) => {
          if (typeof this[captureRejectionSymbol] === "function") this[captureRejectionSymbol](error, name, ...args);
          else this.emit("error", error);
        });
      }
    }
    return true;
  }

  addListener(name, listener) {
    return addListener(this, name, listener, false);
  }

  on(name, listener) {
    return addListener(this, name, listener, false);
  }

  prependListener(name, listener) {
    return addListener(this, name, listener, true);
  }

  once(name, listener) {
    checkListener(listener);
    return addListener(this, name, onceWrapper(this, name, listener), false);
  }

  prependOnceListener(name, listener) {
    checkListener(listener);
    return addListener(this, name, onceWrapper(this, name, listener), true);
  }

  removeListener(name, listener) {
    checkListener(listener);
    const events = eventsOf(this);
    const list = events[name];
    if (list === undefined) return this;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i] === listener || list[i].listener === listener) {
        list.splice(i, 1);
        if (list.length === 0) {
          delete events[name];
          this._eventsCount--;
        }
        if (events.removeListener !== undefined) this.emit("removeListener", name, listener);
        break;
      }
    }
    return this;
  }

  off(name, listener) {
    return this.removeListener(name, listener);
  }

  removeAllListeners(name) {
    const events = eventsOf(this);
    if (name === undefined) {
      for (const key of Reflect.ownKeys(events)) {
        if (key !== "removeListener") this.removeAllListeners(key);
      }
      this.removeAllListeners("removeListener");
      this._events = Object.create(null);
      this._eventsCount = 0;
      return this;
    }
    const list = events[name];
    if (list === undefined) return this;
    for (let i = list.length - 1; i >= 0; i--) this.removeListener(name, list[i]);
    return this;
  }

  listeners(name) {
    return (eventsOf(this)[name] ?? []).map((listener) => listener.listener ?? listener);
  }

  rawListeners(name) {
    return [...(eventsOf(this)[name] ?? [])];
  }

  listenerCount(name, listener) {
    const list = eventsOf(this)[name] ?? [];
    if (listener === undefined) return list.length;
    return list.filter((item) => item === listener || item.listener === listener).length;
  }

  eventNames() {
    return Reflect.ownKeys(eventsOf(this));
  }
}

EventEmitter.prototype._events = undefined;
EventEmitter.prototype._eventsCount = 0;
EventEmitter.prototype._maxListeners = undefined;

function once(emitter, name, options) {
  return new Promise((resolve, reject) => {
    const signal = options?.signal;
    if (signal?.aborted) {
      reject(signal.reason ?? abortError());
      return;
    }
    if (typeof emitter.addEventListener === "function" && typeof emitter.on !== "function") {
      emitter.addEventListener(name, (event) => resolve([event]), { once: true });
      return;
    }
    const onError = (error) => {
      emitter.removeListener(name, onEvent);
      reject(error);
    };
    const onEvent = (...args) => {
      if (name !== "error") emitter.removeListener("error", onError);
      resolve(args);
    };
    emitter.once(name, onEvent);
    if (name !== "error") emitter.once("error", onError);
    signal?.addEventListener?.("abort", () => {
      emitter.removeListener(name, onEvent);
      emitter.removeListener("error", onError);
      reject(signal.reason ?? abortError());
    }, { once: true });
  });
}

function abortError() {
  const error = new Error("The operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
}

function on(emitter, name, options) {
  const queue = [];
  const waiting = [];
  let failure = null;
  let done = false;
  const onEvent = (...args) => {
    if (waiting.length) waiting.shift().resolve({ value: args, done: false });
    else queue.push(args);
  };
  const onError = (error) => {
    done = true;
    if (waiting.length) waiting.shift().reject(error);
    else failure = error;
    cleanup();
  };
  const cleanup = () => {
    emitter.removeListener(name, onEvent);
    emitter.removeListener("error", onError);
  };
  emitter.on(name, onEvent);
  if (name !== "error") emitter.on("error", onError);
  options?.signal?.addEventListener?.("abort", () => onError(options.signal.reason ?? abortError()), { once: true });
  return {
    next() {
      if (queue.length) return Promise.resolve({ value: queue.shift(), done: false });
      if (failure) {
        const error = failure;
        failure = null;
        return Promise.reject(error);
      }
      if (done) return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve, reject) => waiting.push({ resolve, reject }));
    },
    return() {
      done = true;
      cleanup();
      for (const item of waiting.splice(0)) item.resolve({ value: undefined, done: true });
      return Promise.resolve({ value: undefined, done: true });
    },
    throw(error) {
      onError(error);
      return Promise.reject(error);
    },
    [Symbol.asyncIterator]() {
      return this;
    },
  };
}

function listenerCount(emitter, name) {
  return emitter.listenerCount(name);
}

function getEventListeners(emitter, name) {
  return typeof emitter.listeners === "function" ? emitter.listeners(name) : [];
}

function setMaxListeners(count = defaultMaxListeners, ...targets) {
  if (targets.length === 0) defaultMaxListeners = count;
  for (const target of targets) target.setMaxListeners?.(count);
}

EventEmitter.EventEmitter = EventEmitter;
EventEmitter.once = once;
EventEmitter.on = on;
EventEmitter.listenerCount = listenerCount;
EventEmitter.getEventListeners = getEventListeners;
EventEmitter.setMaxListeners = setMaxListeners;
EventEmitter.errorMonitor = errorMonitor;
EventEmitter.captureRejectionSymbol = captureRejectionSymbol;
EventEmitter.captureRejections = false;

export default EventEmitter;
export { EventEmitter, once, on, listenerCount, getEventListeners, setMaxListeners, errorMonitor, captureRejectionSymbol };
