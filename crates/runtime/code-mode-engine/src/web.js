// Web platform globals for Code Mode cells: URL, URLSearchParams, events and
// abort signals, structuredClone, crypto, Blob, Headers, Request, Response,
// fetch, streams and message channels. They follow the WHATWG specifications
// as Node exposes them. Cells have no network, so fetch answers data: and
// blob: URLs and rejects every other scheme the way an unreachable host does.
(host) => {
  const { random, digest, setTimeout, uncaught, clockNow, monotonicNow } = host;
  const encoder = new TextEncoder();
  const utf8 = (text) => encoder.encode(text);
  const decodeUtf8 = (bytes) => new TextDecoder().decode(bytes);
  const define = (target, name, value) => Object.defineProperty(target, name, {
    value, writable: true, configurable: true, enumerable: false,
  });
  const tag = (target, name) => Object.defineProperty(target.prototype, Symbol.toStringTag, {
    value: name, configurable: true,
  });
  const domError = (message, name) => new DOMException(message, name);
  const illegal = Symbol("illegal constructor");
  const isObject = (value) => (typeof value === "object" && value !== null) || typeof value === "function";
  const webidlString = (value) => {
    if (typeof value === "symbol") throw new TypeError("Cannot convert a Symbol value to a string");
    return `${value}`;
  };
  const usvString = (value) => webidlString(value).toWellFormed();
  const requireArgs = (count, needed, where) => {
    if (count < needed) {
      throw new TypeError(`${where}: ${needed} argument${needed === 1 ? "" : "s"} required, but only ${count} present.`);
    }
  };
  const bytesOf = (value) => {
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return null;
  };
  const concatBytes = (chunks) => {
    let length = 0;
    for (const chunk of chunks) length += chunk.byteLength;
    const out = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      out.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return out;
  };
  const hex = (byte) => byte.toString(16).toUpperCase().padStart(2, "0");
  const attempt = (run) => {
    try {
      return Promise.resolve(run());
    } catch (error) {
      return Promise.reject(error);
    }
  };

  // ---------------------------------------------------------------- events

  const eventState = new WeakMap();
  class Event {
    constructor(type, options = undefined) {
      requireArgs(arguments.length, 1, "Event constructor");
      const init = options ?? {};
      eventState.set(this, {
        type: webidlString(type),
        bubbles: Boolean(init.bubbles),
        cancelable: Boolean(init.cancelable),
        composed: Boolean(init.composed),
        canceled: false,
        stop: false,
        stopNow: false,
        dispatching: false,
        target: null,
        current: null,
        phase: 0,
        passive: false,
        time: monotonicNow(),
      });
    }
    get type() { return eventState.get(this).type; }
    get bubbles() { return eventState.get(this).bubbles; }
    get cancelable() { return eventState.get(this).cancelable; }
    get composed() { return eventState.get(this).composed; }
    get defaultPrevented() { const state = eventState.get(this); return state.cancelable && state.canceled; }
    get target() { return eventState.get(this).target; }
    get srcElement() { return eventState.get(this).target; }
    get currentTarget() { return eventState.get(this).current; }
    get eventPhase() { return eventState.get(this).phase; }
    get timeStamp() { return eventState.get(this).time; }
    get isTrusted() { return false; }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(value) { if (!value) this.preventDefault(); }
    get cancelBubble() { return eventState.get(this).stop; }
    set cancelBubble(value) { if (value) eventState.get(this).stop = true; }
    composedPath() { const state = eventState.get(this); return state.dispatching && state.current ? [state.current] : []; }
    preventDefault() { const state = eventState.get(this); if (!state.passive) state.canceled = true; }
    stopPropagation() { eventState.get(this).stop = true; }
    stopImmediatePropagation() { const state = eventState.get(this); state.stop = true; state.stopNow = true; }
    initEvent(type, bubbles = false, cancelable = false) {
      const state = eventState.get(this);
      if (state.dispatching) return;
      Object.assign(state, { type: webidlString(type), bubbles: Boolean(bubbles), cancelable: Boolean(cancelable) });
    }
  }
  for (const [name, value] of [["NONE", 0], ["CAPTURING_PHASE", 1], ["AT_TARGET", 2], ["BUBBLING_PHASE", 3]]) {
    Object.defineProperty(Event, name, { value, enumerable: true });
    Object.defineProperty(Event.prototype, name, { value, enumerable: true });
  }
  tag(Event, "Event");

  class CustomEvent extends Event {
    #detail;
    constructor(type, options = undefined) {
      requireArgs(arguments.length, 1, "CustomEvent constructor");
      super(type, options);
      this.#detail = options?.detail ?? null;
    }
    get detail() { return this.#detail; }
  }
  tag(CustomEvent, "CustomEvent");

  class MessageEvent extends Event {
    #init;
    constructor(type, options = undefined) {
      requireArgs(arguments.length, 1, "MessageEvent constructor");
      super(type, options);
      this.#init = {
        data: options?.data ?? null,
        origin: `${options?.origin ?? ""}`,
        lastEventId: `${options?.lastEventId ?? ""}`,
        source: options?.source ?? null,
        ports: Object.freeze([...(options?.ports ?? [])]),
      };
    }
    get data() { return this.#init.data; }
    get origin() { return this.#init.origin; }
    get lastEventId() { return this.#init.lastEventId; }
    get source() { return this.#init.source; }
    get ports() { return this.#init.ports; }
  }
  tag(MessageEvent, "MessageEvent");

  class CloseEvent extends Event {
    #init;
    constructor(type, options = undefined) {
      super(type, options);
      this.#init = { wasClean: Boolean(options?.wasClean), code: Number(options?.code ?? 0), reason: `${options?.reason ?? ""}` };
    }
    get wasClean() { return this.#init.wasClean; }
    get code() { return this.#init.code; }
    get reason() { return this.#init.reason; }
  }
  tag(CloseEvent, "CloseEvent");

  const listenersOf = new WeakMap();
  const flatten = (options) => (typeof options === "boolean" ? { capture: options } : (options ?? {}));
  class EventTarget {
    constructor() {
      listenersOf.set(this, new Map());
    }
    addEventListener(type, callback, options = undefined) {
      requireArgs(arguments.length, 2, "EventTarget.addEventListener");
      if (callback === null || callback === undefined) return;
      if (!isObject(callback)) throw new TypeError("The \"listener\" argument must be an instance of EventListener.");
      const { capture = false, once = false, passive = false, signal } = flatten(options);
      if (signal?.aborted) return;
      const map = listenersOf.get(this);
      const name = webidlString(type);
      const list = map.get(name) ?? [];
      if (list.some((entry) => entry.callback === callback && entry.capture === Boolean(capture))) return;
      list.push({ callback, capture: Boolean(capture), once: Boolean(once), passive: Boolean(passive), removed: false });
      map.set(name, list);
      signal?.addEventListener("abort", () => this.removeEventListener(name, callback, { capture }));
    }
    removeEventListener(type, callback, options = undefined) {
      requireArgs(arguments.length, 2, "EventTarget.removeEventListener");
      const capture = Boolean(flatten(options).capture);
      const name = webidlString(type);
      const list = listenersOf.get(this)?.get(name);
      if (!list) return;
      const at = list.findIndex((entry) => entry.callback === callback && entry.capture === capture);
      if (at >= 0) {
        list[at].removed = true;
        list.splice(at, 1);
      }
    }
    dispatchEvent(event) {
      requireArgs(arguments.length, 1, "EventTarget.dispatchEvent");
      if (!(event instanceof Event)) throw new TypeError("The \"event\" argument must be an instance of Event.");
      const state = eventState.get(event);
      if (state.dispatching) throw domError("The event is already being dispatched.", "InvalidStateError");
      state.dispatching = true;
      state.target = this;
      state.current = this;
      state.phase = 2;
      const list = [...(listenersOf.get(this)?.get(state.type) ?? [])];
      for (const entry of list) {
        if (entry.removed) continue;
        if (entry.once) this.removeEventListener(state.type, entry.callback, { capture: entry.capture });
        state.passive = entry.passive;
        try {
          if (typeof entry.callback === "function") entry.callback.call(this, event);
          else if (typeof entry.callback.handleEvent === "function") entry.callback.handleEvent(event);
        } catch (error) {
          uncaught(error, false);
        }
        state.passive = false;
        if (state.stopNow) break;
      }
      state.dispatching = false;
      state.current = null;
      state.phase = 0;
      state.stop = false;
      state.stopNow = false;
      return !event.defaultPrevented;
    }
  }
  tag(EventTarget, "EventTarget");

  // Defines an on<type> event handler attribute, which runs where it was first set.
  const handlerAttribute = (Target, type, onSet = undefined) => {
    const handlers = new WeakMap();
    Object.defineProperty(Target.prototype, `on${type}`, {
      configurable: true,
      enumerable: true,
      get() { return handlers.get(this)?.value ?? null; },
      set(value) {
        let slot = handlers.get(this);
        if (!slot) {
          slot = { value: null };
          handlers.set(this, slot);
          this.addEventListener(type, function (event) {
            if (typeof slot.value === "function") return slot.value.call(this, event);
            return undefined;
          });
        }
        slot.value = isObject(value) ? value : null;
        onSet?.(this);
      },
    });
  };

  // ---------------------------------------------------------- abort signals

  const abortState = new WeakMap();
  const abortError = () => domError("This operation was aborted", "AbortError");
  const signalAbort = (signal, reason) => {
    const state = abortState.get(signal);
    if (state.aborted) return;
    state.aborted = true;
    state.reason = reason === undefined ? abortError() : reason;
    const followers = state.followers.splice(0);
    for (const follower of followers) follower(state.reason);
    signal.dispatchEvent(new Event("abort"));
  };
  class AbortSignal extends EventTarget {
    constructor(key) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
      super();
      abortState.set(this, { aborted: false, reason: undefined, followers: [] });
    }
    get aborted() { return abortState.get(this).aborted; }
    get reason() { return abortState.get(this).reason; }
    throwIfAborted() {
      const state = abortState.get(this);
      if (state.aborted) throw state.reason;
    }
    static abort(reason = undefined) {
      const signal = new AbortSignal(illegal);
      const state = abortState.get(signal);
      state.aborted = true;
      state.reason = reason === undefined ? abortError() : reason;
      return signal;
    }
    static timeout(milliseconds) {
      requireArgs(arguments.length, 1, "AbortSignal.timeout");
      const delay = Number(milliseconds);
      if (!Number.isFinite(delay) || delay < 0) {
        throw new RangeError(`The value of "delay" is out of range. It must be >= 0 && <= 4294967295. Received ${milliseconds}`);
      }
      const signal = new AbortSignal(illegal);
      setTimeout(() => signalAbort(signal, domError("The operation was aborted due to timeout", "TimeoutError")), delay);
      return signal;
    }
    static any(signals) {
      requireArgs(arguments.length, 1, "AbortSignal.any");
      const sources = [...signals];
      const signal = new AbortSignal(illegal);
      for (const source of sources) {
        if (!(source instanceof AbortSignal)) throw new TypeError("The \"signals\" argument must be an array of AbortSignal.");
        if (source.aborted) {
          const state = abortState.get(signal);
          state.aborted = true;
          state.reason = source.reason;
          return signal;
        }
      }
      for (const source of sources) abortState.get(source).followers.push((reason) => signalAbort(signal, reason));
      return signal;
    }
  }
  handlerAttribute(AbortSignal, "abort");
  tag(AbortSignal, "AbortSignal");

  class AbortController {
    #signal = new AbortSignal(illegal);
    get signal() { return this.#signal; }
    abort(reason = undefined) { signalAbort(this.#signal, reason); }
  }
  tag(AbortController, "AbortController");

  // ------------------------------------------------------- structuredClone

  const cloneError = (value) => {
    let shown;
    try {
      shown = typeof value === "function" || typeof value === "symbol"
        ? String(value)
        : `#<${Object.prototype.toString.call(value).slice(8, -1)}>`;
    } catch {
      shown = "value";
    }
    return domError(`${shown} could not be cloned.`, "DataCloneError");
  };
  const errorKinds = { Error, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError };
  const typedArrays = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array,
    Float32Array, Float64Array, BigInt64Array, BigUint64Array, globalThis.Float16Array].filter(Boolean);
  const plainKinds = new Set(["Object", "Array", "Arguments"]);
  const SharedBuffer = globalThis.SharedArrayBuffer;
  function structuredClone(value, options = undefined) {
    requireArgs(arguments.length, 1, "structuredClone");
    const memory = new Map();
    for (const buffer of options?.transfer ?? []) {
      if (!(buffer instanceof ArrayBuffer)) throw cloneError(buffer);
      if (memory.has(buffer)) throw domError("Transfer list contains duplicate items", "DataCloneError");
      memory.set(buffer, null);
    }
    for (const buffer of [...memory.keys()]) memory.set(buffer, buffer.transfer ? buffer.transfer() : buffer.slice(0));
    const copy = (input) => {
      if (typeof input === "symbol" || typeof input === "function") throw cloneError(input);
      if (!isObject(input)) return input;
      if (memory.has(input)) return memory.get(input);
      // One agent: the clone of a SharedArrayBuffer is the buffer itself, sharing its memory.
      if (SharedBuffer && input instanceof SharedBuffer) return input;
      let out;
      if (input instanceof Blob) {
        out = input instanceof File
          ? new File([input], input.name, { type: input.type, lastModified: input.lastModified })
          : new Blob([input], { type: input.type });
        memory.set(input, out);
        return out;
      }
      if (input instanceof DOMException) {
        out = new DOMException(input.message, input.name);
        memory.set(input, out);
        return out;
      }
      if (ArrayBuffer.isView(input)) {
        const buffer = copy(input.buffer);
        const Kind = input instanceof DataView ? DataView : typedArrays.find((Type) => input instanceof Type);
        out = Kind === DataView
          ? new DataView(buffer, input.byteOffset, input.byteLength)
          : new Kind(buffer, input.byteOffset, input.length);
        memory.set(input, out);
        return out;
      }
      const kind = Object.prototype.toString.call(input).slice(8, -1);
      if (Array.isArray(input)) out = new Array(input.length);
      else if (input instanceof Date) out = new Date(input.getTime());
      else if (input instanceof RegExp) out = new RegExp(input.source, input.flags);
      else if (input instanceof ArrayBuffer) out = input.slice(0);
      else if (input instanceof Map) out = new Map();
      else if (input instanceof Set) out = new Set();
      else if (input instanceof Error) {
        const Kind = errorKinds[input.name] ?? Error;
        out = Object.create(Kind.prototype);
        memory.set(input, out);
        const message = Object.getOwnPropertyDescriptor(input, "message");
        if (message) define(out, "message", `${message.value}`);
        if (typeof input.stack === "string") define(out, "stack", input.stack);
        if (Object.hasOwn(input, "cause")) define(out, "cause", copy(input.cause));
        return out;
      } else if (input instanceof Boolean || input instanceof Number || input instanceof String || input instanceof BigInt) {
        out = Object(input.valueOf());
      } else if (!plainKinds.has(kind) || input instanceof Promise || input instanceof WeakMap
        || input instanceof WeakSet || input instanceof WeakRef || input instanceof EventTarget
        || input instanceof Headers || input instanceof Request || input instanceof Response) {
        throw cloneError(input);
      } else out = {};
      memory.set(input, out);
      if (input instanceof Map) {
        for (const [key, item] of input) out.set(copy(key), copy(item));
      } else if (input instanceof Set) {
        for (const item of input) out.add(copy(item));
      } else if (Array.isArray(input) || Object.getPrototypeOf(out) === Object.prototype) {
        for (const key of Object.keys(input)) out[key] = copy(input[key]);
      }
      return out;
    };
    return copy(value);
  }

  // -------------------------------------------------------------------- URL

  const FAILURE = Symbol("failure");
  const special = { "ftp:": 21, "file:": null, "http:": 80, "https:": 443, "ws:": 80, "wss:": 443 };
  const isSpecialScheme = (scheme) => Object.hasOwn(special, `${scheme}:`);
  const defaultPort = (scheme) => special[`${scheme}:`] ?? null;
  const c0 = (c) => c < 0x20 || c > 0x7e;
  const fragmentSet = (c) => c0(c) || c === 0x20 || c === 0x22 || c === 0x3c || c === 0x3e || c === 0x60;
  const querySet = (c) => c0(c) || c === 0x20 || c === 0x22 || c === 0x23 || c === 0x3c || c === 0x3e;
  const specialQuerySet = (c) => querySet(c) || c === 0x27;
  const pathSet = (c) => querySet(c) || c === 0x3f || c === 0x5e || c === 0x60 || c === 0x7b || c === 0x7d;
  const userinfoSet = (c) => pathSet(c) || c === 0x2f || c === 0x3a || c === 0x3b || c === 0x3d || c === 0x40
    || (c >= 0x5b && c <= 0x5d) || c === 0x7c;
  const percentEncode = (text, set) => {
    let out = "";
    for (const char of text) {
      const code = char.codePointAt(0);
      if (code < 0x80 && !set(code)) {
        out += char;
        continue;
      }
      for (const byte of utf8(char)) out += set(byte) || byte >= 0x80 ? `%${hex(byte)}` : String.fromCharCode(byte);
    }
    return out;
  };
  function parseHexDigit(byte) {
    if (byte >= 0x30 && byte <= 0x39) return byte - 0x30;
    if (byte >= 0x41 && byte <= 0x46) return byte - 0x37;
    if (byte >= 0x61 && byte <= 0x66) return byte - 0x57;
    return -1;
  }
  const percentDecodeBytes = (bytes) => {
    const out = [];
    for (let at = 0; at < bytes.length; at += 1) {
      const byte = bytes[at];
      const high = parseHexDigit(bytes[at + 1]);
      const low = parseHexDigit(bytes[at + 2]);
      if (byte === 0x25 && high >= 0 && low >= 0) {
        out.push(high * 16 + low);
        at += 2;
      } else out.push(byte);
    }
    return new Uint8Array(out);
  };
  const percentDecode = (text) => decodeUtf8(percentDecodeBytes(utf8(text)));
  const isAlpha = (c) => c !== undefined && /^[A-Za-z]$/.test(c);
  const isDigit = (c) => c !== undefined && /^[0-9]$/.test(c);
  const isWindowsDriveLetter = (text) => text.length === 2 && isAlpha(text[0]) && (text[1] === ":" || text[1] === "|");
  const isNormalizedWindowsDriveLetter = (text) => isWindowsDriveLetter(text) && text[1] === ":";
  const startsWithWindowsDriveLetter = (chars, at) => chars.length - at >= 2
    && isWindowsDriveLetter(chars[at] + chars[at + 1])
    && (chars.length - at === 2 || "/\\?#".includes(chars[at + 2]));
  const isSingleDot = (text) => text === "." || text.toLowerCase() === "%2e";
  const isDoubleDot = (text) => ["..", ".%2e", "%2e.", "%2e%2e"].includes(text.toLowerCase());

  // RFC 3492 Punycode, for internationalized domain labels.
  const punycode = (label) => {
    const input = Array.from(label, (char) => char.codePointAt(0));
    const adapt = (delta, points, first) => {
      let k = 0;
      delta = first ? Math.floor(delta / 700) : delta >> 1;
      delta += Math.floor(delta / points);
      while (delta > 455) {
        delta = Math.floor(delta / 35);
        k += 36;
      }
      return k + Math.floor((36 * delta) / (delta + 38));
    };
    const digit = (value) => String.fromCharCode(value + 22 + (value < 26 ? 75 : 0));
    let out = input.filter((code) => code < 0x80).map((code) => String.fromCharCode(code)).join("");
    const basic = out.length;
    let handled = basic;
    if (basic > 0) out += "-";
    let n = 128;
    let delta = 0;
    let bias = 72;
    while (handled < input.length) {
      const next = Math.min(...input.filter((code) => code >= n));
      delta += (next - n) * (handled + 1);
      n = next;
      for (const code of input) {
        if (code < n) delta += 1;
        if (code !== n) continue;
        let q = delta;
        for (let k = 36; ; k += 36) {
          const t = k <= bias ? 1 : k >= bias + 26 ? 26 : k - bias;
          if (q < t) break;
          out += digit(t + ((q - t) % (36 - t)));
          q = Math.floor((q - t) / (36 - t));
        }
        out += digit(q);
        bias = adapt(delta, handled + 1, handled === basic);
        delta = 0;
        handled += 1;
      }
      delta += 1;
      n += 1;
    }
    return out;
  };
  const domainToASCII = (domain) => {
    const mapped = domain.normalize("NFKC").toLowerCase().replace(/[\u3002\uff0e\uff61]/g, ".").replace(/\u00ad/g, "");
    const ascii = mapped.split(".").map((label) => (/[^\x00-\x7f]/.test(label) ? `xn--${punycode(label)}` : label)).join(".");
    return ascii === "" ? FAILURE : ascii;
  };
  const forbiddenHost = /[\x00\t\n\r #/:<>?@[\\\]^|]/;
  const forbiddenDomain = /[\x00-\x20#%/:<>?@[\\\]^|\x7f]/;
  const parseIPv4Number = (input) => {
    let text = input;
    if (text === "") return FAILURE;
    let radix = 10;
    if (/^0[xX]/.test(text)) {
      text = text.slice(2);
      radix = 16;
    } else if (text.length > 1 && text[0] === "0") {
      text = text.slice(1);
      radix = 8;
    }
    if (text === "") return 0;
    const valid = radix === 10 ? /^[0-9]+$/ : radix === 16 ? /^[0-9a-fA-F]+$/ : /^[0-7]+$/;
    if (!valid.test(text)) return FAILURE;
    return parseInt(text, radix);
  };
  const endsInNumber = (text) => {
    const parts = text.split(".");
    if (parts[parts.length - 1] === "") {
      if (parts.length === 1) return false;
      parts.pop();
    }
    const last = parts[parts.length - 1];
    if (last !== "" && /^[0-9]+$/.test(last)) return true;
    return parseIPv4Number(last) !== FAILURE;
  };
  const parseIPv4 = (text) => {
    const parts = text.split(".");
    if (parts[parts.length - 1] === "" && parts.length > 1) parts.pop();
    if (parts.length > 4) return FAILURE;
    const numbers = [];
    for (const part of parts) {
      const number = parseIPv4Number(part);
      if (number === FAILURE) return FAILURE;
      numbers.push(number);
    }
    if (numbers.slice(0, -1).some((number) => number > 255)) return FAILURE;
    if (numbers[numbers.length - 1] >= 256 ** (5 - numbers.length)) return FAILURE;
    let address = numbers[numbers.length - 1];
    for (let at = 0; at < numbers.length - 1; at += 1) address += numbers[at] * 256 ** (3 - at);
    return address;
  };
  const serializeIPv4 = (address) => [24, 16, 8, 0].map((shift) => Math.floor(address / 2 ** shift) % 256).join(".");
  const parseIPv6 = (text) => {
    const address = [0, 0, 0, 0, 0, 0, 0, 0];
    const chars = Array.from(text);
    let piece = 0;
    let compress = null;
    let at = 0;
    const hexValue = (c) => (c === undefined ? -1 : parseHexDigit(c.charCodeAt(0)));
    if (chars[at] === ":") {
      if (chars[at + 1] !== ":") return FAILURE;
      at += 2;
      piece += 1;
      compress = piece;
    }
    while (at < chars.length) {
      if (piece === 8) return FAILURE;
      if (chars[at] === ":") {
        if (compress !== null) return FAILURE;
        at += 1;
        piece += 1;
        compress = piece;
        continue;
      }
      let value = 0;
      let length = 0;
      while (length < 4 && hexValue(chars[at]) >= 0) {
        value = value * 16 + hexValue(chars[at]);
        at += 1;
        length += 1;
      }
      if (chars[at] === ".") {
        if (length === 0) return FAILURE;
        at -= length;
        if (piece > 6) return FAILURE;
        let seen = 0;
        while (at < chars.length) {
          let part = null;
          if (seen > 0) {
            if (chars[at] === "." && seen < 4) at += 1;
            else return FAILURE;
          }
          if (!isDigit(chars[at])) return FAILURE;
          while (isDigit(chars[at])) {
            const number = Number(chars[at]);
            if (part === null) part = number;
            else if (part === 0) return FAILURE;
            else part = part * 10 + number;
            if (part > 255) return FAILURE;
            at += 1;
          }
          address[piece] = address[piece] * 256 + part;
          seen += 1;
          if (seen === 2 || seen === 4) piece += 1;
        }
        if (seen !== 4) return FAILURE;
        break;
      } else if (chars[at] === ":") {
        at += 1;
        if (at >= chars.length) return FAILURE;
      } else if (at < chars.length) return FAILURE;
      address[piece] = value;
      piece += 1;
    }
    if (compress !== null) {
      let swaps = piece - compress;
      piece = 7;
      while (piece !== 0 && swaps > 0) {
        const other = compress + swaps - 1;
        [address[piece], address[other]] = [address[other], address[piece]];
        piece -= 1;
        swaps -= 1;
      }
    } else if (piece !== 8) return FAILURE;
    return address;
  };
  const serializeIPv6 = (address) => {
    let best = -1;
    let bestLength = 1;
    for (let at = 0; at < 8;) {
      let end = at;
      while (end < 8 && address[end] === 0) end += 1;
      if (end - at > bestLength) {
        best = at;
        bestLength = end - at;
      }
      at = end === at ? at + 1 : end;
    }
    let out = "";
    let ignore = false;
    for (let at = 0; at < 8; at += 1) {
      if (ignore && address[at] === 0) continue;
      ignore = false;
      if (best === at) {
        out += at === 0 ? "::" : ":";
        ignore = true;
        continue;
      }
      out += address[at].toString(16);
      if (at !== 7) out += ":";
    }
    return `[${out}]`;
  };
  const parseHost = (input, notSpecial) => {
    if (input.startsWith("[")) {
      if (!input.endsWith("]")) return FAILURE;
      const address = parseIPv6(input.slice(1, -1));
      return address === FAILURE ? FAILURE : serializeIPv6(address);
    }
    if (notSpecial) return forbiddenHost.test(input) ? FAILURE : percentEncode(input, c0);
    const ascii = domainToASCII(percentDecode(input));
    if (ascii === FAILURE || forbiddenDomain.test(ascii)) return FAILURE;
    if (endsInNumber(ascii)) {
      const address = parseIPv4(ascii);
      return address === FAILURE ? FAILURE : serializeIPv4(address);
    }
    return ascii;
  };


  const newRecord = () => ({ scheme: "", username: "", password: "", host: null, port: null, path: [], query: null, fragment: null });
  const hasOpaquePath = (url) => typeof url.path === "string";
  const includesCredentials = (url) => url.username !== "" || url.password !== "";
  const cannotHaveCredentials = (url) => url.host === null || url.host === "" || url.scheme === "file";
  const shortenPath = (url) => {
    if (url.scheme === "file" && url.path.length === 1 && isNormalizedWindowsDriveLetter(url.path[0])) return;
    url.path.pop();
  };
  const S = {
    SCHEME_START: 1, SCHEME: 2, NO_SCHEME: 3, SPECIAL_RELATIVE_OR_AUTHORITY: 4, PATH_OR_AUTHORITY: 5, RELATIVE: 6,
    RELATIVE_SLASH: 7, SPECIAL_AUTHORITY_SLASHES: 8, SPECIAL_AUTHORITY_IGNORE_SLASHES: 9, AUTHORITY: 10, HOST: 11,
    HOSTNAME: 12, PORT: 13, FILE: 14, FILE_SLASH: 15, FILE_HOST: 16, PATH_START: 17, PATH: 18, OPAQUE_PATH: 19,
    QUERY: 20, FRAGMENT: 21,
  };
  // The WHATWG basic URL parser: returns a record, FAILURE, or undefined when a
  // state override (a setter) finishes early.
  const basicParse = (source, base, given = undefined, override = undefined) => {
    let input = source;
    let url = given;
    if (!url) {
      url = newRecord();
      input = input.replace(/^[\x00-\x20]+|[\x00-\x20]+$/g, "");
    }
    input = input.replace(/[\t\n\r]/g, "");
    let state = override ?? S.SCHEME_START;
    let buffer = "";
    let atSign = false;
    let insideBrackets = false;
    let passwordTokenSeen = false;
    const chars = Array.from(input);
    for (let at = 0; at <= chars.length; at += 1) {
      const c = chars[at];
      const isSpecial = isSpecialScheme(url.scheme);
      switch (state) {
        case S.SCHEME_START:
          if (isAlpha(c)) {
            buffer += c.toLowerCase();
            state = S.SCHEME;
          } else if (override === undefined) {
            state = S.NO_SCHEME;
            at -= 1;
          } else return FAILURE;
          break;
        case S.SCHEME:
          if (c !== undefined && /^[A-Za-z0-9+\-.]$/.test(c)) buffer += c.toLowerCase();
          else if (c === ":") {
            if (override !== undefined) {
              if (isSpecialScheme(url.scheme) !== isSpecialScheme(buffer)) return undefined;
              if ((includesCredentials(url) || url.port !== null) && buffer === "file") return undefined;
              if (url.scheme === "file" && url.host === "") return undefined;
            }
            url.scheme = buffer;
            if (override !== undefined) {
              if (url.port === defaultPort(url.scheme)) url.port = null;
              return undefined;
            }
            buffer = "";
            if (url.scheme === "file") state = S.FILE;
            else if (isSpecialScheme(url.scheme) && base && base.scheme === url.scheme) state = S.SPECIAL_RELATIVE_OR_AUTHORITY;
            else if (isSpecialScheme(url.scheme)) state = S.SPECIAL_AUTHORITY_SLASHES;
            else if (chars[at + 1] === "/") {
              state = S.PATH_OR_AUTHORITY;
              at += 1;
            } else {
              url.path = "";
              state = S.OPAQUE_PATH;
            }
          } else if (override === undefined) {
            buffer = "";
            state = S.NO_SCHEME;
            at = -1;
          } else return FAILURE;
          break;
        case S.NO_SCHEME:
          if (!base || (hasOpaquePath(base) && c !== "#")) return FAILURE;
          if (hasOpaquePath(base) && c === "#") {
            url.scheme = base.scheme;
            url.path = base.path;
            url.query = base.query;
            url.fragment = "";
            state = S.FRAGMENT;
          } else {
            state = base.scheme !== "file" ? S.RELATIVE : S.FILE;
            at -= 1;
          }
          break;
        case S.SPECIAL_RELATIVE_OR_AUTHORITY:
          if (c === "/" && chars[at + 1] === "/") {
            state = S.SPECIAL_AUTHORITY_IGNORE_SLASHES;
            at += 1;
          } else {
            state = S.RELATIVE;
            at -= 1;
          }
          break;
        case S.PATH_OR_AUTHORITY:
          if (c === "/") state = S.AUTHORITY;
          else {
            state = S.PATH;
            at -= 1;
          }
          break;
        case S.RELATIVE:
          url.scheme = base.scheme;
          if (c === "/" || (isSpecialScheme(url.scheme) && c === "\\")) state = S.RELATIVE_SLASH;
          else {
            url.username = base.username;
            url.password = base.password;
            url.host = base.host;
            url.port = base.port;
            url.path = [...base.path];
            url.query = base.query;
            if (c === "?") {
              url.query = "";
              state = S.QUERY;
            } else if (c === "#") {
              url.fragment = "";
              state = S.FRAGMENT;
            } else if (c !== undefined) {
              url.query = null;
              shortenPath(url);
              state = S.PATH;
              at -= 1;
            }
          }
          break;
        case S.RELATIVE_SLASH:
          if (isSpecial && (c === "/" || c === "\\")) state = S.SPECIAL_AUTHORITY_IGNORE_SLASHES;
          else if (c === "/") state = S.AUTHORITY;
          else {
            url.username = base.username;
            url.password = base.password;
            url.host = base.host;
            url.port = base.port;
            state = S.PATH;
            at -= 1;
          }
          break;
        case S.SPECIAL_AUTHORITY_SLASHES:
          state = S.SPECIAL_AUTHORITY_IGNORE_SLASHES;
          if (c === "/" && chars[at + 1] === "/") at += 1;
          else at -= 1;
          break;
        case S.SPECIAL_AUTHORITY_IGNORE_SLASHES:
          if (c !== "/" && c !== "\\") {
            state = S.AUTHORITY;
            at -= 1;
          }
          break;
        case S.AUTHORITY:
          if (c === "@") {
            if (atSign) buffer = `%40${buffer}`;
            atSign = true;
            for (const char of buffer) {
              if (char === ":" && !passwordTokenSeen) {
                passwordTokenSeen = true;
                continue;
              }
              const encoded = percentEncode(char, userinfoSet);
              if (passwordTokenSeen) url.password += encoded;
              else url.username += encoded;
            }
            buffer = "";
          } else if (c === undefined || c === "/" || c === "?" || c === "#" || (isSpecial && c === "\\")) {
            if (atSign && buffer === "") return FAILURE;
            at -= Array.from(buffer).length + 1;
            buffer = "";
            state = S.HOST;
          } else buffer += c;
          break;
        case S.HOST:
        case S.HOSTNAME:
          if (override !== undefined && url.scheme === "file") {
            at -= 1;
            state = S.FILE_HOST;
          } else if (c === ":" && !insideBrackets) {
            if (buffer === "") return FAILURE;
            if (override === S.HOSTNAME) return undefined;
            const parsed = parseHost(buffer, !isSpecial);
            if (parsed === FAILURE) return FAILURE;
            url.host = parsed;
            buffer = "";
            state = S.PORT;
          } else if (c === undefined || c === "/" || c === "?" || c === "#" || (isSpecial && c === "\\")) {
            at -= 1;
            if (isSpecial && buffer === "") return FAILURE;
            if (override !== undefined && buffer === "" && (includesCredentials(url) || url.port !== null)) return undefined;
            const parsed = parseHost(buffer, !isSpecial);
            if (parsed === FAILURE) return FAILURE;
            url.host = parsed;
            buffer = "";
            state = S.PATH_START;
            if (override !== undefined) return undefined;
          } else {
            if (c === "[") insideBrackets = true;
            if (c === "]") insideBrackets = false;
            buffer += c;
          }
          break;
        case S.PORT:
          if (isDigit(c)) buffer += c;
          else if (c === undefined || c === "/" || c === "?" || c === "#" || (isSpecial && c === "\\") || override !== undefined) {
            if (buffer !== "") {
              const port = parseInt(buffer, 10);
              if (port > 65535) return FAILURE;
              url.port = port === defaultPort(url.scheme) ? null : port;
              buffer = "";
            }
            if (override !== undefined) return undefined;
            state = S.PATH_START;
            at -= 1;
          } else return FAILURE;
          break;
        case S.FILE:
          url.scheme = "file";
          url.host = "";
          if (c === "/" || c === "\\") state = S.FILE_SLASH;
          else if (base && base.scheme === "file") {
            url.host = base.host;
            url.path = [...base.path];
            url.query = base.query;
            if (c === "?") {
              url.query = "";
              state = S.QUERY;
            } else if (c === "#") {
              url.fragment = "";
              state = S.FRAGMENT;
            } else if (c !== undefined) {
              url.query = null;
              if (!startsWithWindowsDriveLetter(chars, at)) shortenPath(url);
              else url.path = [];
              state = S.PATH;
              at -= 1;
            }
          } else {
            state = S.PATH;
            at -= 1;
          }
          break;
        case S.FILE_SLASH:
          if (c === "/" || c === "\\") state = S.FILE_HOST;
          else {
            if (base && base.scheme === "file") {
              url.host = base.host;
              if (!startsWithWindowsDriveLetter(chars, at) && isNormalizedWindowsDriveLetter(base.path[0] ?? "")) {
                url.path.push(base.path[0]);
              }
            }
            state = S.PATH;
            at -= 1;
          }
          break;
        case S.FILE_HOST:
          if (c === undefined || c === "/" || c === "\\" || c === "?" || c === "#") {
            at -= 1;
            if (override === undefined && isWindowsDriveLetter(buffer)) state = S.PATH;
            else if (buffer === "") {
              url.host = "";
              if (override !== undefined) return undefined;
              state = S.PATH_START;
            } else {
              let parsed = parseHost(buffer, false);
              if (parsed === FAILURE) return FAILURE;
              if (parsed === "localhost") parsed = "";
              url.host = parsed;
              if (override !== undefined) return undefined;
              buffer = "";
              state = S.PATH_START;
            }
          } else buffer += c;
          break;
        case S.PATH_START:
          if (isSpecial) {
            state = S.PATH;
            if (c !== "/" && c !== "\\") at -= 1;
          } else if (override === undefined && c === "?") {
            url.query = "";
            state = S.QUERY;
          } else if (override === undefined && c === "#") {
            url.fragment = "";
            state = S.FRAGMENT;
          } else if (c !== undefined) {
            state = S.PATH;
            if (c !== "/") at -= 1;
          } else if (override !== undefined && url.host === null) url.path.push("");
          break;
        case S.PATH: {
          const slash = c === "/" || (isSpecial && c === "\\");
          if (c === undefined || slash || (override === undefined && (c === "?" || c === "#"))) {
            if (isDoubleDot(buffer)) {
              shortenPath(url);
              if (!slash) url.path.push("");
            } else if (isSingleDot(buffer) && !slash) url.path.push("");
            else if (!isSingleDot(buffer)) {
              if (url.scheme === "file" && url.path.length === 0 && isWindowsDriveLetter(buffer)) buffer = `${buffer[0]}:`;
              url.path.push(buffer);
            }
            buffer = "";
            if (c === "?") {
              url.query = "";
              state = S.QUERY;
            }
            if (c === "#") {
              url.fragment = "";
              state = S.FRAGMENT;
            }
          } else buffer += percentEncode(c, pathSet);
          break;
        }
        case S.OPAQUE_PATH:
          if (c === "?") {
            url.query = "";
            state = S.QUERY;
          } else if (c === "#") {
            url.fragment = "";
            state = S.FRAGMENT;
          } else if (c !== undefined) url.path += percentEncode(c, c0);
          break;
        case S.QUERY:
          if ((override === undefined && c === "#") || c === undefined) {
            url.query += percentEncode(buffer, isSpecial ? specialQuerySet : querySet);
            buffer = "";
            if (c === "#") {
              url.fragment = "";
              state = S.FRAGMENT;
            }
          } else buffer += c;
          break;
        case S.FRAGMENT:
          if (c !== undefined) url.fragment += percentEncode(c, fragmentSet);
          break;
        default:
          return FAILURE;
      }
    }
    return url;
  };
  const serializePath = (url) => (hasOpaquePath(url) ? url.path : url.path.map((segment) => `/${segment}`).join(""));
  const serializeURL = (url, excludeFragment = false) => {
    let out = `${url.scheme}:`;
    if (url.host !== null) {
      out += "//";
      if (includesCredentials(url)) {
        out += url.username;
        if (url.password !== "") out += `:${url.password}`;
        out += "@";
      }
      out += url.host;
      if (url.port !== null) out += `:${url.port}`;
    } else if (!hasOpaquePath(url) && url.path.length > 1 && url.path[0] === "") out += "/.";
    out += serializePath(url);
    if (url.query !== null) out += `?${url.query}`;
    if (!excludeFragment && url.fragment !== null) out += `#${url.fragment}`;
    return out;
  };
  const originOf = (url) => {
    if (url.scheme === "blob") {
      const inner = basicParse(serializePath(url), null);
      if (inner !== FAILURE && (inner.scheme === "http" || inner.scheme === "https")) return originOf(inner);
      return "null";
    }
    if (["ftp", "http", "https", "ws", "wss"].includes(url.scheme)) {
      return `${url.scheme}://${url.host}${url.port === null ? "" : `:${url.port}`}`;
    }
    return "null";
  };
  const parseURL = (input, base) => {
    let baseRecord = null;
    if (base !== undefined) {
      baseRecord = basicParse(usvString(base), null);
      if (baseRecord === FAILURE) return FAILURE;
    }
    return basicParse(usvString(input), baseRecord);
  };

  const formEncode = (text) => {
    let out = "";
    for (const byte of utf8(text)) {
      if (byte === 0x20) out += "+";
      else if ((byte >= 0x30 && byte <= 0x39) || (byte >= 0x41 && byte <= 0x5a) || (byte >= 0x61 && byte <= 0x7a)
        || byte === 0x2a || byte === 0x2d || byte === 0x2e || byte === 0x5f) out += String.fromCharCode(byte);
      else out += `%${hex(byte)}`;
    }
    return out;
  };
  const formDecode = (text) => decodeUtf8(percentDecodeBytes(utf8(text.replace(/\+/g, " "))));
  const parseForm = (text) => {
    const list = [];
    for (const piece of text.split("&")) {
      if (piece === "") continue;
      const at = piece.indexOf("=");
      const name = at < 0 ? piece : piece.slice(0, at);
      const value = at < 0 ? "" : piece.slice(at + 1);
      list.push([formDecode(name), formDecode(value)]);
    }
    return list;
  };
  const serializeForm = (list) => list.map(([name, value]) => `${formEncode(name)}=${formEncode(value)}`).join("&");

  const iteratorPrototype = Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()));
  const iteratorPrototypes = new Map();
  const iteratorFor = (name, entries, pick) => {
    let prototype = iteratorPrototypes.get(name);
    if (!prototype) {
      prototype = Object.create(iteratorPrototype, {
        [Symbol.toStringTag]: { value: `${name} Iterator`, configurable: true },
        next: { value() { return this.__nanocodeNext(); }, writable: true, configurable: true },
      });
      iteratorPrototypes.set(name, prototype);
    }
    const iterator = entries.map(pick)[Symbol.iterator]();
    return Object.create(prototype, { __nanocodeNext: { value: () => iterator.next() } });
  };

  const paramsState = new WeakMap();
  function updateURLQuery(params) {
    const state = paramsState.get(params);
    if (!state.url) return;
    const text = serializeForm(state.list);
    state.url.query = text === "" ? null : text;
  }
  class URLSearchParams {
    constructor(init = undefined) {
      const state = { list: [], url: null };
      paramsState.set(this, state);
      if (init === undefined || init === null) return;
      if (isObject(init)) {
        if (init instanceof URLSearchParams) {
          state.list = paramsState.get(init).list.map((pair) => [...pair]);
        } else if (typeof init[Symbol.iterator] === "function") {
          for (const pair of init) {
            if (!isObject(pair) || typeof pair[Symbol.iterator] !== "function") {
              throw new TypeError("Each query pair must be an iterable [name, value] tuple");
            }
            const items = [...pair];
            if (items.length !== 2) throw new TypeError("Each query pair must be an iterable [name, value] tuple");
            state.list.push([usvString(items[0]), usvString(items[1])]);
          }
        } else {
          for (const key of Reflect.ownKeys(init)) {
            if (typeof key !== "string" || !Object.getOwnPropertyDescriptor(init, key)?.enumerable) continue;
            state.list.push([usvString(key), usvString(init[key])]);
          }
        }
        return;
      }
      const text = usvString(init);
      state.list = parseForm(text.startsWith("?") ? text.slice(1) : text);
    }
    get size() { return paramsState.get(this).list.length; }
    append(name, value) {
      requireArgs(arguments.length, 2, "URLSearchParams.append");
      paramsState.get(this).list.push([usvString(name), usvString(value)]);
      updateURLQuery(this);
    }
    delete(name, value = undefined) {
      requireArgs(arguments.length, 1, "URLSearchParams.delete");
      const state = paramsState.get(this);
      const key = usvString(name);
      const wanted = value === undefined ? undefined : usvString(value);
      state.list = state.list.filter(([n, v]) => n !== key || (wanted !== undefined && v !== wanted));
      updateURLQuery(this);
    }
    get(name) {
      requireArgs(arguments.length, 1, "URLSearchParams.get");
      const key = usvString(name);
      return paramsState.get(this).list.find(([n]) => n === key)?.[1] ?? null;
    }
    getAll(name) {
      requireArgs(arguments.length, 1, "URLSearchParams.getAll");
      const key = usvString(name);
      return paramsState.get(this).list.filter(([n]) => n === key).map(([, v]) => v);
    }
    has(name, value = undefined) {
      requireArgs(arguments.length, 1, "URLSearchParams.has");
      const key = usvString(name);
      const wanted = value === undefined ? undefined : usvString(value);
      return paramsState.get(this).list.some(([n, v]) => n === key && (wanted === undefined || v === wanted));
    }
    set(name, value) {
      requireArgs(arguments.length, 2, "URLSearchParams.set");
      const state = paramsState.get(this);
      const key = usvString(name);
      const text = usvString(value);
      const at = state.list.findIndex(([n]) => n === key);
      if (at < 0) state.list.push([key, text]);
      else {
        state.list[at][1] = text;
        state.list = state.list.filter(([n], index) => n !== key || index === at);
      }
      updateURLQuery(this);
    }
    sort() {
      const state = paramsState.get(this);
      state.list = state.list
        .map((pair, index) => [pair, index])
        .sort(([a, i], [b, j]) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : i - j))
        .map(([pair]) => pair);
      updateURLQuery(this);
    }
    forEach(callback, thisArg = undefined) {
      if (typeof callback !== "function") throw new TypeError("The \"callback\" argument must be of type function");
      const state = paramsState.get(this);
      for (let at = 0; at < state.list.length; at += 1) {
        const [name, value] = state.list[at];
        callback.call(thisArg, value, name, this);
      }
    }
    keys() { return iteratorFor("URLSearchParams", paramsState.get(this).list, ([n]) => n); }
    values() { return iteratorFor("URLSearchParams", paramsState.get(this).list, ([, v]) => v); }
    entries() { return iteratorFor("URLSearchParams", paramsState.get(this).list, ([n, v]) => [n, v]); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return serializeForm(paramsState.get(this).list); }
  }
  tag(URLSearchParams, "URLSearchParams");

  const urlState = new WeakMap();
  const blobURLs = new Map();
  const record = (url) => urlState.get(url).record;
  const invalidURL = (input, base) => {
    const error = new TypeError("Invalid URL");
    error.code = "ERR_INVALID_URL";
    error.input = input;
    if (base !== undefined) error.base = base;
    return error;
  };
  class URL {
    constructor(url, base = undefined) {
      requireArgs(arguments.length, 1, "URL constructor");
      const parsed = parseURL(url, base);
      if (parsed === FAILURE) throw invalidURL(usvString(url), base === undefined ? undefined : usvString(base));
      const params = new URLSearchParams(parsed.query ?? "");
      paramsState.get(params).url = parsed;
      urlState.set(this, { record: parsed, params });
    }
    static canParse(url, base = undefined) {
      requireArgs(arguments.length, 1, "URL.canParse");
      return parseURL(url, base) !== FAILURE;
    }
    static parse(url, base = undefined) {
      requireArgs(arguments.length, 1, "URL.parse");
      return parseURL(url, base) === FAILURE ? null : new URL(url, base);
    }
    static createObjectURL(blob) {
      if (!(blob instanceof Blob)) throw new TypeError("The \"obj\" argument must be an instance of Blob.");
      const id = `blob:nodedata:${crypto.randomUUID()}`;
      blobURLs.set(id, blob);
      return id;
    }
    static revokeObjectURL(url) {
      blobURLs.delete(`${url}`);
    }
    get href() { return serializeURL(record(this)); }
    set href(value) {
      const parsed = basicParse(usvString(value), null);
      if (parsed === FAILURE) throw invalidURL(usvString(value));
      const state = urlState.get(this);
      state.record = parsed;
      const params = paramsState.get(state.params);
      params.list = parseForm(parsed.query ?? "");
      params.url = parsed;
    }
    get origin() { return originOf(record(this)); }
    get protocol() { return `${record(this).scheme}:`; }
    set protocol(value) { basicParse(`${usvString(value)}:`, null, record(this), S.SCHEME_START); }
    get username() { return record(this).username; }
    set username(value) {
      const url = record(this);
      if (!cannotHaveCredentials(url)) url.username = percentEncode(usvString(value), userinfoSet);
    }
    get password() { return record(this).password; }
    set password(value) {
      const url = record(this);
      if (!cannotHaveCredentials(url)) url.password = percentEncode(usvString(value), userinfoSet);
    }
    get host() {
      const url = record(this);
      if (url.host === null) return "";
      return url.port === null ? url.host : `${url.host}:${url.port}`;
    }
    set host(value) {
      const url = record(this);
      if (!hasOpaquePath(url)) basicParse(usvString(value), null, url, S.HOST);
    }
    get hostname() { return record(this).host ?? ""; }
    set hostname(value) {
      const url = record(this);
      if (!hasOpaquePath(url)) basicParse(usvString(value), null, url, S.HOSTNAME);
    }
    get port() { const port = record(this).port; return port === null ? "" : `${port}`; }
    set port(value) {
      const url = record(this);
      if (cannotHaveCredentials(url)) return;
      const text = usvString(value);
      if (text === "") url.port = null;
      else basicParse(text, null, url, S.PORT);
    }
    get pathname() { return serializePath(record(this)); }
    set pathname(value) {
      const url = record(this);
      if (hasOpaquePath(url)) return;
      url.path = [];
      basicParse(usvString(value), null, url, S.PATH_START);
    }
    get search() { const query = record(this).query; return query === null || query === "" ? "" : `?${query}`; }
    set search(value) {
      const url = record(this);
      const params = paramsState.get(urlState.get(this).params);
      const text = usvString(value);
      if (text === "") {
        url.query = null;
        params.list = [];
        return;
      }
      const input = text.startsWith("?") ? text.slice(1) : text;
      url.query = "";
      basicParse(input, null, url, S.QUERY);
      params.list = parseForm(input);
    }
    get searchParams() { return urlState.get(this).params; }
    get hash() { const fragment = record(this).fragment; return fragment === null || fragment === "" ? "" : `#${fragment}`; }
    set hash(value) {
      const url = record(this);
      const text = usvString(value);
      if (text === "") {
        url.fragment = null;
        return;
      }
      url.fragment = "";
      basicParse(text.startsWith("#") ? text.slice(1) : text, null, url, S.FRAGMENT);
    }
    toString() { return this.href; }
    toJSON() { return this.href; }
  }
  for (const name of ["href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname",
    "search", "searchParams", "hash"]) {
    Object.defineProperty(URL.prototype, name, { ...Object.getOwnPropertyDescriptor(URL.prototype, name), enumerable: true });
  }
  tag(URL, "URL");


  // ------------------------------------------------------------------ crypto

  const integerArrays = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array,
    BigInt64Array, BigUint64Array];
  const randomBytes = (length) => new Uint8Array(random(length));
  const digestNames = { "sha-1": "SHA-1", "sha-256": "SHA-256", "sha-384": "SHA-384", "sha-512": "SHA-512" };
  const blockSize = { "SHA-1": 64, "SHA-256": 64, "SHA-384": 128, "SHA-512": 128 };
  const hashName = (algorithm) => {
    const name = typeof algorithm === "string" ? algorithm : algorithm?.name;
    const known = digestNames[`${name}`.toLowerCase()];
    if (!known) throw domError("Unrecognized algorithm name", "NotSupportedError");
    return known;
  };
  const bufferSource = (data, where) => {
    const bytes = bytesOf(data);
    if (!bytes) {
      throw new TypeError(`Failed to execute '${where}' on 'SubtleCrypto': parameter is not of type 'BufferSource'.`);
    }
    return bytes;
  };
  const hashBytes = (name, bytes) => new Uint8Array(digest(name, bytes.buffer, bytes.byteOffset, bytes.byteLength));
  const hmac = (name, key, data) => {
    const size = blockSize[name];
    const padded = new Uint8Array(size);
    padded.set(key.byteLength > size ? hashBytes(name, key) : key);
    const inner = padded.map((byte) => byte ^ 0x36);
    const outer = padded.map((byte) => byte ^ 0x5c);
    return hashBytes(name, concatBytes([outer, hashBytes(name, concatBytes([inner, data]))]));
  };
  const keyState = new WeakMap();
  class CryptoKey {
    constructor(key) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
    }
    get type() { return keyState.get(this).type; }
    get extractable() { return keyState.get(this).extractable; }
    get algorithm() { return keyState.get(this).algorithm; }
    get usages() { return keyState.get(this).usages; }
  }
  tag(CryptoKey, "CryptoKey");
  const hmacKey = (bytes, algorithm, extractable, usages) => {
    const hash = hashName(algorithm.hash);
    const key = new CryptoKey(illegal);
    keyState.set(key, {
      type: "secret",
      extractable: Boolean(extractable),
      algorithm: { name: "HMAC", length: bytes.byteLength * 8, hash: { name: hash } },
      usages: [...usages],
      bytes: new Uint8Array(bytes),
    });
    return key;
  };
  const isHmac = (algorithm) => `${typeof algorithm === "string" ? algorithm : algorithm?.name}`.toUpperCase() === "HMAC";
  const unsupported = () => Promise.reject(domError("Unrecognized algorithm name", "NotSupportedError"));
  class SubtleCrypto {
    constructor(key) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
    }
    digest(algorithm, data) {
      const count = arguments.length;
      return attempt(() => {
        requireArgs(count, 2, "SubtleCrypto.digest");
        return hashBytes(hashName(algorithm), bufferSource(data, "digest")).slice().buffer;
      });
    }
    importKey(format, keyData, algorithm, extractable, usages) {
      const count = arguments.length;
      return attempt(() => {
        requireArgs(count, 5, "SubtleCrypto.importKey");
        if (!isHmac(algorithm)) throw domError("Unrecognized algorithm name", "NotSupportedError");
        if (format !== "raw") throw domError(`Unsupported key format ${format} for HMAC`, "NotSupportedError");
        return hmacKey(bufferSource(keyData, "importKey"), algorithm, extractable, usages);
      });
    }
    exportKey(format, key) {
      return attempt(() => {
        const state = keyState.get(key);
        if (!state) throw new TypeError("The \"key\" argument must be an instance of CryptoKey.");
        if (!state.extractable) throw domError("key is not extractable", "InvalidAccessError");
        if (format !== "raw") throw domError(`Unsupported key format ${format}`, "NotSupportedError");
        return state.bytes.slice().buffer;
      });
    }
    generateKey(algorithm, extractable, usages) {
      return attempt(() => {
        if (!isHmac(algorithm)) throw domError("Unrecognized algorithm name", "NotSupportedError");
        const length = algorithm.length ?? blockSize[hashName(algorithm.hash)] * 8;
        return hmacKey(randomBytes(Math.ceil(length / 8)), algorithm, extractable, usages);
      });
    }
    sign(algorithm, key, data) {
      return attempt(() => {
        const state = keyState.get(key);
        if (!isHmac(algorithm) || !state) throw domError("Unrecognized algorithm name", "NotSupportedError");
        if (!state.usages.includes("sign")) throw domError("Unable to use this key to sign", "InvalidAccessError");
        return hmac(state.algorithm.hash.name, state.bytes, bufferSource(data, "sign")).buffer;
      });
    }
    verify(algorithm, key, signature, data) {
      return attempt(() => {
        const state = keyState.get(key);
        if (!isHmac(algorithm) || !state) throw domError("Unrecognized algorithm name", "NotSupportedError");
        if (!state.usages.includes("verify")) throw domError("Unable to use this key to verify", "InvalidAccessError");
        const expected = hmac(state.algorithm.hash.name, state.bytes, bufferSource(data, "verify"));
        const given = bufferSource(signature, "verify");
        return expected.length === given.length && expected.every((byte, at) => byte === given[at]);
      });
    }
  }
  for (const name of ["encrypt", "decrypt", "deriveBits", "deriveKey", "wrapKey", "unwrapKey"]) {
    define(SubtleCrypto.prototype, name, unsupported);
  }
  tag(SubtleCrypto, "SubtleCrypto");
  const subtle = new SubtleCrypto(illegal);
  class Crypto {
    constructor(key) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
    }
    get subtle() { return subtle; }
    getRandomValues(array) {
      requireArgs(arguments.length, 1, "Crypto.getRandomValues");
      if (!integerArrays.some((Type) => array instanceof Type)) {
        throw domError("The data argument must be an integer-type TypedArray", "TypeMismatchError");
      }
      if (array.byteLength > 65536) {
        throw domError(`The ArrayBufferView's byte length (${array.byteLength}) exceeds the number of bytes of entropy available via this API (65536)`, "QuotaExceededError");
      }
      new Uint8Array(array.buffer, array.byteOffset, array.byteLength).set(randomBytes(array.byteLength));
      return array;
    }
    randomUUID() {
      const bytes = randomBytes(16);
      bytes[6] = (bytes[6] & 0x0f) | 0x40;
      bytes[8] = (bytes[8] & 0x3f) | 0x80;
      const text = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
      return `${text.slice(0, 8)}-${text.slice(8, 12)}-${text.slice(12, 16)}-${text.slice(16, 20)}-${text.slice(20)}`;
    }
  }
  tag(Crypto, "Crypto");
  const crypto = new Crypto(illegal);

  // -------------------------------------------------------------- Blob, File

  const blobBytes = new WeakMap();
  const normalizeType = (type) => {
    const text = `${type ?? ""}`;
    return /[^\x20-\x7e]/.test(text) ? "" : text.toLowerCase();
  };
  class Blob {
    constructor(parts = undefined, options = undefined) {
      if (parts !== undefined && (!isObject(parts) || typeof parts[Symbol.iterator] !== "function")) {
        throw new TypeError("The \"sources\" argument must be a sequence.");
      }
      const chunks = [];
      for (const part of parts ?? []) {
        const view = bytesOf(part);
        if (view) chunks.push(view.slice());
        else if (part instanceof Blob) chunks.push(blobBytes.get(part).bytes);
        else {
          let text = usvString(part);
          if (options?.endings === "native") text = text.replace(/\r\n?/g, "\n");
          chunks.push(utf8(text));
        }
      }
      blobBytes.set(this, { bytes: concatBytes(chunks), type: normalizeType(options?.type) });
    }
    get size() { return blobBytes.get(this).bytes.byteLength; }
    get type() { return blobBytes.get(this).type; }
    slice(start = undefined, end = undefined, contentType = "") {
      const bytes = blobBytes.get(this).bytes;
      const size = bytes.byteLength;
      const clamp = (value, fallback) => {
        if (value === undefined) return fallback;
        const number = Math.trunc(Number(value)) || 0;
        return number < 0 ? Math.max(size + number, 0) : Math.min(number, size);
      };
      const from = clamp(start, 0);
      const to = clamp(end, size);
      const out = new Blob([], { type: contentType });
      blobBytes.get(out).bytes = bytes.slice(from, Math.max(from, to));
      return out;
    }
    text() { return Promise.resolve(decodeUtf8(blobBytes.get(this).bytes)); }
    arrayBuffer() { return Promise.resolve(blobBytes.get(this).bytes.slice().buffer); }
    bytes() { return Promise.resolve(blobBytes.get(this).bytes.slice()); }
    stream() { return streamOfBytes(blobBytes.get(this).bytes.slice()); }
  }
  tag(Blob, "Blob");
  class File extends Blob {
    #name;
    #lastModified;
    constructor(parts, name, options = undefined) {
      requireArgs(arguments.length, 2, "File constructor");
      super(parts, options);
      this.#name = usvString(name);
      this.#lastModified = options?.lastModified === undefined ? clockNow() : Math.trunc(Number(options.lastModified)) || 0;
    }
    get name() { return this.#name; }
    get lastModified() { return this.#lastModified; }
    get webkitRelativePath() { return ""; }
  }
  tag(File, "File");

  // ------------------------------------------------------------------ FormData

  const formState = new WeakMap();
  const formEntry = (name, value, filename, count) => {
    if (value instanceof Blob) {
      if (!(value instanceof File) || filename !== undefined) {
        const fileName = filename !== undefined ? usvString(filename) : value instanceof File ? value.name : "blob";
        value = new File([value], fileName, { type: value.type, lastModified: value instanceof File ? value.lastModified : undefined });
      }
      return [usvString(name), value];
    }
    if (count > 2) throw new TypeError("The \"value\" argument must be an instance of Blob when a filename is given.");
    return [usvString(name), usvString(value)];
  };
  class FormData {
    constructor(form = undefined) {
      if (form !== undefined) throw new TypeError("The \"form\" argument must be undefined: cells have no HTML forms.");
      formState.set(this, []);
    }
    append(name, value, filename = undefined) {
      requireArgs(arguments.length, 2, "FormData.append");
      formState.get(this).push(formEntry(name, value, filename, arguments.length));
    }
    set(name, value, filename = undefined) {
      requireArgs(arguments.length, 2, "FormData.set");
      const entry = formEntry(name, value, filename, arguments.length);
      const list = formState.get(this);
      const at = list.findIndex(([n]) => n === entry[0]);
      if (at < 0) list.push(entry);
      else formState.set(this, list.filter(([n], index) => n !== entry[0] || index === at).map((item) => (item[0] === entry[0] ? entry : item)));
    }
    delete(name) {
      requireArgs(arguments.length, 1, "FormData.delete");
      const key = usvString(name);
      formState.set(this, formState.get(this).filter(([n]) => n !== key));
    }
    get(name) {
      requireArgs(arguments.length, 1, "FormData.get");
      const key = usvString(name);
      return formState.get(this).find(([n]) => n === key)?.[1] ?? null;
    }
    getAll(name) {
      requireArgs(arguments.length, 1, "FormData.getAll");
      const key = usvString(name);
      return formState.get(this).filter(([n]) => n === key).map(([, v]) => v);
    }
    has(name) {
      requireArgs(arguments.length, 1, "FormData.has");
      const key = usvString(name);
      return formState.get(this).some(([n]) => n === key);
    }
    forEach(callback, thisArg = undefined) {
      for (const [name, value] of formState.get(this)) callback.call(thisArg, value, name, this);
    }
    keys() { return iteratorFor("FormData", formState.get(this), ([n]) => n); }
    values() { return iteratorFor("FormData", formState.get(this), ([, v]) => v); }
    entries() { return iteratorFor("FormData", formState.get(this), ([n, v]) => [n, v]); }
    [Symbol.iterator]() { return this.entries(); }
  }
  tag(FormData, "FormData");
  const escapeMultipart = (text) => text.replace(/\n/g, "%0A").replace(/\r/g, "%0D").replace(/"/g, "%22");
  const latin1 = (text) => Uint8Array.from(text, (char) => char.charCodeAt(0));
  const encodeMultipart = (form) => {
    const boundary = `----formdata-nanocode-${Array.from(randomBytes(8), (byte) => `${byte}`.padStart(3, "0")).join("")}`;
    const chunks = [];
    for (const [name, value] of formState.get(form)) {
      let head = `--${boundary}\r\nContent-Disposition: form-data; name="${escapeMultipart(name)}"`;
      if (typeof value === "string") {
        chunks.push(utf8(`${head}\r\n\r\n${value.replace(/\r\n|\r|\n/g, "\r\n")}\r\n`));
      } else {
        head += `; filename="${escapeMultipart(value.name)}"\r\nContent-Type: ${value.type || "application/octet-stream"}\r\n\r\n`;
        chunks.push(utf8(head), blobBytes.get(value).bytes, utf8("\r\n"));
      }
    }
    chunks.push(utf8(`--${boundary}--\r\n`));
    return { bytes: concatBytes(chunks), type: `multipart/form-data; boundary=${boundary}` };
  };
  const decodeMultipart = (bytes, type) => {
    const boundary = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(type);
    if (!boundary) throw new TypeError("Failed to parse body as FormData.");
    const text = Array.from(bytes, (byte) => String.fromCharCode(byte)).join("");
    const form = new FormData();
    for (const part of text.split(`--${boundary[1] ?? boundary[2]}`).slice(1)) {
      if (part.startsWith("--")) break;
      const split = part.indexOf("\r\n\r\n");
      if (split < 0) continue;
      const head = part.slice(2, split);
      const body = latin1(part.slice(split + 4, part.endsWith("\r\n") ? -2 : undefined));
      const name = decodeUtf8(latin1(/name="([^"]*)"/i.exec(head)?.[1] ?? ""));
      const filename = /filename="([^"]*)"/i.exec(head)?.[1];
      if (filename === undefined) form.append(name, decodeUtf8(body));
      else {
        const fileType = /content-type:\s*([^\r\n]*)/i.exec(head)?.[1] ?? "";
        form.append(name, new File([body], decodeUtf8(latin1(filename)), { type: fileType }));
      }
    }
    return form;
  };


  // ----------------------------------------------------------------- streams

  const deferred = () => {
    let resolve;
    let reject;
    const promise = new Promise((yes, no) => {
      resolve = yes;
      reject = no;
    });
    promise.catch(() => {});
    return { promise, resolve, reject };
  };
  const streamState = new WeakMap();
  const closeStream = (stream) => {
    const state = streamState.get(stream);
    if (state.state !== "readable") return;
    state.state = "closed";
    for (const read of state.reads.splice(0)) read.resolve({ value: undefined, done: true });
    state.closed.resolve();
  };
  const errorStream = (stream, reason) => {
    const state = streamState.get(stream);
    if (state.state !== "readable") return;
    state.state = "errored";
    state.error = reason;
    state.queue = [];
    state.queueSize = 0;
    for (const read of state.reads.splice(0)) read.reject(reason);
    state.closed.reject(reason);
  };
  const pullIfNeeded = (stream) => {
    const state = streamState.get(stream);
    if (!state.started || state.state !== "readable" || state.closeRequested || !state.source.pull) return;
    if (state.reads.length === 0 && state.highWaterMark - state.queueSize <= 0) return;
    if (state.pulling) {
      state.pullAgain = true;
      return;
    }
    state.pulling = true;
    attempt(() => state.source.pull.call(state.underlying, state.controller)).then(() => {
      state.pulling = false;
      if (state.pullAgain) {
        state.pullAgain = false;
        pullIfNeeded(stream);
      }
    }, (error) => errorStream(stream, error));
  };
  const cancelStream = (stream, reason) => {
    const state = streamState.get(stream);
    state.disturbed = true;
    if (state.state === "closed") return Promise.resolve();
    if (state.state === "errored") return Promise.reject(state.error);
    state.queue = [];
    state.queueSize = 0;
    closeStream(stream);
    return attempt(() => state.source.cancel?.call(state.underlying, reason)).then(() => undefined);
  };
  class ReadableStreamDefaultController {
    #stream;
    constructor(key, stream) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
      this.#stream = stream;
    }
    get desiredSize() {
      const state = streamState.get(this.#stream);
      if (state.state === "errored") return null;
      if (state.state === "closed") return 0;
      return state.highWaterMark - state.queueSize;
    }
    enqueue(chunk = undefined) {
      const state = streamState.get(this.#stream);
      if (state.closeRequested || state.state !== "readable") throw new TypeError("Invalid state: Controller is already closed");
      if (state.reads.length > 0) state.reads.shift().resolve({ value: chunk, done: false });
      else {
        const size = state.size(chunk);
        state.queue.push([chunk, size]);
        state.queueSize += size;
      }
      pullIfNeeded(this.#stream);
    }
    close() {
      const state = streamState.get(this.#stream);
      if (state.closeRequested || state.state !== "readable") throw new TypeError("Invalid state: Controller is already closed");
      state.closeRequested = true;
      if (state.queue.length === 0) closeStream(this.#stream);
    }
    error(reason = undefined) {
      errorStream(this.#stream, reason);
    }
  }
  tag(ReadableStreamDefaultController, "ReadableStreamDefaultController");
  const detached = () => Promise.reject(new TypeError("Invalid state: The reader is not attached to a stream"));
  class ReadableStreamDefaultReader {
    #stream;
    constructor(stream) {
      if (!(stream instanceof ReadableStream)) throw new TypeError("The \"stream\" argument must be an instance of ReadableStream.");
      const state = streamState.get(stream);
      if (state.reader) throw new TypeError("Invalid state: ReadableStream is locked");
      state.reader = this;
      this.#stream = stream;
    }
    get closed() { return this.#stream ? streamState.get(this.#stream).closed.promise : detached(); }
    read() {
      if (!this.#stream) return detached();
      const stream = this.#stream;
      const state = streamState.get(stream);
      state.disturbed = true;
      if (state.queue.length > 0) {
        const [chunk, size] = state.queue.shift();
        state.queueSize -= size;
        if (state.closeRequested && state.queue.length === 0) closeStream(stream);
        else pullIfNeeded(stream);
        return Promise.resolve({ value: chunk, done: false });
      }
      if (state.state === "closed") return Promise.resolve({ value: undefined, done: true });
      if (state.state === "errored") return Promise.reject(state.error);
      const read = deferred();
      state.reads.push(read);
      pullIfNeeded(stream);
      return read.promise;
    }
    releaseLock() {
      if (!this.#stream) return;
      const state = streamState.get(this.#stream);
      for (const read of state.reads.splice(0)) read.reject(new TypeError("Invalid state: Releasing reader"));
      state.reader = null;
      this.#stream = null;
    }
    cancel(reason = undefined) {
      return this.#stream ? cancelStream(this.#stream, reason) : detached();
    }
  }
  tag(ReadableStreamDefaultReader, "ReadableStreamDefaultReader");
  class ReadableStream {
    constructor(underlyingSource = undefined, strategy = undefined) {
      const source = underlyingSource ?? {};
      const state = {
        state: "readable", queue: [], queueSize: 0, reads: [], reader: null, error: undefined,
        closeRequested: false, started: false, pulling: false, pullAgain: false, disturbed: false,
        highWaterMark: strategy?.highWaterMark ?? (source.type === "bytes" ? 0 : 1),
        size: typeof strategy?.size === "function" ? strategy.size : () => 1,
        source, underlying: underlyingSource, closed: deferred(), controller: null,
      };
      streamState.set(this, state);
      state.controller = new ReadableStreamDefaultController(illegal, this);
      const started = source.start ? source.start.call(underlyingSource, state.controller) : undefined;
      Promise.resolve(started).then(() => {
        state.started = true;
        pullIfNeeded(this);
      }, (error) => errorStream(this, error));
    }
    get locked() { return streamState.get(this).reader !== null; }
    getReader(options = undefined) {
      if (options?.mode !== undefined && options.mode !== "byob") throw new TypeError(`Invalid reader mode ${options.mode}`);
      return new ReadableStreamDefaultReader(this);
    }
    cancel(reason = undefined) {
      if (this.locked) return Promise.reject(new TypeError("Invalid state: ReadableStream is locked"));
      return cancelStream(this, reason);
    }
    values(options = undefined) {
      const reader = this.getReader();
      const preventCancel = Boolean(options?.preventCancel);
      return {
        next: () => reader.read().then((result) => {
          if (result.done) reader.releaseLock();
          return result;
        }),
        return: async (value) => {
          if (!preventCancel) await reader.cancel(value);
          reader.releaseLock();
          return { value, done: true };
        },
        [Symbol.asyncIterator]() { return this; },
      };
    }
    [Symbol.asyncIterator](options = undefined) { return this.values(options); }
    tee() {
      const reader = this.getReader();
      const controllers = [];
      let reading = false;
      const pull = () => {
        if (reading) return Promise.resolve();
        reading = true;
        return reader.read().then(({ value, done }) => {
          reading = false;
          for (const controller of controllers) {
            try {
              if (done) controller.close();
              else controller.enqueue(value);
            } catch {
              // a cancelled branch takes no more chunks
            }
          }
        }, (error) => {
          for (const controller of controllers) controller.error(error);
        });
      };
      const branch = () => new ReadableStream({ start(controller) { controllers.push(controller); }, pull });
      return [branch(), branch()];
    }
    async pipeTo(destination, options = undefined) {
      const reader = this.getReader();
      const writer = destination.getWriter();
      try {
        for (;;) {
          if (options?.signal?.aborted) throw options.signal.reason;
          const { value, done } = await reader.read();
          if (done) break;
          await writer.write(value);
        }
        if (!options?.preventClose) await writer.close();
      } catch (error) {
        if (!options?.preventAbort) await writer.abort(error).catch(() => {});
        throw error;
      } finally {
        reader.releaseLock();
        writer.releaseLock();
      }
    }
    pipeThrough(transform, options = undefined) {
      this.pipeTo(transform.writable, options).catch(() => {});
      return transform.readable;
    }
    static from(source) {
      if (source instanceof ReadableStream) return source;
      const iterator = source[Symbol.asyncIterator]?.() ?? source[Symbol.iterator]();
      return new ReadableStream({
        async pull(controller) {
          const { value, done } = await iterator.next();
          if (done) controller.close();
          else controller.enqueue(value);
        },
        async cancel(reason) {
          await iterator.return?.(reason);
        },
      }, { highWaterMark: 0 });
    }
  }
  tag(ReadableStream, "ReadableStream");
  function streamOfBytes(bytes) {
    return new ReadableStream({
      start(controller) {
        if (bytes.byteLength > 0) controller.enqueue(bytes);
        controller.close();
      },
    });
  }

  const writableState = new WeakMap();
  const errorWritable = (state, reason) => {
    if (state.state !== "writable") return;
    state.state = "errored";
    state.error = reason;
    state.closed.reject(reason);
  };
  class WritableStreamDefaultController {
    #state;
    constructor(key, state) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
      this.#state = state;
    }
    get signal() { return this.#state.abort.signal; }
    error(reason = undefined) { errorWritable(this.#state, reason); }
  }
  tag(WritableStreamDefaultController, "WritableStreamDefaultController");
  class WritableStreamDefaultWriter {
    #stream;
    constructor(stream) {
      const state = writableState.get(stream);
      if (!state) throw new TypeError("The \"stream\" argument must be an instance of WritableStream.");
      if (state.writer) throw new TypeError("Invalid state: WritableStream is locked");
      state.writer = this;
      this.#stream = stream;
    }
    #state() {
      if (!this.#stream) throw new TypeError("Invalid state: The writer is not attached to a stream");
      return writableState.get(this.#stream);
    }
    get closed() { return attempt(() => this.#state().closed.promise); }
    get ready() { return Promise.resolve(); }
    get desiredSize() { const state = this.#state(); return state.state === "writable" ? state.highWaterMark - state.pending : 0; }
    write(chunk = undefined) {
      return attempt(() => {
        const state = this.#state();
        if (state.state === "errored") throw state.error;
        if (state.state !== "writable" || state.closing) throw new TypeError("Invalid state: WritableStream is closed");
        state.pending += 1;
        state.chain = state.chain.then(() => {
          if (state.state === "errored") throw state.error;
          return state.sink.write?.call(state.underlying, chunk, state.controller);
        }).then(() => {
          state.pending -= 1;
        }, (error) => {
          state.pending -= 1;
          errorWritable(state, error);
          throw error;
        });
        return state.chain;
      });
    }
    close() {
      return attempt(() => {
        const state = this.#state();
        if (state.state !== "writable" || state.closing) throw new TypeError("Invalid state: WritableStream is closed");
        state.closing = true;
        state.chain = state.chain.catch(() => {}).then(() => {
          if (state.state === "errored") throw state.error;
          return state.sink.close?.call(state.underlying);
        }).then(() => {
          state.state = "closed";
          state.closed.resolve();
        });
        return state.chain;
      });
    }
    abort(reason = undefined) {
      return attempt(() => {
        const state = this.#state();
        if (state.state !== "writable") return undefined;
        state.abort.abort(reason);
        errorWritable(state, reason);
        return state.sink.abort?.call(state.underlying, reason);
      }).then(() => undefined);
    }
    releaseLock() {
      if (!this.#stream) return;
      writableState.get(this.#stream).writer = null;
      this.#stream = null;
    }
  }
  tag(WritableStreamDefaultWriter, "WritableStreamDefaultWriter");
  class WritableStream {
    constructor(underlyingSink = undefined, strategy = undefined) {
      const sink = underlyingSink ?? {};
      const state = {
        state: "writable", error: undefined, writer: null, sink, underlying: underlyingSink, pending: 0,
        closing: false, highWaterMark: strategy?.highWaterMark ?? 1, closed: deferred(), abort: new AbortController(),
        controller: null, chain: Promise.resolve(),
      };
      writableState.set(this, state);
      state.controller = new WritableStreamDefaultController(illegal, state);
      state.chain = attempt(() => sink.start?.call(underlyingSink, state.controller));
      state.chain.catch((error) => errorWritable(state, error));
    }
    get locked() { return writableState.get(this).writer !== null; }
    getWriter() { return new WritableStreamDefaultWriter(this); }
    close() {
      if (this.locked) return Promise.reject(new TypeError("Invalid state: WritableStream is locked"));
      const writer = this.getWriter();
      return writer.close().finally(() => writer.releaseLock());
    }
    abort(reason = undefined) {
      if (this.locked) return Promise.reject(new TypeError("Invalid state: WritableStream is locked"));
      const writer = this.getWriter();
      return writer.abort(reason).finally(() => writer.releaseLock());
    }
  }
  tag(WritableStream, "WritableStream");

  class TransformStreamDefaultController {
    #readable;
    constructor(key, readable) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
      this.#readable = readable;
    }
    get desiredSize() { return this.#readable.desiredSize; }
    enqueue(chunk = undefined) { this.#readable.enqueue(chunk); }
    error(reason = undefined) { this.#readable.error(reason); }
    terminate() {
      try {
        this.#readable.close();
      } catch {
        // already closed
      }
    }
  }
  tag(TransformStreamDefaultController, "TransformStreamDefaultController");
  class TransformStream {
    #readable;
    #writable;
    constructor(transformer = undefined, writableStrategy = undefined, readableStrategy = undefined) {
      const methods = transformer ?? {};
      let readableController;
      this.#readable = new ReadableStream({ start(controller) { readableController = controller; } },
        readableStrategy ?? { highWaterMark: 0 });
      const controller = new TransformStreamDefaultController(illegal, readableController);
      this.#writable = new WritableStream({
        start() { return methods.start?.call(transformer, controller); },
        write(chunk) {
          if (methods.transform) return methods.transform.call(transformer, chunk, controller);
          controller.enqueue(chunk);
          return undefined;
        },
        async close() {
          await methods.flush?.call(transformer, controller);
          controller.terminate();
        },
        abort(reason) { readableController.error(reason); },
      }, writableStrategy);
    }
    get readable() { return this.#readable; }
    get writable() { return this.#writable; }
  }
  tag(TransformStream, "TransformStream");
  class TextEncoderStream {
    #transform;
    constructor() {
      let pending = "";
      this.#transform = new TransformStream({
        transform(chunk, controller) {
          let text = pending + webidlString(chunk);
          pending = "";
          const last = text.charCodeAt(text.length - 1);
          if (last >= 0xd800 && last <= 0xdbff) {
            pending = text.slice(-1);
            text = text.slice(0, -1);
          }
          if (text !== "") controller.enqueue(utf8(text));
        },
        flush(controller) {
          if (pending !== "") controller.enqueue(utf8(pending));
        },
      });
    }
    get encoding() { return "utf-8"; }
    get readable() { return this.#transform.readable; }
    get writable() { return this.#transform.writable; }
  }
  tag(TextEncoderStream, "TextEncoderStream");
  class TextDecoderStream {
    #transform;
    #decoder;
    constructor(label = "utf-8", options = undefined) {
      const decoder = new TextDecoder(label, options);
      this.#decoder = decoder;
      this.#transform = new TransformStream({
        transform(chunk, controller) {
          const text = decoder.decode(bytesOf(chunk) ?? chunk, { stream: true });
          if (text !== "") controller.enqueue(text);
        },
        flush(controller) {
          const text = decoder.decode();
          if (text !== "") controller.enqueue(text);
        },
      });
    }
    get encoding() { return this.#decoder.encoding; }
    get fatal() { return this.#decoder.fatal; }
    get ignoreBOM() { return this.#decoder.ignoreBOM; }
    get readable() { return this.#transform.readable; }
    get writable() { return this.#transform.writable; }
  }
  tag(TextDecoderStream, "TextDecoderStream");
  class CountQueuingStrategy {
    #highWaterMark;
    constructor(init) { this.#highWaterMark = Number(init?.highWaterMark); }
    get highWaterMark() { return this.#highWaterMark; }
    get size() { return () => 1; }
  }
  class ByteLengthQueuingStrategy {
    #highWaterMark;
    constructor(init) { this.#highWaterMark = Number(init?.highWaterMark); }
    get highWaterMark() { return this.#highWaterMark; }
    get size() { return (chunk) => chunk.byteLength; }
  }


  // ------------------------------------------------- Headers, Request, Response

  const token = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
  const headerState = new WeakMap();
  const headerName = (name, where) => {
    const text = webidlString(name);
    if (!token.test(text)) throw new TypeError(`Headers.${where}: "${text}" is an invalid header name.`);
    return text.toLowerCase();
  };
  const headerValue = (value, where) => {
    const text = webidlString(value).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, "");
    if (/[\0\n\r]/.test(text)) throw new TypeError(`Headers.${where}: "${text}" is an invalid header value.`);
    return text;
  };
  const checkMutable = (headers) => {
    if (headerState.get(headers).immutable) throw new TypeError("immutable");
  };
  class Headers {
    constructor(init = undefined) {
      headerState.set(this, { list: [], immutable: false });
      if (init === undefined || init === null) return;
      if (!isObject(init)) throw new TypeError("The \"init\" argument must be of type object.");
      if (init instanceof Headers) {
        headerState.get(this).list = headerState.get(init).list.map((pair) => [...pair]);
      } else if (typeof init[Symbol.iterator] === "function") {
        for (const pair of init) {
          const items = [...pair];
          if (items.length !== 2) throw new TypeError(`Headers constructor: expected name/value pair to be length 2, found ${items.length}.`);
          this.append(items[0], items[1]);
        }
      } else {
        for (const key of Reflect.ownKeys(init)) {
          if (typeof key !== "string" || !Object.getOwnPropertyDescriptor(init, key)?.enumerable) continue;
          this.append(key, init[key]);
        }
      }
    }
    append(name, value) {
      requireArgs(arguments.length, 2, "Headers.append");
      checkMutable(this);
      headerState.get(this).list.push([headerName(name, "append"), headerValue(value, "append")]);
    }
    delete(name) {
      requireArgs(arguments.length, 1, "Headers.delete");
      checkMutable(this);
      const key = headerName(name, "delete");
      const state = headerState.get(this);
      state.list = state.list.filter(([n]) => n !== key);
    }
    get(name) {
      requireArgs(arguments.length, 1, "Headers.get");
      const key = headerName(name, "get");
      const values = headerState.get(this).list.filter(([n]) => n === key).map(([, v]) => v);
      return values.length === 0 ? null : values.join(", ");
    }
    getSetCookie() {
      return headerState.get(this).list.filter(([n]) => n === "set-cookie").map(([, v]) => v);
    }
    has(name) {
      requireArgs(arguments.length, 1, "Headers.has");
      const key = headerName(name, "has");
      return headerState.get(this).list.some(([n]) => n === key);
    }
    set(name, value) {
      requireArgs(arguments.length, 2, "Headers.set");
      checkMutable(this);
      const key = headerName(name, "set");
      const text = headerValue(value, "set");
      const state = headerState.get(this);
      const at = state.list.findIndex(([n]) => n === key);
      if (at < 0) state.list.push([key, text]);
      else {
        state.list[at][1] = text;
        state.list = state.list.filter(([n], index) => n !== key || index === at);
      }
    }
    #sorted() {
      const names = [...new Set(headerState.get(this).list.map(([n]) => n))].sort();
      const out = [];
      for (const name of names) {
        if (name === "set-cookie") for (const value of this.getSetCookie()) out.push([name, value]);
        else out.push([name, this.get(name)]);
      }
      return out;
    }
    forEach(callback, thisArg = undefined) {
      for (const [name, value] of this.#sorted()) callback.call(thisArg, value, name, this);
    }
    keys() { return iteratorFor("Headers", this.#sorted(), ([n]) => n); }
    values() { return iteratorFor("Headers", this.#sorted(), ([, v]) => v); }
    entries() { return iteratorFor("Headers", this.#sorted(), ([n, v]) => [n, v]); }
    [Symbol.iterator]() { return this.entries(); }
  }
  tag(Headers, "Headers");

  const extractBody = (body) => {
    if (typeof body === "string") return { source: utf8(body), type: "text/plain;charset=UTF-8" };
    if (body instanceof URLSearchParams) {
      return { source: utf8(body.toString()), type: "application/x-www-form-urlencoded;charset=UTF-8" };
    }
    if (body instanceof Blob) return { source: blobBytes.get(body).bytes.slice(), type: body.type || null };
    if (body instanceof FormData) {
      const { bytes, type } = encodeMultipart(body);
      return { source: bytes, type };
    }
    if (body instanceof ReadableStream) {
      if (body.locked || streamState.get(body).disturbed) throw new TypeError("Response body object should not be disturbed or locked");
      return { source: body, type: null };
    }
    const view = bytesOf(body);
    if (view) return { source: view.slice(), type: null };
    return { source: utf8(webidlString(body)), type: "text/plain;charset=UTF-8" };
  };
  const bodyState = new WeakMap();
  const bodyStream = (owner) => {
    const state = bodyState.get(owner);
    if (state.source === null) return null;
    if (!state.stream) state.stream = state.source instanceof ReadableStream ? state.source : streamOfBytes(state.source);
    return state.stream;
  };
  const isUsed = (owner) => {
    const state = bodyState.get(owner);
    return state.used || (state.stream !== null && streamState.get(state.stream).disturbed);
  };
  const consume = async (owner) => {
    const state = bodyState.get(owner);
    if (isUsed(owner) || (state.stream && state.stream.locked)) throw new TypeError("Body is unusable: Body has already been read");
    state.used = true;
    if (state.source === null) return new Uint8Array(0);
    if (!state.stream && state.source instanceof Uint8Array) return state.source;
    const chunks = [];
    for await (const chunk of bodyStream(owner)) {
      const view = bytesOf(chunk);
      if (!view) throw new TypeError("Received non-Uint8Array chunk");
      chunks.push(view);
    }
    return concatBytes(chunks);
  };
  const contentType = (owner) => owner.headers.get("content-type") ?? "";
  const bodyMixin = (Target) => {
    Object.defineProperties(Target.prototype, Object.getOwnPropertyDescriptors({
      get body() { return bodyStream(this); },
      get bodyUsed() { return isUsed(this); },
      async arrayBuffer() { return (await consume(this)).slice().buffer; },
      async bytes() { return (await consume(this)).slice(); },
      async text() {
        const bytes = await consume(this);
        const start = bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf ? 3 : 0;
        return decodeUtf8(bytes.subarray(start));
      },
      async json() { return JSON.parse(await this.text()); },
      async blob() { return new Blob([await consume(this)], { type: contentType(this) }); },
      async formData() {
        const type = contentType(this);
        const bytes = await consume(this);
        if (/^multipart\/form-data/i.test(type)) return decodeMultipart(bytes, type);
        if (/^application\/x-www-form-urlencoded/i.test(type)) {
          const form = new FormData();
          for (const [name, value] of parseForm(decodeUtf8(bytes))) form.append(name, value);
          return form;
        }
        throw new TypeError("Response.formData: Could not parse content as FormData.");
      },
    }));
  };
  const cloneBody = (from, to) => {
    if (isUsed(from)) throw new TypeError("Response.clone: Body has already been consumed.");
    const state = bodyState.get(from);
    const copy = { source: state.source, stream: null, used: false };
    if (state.source instanceof ReadableStream || state.stream) {
      const [left, right] = bodyStream(from).tee();
      state.stream = left;
      state.source = left;
      copy.source = right;
    }
    bodyState.set(to, copy);
  };

  const responseState = new WeakMap();
  const nullBodyStatus = [101, 103, 204, 205, 304];
  const redirectStatus = [301, 302, 303, 307, 308];
  class Response {
    constructor(body = null, init = undefined) {
      const options = init ?? {};
      const status = options.status === undefined ? 200 : Number(options.status);
      if (!Number.isInteger(status) || status < 200 || status > 599) {
        throw new RangeError("init[\"status\"] must be in the range of 200 to 599, inclusive.");
      }
      const statusText = options.statusText === undefined ? "" : webidlString(options.statusText);
      if (/[^\t\x20-\x7e\x80-\xff]/.test(statusText)) throw new TypeError("Invalid statusText");
      const headers = new Headers(options.headers);
      responseState.set(this, { status, statusText, headers, type: "default", url: "", redirected: false });
      bodyState.set(this, { source: null, stream: null, used: false });
      if (body !== null && body !== undefined) {
        if (nullBodyStatus.includes(status)) throw new TypeError(`Response constructor: Invalid response status code ${status}`);
        const { source, type } = extractBody(body);
        bodyState.get(this).source = source;
        if (type !== null && !headers.has("content-type")) headers.append("content-type", type);
      }
    }
    get type() { return responseState.get(this).type; }
    get url() { return responseState.get(this).url; }
    get redirected() { return responseState.get(this).redirected; }
    get status() { return responseState.get(this).status; }
    get ok() { const { status } = responseState.get(this); return status >= 200 && status <= 299; }
    get statusText() { return responseState.get(this).statusText; }
    get headers() { return responseState.get(this).headers; }
    clone() {
      const state = responseState.get(this);
      const out = Object.create(Response.prototype);
      const headers = new Headers(state.headers);
      headerState.get(headers).immutable = headerState.get(state.headers).immutable;
      responseState.set(out, { ...state, headers });
      cloneBody(this, out);
      return out;
    }
    static error() {
      const out = new Response(null);
      const state = responseState.get(out);
      state.type = "error";
      state.status = 0;
      headerState.get(state.headers).immutable = true;
      return out;
    }
    static json(data, init = undefined) {
      const text = JSON.stringify(data);
      if (text === undefined) throw new TypeError("Value is not JSON serializable");
      const out = new Response(text, init);
      if (!new Headers(init?.headers).has("content-type")) out.headers.set("content-type", "application/json");
      return out;
    }
    static redirect(url, status = 302) {
      const parsed = parseURL(url, undefined);
      if (parsed === FAILURE) throw new TypeError(`Failed to parse URL from ${url}`);
      if (!redirectStatus.includes(Number(status))) throw new RangeError(`Invalid status code ${status}`);
      const out = new Response(null);
      const state = responseState.get(out);
      state.status = Number(status);
      state.headers.set("location", serializeURL(parsed));
      headerState.get(state.headers).immutable = true;
      return out;
    }
  }
  bodyMixin(Response);
  tag(Response, "Response");

  const requestState = new WeakMap();
  const normalMethods = ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"];
  class Request {
    constructor(input, init = undefined) {
      requireArgs(arguments.length, 1, "Request constructor");
      const options = init ?? {};
      let state;
      bodyState.set(this, { source: null, stream: null, used: false });
      if (input instanceof Request) {
        const from = requestState.get(input);
        state = { ...from, headers: new Headers(from.headers) };
        if (bodyState.get(input).source !== null && options.body === undefined) {
          if (isUsed(input)) throw new TypeError("Cannot construct a Request with a Request object that has already been used.");
          cloneBody(input, this);
        }
      } else {
        const parsed = parseURL(input, undefined);
        if (parsed === FAILURE) throw new TypeError(`Failed to parse URL from ${webidlString(input)}`);
        if (includesCredentials(parsed)) {
          throw new TypeError(`Request cannot be constructed from a URL that includes credentials: ${webidlString(input)}`);
        }
        state = {
          method: "GET", url: serializeURL(parsed), headers: new Headers(), mode: "cors", credentials: "same-origin",
          cache: "default", redirect: "follow", referrer: "about:client", referrerPolicy: "", integrity: "",
          keepalive: false, signal: null,
        };
      }
      if (options.method !== undefined) {
        const method = webidlString(options.method);
        if (!token.test(method)) throw new TypeError(`'${method}' is not a valid HTTP method.`);
        const upper = method.toUpperCase();
        if (["CONNECT", "TRACE", "TRACK"].includes(upper)) throw new TypeError(`'${method}' HTTP method is unsupported.`);
        state.method = normalMethods.includes(upper) ? upper : method;
      }
      for (const name of ["mode", "credentials", "cache", "redirect", "referrerPolicy", "integrity"]) {
        if (options[name] !== undefined) state[name] = `${options[name]}`;
      }
      if (options.keepalive !== undefined) state.keepalive = Boolean(options.keepalive);
      if (options.headers !== undefined) state.headers = new Headers(options.headers);
      const controller = new AbortController();
      const parent = options.signal ?? state.signal;
      if (parent) {
        if (parent.aborted) controller.abort(parent.reason);
        else parent.addEventListener("abort", () => controller.abort(parent.reason), { once: true });
      }
      state.signal = controller.signal;
      const hasBody = options.body !== undefined && options.body !== null;
      if ((hasBody || bodyState.get(this).source !== null) && (state.method === "GET" || state.method === "HEAD")) {
        throw new TypeError("Request with GET/HEAD method cannot have body.");
      }
      if (hasBody) {
        const { source, type } = extractBody(options.body);
        bodyState.get(this).source = source;
        if (type !== null && !state.headers.has("content-type")) state.headers.append("content-type", type);
      }
      requestState.set(this, state);
    }
    get method() { return requestState.get(this).method; }
    get url() { return requestState.get(this).url; }
    get headers() { return requestState.get(this).headers; }
    get destination() { return ""; }
    get referrer() { return requestState.get(this).referrer; }
    get referrerPolicy() { return requestState.get(this).referrerPolicy; }
    get mode() { return requestState.get(this).mode; }
    get credentials() { return requestState.get(this).credentials; }
    get cache() { return requestState.get(this).cache; }
    get redirect() { return requestState.get(this).redirect; }
    get integrity() { return requestState.get(this).integrity; }
    get keepalive() { return requestState.get(this).keepalive; }
    get isReloadNavigation() { return false; }
    get isHistoryNavigation() { return false; }
    get signal() { return requestState.get(this).signal; }
    get duplex() { return "half"; }
    clone() {
      const out = Object.create(Request.prototype);
      const state = requestState.get(this);
      requestState.set(out, { ...state, headers: new Headers(state.headers) });
      cloneBody(this, out);
      return out;
    }
  }
  bodyMixin(Request);
  tag(Request, "Request");

  const dataURL = (url) => {
    const text = serializeURL(url, true).slice(5);
    const comma = text.indexOf(",");
    if (comma < 0) return null;
    let type = text.slice(0, comma).trim();
    let body = percentDecodeBytes(utf8(text.slice(comma + 1)));
    if (/;[\x20]*base64$/i.test(type)) {
      type = type.replace(/;[\x20]*base64$/i, "");
      try {
        body = latin1(atob(decodeUtf8(body).replace(/[\t\n\f\r ]/g, "")));
      } catch {
        return null;
      }
    }
    if (type.startsWith(";")) type = `text/plain${type}`;
    if (type === "") type = "text/plain;charset=US-ASCII";
    return { body, type };
  };
  function fetch(input, init = undefined) {
    let request;
    try {
      request = new Request(input, init);
    } catch (error) {
      return Promise.reject(error);
    }
    if (request.signal.aborted) return Promise.reject(request.signal.reason);
    const url = basicParse(request.url, null);
    const fail = (message) => {
      const error = new TypeError("fetch failed");
      error.cause = new Error(message);
      return Promise.reject(error);
    };
    let response;
    if (url.scheme === "data") {
      const parsed = dataURL(url);
      if (!parsed) return fail("invalid data: URL");
      response = new Response(parsed.body, { headers: { "content-type": parsed.type } });
    } else if (url.scheme === "blob") {
      const blob = blobURLs.get(serializeURL(url, true));
      if (!blob || request.method !== "GET") return fail("invalid method or blob: URL");
      response = new Response(blob, { headers: { "content-type": blob.type, "content-length": `${blob.size}` } });
    } else {
      return fail(`Code Mode cells have no network access, so ${url.scheme}: URLs cannot be fetched; use a tool instead`);
    }
    responseState.get(response).url = request.url;
    return Promise.resolve(response);
  }

  // -------------------------------------------------------- message channels

  const portState = new WeakMap();
  const flushPort = (port) => {
    const state = portState.get(port);
    if (!state.started || state.scheduled || state.queue.length === 0) return;
    state.scheduled = true;
    setTimeout(() => {
      state.scheduled = false;
      while (state.started && !state.closed && state.queue.length > 0) {
        port.dispatchEvent(new MessageEvent("message", { data: state.queue.shift() }));
      }
    }, 0);
  };
  class MessagePort extends EventTarget {
    constructor(key) {
      if (key !== illegal) throw new TypeError("Illegal constructor");
      super();
      portState.set(this, { other: null, queue: [], started: false, closed: false, scheduled: false });
    }
    postMessage(message, transfer = undefined) {
      const state = portState.get(this);
      const data = structuredClone(message, Array.isArray(transfer) ? { transfer } : transfer);
      const other = state.other;
      if (state.closed || !other) return;
      const target = portState.get(other);
      if (target.closed) return;
      target.queue.push(data);
      flushPort(other);
    }
    start() {
      portState.get(this).started = true;
      flushPort(this);
    }
    close() {
      const state = portState.get(this);
      state.closed = true;
      const other = state.other;
      state.other = null;
      if (other) {
        portState.get(other).other = null;
        other.dispatchEvent(new Event("close"));
      }
    }
  }
  handlerAttribute(MessagePort, "message", (port) => port.start());
  handlerAttribute(MessagePort, "messageerror");
  handlerAttribute(MessagePort, "close");
  tag(MessagePort, "MessagePort");
  class MessageChannel {
    #port1 = new MessagePort(illegal);
    #port2 = new MessagePort(illegal);
    constructor() {
      portState.get(this.#port1).other = this.#port2;
      portState.get(this.#port2).other = this.#port1;
    }
    get port1() { return this.#port1; }
    get port2() { return this.#port2; }
  }
  tag(MessageChannel, "MessageChannel");

  const channels = new Map();
  class BroadcastChannel extends EventTarget {
    #name;
    #closed = false;
    constructor(name) {
      requireArgs(arguments.length, 1, "BroadcastChannel constructor");
      super();
      this.#name = webidlString(name);
      const peers = channels.get(this.#name) ?? new Set();
      peers.add(this);
      channels.set(this.#name, peers);
    }
    get name() { return this.#name; }
    postMessage(message) {
      if (this.#closed) throw domError("BroadcastChannel is closed.", "InvalidStateError");
      const data = structuredClone(message);
      for (const peer of channels.get(this.#name) ?? []) {
        if (peer === this) continue;
        setTimeout(() => {
          if (!peer.#closed) peer.dispatchEvent(new MessageEvent("message", { data }));
        }, 0);
      }
    }
    close() {
      this.#closed = true;
      channels.get(this.#name)?.delete(this);
    }
  }
  handlerAttribute(BroadcastChannel, "message");
  handlerAttribute(BroadcastChannel, "messageerror");
  tag(BroadcastChannel, "BroadcastChannel");

  // A WebSocket checks its URL as a browser does and then fails to connect,
  // since cells have no network.
  class WebSocket extends EventTarget {
    #url;
    #state = 0;
    constructor(url, protocols = undefined) {
      requireArgs(arguments.length, 1, "WebSocket constructor");
      super();
      const parsed = parseURL(url, undefined);
      if (parsed === FAILURE) throw domError(`Failed to construct 'WebSocket': The URL '${url}' is invalid.`, "SyntaxError");
      if (parsed.scheme === "http") parsed.scheme = "ws";
      else if (parsed.scheme === "https") parsed.scheme = "wss";
      if (parsed.scheme !== "ws" && parsed.scheme !== "wss") {
        throw domError(`Failed to construct 'WebSocket': The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '${parsed.scheme}:' is not allowed.`, "SyntaxError");
      }
      if (parsed.fragment !== null) throw domError("Failed to construct 'WebSocket': The URL contains a fragment identifier.", "SyntaxError");
      if (protocols !== undefined && typeof protocols !== "string" && typeof protocols?.[Symbol.iterator] !== "function") {
        throw domError("Failed to construct 'WebSocket': The subprotocol is invalid.", "SyntaxError");
      }
      this.#url = serializeURL(parsed);
      setTimeout(() => {
        if (this.#state === 3) return;
        this.#state = 3;
        this.dispatchEvent(new Event("error"));
        this.dispatchEvent(new CloseEvent("close", { wasClean: false, code: 1006, reason: "" }));
      }, 0);
    }
    get url() { return this.#url; }
    get readyState() { return this.#state; }
    get bufferedAmount() { return 0; }
    get extensions() { return ""; }
    get protocol() { return ""; }
    get binaryType() { return "blob"; }
    set binaryType(value) {}
    send() {
      if (this.#state === 0) throw domError("Sent before connected.", "InvalidStateError");
    }
    close() {
      if (this.#state < 2) this.#state = 2;
    }
  }
  for (const [name, value] of [["CONNECTING", 0], ["OPEN", 1], ["CLOSING", 2], ["CLOSED", 3]]) {
    Object.defineProperty(WebSocket, name, { value, enumerable: true });
    Object.defineProperty(WebSocket.prototype, name, { value, enumerable: true });
  }
  for (const type of ["open", "error", "close", "message"]) handlerAttribute(WebSocket, type);
  tag(WebSocket, "WebSocket");

  // ------------------------------------------------------------- performance

  let timeOrigin;
  Object.defineProperty(globalThis.performance, "timeOrigin", {
    configurable: true,
    enumerable: true,
    get() {
      timeOrigin ??= clockNow() - monotonicNow();
      return timeOrigin;
    },
  });
  define(globalThis.performance, "toJSON", function toJSON() {
    return { timeOrigin: this.timeOrigin };
  });

  const globals = {
    Event, CustomEvent, MessageEvent, CloseEvent, EventTarget, AbortSignal, AbortController, structuredClone,
    URL, URLSearchParams, Crypto, CryptoKey, SubtleCrypto, Blob, File, FormData, ReadableStream,
    ReadableStreamDefaultReader, ReadableStreamDefaultController, WritableStream, WritableStreamDefaultWriter,
    WritableStreamDefaultController, TransformStream, TransformStreamDefaultController, TextEncoderStream,
    TextDecoderStream, CountQueuingStrategy, ByteLengthQueuingStrategy, Headers, Request, Response, fetch,
    MessagePort, MessageChannel, BroadcastChannel, WebSocket,
  };
  for (const name of Object.keys(globals)) define(globalThis, name, globals[name]);
  Object.defineProperty(globalThis, "crypto", { value: crypto, writable: true, configurable: true, enumerable: true });
}

