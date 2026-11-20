// ECMA-402 (Intl): the constructors read their options here, in the order the spec
// reads them, into a raw option list; builtins/intl (Rust) resolves it against the locale
// data into the resolved options and formats with them. Both lists are strings of
// name \u0002 value pairs joined by \u0001; a resolved value is a string, or a number
// (#), a boolean (!t, !f) or a list (@a,b). en-US matches node; a few other locales have
// their number, list and relative time data. Messages are node's. Builds without
// djs-core's \`embed\` have no locale data: IntlEnabled is false there, and the
// toLocaleString methods keep their locale-free results.

/** CanonicalizeLocaleList's input: the language tags, joined by LF. */
function IntlLocaleTags(locales: Value): Str {
  if (locales === undefined) {
    return "";
  }
  if (IsString(locales)) {
    if (AsStr(locales) === "") {
      throw NewRangeError("Incorrect locale information provided");
    }
    return AsStr(locales);
  }
  if (locales === null) {
    throw NewTypeError("Cannot convert undefined or null to object");
  }
  const one: Value = IntlSpec(locales, 16);
  if (one !== undefined) {
    return AsStr(one);
  }
  const o: Obj = ToObject(locales);
  const len: f64 = LengthOfArrayLike(o);
  let out: Str = "";
  let i: f64 = 0;
  while (i < len) {
    if (HasProperty(o, i)) {
      const v: Value = Get(o, i);
      if (!IsString(v) && !IsObject(v)) {
        throw NewTypeError("Language ID should be string or object.");
      }
      const own: Value = IntlSpec(v, 16);
      const t: Str = own !== undefined ? AsStr(own) : ToString(v);
      if (t === "") {
        throw NewRangeError("Incorrect locale information provided");
      }
      out = out === "" ? t : out + "\n" + t;
    }
    i += 1;
  }
  return out;
}

/** The items of a list joined by sep, as an array. */
function IntlSplit(s: Str, sep: Str, from: f64): Obj {
  const out: Obj = ArrayCreate(0);
  let start: f64 = from;
  const len: f64 = StrLength(s);
  while (start < len) {
    let end: f64 = StrIndexOf(s, sep, start);
    if (end < 0) {
      end = len;
    }
    ArrayPushDense(out, StrSubstring(s, start, end));
    start = end + 1;
  }
  return out;
}

/**
 * The raw option list of options: each of names (name:kind, joined by commas; kind s for
 * a string, b a boolean, n a number, g useGrouping's boolean or string) that is not
 * undefined, converted.
 */
/** options, refusing null with V8's TypeError for the constructor. */
function IntlNoNull(locales: Value, options: Value, message: Str): Value {
  if (options === null) {
    // The locales are read (and refused) first.
    IntlLocales(IntlLocaleTags(locales), 0);
    throw NewTypeError(message);
  }
  return options;
}

function IntlReadOptions(options: Value, names: Str): Str {
  if (options === undefined) {
    return "";
  }
  const o: Obj = ToObject(options);
  let out: Str = "";
  let start: f64 = 0;
  const len: f64 = StrLength(names);
  while (start < len) {
    let end: f64 = StrIndexOf(names, ",", start);
    if (end < 0) {
      end = len;
    }
    const kind: Str = StrSubstring(names, end - 1, end);
    const name: Str = StrSubstring(names, start, end - 2);
    start = end + 1;
    const v: Value = Get(o, name);
    if (v === undefined) {
      continue;
    }
    let s: Str = "";
    if (kind === "b") {
      s = ToBoolean(v) ? "true" : "false";
    } else if (kind === "n") {
      const n: f64 = ToNumber(v);
      if (n !== n) {
        throw NewRangeError(name + " value is out of range.");
      }
      s = NumberToString(n, 10);
    } else if (kind === "g") {
      if (v === true) {
        s = "true";
      } else if (!ToBoolean(v)) {
        s = "false";
      } else {
        s = ToString(v);
        if (s === "true" || s === "false") {
          continue;
        }
      }
    } else {
      s = ToString(v);
    }
    out = IntlWith(out, name, s);
  }
  return out;
}

/** raw with one more pair. */
function IntlWith(raw: Str, name: Str, v: Str): Str {
  return raw === "" ? name + "\u0002" + v : raw + "\u0001" + name + "\u0002" + v;
}

/** The value of a resolved option. */
function IntlValue(v: Str): Value {
  if (v === "!t") {
    return true;
  }
  if (v === "!f") {
    return false;
  }
  const c: f64 = StrLength(v) > 0 ? StrCharCodeAt(v, 0) : 0;
  if (c === 35) {
    return ToNumber(StrSubstring(v, 1, StrLength(v)));
  }
  if (c === 64) {
    return IntlSplit(v, ",", 1);
  }
  return v;
}

/** The resolved options object of a resolved option list (internal names start with "at"). */
function IntlResolvedObject(spec: Str): Obj {
  const o: Obj = ObjectCreate(RealmIntrinsic("ObjectPrototype"));
  let start: f64 = 0;
  const len: f64 = StrLength(spec);
  while (start < len) {
    let end: f64 = StrIndexOf(spec, "\u0001", start);
    if (end < 0) {
      end = len;
    }
    const mid: f64 = StrIndexOf(spec, "\u0002", start);
    const name: Str = StrSubstring(spec, start, mid);
    if (StrCharCodeAt(name, 0) !== 64) {
      CreateDataPropertyOrThrow(o, name, IntlValue(StrSubstring(spec, mid + 1, end)));
    }
    start = end + 1;
  }
  return o;
}

/** formatToParts's array of { type, value } (and unit, for a relative time's number). */
function IntlParts(encoded: Value, unit: Value): Obj {
  const s: Str = AsStr(encoded);
  const out: Obj = ArrayCreate(0);
  let start: f64 = 0;
  const len: f64 = StrLength(s);
  while (start < len) {
    let end: f64 = StrIndexOf(s, "\u0001", start);
    if (end < 0) {
      end = len;
    }
    const mid: f64 = StrIndexOf(s, "\u0002", start);
    const part: Obj = ObjectCreate(RealmIntrinsic("ObjectPrototype"));
    let ty: Str = StrSubstring(s, start, mid);
    // A range's part: type \u0003 source.
    const at: f64 = StrIndexOf(ty, "\u0003", 0);
    let source: Value = undefined;
    if (at >= 0 && at < mid - start) {
      source = StrSubstring(ty, at + 1, StrLength(ty));
      ty = StrSubstring(ty, 0, at);
    }
    CreateDataPropertyOrThrow(part, "type", ty);
    CreateDataPropertyOrThrow(part, "value", StrSubstring(s, mid + 1, end));
    if (unit !== undefined && ty !== "literal") {
      CreateDataPropertyOrThrow(part, "unit", unit);
    }
    if (source !== undefined) {
      CreateDataPropertyOrThrow(part, "source", source);
    }
    ArrayPushDense(out, part);
    start = end + 1;
  }
  return out;
}

