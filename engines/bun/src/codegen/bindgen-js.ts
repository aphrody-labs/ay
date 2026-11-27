#!/usr/bin/env bun
// Usage: bindgen-js.ts --sources=<crate>=<a.bind.ts>,.. --codegen-path=<dir>
//
// The `.bind.ts` schemas as Rust against `bun_js` (src/bun_js), the engine boundary on Boa: one
// `generated_bun_js_<crate>.rs` per crate, which the crate `include!`s. There is no C++: each host function converts
// its own arguments, with `bun_js::bindgen::from_js`. What it does not support yet it refuses by name, rather than
// generate something that differs from the JavaScriptCore build.
//
// Per schema file it writes a module named after the file, which holds:
//   - a `bun_js::JSHostFn` per function (`pub fn <name>(global, frame)`), and `create_<name>(global)`, its
//     function object;
//   - `create_<namespace>(global)`, the namespace object;
//   - `impl bun_js::JsClass` for each class's payload: its builtins are the JavaScript of src/js/builtins/<class>.ts,
//     and a method with a `privateSymbol` is the property `__intrinsic__<symbol>` that a builtin reads;
//   - a struct with `into_js` for each dictionary that is returned, and a struct or an enum with `from_js` for each
//     dictionary and union that is taken;
//   - `exports(global)`: every function, namespace and class constructor by the name JavaScript knows it by;
//   - `native(global, symbol)`: what `$bindgen("<file>.bind.ts", symbol)` answers.
// At the top level, `native(global, file, symbol)` picks the module of `file`.
//
// What a body takes and returns is that of the JavaScriptCore build, but for strings: `b.string.local` and
// `b.string.utf8` are `&'a [u8]` of UTF-8 as an argument and anything that is `AsRef<[u8]>` as a result, and a member
// of a dictionary that is returned is `&'a [u8]`.

import path from "node:path";
import {
  AnyType,
  ArrayType,
  BigIntResultType,
  BoolType,
  BufferSourceType,
  BytesType,
  callableType,
  ClassType,
  Construction,
  DefaultType,
  DictionaryType,
  DoubleType,
  type FunctionDefinition,
  type Getter,
  IntegerType,
  LocalStringType,
  type Method,
  type NamespaceDefinition,
  NamedType,
  OptionalType,
  type OverloadDispatch,
  OwnedUtf8Type,
  RestType,
  Signature,
  snakeCase,
  StringType,
  type Type,
  UnionType,
  UnitType,
  Utf8Type,
  VariadicType,
} from "./bindgen-lib-internal.ts";
import { loadSchemaFile, sortTypes } from "./bindgen-load.ts";
import type { SchemaFile } from "./bindgen-emit.ts";
import { argParse, writeIfNotChanged } from "./helpers.ts";

class Unsupported extends Error {}

/** What `b.<type>` the emitter does not convert yet. */
const refuse = (what: string): never => {
  throw new Unsupported(`${what} is not supported by the bun_js emitter yet`);
};

const KEYWORDS = new Set(
  "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while abstract become box do final macro override priv typeof unsized virtual yield try gen".split(
    " ",
  ),
);
const ident = (name: string) => (KEYWORDS.has(name) ? `r#${name}` : name);
const rustString = (s: string) => JSON.stringify(s);

/** The items of a schema's module that are not named after one of its exports. */
const MODULE_ITEMS = new Set(["exports", "namespaces", "native"]);

// ───────────────────────────── types ─────────────────────────────

const isPlainInteger = (type: IntegerType) => /^Bun::Bindgen::IDL(Strict|Loose)Integer<[^,>]+>$/.test(type.idlType);
/** `b.<integer>.allowBigInt`: a number as the integer is, or a BigInt in its range. */
const isBigIntInteger = (type: IntegerType) =>
  /^Bun::Bindgen::IDLStrictInteger<[^,>]+, Bun::Bindgen::AllowBigInt::Yes>$/.test(type.idlType);

/** The `from_js` function for an argument of `type` that is not optional. */
function converter(type: Type): string {
  if (type instanceof BoolType) return type.valueKinds === "any" ? "loose_bool" : "strict_bool";
  if (type instanceof IntegerType) {
    if (isBigIntInteger(type)) return `strict_integer_or_bigint::<${type.rustName}>`;
    const integer = type.idlType.match(/^Bun::Bindgen::IDL(Strict|Loose)Integer<([^,>]+)>$/);
    if (!integer) return refuse(`${type.idlType} (an integer with a range, .wrap or .allowBigInt)`);
    return `${integer[1] === "Strict" ? "strict" : "loose"}_integer::<${type.rustName}>`;
  }
  if (type instanceof DoubleType) {
    if (type.idlType === "Bun::Bindgen::IDLStrictDouble") return "strict_f64";
    if (type.idlType === "WebCore::IDLUnrestrictedDouble") return "loose_f64";
    return refuse(type.idlType);
  }
  if (type instanceof Utf8Type) return type.isLoose ? "loose_utf8" : "strict_utf8";
  if (type instanceof OwnedUtf8Type) return type.valueKinds === "any" ? "loose_owned_utf8" : "strict_owned_utf8";
  // `.allowStringObject` made a `String` object a string already (`prepared`).
  if (type instanceof LocalStringType) return "local_string";
  // `b.any.orAbsent`: an argument that is not passed is `undefined`, as C++'s `arguments.at(i)` reads it.
  if (type instanceof AnyType) return "any";
  return refuse(`${type.constructor.name} (${type.tsType})`);
}

/** The `from_js` call that converts `value` (a `JSValue`) to an argument of `type` that is not optional. */
function converterCall(type: Type, value: string, ctx: string): string {
  const call = (name: string, ...extra: string[]) =>
    `bindgen::from_js::${name}(${["heap", `&${value}`, ctx, ...extra].join(", ")})`;
  if (type instanceof BytesType) return bytesCall(type, call);
  if (type instanceof BufferSourceType) return call("buffer_source", String(type.refuses === null));
  if (type.constructor === StringType) {
    if (type.idlType === "WebCore::IDLUSVString") refuse("b.string.usv");
    if (type.valueKinds === "any") return call("loose_string");
    return call(type.valueKinds.includes("stringObject") ? "string_or_string_object" : "strict_string");
  }
  if (type === callableType) return call("callable");
  if (type instanceof DoubleType && type.idlType === "Bun::Bindgen::IDLStrictDoubleOrDate")
    return call("strict_f64_or_date");
  return call(converter(type));
}

