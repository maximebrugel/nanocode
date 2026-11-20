import assert from "node:assert";

const strict = assert.strict;

export default strict;
export const { ok, equal, notEqual, strictEqual, notStrictEqual, deepEqual, notDeepEqual, deepStrictEqual, notDeepStrictEqual, partialDeepStrictEqual, throws, doesNotThrow, rejects, doesNotReject, match, doesNotMatch, ifError, fail, AssertionError } = strict;
export { strict };
