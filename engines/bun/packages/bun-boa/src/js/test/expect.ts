// expect() and its matchers: a port of src/runtime/test_runner/expect.rs and expect/*.rs. Each matcher keeps Bun's
// checks and messages; `M` is the `Matcher` of expect/matcher.rs.
import { ASYMMETRIC, builtinMatcher, deepEquals, deepMatch, setEqualityTesters } from "./equals.ts";
import {
  className,
  diffText,
  displayName,
  isDate,
  isError,
  isMap,
  isPromise,
  isRegExp,
  isSet,
  markup,
  setAsymmetricPrinter,
  show,
  showUnquoted,
} from "./format.ts";
import { append, native, runtime } from "./state.ts";

type Fn = (...args: any[]) => any;
const colors = () => native.colors;
const pretty = (text: string) => markup(text, colors());

/** `Matcher error` and other failures are plain Errors, as `global.throw` makes them. */
function thrown(message: string): Error {
  const error = new Error(message);
  return error;
}

// ── flags ─────────────────────────────────────────────────────────────────────
const NONE = 0,
  RESOLVES = 1,
  REJECTS = 2;
type PromiseFlag = typeof NONE | typeof RESOLVES | typeof REJECTS;
interface Flags {
  not: boolean;
  promise: PromiseFlag;
  soft?: boolean;
  poll?: [interval: number, timeout: number];
}

// ── the signature: `expect(received).not.toBe(expected)` ─────────────────────
const signature = (promise: PromiseFlag, not: boolean, name: string, params: string) =>
  pretty("<d>expect(<r><red>received<r><d>).<r>") +
  (promise === RESOLVES ? pretty("resolves<d>.<r>") : promise === REJECTS ? pretty("rejects<d>.<r>") : "") +
  (not ? pretty("not<d>.<r>") : "") +
  name +
  pretty("<d>(<r>") +
  pretty(params) +
  pretty("<d>)<r>");

const EXPECTED = "<green>expected<r>";
const NO_PARAMS = "";

/** One call of a matcher, as in expect/matcher.rs. */
class M {
  signedNot: boolean;
  constructor(
    readonly expect: Expect,
    readonly name: string,
    public params: string,
    readonly flags: Flags,
    readonly received: unknown,
  ) {
    this.signedNot = flags.not;
  }
  get not() {
    return this.flags.not;
  }
  get notWord() {
    return this.flags.not ? "not " : "";
  }
  passes(matched: boolean) {
    return matched !== this.flags.not;
  }
  show(v: unknown) {
    return show(v, { colors: colors() });
  }
  withParams(params: string): M {
    const m = new M(this.expect, this.name, params, this.flags, this.received);
    m.signedNot = this.signedNot;
    return m;
  }
  signed(not: boolean): M {
    const m = new M(this.expect, this.name, this.params, this.flags, this.received);
    m.signedNot = not;
    return m;
  }
  /** `fail!`: the body's markup is the template's; values already printed are inserted as they are. */
  fail(body: string): never {
    const label = labelOf(this.expect);
    throw thrown(`${label ?? signature(NONE, this.signedNot, this.name, this.params)}\n\n${body}`);
  }
  failf(template: string, ...values: unknown[]): never {
    let i = 0;
    const body = pretty(template).replace(/\{\}/g, () => String(values[i++]));
    this.fail(body);
  }
  failReceived(received: unknown): never {
    this.failf("Received: <red>{}<r>\n", this.show(received));
  }
  failExpected(expected: unknown, received: unknown): never {
    this.failf("Expected: {}<green>{}<r>\nReceived: <red>{}<r>\n", this.notWord, this.show(expected), this.show(received));
  }
  failExpectedTo(verb: string, expected: unknown, received: unknown): never {
    this.failf("Expected to {}{}: <green>{}<r>\nReceived: <red>{}<r>\n", this.notWord, verb, this.show(expected), this.show(received));
  }
  failDiff(expected: unknown, received: unknown): never {
    this.fail(diffOf(received, expected, this.not) + "\n");
  }
}

/** DiffFormatter: both values printed by pretty-format, then printDiff.rs. */
export function diffOf(received: unknown, expected: unknown, not = false): string {
  return native.diff(not, diffText(received), diffText(expected), colors());
}
export function diffStrings(received: string, expected: string, not = false): string {
  return native.diff(not, received, expected, colors());
}

// ── Expect ────────────────────────────────────────────────────────────────────
const VALUE = Symbol("value");
const FLAGS = Symbol("flags");
const LABEL = Symbol("label");
const RAN = Symbol("ran");

const labelOf = (e: Expect): string | undefined => e[LABEL];

function countExpect(n = 1) {
  // A matcher that runs again once a promise settled was counted the first time.
  if (journal?.replaying) return;
  const sequence = runtime.sequence;
  if (sequence && !sequence.concurrent) sequence.expectCalls += n;
  else runtime.strayExpects += n;
}

export class Expect {
  [VALUE]: unknown;
  [FLAGS]: Flags = { not: false, promise: NONE };
  [LABEL]: string | undefined;
  [RAN] = false;
  constructor() {
    throw thrown("expect() cannot be called with new");
  }
  get not(): this {
    const isNewChain = this[RAN];
    this[RAN] = false;
    this[FLAGS] = { ...this[FLAGS], not: !this[FLAGS].not || isNewChain };
    return this;
  }
  get resolves(): this {
    if (this[FLAGS].poll) throw thrown("expect.poll() does not support .resolves");
    if (this[FLAGS].promise === REJECTS) throw thrown("Cannot chain .resolves() after .rejects()");
    this[FLAGS] = { ...this[FLAGS], promise: RESOLVES };
    return this;
  }
  get rejects(): this {
    if (this[FLAGS].poll) throw thrown("expect.poll() does not support .rejects");
    if (this[FLAGS].promise === RESOLVES) throw thrown("Cannot chain .rejects() after .resolves()");
    this[FLAGS] = { ...this[FLAGS], promise: REJECTS };
    return this;
  }
}

function makeExpect(value: unknown, message?: unknown, flags?: Partial<Flags>): Expect {
  const e: Expect = Object.create(Expect.prototype);
  e[VALUE] = value;
  e[FLAGS] = { not: false, promise: NONE, ...flags };
  e[RAN] = false;
  if (typeof message === "string") e[LABEL] = message;
  else if (message !== undefined && message !== null && typeof message === "object" && typeof (message as any).toString === "function" && (message as any).toString !== Object.prototype.toString)
    e[LABEL] = String(message);
  return e;
}

const handled = (promise: Promise<unknown>) => Promise.prototype.then.call(promise, undefined, () => {});

/**
 * `Expect::promise_to_await`: a promise that settles the way `await value` would, or nothing if `value` is not
 * thenable. A settled promise is itself; anything else is adopted as `await` does, which reads `then` once (a getter
 * that throws is a rejection) and calls it in a microtask.
 */
function promiseToAwait(value: unknown): Promise<unknown> | undefined {
  const state = native.promiseState(value);
  if (state !== undefined) {
    handled(value as Promise<unknown>);
    if (state !== 0) return value as Promise<unknown>;
  }
  const adopted = new Promise(resolve => resolve(value));
  handled(adopted);
  return native.promiseState(adopted) === 1 ? undefined : adopted;
}

/** `Expect::received_promise`: the received thenable, or what the received function returns. */
function receivedPromise(received: unknown): Promise<unknown> | undefined {
  const promise = promiseToAwait(received);
  return promise === undefined && typeof received === "function" ? promiseToAwait((received as Fn)()) : promise;
}

/**
 * `received_from_promise`: runs `matcher` on what the received promise settled with, once it settled the way
 * `.resolves`/`.rejects` expects. The value goes to `matcher` as it is: returned from here, a thenable would be adopted.
 */
async function settledValue<R>(e: Expect, flags: Flags, name: string, params: string, matcher: (received: unknown) => R): Promise<Awaited<R>> {
  const value = e[VALUE];
  const promise = receivedPromise(value);
  let state: "fulfilled" | "rejected" | "none" = "none";
  let settled: unknown;
  if (promise !== undefined) {
    try {
      settled = await promise;
      state = "fulfilled";
    } catch (error) {
      settled = error;
      state = "rejected";
    }
  }
  const sig = labelOf(e) ?? signature(flags.promise, flags.not, name, params);
  const fail = (expected: string, received: string, shown: unknown): never => {
    throw thrown(`${sig}\n\n${pretty(`${expected}<r>\n${received}<red>`)}${show(shown, { colors: colors() })}${pretty("<r>")}\n`);
  };
  if (state === "none") fail("Expected promise", "Received: ", value);
  if (state === "fulfilled" && flags.promise === REJECTS) fail("Expected promise that rejects", "Received promise that resolved: ", value);
  if (state === "rejected" && flags.promise === RESOLVES) fail("Expected promise that resolves", "Received promise that rejected: ", value);
  return await matcher(settled);
}

const isThenable = (v: unknown): v is PromiseLike<unknown> =>
  (typeof v === "object" || typeof v === "function") && v !== null && typeof (v as any).then === "function";

type MatcherImpl = (m: M, ...args: any[]) => unknown;
interface MatcherDef {
  params: string;
  impl: MatcherImpl;
  /** Returns `this` when it passes. */
  chainable?: boolean;
  /** Counted only after its arguments are checked. */
  countLate?: boolean;
  length?: number;
}

const matchers: Record<string, MatcherDef> = {};

// ── expect_deferred.rs ───────────────────────────────────────────────────────
// A matcher that hits a pending promise returns a promise of its own, and runs again from the top once the pending
// one settles. The run again replays what the user code it called answered the first time (`Pass::once`).
class Deferred {
  constructor(readonly promise: Promise<unknown>) {}
}
interface Journal {
  answers: [threw: boolean, value: unknown][];
  cursor: number;
  replaying: boolean;
}
let journal: Journal | undefined;

/** `Pass::once`: calls user code the first time, and answers as it did when the matcher runs again. */
function once(ask: () => unknown): unknown {
  const j = journal;
  if (j?.replaying && j.cursor < j.answers.length) {
    const [threw, value] = j.answers[j.cursor++];
    if (threw) throw value;
    return value;
  }
  let threw = false;
  let value: unknown;
  try {
    value = ask();
  } catch (error) {
    threw = true;
    value = error;
  }
  if (j) {
    append(j.answers, [threw, value]);
    j.cursor++;
  }
  if (threw) throw value;
  return value;
}

/** `Pass::wait_for`: the running matcher returns a promise, and runs again once `promise` settles. */
function waitFor(promise: Promise<unknown>): never {
  if (journal === undefined)
    throw new Error(
      "An async matcher only works inside an expect() matcher, which then returns a promise: await expect(received).toEqual(expected)",
    );
  handled(promise);
  throw new Deferred(promise);
}

function runDeferring(attempt: () => unknown): unknown {
  const j: Journal = { answers: [], cursor: 0, replaying: false };
  const within = () => {
    const outer = journal;
    journal = j;
    try {
      return attempt();
    } finally {
      journal = outer;
    }
  };
  try {
    return within();
  } catch (error) {
    if (!(error instanceof Deferred)) throw error;
    const settled = (async (deferred: Deferred) => {
      for (;;) {
        await deferred.promise.then(
          () => {},
          () => {},
        );
        j.cursor = 0;
        j.replaying = true;
        try {
          const result = within();
          return isThenable(result) ? await result : result;
        } catch (again) {
          if (!(again instanceof Deferred)) throw again;
          deferred = again;
        }
      }
    })(error);
    // The test ends once the matchers it left waiting are done (`pending_matchers`).
    if (runtime.sequence) append(runtime.sequence.pendingMatchers, settled);
    return settled;
  }
}

function runMatcher(e: Expect, name: string, def: MatcherDef, args: unknown[]) {
  return runDeferring(() => runMatcherOnce(e, name, def, args));
}