/** The resolved options of an Intl object of kind, else TypeError naming method. */
function IntlThis(v: Value, kind: i32, method: Str): Str {
  const spec: Value = IntlSpec(v, kind);
  if (spec === undefined) {
    let shown: Str = "#<Object>";
    if (!IsObject(v)) {
      shown = IsSymbol(v) ? "Symbol()" : ToString(v);
    }
    throw NewTypeError("Method " + method + " called on incompatible receiver " + shown);
  }
  return AsStr(spec);
}

/** Intl[name]. */
function IntlConstructor(name: Str): Obj {
  return AsObj(Get(AsObj(Get(GlobalObject(), "Intl")), name));
}

/**
 * The requested locales' tags (IntlLocaleTags), after checking each is a valid tag: a
 * constructor canonicalizes them, with its RangeError, before it reads its options.
 */
function IntlRequested(locales: Value): Str {
  const tags: Str = IntlLocaleTags(locales);
  IntlLocales(tags, 0);
  return tags;
}

/** A new Intl object of kind for the requested tags (IntlRequested) and the raw options. */
function IntlConstruct(newTarget: Value, name: Str, kind: i32, tags: Str, raw: Str): Obj {
  const ctor: Obj = IntlConstructor(name);
  let proto: Value = Get(AsObj(newTarget === undefined ? ctor : newTarget), "prototype");
  if (!IsObject(proto)) {
    proto = Get(ctor, "prototype");
  }
  return IntlCreate(AsObj(proto), kind, IntlResolve(kind, tags, raw));
}

/** The TypeError of an Intl constructor called without new. */
function IntlRequireNew(newTarget: Value, name: Str): void {
  if (newTarget === undefined) {
    throw NewTypeError("Constructor Intl." + name + " requires 'new'");
  }
}

/**
 * The bound format function of a NumberFormat or DateTimeFormat: \`this\` is the format.
 * @intrinsic
 */
function IntlBoundFormat(this: Value, x: Value): Str {
  const spec: Value = IntlSpec(this, 0);
  if (spec !== undefined) {
    return AsStr(IntlFormat(0, AsStr(spec), IntlNumeric(x), undefined, false));
  }
  return AsStr(IntlFormat(1, AsStr(IntlSpec(this, 1)), IntlDateValue(x), undefined, false));
}

/**
 * The bound compare function of a Collator: \`this\` is the collator.
 * @intrinsic
 */
function IntlBoundCompare(this: Value, x: Value, y: Value): f64 {
  return AsNumber(IntlFormat(2, AsStr(IntlSpec(this, 2)), ToString(x), ToString(y), false));
}

/** The cached bound function (closure kind) of an Intl object. */
function IntlBoundFunction(o: Value, kind: i32, length: i32): Value {
  const cached: Value = IntlBound(AsObj(o), undefined);
  if (cached !== undefined) {
    return cached;
  }
  return IntlBound(AsObj(o), NewBuiltinClosure(kind, o, length));
}

/** The options its constructor reads. */
function IntlNfOptions(): Str {
  return "localeMatcher:s,numberingSystem:s,style:s,currency:s,currencyDisplay:s,currencySign:s,unit:s,unitDisplay:s,notation:s,minimumIntegerDigits:n,minimumFractionDigits:n,maximumFractionDigits:n,minimumSignificantDigits:n,maximumSignificantDigits:n,roundingIncrement:n,roundingMode:s,roundingPriority:s,trailingZeroDisplay:s,compactDisplay:s,useGrouping:g,signDisplay:s";
}
/** The options its constructor reads. */
function IntlDtfOptions(): Str {
  return "localeMatcher:s,calendar:s,numberingSystem:s,hour12:b,hourCycle:s,timeZone:s,weekday:s,era:s,year:s,month:s,day:s,dayPeriod:s,hour:s,minute:s,second:s,fractionalSecondDigits:n,timeZoneName:s,formatMatcher:s,dateStyle:s,timeStyle:s";
}
/** The options its constructor reads. */
function IntlCollatorOptions(): Str {
  return "usage:s,localeMatcher:s,collation:s,numeric:b,caseFirst:s,sensitivity:s,ignorePunctuation:b";
}
/** The options its constructor reads. */
function IntlPluralOptions(): Str {
  return "localeMatcher:s,type:s,minimumIntegerDigits:n,minimumFractionDigits:n,maximumFractionDigits:n,minimumSignificantDigits:n,maximumSignificantDigits:n,roundingIncrement:n,roundingMode:s,roundingPriority:s,trailingZeroDisplay:s";
}

/**
 * The %Intl% object.
 * @value globalThis.Intl
 * @attributes writable configurable
 */
function IntlObject(): Obj {
  return ObjectCreate(RealmIntrinsic("ObjectPrototype"));
}

/**
 * Intl [ %Symbol.toStringTag% ].
 * @value Intl[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlToStringTag(): Str {
  return "Intl";
}

/**
 * TransformCase's language for StrToCase (0 lower, 1 upper): V8 takes the first requested
 * locale's language and applies ICU's rules only for tr, az, lt and el.
 */
function IntlCaseMode(locales: Value, upper: i32): i32 {
  if (locales === undefined) {
    return upper;
  }
  const tags: Str = IntlLocales(IntlLocaleTags(locales), 0);
  const nl: f64 = StrIndexOf(tags, "\n", 0);
  const first: Str = nl < 0 ? tags : StrSubstring(tags, 0, nl);
  const n: u32 = StrLength(first);
  const lang: Str = n >= 2 && (n === 2 || StrCharCodeAt(first, 2) === 45) ? StrSubstring(first, 0, 2) : "";
  if (lang === "tr" || lang === "az") {
    return upper + 2;
  }
  if (lang === "lt") {
    return upper + 4;
  }
  if (lang === "el") {
    return upper + 6;
  }
  return upper;
}

/**
 * Intl.getCanonicalLocales ( locales ).
 * @builtin Intl.getCanonicalLocales
 * @length 1
 */
function IntlGetCanonicalLocales(locales?: Value): Obj {
  return IntlSplit(IntlLocales(IntlLocaleTags(locales), 0), "\n", 0);
}

