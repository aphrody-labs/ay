// expect()'s equality: a port of Bun__deepEquals and Bun__deepMatch (src/jsc/bindings/bindings.cpp) in the modes
// bun:test runs them, `<isStrict, enableAsymmetricMatchers = true, checkPrototypes = false>`.
import {
  className,
  isArrayBuffer,
  isBoxedBigInt,
  isBoxedBoolean,
  isBoxedNumber,
  isBoxedString,
  isBoxedSymbol,
  isDataView,
  isDate,
  isError,
  isMap,
  isPromise,
  isRegExp,
  isSet,
  isSharedArrayBuffer,
  isTypedArray,
  isWeakMap,
  isWeakSet,
  typedArrayName,
} from "./format.ts";
import { append } from "./state.ts";

export const ASYMMETRIC = Symbol.for("bun:test:asymmetric");

/** A built-in asymmetric matcher (`expect.any()`…) answers through this method: true, false, or undefined if it isn't one. */
export type MatchesFn = (other: unknown) => boolean;

const { getOwnPropertyNames, getOwnPropertySymbols, getOwnPropertyDescriptor, getPrototypeOf, prototype: ObjectPrototype } =
  Object;
const hasOwn = Object.prototype.hasOwnProperty;

let equalityTesters: (() => ((a: unknown, b: unknown) => boolean | undefined) | undefined) | undefined;
/** expect.addEqualityTesters(): the testers, which return true, false or undefined (no verdict). */
export function setEqualityTesters(get: () => ((a: unknown, b: unknown) => boolean | undefined) | undefined) {
  equalityTesters = get;
}

/** A built-in matcher: an object whose [ASYMMETRIC] is its match function (its `.not` applied). */
export function builtinMatcher(v: unknown): MatchesFn | undefined {
  if (typeof v !== "object" || v === null) return undefined;
  let fn: unknown;
  try {
    fn = (v as any)[ASYMMETRIC];
  } catch {
    return undefined;
  }
  return typeof fn === "function" ? (fn as MatchesFn).bind(v) : undefined;
}

/** `asymmetricMatchMethod`: an `asymmetricMatch` data property below Object.prototype, without running getters. */
function asymmetricMatchMethod(v: unknown): Function | undefined {
  if (typeof v !== "object" || v === null) return undefined;
  if (builtinMatcher(v)) return undefined;
  let object: any = v;
  try {
    for (let depth = 0; object && object !== ObjectPrototype && depth < MAX_PROTOTYPES; depth++) {
      const desc = getOwnPropertyDescriptor(object, "asymmetricMatch");
      if (desc) return "value" in desc && typeof desc.value === "function" ? desc.value : undefined;
      object = getPrototypeOf(object);
    }
  } catch {}
  return undefined;
}

function matchUserAsymmetricMatcher(a: unknown, b: unknown): boolean | undefined {
  const methodOfA = asymmetricMatchMethod(a);
  const methodOfB = asymmetricMatchMethod(b);
  if (!methodOfA === !methodOfB) return undefined;
  const method = methodOfA ?? methodOfB!;
  return !!method.call(methodOfA ? a : b, methodOfA ? b : a);
}

function runTesters(a: unknown, b: unknown): boolean | undefined {
  const tester = equalityTesters?.();
  return tester ? tester(a, b) : undefined;
}

const isObjectLike = (v: unknown): v is object => (typeof v === "object" && v !== null) || typeof v === "function";

/** Own enumerable string and symbol keys, in the engine's order. */
function enumerableKeys(o: object): (string | symbol)[] {
  const out: (string | symbol)[] = [];
  for (const key of [...getOwnPropertyNames(o), ...getOwnPropertySymbols(o)]) {
    const desc = getOwnPropertyDescriptor(o, key);
    if (desc?.enumerable) append(out, key);
  }
  return out;
}

const isStackOfError = (o: object, key: string | symbol) => key === "stack" && isError(o);

class Equals {
  stack: [unknown, unknown][] = [];
  constructor(readonly strict: boolean) {}

  eq(v1: unknown, v2: unknown, addToStack = true, present1 = true, present2 = true): boolean {
    {
      const m2 = present2 ? builtinMatcher(v2) : undefined;
      if (m2) return m2(present1 ? v1 : undefined);
      const m1 = present1 ? builtinMatcher(v1) : undefined;
      if (m1) return m1(present2 ? v2 : undefined);
      const user = present1 && present2 ? matchUserAsymmetricMatcher(v1, v2) : undefined;
      if (user !== undefined) return user;
      const verdict = runTesters(present1 ? v1 : undefined, present2 ? v2 : undefined);
      if (verdict !== undefined) return verdict;
    }
    if (present1 && present2 && Object.is(v1, v2)) return true;
    if (!present1 || !present2) return present1 === present2;
    if (!isObjectLike(v1) || !isObjectLike(v2)) return false;

    for (const [a, b] of this.stack) {
      if (a === v1) return b === v2;
      if (b === v2) return false;
    }
    if (addToStack) append(this.stack, [v1, v2]);
    try {
      return this.objects(v1, v2);
    } finally {
      if (addToStack) this.stack.pop();
    }
  }