function runMatcherOnce(e: Expect, name: string, def: MatcherDef, args: unknown[]) {
  const flags = e[FLAGS];
  e[RAN] = true;
  const run = (received: unknown) => {
    const m = new M(e, name, def.params, flags, received);
    if (!def.countLate) countExpect();
    const result = def.impl(m, ...args);
    return def.chainable && result === undefined ? e : result === PASSED ? undefined : result;
  };
  if (flags.poll) return poll(e, flags, run);
  if (flags.promise !== NONE)
    return settledValue(e, flags, name, def.params, received => {
      const r = run(received);
      return r === e ? undefined : r;
    });
  if (flags.soft) {
    try {
      return run(e[VALUE]);
    } catch (error) {
      if (runtime.sequence) append(runtime.sequence.softErrors, error);
      return undefined;
    }
  }
  return run(e[VALUE]);
}

async function poll(e: Expect, flags: Flags, run: (received: unknown) => unknown) {
  const [interval, timeout] = flags.poll!;
  const deadline = Date.now() + timeout;
  for (;;) {
    let value: unknown;
    try {
      value = await (e[VALUE] as Fn)();
      return run(value);
    } catch (error) {
      if (Date.now() >= deadline) throw error;
    }
    await new Promise(resolve => setTimeout(resolve, interval));
  }
}

const PASSED = Symbol("passed");

function define(name: string, params: string, impl: MatcherImpl, options: Partial<MatcherDef> = {}) {
  const def: MatcherDef = { params, impl, ...options };
  matchers[name] = def;
  const method = {
    [name](this: Expect, ...args: unknown[]) {
      if (!(this instanceof Expect)) throw new TypeError(`Expected this to be an Expect`);
      return runMatcher(this, name, def, args);
    },
  }[name];
  Object.defineProperty(method, "length", { value: options.length ?? impl.length - 1 });
  Object.defineProperty(Expect.prototype, name, { value: method, writable: true, configurable: true, enumerable: false });
}
const alias = (name: string, of: string) =>
  Object.defineProperty(Expect.prototype, name, {
    value: (Expect.prototype as any)[of],
    writable: true,
    configurable: true,
    enumerable: false,
  });

// ── equality ────────────────────────────────────────────────────────────────
define("toBe", EXPECTED, (m, expected) => {
  const received = m.received;
  if (m.passes(Object.is(received, expected))) return;
  if (m.not) m.failf("Expected: not <green>{}<r>\n", m.show(expected));
  if (deepEquals(received, expected) || deepEquals(received, expected, true)) {
    if (labelOf(m.expect) === undefined) m.failf("Expected: <green>{}<r>\nReceived: serializes to the same string\n", m.show(expected));
    m.failf(
      '<d>If this test should pass, replace "toBe" with "toEqual" or "toStrictEqual"<r>\n\nExpected: <green>{}<r>\nReceived: serializes to the same string\n',
      m.show(expected),
    );
  }
  if (typeof expected === "string" && typeof received === "string") m.failDiff(expected, received);
  m.failExpected(expected, received);
});
define("toEqual", EXPECTED, (m, expected) => {
  if (!m.passes(deepEquals(m.received, expected))) m.failDiff(expected, m.received);
});
define("toStrictEqual", EXPECTED, (m, expected) => {
  if (!m.passes(deepEquals(m.received, expected, true))) m.failDiff(expected, m.received);
});
define("toMatchObject", EXPECTED, (m, expected) => {
  const received = m.received as any;
  const isObj = (v: unknown) => (typeof v === "object" && v !== null) || typeof v === "function";
  if (!isObj(received)) m.failf("<b>Matcher error<r>: <red>received<r> value must be a non-null object\n");
  if (!isObj(expected)) m.withParams(NO_PARAMS).failf("<b>Matcher error<r>: <green>expected<r> value must be a non-null object\n");
  const matched = deepMatch(received, expected);
  if (!m.passes(matched)) m.failDiff(expected, matched || m.not ? received : withMatched(received, expected));
});

/** What toMatchObject's diff shows of the received object: the properties the pattern names. */
function withMatched(received: any, expected: any): unknown {
  if (Array.isArray(received) || typeof received !== "object" || received === null) return received;
  if (typeof expected !== "object" || expected === null || Array.isArray(expected)) return received;
  const out: any = Object.create(Object.getPrototypeOf(received));
  for (const key of Object.keys(received)) {
    const value = received[key];
    out[key] =
      key in expected && typeof value === "object" && value !== null && !builtinMatcher(value) && typeof expected[key] === "object" && expected[key] !== null && !builtinMatcher(expected[key])
        ? withMatched(value, expected[key])
        : key in expected && builtinMatcher(expected[key])?.(value)
          ? expected[key]
          : value;
  }
  return out;
}

define(
  "toHaveProperty",
  "<green>path<r><d>, <r><green>value<r>",
  (m, path, ...value) => {
    const received = m.received as any;
    if (typeof path !== "string" && !(path != null && typeof path[Symbol.iterator] === "function"))
      throw thrown("Expected path must be a string or an array");
    const parts: unknown[] = typeof path === "string" ? propertyPath(path) : [...(path as Iterable<unknown>)];
    let found: { value: unknown } | undefined;
    if (received !== undefined && received !== null) {
      if (typeof path === "string" && m.expect[FLAGS] && runtime.sequence?.vitest && Object.prototype.hasOwnProperty.call(received, path))
        found = { value: received[path] };
      if (!found) {
        let current: any = received;
        let ok = true;
        for (const part of parts) {
          if (current === undefined || current === null) {
            ok = false;
            break;
          }
          const key = typeof part === "symbol" ? part : String(part);
          if (!(key in Object(current))) {
            ok = false;
            break;
          }
          current = current[key as any];
        }
        if (ok) found = { value: current };
      }
    }
    const hasExpected = value.length > 0;
    const expected = value[0];
    const matched = found ? (hasExpected ? deepEquals(found.value, expected) : true) : false;
    if (m.passes(matched)) return;
    const pathOnly = m.withParams("<green>path<r>");
    if (!found && !hasExpected) pathOnly.failf("Expected path: <green>{}<r>\n\nUnable to find property\n", m.show(path));
    if (found && !hasExpected)
      pathOnly.failf("Expected path: not <green>{}<r>\n\nReceived value: <red>{}<r>\n", m.show(path), m.show(found.value));
    if (!found)
      m.failf("Expected path: <green>{}<r>\n\nExpected value: <green>{}<r>\n\nUnable to find property\n", m.show(path), m.show(expected));
    if (m.not) m.failf("Expected path: <green>{}<r>\n\nExpected value: not <green>{}<r>\n", m.show(path), m.show(expected));
    m.failDiff(expected, found!.value);
  },
  { length: 2 },
);

/** `a.b[0].c` as Jest splits it. */
function propertyPath(path: string): string[] {
  const parts: string[] = [];
  let current = "";
  for (let i = 0; i < path.length; i++) {
    const c = path[i];
    if (c === ".") {
      append(parts, current);
      current = "";
    } else if (c === "[") {
      if (current) append(parts, current);
      current = "";
      const end = path.indexOf("]", i);
      if (end < 0) {
        current = path.slice(i);
        break;
      }
      append(parts, path.slice(i + 1, end));
      i = end;
      if (path[i + 1] === ".") i++;
    } else current += c;
  }
  if (current || !parts.length || path.endsWith(".")) append(parts, current);
  return path === "" ? [""] : parts;
}

// ── numbers ─────────────────────────────────────────────────────────────────
function ordered(name: string, op: string, holds: (o: number) => boolean) {
  define(
    name,
    EXPECTED,
    (m, expected) => {
      countExpect();
      const received = m.received as any;
      let order: number | undefined;
      const num = (v: unknown) => typeof v === "number";
      const big = (v: unknown) => typeof v === "bigint";
      if ((num(received) || big(received)) && (num(expected) || big(expected))) {
        if (num(received) && num(expected)) order = received < expected ? -1 : received > expected ? 1 : received === expected ? 0 : undefined;
        else if (num(received) && Number.isNaN(received)) order = undefined;
        else if (num(expected) && Number.isNaN(expected)) order = undefined;
        else order = received < expected ? -1 : received > expected ? 1 : 0;
      } else throw thrown("Expected and actual values must be numbers or bigints");
      if (m.passes(order !== undefined && holds(order))) return;
      m.failf("Expected: {}{} <green>{}<r>\nReceived: <red>{}<r>\n", m.notWord, op, m.show(expected), m.show(received));
    },
    { countLate: true },
  );
}
ordered("toBeGreaterThan", ">", o => o > 0);
ordered("toBeGreaterThanOrEqual", ">=", o => o >= 0);
ordered("toBeLessThan", "<", o => o < 0);
ordered("toBeLessThanOrEqual", "<=", o => o <= 0);

const invalidArgumentType = (name: string, field: string, type: string) => {
  const error = new TypeError(`Expected ${field} to be a ${type} for '${name}'.`);
  (error as any).code = "ERR_INVALID_ARG_TYPE";
  return error;
};

/** An f64 as Rust's `{}` prints it, as numbers.rs does: `inf`, `NaN`, `-0`, and no exponent. */
function rustF64(n: number): string {
  if (Number.isNaN(n)) return "NaN";
  if (n === Infinity) return "inf";
  if (n === -Infinity) return "-inf";
  if (Object.is(n, -0)) return "-0";
  const text = String(n);
  const e = /^(-?)(\d)(?:\.(\d+))?e([+-]\d+)$/.exec(text);
  if (!e) return text;
  const [, sign, first, rest = "", exponent] = e;
  const digits = first + rest;
  const power = Number(exponent);
  return sign + (power < 0 ? `0.${"0".repeat(-power - 1)}${digits}` : digits.padEnd(power + 1, "0"));
}

define(
  "toBeCloseTo",
  "<green>expected<r>, precision",
  (m, expected, precision) => {
    countExpect();
    if (typeof expected !== "number") throw invalidArgumentType("toBeCloseTo", "expected", "number");
    if (precision !== undefined && typeof precision !== "number") throw invalidArgumentType("toBeCloseTo", "precision", "number");
    const p = precision === undefined ? 2 : precision;
    const received = m.received;
    if (typeof received !== "number") throw invalidArgumentType("expect", "received", "number");
    const sameInfinity = !Number.isFinite(received) && !Number.isNaN(received) && received === expected;
    const expectedDifference = 10 ** -p / 2;
    const difference = sameInfinity ? 0 : Math.abs(received - expected);
    if (m.passes(sameInfinity || difference < expectedDifference)) return;
    m.failf(
      "Expected: <green>{}<r>\nReceived: <red>{}<r>\n\nExpected precision: {}\nExpected difference: < <green>{}<r>\nReceived difference: <red>{}<r>\n",
      m.show(expected),
      m.show(received),
      rustF64(p),
      rustF64(expectedDifference),
      rustF64(difference),
    );
  },
  { countLate: true, length: 1 },
);
define("toBeWithin", "<green>start<r><d>, <r><green>end<r>", (m, start, end) => {
  if (typeof start !== "number") throw thrown("toBeWithin() requires the first argument to be a number");
  if (typeof end !== "number") throw thrown("toBeWithin() requires the second argument to be a number");
  const r = m.received;
  if (m.passes(typeof r === "number" && r >= start && r < end)) return;
  m.failf(
    "Expected: {}between <green>{}<r> <d>(inclusive)<r> and <green>{}<r> <d>(exclusive)<r>\nReceived: <red>{}<r>\n",
    m.notWord,
    m.show(start),
    m.show(end),
    m.show(r),
  );
});