/** `b.bytes`: the call for what the body is handed, and what the error says it takes, as C++'s `cppConvertInto`. */
function bytesCall(type: BytesType, call: (name: string, ...extra: string[]) => string): string {
  const { from, otherwise, refuse: refused, string, shared, held } = type.options;
  if (shared === "atomic" || held === "pinned") refuse(`b.bytes with shared: "${shared}", held: "${held}"`);
  const spelling = { string: ["a string"], buffer: ["ArrayBuffer", "ArrayBufferView"], blob: ["Blob"] };
  const expected = (["string", "buffer", "blob"] as const).filter(kind => from.includes(kind)).flatMap(kind => spelling[kind]);
  if (!from.includes("string")) expected[0] = `${/^A/.test(expected[0]) ? "an" : "a"} ${expected[0]}`;
  const last = expected.pop()!;
  const list = expected.length > 0 ? `${expected.join(", ")} or ${last}` : last;
  const options = {
    string: from.includes("string"),
    buffer: from.includes("buffer"),
    blob: from.includes("blob"),
    to_string_otherwise: otherwise === "toString",
    refuse_undefined: refused.includes("undefined"),
    refuse_null: refused.includes("null"),
    refuse_string_object: refused.includes("stringObject"),
  };
  const fields = Object.entries(options).map(([name, value]) => `${name}: ${value}`);
  const name =
    string === "asIs" ? "string_or_bytes" : { borrowed: "bytes", handle: "bytes_handle", copied: "copied_bytes" }[held];
  return call(name!, `bindgen::from_js::BytesFrom { ${fields.join(", ")} }`, rustString(list));
}

/** As C++'s `cppPrepare`: whether a `String` object is made a string before anything is converted. */
const prepared = (type: Type) => type.cppPrepare("") !== null;

type Argument = Signature["args"][number];

/** What an error of the conversion names: C++'s `LiteralConversionContext`, or `ValidatorArgumentContext` for one
 * that `validatorExpects` names. `index` is a Rust expression, for an element of a `b.rest`. */
function context(arg: Pick<Argument, "name" | "validatorExpects"> & { expected?: Argument["expected"] }, index?: string): string {
  if (typeof arg.expected === "string")
    return `bindgen::from_js::Context::Message { name: ${rustString(arg.name)}, message: ${rustString(arg.expected)} }`;
  if (arg.expected !== undefined)
    return `bindgen::from_js::Context::Node { name: ${rustString(arg.name)}, expected: &[${arg.expected.map(rustString).join(", ")}] }`;
  if (arg.validatorExpects === undefined) return `bindgen::from_js::Context::Argument(${rustString(arg.name)})`;
  return `bindgen::from_js::Context::Validator { name: ${rustString(arg.name)}, expected: ${rustString(arg.validatorExpects)}, index: ${index === undefined ? "None" : `Some(${index})`} }`;
}

/** The schema whose module is being written: where a dictionary or a union of another schema is found from. */
let currentFile: SchemaFile;

/** Rust that converts `value` (a `JSValue`) to an argument of `type`, an expression of `bindgen::Result<'a, T>`. */
function convert(type: Type, value: string, ctx: string): string {
  const call = (inner: Type) =>
    inner instanceof DictionaryType || inner instanceof UnionType
      ? `${namedPath(inner, currentFile)}::from_js(heap, &${value}, ${ctx})`
      : converterCall(inner, value, ctx);
  if (type instanceof OptionalType) {
    if (type.modifier === "otherAsNull") {
      const kinds = type.inner.valueKinds;
      if (kinds === "any") return refuse(".otherAsNull of a type that takes every kind");
      const tests = kinds.map(kind => `${value}.${kindTest(kind, `.otherAsNull of ${type.inner.tsType}`)}()`);
      return `if !(${tests.join(" || ")}) { Ok(None) } else { ${call(type.inner)}.map(Some) }`;
    }
    const absent = { optional: `${value}.is_undefined()`, nullable: `${value}.is_undefined_or_null()` }[
      type.modifier as "optional" | "nullable"
    ];
    if (!absent) return refuse(`.${type.modifier}`);
    return `if ${absent} { Ok(None) } else { ${call(type.inner)}.map(Some) }`;
  }
  if (type instanceof DefaultType) {
    const absent = type.onNull ? `${value}.is_undefined_or_null()` : `${value}.is_undefined()`;
    // A byte string literal is an array: the slice is what the conversion gives.
    const fallback =
      type.inner instanceof Utf8Type ? `&${type.inner.toRust(type.value)}[..]` : type.inner.toRust(type.value);
    return `if ${absent} { Ok(${fallback}) } else { ${call(type.inner)} }`;
  }
  return call(type);
}

/** Where `type` is in the module of `file`: a dictionary of another schema is in that one's module. */
function namedPath(type: DictionaryType | UnionType, file: SchemaFile): string {
  if (type.origin!.crate !== file.origin.crate) refuse(`${type.name}: a dictionary of another crate`);
  return type.origin!.rustModule === file.origin.rustModule
    ? type.name
    : `super::${type.origin!.rustModule}::${type.name}`;
}

/** Whether what a body returns as `type` borrows from the call. A union that is returned owns its strings
 * (`ownsStrings`). */
function borrowsCall(type: Type, ownsStrings = false): boolean {
  if (type instanceof Utf8Type || type.constructor === StringType) return !ownsStrings;
  if (type instanceof LocalStringType || type instanceof AnyType) return true;
  if (type instanceof BufferSourceType || type instanceof BytesType || type === callableType) return true;
  if (type instanceof OptionalType || type instanceof DefaultType) return borrowsCall(type.inner, ownsStrings);
  if (type instanceof DictionaryType) return type.members.some(member => borrowsCall(member.type));
  if (type instanceof UnionType)
    return type.alternatives.some(alternative => borrowsCall(alternative.type, ownsStrings));
  return false;
}

/** The Rust type of a member of a dictionary or an alternative of a union. A member with a default that is taken
 * has it filled in. */
function memberRustType(type: Type, file: SchemaFile, returned = false): string {
  if (returned && (type instanceof Utf8Type || type.constructor === StringType)) return "Vec<u8>";
  if (type instanceof LocalStringType || type instanceof Utf8Type) return "&'a [u8]";
  if (type instanceof OwnedUtf8Type) return "Vec<u8>";
  if (type.constructor === StringType) return "bindgen::JsStr<'a>";
  if (type instanceof BufferSourceType) return "bindgen::JsLocal<'a, bindgen::BufferSource>";
  if (type === callableType) return "bindgen::JsLocal<'a>";
  if (type instanceof BoolType || type instanceof DoubleType) return type.rustName;
  if (type instanceof IntegerType && (isPlainInteger(type) || isBigIntInteger(type))) return type.rustName;
  if (type instanceof AnyType && type.checkedResult === null) return "bindgen::JsLocal<'a>";
  if (type instanceof OptionalType && (type.modifier === "optional" || type.modifier === "nullable"))
    return `Option<${memberRustType(type.inner, file)}>`;
  if (type instanceof DefaultType) return memberRustType(type.inner, file);
  if (type instanceof DictionaryType || type instanceof UnionType)
    return `${namedPath(type, file)}${borrowsCall(type) ? "<'a>" : ""}`;
  return refuse(`a member of a dictionary of ${type.constructor.name} (${type.tsType})`);
}