  objects(o1: any, o2: any): boolean {
    const special = this.special(o1, o2) ?? this.special(o2, o1);
    if (special !== undefined) return special;

    const a1 = Array.isArray(o1);
    const a2 = Array.isArray(o2);
    if (a1 !== a2) return false;
    if (a1 && a2) {
      const len1 = o1.length;
      const len2 = o2.length;
      if (this.strict && len1 !== len2) return false;
      let i = 0;
      for (; i < len1; i++) {
        const [has1, left] = indexWithoutAccessors(o1, i);
        const [has2, right] = indexWithoutAccessors(o2, i);
        if (this.strict) {
          if (!has1 && !has2) continue;
          if (!has1 || !has2) return false;
        } else if ((!has1 || !has2) && ((has1 && left === undefined) || (has2 && right === undefined) || (!has1 && !has2)))
          continue;
        let l = left,
          r = right,
          p1 = has1,
          p2 = has2;
        if (!has1 && builtinMatcher(right)) {
          l = o1[i];
          p1 = true;
        } else if (!has2 && builtinMatcher(left)) {
          r = o2[i];
          p2 = true;
        }
        if (!this.eq(l, r, true, p1, p2)) return false;
      }
      for (; i < len2; i++) {
        const [has, right] = indexWithoutAccessors(o2, i);
        if (!has || right === undefined) continue;
        return false;
      }
      const s1 = getOwnPropertySymbols(o1).filter(k => getOwnPropertyDescriptor(o1, k)?.enumerable);
      const s2 = getOwnPropertySymbols(o2).filter(k => getOwnPropertyDescriptor(o2, k)?.enumerable);
      if (!s1.length && !s2.length) return true;
      return this.enumerated(o1, s1, o2, s2);
    }

    if (this.strict && className(o1) !== className(o2)) return false;
    return this.enumerated(o1, enumerableKeys(o1), o2, enumerableKeys(o2));
  }

  /** `enumeratedPropertiesEqual`. */
  enumerated(o1: any, names1: (string | symbol)[], o2: any, names2: (string | symbol)[]): boolean {
    if (this.strict && names1.length !== names2.length) return false;
    const set2 = new Set(names2);
    const set1 = new Set(names1);
    let paired = 0;
    for (const name of names1) {
      const isPaired = set2.has(name);
      if (isPaired) paired++;
      if (!(isPaired ? this.paired(o1, o2, name) : this.unpaired(o1, o2, name, true))) return false;
    }
    if (paired === names2.length) return true;
    for (const name of names2) {
      if (set1.has(name)) continue;
      if (!this.unpaired(o1, o2, name, false)) return false;
    }
    return true;
  }

  paired(o1: any, o2: any, name: string | symbol): boolean {
    if (isStackOfError(o1, name)) return true;
    return this.eq(o1[name], o2[name]);
  }

  unpaired(o1: any, o2: any, name: string | symbol, inFirst: boolean): boolean {
    if (this.strict) return false;
    if (isStackOfError(o1, name)) return true;
    const value = (inFirst ? o1 : o2)[name];
    if (value === undefined) return true;
    if (!isError(o1) || name === "__proto__") return inFirst && isMatcherOfUnlistedProperty(value, o2, name);
    const other = inFirst ? o2 : o1;
    if (!(name in other)) return false;
    return inFirst ? this.eq(value, other[name]) : this.eq(other[name], value);
  }