// ── predicates ──────────────────────────────────────────────────────────────
const isOdd = (v: unknown): boolean | undefined => {
  if (typeof v === "bigint") return (v & 1n) === 1n || (v & 1n) === -1n;
  if (typeof v !== "number") return undefined;
  const r = Math.abs(v % 2);
  return r === 0 || r === 1 ? r === 1 : undefined;
};
const predicates: [string, (v: any) => boolean][] = [
  ["toBeArray", v => Array.isArray(v)],
  ["toBeBoolean", v => typeof v === "boolean" || (typeof v === "object" && v !== null && isBoxed(v, Boolean))],
  ["toBeDate", v => isDate(v)],
  ["toBeDefined", v => v !== undefined],
  ["toBeEven", v => isOdd(v) === false],
  ["toBeFalse", v => v === false],
  ["toBeFalsy", v => !v],
  ["toBeFinite", v => typeof v === "number" && Number.isFinite(v)],
  ["toBeFunction", v => typeof v === "function"],
  ["toBeInteger", v => typeof v === "number" && Number.isInteger(v)],
  ["toBeNaN", v => typeof v === "number" && Number.isNaN(v)],
  ["toBeNegative", v => typeof v === "number" && Number.isFinite(v) && Math.round(v) < 0],
  ["toBeNil", v => v === undefined || v === null],
  ["toBeNull", v => v === null],
  ["toBeNumber", v => typeof v === "number"],
  ["toBeOdd", v => isOdd(v) === true],
  ["toBePositive", v => typeof v === "number" && Number.isFinite(v) && Math.round(v) > 0],
  ["toBeString", v => typeof v === "string" || (typeof v === "object" && v !== null && isBoxed(v, String))],
  ["toBeSymbol", v => typeof v === "symbol"],
  ["toBeTrue", v => v === true],
  ["toBeTruthy", v => !!v],
  ["toBeUndefined", v => v === undefined],
];
function isBoxed(v: object, C: any): boolean {
  try {
    C.prototype.valueOf.call(v);
    return true;
  } catch {
    return false;
  }
}
for (const [name, test] of predicates)
  define(name, NO_PARAMS, m => {
    if (!m.passes(test(m.received))) m.failReceived(m.received);
  });

define(
  "toBeObject",
  NO_PARAMS,
  m => {
    const r = m.received;
    if (m.passes((typeof r === "object" && r !== null) || typeof r === "function")) return;
    if (m.not) m.failf("Expected value <b>not<r> to be an object\n\nReceived: <red>{}<r>\n", m.show(r));
    m.failf("Expected value to be an object\n\nReceived: <red>{}<r>\n", m.show(r));
  },
  { chainable: true },
);
define(
  "toBeEmptyObject",
  NO_PARAMS,
  m => {
    // `JSValue::is_object_empty`: as jest-get-type, a Map, Set, RegExp or Date is not an object.
    const r = m.received as any;
    const empty =
      ((typeof r === "object" && r !== null) || typeof r === "function") &&
      !isMap(r) &&
      !isSet(r) &&
      !isRegExp(r) &&
      !isDate(r) &&
      Object.keys(r).length === 0;
    if (!m.passes(empty)) m.failReceived(r);
  },
  { chainable: true },
);
define(
  "toBeValidDate",
  NO_PARAMS,
  m => {
    const r = m.received;
    if (!m.passes(isDate(r) && !Number.isNaN(Date.prototype.getTime.call(r)))) m.failReceived(r);
  },
  { chainable: true },
);
const TYPES = ["function", "object", "bigint", "boolean", "number", "string", "symbol", "undefined"];
define("toBeTypeOf", NO_PARAMS, (m, expected) => {
  if (typeof expected !== "string") throw new TypeError("toBeTypeOf() requires a string argument");
  if (!TYPES.includes(expected))
    throw new TypeError(
      "toBeTypeOf() requires a valid type string argument ('function', 'object', 'bigint', 'boolean', 'number', 'string', 'symbol', 'undefined')",
    );
  const r = m.received;
  const t = typeof r === "function" ? "function" : r === null ? "object" : typeof r;
  if (m.passes(expected === t)) return;
  m.failf("Expected type: {}<green>{}<r>\nReceived type: <red>\"{}\"<r>\nReceived value: <red>{}<r>\n", m.notWord, m.show(expected), t, m.show(r));
});
const isConstructor = (f: unknown) => {
  if (typeof f !== "function") return false;
  try {
    Reflect.construct(String, [], f as any);
    return true;
  } catch {
    return false;
  }
};
define(
  "toBeInstanceOf",
  EXPECTED,
  (m, expected) => {
    countExpect();
    if (!isConstructor(expected)) throw thrown(`Expected value must be a function: ${m.show(expected)}`);
    const r = m.received;
    if (m.passes(r instanceof (expected as Fn))) return;
    m.failf("Expected constructor: {}<green>{}<r>\nReceived value: <red>{}<r>\n", m.notWord, m.show(expected), m.show(r));
  },
  { countLate: true },
);
define(
  "toSatisfy",
  EXPECTED,
  (m, predicate) => {
    countExpect();
    if (typeof predicate !== "function") throw thrown("toSatisfy() argument must be a function");
    const matched = predicate(m.received) === true;
    if (m.passes(matched)) return;
    if (m.not) m.failf("Expected: not <green>{}<r>\n", m.show(predicate));
    m.failExpected(predicate, m.received);
  },
  { countLate: true },
);
function withMessage(name: string, message: unknown, fallback: string) {
  if (message === undefined) return fallback;
  if (typeof message === "string") return message;
  throw invalidArgumentType(name, "message", "string");
}
define(
  "pass",
  NO_PARAMS,
  (m, message) => {
    const text = withMessage("pass", message, "passes by .pass() assertion");
    countExpect();
    if (!m.passes(true)) m.fail(`${text}\n`);
  },
  { countLate: true },
);
define(
  "fail",
  NO_PARAMS,
  (m, message) => {
    const text = withMessage("fail", message, "fails by .fail() assertion");
    countExpect();
    if (!m.passes(false)) m.signed(true).fail(`${text}\n`);
  },
  { countLate: true },
);

// ── text ────────────────────────────────────────────────────────────────────
function substring(name: string, verb: string, has: (text: string, sub: string) => boolean) {
  define(
    name,
    EXPECTED,
    (m, expected) => {
      if (typeof expected !== "string") throw thrown(`${name}() requires the first argument to be a string`);
      countExpect();
      const r = m.received;
      const matched = typeof r === "string" && (expected === "" || has(r, expected));
      if (!m.passes(matched)) m.failExpectedTo(verb, expected, r);
    },
    { countLate: true },
  );
}
substring("toStartWith", "start with", (t, s) => t.startsWith(s));
substring("toEndWith", "end with", (t, s) => t.endsWith(s));
substring("toInclude", "include", (t, s) => t.includes(s));
define(
  "toIncludeRepeated",
  EXPECTED,
  (m, sub, count) => {
    countExpect();
    if (typeof sub !== "string") throw thrown("toIncludeRepeated() requires the first argument to be a string");
    if (typeof count !== "number" || !Number.isInteger(count)) throw thrown("toIncludeRepeated() requires the second argument to be a number");
    const r = m.received;
    if (typeof r !== "string") throw thrown("toIncludeRepeated() requires the expect(value) to be a string");
    if (!sub) throw thrown("toIncludeRepeated() requires the first argument to be a non-empty string");
    const times = count >>> 0;
    let found = 0;
    for (let i = r.indexOf(sub); i >= 0; i = r.indexOf(sub, i + sub.length)) found++;
    if (m.passes(found === times)) return;
    const verb = !m.not && times === 0 ? "to not include" : m.not && times >= 1 ? "not to include" : "to include";
    if (times > 1) m.failf("Expected {}: <green>{}<r> <green>{}<r> times \nReceived: <red>{}<r>\n", verb, m.show(sub), m.show(count), m.show(r));
    m.failf("Expected {}: <green>{}<r>{}\nReceived: <red>{}<r>\n", verb, m.show(sub), m.not ? " " : "", m.show(r));
  },
  { countLate: true },
);
define("toMatch", EXPECTED, (m, expected) => {
  const r = m.received;
  if (typeof expected !== "string" && !isRegExp(expected)) throw thrown(`Expected value must be a string or regular expression: ${m.show(expected)}`);
  if (typeof r !== "string") throw thrown(`Received value must be a string: ${m.show(r)}`);
  let matched: boolean;
  if (typeof expected === "string") matched = r.includes(expected);
  else {
    (expected as RegExp).lastIndex = 0;
    matched = (expected as RegExp).test(r);
  }
  if (m.passes(matched)) return;
  m.failf("Expected substring or pattern: {}<green>{}<r>\nReceived: <red>{}<r>\n", m.notWord, m.show(expected), m.show(r));
});
define("toEqualIgnoringWhitespace", EXPECTED, (m, expected) => {
  if (typeof expected !== "string") throw thrown("toEqualIgnoringWhitespace() requires argument to be a string");
  const r = m.received;
  const strip = (s: string) => s.replace(/[ \t\n\v\f\r]/g, "");
  if (!m.passes(typeof r === "string" && strip(r) === strip(expected))) m.failExpected(expected, r);
});