/** The value of a member that is absent, for `Default`: its `.default()`, or `None`. */
function absentValue(type: Type): string {
  if (type instanceof DefaultType)
    return type.inner instanceof Utf8Type ? `&${type.inner.toRust(type.value)}[..]` : type.inner.toRust(type.value);
  if (type instanceof OptionalType) return "None";
  if (type instanceof AnyType) return "bindgen::JsLocal::UNDEFINED";
  return refuse(`a default of ${type.constructor.name} (${type.tsType})`);
}

/** Rust that converts `value` (what a body returns) to JS, an expression of `bindgen::Result<'a, JsLocal<'a>>`. */
function toJs(type: Type, value: string): string {
  if (type instanceof OptionalType) {
    if (type.modifier !== "nullable" && type.modifier !== "optional") return refuse(`a result of .${type.modifier}`);
    const absent = type.modifier === "nullable" ? "bun_js::JSValue::NULL" : "bun_js::JSValue::UNDEFINED";
    return `match ${value} { Some(value) => ${toJs(type.inner, "value")}, None => Ok(bindgen::JsLocal::new(heap, ${absent})) }`;
  }
  if (type instanceof UnitType) {
    if (type.unit !== "undefined") return refuse("a result of b.null");
    return `bindgen::to_js::undefined(heap, ${value})`;
  }
  if (type instanceof BoolType || type instanceof DoubleType) return `bindgen::to_js::${type.rustName}(heap, ${value})`;
  if (type instanceof IntegerType) {
    if (!isPlainInteger(type)) return refuse(`a result of ${type.idlType}`);
    return `bindgen::to_js::${type.rustName}(heap, ${value})`;
  }
  if (type instanceof Utf8Type || type instanceof LocalStringType) return `bindgen::to_js::utf8(heap, ${value})`;
  // A body returns a string as its UTF-8.
  if (type.constructor === StringType) return `bindgen::to_js::utf8(heap, ${value})`;
  if (type instanceof BigIntResultType) return `bindgen::to_js::u64_as_bigint(heap, ${value})`;
  if (type instanceof BufferSourceType) return `bindgen::to_js::any(heap, ${value})`;
  if (type instanceof ArrayType)
    return `bindgen::to_js::array(heap, ${value}, |heap, value| ${toJs(type.element, "value")})`;
  if (type instanceof UnionType) return `${value}.into_js(heap)`;
  // The C++ build holds a result to the words of `.returning` for compiled code. Boa compiles none: no one reads them.
  if (type instanceof AnyType) return `bindgen::to_js::any(heap, ${value})`;
  if (type instanceof DictionaryType) return `${value}.into_js(heap)`;
  // The body returns the payload of a new instance.
  if (type instanceof ClassType) return `bindgen::to_js::class::<${type.rustPath}>(heap, ${value})`;
  return refuse(`a result of ${type.constructor.name} (${type.tsType})`);
}

// ───────────────────────────── calls ─────────────────────────────

const capabilityArgument = { none: [], heap: ["&cx.heap"], js: ["cx"] } as const;

/** What a host function returns for the body's `result`. */
const returnConverted = (signature: Signature) => (result: string) =>
  `bindgen::host_fn_return(cx, ${result}, |heap, value| ${toJs(signature.returns, "value")})`;

const orReturn = (expression: string) =>
  `match ${expression} { Ok(value) => value, Err(error) => return error.into_host_fn_return(cx) }`;

/** As C++'s `cppDeclareArgument`: `argument<i>`, the argument as the frame has it, or the rest of them, with a
 * `String` object made a string where the type says so. Every argument is declared, in order, before any is converted. */
function declareArgument({ type }: Argument, i: number): string[] {
  // `b.any.variadic`: the body takes the rest of the arguments as they are, `&[JSValue]`.
  if (type instanceof VariadicType) return [`let argument${i} = frame.arguments().get(${i}..).unwrap_or_default();`];
  if (type instanceof RestType) {
    const values = `frame.arguments().get(${i}..).unwrap_or_default()`;
    if (!prepared(type.element)) return [`let argument${i} = ${values};`];
    return [
      `let mut argument${i} = ${values}.to_vec();`,
      `for value in &mut argument${i} { *value = ${orReturn("bindgen::from_js::string_of_string_object(heap, value)")}; }`,
    ];
  }
  if (prepared(type))
    return [
      `let argument${i} = ${orReturn(`bindgen::from_js::string_of_string_object(heap, &frame.argument(${i}))`)};`,
    ];
  return [`let argument${i} = frame.argument(${i});`];
}

/** The statements that convert a signature's arguments from `frame`, then run `beforeCall` (which borrows a class's
 * payload, once nothing else can run script), then `call` with them, then `finish`. */