/** SupportedLocales ( availableLocales, requestedLocales, options ). */
function IntlSupportedLocales(locales: Value, options: Value, kind: i32, ctor: Str): Obj {
  const tags: Str = IntlLocales(IntlLocaleTags(locales), kind + 1);
  if (options !== undefined) {
    if (options === null) {
      throw NewTypeError(ctor + ".supportedLocalesOf called on null or undefined");
    }
    const m: Value = Get(ToObject(options), "localeMatcher");
    if (m !== undefined) {
      const s: Str = ToString(m);
      if (s !== "lookup" && s !== "best fit") {
        throw NewRangeError("Value " + s + " out of range for " + ctor + ".supportedLocalesOf options property localeMatcher");
      }
    }
  }
  return IntlSplit(tags, "\n", 0);
}

// ---- NumberFormat ----------------------------------------------------------------------

/** A new NumberFormat. */
function IntlNewNumberFormat(newTarget: Value, locales: Value, options: Value): Obj {
  const tags: Str = IntlRequested(locales);
  return IntlConstruct(newTarget, "NumberFormat", 0, tags, IntlReadOptions(IntlNoNull(locales, options, "Intl.NumberFormat called on null or undefined"), IntlNfOptions()));
}

/**
 * Intl.NumberFormat ( [ locales [ , options ] ] ).
 * @constructor Intl.NumberFormat
 * @length 0
 */
function IntlNumberFormatConstructor(locales?: Value, options?: Value): Obj {
  return IntlNewNumberFormat(new.target, locales, options);
}

/**
 * Intl.NumberFormat.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.NumberFormat.supportedLocalesOf
 * @length 1
 */
function IntlNumberFormatSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 0, "Intl.NumberFormat");
}

/** ToIntlMathematicalValue: a number, or a BigInt's or string's decimal digits. */
function IntlNumeric(x: Value): Value {
  const p: Value = ToPrimitive(x, "number");
  if (IsBigInt(p)) {
    return BigIntToString(p, 10);
  }
  if (IsString(p)) {
    return p;
  }
  return ToNumber(p);
}

/**
 * get Intl.NumberFormat.prototype.format.
 * @getter Intl.NumberFormat.prototype.format
 */
function IntlNumberFormatPrototypeFormat(this: Value): Value {
  IntlThis(this, 0, "get Intl.NumberFormat.prototype.format");
  return IntlBoundFunction(this, 25, 1);
}

/**
 * Intl.NumberFormat.prototype.formatToParts ( x ).
 * @builtin Intl.NumberFormat.prototype.formatToParts
 */
function IntlNumberFormatPrototypeFormatToParts(this: Value, x?: Value): Obj {
  const spec: Str = IntlThis(this, 0, "Intl.NumberFormat.prototype.formatToParts");
  return IntlParts(IntlFormat(0, spec, IntlNumeric(x), undefined, true), undefined);
}

/** formatRange's two values, read as the spec reads them. */
function IntlRangeValue(v: Value, which: Str): Value {
  if (v === undefined) {
    throw NewTypeError("Invalid " + which + " : undefined");
  }
  const n: Value = IntlNumeric(v);
  // An object that reads as NaN shows as V8 shows it in the RangeError ("Invalid start : #<Object>").
  if (IsObject(v) && n !== n) {
    return "#<Object>";
  }
  return n;
}

/**
 * Intl.NumberFormat.prototype.formatRange ( start, end ).
 * @builtin Intl.NumberFormat.prototype.formatRange
 * @length 2
 */
function IntlNumberFormatPrototypeFormatRange(this: Value, start?: Value, end?: Value): Str {
  const spec: Str = IntlThis(this, 0, "Intl.NumberFormat.prototype.formatRange");
  if (start === undefined || end === undefined) {
    IntlRangeValue(start, "start");
    IntlRangeValue(end, "end");
  }
  const x: Value = IntlRangeValue(start, "start");
  return AsStr(IntlFormat(0, spec, x, IntlRangeValue(end, "end"), false));
}

/**
 * Intl.NumberFormat.prototype.formatRangeToParts ( start, end ).
 * @builtin Intl.NumberFormat.prototype.formatRangeToParts
 * @length 2
 */
function IntlNumberFormatPrototypeFormatRangeToParts(this: Value, start?: Value, end?: Value): Obj {
  const spec: Str = IntlThis(this, 0, "Intl.NumberFormat.prototype.formatRangeToParts");
  if (start === undefined || end === undefined) {
    IntlRangeValue(start, "start");
    IntlRangeValue(end, "end");
  }
  const x: Value = IntlRangeValue(start, "start");
  return IntlParts(IntlFormat(0, spec, x, IntlRangeValue(end, "end"), true), undefined);
}

/**
 * Intl.NumberFormat.prototype.resolvedOptions ( ).
 * @builtin Intl.NumberFormat.prototype.resolvedOptions
 */
function IntlNumberFormatPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 0, "Intl.NumberFormat.prototype.resolvedOptions"));
}

/**
 * Intl.NumberFormat.prototype [ %Symbol.toStringTag% ].
 * @value Intl.NumberFormat.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlNumberFormatToStringTag(): Str {
  return "Intl.NumberFormat";
}

// ---- DateTimeFormat --------------------------------------------------------------------

/**
 * A new DateTimeFormat; required and defaults as ToDateTimeOptions takes them (any, date
 * or time; date, time or all).
 */
function IntlNewDateTimeFormat(newTarget: Value, locales: Value, options: Value, required: Str, defaults: Str): Obj {
  const tags: Str = IntlRequested(locales);
  let raw: Str = IntlReadOptions(options, IntlDtfOptions());
  raw = IntlWith(raw, "@required", required);
  raw = IntlWith(raw, "@defaults", defaults);
  raw = IntlWith(raw, "@offset", NumberToString(0 - DateTimezoneOffset(0), 10));
  return IntlConstruct(newTarget, "DateTimeFormat", 1, tags, raw);
}

/**
 * Intl.DateTimeFormat ( [ locales [ , options ] ] ).
 * @constructor Intl.DateTimeFormat
 * @length 0
 */
function IntlDateTimeFormatConstructor(locales?: Value, options?: Value): Obj {
  return IntlNewDateTimeFormat(new.target, locales, options, "any", "date");
}

/**
 * Intl.DateTimeFormat.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.DateTimeFormat.supportedLocalesOf
 * @length 1
 */
function IntlDateTimeFormatSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 1, "Intl.DateTimeFormat");
}

/** The time value a DateTimeFormat formats: now for undefined. */
function IntlDateValue(x: Value): f64 {
  if (x === undefined) {
    return DateNow();
  }
  return ToNumber(x);
}