  /** `specialObjectsDequal`: undefined if neither side is special. */
  special(c1: any, c2: any): boolean | undefined {
    if (isSet(c1)) {
      if (!isSet(c2)) return false;
      if (c1.size !== c2.size) return false;
      outer: for (const key1 of c1) {
        if (c2.has(key1)) continue;
        for (const key2 of c2) if (this.eq(key1, key2, false)) continue outer;
        return false;
      }
      return true;
    }
    if (isMap(c1)) {
      if (!isMap(c2)) return false;
      if (c1.size !== c2.size) return false;
      for (const [key1, value1] of c1) {
        let value2 = c2.get(key1);
        if (value2 === undefined) {
          let found = false;
          for (const [key2, v2] of c2) {
            if (this.eq(key1, key2, false)) {
              found = true;
              value2 = v2;
              break;
            }
          }
          if (!found) return false;
        }
        if (!this.eq(value1, value2, false)) return false;
      }
      return true;
    }
    if (isArrayBuffer(c1) || isSharedArrayBuffer(c1)) {
      if (!(isArrayBuffer(c2) || isSharedArrayBuffer(c2))) return false;
      if (c1.byteLength !== c2.byteLength) return false;
      if (isSharedArrayBuffer(c1) !== isSharedArrayBuffer(c2)) return false;
      const x = new Uint8Array(c1),
        y = new Uint8Array(c2);
      for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
      return true;
    }
    if (isDate(c1)) {
      if (!isDate(c2)) return false;
      return Date.prototype.getTime.call(c1) === Date.prototype.getTime.call(c2);
    }
    if (isRegExp(c1)) {
      if (!isRegExp(c2)) return false;
      return c1.source === c2.source && c1.flags === c2.flags;
    }
    if (isError(c1)) {
      if (!isError(c2)) return false;
      if (errorType(c1) !== errorType(c2)) return false;
      if (String(c1.name) !== String(c2.name)) return false;
      if (String(c1.message) !== String(c2.message)) return false;
      if (this.strict) {
        const has1 = "cause" in c1,
          has2 = "cause" in c2;
        if (has1 !== has2) return false;
      }
      if (!this.eq(c1.cause, c2.cause)) return false;
      const k1: (string | symbol)[] = [];
      const k2: (string | symbol)[] = [];
      for (const k in c1) append(k1, k);
      for (const k in c2) append(k2, k);
      append(k1, ...getOwnPropertySymbols(c1).filter(s => getOwnPropertyDescriptor(c1, s)?.enumerable));
      append(k2, ...getOwnPropertySymbols(c2).filter(s => getOwnPropertyDescriptor(c2, s)?.enumerable));
      return this.enumerated(c1, k1, c2, k2);
    }
    if (isTypedArray(c1)) {
      if (!isTypedArray(c2) || typedArrayName(c1) !== typedArrayName(c2)) return false;
      if (c1.byteLength !== c2.byteLength) return false;
      const extras =
        this.strict && (getOwnPropertySymbols(c1).length > 0 || getOwnPropertySymbols(c2).length > 0 || nonIndexKeys(c1).length > 0 || nonIndexKeys(c2).length > 0);
      if (c1.byteLength === 0) return extras ? undefined : true;
      const tester = equalityTesters?.();
      if (tester) {
        for (let i = 0; i < c1.length; i++) if (!this.eq(c1[i], c2[i])) return false;
        return extras ? undefined : true;
      }
      const name = typedArrayName(c1);
      if (!this.strict && (name === "Float16Array" || name === "Float32Array" || name === "Float64Array")) {
        for (let i = 0; i < c1.length; i++) if (c1[i] !== c2[i]) return false;
      } else {
        const x = new Uint8Array(c1.buffer, c1.byteOffset, c1.byteLength);
        const y = new Uint8Array(c2.buffer, c2.byteOffset, c2.byteLength);
        for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
      }
      return extras ? undefined : true;
    }
    if (isBoxedString(c1)) {
      if (!isBoxedString(c2)) return false;
      if (className(c1) !== className(c2)) return false;
      if (String.prototype.valueOf.call(c1) !== String.prototype.valueOf.call(c2)) return false;
      if (this.strict && (nonIndexKeys(c1).length || nonIndexKeys(c2).length)) return undefined;
      return true;
    }
    if (typeof c1 === "function") return false;
    if (c1 === globalThis) return c2 === globalThis;
    if (isBoxedNumber(c1) || isBoxedBoolean(c1)) {
      const num = isBoxedNumber(c1);
      if (num ? !isBoxedNumber(c2) : !isBoxedBoolean(c2)) return false;
      const v1 = num ? Number.prototype.valueOf.call(c1) : Boolean.prototype.valueOf.call(c1);
      const v2 = num ? Number.prototype.valueOf.call(c2) : Boolean.prototype.valueOf.call(c2);
      if (!Object.is(v1, v2)) return false;
      return undefined;
    }
    if (isBoxedSymbol(c1) || isBoxedBigInt(c1)) {
      const sym = isBoxedSymbol(c1);
      if (sym !== isBoxedSymbol(c2) || isBoxedBigInt(c1) !== isBoxedBigInt(c2)) return false;
      const v1 = sym ? Symbol.prototype.valueOf.call(c1) : BigInt.prototype.valueOf.call(c1);
      const v2 = sym ? Symbol.prototype.valueOf.call(c2) : BigInt.prototype.valueOf.call(c2);
      if (!Object.is(v1, v2)) return false;
      return undefined;
    }
    if (c1 instanceof URL && c2 instanceof URL) {
      if (c1.href !== c2.href) return false;
      return undefined;
    }
    if (this.strict && (c1 instanceof URL) !== (c2 instanceof URL)) return false;
    return undefined;
  }
}