function body(
  signature: Signature,
  call: (args: string[]) => string,
  finish: (result: string) => string = returnConverted(signature),
  beforeCall: string[] = [],
): string {
  if (signature.rejects) refuse("rejects");
  const lines: string[] = ["let heap = &cx.heap;"];
  if (signature.requiredArguments > 0) {
    // As bindgen-emit.ts's `cppThrowIfArgumentsMissing`: the message names every required argument.
    const names = signature.args.slice(0, signature.requiredArguments).map(arg => `"${arg.name}"`);
    const message = `The ${new Intl.ListFormat("en").format(names)} argument${names.length > 1 ? "s" : ""} must be specified`;
    lines.push(
      `if frame.arguments_count() < ${signature.requiredArguments} { return bindgen::from_js::missing_arguments(heap, ${rustString(message)}).into_host_fn_return(cx); }`,
    );
  }
  const names = signature.args.map(arg => ident(snakeCase(arg.name)));
  signature.args.forEach((arg, i) => lines.push(...declareArgument(arg, i)));
  for (const i of signature.conversionOrder) {
    const arg = signature.args[i];
    if (arg.type instanceof VariadicType) {
      lines.push(`let ${names[i]} = argument${i};`);
      continue;
    }
    if (arg.type instanceof RestType) {
      lines.push(
        `let mut ${names[i]} = Vec::with_capacity(argument${i}.len());`,
        `for (rest_index, rest_value) in argument${i}.iter().enumerate() { ${names[i]}.push(${orReturn(convert(arg.type.element, "(*rest_value)", context(arg, "rest_index")))}); }`,
      );
      continue;
    }
    // `.ifPassed`: absent only where the call has no such argument; an `undefined` that is passed is converted.
    if (arg.type instanceof OptionalType && arg.type.modifier === "ifPassed") {
      const converted = convert(arg.type.inner, `argument${i}`, context(arg));
      lines.push(
        `let ${names[i]} = if frame.arguments_count() <= ${i} { None } else { Some(${orReturn(converted)}) };`,
      );
      continue;
    }
    lines.push(`let ${names[i]} = ${orReturn(convert(arg.type, `argument${i}`, context(arg)))};`);
  }
  lines.push(...beforeCall);
  const passed = signature.args.map((arg, i) => (arg.type instanceof RestType ? `&${names[i]}` : names[i]));
  const invoked = call([...capabilityArgument[signature.capability], ...passed]);
  const result = signature.throws
    ? invoked
    : signature.returns instanceof UnitType && !(signature instanceof Construction)
      ? `{ ${invoked}; Ok::<_, core::convert::Infallible>(()) }`
      : `Ok::<_, core::convert::Infallible>(${invoked})`;
  lines.push(`let result = ${result};`);
  lines.push(finish("result"));
  return lines.join("\n");
}

const scoped = (global: string, statements: string) =>
  `bindgen::JsScope::with(${global}, |cx| {\n${indent(statements, 4)}\n})`;

function indent(text: string, by: number): string {
  const pad = " ".repeat(by);
  return text
    .split("\n")
    .map(line => (line ? pad + line : line))
    .join("\n");
}

function onlySignature(fn: FunctionDefinition, what: string): Signature {
  if (fn.signatures.length !== 1) refuse(`${what}: b.overloads`);
  const signature = fn.signatures[0];
  if (fn.passThis || fn.passCallee) refuse(`${what}: passThis and passCallee`);
  return signature;
}

/** What tells a value of `kind` apart: a `JSValue` method. */
function kindTest(kind: string | { cpp: string }, what: string): string {
  if (typeof kind === "object") {
    if (kind.cpp === "JSC::DateInstance") return "is_date";
    return refuse(`${what}: telling apart a value of kind ${kind.cpp}`);
  }
  return KIND_TESTS[kind] ?? refuse(`${what}: telling apart a value of kind ${kind}`);
}

/** Rust that runs the `leaf` of the signature `dispatch` picks by the arguments' kinds ("Overloads" in
 * docs/project/bindgen.mdx). Choosing converts nothing and runs no script. */
function rustDispatch(dispatch: OverloadDispatch, leaf: (signature: Signature) => string): string {
  if (dispatch instanceof Signature) return `{\n${indent(leaf(dispatch), 4)}\n}`;
  if ("passed" in dispatch)
    return `if frame.arguments_count() <= ${dispatch.argument} ${rustDispatch(dispatch.notPassed, leaf)} else ${rustDispatch(dispatch.passed, leaf)}`;
  const choice = `choice${dispatch.argument}`;
  const branches: string[] = [];
  let other: OverloadDispatch | undefined;
  for (const [kind, next] of dispatch.byKind) {
    if (kind === "other") {
      other = next;
      continue;
    }
    branches.push(`if ${choice}.${kindTest(kind, "an overload")}() ${rustDispatch(next, leaf)}`);
  }
  if (other === undefined) return refuse("overloads that no value of another kind reaches");
  return `{\nlet ${choice} = frame.argument(${dispatch.argument});\n${[...branches, rustDispatch(other, leaf)].join(" else ")}\n}`;
}

/** Each signature's own body, or one, behind the choice of an overload. */
function overloaded(fn: FunctionDefinition, leaf: (signature: Signature) => string): string {
  return fn.signatures.length === 1 ? leaf(fn.signatures[0]) : rustDispatch(fn.dispatch, leaf);
}

function rustForFunction(fn: FunctionDefinition, file: SchemaFile): string {
  if (fn.passThis || fn.passCallee) refuse(`${fn.name}: passThis and passCallee`);
  const modulePath = file.moduleDefinition?.rustPath;
  const targetOf = (signature: Signature) => {
    // A body that the JavaScriptCore build has in C++ (`cppSymbol`) is a port in Rust here, at the module's rustPath.
    if (signature.cppSymbol && !signature.rustPath && !modulePath) refuse(`${fn.name}: cppSymbol without a rustPath`);
    return (
      signature.rustPath ??
      (modulePath ? `${modulePath}::${ident(snakeCase(fn.name))}` : refuse(`${fn.name}: a rustPath`))
    );
  };
  const host = ident(snakeCase(fn.name));
  // As C++'s `createFunctionNamedAsBound`: `name` is `bound <jsName>`, and it prints and is traced as `<jsName>`.
  const create = fn.namedAsBound ? "create_named_as_bound" : "create";
  return `
/// \`${fn.jsName}\`
pub fn ${host}(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
${indent(
  scoped(
    "global",
    overloaded(fn, signature => body(signature, args => `${targetOf(signature)}(${args.join(", ")})`)),
  ),
  4,
)}
}

/// The function object of \`${fn.jsName}\`.
pub fn create_${snakeCase(fn.name)}(global: &JSGlobalObject) -> JSValue {
    bun_js::JSFunction::${create}(global, ${rustString(fn.jsName)}, ${host}, ${fn.length})
}
`;
}

/** `b.value`: no function, the value the body makes, each time `$bindgen` asks for it. */
function rustForValue(fn: FunctionDefinition, file: SchemaFile): string {
  const signature = onlySignature(fn, fn.name);
  if (signature.args.length) refuse(`${fn.name}: a b.value with arguments`);
  const modulePath = file.moduleDefinition?.rustPath;
  const target =
    signature.rustPath ??
    (modulePath ? `${modulePath}::${ident(snakeCase(fn.name))}` : refuse(`${fn.name}: a rustPath`));
  return `
/// \`$bindgen(file, ${rustString(fn.name)})\`: the value of \`${fn.name}\`.
pub fn ${ident(snakeCase(fn.name))}(global: &JSGlobalObject) -> JsResult<JSValue> {
${indent(
  scoped(
    "global",
    body(signature, args => `${target}(${args.join(", ")})`),
  ),
  4,
)}
}
`;
}

