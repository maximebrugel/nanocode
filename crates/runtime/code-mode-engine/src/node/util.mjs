import process from "node:process";
import { Buffer } from "node:buffer";

const customInspectSymbol = Symbol.for("nodejs.util.inspect.custom");
const customPromisify = Symbol.for("nodejs.util.promisify.custom");
const ARRAY = 1;
const OBJECT = 0;
const IDENTIFIER = /^[a-zA-Z_][a-zA-Z_0-9]*$/;
const TypedArray = Object.getPrototypeOf(Uint8Array);
const typedArrayTag = Object.getOwnPropertyDescriptor(TypedArray.prototype, Symbol.toStringTag).get;
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const GeneratorFunction = Object.getPrototypeOf(function* () {}).constructor;
const AsyncGeneratorFunction = Object.getPrototypeOf(async function* () {}).constructor;
const objectToString = Object.prototype.toString;

const inspectDefaultOptions = Object.seal({
  showHidden: false,
  depth: 2,
  colors: false,
  customInspect: true,
  showProxy: false,
  maxArrayLength: 100,
  maxStringLength: 10000,
  breakLength: 80,
  compact: 3,
  sorted: false,
  getters: false,
  numericSeparator: false,
});

function codedError(Kind, code, message) {
  const error = new Kind(message);
  error.code = code;
  return error;
}

function tagOf(value) {
  return objectToString.call(value).slice(8, -1);
}

const types = {
  isAnyArrayBuffer: (value) => value instanceof ArrayBuffer || (typeof SharedArrayBuffer === "function" && value instanceof SharedArrayBuffer),
  isArrayBuffer: (value) => value instanceof ArrayBuffer,
  isArrayBufferView: (value) => ArrayBuffer.isView(value),
  isArgumentsObject: (value) => tagOf(value) === "Arguments",
  isAsyncFunction: (value) => typeof value === "function" && (value instanceof AsyncFunction || value instanceof AsyncGeneratorFunction),
  isBigInt64Array: (value) => value instanceof BigInt64Array,
  isBigUint64Array: (value) => value instanceof BigUint64Array,
  isBooleanObject: (value) => typeof value === "object" && value !== null && tagOf(value) === "Boolean",
  isBoxedPrimitive: (value) => typeof value === "object" && value !== null && ["Number", "String", "Boolean", "BigInt", "Symbol"].includes(tagOf(value)) && !(value instanceof Error),
  isDataView: (value) => value instanceof DataView,
  isDate: (value) => value instanceof Date,
  isFloat32Array: (value) => value instanceof Float32Array,
  isFloat64Array: (value) => value instanceof Float64Array,
  isGeneratorFunction: (value) => typeof value === "function" && (value instanceof GeneratorFunction || value instanceof AsyncGeneratorFunction),
  isGeneratorObject: (value) => tagOf(value) === "Generator",
  isInt8Array: (value) => value instanceof Int8Array,
  isInt16Array: (value) => value instanceof Int16Array,
  isInt32Array: (value) => value instanceof Int32Array,
  isMap: (value) => value instanceof Map,
  isMapIterator: (value) => tagOf(value) === "Map Iterator",
  isModuleNamespaceObject: (value) => tagOf(value) === "Module",
  isNativeError: (value) => value instanceof Error,
  isNumberObject: (value) => typeof value === "object" && value !== null && tagOf(value) === "Number",
  isPromise: (value) => value instanceof Promise,
  isProxy: () => false,
  isRegExp: (value) => value instanceof RegExp,
  isSet: (value) => value instanceof Set,
  isSetIterator: (value) => tagOf(value) === "Set Iterator",
  isSharedArrayBuffer: (value) => typeof SharedArrayBuffer === "function" && value instanceof SharedArrayBuffer,
  isStringObject: (value) => typeof value === "object" && value !== null && tagOf(value) === "String",
  isSymbolObject: (value) => typeof value === "object" && value !== null && tagOf(value) === "Symbol",
  isTypedArray: (value) => value instanceof TypedArray,
  isUint8Array: (value) => value instanceof Uint8Array,
  isUint8ClampedArray: (value) => value instanceof Uint8ClampedArray,
  isUint16Array: (value) => value instanceof Uint16Array,
  isUint32Array: (value) => value instanceof Uint32Array,
  isWeakMap: (value) => value instanceof WeakMap,
  isWeakSet: (value) => value instanceof WeakSet,
  isKeyObject: () => false,
  isCryptoKey: () => false,
  isExternal: () => false,
};

const META = [];
for (let i = 0; i < 32; i++) META[i] = "\\x" + i.toString(16).toUpperCase().padStart(2, "0");
META[8] = "\\b";
META[9] = "\\t";
META[10] = "\\n";
META[12] = "\\f";
META[13] = "\\r";
META[39] = "\\'";
META[92] = "\\\\";
for (let i = 127; i < 160; i++) META[i] = "\\x" + i.toString(16).toUpperCase();

function strEscape(text) {
  let quote = 39;
  if (text.includes("'")) {
    if (!text.includes("\"")) quote = 34;
    else if (!text.includes("\x60") && !text.includes("$" + "{")) quote = 96;
  }
  let result = "";
  let last = 0;
  for (let i = 0; i < text.length; i++) {
    const point = text.charCodeAt(i);
    if ((point === 39 && quote === 39) || point === 92 || point < 32 || (point > 126 && point < 160)) {
      result += text.slice(last, i) + META[point];
      last = i + 1;
    } else if (point >= 0xd800 && point <= 0xdfff) {
      if (point <= 0xdbff && i + 1 < text.length) {
        const next = text.charCodeAt(i + 1);
        if (next >= 0xdc00 && next <= 0xdfff) {
          i++;
          continue;
        }
      }
      result += text.slice(last, i) + "\\u" + point.toString(16);
      last = i + 1;
    }
  }
  result += text.slice(last);
  const mark = String.fromCharCode(quote);
  return mark + result + mark;
}