// ── collections ─────────────────────────────────────────────────────────────
const isArrayLike = (v: unknown): v is ArrayLike<unknown> => Array.isArray(v) || (ArrayBuffer.isView(v) && !(v instanceof DataView));
const isIterable = (v: unknown) => v !== null && v !== undefined && typeof (v as any)[Symbol.iterator] === "function";
const notAList = () => thrown("Received value must be an array type, or both received and expected values must be strings.");
function lengthOf(v: any): number {
  if (v === null || v === undefined) return Infinity;
  if (!("length" in Object(v))) return Infinity;
  const length = v.length;
  return typeof length === "number" ? length : Number(length);
}
define("toHaveLength", EXPECTED, (m, expected) => {
  const r = m.received as any;
  const noLength = () => thrown(`Received value does not have a length property: ${m.show(r)}`);
  if (!((typeof r === "object" && r !== null) || typeof r === "function" || typeof r === "string")) throw noLength();
  const n = nonNegativeInteger(expected, Number.MAX_VALUE);
  const length = lengthOf(r);
  if (length === Infinity) throw noLength();
  if (Number.isNaN(length)) throw thrown("Received value has non-number length property: NaN");
  if (m.passes(length === n)) return;
  if (m.not) m.failf("Expected length: not <green>{}<r>\n", n);
  m.failf("Expected length: <green>{}<r>\nReceived length: <red>{}<r>\n", n, length);
});
function nonNegativeInteger(value: unknown, max: number): number {
  if (typeof value === "number" && Math.round(value) === value && value >= 0 && value <= max) return value;
  throw thrown(`Expected value must be a non-negative integer: ${show(value, { colors: colors() })}`);
}
define(
  "toBeArrayOfSize",
  NO_PARAMS,
  (m, size) => {
    if (typeof size !== "number" || !Number.isInteger(size)) throw thrown("toBeArrayOfSize() requires the first argument to be a number");
    countExpect();
    const r = m.received;
    if (!m.passes(Array.isArray(r) && r.length === size)) m.failReceived(r);
  },
  { countLate: true },
);
define("toBeEmpty", NO_PARAMS, m => {
  const r = m.received as any;
  const length = typeof r === "string" ? r.length : lengthOf(r);
  let empty: boolean;
  if (Number.isNaN(length)) throw thrown("Received value has non-number length property: NaN");
  else if (length !== Infinity) empty = length === 0;
  else if (!((typeof r === "object" && r !== null) || typeof r === "function"))
    m.signed(false).failf("Expected value to be a string, object, or iterable\n\nReceived: <red>{}<r>\n", m.show(r));
  else if (isIterable(r)) {
    empty = true;
    for (const _ of r) empty = false;
  } else {
    empty = true;
    for (const _ in r) {
      empty = false;
      break;
    }
    if (empty && Object.getOwnPropertySymbols(r).some(s => Object.prototype.propertyIsEnumerable.call(r, s))) empty = false;
  }
  if (m.passes(empty!)) return;
  if (m.not) m.failf("Expected value <b>not<r> to be a string, object, or iterable\n\nReceived: <red>{}<r>\n", m.show(r));
  m.failf("Expected value to be empty\n\nReceived: <red>{}<r>\n", m.show(r));
});
define("toBeOneOf", EXPECTED, (m, expected) => {
  const r = m.received;
  let matched: boolean;
  if (isArrayLike(expected)) matched = Array.prototype.some.call(expected, x => deepEquals(x, r));
  else if (isIterable(expected)) {
    matched = false;
    for (const x of expected as Iterable<unknown>) matched = deepEquals(x, r) || matched;
  } else throw notAList();
  if (!m.passes(matched)) m.failExpectedTo("be one of", expected, r);
});
define("toContain", EXPECTED, (m, expected) => {
  const r = m.received as any;
  let matched = false;
  if (isArrayLike(r)) matched = Array.prototype.some.call(r, x => x === expected);
  else if (typeof r === "string" && (typeof expected === "string" || runtime.sequence?.vitest)) {
    const sub = String(expected);
    matched = sub === "" || r.includes(sub);
  } else if (isIterable(r)) for (const x of r) matched = x === expected || matched;
  else throw notAList();
  if (!m.passes(matched)) m.failExpectedTo("contain", expected, r);
});
define(
  "toContainEqual",
  EXPECTED,
  (m, expected) => {
    const r = m.received as any;
    let matched = false;
    if (isArrayLike(r)) matched = Array.prototype.some.call(r, x => deepEquals(x, expected));
    else if (typeof r === "string" && typeof expected === "object" && expected !== null && isBoxed(expected, String)) matched = false;
    else if (typeof r === "string" && typeof expected === "string") matched = [...expected].length === 1 && r.includes(expected);
    else if (isIterable(r)) matched = [...r].some(x => deepEquals(x, expected));
    else throw notAList();
    if (m.passes(matched)) return;
    if (m.not) m.failf("Expected to not contain: <green>{}<r>\n", m.show(expected));
    m.failExpectedTo("contain", expected, r);
  },
  { chainable: true },
);
function arrayArgument(name: string, expected: unknown): unknown[] {
  if (Array.isArray(expected)) return expected;
  throw invalidArgumentType(name, "expected", "array");
}
const isObject = (v: unknown): v is object => (typeof v === "object" && v !== null) || typeof v === "function";
const hasOwn = (o: object, key: unknown) => Object.prototype.hasOwnProperty.call(o, key as PropertyKey);
function contained(m: M, matched: boolean, verb: string, expected: unknown, received: unknown) {
  if (!m.passes(matched)) m.failExpectedTo(verb, expected, received);
}
const chain = { chainable: true, countLate: true } as const;
define(
  "toContainKey",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const r = m.received;
    if (!isObject(r)) throw new TypeError(`Expected value must be an object\nReceived: ${m.show(r)}`);
    contained(m, hasOwn(r, expected), "contain", expected, r);
  },
  chain,
);
define(
  "toContainKeys",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const r = m.received;
    const keys = arrayArgument("toContainKeys", expected);
    const matched = isObject(r) ? keys.every(k => hasOwn(r, k)) : keys.length === 0;
    contained(m, matched, "contain", expected, r);
  },
  chain,
);
define(
  "toContainAnyKeys",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const r = m.received;
    const keys = arrayArgument("toContainAnyKeys", expected);
    contained(m, isObject(r) && keys.some(k => hasOwn(r, k)), "contain", expected, r);
  },
  chain,
);
define(
  "toContainAllKeys",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const r = m.received as any;
    const keys = arrayArgument("toContainAllKeys", expected);
    const own = Object.keys(Object(r));
    const matched = own.length === keys.length && own.every(k => keys.some(e => deepEquals(k, e)));
    contained(m, matched, "contain all keys", expected, own);
  },
  chain,
);
const valuesOf = (v: unknown) => (v === undefined || v === null ? undefined : Object.values(v as object));
define(
  "toContainValue",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const r = m.received;
    const values = valuesOf(r);
    contained(m, !!values && values.some(v => deepEquals(v, expected)), "contain", expected, r);
  },
  chain,
);
define(
  "toContainValues",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const list = arrayArgument("toContainValues", expected);
    const r = m.received;
    const values = valuesOf(r);
    contained(m, values ? list.every(e => values.some(v => deepEquals(v, e))) : true, "contain", expected, r);
  },
  chain,
);
define(
  "toContainAnyValues",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const list = arrayArgument("toContainAnyValues", expected);
    const r = m.received;
    const values = valuesOf(r);
    contained(m, values ? list.some(e => values.some(v => deepEquals(v, e))) : false, "contain any of the following values", expected, r);
  },
  chain,
);
define(
  "toContainAllValues",
  EXPECTED,
  (m, expected) => {
    countExpect();
    const list = arrayArgument("toContainAllValues", expected);
    const r = m.received;
    const values = valuesOf(r);
    const matched = !!values && values.length !== 0 && values.length === list.length && list.every(e => values.some(v => deepEquals(v, e)));
    contained(m, matched, "contain all values", expected, r);
  },
  chain,
);

// ── toThrow ─────────────────────────────────────────────────────────────────
define("toThrow", EXPECTED, (m, expectedArg) => {
  let expected: unknown;
  if (isObject(expectedArg)) expected = expectedArg instanceof ExpectAny ? expectedArg.constructorValue : expectedArg;
  else if (typeof expectedArg === "string") expected = expectedArg.length ? expectedArg : undefined;
  else if (expectedArg === undefined) expected = undefined;
  else throw thrown(`Expected value must be string or Error: ${m.show(expectedArg)}`);
  const received = m.received;
  const resolvedWithoutThrowing = m.flags.promise === RESOLVES && typeof received !== "function" && (!isError(received) || runtime.sequence?.vitest);
  let outcome: { threw: boolean; value: unknown };
  if (resolvedWithoutThrowing) outcome = { threw: false, value: received };
  else if (typeof received !== "function") {
    if (m.flags.promise !== NONE) outcome = { threw: true, value: received };
    else throw thrown("Expected value must be a function");
  } else {
    try {
      const value = once(() => (received as Fn)());
      outcome = { threw: false, value };
      if (isPromise(value)) {
        const state = native.promiseState(value);
        if (state === 2) {
          value.catch(() => {});
          outcome = { threw: true, value: native.promiseResult(value) };
        } else if (state === 0) waitFor(value);
      }
    } catch (error) {
      if (error instanceof Deferred) throw error;
      outcome = { threw: true, value: error };
    }
  }
  if (!outcome.threw) {
    if (m.not) return;
    failDidNotThrow(m, expected, outcome.value);
  }
  if (expected === undefined) {
    if (m.not) failThrew(m.withParams(NO_PARAMS), outcome.value);
    return;
  }
  compareThrown(m, expected, outcome.value);
});
alias("toThrowError", "toThrow");

const messageOf = (v: unknown): { value: unknown } | undefined => {
  if (isObject(v)) return "message" in v ? { value: (v as any).message } : undefined;
  return { value: String(v) };
};
type Text = "substring" | "pattern" | "message";
function expectedOf(v: any): [Text, unknown] | ["class", unknown] {
  if (typeof v === "string") return ["substring", v];
  if (isRegExp(v)) return ["pattern", v];
  if (isObject(v) && "message" in v) return ["message", (v as any).message];
  return ["class", v];
}
function compareThrown(m: M, expected: any, thrownValue: unknown) {
  const readFirst = !m.not ? messageOf(thrownValue) : undefined;
  const message = () => readFirst ?? messageOf(thrownValue);
  if (!m.not && isObject(expected) && isAsymmetricMatcher(expected)) {
    if (deepEquals(thrownValue, expected, true)) return;
    m.failf("Expected value: <green>{}<r>\nReceived value: <red>{}<r>\n", m.show(expected), m.show(thrownValue));
  }
  const [kind, value] = expectedOf(expected);
  if (kind !== "class") {
    let found = message();
    if (!found && m.not) found = { value: undefined };
    if (!found) m.failf("Expected {}: <green>{}<r>\nReceived value: <red>{}<r>{}", kind, m.show(value), m.show(thrownValue), kind === "message" ? "\n" : "");
    const msg = found!.value;
    let matched: boolean | undefined;
    if (kind === "substring") matched = String(msg).includes(String(value));
    else if (kind === "pattern") matched = typeof (value as any).test === "function" ? !!(value as any).test(msg) : undefined;
    else matched = Object.is(msg, value);
    if (matched !== undefined && m.passes(matched)) return;
    if (m.not && kind === "message") m.failf("Expected message: not <green>{}<r>\n", m.show(value));
    m.failf("Expected {}: {}<green>{}<r>\nReceived message: <red>{}<r>\n", kind, m.notWord, m.show(value), m.show(msg));
  }
  let instance: boolean;
  try {
    instance = thrownValue instanceof (value as any);
  } catch (e) {
    throw e;
  }
  if (m.passes(instance)) return;
  const name = className(value);
  const received = (msg: { value: unknown } | undefined) => (msg ? ["message", msg.value] : ["value", thrownValue]);
  if (m.not) {
    const [noun, r] = received(isObject(thrownValue) ? message() : undefined);
    m.failf("Expected constructor: not <green>{}<r>\n\nReceived {}: <red>{}<r>\n", name, noun, m.show(r));
  }
  const thrownName = isObject(thrownValue) ? className(thrownValue) : "[not a class]";
  const [noun, r] = received(message());
  m.failf("Expected constructor: <green>{}<r>\nReceived constructor: <red>{}<r>\n\nReceived {}: <red>{}<r>\n", name, thrownName, noun, m.show(r));
}
function failThrew(m: M, thrownValue: unknown): never {
  if (!isError(thrownValue)) m.failf("Thrown value: <red>{}<r>\n", m.show(thrownValue));
  const e = thrownValue as any;
  m.failf("Error name: <red>{}<r>\nError message: <red>{}<r>\n", m.show(e.name || undefined), m.show(e.message || undefined));
}
function failDidNotThrow(m: M, expected: unknown, returned: unknown): never {
  if (expected === undefined) m.withParams(NO_PARAMS).failf("Received function did not throw\nReceived value: <red>{}<r>\n", m.show(returned));
  const [kind, value] = expectedOf(expected);
  const shown = kind === "class" ? className(value) : m.show(value);
  m.failf("Expected {}: <green>{}<r>\n\nReceived function did not throw\nReceived value: <red>{}<r>\n", kind === "class" ? "constructor" : kind, shown, m.show(returned));
}

// ── mock functions ──────────────────────────────────────────────────────────
let isMock: (v: unknown) => boolean = () => false;
export function setIsMock(test: (v: unknown) => boolean) {
  isMock = test;
}
const notAMock = (m: M, withText: string, v: unknown) => thrown(`Expected value must be a mock function${withText}: ${m.show(v)}`);
function callsOf(m: M, mock: unknown): unknown[] {
  if (!isMock(mock)) throw notAMock(m, "", mock);
  return (mock as any).mock.calls;
}
function mockCalls(m: M, mock: unknown): unknown[] {
  if (!isMock(mock)) m.signed(false).failf("Matcher error: <red>received<r> value must be a mock function\nReceived: {}", m.show(mock));
  return (mock as any).mock.calls;
}
const ARGUMENTS = "<green>...expected<r>";
function hasArguments(call: unknown, expected: unknown[]): boolean {
  if (call === undefined) return false;
  if (!Array.isArray(call)) throw thrown("Internal error: expected mock call item to be an array of arguments.");
  if (call.length !== expected.length) return false;
  return expected.every((e, i) => deepEquals(call[i], e));
}
/** `Rows`: a numbered list, printed by one formatter. */
function rows(items: [string, unknown?][], width: number): string {
  return items
    .map(([text, value], i) => `           ${String(i + 1).padStart(width)}: ${text}${value === undefined && items[i].length < 2 ? "" : show(value, { colors: colors() })}`)
    .join("\n");
}
const listCalls = (calls: unknown[]) => (calls.length === 0 ? "(no calls)" : rows(calls.map(c => ["", c]), 4));
function failNumberOfCalls(m: M, relation: string, expected: number, count: number): never {
  m.failf("Expected number of calls: {}<green>{}<r>\nReceived number of calls: <red>{}<r>\n", relation, expected, count);
}
function noArgument(m: M, argument: unknown) {
  if (argument !== undefined) throw new TypeError(`${m.name}() must not have an argument`);
}
function timesArgument(m: M, times: unknown): number {
  if (typeof times === "number" && Number.isInteger(times) && times >= 0 && times <= 0xffffffff) return times;
  throw new TypeError(`${m.name}() requires 1 non-negative integer argument`);
}
function nthArgument(m: M, nth: unknown, notInteger: string, notPositive: string): number {
  if (typeof nth !== "number" || !Number.isInteger(nth)) throw new TypeError(`${m.name}() ${notInteger}`);
  if ((nth | 0) < 1) throw new TypeError(`${m.name}() ${notPositive}`);
  return nth | 0;
}
const failNotCalled = (m: M, expected: unknown): never => m.failf("Expected: <green>{}<r>\nBut it was not called.", m.show(expected));
const failCalledLess = (m: M, count: number, nth: number, end: string): never =>
  m.fail(`The mock function was called ${count} time${count === 1 ? "" : "s"}, but call ${nth} was requested.${end}`);