/**
 * get Intl.DateTimeFormat.prototype.format.
 * @getter Intl.DateTimeFormat.prototype.format
 */
function IntlDateTimeFormatPrototypeFormat(this: Value): Value {
  IntlThis(this, 1, "get Intl.DateTimeFormat.prototype.format");
  return IntlBoundFunction(this, 25, 1);
}

/**
 * Intl.DateTimeFormat.prototype.formatToParts ( date ).
 * @builtin Intl.DateTimeFormat.prototype.formatToParts
 */
function IntlDateTimeFormatPrototypeFormatToParts(this: Value, date?: Value): Obj {
  const spec: Str = IntlThis(this, 1, "Intl.DateTimeFormat.prototype.formatToParts");
  return IntlParts(IntlFormat(1, spec, IntlDateValue(date), undefined, true), undefined);
}

/** formatRange's two dates: both must be defined, then each is read as a number, start first. */
function IntlDateRange(start: Value, end: Value): f64 {
  if (start === undefined || end === undefined) {
    throw NewTypeError("Invalid time value");
  }
  return IntlRangeNumber(start);
}

/** ToNumber with V8's messages for the values it rejects. */
function IntlRangeNumber(v: Value): f64 {
  if (IsBigInt(v)) {
    throw NewTypeError("Cannot convert a BigInt value to a number");
  }
  if (IsSymbol(v)) {
    throw NewTypeError("Cannot convert a Symbol value to a number");
  }
  return ToNumber(v);
}

/**
 * Intl.DateTimeFormat.prototype.formatRange ( startDate, endDate ).
 * @builtin Intl.DateTimeFormat.prototype.formatRange
 * @length 2
 */
function IntlDateTimeFormatPrototypeFormatRange(this: Value, startDate?: Value, endDate?: Value): Str {
  const spec: Str = IntlThis(this, 1, "Intl.DateTimeFormat.prototype.formatRange");
  const x: f64 = IntlDateRange(startDate, endDate);
  return AsStr(IntlFormat(1, spec, x, IntlRangeNumber(endDate), false));
}

/**
 * Intl.DateTimeFormat.prototype.formatRangeToParts ( startDate, endDate ).
 * @builtin Intl.DateTimeFormat.prototype.formatRangeToParts
 * @length 2
 */
function IntlDateTimeFormatPrototypeFormatRangeToParts(this: Value, startDate?: Value, endDate?: Value): Obj {
  const spec: Str = IntlThis(this, 1, "Intl.DateTimeFormat.prototype.formatRangeToParts");
  const x: f64 = IntlDateRange(startDate, endDate);
  return IntlParts(IntlFormat(1, spec, x, IntlRangeNumber(endDate), true), undefined);
}


/**
 * Intl.DateTimeFormat.prototype.resolvedOptions ( ).
 * @builtin Intl.DateTimeFormat.prototype.resolvedOptions
 */
function IntlDateTimeFormatPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 1, "Intl.DateTimeFormat.prototype.resolvedOptions"));
}

/**
 * Intl.DateTimeFormat.prototype [ %Symbol.toStringTag% ].
 * @value Intl.DateTimeFormat.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlDateTimeFormatToStringTag(): Str {
  return "Intl.DateTimeFormat";
}

// ---- Collator --------------------------------------------------------------------------

/** A new Collator. */
function IntlNewCollator(newTarget: Value, locales: Value, options: Value): Obj {
  const tags: Str = IntlRequested(locales);
  return IntlConstruct(newTarget, "Collator", 2, tags, IntlReadOptions(options, IntlCollatorOptions()));
}

/**
 * Intl.Collator ( [ locales [ , options ] ] ).
 * @constructor Intl.Collator
 * @length 0
 */
function IntlCollatorConstructor(locales?: Value, options?: Value): Obj {
  return IntlNewCollator(new.target, locales, options);
}

/**
 * Intl.Collator.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.Collator.supportedLocalesOf
 * @length 1
 */
function IntlCollatorSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 2, "Intl.Collator");
}

/**
 * get Intl.Collator.prototype.compare.
 * @getter Intl.Collator.prototype.compare
 */
function IntlCollatorPrototypeCompare(this: Value): Value {
  IntlThis(this, 2, "get Intl.Collator.prototype.compare");
  return IntlBoundFunction(this, 26, 2);
}

/**
 * Intl.Collator.prototype.resolvedOptions ( ).
 * @builtin Intl.Collator.prototype.resolvedOptions
 */
function IntlCollatorPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 2, "Intl.Collator.prototype.resolvedOptions"));
}

/**
 * Intl.Collator.prototype [ %Symbol.toStringTag% ].
 * @value Intl.Collator.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlCollatorToStringTag(): Str {
  return "Intl.Collator";
}

// ---- PluralRules -----------------------------------------------------------------------

/**
 * Intl.PluralRules ( [ locales [ , options ] ] ).
 * @constructor Intl.PluralRules
 * @length 0
 */
function IntlPluralRulesConstructor(locales?: Value, options?: Value): Obj {
  IntlRequireNew(new.target, "PluralRules");
  const tags: Str = IntlRequested(locales);
  return IntlConstruct(new.target, "PluralRules", 3, tags, IntlReadOptions(IntlNoNull(locales, options, "Intl.PluralRules called on null or undefined"), IntlPluralOptions()));
}

/**
 * Intl.PluralRules.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.PluralRules.supportedLocalesOf
 * @length 1
 */
function IntlPluralRulesSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 3, "Intl.PluralRules");
}

/**
 * Intl.PluralRules.prototype.select ( value ).
 * @builtin Intl.PluralRules.prototype.select
 */
function IntlPluralRulesPrototypeSelect(this: Value, value?: Value): Str {
  const spec: Str = IntlThis(this, 3, "Intl.PluralRules.prototype.select");
  return AsStr(IntlFormat(3, spec, ToNumber(value), undefined, false));
}

/**
 * Intl.PluralRules.prototype.selectRange ( start, end ).
 * @builtin Intl.PluralRules.prototype.selectRange
 * @length 2
 */
function IntlPluralRulesPrototypeSelectRange(this: Value, start?: Value, end?: Value): Str {
  const spec: Str = IntlThis(this, 3, "Intl.PluralRules.prototype.selectRange");
  if (start === undefined) {
    throw NewTypeError("Invalid startRange : undefined");
  }
  if (end === undefined) {
    throw NewTypeError("Invalid endRange : undefined");
  }
  const x: f64 = ToNumber(start);
  const y: f64 = ToNumber(end);
  if (x !== x) {
    throw NewRangeError("Invalid startRange : NaN");
  }
  if (y !== y) {
    throw NewRangeError("Invalid endRange : NaN");
  }
  const b: Str = AsStr(IntlFormat(3, spec, y, undefined, false));
  return AsStr(IntlFormat(3, spec, x, b, false));
}