function formatNumber(number, separator) {
  if (Object.is(number, -0)) return "-0";
  if (!separator || !Number.isFinite(number)) return String(number);
  const [whole, fraction] = String(number).split(".");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, "_");
  return fraction === undefined ? grouped : grouped + "." + fraction.replace(/(\d{3})(?=\d)/g, "$1_");
}

function formatPrimitive(value, ctx) {
  if (typeof value === "string") {
    let trailer = "";
    if (value.length > ctx.maxStringLength) {
      const remaining = value.length - ctx.maxStringLength;
      value = value.slice(0, ctx.maxStringLength);
      trailer = "... " + remaining + " more character" + (remaining > 1 ? "s" : "");
    }
    if (ctx.compact !== true && value.length > 16 && value.length > ctx.breakLength - ctx.indentationLvl - 4) {
      return value.split(/(?<=\n)/).map((line) => strEscape(line)).join(" +\n" + " ".repeat(ctx.indentationLvl + 2)) + trailer;
    }
    return strEscape(value) + trailer;
  }
  if (typeof value === "number") return formatNumber(value, ctx.numericSeparator);
  if (typeof value === "bigint") return String(value) + "n";
  if (typeof value === "symbol") return value.toString();
  return String(value);
}

function constructorName(value) {
  let object = value;
  while (object !== null && object !== undefined) {
    const descriptor = Object.getOwnPropertyDescriptor(object, "constructor");
    if (descriptor !== undefined && typeof descriptor.value === "function" && descriptor.value.name !== "") {
      try {
        if (value instanceof descriptor.value) return descriptor.value.name;
      } catch {
        return descriptor.value.name;
      }
    }
    object = Object.getPrototypeOf(object);
  }
  return null;
}

function prefix(constructor, tag, fallback, size = "") {
  if (constructor === null) {
    if (tag !== "" && fallback !== tag) return "[" + fallback + size + ": null prototype] [" + tag + "] ";
    return "[" + fallback + size + ": null prototype] ";
  }
  if (tag !== "" && constructor !== tag) return constructor + size + " [" + tag + "] ";
  return constructor + size + " ";
}

function keysOf(value, showHidden) {
  const symbols = Object.getOwnPropertySymbols(value);
  if (showHidden) return [...Object.getOwnPropertyNames(value), ...symbols];
  return [...Object.keys(value), ...symbols.filter((symbol) => Object.prototype.propertyIsEnumerable.call(value, symbol))];
}

function nonIndexKeys(value, showHidden) {
  return keysOf(value, showHidden).filter((key) => typeof key === "symbol" || !/^(0|[1-9][0-9]*)$/.test(key) || Number(key) >= 4294967295);
}

