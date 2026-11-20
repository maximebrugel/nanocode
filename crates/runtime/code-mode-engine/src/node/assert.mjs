import { inspect, isDeepStrictEqual } from "node:util";

const deepEqual = isDeepStrictEqual[Symbol.for("nanocode.node.deepEqual")];

const HEADERS = {
  deepStrictEqual: "Expected values to be strictly deep-equal:",
  strictEqual: "Expected values to be strictly equal:",
  deepEqual: "Expected values to be loosely deep-equal:",
  equal: "Expected values to be loosely equal:",
  notDeepStrictEqual: "Expected \"actual\" not to be strictly deep-equal to:",
  notStrictEqual: "Expected \"actual\" to be strictly unequal to:",
  notDeepEqual: "Expected \"actual\" not to be loosely deep-equal to:",
  notEqual: "Expected \"actual\" to be loosely unequal to:",
};

function show(value) {
  return inspect(value, { compact: false, sorted: true, getters: true, depth: 1000, maxArrayLength: Infinity, breakLength: Infinity });
}

function defaultMessage(actual, expected, operator) {
  const header = HEADERS[operator];
  if (header === undefined) return show(actual) + " " + operator + " " + show(expected);
  const left = show(actual);
  const right = show(expected);
  if (operator.startsWith("not")) return header + "\n\n" + left + "\n";
  if (operator === "strictEqual" && (typeof actual !== "object" || actual === null) && (typeof expected !== "object" || expected === null) && !left.includes("\n") && !right.includes("\n")) {
    return header + "\n\n" + left + " !== " + right + "\n";
  }
  if (operator === "equal") return header + "\n\n" + left + " != " + right + "\n";
  return header + "\n+ actual - expected\n\n" + left.split("\n").map((line) => "+ " + line).join("\n") + "\n" + right.split("\n").map((line) => "- " + line).join("\n") + "\n";
}

