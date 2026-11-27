// What tests/bindgen.rs calls through the Rust that src/codegen/bindgen-js.ts writes from it: one of each kind of
// member and conversion the emitter supports.
import * as b from "../../codegen/bindgen-lib.ts";
import * as table from "../../codegen/builtins/lib.ts";

export default b.module({ rustPath: "crate::fixture" });

/** Named as bound; `b.rest`; `.allowStringObject.local`; an error of `validatorExpects`; `convertedFirst`. */
export const joinAll = b.fn({
  namedAsBound: true,
  length: 1,
  args: { separator: b.string.allowStringObject.local, parts: b.rest(b.string.allowStringObject.local) },
  validatorExpects: { parts: "string" },
  convertedFirst: ["parts"],
  returns: b.string.local,
  capability: "heap",
  throws: true,
});

/** A dictionary that is returned. */
export const Split = b.dictionary({ head: b.string.local, count: b.u32, last: b.any, done: b.bool.optional });

/** `b.rest` of `b.any.stringObjectAsString`. */
export const split = b.fn({
  args: { text: b.string.local, items: b.rest(b.any.stringObjectAsString) },
  returns: Split,
  capability: "heap",
  throws: true,
});

/** `b.any.returning` with what is not a word: the result is held to nothing. */
export const same = b.fn({
  args: { value: b.any },
  returns: b.any.returning(table.operand("value").spec),
  capability: "heap",
  uses: { value: [table.returned] },
  reads: [],
  writes: [],
});

export const add = b.fn({
  args: { a: b.i32, b: b.i32.loose },
  returns: b.i32,
  requiredArguments: 1,
});

export const greet = b.fn({
  args: { name: b.string.utf8, punctuation: b.string.loose.utf8.default("!") },
  returns: b.string.utf8,
  capability: "heap",
  throws: true,
});

export const call = b.fn({
  args: { callback: b.any, flag: b.bool.optional },
  returns: b.any,
  capability: "js",
  throws: true,
});

/** A dictionary that is taken: defaults, `.nullable`, a member read by a second name, one that is required. */
export const Shape = b.dictionary({
  sides: b.u32,
  label: { type: b.string.utf8.nullable, altNames: ["name"] },
  filled: b.bool.loose.nullable.default(true),
});

/** A union of a string and a dictionary: what neither takes goes to the dictionary, which throws. */
export const ShapeOrName = b.union({ name: b.string.utf8, shape: Shape });

export const describeShape = b.fn({
  args: { shape: ShapeOrName.nullable },
  returns: b.string.utf8,
  capability: "heap",
  throws: true,
});

/** A number or a BigInt, and nothing else: what neither takes is a type error. */
export const Wide = b.union({ number: b.f64, bigint: b.u64.allowBigInt });

export const describeWide = b.fn({
  args: { wide: Wide },
  returns: b.string.utf8,
});

export const math = b.namespace({
  half: { args: { value: b.f64 }, returns: b.f64 },
  isTruthy: { args: { value: b.bool.loose }, returns: b.bool },
});

export const Counter = b.class({
  rustPath: "crate::fixture::Counter",
  construct: { args: { start: b.u32.default(0) } },
  proto: {
    count: { getter: b.u32 },
    label: { getter: b.string.utf8, setter: true },
    increment: { args: { by: b.u32.default(1) }, returns: b.u32, mutates: true },
    describe: { returns: b.string.utf8, capability: "heap", throws: true },
    __peek: { returns: b.u32, privateSymbol: "counterPeek" },
  },
  static: {
    zero: { returns: b.u32 },
  },
});

/** `b.any.variadic`: the body takes the arguments as they are; `replacesBoundFunction` is an ordinary function here. */
export const countArguments = b.fn({
  args: { args: b.any.variadic },
  returns: b.u32,
  length: 2,
  replacesBoundFunction: true,
});

/** A class that only Rust makes, with a method of `b.any.variadic`. */
export const Tally = b.class({
  rustPath: "crate::fixture::Tally",
  proto: {
    sum: { args: { values: b.any.variadic }, returns: b.f64, capability: "js", throws: true, length: 1 },
  },
});

/** `b.value`: the instance the body makes. */
export const tally = b.value({ returns: Tally, capability: "js" });