define(
  "toHaveBeenCalled",
  NO_PARAMS,
  (m, expected) => {
    const count = callsOf(m, m.received).length;
    noArgument(m, expected);
    if (m.passes(count > 0)) return;
    if (m.not) failNumberOfCalls(m, "", 0, count);
    failNumberOfCalls(m, ">= ", 1, count);
  },
  { length: 0 },
);
alias("toBeCalled", "toHaveBeenCalled");
define("toHaveBeenCalledOnce", EXPECTED, m => {
  const count = callsOf(m, m.received).length;
  if (!m.passes(count === 1)) failNumberOfCalls(m, m.notWord, 1, count);
});
define("toHaveBeenCalledTimes", EXPECTED, (m, expected) => {
  const count = callsOf(m, m.received).length;
  const times = timesArgument(m, expected);
  if (!m.passes(count === times)) failNumberOfCalls(m, m.notWord, times, count);
});
alias("toBeCalledTimes", "toHaveBeenCalledTimes");
define(
  "toHaveBeenCalledWith",
  ARGUMENTS,
  (m, ...expected) => {
    const calls = mockCalls(m, m.received);
    const matched = calls.some(call => hasArguments(call, expected));
    if (m.passes(matched)) return;
    if (m.not) m.failf("Expected mock function not to have been called with: <green>{}<r>\nBut it was.", m.show(expected));
    if (calls.length === 0) failNotCalled(m, expected);
    if (calls.length === 1) m.failDiff(expected, calls[0]);
    m.failf("    <green>Expected<r>: {}\n    <red>Received<r>:\n{}\n\n    Number of calls: {}\n", m.show(expected), listCalls(calls), calls.length);
  },
  { length: 0 },
);
alias("toBeCalledWith", "toHaveBeenCalledWith");
define(
  "toHaveBeenCalledExactlyOnceWith",
  ARGUMENTS,
  (m, ...expected) => {
    const calls = mockCalls(m, m.received);
    const call = calls.length === 1 ? calls[0] : undefined;
    if (m.passes(hasArguments(call, expected))) return;
    if (m.not) m.failf("Expected mock function not to have been called exactly once with: <green>{}<r>\nBut it was.", m.show(expected));
    if (call !== undefined) m.failDiff(expected, call);
    if (calls.length === 0) failNotCalled(m, expected);
    m.failf(
      "    <green>Expected<r>: {}\n    <red>Received<r>:\n{}\n\n    Expected number of calls: <green>1<r>\n    Received number of calls: <red>{}<r>\n",
      m.show(expected),
      listCalls(calls),
      calls.length,
    );
  },
  { length: 0 },
);
define(
  "toHaveBeenLastCalledWith",
  ARGUMENTS,
  (m, ...expected) => {
    const calls = mockCalls(m, m.received);
    const call = calls.length > 0 ? calls[calls.length - 1] : undefined;
    if (call !== undefined && !Array.isArray(call)) throw notAMock(m, " with calls", m.received);
    if (m.passes(hasArguments(call, expected))) return;
    if (m.not) m.failf("Expected last call not to be with: <green>{}<r>\nBut it was.", m.show(expected));
    if (call !== undefined) m.failDiff(expected, call);
    failNotCalled(m, expected);
  },
  { length: 0 },
);
alias("lastCalledWith", "toHaveBeenLastCalledWith");
define(
  "toHaveBeenNthCalledWith",
  "<green>n<r>, <green>...expected<r>",
  (m, nthValue, ...expected) => {
    const calls = mockCalls(m, m.received);
    const nth = nthArgument(m, nthValue, "requires a positive integer as the first argument", "first argument must be a positive integer");
    const call = nth <= calls.length ? calls[nth - 1] : undefined;
    if (m.passes(hasArguments(call, expected))) return;
    if (m.not) m.failf("Expected call #{} not to be with: <green>{}<r>\nBut it was.", nth, m.show(expected));
    if (call !== undefined) m.fail(`Call #${nth}:\n${diffOf(call, expected)}\n`);
    failCalledLess(m, calls.length, nth, "");
  },
  { length: 0 },
);
alias("nthCalledWith", "toHaveBeenNthCalledWith");
function firstInvocation(m: M, mock: unknown): number | undefined {
  const order = (mock as any)?.mock?.invocationCallOrder;
  if (!Array.isArray(order)) throw notAMock(m, " with calls", mock);
  return order.length ? Number(order[0]) : undefined;
}
function calledInOrder(m: M, other: unknown, mustBeCalled: unknown, before: boolean) {
  mockCalls(m, m.received);
  if (!isMock(other)) m.failf("Matcher error: <green>expected<r> value must be a mock function\nExpected: {}", m.show(other));
  const received = firstInvocation(m, m.received);
  const expected = firstInvocation(m, other);
  const [first, second] = before ? [received, expected] : [expected, received];
  const matched =
    first === undefined ? mustBeCalled !== undefined && !mustBeCalled : second === undefined ? false : first < second;
  if (m.passes(matched)) return;
  const s = (id: number | undefined) => (id === undefined ? "(no calls)" : `invocation ${id}`);
  m.failf(
    "Expected the first call of <red>received<r> {}to be {} the first call of <green>expected<r>\n\nFirst call of received: <red>{}<r>\nFirst call of expected: <green>{}<r>\n",
    m.notWord,
    before ? "before" : "after",
    s(received),
    s(expected),
  );
}
define("toHaveBeenCalledBefore", EXPECTED, (m, other, must) => calledInOrder(m, other, must, true), { length: 1 });
define("toHaveBeenCalledAfter", EXPECTED, (m, other, must) => calledInOrder(m, other, must, false), { length: 1 });

// results
type Outcome = { kind: "value"; value: unknown } | { kind: "error"; value: unknown } | { kind: "pending" };
function results(m: M, mock: unknown, resolved: boolean) {
  if (!isMock(mock)) throw notAMock(m, "", mock);
  const entries: any[] = resolved ? (mock as any).mock.settledResults : (mock as any).mock.results;
  if (!Array.isArray(entries)) throw notAMock(m, " with returns", mock);
  const outcome = (i: number): Outcome => {
    const entry = entries[i];
    const type = typeof entry === "object" && entry !== null && typeof entry.type === "string" ? entry.type : undefined;
    if (!resolved) {
      if (type === "return") return { kind: "value", value: entry.value };
      if (type === "throw") return { kind: "error", value: entry.value };
      return { kind: "pending" };
    }
    if (type === "fulfilled") return { kind: "value", value: entry.value };
    if (type === "rejected") return { kind: "error", value: entry.value };
    return { kind: "pending" };
  };
  const countValues = () => {
    let n = 0;
    for (let i = 0; i < entries.length; i++) {
      if (!resolved) {
        const entry = entries[i];
        const type = typeof entry === "object" && entry !== null ? entry.type : undefined;
        if (type !== "return" && type !== "throw" && type !== "incomplete") throw notAMock(m, " with returns", entry);
      }
      if (outcome(i).kind === "value") n++;
    }
    return n;
  };
  return { entries, count: entries.length, outcome, countValues, resolved };
}
function hasValues(m: M, resolved: boolean, exactly: boolean, argument: unknown) {
  const r = results(m, m.received, resolved);
  const expected = exactly ? timesArgument(m, argument) : (noArgument(m, argument), 1);
  const values = r.countValues();
  const matched = exactly ? values === expected : values >= expected;
  if (m.passes(matched)) return;
  const [noun, calls] = resolved ? ["resolved calls: ", "calls:          "] : ["succesful returns: ", "calls:             "];
  const [relation, padding] = !exactly ? (m.not ? ["< ", "  "] : [">= ", "   "]) : m.not ? ["!= ", "   "] : ["== ", "   "];
  m.failf(
    "Expected number of {}{}<green>{}<r>\nReceived number of {}{}<red>{}<r>\nReceived number of {}{}<red>{}<r>\n",
    noun,
    relation,
    expected,
    noun,
    padding,
    values,
    calls,
    padding,
    r.count,
  );
}
define("toHaveReturned", EXPECTED, (m, e) => hasValues(m, false, false, e), { length: 0 });
alias("toReturn", "toHaveReturned");
define("toHaveReturnedTimes", EXPECTED, (m, e) => hasValues(m, false, true, e));
define("toHaveResolved", NO_PARAMS, (m, e) => hasValues(m, true, false, e), { length: 0 });
define("toHaveResolvedTimes", EXPECTED, (m, e) => hasValues(m, true, true, e));
function failMismatch(m: M, label: string, expected: unknown, received: unknown): never {
  if (typeof expected === "string" && typeof received === "string") m.fail(`${label}${diffOf(received, expected)}\n`);
  m.fail(label + pretty(`Expected: <green>`) + m.show(expected) + pretty(`<r>\nReceived: <red>`) + m.show(received) + pretty("<r>"));
}
function callHasValue(m: M, r: ReturnType<typeof results>, nth: number | undefined, expected: unknown) {
  const call = nth === undefined ? "The last call" : `Call ${nth}`;
  const outcome = nth === undefined ? (r.count > 0 ? r.outcome(r.count - 1) : undefined) : nth <= r.count ? r.outcome(nth - 1) : undefined;
  const matched = outcome?.kind === "value" && deepEquals(outcome.value, expected);
  if (m.passes(matched)) return;
  if (m.not) {
    if (!r.resolved && nth === undefined) m.failf("Expected mock function not to have last returned: <green>{}<r>\nBut it did.\n", m.show(expected));
    if (!r.resolved) m.failf("Expected mock function not to have returned on call {}: <green>{}<r>\nBut it did.\n", nth, m.show(expected));
    m.failf("{} was expected not to resolve with: <green>{}<r>\nBut it did.\n", call, m.show(expected));
  }
  let received: unknown;
  if (!outcome) {
    if (nth === undefined) m.fail("The mock function was not called.");
    failCalledLess(m, r.count, nth!, "\n");
  } else if (outcome.kind === "value") received = outcome.value;
  else if (outcome.kind === "pending") {
    if (r.resolved) m.fail(`${call} has not settled.\n`);
    received = undefined;
  } else if (!r.resolved) m.failf("{} threw an error: <red>{}<r>\n", call, m.show(outcome.value));
  else m.failf("{} rejected with: <red>{}<r>\n", call, m.show(outcome.value));
  if (!r.resolved && nth === undefined) failMismatch(m, "", expected, received);
  failMismatch(m, `${call}:\n`, expected, received);
}
define("toHaveLastReturnedWith", EXPECTED, (m, e) => {
  const mm = m.withParams(EXPECTED);
  callHasValue(mm, results(mm, m.received, false), undefined, e);
});
alias("lastReturnedWith", "toHaveLastReturnedWith");
define("toHaveLastResolvedWith", EXPECTED, (m, e) => callHasValue(m, results(m, m.received, true), undefined, e));
define("toHaveNthReturnedWith", "<green>n<r>, <green>expected<r>", (m, nth, e) => {
  const r = results(m, m.received, false);
  callHasValue(m, r, nthArgument(m, nth, "first argument must be an integer", "n must be greater than 0"), e);
}, { length: 1 });
alias("nthReturnedWith", "toHaveNthReturnedWith");
define("toHaveNthResolvedWith", "<green>n<r>, <green>expected<r>", (m, nth, e) => {
  const r = results(m, m.received, true);
  callHasValue(m, r, nthArgument(m, nth, "first argument must be an integer", "n must be greater than 0"), e);
}, { length: 2 });
define("toHaveReturnedWith", EXPECTED, (m, expected) => {
  const r = results(m, m.received, false);
  let matched = false;
  let someThrew = false;
  const returned: unknown[] = [];
  for (let i = 0; i < r.count; i++) {
    const o = r.outcome(i);
    if (o.kind === "value") {
      append(returned, o.value);
      matched = matched || deepEquals(o.value, expected);
    } else if (o.kind === "error") someThrew = true;
  }
  if (m.passes(matched)) return;
  if (m.not) m.failf("Expected mock function not to have returned: <green>{}<r>\n", m.show(expected));
  if (r.count === 1 && returned.length === 1) failMismatch(m, "", expected, returned[0]);
  if (!someThrew)
    m.failf(
      "    <green>Expected<r>: {}\n    <red>Received<r>:\n{}\n\n    Number of returns: {}\n",
      m.show(expected),
      rows(returned.map(v => ["", v]), 4),
      returned.length,
    );
  const list = rows(
    r.entries.map((entry: any) =>
      entry.type === "return" ? ["", entry.value] : entry.type === "throw" ? ["function call threw an error: ", entry.value] : ["<incomplete call>"],
    ) as [string, unknown?][],
    2,
  );
  m.fail(
    `Some calls errored:\n\n    Expected: ${m.show(expected)}\n    Received:\n${list}\n\n    Number of returns: ${returned.length}\n    Number of calls:   ${r.count}\n`,
  );
});
define("toHaveResolvedWith", EXPECTED, (m, expected) => {
  const r = results(m, m.received, true);
  let matched = false;
  let resolved = 0;
  let last: unknown;
  for (let i = 0; i < r.count; i++) {
    const o = r.outcome(i);
    if (o.kind === "value") {
      resolved++;
      last = o.value;
      matched = matched || deepEquals(o.value, expected);
    }
  }
  if (m.passes(matched)) return;
  if (m.not) m.failf("Expected mock function not to have resolved with: <green>{}<r>\n", m.show(expected));
  if (r.count === 0) failNotCalled(m, expected);
  if (r.count === 1 && resolved === 1) failMismatch(m, "", expected, last);
  const list = rows(
    Array.from({ length: r.count }, (_, i) => {
      const o = r.outcome(i);
      return o.kind === "value" ? ["", o.value] : o.kind === "error" ? ["function call rejected with: ", o.value] : ["<pending call>"];
    }) as [string, unknown?][],
    4,
  );
  m.failf(
    "    <green>Expected<r>: {}\n    <red>Received<r>:\n{}\n\n    Number of resolved calls: {}\n    Number of calls:          {}\n",
    m.show(expected),
    list,
    resolved,
    r.count,
  );
});