function rustForNamespace(namespace: NamespaceDefinition): string {
  const puts = namespace.functions
    // As C++'s namespace object: each function is named by its key, not its `jsName`, and none is named as bound.
    .map(
      fn =>
        `object.put(global, ${rustString(fn.name)}, bun_js::JSFunction::create(global, ${rustString(fn.name)}, ${ident(snakeCase(fn.name))}, ${fn.length}));`,
    )
    .join("\n");
  // A namespace with `call` is that function, named after the namespace, with the others as its properties.
  const object = namespace.call
    ? `bun_js::JSFunction::create(global, ${rustString(namespace.name)}, ${ident(snakeCase(namespace.call.name))}, ${namespace.call.length})`
    : "bun_js::JSObject::create(global).to_js()";
  return `
/// The namespace \`${namespace.name}\`.
pub fn create_${snakeCase(namespace.name)}(global: &JSGlobalObject) -> JSValue {
    let object = ${object};
${indent(puts, 4)}
    object
}
`;
}

// ───────────────────────────── dictionaries ─────────────────────────────

/** A dictionary that a body returns: a struct, and `into_js`, which makes the object as C++ does, a new ordinary one
 * with each member in order. */
function rustForDictionary(type: DictionaryType, file: SchemaFile): string {
  const what = `dictionary ${type.name}`;
  const unsupported = (
    [
      ["fromJS", type.fromJS],
      ["allowArray", type.options.allowArray],
      ["nonObjectAsEmpty", type.nonObjectAsEmpty],
      ["nullPrototype", type.nullPrototype],
      ["webIDL", type.webIDL],
    ] as const
  ).filter(([, value]) => value);
  if (unsupported.length) refuse(`${what}: ${unsupported.map(([key]) => key).join(", ")}`);
  const lifetime = borrowsCall(type) ? "<'a>" : "";
  const fields = type.members.map(member => `pub ${member.rustName}: ${memberRustType(member.type, file)},`);
  // As C++: a member that is `.optional` and absent is left out, one that is `.nullable` is `null`.
  const puts = type.members.map(member => {
    const put = (memberType: Type) =>
      `object.value().put(global, ${rustString(member.name)}, ${toJs(memberType, "value")}?.to_js());`;
    if (member.type instanceof OptionalType && member.type.modifier === "optional")
      return `if let Some(value) = self.${member.rustName} { ${put(member.type.inner)} }`;
    return `{ let value = self.${member.rustName}; ${put(member.type)} }`;
  });
  return `
/// \`${type.name}\`, as a body returns it.
pub struct ${type.name}${lifetime} {
${indent(fields.join("\n"), 4)}
}

impl${lifetime ? "<'a>" : ""} ${type.name}${lifetime} {
    /// A new object with these members, in this order.
    pub fn into_js${lifetime ? "" : "<'a>"}(self, heap: &bindgen::JsHeap<'a>) -> bindgen::Result<'a, bindgen::JsLocal<'a>> {
        let global = heap.global_object();
        let object = heap.create_empty_object();
${indent(puts.join("\n"), 8)}
        Ok(object)
    }
}
`;
}

/** A dictionary that a body takes: a struct, `Default` where every member can be absent, and `from_js`, which reads
 * it as C++'s `convertDictionaryInto`: each member in order, from the object and its prototypes short of
 * `Object.prototype`. */
function rustForAcceptedDictionary(type: DictionaryType, file: SchemaFile): string {
  const what = `dictionary ${type.name}`;
  const unsupported = (
    [
      ["nonObjectAsEmpty", type.nonObjectAsEmpty],
      ["webIDL", type.webIDL],
    ] as const
  ).filter(([, value]) => value);
  if (unsupported.length) refuse(`${what}: ${unsupported.map(([key]) => key).join(", ")}`);
  for (const member of type.members)
    if (member.expected !== undefined) refuse(`${what}.${member.name}: expected (Node's ERR_INVALID_ARG_TYPE text)`);
  const lifetime = borrowsCall(type) ? "<'a>" : "";
  const fields = type.members.map(member => `pub ${member.rustName}: ${memberRustType(member.type, file)},`);
  const reads = type.members.map((member, i) => {
    const qualified = type.reportedAs ? `${type.reportedAs}.${member.name}` : member.name;
    const ctx = `bindgen::from_js::Context::Argument(${rustString(qualified)})`;
    const [first, ...others] = member.names;
    const lines = [
      `let ${others.length ? "mut " : ""}value${i} = bindgen::from_js::dictionary_member(heap, &object, ${rustString(first)})?;`,
    ];
    for (const other of others)
      lines.push(
        `if value${i}.is_undefined() { value${i} = bindgen::from_js::dictionary_member(heap, &object, ${rustString(other)})?; }`,
      );
    if (!member.type.takesUndefined)
      lines.push(`if value${i}.is_undefined() { return Err(bindgen::from_js::required(heap, ${ctx})); }`);
    lines.push(`let ${member.rustName} = (${convert(member.type, `value${i}`, ctx)})?;`);
    return lines.join("\n");
  });
  const defaults = type.hasDefault
    ? `
impl${lifetime} Default for ${type.name}${lifetime} {
    fn default() -> Self {
        Self {
${indent(type.members.map(member => `${member.rustName}: ${absentValue(member.type)},`).join("\n"), 12)}
        }
    }
}
`
    : "";
  return `
/// \`${type.name}\`, as a body takes it.
pub struct ${type.name}${lifetime} {
${indent(fields.join("\n"), 4)}
}
${defaults}
impl<'a> ${type.name}${lifetime} {
    /// \`value\` as this dictionary. It names itself in an error, as C++'s does: \`_ctx\` names the argument.
    pub fn from_js(heap: &bindgen::JsHeap<'a>, value: &JSValue, _ctx: bindgen::from_js::Context<'_>) -> bindgen::Result<'a, Self> {
        let object = bindgen::from_js::dictionary_object(heap, value, ${rustString(type.reportedAs || type.name)}, ${type.allowArray})?;
${indent(reads.join("\n"), 8)}
        Ok(Self { ${type.members.map(member => member.rustName).join(", ")} })
    }
}
`;
}

/** What `value.is_<kind>()` tells a union's alternatives apart by. */
const KIND_TESTS: Record<string, string> = {
  undefined: "is_undefined",
  null: "is_null",
  boolean: "is_boolean",
  number: "is_number",
  bigint: "is_big_int",
  string: "is_string",
  symbol: "is_symbol",
  stringObject: "is_string_object",
  array: "is_array",
  arrayBuffer: "is_array_buffer",
  arrayBufferView: "is_array_buffer_view",
  callable: "is_callable",
};