/**
 * Intl.PluralRules.prototype.resolvedOptions ( ).
 * @builtin Intl.PluralRules.prototype.resolvedOptions
 */
function IntlPluralRulesPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 3, "Intl.PluralRules.prototype.resolvedOptions"));
}

/**
 * Intl.PluralRules.prototype [ %Symbol.toStringTag% ].
 * @value Intl.PluralRules.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlPluralRulesToStringTag(): Str {
  return "Intl.PluralRules";
}

// ---- ListFormat ------------------------------------------------------------------------

/**
 * Intl.ListFormat ( [ locales [ , options ] ] ).
 * @constructor Intl.ListFormat
 * @length 0
 */
function IntlListFormatConstructor(locales?: Value, options?: Value): Obj {
  IntlRequireNew(new.target, "ListFormat");
  const tags: Str = IntlRequested(locales);
  return IntlConstruct(new.target, "ListFormat", 4, tags, IntlReadOptions(IntlNoNull(locales, options, "invalid_argument"), "localeMatcher:s,type:s,style:s"));
}

/**
 * Intl.ListFormat.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.ListFormat.supportedLocalesOf
 * @length 1
 */
function IntlListFormatSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 4, "Intl.ListFormat");
}

/** StringListFromIterable, joined by \u0001. */
function IntlStringList(iterable: Value): Str {
  if (iterable === undefined) {
    return "";
  }
  const list: Obj = IterableToList(iterable);
  const len: f64 = LengthOfArrayLike(list);
  let out: Str = "";
  let i: f64 = 0;
  while (i < len) {
    const v: Value = Get(list, i);
    if (!IsString(v)) {
      throw NewTypeError("Iterable yielded " + (IsSymbol(v) ? "Symbol()" : ToString(v)) + " which is not a string");
    }
    out = i === 0 ? AsStr(v) : out + "\u0001" + AsStr(v);
    i += 1;
  }
  return out;
}

/**
 * Intl.ListFormat.prototype.format ( list ).
 * @builtin Intl.ListFormat.prototype.format
 */
function IntlListFormatPrototypeFormat(this: Value, list?: Value): Str {
  const spec: Str = IntlThis(this, 4, "Intl.ListFormat.prototype.format");
  return AsStr(IntlFormat(4, spec, IntlStringList(list), undefined, false));
}

/**
 * Intl.ListFormat.prototype.formatToParts ( list ).
 * @builtin Intl.ListFormat.prototype.formatToParts
 */
function IntlListFormatPrototypeFormatToParts(this: Value, list?: Value): Obj {
  const spec: Str = IntlThis(this, 4, "Intl.ListFormat.prototype.formatToParts");
  return IntlParts(IntlFormat(4, spec, IntlStringList(list), undefined, true), undefined);
}

/**
 * Intl.ListFormat.prototype.resolvedOptions ( ).
 * @builtin Intl.ListFormat.prototype.resolvedOptions
 */
function IntlListFormatPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 4, "Intl.ListFormat.prototype.resolvedOptions"));
}

/**
 * Intl.ListFormat.prototype [ %Symbol.toStringTag% ].
 * @value Intl.ListFormat.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlListFormatToStringTag(): Str {
  return "Intl.ListFormat";
}

// ---- RelativeTimeFormat ----------------------------------------------------------------

/**
 * Intl.RelativeTimeFormat ( [ locales [ , options ] ] ).
 * @constructor Intl.RelativeTimeFormat
 * @length 0
 */
function IntlRelativeTimeFormatConstructor(locales?: Value, options?: Value): Obj {
  IntlRequireNew(new.target, "RelativeTimeFormat");
  const tags: Str = IntlRequested(locales);
  const raw: Str = IntlReadOptions(IntlNoNull(locales, options, "Intl.RelativeTimeFormat called on null or undefined"), "localeMatcher:s,numberingSystem:s,style:s,numeric:s");
  return IntlConstruct(new.target, "RelativeTimeFormat", 5, tags, raw);
}

/**
 * Intl.RelativeTimeFormat.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.RelativeTimeFormat.supportedLocalesOf
 * @length 1
 */
function IntlRelativeTimeFormatSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 5, "Intl.RelativeTimeFormat");
}

/**
 * Intl.RelativeTimeFormat.prototype.format ( value, unit ).
 * @builtin Intl.RelativeTimeFormat.prototype.format
 */
function IntlRelativeTimeFormatPrototypeFormat(this: Value, value?: Value, unit?: Value): Str {
  const spec: Str = IntlThis(this, 5, "Intl.RelativeTimeFormat.prototype.format");
  return AsStr(IntlFormat(5, spec, ToNumber(value), ToString(unit), false));
}

/**
 * Intl.RelativeTimeFormat.prototype.formatToParts ( value, unit ).
 * @builtin Intl.RelativeTimeFormat.prototype.formatToParts
 */
function IntlRelativeTimeFormatPrototypeFormatToParts(this: Value, value?: Value, unit?: Value): Obj {
  const spec: Str = IntlThis(this, 5, "Intl.RelativeTimeFormat.prototype.formatToParts");
  const u: Str = ToString(unit);
  const parts: Value = IntlFormat(5, spec, ToNumber(value), u, true);
  const singular: Str = StrLength(u) > 1 && StrCharCodeAt(u, StrLength(u) - 1) === 115 ? StrSubstring(u, 0, StrLength(u) - 1) : u;
  return IntlParts(parts, singular);
}

/**
 * Intl.RelativeTimeFormat.prototype.resolvedOptions ( ).
 * @builtin Intl.RelativeTimeFormat.prototype.resolvedOptions
 */
function IntlRelativeTimeFormatPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 5, "Intl.RelativeTimeFormat.prototype.resolvedOptions"));
}

/**
 * Intl.RelativeTimeFormat.prototype [ %Symbol.toStringTag% ].
 * @value Intl.RelativeTimeFormat.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlRelativeTimeFormatToStringTag(): Str {
  return "Intl.RelativeTimeFormat";
}

// ---- Locale (ECMA-402 14) ---------------------------------------------------------------
// A Locale's spec (kind 16) is its canonical tag; IntlFormat(16, tag, what) reads a getter
// (locale.rs). Kind 15 is Intl.supportedValuesOf's lists.

/**
 * Intl.Locale ( tag [ , options ] ).
 * @constructor Intl.Locale
 * @length 1
 */