function functionBase(value, constructor, tag) {
  const source = Function.prototype.toString.call(value);
  if (source.startsWith("class") && /^class(\s|\{)/.test(source)) {
    let base = "[class " + (value.name || "(anonymous)");
    const parent = Object.getPrototypeOf(value);
    if (parent !== Function.prototype && parent !== null && parent.name) base += " extends " + parent.name;
    else if (parent === null) base += " extends [null prototype]";
    return base + "]";
  }
  let type = "Function";
  if (value instanceof AsyncGeneratorFunction) type = "AsyncGeneratorFunction";
  else if (value instanceof GeneratorFunction) type = "GeneratorFunction";
  else if (value instanceof AsyncFunction) type = "AsyncFunction";
  let base = "[" + type;
  if (constructor === null) base += " (null prototype)";
  base += value.name === "" ? " (anonymous)" : ": " + value.name;
  base += "]";
  if (constructor !== type && constructor !== null) base += " " + constructor;
  if (tag !== "" && constructor !== tag) base += " [" + tag + "]";
  return base;
}

function formatError(error, keys, ctx) {
  const name = error.name != null ? String(error.name) : "Error";
  const message = error.message != null ? String(error.message) : "";
  let stack = typeof error.stack === "string" && error.stack ? error.stack : Error.prototype.toString.call(error);
  if (!stack.includes(name) && !stack.includes(message)) stack = name + ": " + message + "\n" + stack;
  for (const key of ["cause", "errors"]) {
    if (Object.prototype.hasOwnProperty.call(error, key) && !keys.includes(key) && !(key === "errors" && !Array.isArray(error.errors))) keys.push(key);
  }
  const at = keys.indexOf("stack");
  if (at !== -1) keys.splice(at, 1);
  const messageAt = keys.indexOf("message");
  if (messageAt !== -1 && stack.includes(message)) keys.splice(messageAt, 1);
  if (ctx.indentationLvl !== 0) stack = stack.replace(/\n/g, "\n" + " ".repeat(ctx.indentationLvl));
  return stack;
}

function formatValue(ctx, value, recurseTimes, typedArray) {
  if (typeof value !== "object" && typeof value !== "function") return formatPrimitive(value, ctx);
  if (value === null) return "null";
  if (ctx.customInspect) {
    const custom = value[customInspectSymbol];
    if (typeof custom === "function" && custom !== inspect && Object.getOwnPropertyDescriptor(value, "constructor")?.value?.prototype !== value) {
      const depth = ctx.depth === null ? null : ctx.depth - recurseTimes;
      const result = custom.call(value, depth, { ...ctx.userOptions, depth, stylize: (text) => text }, inspect);
      if (result !== value) {
        if (typeof result !== "string") return formatValue(ctx, result, recurseTimes);
        return result.replace(/\n/g, "\n" + " ".repeat(ctx.indentationLvl));
      }
    }
  }
  if (ctx.seen.includes(value)) {
    let index = 1;
    if (ctx.circular === undefined) {
      ctx.circular = new Map([[value, index]]);
    } else {
      index = ctx.circular.get(value);
      if (index === undefined) {
        index = ctx.circular.size + 1;
        ctx.circular.set(value, index);
      }
    }
    return "[Circular *" + index + "]";
  }
  return formatRaw(ctx, value, recurseTimes, typedArray);
}

function formatBuffer(value) {
  const max = 50;
  let text = Array.from(value.subarray(0, max), (byte) => byte.toString(16).padStart(2, "0")).join(" ");
  if (value.length > max) text += " ... " + (value.length - max) + " more byte" + (value.length - max > 1 ? "s" : "");
  return "<" + (constructorName(value) ?? "Buffer") + (text ? " " + text : "") + ">";
}

function formatRaw(ctx, value, recurseTimes, typedArray) {
  const constructor = constructorName(value);
  let tag = "";
  try {
    tag = value[Symbol.toStringTag];
  } catch {}
  if (typeof tag !== "string" || (tag !== "" && (ctx.showHidden ? Object.prototype.hasOwnProperty : Object.prototype.propertyIsEnumerable).call(value, Symbol.toStringTag))) tag = "";
  let base = "";
  let formatter = () => [];
  let braces;
  let keys;
  let extrasType = OBJECT;
  let extraKeys = [];
  let iterable = false;
  if (value instanceof Buffer && ctx.customInspect) {
    keys = nonIndexKeys(value, ctx.showHidden);
    if (keys.length === 0) return formatBuffer(value);
  }
  if (Symbol.iterator in value || constructor === null) {
    iterable = true;
    if (Array.isArray(value)) {
      const head = constructor !== "Array" || tag !== "" ? prefix(constructor, tag, "Array", "(" + value.length + ")") : "";
      keys = nonIndexKeys(value, ctx.showHidden).filter((key) => key !== "length");
      braces = [head + "[", "]"];
      if (value.length === 0 && keys.length === 0) return braces[0] + "]";
      extrasType = ARRAY;
      formatter = formatArray;
    } else if (value instanceof Set) {
      const head = prefix(constructor, tag, "Set", "(" + value.size + ")");
      keys = keysOf(value, ctx.showHidden);
      formatter = formatSet;
      if (value.size === 0 && keys.length === 0) return head + "{}";
      braces = [head + "{", "}"];
    } else if (value instanceof Map) {
      const head = prefix(constructor, tag, "Map", "(" + value.size + ")");
      keys = keysOf(value, ctx.showHidden);
      formatter = formatMap;
      if (value.size === 0 && keys.length === 0) return head + "{}";
      braces = [head + "{", "}"];
    } else if (value instanceof TypedArray) {
      keys = nonIndexKeys(value, ctx.showHidden);
      const fallback = constructor === null ? typedArrayTag.call(value) : "";
      const head = prefix(constructor, tag, fallback, "(" + value.length + ")");
      braces = [head + "[", "]"];
      if (value.length === 0 && keys.length === 0 && !ctx.showHidden) return braces[0] + "]";
      formatter = formatTypedArray;
      extrasType = ARRAY;
    } else if (tagOf(value) === "Map Iterator" || tagOf(value) === "Set Iterator") {
      keys = keysOf(value, ctx.showHidden);
      braces = ["[" + tagOf(value) + "] {", "}"];
      formatter = () => ["<unknown>"];
    } else {
      iterable = false;
    }
  }
  if (!iterable) {
    keys = keysOf(value, ctx.showHidden);
    braces = ["{", "}"];
    if (typeof value === "function") {
      base = functionBase(value, constructor, tag);
      if (keys.length === 0) return base;
    } else if (constructor === "Object") {
      if (tagOf(value) === "Arguments") braces[0] = "[Arguments] {";
      else if (tag !== "") braces[0] = prefix(constructor, tag, "Object") + "{";
      if (keys.length === 0) return braces[0] + "}";
    } else if (value instanceof RegExp) {
      base = RegExp.prototype.toString.call(value);
      const head = prefix(constructor, tag, "RegExp");
      if (head !== "RegExp ") base = head + base;
      if (keys.length === 0 || (ctx.depth !== null && recurseTimes > ctx.depth)) return base;
    } else if (value instanceof Date) {
      base = Number.isNaN(value.getTime()) ? Date.prototype.toString.call(value) : Date.prototype.toISOString.call(value);
      const head = prefix(constructor, tag, "Date");
      if (head !== "Date ") base = head + base;
      if (keys.length === 0) return base;
    } else if (value instanceof Error) {
      base = formatError(value, keys, ctx);
      if (keys.length === 0) return base;
    } else if (types.isAnyArrayBuffer(value)) {
      const head = prefix(constructor, tag, value instanceof ArrayBuffer ? "ArrayBuffer" : "SharedArrayBuffer");
      if (typedArray === undefined) formatter = formatArrayBuffer;
      else if (keys.length === 0) return head + "{ byteLength: " + formatNumber(value.byteLength, false) + " }";
      braces[0] = head + "{";
      extraKeys = ["byteLength"];
    } else if (value instanceof DataView) {
      braces[0] = prefix(constructor, tag, "DataView") + "{";
      extraKeys = ["byteLength", "byteOffset", "buffer"];
    } else if (value instanceof Promise) {
      braces[0] = prefix(constructor, tag, "Promise") + "{";
      formatter = () => ["<unknown>"];
    } else if (value instanceof WeakSet || value instanceof WeakMap) {
      braces[0] = prefix(constructor, tag, value instanceof WeakSet ? "WeakSet" : "WeakMap") + "{";
      formatter = () => ["<items unknown>"];
    } else if (tagOf(value) === "Module") {
      braces[0] = prefix(constructor, tag, "Module") + "{";
    } else if (types.isBoxedPrimitive(value)) {
      const kind = tagOf(value);
      const primitive = kind === "Symbol" ? Symbol.prototype.valueOf.call(value) : value.valueOf();
      base = "[" + kind;
      if (kind !== constructor) base += constructor === null ? " (null prototype)" : " (" + constructor + ")";
      base += ": " + formatPrimitive(primitive, { ...ctx, compact: true }) + "]";
      if (tag !== "" && tag !== constructor) base += " [" + tag + "]";
      if (kind === "String") keys = keys.filter((key) => typeof key === "symbol" || !/^(0|[1-9][0-9]*)$/.test(key) || Number(key) >= primitive.length);
      if (keys.length === 0) return base;
    } else if (typeof globalThis.URL === "function" && value instanceof globalThis.URL && !(ctx.depth !== null && recurseTimes > ctx.depth)) {
      base = value.href;
      if (keys.length === 0) return base;
    } else {
      const style = constructor === null ? prefix(null, tag, "Object") : prefix(constructor, tag, "Object");
      if (keys.length === 0) return style + "{}";
      braces[0] = style + "{";
    }
  }
  if (ctx.depth !== null && recurseTimes > ctx.depth) {
    let name = (constructor === null ? prefix(null, tag, "Object") : prefix(constructor, tag, "Object")).slice(0, -1);
    if (constructor !== null) name = "[" + name + "]";
    return name;
  }
  recurseTimes += 1;
  ctx.seen.push(value);
  ctx.currentDepth = recurseTimes;
  const output = formatter(ctx, value, recurseTimes);
  for (const key of extraKeys) {
    ctx.indentationLvl += 2;
    output.push("[" + key + "]: " + formatValue(ctx, value[key], recurseTimes));
    ctx.indentationLvl -= 2;
  }
  for (const key of keys) output.push(formatProperty(ctx, value, recurseTimes, key, extrasType));
  if (ctx.circular !== undefined) {
    const index = ctx.circular.get(value);
    if (index !== undefined) base = base === "" ? "<ref *" + index + ">" : "<ref *" + index + "> " + base;
  }
  ctx.seen.pop();
  if (ctx.sorted) {
    const comparator = ctx.sorted === true ? undefined : ctx.sorted;
    if (extrasType === OBJECT) output.sort(comparator);
    else if (keys.length > 1) {
      const sorted = output.slice(output.length - keys.length).sort(comparator);
      output.splice(output.length - keys.length, keys.length, ...sorted);
    }
  }
  const result = reduceToSingleString(ctx, output, base, braces, extrasType, recurseTimes, value);
  const budget = ctx.budget[ctx.indentationLvl] || 0;
  const length = budget + result.length;
  ctx.budget[ctx.indentationLvl] = length;
  return result;
}

function formatArray(ctx, value, recurseTimes) {
  const length = value.length;
  const shown = Math.min(Math.max(0, ctx.maxArrayLength), length);
  const remaining = length - shown;
  const output = [];
  let i = 0;
  for (; i < shown; i++) {
    if (!Object.prototype.hasOwnProperty.call(value, i)) {
      let end = i;
      while (end < length && !Object.prototype.hasOwnProperty.call(value, end)) end++;
      const holes = end - i;
      output.push("<" + holes + " empty item" + (holes > 1 ? "s" : "") + ">");
      i = end - 1;
      if (output.length >= shown) break;
      continue;
    }
    output.push(formatProperty(ctx, value, recurseTimes, i, ARRAY));
    if (output.length >= shown && i < length - 1) {
      i++;
      break;
    }
  }
  const left = length - (i >= shown ? Math.max(i, shown) : i);
  if (remaining > 0 && left > 0) output.push("... " + left + " more item" + (left > 1 ? "s" : ""));
  return output;
}

function formatTypedArray(ctx, value) {
  const length = value.length;
  const shown = Math.min(Math.max(0, ctx.maxArrayLength), length);
  const output = [];
  for (let i = 0; i < shown; i++) output.push(typeof value[i] === "bigint" ? value[i] + "n" : formatNumber(value[i], ctx.numericSeparator));
  if (length > shown) output.push("... " + (length - shown) + " more item" + (length - shown > 1 ? "s" : ""));
  if (ctx.showHidden) {
    ctx.indentationLvl += 2;
    for (const key of ["BYTES_PER_ELEMENT", "length", "byteLength", "byteOffset", "buffer"]) {
      output.push("[" + key + "]: " + formatValue(ctx, value[key], ctx.currentDepth, true));
    }
    ctx.indentationLvl -= 2;
  }
  return output;
}

function formatArrayBuffer(ctx, value) {
  const bytes = new Uint8Array(value);
  let text = Array.from(bytes.subarray(0, Math.min(ctx.maxArrayLength, bytes.length)), (byte) => byte.toString(16).padStart(2, "0")).join(" ");
  const remaining = bytes.length - ctx.maxArrayLength;
  if (remaining > 0) text += " ... " + remaining + " more byte" + (remaining > 1 ? "s" : "");
  return ["[Uint8Contents]: <" + text + ">"];
}

function formatSet(ctx, value, recurseTimes) {
  const length = value.size;
  const shown = Math.min(Math.max(0, ctx.maxArrayLength), length);
  const output = [];
  ctx.indentationLvl += 2;
  let i = 0;
  for (const item of value) {
    if (i >= shown) break;
    output.push(formatValue(ctx, item, recurseTimes));
    i++;
  }
  if (length > shown) output.push("... " + (length - shown) + " more item" + (length - shown > 1 ? "s" : ""));
  ctx.indentationLvl -= 2;
  return output;
}

function formatMap(ctx, value, recurseTimes) {
  const length = value.size;
  const shown = Math.min(Math.max(0, ctx.maxArrayLength), length);
  const output = [];
  ctx.indentationLvl += 2;
  let i = 0;
  for (const [key, item] of value) {
    if (i >= shown) break;
    output.push(formatValue(ctx, key, recurseTimes) + " => " + formatValue(ctx, item, recurseTimes));
    i++;
  }
  if (length > shown) output.push("... " + (length - shown) + " more item" + (length - shown > 1 ? "s" : ""));
  ctx.indentationLvl -= 2;
  return output;
}

function formatProperty(ctx, value, recurseTimes, key, type) {
  let text;
  const descriptor = Object.getOwnPropertyDescriptor(value, key) ?? { value: value[key], enumerable: true };
  if (descriptor.value !== undefined) {
    const diff = ctx.compact !== true || type !== OBJECT ? 2 : 3;
    ctx.indentationLvl += diff;
    text = formatValue(ctx, descriptor.value, recurseTimes);
    if (diff === 3 && ctx.breakLength < text.length) text = "\n" + " ".repeat(ctx.indentationLvl) + text;
    ctx.indentationLvl -= diff;
  } else if (descriptor.get !== undefined) {
    const label = descriptor.set !== undefined ? "Getter/Setter" : "Getter";
    if (ctx.getters && (ctx.getters === true || (ctx.getters === "get" && descriptor.set === undefined) || (ctx.getters === "set" && descriptor.set !== undefined))) {
      try {
        const got = descriptor.get.call(value);
        ctx.indentationLvl += 2;
        text = got === null ? "[" + label + ": null]" : typeof got === "object" ? "[" + label + "] " + formatValue(ctx, got, recurseTimes) : "[" + label + ": " + formatPrimitive(got, ctx) + "]";
        ctx.indentationLvl -= 2;
      } catch (error) {
        text = "[" + label + ": <Inspection threw (" + error.message + ")>]";
      }
    } else {
      text = "[" + label + "]";
    }
  } else if (descriptor.set !== undefined) {
    text = "[Setter]";
  } else {
    text = "undefined";
  }
  if (type === ARRAY) return text;
  let name;
  if (typeof key === "symbol") name = key.toString().replace(/[\x00-\x1f\x27\x5c\x7f-\x9f]/g, (c) => META[c.charCodeAt(0)]);
  else if (key === "__proto__") name = "['__proto__']";
  else if (descriptor.enumerable === false) name = "[" + (IDENTIFIER.test(key) ? key : strEscape(key)) + "]";
  else if (IDENTIFIER.test(key)) name = key;
  else name = strEscape(key);
  return name + ": " + text;
}

function groupArrayElements(ctx, output, value) {
  let totalLength = 0;
  let maxLength = 0;
  let outputLength = output.length;
  if (ctx.maxArrayLength < output.length) outputLength--;
  const separatorSpace = 2;
  const dataLen = new Array(outputLength);
  for (let i = 0; i < outputLength; i++) {
    const length = output[i].length;
    dataLen[i] = length;
    totalLength += length + separatorSpace;
    if (maxLength < length) maxLength = length;
  }
  const actualMax = maxLength + separatorSpace;
  if (actualMax * 3 + ctx.indentationLvl < ctx.breakLength && (totalLength / actualMax > 5 || maxLength <= 6)) {
    const averageBias = Math.sqrt(actualMax - totalLength / output.length);
    const biasedMax = Math.max(actualMax - 3 - averageBias, 1);
    const columns = Math.min(Math.round(Math.sqrt(2.5 * biasedMax * outputLength) / biasedMax), Math.floor((ctx.breakLength - ctx.indentationLvl) / actualMax), ctx.compact * 4, 15);
    if (columns <= 1) return output;
    const grouped = [];
    const maxLineLength = [];
    for (let i = 0; i < columns; i++) {
      let lineMaxLength = 0;
      for (let j = i; j < output.length; j += columns) if (dataLen[j] > lineMaxLength) lineMaxLength = dataLen[j];
      maxLineLength.push(lineMaxLength + separatorSpace);
    }
    let padStart = true;
    if (value !== undefined) {
      for (let i = 0; i < output.length; i++) {
        if (typeof value[i] !== "number" && typeof value[i] !== "bigint") {
          padStart = false;
          break;
        }
      }
    }
    for (let i = 0; i < outputLength; i += columns) {
      const max = Math.min(i + columns, outputLength);
      let line = "";
      let j = i;
      for (; j < max - 1; j++) {
        const padding = maxLineLength[j - i] + output[j].length - dataLen[j];
        line += padStart ? (output[j] + ", ").padStart(padding, " ") : (output[j] + ", ").padEnd(padding, " ");
      }
      if (padStart) {
        const padding = maxLineLength[j - i] + output[j].length - dataLen[j] - separatorSpace;
        line += output[j].padStart(padding, " ");
      } else {
        line += output[j];
      }
      grouped.push(line);
    }
    if (ctx.maxArrayLength < output.length) grouped.push(output[outputLength]);
    output = grouped;
  }
  return output;
}

function isBelowBreakLength(ctx, output, start, base) {
  let totalLength = output.length + start;
  if (totalLength + output.length > ctx.breakLength) return false;
  for (const item of output) {
    totalLength += item.length;
    if (totalLength > ctx.breakLength) return false;
  }
  return base === "" || !base.includes("\n");
}

function reduceToSingleString(ctx, output, base, braces, extrasType, recurseTimes, value) {
  if (ctx.compact !== true) {
    if (typeof ctx.compact === "number" && ctx.compact >= 1) {
      const entries = output.length;
      if (extrasType === ARRAY && entries > 6) output = groupArrayElements(ctx, output, value);
      if (ctx.currentDepth - recurseTimes < ctx.compact && entries === output.length) {
        const start = output.length + ctx.indentationLvl + braces[0].length + base.length + 10;
        if (isBelowBreakLength(ctx, output, start, base)) {
          const joined = output.join(", ");
          if (!joined.includes("\n")) return (base ? base + " " : "") + braces[0] + " " + joined + " " + braces[1];
        }
      }
    }
    const indentation = "\n" + " ".repeat(ctx.indentationLvl);
    return (base ? base + " " : "") + braces[0] + indentation + "  " + output.join("," + indentation + "  ") + indentation + braces[1];
  }
  if (isBelowBreakLength(ctx, output, 0, base)) {
    return braces[0] + (base ? " " + base : "") + " " + output.join(", ") + " " + braces[1];
  }
  const indentation = "\n" + " ".repeat(ctx.indentationLvl);
  const line = base === "" && braces[0].length === 1 ? " " : (base ? " " + base : "") + indentation + "  ";
  return braces[0] + line + output.join("," + indentation + "  ") + " " + braces[1];
}

function inspect(value, options, ...rest) {
  const ctx = {
    budget: {},
    indentationLvl: 0,
    seen: [],
    currentDepth: 0,
    circular: undefined,
    ...inspectDefaultOptions,
  };
  let userOptions = {};
  if (typeof options === "boolean") {
    ctx.showHidden = options;
    if (rest[0] !== undefined) ctx.depth = rest[0];
    if (rest[1] !== undefined) ctx.colors = rest[1];
  } else if (options !== null && typeof options === "object") {
    userOptions = options;
    for (const key of Object.keys(options)) {
      if (Object.prototype.hasOwnProperty.call(inspectDefaultOptions, key) || key === "stylize") ctx[key] = options[key];
    }
  }
  if (ctx.maxArrayLength === null) ctx.maxArrayLength = Infinity;
  if (ctx.maxStringLength === null) ctx.maxStringLength = Infinity;
  if (ctx.depth === Infinity) ctx.depth = null;
  ctx.userOptions = userOptions;
  return formatValue(ctx, value, 0);
}
inspect.custom = customInspectSymbol;
Object.defineProperty(inspect, "defaultOptions", {
  get: () => inspectDefaultOptions,
  set: (options) => Object.assign(inspectDefaultOptions, options),
});
inspect.colors = Object.assign(Object.create(null), { bold: [1, 22], italic: [3, 23], underline: [4, 24], inverse: [7, 27], white: [37, 39], grey: [90, 39], black: [30, 39], blue: [34, 39], cyan: [36, 39], green: [32, 39], magenta: [35, 39], red: [31, 39], yellow: [33, 39] });
inspect.styles = Object.assign(Object.create(null), { special: "cyan", number: "yellow", bigint: "yellow", boolean: "yellow", undefined: "grey", null: "bold", string: "green", symbol: "green", date: "magenta", regexp: "red", module: "underline" });

function formatWithOptions(options, ...args) {
  if (options === null || typeof options !== "object") {
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"inspectOptions\" argument must be of type object. Received " + (options === null ? "null" : typeof options));
  }
  return formatList(options, args);
}