/** A union that a body takes: an enum, and `from_js`, which hands a value to the first alternative that takes its
 * kind, and what no other takes to the one that converts it. Only a union where each kind goes to one alternative. */
function rustForAcceptedUnion(type: UnionType, file: SchemaFile): string {
  const what = `union ${type.name}`;
  const lifetime = borrowsCall(type) ? "<'a>" : "";
  const variants = type.alternatives.map(a =>
    a.type instanceof UnitType ? `${a.variant},` : `${a.variant}(${memberRustType(a.type, file)}),`,
  );
  const branch = (index: number) => {
    const alternative = type.alternatives[index];
    if (alternative.type instanceof UnitType) return `Ok(Self::${alternative.variant})`;
    const ctx = `bindgen::from_js::Context::Argument(${rustString(alternative.name)})`;
    return `(${convert(alternative.type, "(*value)", ctx)}).map(Self::${alternative.variant})`;
  };
  const tests: string[] = [];
  let other: number | undefined;
  // What no alternative takes is refused, as a value of the wrong type.
  let noneTakes = false;
  for (const [kind, list] of type.alternativesByKind) {
    if (kind === "other" && list.length === 0) {
      noneTakes = true;
      continue;
    }
    if (list.length !== 1) refuse(`${what}: a value of kind ${String(kind)} that one alternative may pass to the next`);
    if (kind === "other") {
      other = list[0];
      continue;
    }
    const test = kindTest(kind, what);
    tests.push(`if value.${test}() { return ${branch(list[0])}; }`);
  }
  if (other === undefined && !noneTakes) refuse(`${what}: a value no alternative takes`);
  const last =
    other === undefined
      ? `Err(bindgen::from_js::invalid_arg_type(heap, _ctx.name(), ${rustString(
          [...type.alternativesByKind.keys()].filter(kind => typeof kind === "string" && kind !== "other").join(" or "),
        )}, value))`
      : branch(other);
  return `
/// \`${type.name}\`, as a body takes it.
pub enum ${type.name}${lifetime} {
${indent(variants.join("\n"), 4)}
}

impl<'a> ${type.name}${lifetime} {
    /// \`value\` as the alternative that takes it.
    pub fn from_js(heap: &bindgen::JsHeap<'a>, value: &JSValue, _ctx: bindgen::from_js::Context<'_>) -> bindgen::Result<'a, Self> {
${indent(tests.join("\n"), 8)}
        ${last}
    }
}
`;
}

/** A union that a body returns: an enum, and `into_js`, which makes the value of the alternative it holds. It owns
 * its strings, as UTF-8. */
function rustForReturnedUnion(type: UnionType, file: SchemaFile): string {
  const lifetime = borrowsCall(type, true) ? "<'a>" : "";
  const variants = type.alternatives.map(a =>
    a.type instanceof UnitType ? `${a.variant},` : `${a.variant}(${memberRustType(a.type, file, true)}),`,
  );
  const arms = type.alternatives.map(a =>
    a.type instanceof UnitType
      ? `Self::${a.variant} => ${toJs(a.type, "()")},`
      : `Self::${a.variant}(value) => ${toJs(a.type, "value")},`,
  );
  return `
/// \`${type.name}\`, as a body returns it.
pub enum ${type.name}${lifetime} {
${indent(variants.join("\n"), 4)}
}

impl${lifetime ? "<'a>" : ""} ${type.name}${lifetime} {
    /// The value of the alternative it holds.
    pub fn into_js${lifetime ? "" : "<'a>"}(self, heap: &bindgen::JsHeap<'a>) -> bindgen::Result<'a, bindgen::JsLocal<'a>> {
        match self {
${indent(arms.join("\n"), 12)}
        }
    }
}
`;
}

// ───────────────────────────── classes ─────────────────────────────

function rustForClass(type: ClassType): string {
  const what = `class ${type.name}`;
  const unsupported = (
    [
      ["extends", type.base],
      ["refCounted", type.refCounted],
      ["passStructure", type.passStructure],
      ["webIDL", type.webIDL],
      ["call", type.call],
      ["callInstance", type.callInstance],
      ["internalFields", type.internalFields.length],
      ["staticGetters", type.staticGetters.length],
      ["calledFromCpp", type.calledFromCpp.length],
      ["calledFromRust", type.calledFromRust.length],
      ["protoCaller", type.protoCaller],
      ["prototypeBase", type.prototypeBase],
      ["constants", type.constants.length],
      ["finalize", type.finalize],
      ["thisValue", type.thisValue],
      ["forBind", type.forBind],
      ["overridesToJS", type.overridesToJS],
    ] as const
  ).filter(([, value]) => value);
  if (unsupported.length) refuse(`${what}: ${unsupported.map(([key]) => key).join(", ")}`);
  const rustPath = type.rustPath;
  const members: string[] = [];
  const attributes = (enumerable = type.enumerable, configurable = type.configurable) =>
    `bun_js::class::Attributes { writable: ${type.writable}, enumerable: ${enumerable}, configurable: ${configurable} }`;
  const named = (name: string) => {
    // A key such as `@@iterator` is a well-known symbol in C++, which this emitter does not define yet.
    if (name.startsWith("@@")) refuse(`${what}.${name}: a symbol-keyed member`);
    return rustString(name);
  };
  /** Borrows the payload as the member's receiver wants it: after every conversion, for the body alone. */
  const borrow = (receiver: string) =>
    receiver === "mut"
      ? ["let mut this = this.borrow_mut(global)?;", "let this = &mut *this;"]
      : ["let this = this.borrow(global)?;", "let this = &*this;"];

  for (const fn of [...type.functions, ...type.staticFunctions])
    if (fn.namedAsBound) refuse(`${what}.${fn.name}: namedAsBound`);

  for (const fn of type.functions) {
    const method = onlySignature(fn, `${type.name}.${fn.name}`) as Method;
    if (method.passThis || method.answersThis || method.functionName)
      refuse(`${what}.${fn.name}: passThis, answersThis, functionName`);
    if (method.invalidThis !== "throw") refuse(`${what}.${fn.name}: invalidThis "${method.invalidThis}"`);
    const call = (args: string[]) => `<${rustPath}>::${method.rustName}(${["this", ...args].join(", ")})`;
    const statements = body(method, call, returnConverted(method), borrow(method.receiver));
    const host = `|this, global, frame| ${scoped("global", statements)}`;
    // As C++'s: keyed by the private name (`this.$globScan` in a builtin), read-only and not enumerable.
    members.push(
      method.privateSymbol
        ? `class.private_method(${rustString(method.privateSymbol)}, ${named(fn.name)}, ${fn.length}, ${host})?;`
        : `class.method(${named(fn.name)}, ${fn.length}, ${attributes()}, ${host})?;`,
    );
  }

  for (const builtin of type.builtins) {
    const source = builtinSources.get(`${type.name}.${builtin.name}`);
    if (source === undefined) throw new Error(`${what}.${builtin.name}: its builtin source was not loaded`);
    members.push(
      `class.builtin(${named(builtin.name)}, ${builtin.length}, ${attributes()}, ${rustString(source)})?;`,
    );
  }

  for (const fn of type.staticFunctions) {
    const method = onlySignature(fn, `${type.name}.${fn.name}`) as Method;
    const call = (args: string[]) => `<${rustPath}>::${method.rustName}(${args.join(", ")})`;
    members.push(
      `class.static_method(${named(fn.name)}, ${fn.length}, ${attributes()}, |global, frame| ${scoped("global", body(method, call))})?;`,
    );
  }

  for (const getter of type.getters) {
    named(getter.name);
    members.push(rustForGetter(type, getter, attributes(getter.enumerable, getter.configurable), borrow));
  }

  const [construction, ...more] = type.illegalConstructor ? [] : type.constructions;
  if (more.length) refuse(`${what}: overloaded constructors`);
  const construct = construction
    ? `
    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
${indent(
  scoped(
    "global",
    body(
      construction,
      args => `<${rustPath}>::${construction.rustName}(${args.join(", ")})`,
      result => `bindgen::host_fn_construct_return(cx, ${result})`,
    ),
  ),
  8,
)}
    }
`
    : "";

  return `
impl bun_js::JsClass for ${rustPath} {
    const NAME: &'static str = ${rustString(type.name)};
    const LENGTH: usize = ${type.construct?.length ?? 0};
${construct}
    fn define(class: &mut bun_js::class::ClassBuilder<'_, Self>) -> JsResult<()> {
${indent(members.join("\n"), 8)}
        Ok(())
    }
}
`;
}