function IntlLocaleConstructor(tag?: Value, options?: Value): Obj {
  IntlRequireNew(new.target, "Locale");
  if (!IsString(tag) && !IsObject(tag)) {
    throw NewTypeError("First argument to Intl.Locale constructor can't be empty or missing");
  }
  const own: Value = IntlSpec(tag, 16);
  const s: Str = own !== undefined ? AsStr(own) : ToString(tag);
  if (s === "") {
    throw NewRangeError("First argument to Intl.Locale constructor can't be empty or missing");
  }
  if (options === null) {
    throw NewTypeError("Intl.Locale called on null or undefined");
  }
  const raw: Str = IntlReadOptions(options, "language:s,script:s,region:s,calendar:s,collation:s,hourCycle:s,caseFirst:s,numeric:b,numberingSystem:s");
  return IntlConstruct(new.target, "Locale", 16, s, raw);
}

/** A Locale getter's value: what of the Locale v, named method in its TypeError. */
function IntlLocaleGet(v: Value, what: Str, method: Str): Value {
  const spec: Str = IntlThis(v, 16, method);
  const r: Value = IntlFormat(16, spec, what, undefined, false);
  if (r === undefined) {
    return undefined;
  }
  const s: Str = AsStr(r);
  if (s === "!t") {
    return true;
  }
  if (s === "!f") {
    return false;
  }
  if (StrLength(s) > 0 && StrSubstring(s, 0, 1) === "@") {
    return IntlSplit(s, ",", 1);
  }
  return s;
}

/** A new Locale of the canonical tag t. */
function IntlNewLocale(t: Str): Obj {
  return IntlCreate(AsObj(Get(IntlConstructor("Locale"), "prototype")), 16, t);
}

/**
 * Intl.Locale.prototype.maximize ( ).
 * @builtin Intl.Locale.prototype.maximize
 */
function IntlLocalePrototypeMaximize(this: Value): Obj {
  return IntlNewLocale(AsStr(IntlLocaleGet(this, "maximize", "Intl.Locale.prototype.maximize")));
}

/**
 * Intl.Locale.prototype.minimize ( ).
 * @builtin Intl.Locale.prototype.minimize
 */
function IntlLocalePrototypeMinimize(this: Value): Obj {
  return IntlNewLocale(AsStr(IntlLocaleGet(this, "minimize", "Intl.Locale.prototype.minimize")));
}

/**
 * Intl.Locale.prototype.toString ( ).
 * @builtin Intl.Locale.prototype.toString
 */
function IntlLocalePrototypeToString(this: Value): Str {
  return IntlThis(this, 16, "Intl.Locale.prototype.toString");
}

/**
 * get Intl.Locale.prototype.baseName.
 * @getter Intl.Locale.prototype.baseName
 */
function IntlLocalePrototypeBaseName(this: Value): Value {
  return IntlLocaleGet(this, "baseName", "Intl.Locale.prototype.baseName");
}

/**
 * get Intl.Locale.prototype.calendar.
 * @getter Intl.Locale.prototype.calendar
 */
function IntlLocalePrototypeCalendar(this: Value): Value {
  return IntlLocaleGet(this, "calendar", "Intl.Locale.prototype.calendar");
}

/**
 * get Intl.Locale.prototype.caseFirst.
 * @getter Intl.Locale.prototype.caseFirst
 */
function IntlLocalePrototypeCaseFirst(this: Value): Value {
  return IntlLocaleGet(this, "caseFirst", "Intl.Locale.prototype.caseFirst");
}

/**
 * get Intl.Locale.prototype.collation.
 * @getter Intl.Locale.prototype.collation
 */
function IntlLocalePrototypeCollation(this: Value): Value {
  return IntlLocaleGet(this, "collation", "Intl.Locale.prototype.collation");
}

/**
 * get Intl.Locale.prototype.hourCycle.
 * @getter Intl.Locale.prototype.hourCycle
 */
function IntlLocalePrototypeHourCycle(this: Value): Value {
  return IntlLocaleGet(this, "hourCycle", "Intl.Locale.prototype.hourCycle");
}

/**
 * get Intl.Locale.prototype.numeric.
 * @getter Intl.Locale.prototype.numeric
 */
function IntlLocalePrototypeNumeric(this: Value): Value {
  return IntlLocaleGet(this, "numeric", "Intl.Locale.prototype.numeric");
}

/**
 * get Intl.Locale.prototype.numberingSystem.
 * @getter Intl.Locale.prototype.numberingSystem
 */
function IntlLocalePrototypeNumberingSystem(this: Value): Value {
  return IntlLocaleGet(this, "numberingSystem", "Intl.Locale.prototype.numberingSystem");
}

/**
 * get Intl.Locale.prototype.language.
 * @getter Intl.Locale.prototype.language
 */
function IntlLocalePrototypeLanguage(this: Value): Value {
  return IntlLocaleGet(this, "language", "Intl.Locale.prototype.language");
}

/**
 * get Intl.Locale.prototype.script.
 * @getter Intl.Locale.prototype.script
 */
function IntlLocalePrototypeScript(this: Value): Value {
  return IntlLocaleGet(this, "script", "Intl.Locale.prototype.script");
}

/**
 * get Intl.Locale.prototype.region.
 * @getter Intl.Locale.prototype.region
 */
function IntlLocalePrototypeRegion(this: Value): Value {
  return IntlLocaleGet(this, "region", "Intl.Locale.prototype.region");
}

/**
 * get Intl.Locale.prototype.calendars.
 * @getter Intl.Locale.prototype.calendars
 */
function IntlLocalePrototypeCalendars(this: Value): Value {
  return IntlLocaleGet(this, "calendars", "Intl.Locale.prototype.calendars");
}

/**
 * get Intl.Locale.prototype.collations.
 * @getter Intl.Locale.prototype.collations
 */
function IntlLocalePrototypeCollations(this: Value): Value {
  return IntlLocaleGet(this, "collations", "Intl.Locale.prototype.collations");
}

/**
 * get Intl.Locale.prototype.hourCycles.
 * @getter Intl.Locale.prototype.hourCycles
 */
function IntlLocalePrototypeHourCycles(this: Value): Value {
  return IntlLocaleGet(this, "hourCycles", "Intl.Locale.prototype.hourCycles");
}

/**
 * get Intl.Locale.prototype.numberingSystems.
 * @getter Intl.Locale.prototype.numberingSystems
 */