function format(...args) {
  return formatList(undefined, args);
}

function formatList(options, args) {
  const first = args[0];
  let a = 0;
  let text = "";
  let join = "";
  if (typeof first === "string") {
    if (args.length === 1) return first;
    let last = 0;
    for (let i = 0; i < first.length - 1; i++) {
      if (first.charCodeAt(i) !== 37) continue;
      const next = first.charCodeAt(++i);
      if (a + 1 !== args.length) {
        let piece;
        switch (next) {
          case 115: {
            const value = args[++a];
            if (typeof value === "number") piece = formatNumber(value, false);
            else if (typeof value === "bigint") piece = value + "n";
            else if (typeof value !== "object" || value === null || !hasBuiltInToString(value)) piece = String(value);
            else piece = inspect(value, { ...options, depth: 0, colors: false, compact: 3 });
            break;
          }
          case 106:
            piece = tryStringify(args[++a]);
            break;
          case 100: {
            const value = args[++a];
            if (typeof value === "bigint") piece = value + "n";
            else if (typeof value === "symbol") piece = "NaN";
            else piece = formatNumber(Number(value), false);
            break;
          }
          case 79:
            piece = inspect(args[++a], options);
            break;
          case 111:
            piece = inspect(args[++a], { ...options, showHidden: true, showProxy: true, depth: 4 });
            break;
          case 105: {
            const value = args[++a];
            if (typeof value === "bigint") piece = value + "n";
            else if (typeof value === "symbol") piece = "NaN";
            else piece = formatNumber(Number.parseInt(value), false);
            break;
          }
          case 102: {
            const value = args[++a];
            piece = typeof value === "symbol" ? "NaN" : formatNumber(Number.parseFloat(value), false);
            break;
          }
          case 99:
            a += 1;
            piece = "";
            break;
          case 37:
            text += first.slice(last, i);
            last = i + 1;
            continue;
          default:
            continue;
        }
        if (last !== i - 1) text += first.slice(last, i - 1);
        text += piece;
        last = i + 1;
      } else if (next === 37) {
        text += first.slice(last, i);
        last = i + 1;
      }
    }
    if (last !== 0) {
      a++;
      join = " ";
      if (last < first.length) text += first.slice(last);
    }
  }
  while (a < args.length) {
    const value = args[a];
    text += join;
    text += typeof value !== "string" ? inspect(value, { ...options, compact: 3, colors: false, depth: 2 }) : value;
    join = " ";
    a++;
  }
  return text;
}