// ── snapshots (snapshot.ts) ───────────────────────────────────────────────────
export interface SnapshotHooks {
  toMatchSnapshot(m: M, received: { thrown: boolean; value: unknown }, propertyMatchers: unknown, hint: string): void;
  toMatchInlineSnapshot(m: M, received: { thrown: boolean; value: unknown }, propertyMatchers: unknown, saved: string | undefined, args: number): void;
}
let snapshotHooks: SnapshotHooks | undefined;
export function setSnapshotHooks(hooks: SnapshotHooks) {
  snapshotHooks = hooks;
}
function receivedForSnapshot(m: M, fromThrow: boolean): { thrown: boolean; value: unknown } {
  if (!fromThrow) return { thrown: false, value: m.received };
  const received = m.received;
  if (typeof received !== "function") {
    if (m.flags.promise !== NONE) return { thrown: true, value: received };
    throw thrown("Expected value must be a function");
  }
  try {
    const value = (received as Fn)();
    return { thrown: false, value };
  } catch (error) {
    return { thrown: true, value: error };
  }
}
function snapshotArgs(m: M, a: unknown, b: unknown): [unknown, string] {
  let propertyMatchers: unknown;
  let hint = "";
  if (typeof a === "string") hint = a;
  else if (a !== undefined) {
    if (typeof a !== "object" || a === null) m.withParams(NO_PARAMS).fail("Expected properties must be an object\n");
    propertyMatchers = a;
    if (typeof b === "string") hint = b;
  }
  return [propertyMatchers, hint];
}
define(
  "toMatchSnapshot",
  "<green>propertyMatchers<r>, hint",
  (m, a, b) => {
    if (m.not) throw thrown("Snapshot matchers cannot be used with not");
    const [propertyMatchers, hint] = snapshotArgs(m, a, b);
    snapshotHooks!.toMatchSnapshot(m, receivedForSnapshot(m, false), propertyMatchers, hint);
  },
  { length: 1 },
);
define(
  "toMatchInlineSnapshot",
  "<green>propertyMatchers<r>, inlineSnapshot",
  (m, ...args) => {
    if (m.not) throw thrown("Snapshot matchers cannot be used with not");
    const [a, b] = args;
    let propertyMatchers: unknown;
    let saved: string | undefined;
    if (typeof a === "string") saved = a;
    else if (a !== undefined) {
      propertyMatchers = a;
      if (typeof b === "string") saved = b;
    }
    snapshotHooks!.toMatchInlineSnapshot(m, receivedForSnapshot(m, false), propertyMatchers, saved, args.length);
  },
  { length: 1 },
);
define("toThrowErrorMatchingSnapshot", "hint", (m, hint) => {
  if (m.not) throw thrown("Snapshot matchers cannot be used with not");
  snapshotHooks!.toMatchSnapshot(m, receivedForSnapshot(m, true), undefined, typeof hint === "string" ? hint : "");
});
define(
  "toThrowErrorMatchingInlineSnapshot",
  "inlineSnapshot",
  (m, ...args) => {
    if (m.not) throw thrown("Snapshot matchers cannot be used with not");
    snapshotHooks!.toMatchInlineSnapshot(m, receivedForSnapshot(m, true), undefined, typeof args[0] === "string" ? args[0] : undefined, args.length);
  },
  { length: 1 },
);
define("toMatchFileSnapshot", "<green>path<r>, hint", () => {
  throw thrown("toMatchFileSnapshot() is not supported yet");
}, { length: 1 });

// ── asymmetric matchers ─────────────────────────────────────────────────────
interface MatcherFlags {
  not: boolean;
  promise: PromiseFlag;
}
/** `Expect_readFlagsAndProcessPromise`: the value a `resolvesTo`/`rejectsTo` matcher compares, or nothing. */
function processPromise(flags: MatcherFlags, value: unknown): { value: unknown } | undefined {
  if (flags.promise === NONE) return { value };
  const promise = once(() => promiseToAwait(value)) as Promise<unknown> | undefined;
  if (promise === undefined) return undefined;
  const state = native.promiseState(promise);
  if (state === 0) waitFor(promise);
  if (state === (flags.promise === RESOLVES ? 1 : 2)) return { value: native.promiseResult(promise) };
  return undefined;
}