function nonIndexKeys(o: object): string[] {
  return getOwnPropertyNames(o).filter(k => !/^(0|[1-9]\d*)$/.test(k) && k !== "length" && getOwnPropertyDescriptor(o, k)?.enumerable);
}

function errorType(e: any): string {
  for (const C of [AggregateError, EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError] as any[])
    if (e instanceof C) return C.name;
  return "Error";
}

/** `getIndexWithoutAccessors`: [present, value]; a getter is not run. */
function indexWithoutAccessors(o: any, i: number): [boolean, unknown] {
  const desc = getOwnPropertyDescriptor(o, i);
  if (!desc) return [false, undefined];
  if (!("value" in desc)) return [false, undefined];
  return [true, desc.value];
}

/** `isMatcherOfUnlistedProperty`. */
function isMatcherOfUnlistedProperty(value: unknown, object: any, name: string | symbol): boolean {
  const builtin = builtinMatcher(value);
  if (!builtin && !asymmetricMatchMethod(value)) return false;
  if (hasOwn.call(object, name)) return false;
  if (!(name in object)) return false;
  const received = object[name];
  if (builtin) return builtin(received);
  return new Equals(false).eq(value, received);
}

/** `received.jest_deep_equals(expected)` (toEqual) or `jest_strict_deep_equals` (toStrictEqual). */
export function deepEquals(a: unknown, b: unknown, strict = false): boolean {
  return new Equals(strict).eq(a, b);
}

const isMatcherCell = (v: unknown) => builtinMatcher(v) !== undefined;

/** `Bun__deepMatch`: does `obj` have every property of `subset` (toMatchObject, expect.objectContaining). */
export function deepMatch(obj: any, subset: any, isMatchingObjectContaining = false, seen = new Set<string>(), pairs: [unknown, unknown][] = []): boolean {
  if (!isMatchingObjectContaining) {
    const verdict = runTesters(obj, subset);
    if (verdict !== undefined) return verdict;
  }
  if (obj === subset) return true;
  const subsetProps: (string | symbol)[] = [];
  for (const k in subset) append(subsetProps, k);
  for (const s of getOwnPropertySymbols(subset)) if (getOwnPropertyDescriptor(subset, s)?.enumerable) append(subsetProps, s);

  if (Array.isArray(obj) && Array.isArray(subset)) {
    if (obj.length !== subset.length) return false;
    let objCount = 0;
    for (const _ in obj) objCount++;
    objCount += getOwnPropertySymbols(obj).filter(s => getOwnPropertyDescriptor(obj, s)?.enumerable).length;
    if (objCount !== subsetProps.length) return false;
  }

  for (const property of subsetProps) {
    if (property === "__proto__" && !isMatchingObjectContaining) {
      if (hasOwn.call(subset, property) && !hasPropertyBelowObjectPrototype(obj, property)) return false;
    }
    if (!(property in Object(obj))) return false;
    const prop = obj[property];
    const subsetProp = subset[property];

    const sm = builtinMatcher(subsetProp);
    if (sm) {
      if (!sm(prop)) return false;
      continue;
    }
    const pm = builtinMatcher(prop);
    if (pm) {
      if (!pm(subsetProp)) return false;
      continue;
    }
    const user = matchUserAsymmetricMatcher(prop, subsetProp);
    if (user !== undefined) {
      if (!user) return false;
      continue;
    }

    if (isObjectLike(subsetProp) && typeof subsetProp === "object" && isObjectLike(prop) && typeof prop === "object") {
      if (isMatchingObjectContaining) {
        if (!deepEquals(prop, subsetProp)) return false;
      } else {
        if (pairs.some(([p, s]) => p === prop && s === subsetProp)) continue;
        append(pairs, [prop, subsetProp]);
        if (!deepMatch(prop, subsetProp, isMatchingObjectContaining, seen, pairs)) return false;
      }
    } else {
      const verdict = runTesters(prop, subsetProp);
      if (verdict !== undefined) {
        if (!verdict) return false;
        continue;
      }
      if (!Object.is(prop, subsetProp)) return false;
    }
  }
  return true;
}

/** A proxy can answer a new prototype each time: past this many, JSC's walk has overflowed its stack. */
const MAX_PROTOTYPES = 10_000;

function hasPropertyBelowObjectPrototype(object: any, name: string | symbol): boolean {
  for (let depth = 0; isObjectLike(object) && object !== ObjectPrototype; depth++) {
    if (depth === MAX_PROTOTYPES) throw new RangeError("Maximum call stack size exceeded.");
    if (hasOwn.call(object, name)) return true;
    object = getPrototypeOf(object);
  }
  return false;
}

export { isMatcherCell, isPromise, isWeakMap, isWeakSet, isDataView };