function hasBuiltInToString(value) {
  let object = value;
  while (object !== null && object !== undefined) {
    if (Object.prototype.hasOwnProperty.call(object, "toString")) {
      return object === Object.prototype || object === Error.prototype || object === Date.prototype || object === Array.prototype || object === RegExp.prototype || object === Function.prototype || object === Map.prototype || object === Set.prototype || object === TypedArray.prototype;
    }
    object = Object.getPrototypeOf(object);
  }
  return true;
}

function tryStringify(value) {
  try {
    return JSON.stringify(value);
  } catch (error) {
    if (error instanceof TypeError && /circular/i.test(error.message)) return "[Circular]";
    throw error;
  }
}

function isDeepStrictEqual(a, b) {
  return deepEqual(a, b, true, new Map());
}

function deepEqual(a, b, strict, memo) {
  if (a === b) return a !== 0 || !strict || Object.is(a, b);
  if (strict) {
    if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
      return typeof a === "number" && typeof b === "number" && Number.isNaN(a) && Number.isNaN(b);
    }
    if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
  } else {
    if (a === null || typeof a !== "object") {
      if (b === null || typeof b !== "object") return a == b || (Number.isNaN(a) && Number.isNaN(b));
      return false;
    }
    if (b === null || typeof b !== "object") return false;
  }
  const tag = objectToString.call(a);
  if (tag !== objectToString.call(b)) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (a instanceof Date) {
    if (!(b instanceof Date) || a.getTime() !== b.getTime()) return false;
  } else if (a instanceof RegExp) {
    if (!(b instanceof RegExp) || a.source !== b.source || a.flags !== b.flags || a.lastIndex !== b.lastIndex) return false;
  } else if (a instanceof Error) {
    if (!(b instanceof Error) || a.message !== b.message || a.name !== b.name) return false;
    if (Object.prototype.hasOwnProperty.call(a, "cause") !== Object.prototype.hasOwnProperty.call(b, "cause")) return false;
    if (Object.prototype.hasOwnProperty.call(a, "cause") && !deepEqual(a.cause, b.cause, strict, memo)) return false;
  } else if (ArrayBuffer.isView(a)) {
    if (!ArrayBuffer.isView(b) || a.byteLength !== b.byteLength) return false;
    if (strict && a.constructor !== b.constructor) return false;
    if (a instanceof Float32Array || a instanceof Float64Array) {
      for (let i = 0; i < a.length; i++) if (strict ? !Object.is(a[i], b[i]) && !(a[i] === b[i]) : a[i] != b[i]) return false;
    } else {
      const x = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
      const y = new Uint8Array(b.buffer, b.byteOffset, b.byteLength);
      for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
    }
  } else if (types.isAnyArrayBuffer(a)) {
    if (!types.isAnyArrayBuffer(b) || a.byteLength !== b.byteLength) return false;
    const x = new Uint8Array(a);
    const y = new Uint8Array(b);
    for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
  } else if (types.isBoxedPrimitive(a)) {
    if (!types.isBoxedPrimitive(b) || !Object.is(a.valueOf(), b.valueOf())) return false;
  } else if (a instanceof WeakMap || a instanceof WeakSet || a instanceof Promise) {
    return false;
  }
  const seen = memo.get(a);
  if (seen !== undefined && seen.has(b)) return true;
  if (seen === undefined) memo.set(a, new Set([b]));
  else seen.add(b);
  const ownKeys = (value) => {
    const keys = Object.keys(value);
    if (strict) for (const symbol of Object.getOwnPropertySymbols(value)) if (Object.prototype.propertyIsEnumerable.call(value, symbol)) keys.push(symbol);
    return ArrayBuffer.isView(value) ? keys.filter((key) => typeof key === "symbol" || !/^\d+$/.test(key)) : keys;
  };
  if (Array.isArray(a)) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      const hasA = Object.prototype.hasOwnProperty.call(a, i);
      const hasB = Object.prototype.hasOwnProperty.call(b, i);
      if (strict && hasA !== hasB) return false;
      if (!deepEqual(a[i], b[i], strict, memo)) return false;
    }
  }
  const keysA = ownKeys(a).filter((key) => !Array.isArray(a) || typeof key === "symbol" || !/^\d+$/.test(key));
  const keysB = ownKeys(b).filter((key) => !Array.isArray(b) || typeof key === "symbol" || !/^\d+$/.test(key));
  if (keysA.length !== keysB.length) return false;
  for (const key of keysA) {
    if (!Object.prototype.propertyIsEnumerable.call(b, key)) return false;
    if (!deepEqual(a[key], b[key], strict, memo)) return false;
  }
  if (a instanceof Set) {
    if (!(b instanceof Set) || a.size !== b.size) return false;
    const pending = [];
    for (const item of a) {
      if (b.has(item)) continue;
      if (typeof item !== "object" || item === null) {
        if (strict) return false;
        if (![...b].some((other) => deepEqual(item, other, strict, memo))) return false;
      } else {
        pending.push(item);
      }
    }
    if (pending.length) {
      const candidates = [...b].filter((item) => !a.has(item));
      for (const item of pending) {
        const at = candidates.findIndex((other) => deepEqual(item, other, strict, memo));
        if (at === -1) return false;
        candidates.splice(at, 1);
      }
    }
  } else if (a instanceof Map) {
    if (!(b instanceof Map) || a.size !== b.size) return false;
    const pending = [];
    for (const [key, item] of a) {
      if (b.has(key)) {
        if (!deepEqual(item, b.get(key), strict, memo)) return false;
      } else if (typeof key === "object" && key !== null) {
        pending.push([key, item]);
      } else {
        return false;
      }
    }
    if (pending.length) {
      const candidates = [...b].filter(([key]) => !a.has(key));
      for (const [key, item] of pending) {
        const at = candidates.findIndex(([otherKey, other]) => deepEqual(key, otherKey, strict, memo) && deepEqual(item, other, strict, memo));
        if (at === -1) return false;
        candidates.splice(at, 1);
      }
    }
  }
  return true;
}