function IntlLocalePrototypeNumberingSystems(this: Value): Value {
  return IntlLocaleGet(this, "numberingSystems", "Intl.Locale.prototype.numberingSystems");
}

/**
 * get Intl.Locale.prototype.timeZones.
 * @getter Intl.Locale.prototype.timeZones
 */
function IntlLocalePrototypeTimeZones(this: Value): Value {
  return IntlLocaleGet(this, "timeZones", "Intl.Locale.prototype.timeZones");
}

/**
 * get Intl.Locale.prototype.textInfo.
 * @getter Intl.Locale.prototype.textInfo
 */
function IntlLocalePrototypeTextInfo(this: Value): Obj {
  const d: Value = IntlLocaleGet(this, "textInfo", "Intl.Locale.prototype.textInfo");
  const o: Obj = ObjectCreate(RealmIntrinsic("ObjectPrototype"));
  CreateDataPropertyOrThrow(o, "direction", d);
  return o;
}

/**
 * get Intl.Locale.prototype.weekInfo.
 * @getter Intl.Locale.prototype.weekInfo
 */
function IntlLocalePrototypeWeekInfo(this: Value): Obj {
  // firstDay:weekend:minimalDays
  const s: Str = AsStr(IntlLocaleGet(this, "weekInfo", "Intl.Locale.prototype.weekInfo"));
  const a: f64 = StrIndexOf(s, ":", 0);
  const b: f64 = StrIndexOf(s, ":", a + 1);
  const o: Obj = ObjectCreate(RealmIntrinsic("ObjectPrototype"));
  CreateDataPropertyOrThrow(o, "firstDay", ToNumber(StrSubstring(s, 0, a)));
  const weekend: Obj = ArrayCreate(0);
  const days: Obj = IntlSplit(StrSubstring(s, a + 1, b), ",", 0);
  let i: f64 = 0;
  const n: f64 = LengthOfArrayLike(days);
  while (i < n) {
    ArrayPushDense(weekend, ToNumber(Get(days, i)));
    i += 1;
  }
  CreateDataPropertyOrThrow(o, "weekend", weekend);
  CreateDataPropertyOrThrow(o, "minimalDays", ToNumber(StrSubstring(s, b + 1, StrLength(s))));
  return o;
}

/**
 * Intl.Locale.prototype [ %Symbol.toStringTag% ].
 * @value Intl.Locale.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlLocaleToStringTag(): Str {
  return "Intl.Locale";
}

/**
 * Intl.supportedValuesOf ( key ).
 * @builtin Intl.supportedValuesOf
 * @length 1
 */
function IntlSupportedValuesOf(key?: Value): Obj {
  return IntlSplit(AsStr(IntlFormat(15, ToString(key), undefined, undefined, false)), ",", 0);
}


// ---- DisplayNames (ECMA-402 12) -------------------------------------------------------
// Kind 17: displaynames.rs.

/**
 * Intl.DisplayNames ( locales, options ).
 * @constructor Intl.DisplayNames
 * @length 2
 */
function IntlDisplayNamesConstructor(locales?: Value, options?: Value): Obj {
  IntlRequireNew(new.target, "DisplayNames");
  const tags: Str = IntlRequested(locales);
  if (options === undefined || options === null) {
    throw NewTypeError("invalid_argument");
  }
  return IntlConstruct(new.target, "DisplayNames", 17, tags, IntlReadOptions(options, "localeMatcher:s,style:s,type:s,fallback:s,languageDisplay:s"));
}

/**
 * Intl.DisplayNames.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.DisplayNames.supportedLocalesOf
 * @length 1
 */
function IntlDisplayNamesSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 17, "Intl.DisplayNames");
}

/**
 * Intl.DisplayNames.prototype.resolvedOptions ( ).
 * @builtin Intl.DisplayNames.prototype.resolvedOptions
 */
function IntlDisplayNamesPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 17, "Intl.DisplayNames.prototype.resolvedOptions"));
}

/**
 * Intl.DisplayNames.prototype.of ( code ).
 * @builtin Intl.DisplayNames.prototype.of
 */
function IntlDisplayNamesPrototypeOf(this: Value, code?: Value): Value {
  const spec: Str = IntlThis(this, 17, "Intl.DisplayNames.prototype.of");
  return IntlFormat(17, spec, ToString(code), undefined, false);
}

/**
 * Intl.DisplayNames.prototype [ %Symbol.toStringTag% ].
 * @value Intl.DisplayNames.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlDisplayNamesToStringTag(): Str {
  return "Intl.DisplayNames";
}

// ---- Segmenter (ECMA-402 18) ----------------------------------------------------------
// Kind 18 is a Segmenter (segmenter.rs), 19 its Segments and 20 their iterator; a Segments'
// and an iterator's slot holds the internal array [string, starts, wordLike, position].

/**
 * Intl.Segmenter ( [ locales [ , options ] ] ).
 * @constructor Intl.Segmenter
 * @length 0
 */
function IntlSegmenterConstructor(locales?: Value, options?: Value): Obj {
  IntlRequireNew(new.target, "Segmenter");
  const tags: Str = IntlRequested(locales);
  if (options === null) {
    throw NewTypeError("Cannot convert undefined or null to object");
  }
  return IntlConstruct(new.target, "Segmenter", 18, tags, IntlReadOptions(options, "localeMatcher:s,granularity:s"));
}

/**
 * Intl.Segmenter.supportedLocalesOf ( locales [ , options ] ).
 * @builtin Intl.Segmenter.supportedLocalesOf
 * @length 1
 */
function IntlSegmenterSupportedLocalesOf(locales?: Value, options?: Value): Obj {
  return IntlSupportedLocales(locales, options, 18, "Intl.Segmenter");
}

/**
 * Intl.Segmenter.prototype.resolvedOptions ( ).
 * @builtin Intl.Segmenter.prototype.resolvedOptions
 */
function IntlSegmenterPrototypeResolvedOptions(this: Value): Obj {
  return IntlResolvedObject(IntlThis(this, 18, "Intl.Segmenter.prototype.resolvedOptions"));
}

/**
 * Intl.Segmenter.prototype.segment ( string ).
 * @builtin Intl.Segmenter.prototype.segment
 */
function IntlSegmenterPrototypeSegment(this: Value, string?: Value): Obj {
  const spec: Str = IntlThis(this, 18, "Intl.Segmenter.prototype.segment");
  const s: Str = ToString(string);
  const o: Obj = IntlCreate(RealmIntrinsic("IntlSegmentsPrototype"), 19, spec);
  const data: Obj = ArrayCreate(0);
  ArrayPushDense(data, s);
  ArrayPushDense(data, undefined);
  ArrayPushDense(data, undefined);
  ArrayPushDense(data, 0);
  IntlBound(o, data);
  return o;
}