class AssertionError extends Error {
  constructor(options) {
    if (options === null || typeof options !== "object") {
      const error = new TypeError("The \"options\" argument must be of type object. Received " + (options === null ? "null" : typeof options));
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    const { message, actual, expected, operator, stackStartFn } = options;
    super(message !== undefined ? String(message) : defaultMessage(actual, expected, operator));
    this.generatedMessage = message === undefined;
    this.code = "ERR_ASSERTION";
    this.actual = actual;
    this.expected = expected;
    this.operator = operator;
    Object.defineProperty(this, "name", { value: "AssertionError", enumerable: false, writable: true, configurable: true });
    if (typeof Error.captureStackTrace === "function") Error.captureStackTrace(this, stackStartFn);
  }

  toString() {
    return this.name + " [" + this.code + "]: " + this.message;
  }
}

function fail(actual, expected, message, operator, stackStartFn) {
  if (message instanceof Error) throw message;
  throw new AssertionError({ actual, expected, message, operator, stackStartFn });
}

function ok(value, message) {
  if (arguments.length === 0) fail(undefined, true, "No value argument passed to `assert.ok()`", "==", ok);
  if (!value) fail(value, true, message ?? "The expression evaluated to a falsy value:\n\n  assert.ok(" + inspect(value) + ")\n", "==", ok);
}

function assert(value, message) {
  if (arguments.length === 0) fail(undefined, true, "No value argument passed to `assert.ok()`", "==", assert);
  if (!value) fail(value, true, message ?? "The expression evaluated to a falsy value:\n\n  assert(" + inspect(value) + ")\n", "==", assert);
}

function equal(actual, expected, message) {
  if (!(actual == expected || (Number.isNaN(actual) && Number.isNaN(expected)))) fail(actual, expected, message, "==", equal);
}

function notEqual(actual, expected, message) {
  if (actual == expected || (Number.isNaN(actual) && Number.isNaN(expected))) fail(actual, expected, message, "!=", notEqual);
}

function strictEqual(actual, expected, message) {
  if (!Object.is(actual, expected)) fail(actual, expected, message, "strictEqual", strictEqual);
}

function notStrictEqual(actual, expected, message) {
  if (Object.is(actual, expected)) fail(actual, expected, message, "notStrictEqual", notStrictEqual);
}

function deepEqualAssert(actual, expected, message) {
  if (!deepEqual(actual, expected, false, new Map())) fail(actual, expected, message, "deepEqual", deepEqualAssert);
}

function notDeepEqual(actual, expected, message) {
  if (deepEqual(actual, expected, false, new Map())) fail(actual, expected, message, "notDeepEqual", notDeepEqual);
}

function deepStrictEqual(actual, expected, message) {
  if (!isDeepStrictEqual(actual, expected)) fail(actual, expected, message, "deepStrictEqual", deepStrictEqual);
}

function notDeepStrictEqual(actual, expected, message) {
  if (isDeepStrictEqual(actual, expected)) fail(actual, expected, message, "notDeepStrictEqual", notDeepStrictEqual);
}

function partialDeepStrictEqual(actual, expected, message) {
  const contains = (a, e) => {
    if (e === null || typeof e !== "object" || a === null || typeof a !== "object") return isDeepStrictEqual(a, e);
    if (Array.isArray(e)) return Array.isArray(a) && e.every((item) => a.some((other) => contains(other, item)));
    return Object.keys(e).every((key) => Object.prototype.hasOwnProperty.call(a, key) && contains(a[key], e[key]));
  };
  if (!contains(actual, expected)) fail(actual, expected, message, "partialDeepStrictEqual", partialDeepStrictEqual);
}

function matchesExpected(error, expected, message, fn) {
  if (expected === undefined) return;
  if (typeof expected === "function") {
    if (expected.prototype !== undefined && error instanceof expected) return;
    if (Error.isPrototypeOf(expected) || expected === Error) throw error;
    if (expected.call({}, error) !== true) {
      fail(error, expected, message ?? "The " + (fn === rejects ? "\"validate\" validation function" : "validation function") + " is expected to return \"true\". Received " + inspect(expected.call({}, error)), fn.name, fn);
    }
    return;
  }
  if (expected instanceof RegExp) {
    if (!expected.test(String(error))) fail(error, expected, message ?? "The input did not match the regular expression " + inspect(expected) + ". Input:\n\n" + inspect(String(error)) + "\n", fn.name, fn);
    return;
  }
  if (typeof expected === "object" && expected !== null) {
    for (const key of Object.keys(expected)) {
      const want = expected[key];
      const got = error?.[key];
      const same = want instanceof RegExp && typeof got === "string" ? want.test(got) : isDeepStrictEqual(got, want);
      if (!same) fail(error, expected, message ?? "Expected values to be strictly deep-equal:\n+ actual - expected\n\n+ " + key + ": " + inspect(got) + "\n- " + key + ": " + inspect(want) + "\n", fn.name, fn);
    }
  }
}

function throws(fn, expected, message) {
  if (typeof fn !== "function") {
    const error = new TypeError("The \"fn\" argument must be of type function. Received " + typeof fn);
    error.code = "ERR_INVALID_ARG_TYPE";
    throw error;
  }
  if (typeof expected === "string") {
    message = expected;
    expected = undefined;
  }
  let caught;
  let threw = false;
  try {
    fn();
  } catch (error) {
    caught = error;
    threw = true;
  }
  if (!threw) fail(undefined, expected, "Missing expected exception" + (message ? ": " + message : expected?.name ? " (" + expected.name + ")." : "."), "throws", throws);
  matchesExpected(caught, expected, message, throws);
}

function doesNotThrow(fn, expected, message) {
  if (typeof expected === "string") message = expected;
  try {
    fn();
  } catch (error) {
    fail(error, expected, "Got unwanted exception" + (message ? ": " + message : ".") + "\nActual message: \"" + error?.message + "\"", "doesNotThrow", doesNotThrow);
  }
}

async function rejects(promiseOrFn, expected, message) {
  if (typeof expected === "string") {
    message = expected;
    expected = undefined;
  }
  let caught;
  let threw = false;
  try {
    await (typeof promiseOrFn === "function" ? promiseOrFn() : promiseOrFn);
  } catch (error) {
    caught = error;
    threw = true;
  }
  if (!threw) fail(undefined, expected, "Missing expected rejection" + (message ? ": " + message : expected?.name ? " (" + expected.name + ")." : "."), "rejects", rejects);
  matchesExpected(caught, expected, message, rejects);
}

async function doesNotReject(promiseOrFn, expected, message) {
  if (typeof expected === "string") message = expected;
  try {
    await (typeof promiseOrFn === "function" ? promiseOrFn() : promiseOrFn);
  } catch (error) {
    fail(error, expected, "Got unwanted rejection" + (message ? ": " + message : ".") + "\nActual message: \"" + error?.message + "\"", "doesNotReject", doesNotReject);
  }
}

function match(string, regexp, message) {
  if (typeof string !== "string" || !regexp.test(string)) {
    fail(string, regexp, message ?? (typeof string !== "string" ? "The \"string\" argument must be of type string. Received type " + typeof string + " (" + inspect(string) + ")" : "The input did not match the regular expression " + inspect(regexp) + ". Input:\n\n" + inspect(string) + "\n"), "match", match);
  }
}

function doesNotMatch(string, regexp, message) {
  if (typeof string !== "string" || regexp.test(string)) {
    fail(string, regexp, message ?? "The input was expected to not match the regular expression " + inspect(regexp) + ". Input:\n\n" + inspect(string) + "\n", "doesNotMatch", doesNotMatch);
  }
}

function ifError(value) {
  if (value !== null && value !== undefined) {
    let text = "ifError got unwanted exception: ";
    text += typeof value?.message === "string" && value.message ? value.message : inspect(value);
    const error = new AssertionError({ actual: value, expected: null, operator: "ifError", message: text, stackStartFn: ifError });
    error.generatedMessage = false;
    throw error;
  }
}

function failAssert(message = "Failed") {
  if (message instanceof Error) throw message;
  throw new AssertionError({ message, operator: "fail", stackStartFn: failAssert });
}

Object.assign(assert, {
  ok,
  equal,
  notEqual,
  strictEqual,
  notStrictEqual,
  deepEqual: deepEqualAssert,
  notDeepEqual,
  deepStrictEqual,
  notDeepStrictEqual,
  partialDeepStrictEqual,
  throws,
  doesNotThrow,
  rejects,
  doesNotReject,
  match,
  doesNotMatch,
  ifError,
  fail: failAssert,
  AssertionError,
});

function strict(value, message) {
  if (arguments.length === 0) fail(undefined, true, "No value argument passed to `assert.ok()`", "==", strict);
  if (!value) fail(value, true, message ?? "The expression evaluated to a falsy value:\n\n  assert(" + inspect(value) + ")\n", "==", strict);
}
Object.assign(strict, assert, {
  equal: strictEqual,
  notEqual: notStrictEqual,
  deepEqual: deepStrictEqual,
  notDeepEqual: notDeepStrictEqual,
});
strict.strict = strict;
assert.strict = strict;

export default assert;
export {
  ok,
  equal,
  notEqual,
  strictEqual,
  notStrictEqual,
  deepEqualAssert as deepEqual,
  notDeepEqual,
  deepStrictEqual,
  notDeepStrictEqual,
  partialDeepStrictEqual,
  throws,
  doesNotThrow,
  rejects,
  doesNotReject,
  match,
  doesNotMatch,
  ifError,
  failAssert as fail,
  AssertionError,
  strict,
};