function promisify(original) {
  if (typeof original !== "function") {
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"original\" argument must be of type function. Received " + (original === null ? "null" : typeof original));
  }
  if (typeof original[customPromisify] === "function") {
    const custom = original[customPromisify];
    Object.defineProperty(custom, customPromisify, { value: custom, enumerable: false, writable: false, configurable: true });
    return custom;
  }
  function fn(...args) {
    return new Promise((resolve, reject) => {
      original.call(this, ...args, (error, ...values) => {
        if (error) reject(error);
        else resolve(values[0]);
      });
    });
  }
  Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
  Object.defineProperty(fn, customPromisify, { value: fn, enumerable: false, writable: false, configurable: true });
  return Object.defineProperties(fn, Object.getOwnPropertyDescriptors(original));
}
promisify.custom = customPromisify;
Object.defineProperty(isDeepStrictEqual, Symbol.for("nanocode.node.deepEqual"), { value: deepEqual });

function callbackify(original) {
  if (typeof original !== "function") {
    throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"original\" argument must be of type function. Received " + (original === null ? "null" : typeof original));
  }
  return function (...args) {
    const callback = args.pop();
    if (typeof callback !== "function") {
      throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The last argument must be of type function. Received " + (callback === null ? "null" : typeof callback));
    }
    Reflect.apply(original, this, args).then(
      (value) => process.nextTick(callback, null, value),
      (reason) => {
        if (!reason) {
          const error = new Error("Promise was rejected with a falsy value");
          error.code = "ERR_FALSY_VALUE_REJECTION";
          error.reason = reason;
          reason = error;
        }
        process.nextTick(callback, reason);
      },
    );
  };
}