abstract class AsymmetricMatcher {
  #flags: MatcherFlags;
  constructor(flags: MatcherFlags) {
    this.#flags = flags;
  }
  get flags() {
    return this.#flags;
  }
  abstract test(value: unknown): boolean;
  /** `matchAsymmetricMatcher`, with `.not` applied. */
  [ASYMMETRIC](other: unknown): boolean {
    const processed = processPromise(this.#flags, other);
    const pass = processed ? this.test(processed.value) : false;
    return this.#flags.not && this.applyNot(processed) ? !pass : pass;
  }
  applyNot(_processed: unknown): boolean {
    return true;
  }
  asymmetricMatch(other: unknown): boolean {
    return this[ASYMMETRIC](other);
  }
}

const constructorType = (c: unknown) =>
  c === Number
    ? "Number"
    : c === Boolean
      ? "Boolean"
      : c === String
        ? "String"
        : c === Symbol
          ? "Symbol"
          : c === BigInt
            ? "BigInt"
            : c === Promise
              ? "Promise"
              : c === Array
                ? "Array"
                : c === Object
                  ? "Object"
                  : "InstanceOf";

export class ExpectAnything extends AsymmetricMatcher {
  test(v: unknown) {
    return v !== undefined && v !== null;
  }
}
export class ExpectAny extends AsymmetricMatcher {
  #expected: unknown;
  constructor(flags: MatcherFlags, ctor: unknown) {
    super(flags);
    if (ctor === undefined)
      throw thrown("any() expects to be passed a constructor function. Please pass one or use anything() to match any object.");
    if (!isConstructor(ctor)) throw thrown(pretty("<d>expect.<r>any<d>(<r>constructor<d>)<r>\n\nExpected a constructor\n"));
    this.#expected = ctor;
  }
  get constructorValue() {
    return this.#expected;
  }
  test(v: unknown) {
    const c = this.#expected as any;
    switch (constructorType(c)) {
      case "Symbol":
        if (typeof v === "symbol") return true;
        break;
      case "String":
        if (typeof v === "string" || (typeof v === "object" && v !== null && isBoxed(v, String))) return true;
        break;
      case "BigInt":
        if (typeof v === "bigint") return true;
        break;
      case "Boolean":
        if (typeof v === "boolean" || (typeof v === "object" && v !== null && isBoxed(v, Boolean))) return true;
        break;
      case "Number":
        if (typeof v === "number" || (typeof v === "object" && v !== null && isBoxed(v, Number))) return true;
        break;
      case "Promise":
        if (isPromise(v)) return true;
        break;
      case "Array":
        if (Array.isArray(v)) return true;
        break;
      case "Object":
        return v === null || (typeof v === "object" && v !== null);
    }
    return v instanceof c;
  }
}
export class ExpectArrayContaining extends AsymmetricMatcher {
  #array: unknown[];
  constructor(flags: MatcherFlags, array: unknown) {
    super(flags);
    if (!Array.isArray(array)) throw thrown(pretty("<d>expect.<r>arrayContaining<d>(<r>array<d>)<r>\n\nExpected a array\n"));
    this.#array = array;
  }
  get arrayValue() {
    return this.#array;
  }
  test(v: unknown) {
    if (!Array.isArray(v)) return false;
    if (this.#array.length === 0) return true;
    return this.#array.every(e => v.some(o => deepEquals(e, o)));
  }
}
export class ExpectObjectContaining extends AsymmetricMatcher {
  #object: object;
  constructor(flags: MatcherFlags, object: unknown) {
    super(flags);
    if (!isObject(object)) throw thrown(pretty("<d>expect.<r>objectContaining<d>(<r>object<d>)<r>\n\nExpected an object\n"));
    this.#object = object;
  }
  get objectValue() {
    return this.#object;
  }
  test(v: unknown) {
    return isObject(v) && deepMatch(v, this.#object, true);
  }
}
export class ExpectStringContaining extends AsymmetricMatcher {
  #string: string;
  constructor(flags: MatcherFlags, s: unknown) {
    super(flags);
    if (typeof s !== "string") throw thrown(pretty("<d>expect.<r>stringContaining<d>(<r>string<d>)<r>\n\nExpected a string\n"));
    this.#string = s;
  }
  get stringValue() {
    return this.#string;
  }
  test(v: unknown) {
    return typeof v === "string" && v.includes(this.#string);
  }
}
export class ExpectStringMatching extends AsymmetricMatcher {
  #test: string | RegExp;
  #pattern: RegExp;
  constructor(flags: MatcherFlags, t: unknown) {
    super(flags);
    if (typeof t !== "string" && !isRegExp(t))
      throw thrown(pretty("<d>expect.<r>stringMatching<d>(<r>string<d>)<r>\n\nExpected a string or regular expression\n"));
    this.#test = t;
    this.#pattern = typeof t === "string" ? new RegExp(t) : t;
  }
  get testValue() {
    return this.#test;
  }
  test(v: unknown) {
    if (typeof v !== "string") return false;
    this.#pattern.lastIndex = 0;
    return this.#pattern.test(v);
  }
}
export class ExpectCloseTo extends AsymmetricMatcher {
  #number: number;
  #digits: number;
  constructor(flags: MatcherFlags, n: unknown, precision: unknown) {
    super(flags);
    if (typeof n !== "number") throw thrown(pretty("<d>expect.<r>closeTo<d>(<r>number<d>, precision?)<r>\n\nExpected a number value"));
    if (precision === undefined) precision = 2;
    if (typeof precision !== "number")
      throw thrown(pretty("<d>expect.<r>closeTo<d>(number, <r>precision?<d>)<r>\n\nPrecision must be a number or undefined"));
    this.#number = n;
    this.#digits = precision;
  }
  get numberValue() {
    return this.#number;
  }
  get digitsValue() {
    return this.#digits;
  }
  [ASYMMETRIC](other: unknown): boolean {
    const processed = processPromise(this.flags, other);
    if (!processed) return this.flags.not;
    if (typeof processed.value !== "number") return false;
    const pass = this.test(processed.value);
    return this.flags.not ? !pass : pass;
  }
  test(received: number) {
    const expected = this.#number;
    if ((received === Infinity && expected === Infinity) || (received === -Infinity && expected === -Infinity)) return true;
    return Math.abs(expected - received) < 0.5 * 10 ** -(this.#digits | 0);
  }
}

/** What `expect.extend()` gives `expect.myMatcher(…)`: its arguments, the matcher run later. */
export class ExpectCustomAsymmetricMatcher extends AsymmetricMatcher {
  #fn: Fn;
  #args: unknown[];
  constructor(flags: MatcherFlags, args: unknown[], fn: Fn) {
    super(flags);
    this.#fn = fn;
    this.#args = args;
  }
  get matcherFn() {
    return this.#fn;
  }
  get capturedArgs() {
    return this.#args;
  }
  [ASYMMETRIC](other: unknown): boolean {
    const processed = processPromise(this.flags, other);
    if (!processed) return false;
    return executeCustomMatcher(undefined, displayName(this.#fn), this.#fn, [processed.value, ...this.#args], this.flags, true);
  }
  test(): boolean {
    return false;
  }
  toAsymmetricMatcher(): string | undefined {
    const custom = (this.#fn as any).toAsymmetricMatcher;
    if (typeof custom === "function") return String(this.#fn.apply(this, this.#args));
    return undefined;
  }
}

export const isAsymmetricMatcher = (v: unknown) => v instanceof AsymmetricMatcher;

setAsymmetricPrinter((matcher, print) => {
  if (!(matcher instanceof AsymmetricMatcher)) return undefined as any;
  const { not, promise } = matcher.flags;
  const prefix = promise === RESOLVES ? "promise resolved to " : promise === REJECTS ? "promise rejected to " : "";
  if (matcher instanceof ExpectAnything) return prefix + (not ? "NotAnything" : "Anything");
  if (matcher instanceof ExpectAny) return prefix + (not ? "NotAny<" : "Any<") + markup("<cyan>", colors()) + className(matcher.constructorValue) + markup("<r>", colors()) + ">";
  if (matcher instanceof ExpectCloseTo) {
    const d = matcher.digitsValue | 0;
    return prefix + (not ? "NumberNotCloseTo" : "NumberCloseTo ") + `${matcher.numberValue | 0} (${d} digit${d === 1 ? "" : "s"})`;
  }
  if (matcher instanceof ExpectObjectContaining) return prefix + (not ? "ObjectNotContaining " : "ObjectContaining ") + print(matcher.objectValue, "object");
  if (matcher instanceof ExpectStringContaining) return prefix + (not ? "StringNotContaining " : "StringContaining ") + print(matcher.stringValue, "string");
  if (matcher instanceof ExpectStringMatching) {
    const t = matcher.testValue;
    return prefix + (not ? "StringNotMatching " : "StringMatching ") + (isRegExp(t) ? String(t) : print(t, "string"));
  }
  if (matcher instanceof ExpectCustomAsymmetricMatcher) {
    const custom = matcher.toAsymmetricMatcher();
    if (custom !== undefined) return custom;
    return prefix + (not ? "not " : "") + displayName(matcher.matcherFn) + " " + print(matcher.capturedArgs, "array");
  }
  return undefined as any;
});

// ── expect.extend ───────────────────────────────────────────────────────────
class ExpectMatcherContext {
  #flags: MatcherFlags;
  constructor(flags: MatcherFlags) {
    this.#flags = flags;
  }
  get isNot() {
    return this.#flags.not;
  }
  get promise() {
    return this.#flags.promise === REJECTS ? "rejects" : this.#flags.promise === RESOLVES ? "resolves" : "";
  }
  get expand() {
    return false;
  }
  get utils() {
    return utils;
  }
  get equals() {
    return contextEquals;
  }
  get assertionCalls() {
    return runtime.sequence?.expectCalls ?? 0;
  }
  get currentTestName() {
    return runtime.sequence?.testName;
  }
  get expectedAssertionsNumber() {
    const a = runtime.sequence?.assertions;
    return a?.kind === "exact" ? a.count : null;
  }
  get isExpectingAssertions() {
    return runtime.sequence?.assertions.kind === "atLeastOne";
  }
  get suppressedErrors() {
    return [];
  }
  get testPath() {
    return runtime.path || undefined;
  }
  get customTesters() {
    return [...equalityTesters];
  }
}

function contextEquals(a: unknown, b: unknown, customTesters?: unknown, strictCheck?: unknown): boolean {
  const testers = Array.isArray(customTesters) && customTesters.length ? customTesters : undefined;
  const saved = extraTesters;
  extraTesters = testers as Fn[] | undefined;
  try {
    return deepEquals(a, b, !!strictCheck);
  } finally {
    extraTesters = saved;
  }
}

function invalidMatcherResult(name: string, result: unknown): Error {
  const error = new Error(
    `Unexpected return from matcher function \`${name}\`.\nMatcher functions should return an object in the following format:\n  {message?: string | function, pass: boolean}\n'${show(result, { colors: colors() })}' was returned`,
  );
  error.name = "InvalidMatcherError";
  return error;
}

function executeCustomMatcher(label: string | undefined, name: string, fn: Fn, args: unknown[], flags: MatcherFlags, silent: boolean): boolean {
  let result = fn.apply(new ExpectMatcherContext(flags), args);
  if (isPromise(result)) {
    const state = native.promiseState(result);
    if (state === 0) {
      if (silent) return false;
      return result.then(r => finish(r)) as any;
    }
    const settled = native.promiseResult(result);
    if (state === 2) throw thrown(`Matcher \`${name}\` returned a promise that rejected`);
    result = settled;
  }
  return finish(result);
  function finish(result: any): boolean {
    let pass = false;
    let message: unknown;
    let valid = false;
    if (typeof result === "object" && result !== null && "pass" in result) {
      pass = !!result.pass;
      valid = true;
      if ("message" in result && result.message !== undefined) {
        if (typeof result.message !== "string" && typeof result.message !== "function") valid = false;
        message = result.message;
      }
    }
    if (!valid) throw invalidMatcherResult(name, result);
    if (flags.not) pass = !pass;
    if (pass || silent) return pass;
    if (typeof message === "function") message = message.call(globalThis);
    const text = message ? String(message) : "No message was specified for this matcher.";
    throw thrown(label === undefined ? text : `${label}\n\n${text}`);
  }
}

function extend(definitions: unknown) {
  if (!isObject(definitions)) throw thrown(pretty("<d>expect.<r>extend<d>(<r>matchers<d>)<r>\n\nExpected an object containing matchers\n"));
  for (const name in definitions) {
    const fn = (definitions as any)[name];
    if (typeof fn !== "function") {
      if (name === "default" || name === "__esModule") continue;
      throw new TypeError(`expect.extend: \`${name}\` is not a valid matcher. Must be a function, is "${fn === null ? "null" : typeof fn}"`);
    }
    const method = {
      [name](this: any, ...args: unknown[]) {
        if (this instanceof Expect) {
          const e = this;
          const flags = e[FLAGS];
          e[RAN] = true;
          const run = (received: unknown) => {
            countExpect();
            const r = executeCustomMatcher(labelOf(e), displayName(fn), fn, [received, ...args], flags, false);
            return isPromise(r) ? r.then(() => undefined) : e;
          };
          if (flags.promise !== NONE) return settledValue(e, flags, name, customParams(fn), run).then(() => undefined);
          if (flags.soft) {
            try {
              return run(e[VALUE]);
            } catch (error) {
              if (runtime.sequence) append(runtime.sequence.softErrors, error);
              return undefined;
            }
          }
          return run(e[VALUE]);
        }
        const flags: MatcherFlags = this instanceof ExpectStatic ? this.flags : { not: false, promise: NONE };
        return new ExpectCustomAsymmetricMatcher(flags, args, fn);
      },
    }[name];
    for (const target of [Expect.prototype, expect, ExpectStatic.prototype])
      Object.defineProperty(target, name, { value: method, writable: true, configurable: true, enumerable: false });
  }
}

/** `CustomMatcherParamsFormatter`: the matcher's parameters after the first, from its source. */
function customParams(fn: Fn): string {
  let source = "";
  try {
    source = Function.prototype.toString.call(fn);
  } catch {}
  const l = source.indexOf("(");
  const r = l >= 0 ? source.indexOf(")", l) : -1;
  if (l < 0 || r < 0) return "<green>...args<r>";
  const params = source.slice(l + 1, r).split(",");
  return params
    .slice(1)
    .map((p, i) => (p.trim() || `arg${i}`))
    .map(p => `<green>${p}<r>`)
    .join("<d>, <r>");
}

// ── matcher utils ───────────────────────────────────────────────────────────
const paint = (color: "d" | "green" | "red", text: string) => (text ? markup(`<${color}>`, colors()) + text + markup("<r>", colors()) : "");
function printValue(value: unknown, color?: "green" | "red") {
  const body = isError(value) ? `[${String(value)}]` : show(value, { colors: false });
  return color && colors() ? markup(`<${color}>`, true) + body + markup("<r>", true) : body;
}
function typeName(v: unknown): string {
  if (v === undefined) return "undefined";
  if (v === null) return "null";
  if (Array.isArray(v)) return "array";
  if (typeof v === "boolean") return "boolean";
  if (typeof v === "function") return "function";
  if (typeof v === "number") return "number";
  if (typeof v === "string") return "string";
  if (typeof v === "bigint") return "bigint";
  if (typeof v === "symbol") return "symbol";
  if (isRegExp(v)) return "regexp";
  if (isMap(v)) return "map";
  if (isSet(v)) return "set";
  if (isDate(v)) return "date";
  return "object";
}
const NO_VISUAL_DIFFERENCE = "Compared values have no visual difference.";
const utils = {
  stringify: (v: unknown) => printValue(v),
  printExpected: (v: unknown) => printValue(v, "green"),
  printReceived: (v: unknown) => printValue(v, "red"),
  EXPECTED_COLOR: (...t: unknown[]) => paint("green", t.map(String).join(" ")),
  RECEIVED_COLOR: (...t: unknown[]) => paint("red", t.map(String).join(" ")),
  matcherHint(...args: unknown[]) {
    const arg = (i: number) => (args[i] === undefined ? undefined : args[i]);
    const name = arg(0);
    if (typeof name !== "string") throw thrown("matcherHint: the first argument (matcher name) must be a string");
    const received = arg(1) === undefined ? "received" : String(arg(1));
    const expected = arg(2) === undefined ? "expected" : String(arg(2));
    const options = arg(3) === null ? undefined : arg(3);
    if (options !== undefined && !isObject(options)) throw thrown("matcherHint: options must be an object (or undefined)");
    const o = (k: string) => (options ? (options as any)[k] : undefined);
    const comment = o("comment") === undefined ? "" : String(o("comment"));
    const isDirect = !!o("isDirectExpectCall");
    const isNot = !!o("isNot");
    const promise = o("promise") === undefined ? "" : String(o("promise"));
    const second = o("secondArgument") ? String(o("secondArgument")) : "";
    const label = (text: string, color: "green" | "red", fnName: string, f: unknown) => {
      if (f === undefined) return paint(color, text);
      if (typeof f !== "function") throw new TypeError(`matcherHint: options.${fnName} must be a function`);
      return String(f(text));
    };
    let hint = "";
    let dim = "expect";
    const flush = (last: string) => {
      dim += last;
      hint += paint("d", dim);
      dim = "";
    };
    if (!isDirect && received) {
      flush("(");
      hint += label(received, "red", "receivedColor", o("receivedColor"));
      dim += ")";
    }
    if (promise) {
      flush(".");
      hint += promise;
    }
    if (isNot) {
      flush(".");
      hint += "not";
    }
    if (name.includes(".")) dim += name;
    else {
      flush(".");
      hint += name;
    }
    if (!expected) dim += "()";
    else {
      flush("(");
      hint += label(expected, "green", "expectedColor", o("expectedColor"));
      if (second) {
        hint += paint("d", ", ");
        hint += label(second, "green", "secondArgumentColor", o("secondArgumentColor"));
      }
      dim += ")";
    }
    if (comment) dim += " // " + comment;
    hint += paint("d", dim);
    return hint;
  },
  printWithType(name: unknown, value: unknown, print: unknown) {
    if (typeof print !== "function") throw new TypeError("printWithType: the third argument (print) must be a function");
    const printed = String(print(value));
    if (value === undefined || value === null) return `${name} has value: ${printed}`;
    return `${name} has type:  ${typeName(value)}\n${name} has value: ${printed}`;
  },
  diff(expected: unknown, received: unknown) {
    if ((typeof expected === "number" && typeof received === "number") || (typeof expected === "bigint" && typeof received === "bigint") || (typeof expected === "boolean" && typeof received === "boolean"))
      return null;
    if (Object.is(expected, received)) return paint("d", NO_VISUAL_DIFFERENCE);
    if (isAsymmetricMatcher(expected)) return null;
    const et = typeName(expected),
      rt = typeName(received);
    if (et !== rt) return `  Comparing two different types of values. Expected ${paint("green", et)} but received ${paint("red", rt)}.`;
    const [r, e] = et === "string" ? [String(received), String(expected)] : [diffText(received), diffText(expected)];
    if (r === e) return paint("d", NO_VISUAL_DIFFERENCE);
    return native.diff(false, r, e, colors());
  },
};

// ── equality testers ────────────────────────────────────────────────────────
const equalityTesters: Fn[] = [];
let extraTesters: Fn[] | undefined;
setEqualityTesters(() => {
  const testers = extraTesters ? [...equalityTesters, ...extraTesters] : equalityTesters;
  if (!testers.length) return undefined;
  return (a, b) => {
    for (const tester of testers) {
      const verdict = tester.call(new ExpectMatcherContext({ not: false, promise: NONE }), a, b, testers);
      if (verdict !== undefined) return !!verdict;
    }
    return undefined;
  };
});

// ── expect ────────────────────────────────────────────────────────────────────
class ExpectStatic {
  #flags: MatcherFlags;
  constructor(flags: MatcherFlags) {
    this.#flags = flags;
  }
  get flags() {
    return this.#flags;
  }
  get not() {
    return new ExpectStatic({ ...this.#flags, not: !this.#flags.not });
  }
  get resolvesTo() {
    if (this.#flags.promise !== NONE) throw thrown(`expect.resolvesTo: already called expect.${this.#flags.promise === RESOLVES ? "resolvesTo" : "rejectsTo"} on this chain`);
    return new ExpectStatic({ ...this.#flags, promise: RESOLVES });
  }
  get rejectsTo() {
    if (this.#flags.promise !== NONE) throw thrown(`expect.rejectsTo: already called expect.${this.#flags.promise === RESOLVES ? "resolvesTo" : "rejectsTo"} on this chain`);
    return new ExpectStatic({ ...this.#flags, promise: REJECTS });
  }
  anything() {
    return new ExpectAnything(this.#flags);
  }
  any(c: unknown) {
    return new ExpectAny(this.#flags, c);
  }
  arrayContaining(a: unknown) {
    return new ExpectArrayContaining(this.#flags, a);
  }
  closeTo(n: unknown, p: unknown) {
    return new ExpectCloseTo(this.#flags, n, p);
  }
  objectContaining(o: unknown) {
    return new ExpectObjectContaining(this.#flags, o);
  }
  stringContaining(s: unknown) {
    return new ExpectStringContaining(this.#flags, s);
  }
  stringMatching(t: unknown) {
    return new ExpectStringMatching(this.#flags, t);
  }
  toSatisfy(...args: unknown[]) {
    return toSatisfyMatcher(this.#flags, args);
  }
}

const toSatisfyFn = function toSatisfy(received: unknown, predicate: Fn) {
  return { pass: !!predicate(received) };
};
function toSatisfyMatcher(flags: MatcherFlags, args: unknown[]) {
  if (typeof args[0] !== "function") throw thrown(pretty("<d>expect.<r>toSatisfy<d>(<r>predicate<d>, message?)<r>\n\nExpected a function\n"));
  return new ExpectCustomAsymmetricMatcher(flags, args, toSatisfyFn);
}

const plain: MatcherFlags = { not: false, promise: NONE };
export const expect: any = function expect(value?: unknown, message?: unknown) {
  if (new.target) throw thrown("expect() cannot be called with new");
  return makeExpect(value, message);
};
Object.defineProperty(expect, "prototype", { value: Expect.prototype });
Object.assign(expect, {
  anything: () => new ExpectAnything(plain),
  any: (c: unknown) => new ExpectAny(plain, c),
  arrayContaining: (a: unknown) => new ExpectArrayContaining(plain, a),
  closeTo: (n: unknown, p: unknown) => new ExpectCloseTo(plain, n, p),
  objectContaining: (o: unknown) => new ExpectObjectContaining(plain, o),
  stringContaining: (s: unknown) => new ExpectStringContaining(plain, s),
  stringMatching: (t: unknown) => new ExpectStringMatching(plain, t),
  toSatisfy: (...args: unknown[]) => toSatisfyMatcher(plain, args),
  extend,
  addEqualityTesters(testers: unknown) {
    if (!Array.isArray(testers))
      throw new TypeError(`expect.addEqualityTesters: expected an array of functions, got ${show(testers)}`);
    for (const t of testers) {
      if (typeof t !== "function") throw new TypeError(`expect.addEqualityTesters: expected an array of functions, got ${show(t)}`);
      append(equalityTesters, t);
    }
  },
  addSnapshotSerializer(serializer: unknown) {
    append(snapshotSerializers, serializer);
  },
  assertions(expected: unknown) {
    const n = nonNegativeInteger(expected, 0xffffffff);
    const sequence = currentSequenceFor("expect.assertions()");
    sequence.assertions = { kind: "exact", count: n };
  },
  hasAssertions() {
    const sequence = currentSequenceFor("expect.assertions()");
    if (sequence.assertions.kind !== "exact") sequence.assertions = { kind: "atLeastOne" };
  },
  unreachable(message?: unknown) {
    if (message === undefined || message === null) {
      const error = new Error("reached unreachable code");
      error.name = "UnreachableError";
      throw error;
    }
    if (typeof message === "string") {
      const error = new Error(message);
      error.name = "UnreachableError";
      throw error;
    }
    throw message;
  },
  getState() {
    const s = runtime.sequence;
    Object.assign(state, {
      assertionCalls: s?.expectCalls ?? 0,
      currentTestName: s?.testName,
      expectedAssertionsNumber: s?.assertions.kind === "exact" ? s.assertions.count : null,
      isExpectingAssertions: s?.assertions.kind === "atLeastOne",
      suppressedErrors: [],
      testPath: runtime.path || undefined,
    });
    return state;
  },
  setState(partial: unknown) {
    if (!isObject(partial)) throw invalidArgumentType("expect.setState", "state", "object");
    let touched = false;
    for (const key of Object.keys(partial)) {
      const value = (partial as any)[key];
      if (key === "assertionCalls" || key === "expectedAssertionsNumber" || key === "isExpectingAssertions") touched = true;
      else (state as any)[key] = value;
    }
    if (!touched) return;
    const s = runtime.sequence;
    if (!s || s.concurrent)
      throw thrown(
        "expect.setState() cannot set the assertion counters in the describe phase, in concurrent tests, between tests, or after test execution has completed",
      );
    const p = partial as any;
    if ("assertionCalls" in p) s.expectCalls = p.assertionCalls;
    if ("expectedAssertionsNumber" in p)
      s.assertions = p.expectedAssertionsNumber == null ? (s.assertions.kind === "exact" ? { kind: "none" } : s.assertions) : { kind: "exact", count: p.expectedAssertionsNumber };
    if ("isExpectingAssertions" in p) {
      if (p.isExpectingAssertions && s.assertions.kind === "none") s.assertions = { kind: "atLeastOne" };
      if (!p.isExpectingAssertions && s.assertions.kind === "atLeastOne") s.assertions = { kind: "none" };
    }
  },
  soft(value: unknown, message?: unknown) {
    return makeExpect(value, message, { soft: true });
  },
  poll(fn: unknown, options?: any) {
    if (typeof fn !== "function") throw new TypeError(`The "fn" argument must be of type function. Received ${show(fn)}`);
    const interval = options?.interval ?? 50;
    const timeout = options?.timeout ?? 1000;
    return makeExpect(fn, options?.message, { poll: [interval, timeout] });
  },
});
for (const [name, getter] of [
  ["not", () => new ExpectStatic({ not: true, promise: NONE })],
  ["resolvesTo", () => new ExpectStatic({ not: false, promise: RESOLVES })],
  ["rejectsTo", () => new ExpectStatic({ not: false, promise: REJECTS })],
] as const)
  Object.defineProperty(expect, name, { get: getter, configurable: true });

const state: Record<string, unknown> = {};
export const snapshotSerializers: unknown[] = [];

function currentSequenceFor(what: string) {
  const s = runtime.sequence;
  if (runtime.phase !== "execution" && !s) throw thrown(`${what} must be called within a test`);
  if (!s || s.concurrent)
    throw thrown(`${what} is not supported in the describe phase, in concurrent tests, between tests, or after test execution has completed`);
  return s;
}

// ── expectTypeOf ────────────────────────────────────────────────────────────
/** Only the type checker looks at `expectTypeOf()`: at run time nothing is checked. */
export class ExpectTypeOf {
  constructor() {
    throw thrown("expectTypeOf() cannot be called with new");
  }
}
const typeOfInstance = () => Object.create(ExpectTypeOf.prototype);
for (const name of [
  "toBeAny", "toBeArray", "toBeBigInt", "toBeBoolean", "toBeConstructibleWith", "toBeFunction", "toBeNever", "toBeNull",
  "toBeNullable", "toBeNumber", "toBeObject", "toBeString", "toBeSymbol", "toBeUndefined", "toBeUnknown", "toBeVoid",
  "toEqualTypeOf", "toExtend", "toMatchObjectType", "toMatchTypeOf",
])
  Object.defineProperty(ExpectTypeOf.prototype, name, { value: { [name]() {} }[name], writable: true, configurable: true });
for (const name of ["exclude", "extract", "map", "omit", "parameter", "pick", "toBeCallableWith", "toHaveProperty"])
  Object.defineProperty(ExpectTypeOf.prototype, name, { value: { [name]: () => typeOfInstance() }[name], writable: true, configurable: true });
for (const name of ["asserts", "branded", "constructorParameters", "guards", "instance", "items", "not", "parameters", "resolves", "returns", "thisParameter"])
  Object.defineProperty(ExpectTypeOf.prototype, name, { get: typeOfInstance, configurable: true });
export const expectTypeOf: any = function expectTypeOf() {
  return typeOfInstance();
};
Object.defineProperty(expectTypeOf, "prototype", { value: ExpectTypeOf.prototype });

export { M, countExpect, labelOf, signature, thrown, NONE, RESOLVES, REJECTS };