/**
 * Intl.Segmenter.prototype [ %Symbol.toStringTag% ].
 * @value Intl.Segmenter.prototype[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlSegmenterToStringTag(): Str {
  return "Intl.Segmenter";
}

/** The segment starts (and the end) of a Segments' data, computed once. */
function IntlSegmentStarts(spec: Str, data: Obj): Obj {
  const have: Value = Get(data, 1);
  if (have !== undefined) {
    return AsObj(have);
  }
  const enc: Str = AsStr(IntlFormat(18, spec, Get(data, 0), undefined, false));
  const bar: f64 = StrIndexOf(enc, "|", 0);
  const list: Str = bar < 0 ? enc : StrSubstring(enc, 0, bar);
  const parts: Obj = IntlSplit(list, ",", 0);
  const starts: Obj = ArrayCreate(0);
  let i: f64 = 0;
  const n: f64 = LengthOfArrayLike(parts);
  while (i < n) {
    ArrayPushDense(starts, ToNumber(Get(parts, i)));
    i += 1;
  }
  Set(data, 1, starts);
  if (bar >= 0) {
    Set(data, 2, StrSubstring(enc, bar + 1, StrLength(enc)));
  }
  return starts;
}

/** CreateSegmentDataObject for the k-th segment. */
function IntlSegmentData(spec: Str, data: Obj, k: f64): Obj {
  const starts: Obj = IntlSegmentStarts(spec, data);
  const s: Str = AsStr(Get(data, 0));
  const a: f64 = ToNumber(Get(starts, k));
  const b: f64 = ToNumber(Get(starts, k + 1));
  const o: Obj = ObjectCreate(RealmIntrinsic("ObjectPrototype"));
  CreateDataPropertyOrThrow(o, "segment", StrSubstring(s, a, b));
  CreateDataPropertyOrThrow(o, "index", a);
  CreateDataPropertyOrThrow(o, "input", s);
  const flags: Value = Get(data, 2);
  if (flags !== undefined) {
    CreateDataPropertyOrThrow(o, "isWordLike", StrSubstring(AsStr(flags), k, k + 1) === "1");
  }
  return o;
}

/**
 * %IntlSegmentsPrototype%.containing ( index ).
 * @builtin %IntlSegmentsPrototype%.containing
 */
function IntlSegmentsPrototypeContaining(this: Value, index?: Value): Value {
  const spec: Str = IntlThis(this, 19, "%Segments.prototype%.containing");
  const data: Obj = AsObj(IntlBound(AsObj(this), undefined));
  const s: Str = AsStr(Get(data, 0));
  const n: f64 = ToIntegerOrInfinity(index);
  const len: f64 = StrLength(s);
  if (n < 0 || n >= len) {
    return undefined;
  }
  const starts: Obj = IntlSegmentStarts(spec, data);
  let lo: f64 = 0;
  let hi: f64 = LengthOfArrayLike(starts) - 1;
  while (hi - lo > 1) {
    const mid: f64 = MathFloor((lo + hi) / 2);
    if (ToNumber(Get(starts, mid)) <= n) {
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return IntlSegmentData(spec, data, lo);
}

/**
 * %IntlSegmentsPrototype% [ %Symbol.iterator% ] ( ).
 * @builtin %IntlSegmentsPrototype%[Symbol.iterator]
 */
function IntlSegmentsPrototypeIterator(this: Value): Obj {
  const spec: Str = IntlThis(this, 19, "%Segments.prototype%[@@iterator]");
  const data: Obj = AsObj(IntlBound(AsObj(this), undefined));
  const it: Obj = IntlCreate(RealmIntrinsic("IntlSegmentIteratorPrototype"), 20, spec);
  const own: Obj = ArrayCreate(0);
  ArrayPushDense(own, Get(data, 0));
  ArrayPushDense(own, Get(data, 1));
  ArrayPushDense(own, Get(data, 2));
  ArrayPushDense(own, 0);
  IntlBound(it, own);
  return it;
}

/**
 * %IntlSegmentIteratorPrototype%.next ( ).
 * @builtin %IntlSegmentIteratorPrototype%.next
 */
function IntlSegmentIteratorPrototypeNext(this: Value): Obj {
  const spec: Str = IntlThis(this, 20, "%SegmentIterator.prototype%.next");
  const data: Obj = AsObj(IntlBound(AsObj(this), undefined));
  const starts: Obj = IntlSegmentStarts(spec, data);
  const k: f64 = ToNumber(Get(data, 3));
  if (k + 1 >= LengthOfArrayLike(starts)) {
    return CreateIterResultObject(undefined, true);
  }
  Set(data, 3, k + 1);
  return CreateIterResultObject(IntlSegmentData(spec, data, k), false);
}

/**
 * %IntlSegmentIteratorPrototype% [ %Symbol.toStringTag% ].
 * @value %IntlSegmentIteratorPrototype%[Symbol.toStringTag]
 * @attributes configurable
 */
function IntlSegmentIteratorToStringTag(): Str {
  return "Segmenter String Iterator";
}

// ---- the locale-sensitive methods of other builtins (ECMA-402 chapters 19 to 21) -------

/** Number.prototype.toLocaleString and BigInt.prototype.toLocaleString with Intl. */
function IntlNumberToLocaleString(x: Value, locales: Value, options: Value): Str {
  const nf: Obj = IntlNewNumberFormat(undefined, locales, options);
  return AsStr(IntlFormat(0, AsStr(IntlSpec(nf, 0)), IntlNumeric(x), undefined, false));
}

/** Date.prototype.toLocale(Date|Time)String with Intl: "Invalid Date" for NaN. */
function IntlDateToLocaleString(t: f64, locales: Value, options: Value, required: Str, defaults: Str): Str {
  if (t !== t) {
    return "Invalid Date";
  }
  const dtf: Obj = IntlNewDateTimeFormat(undefined, locales, options, required, defaults);
  return AsStr(IntlFormat(1, AsStr(IntlSpec(dtf, 1)), t, undefined, false));
}

/** String.prototype.localeCompare with Intl. */
function IntlLocaleCompare(s: Str, that: Str, locales: Value, options: Value): f64 {
  const c: Obj = IntlNewCollator(undefined, locales, options);
  return AsNumber(IntlFormat(2, AsStr(IntlSpec(c, 2)), s, that, false));
}