function inherits(ctor, superCtor) {
  if (typeof ctor !== "function") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"ctor\" argument must be of type function");
  if (typeof superCtor !== "function") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"superCtor\" argument must be of type function");
  if (superCtor.prototype === undefined) throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"superCtor.prototype\" property must be of type object");
  Object.defineProperty(ctor, "super_", { value: superCtor, writable: true, configurable: true });
  Object.setPrototypeOf(ctor.prototype, superCtor.prototype);
}

const warned = new Set();

function deprecate(fn, message, code) {
  return function (...args) {
    const key = code ?? message;
    if (!warned.has(key)) {
      warned.add(key);
      process.emitWarning(message, "DeprecationWarning", code);
    }
    return new.target ? Reflect.construct(fn, args, new.target) : fn.apply(this, args);
  };
}

function debuglog() {
  const log = () => {};
  log.enabled = false;
  return log;
}

function stripVTControlCharacters(text) {
  return String(text).replace(/[\u001B\u009B][[\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\d/#&.:=?%@~_]+)*|[a-zA-Z\d]+(?:;[-a-zA-Z\d/#&.:=?%@~_]*)*)?\u0007)|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-TZcf-ntqry=><~]))/g, "");
}

function styleText(format, text) {
  if (typeof text !== "string") throw codedError(TypeError, "ERR_INVALID_ARG_TYPE", "The \"text\" argument must be of type string. Received " + typeof text);
  for (const key of Array.isArray(format) ? format : [format]) {
    if (key !== "none" && !(key in inspect.colors) && !/^(bg|)[A-Za-z]+$/.test(key)) {
      throw codedError(TypeError, "ERR_INVALID_ARG_VALUE", "The argument 'format' must be one of: " + Object.keys(inspect.colors).join(", ") + ". Received " + inspect(key));
    }
  }
  return text;
}

function toUSVString(text) {
  return String(text).replace(/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g, "\ufffd");
}

const ERRNO_NAMES = { "-1": "EPERM", "-2": "ENOENT", "-9": "EBADF", "-13": "EACCES", "-17": "EEXIST", "-20": "ENOTDIR", "-21": "EISDIR", "-22": "EINVAL", "-38": "ENOSYS", "-39": "ENOTEMPTY" };

function getSystemErrorName(errno) {
  return ERRNO_NAMES[errno] ?? "Unknown system error " + errno;
}

function getSystemErrorMap() {
  return new Map(Object.entries(ERRNO_NAMES).map(([code, name]) => [Number(code), [name, ""]]));
}

function aborted(signal) {
  return new Promise((resolve) => {
    if (signal.aborted) resolve();
    else signal.addEventListener("abort", () => resolve(), { once: true });
  });
}

function parseArgs(config = {}) {
  const args = config.args ?? process.argv.slice(2);
  const options = config.options ?? {};
  const strict = config.strict ?? true;
  const allowPositionals = config.allowPositionals ?? !strict;
  const values = { __proto__: null };
  const positionals = [];
  const tokens = [];
  const byShort = {};
  for (const [name, option] of Object.entries(options)) if (option.short) byShort[option.short] = name;
  const fail = (code, message) => {
    throw codedError(TypeError, code, message);
  };
  const store = (name, value) => {
    const option = options[name];
    if (option?.multiple) (values[name] ??= []).push(value);
    else values[name] = value;
  };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--") {
      positionals.push(...args.slice(i + 1));
      break;
    }
    let name;
    let inline;
    if (arg.startsWith("--")) {
      const eq = arg.indexOf("=");
      name = eq === -1 ? arg.slice(2) : arg.slice(2, eq);
      inline = eq === -1 ? undefined : arg.slice(eq + 1);
    } else if (arg.startsWith("-") && arg.length > 1) {
      const letters = arg.slice(1);
      if (letters.length > 1 && options[byShort[letters[0]]]?.type !== "string") {
        args.splice(i, 1, ...[...letters].map((letter) => "-" + letter));
        i--;
        continue;
      }
      name = byShort[letters[0]] ?? letters[0];
      if (letters.length > 1) inline = letters.slice(1);
    } else {
      if (strict && !allowPositionals) fail("ERR_PARSE_ARGS_UNEXPECTED_POSITIONAL", "Unexpected argument '" + arg + "'. This command does not take positional arguments");
      positionals.push(arg);
      continue;
    }
    const option = options[name];
    if (option === undefined) {
      if (strict) fail("ERR_PARSE_ARGS_UNKNOWN_OPTION", "Unknown option '" + arg + "'");
      store(name, inline ?? true);
      continue;
    }
    if (option.type === "string") {
      let value = inline;
      if (value === undefined) {
        value = args[i + 1];
        if (value === undefined || (strict && value.startsWith("-"))) fail("ERR_PARSE_ARGS_INVALID_OPTION_VALUE", "Option '" + (option.short ? "-" + option.short + ", " : "") + "--" + name + " <value>' argument missing");
        i++;
      }
      store(name, value);
    } else {
      if (inline !== undefined && strict) fail("ERR_PARSE_ARGS_INVALID_OPTION_VALUE", "Option '--" + name + "' does not take an argument");
      store(name, true);
    }
  }
  for (const [name, option] of Object.entries(options)) if (values[name] === undefined && option.default !== undefined) values[name] = option.default;
  const result = { values, positionals };
  if (config.tokens) result.tokens = tokens;
  return result;
}

const isArray = Array.isArray;
const TextEncoder = globalThis.TextEncoder;
const TextDecoder = globalThis.TextDecoder;

const util = {
  format,
  formatWithOptions,
  inspect,
  isDeepStrictEqual,
  promisify,
  callbackify,
  inherits,
  deprecate,
  debuglog,
  debug: debuglog,
  stripVTControlCharacters,
  styleText,
  toUSVString,
  getSystemErrorName,
  getSystemErrorMap,
  aborted,
  parseArgs,
  isArray,
  types,
  TextEncoder,
  TextDecoder,
};

export default util;
export {
  format,
  formatWithOptions,
  inspect,
  isDeepStrictEqual,
  promisify,
  callbackify,
  inherits,
  deprecate,
  debuglog,
  debuglog as debug,
  stripVTControlCharacters,
  styleText,
  toUSVString,
  getSystemErrorName,
  getSystemErrorMap,
  aborted,
  parseArgs,
  isArray,
  types,
  TextEncoder,
  TextDecoder,
};