function rustForGetter(
  type: ClassType,
  getter: Getter,
  attributes: string,
  borrow: (receiver: string) => string[],
): string {
  const what = `${type.name}.${getter.name}`;
  if (getter.internalField || getter.passThis || getter.isStatic) refuse(`${what}: cache, passThis, static`);
  const capability = (c: keyof typeof capabilityArgument) => capabilityArgument[c];
  const get = `<${type.rustPath}>::${getter.rustName}(${["this", ...capability(getter.capability)].join(", ")})`;
  // The borrow lasts until the result is converted: what a getter returns may be a slice of the payload.
  const getBody = [
    "let heap = &cx.heap;",
    ...borrow(getter.receiver),
    `let result = ${getter.throws ? get : `Ok::<_, core::convert::Infallible>(${get})`};`,
    `bindgen::host_fn_return(cx, result, |heap, value| ${toJs(getter.type, "value")})`,
  ].join("\n");
  const getterClosure = `|this, global| ${scoped("global", getBody)}`;
  if (!getter.hasSetter) return `class.getter(${rustString(getter.name)}, ${attributes}, ${getterClosure})?;`;
  if (prepared(getter.type)) refuse(`${what}: a setter of a type that makes a String object a string first`);
  const set = `<${type.rustPath}>::${getter.setterRustName}(${["this", ...capability(getter.setterCapability), "value"].join(", ")})`;
  const setBody = [
    "let heap = &cx.heap;",
    `let value = ${orReturn(convert(getter.type, "value", context({ name: getter.name })))};`,
    ...borrow(getter.setterReceiver),
    `let result = ${getter.throws ? set : `{ ${set}; Ok::<_, core::convert::Infallible>(()) }`};`,
    "bindgen::host_fn_return(cx, result, bindgen::to_js::undefined).map(drop)",
  ].join("\n");
  return `class.accessor(${rustString(getter.name)}, ${attributes}, ${getterClosure}, |this, global, value| ${scoped("global", setBody)})?;`;
}

// ───────────────────────────── files ─────────────────────────────

/** The named types an argument reaches, and those a result reaches: as bindgen-emit.ts's `analyze`. */
function directions(files: readonly SchemaFile[]) {
  const accepted = new Set<NamedType>();
  const returned = new Set<NamedType>();
  const walk = (type: Type, into: Set<NamedType>, seen = new Set<Type>()) => {
    if (seen.has(type)) return;
    seen.add(type);
    if (type instanceof NamedType) into.add(type);
    for (const dependency of type.dependencies) walk(dependency, into, seen);
  };
  for (const file of files) {
    const signatures: Signature[] = file.functions.flatMap(fn => fn.signatures);
    for (const type of file.types) {
      if (type instanceof DictionaryType && type.fromJS) walk(type, accepted);
      if (!(type instanceof ClassType)) continue;
      signatures.push(...type.constructions, ...type.methods);
      for (const getter of [...type.getters, ...type.staticGetters]) {
        walk(getter.type, returned);
        if (getter.hasSetter) walk(getter.type, accepted);
      }
    }
    for (const signature of signatures) {
      for (const arg of signature.args) walk(arg.type, accepted);
      walk(signature.returns, returned);
    }
  }
  return { accepted, returned };
}
let direction: ReturnType<typeof directions>;

function rustForFile(file: SchemaFile): string {
  currentFile = file;
  const items: string[] = [];
  const exports: string[] = [];
  /** `symbol => value` of `native`: what C++'s `js2native_bindgen_<stem>_<symbol>` answers. */
  const natives: string[] = [];
  for (const type of file.types) {
    if (type instanceof DictionaryType || type instanceof UnionType) {
      const taken = direction.accepted.has(type);
      if (taken && direction.returned.has(type)) refuse(`${type.name}: a type that is both taken and returned`);
      if (type instanceof UnionType) items.push(taken ? rustForAcceptedUnion(type, file) : rustForReturnedUnion(type, file));
      else items.push(taken ? rustForAcceptedDictionary(type, file) : rustForDictionary(type, file));
      continue;
    }
    if (!(type instanceof ClassType)) refuse(`${type.constructor.name} ${type.name}`);
    items.push(rustForClass(type));
    // As C++'s `$bindgen`: a class with `construct`, an illegal one included, has its constructor exported.
    if (type.construct) {
      exports.push(`(${rustString(type.name)}, bun_js::class::constructor::<${type.rustPath}>(global)?),`);
      natives.push(`${rustString(type.name)} => bun_js::class::constructor::<${type.rustPath}>(global),`);
    }
  }
  // As C++'s `$bindgen(file, name)`: every function by its key, those of a namespace too.
  for (const fn of file.functions) {
    if (MODULE_ITEMS.has(snakeCase(fn.name)))
      throw new Error(`${fn.name}: the generated module has an item of that name`);
    if (fn.isValue) {
      items.push(rustForValue(fn, file));
      exports.push(`(${rustString(fn.name)}, ${ident(snakeCase(fn.name))}(global)?),`);
      natives.push(`${rustString(fn.name)} => return Some(${ident(snakeCase(fn.name))}(global)),`);
      continue;
    }
    items.push(rustForFunction(fn, file));
    exports.push(`(${rustString(fn.name)}, create_${snakeCase(fn.name)}(global)),`);
    natives.push(`${rustString(fn.name)} => Ok(create_${snakeCase(fn.name)}(global)),`);
  }
  const namespaces: string[] = [];
  for (const namespace of file.namespaces) {
    items.push(rustForNamespace(namespace));
    namespaces.push(`(${rustString(namespace.name)}, create_${snakeCase(namespace.name)}(global)),`);
    natives.push(`${rustString(namespace.name)} => Ok(create_${snakeCase(namespace.name)}(global)),`);
  }
  return `
/// \`${file.origin.stem}.bind.ts\`
#[allow(non_snake_case, unused_variables, clippy::redundant_closure, clippy::needless_return)]
pub mod ${file.origin.rustModule} {
    use bun_js::{CallFrame, JSGlobalObject, JSValue, JsResult, bindgen};
${indent(items.join(""), 4)}
    /// What \`$bindgen(file, name)\` answers in the JavaScriptCore build: each function and class constructor by name.
    pub fn exports(global: &JSGlobalObject) -> JsResult<Vec<(&'static str, JSValue)>> {
        Ok(vec![
${indent(exports.join("\n"), 12)}
        ])
    }

    /// The namespace objects, by name, which the embedder puts where JavaScript finds them (\`Bun.semver\`).
    pub fn namespaces(global: &JSGlobalObject) -> JsResult<Vec<(&'static str, JSValue)>> {
        Ok(vec![
${indent(namespaces.join("\n"), 12)}
        ])
    }

    /// What \`$bindgen("${file.origin.stem}.bind.ts", symbol)\` answers: the function, namespace or class constructor the
    /// schema exports as \`symbol\`, made anew each time, as C++ does. \`None\` for a symbol it does not export.
    pub fn native(global: &JSGlobalObject, symbol: &str) -> Option<JsResult<JSValue>> {
        Some(match symbol {
${indent(natives.join("\n"), 12)}
            _ => return None,
        })
    }
}
`;
}

/** As bindgen.ts: a type or an `exportName` belongs to one schema. */
function assertOwnersUnique(files: readonly SchemaFile[]) {
  const owners = new Map<string, string>();
  const own = (name: string, file: SchemaFile, what: string) => {
    const owner = owners.get(name);
    if (owner !== undefined) throw new Error(`${what} ${name} is in ${owner}.bind.ts and ${file.origin.stem}.bind.ts`);
    owners.set(name, file.origin.stem);
  };
  for (const file of files) {
    for (const { name } of file.types) own(name, file, "type");
    for (const { exportName } of file.functions) if (exportName) own(exportName, file, "export");
  }
}

export function rustForCrate(files: readonly SchemaFile[]): string {
  assertOwnersUnique(files);
  direction = directions(files);
  for (const file of files)
    if (file.origin.rustModule === "native")
      throw new Error(`${file.origin.stem}.bind.ts: its module would be named native`);
  const modules = files
    .map(file => {
      try {
        return rustForFile(file);
      } catch (error) {
        if (error instanceof Unsupported) throw new Error(`${file.origin.stem}.bind.ts: ${error.message}`);
        throw error;
      }
    })
    .join("");
  const byFile = files.map(
    file => `${rustString(`${file.origin.stem}.bind.ts`)} => ${file.origin.rustModule}::native(global, symbol),`,
  );
  return `// Generated by src/codegen/bindgen-js.ts. Do not edit.
${modules}
/// What \`$bindgen(file, symbol)\` answers: \`file\` is the schema's file name (\`node_path.bind.ts\`), \`symbol\` what it
/// exports. See each module's \`native\`.
pub fn native(global: &bun_js::JSGlobalObject, file: &str, symbol: &str) -> Option<bun_js::JsResult<bun_js::JSValue>> {
    match file {
${indent(byFile.join("\n"), 8)}
        _ => None,
    }
}
`;
}

/** `<class>.<name>` → the JavaScript of a class's builtin, which `ClassBuilder::builtin` evaluates. */
const builtinSources = new Map<string, string>();

/** Each builtin of the classes in `files`: the export of its name in src/js/builtins/<class>.ts, as Bun transpiles
 * it, with `$name` as `__intrinsic__name`, the property a private name is on Boa. A builtin that reads an intrinsic
 * that is not a member (`$isObject(x)`) is refused: nothing defines those for it. */
export async function loadBuiltinSources(files: readonly SchemaFile[]) {
  for (const file of files)
    for (const type of file.types) {
      if (!(type instanceof ClassType) || type.builtins.length === 0) continue;
      const module = await import(path.join(import.meta.dir, "../js/builtins", `${type.name}.ts`));
      for (const builtin of type.builtins) {
        const fn = module[builtin.name];
        if (typeof fn !== "function") throw new Error(`src/js/builtins/${type.name}.ts: no export ${builtin.name}`);
        const source = ` ${fn.toString()}`.replace(/([^a-zA-Z0-9_$])\$([a-zA-Z0-9_]+\b)/g, "$1__intrinsic__$2").slice(1);
        const free = source.match(/(?<![.\w$])__intrinsic__\w+/);
        if (free) refuse(`${type.name}.${builtin.name}: a builtin that reads ${free[0].replace("__intrinsic__", "$")}`);
        builtinSources.set(`${type.name}.${builtin.name}`, source);
      }
    }
}

if (import.meta.main) {
  const args = argParse(["sources", "codegen-path"]);
  const sources = (args.sources as string).split(",").filter(Boolean).sort();
  const files: SchemaFile[] = [];
  for (const source of sources) files.push(await loadSchemaFile(source));
  files.forEach(sortTypes);
  await loadBuiltinSources(files);
  for (const [crate, crateFiles] of Map.groupBy(files, file => file.origin.crate)) {
    writeIfNotChanged(path.join(args["codegen-path"], `generated_bun_js_${crate}.rs`), rustForCrate(crateFiles));
  }
}
