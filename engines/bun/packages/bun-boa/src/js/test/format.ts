// How bun:test prints values: `show()` is the console's formatter (src/jsc/ConsoleObject.rs) as expect()'s messages
// use it (`make_formatter` in expect.rs), and `pretty()` is the snapshot format (src/runtime/test_runner/pretty_format.rs),
// which diffs and snapshots use. Both are ports: their line breaking follows the same estimated line length.
import { append } from "./state.ts";

const ASYMMETRIC = Symbol.for("bun:test:asymmetric");
const INSPECT_CUSTOM = Symbol.for("nodejs.util.inspect.custom");
const { getPrototypeOf, getOwnPropertyNames, getOwnPropertySymbols, getOwnPropertyDescriptor } = Object;
const FunctionToString = Function.prototype.toString;

export type Colors = boolean;

/** Bun's `pretty_fmt` markup, without colors or with them. */
export function markup(text: string, colors: Colors): string {
  return text.replace(/<(\/?)(r|d|b|red|green|yellow|blue|magenta|cyan|white|i|u)>/g, (_, close, tag) => {
    if (!colors) return "";
    const codes: Record<string, string> = {
      r: "\x1b[0m",
      d: "\x1b[2m",
      b: "\x1b[1m",
      i: "\x1b[3m",
      u: "\x1b[4m",
      red: "\x1b[31m",
      green: "\x1b[32m",
      yellow: "\x1b[33m",
      blue: "\x1b[34m",
      magenta: "\x1b[35m",
      cyan: "\x1b[36m",
      white: "\x1b[37m",
    };
    return close ? "\x1b[0m" : codes[tag];
  });
}

// ── what a value is ─────────────────────────────────────────────────────────
const is = (test: (v: any) => unknown, v: unknown) => {
  try {
    test(v);
    return true;
  } catch {
    return false;
  }
};
export const isMap = (v: unknown): v is Map<unknown, unknown> => is(x => Map.prototype.has.call(x, 0), v);
export const isSet = (v: unknown): v is Set<unknown> => is(x => Set.prototype.has.call(x, 0), v);
export const isWeakMap = (v: unknown) => is(x => WeakMap.prototype.has.call(x, {}), v);
export const isWeakSet = (v: unknown) => is(x => WeakSet.prototype.has.call(x, {}), v);
export const isDate = (v: unknown): v is Date => is(x => Date.prototype.getTime.call(x), v);
export const isRegExp = (v: unknown): v is RegExp =>
  is(x => getOwnPropertyDescriptor(RegExp.prototype, "source")!.get!.call(x), v) && v !== RegExp.prototype;
export const isPromise = (v: unknown): v is Promise<unknown> =>
  v instanceof Promise || (typeof v === "object" && v !== null && (v as any)[Symbol.toStringTag] === "Promise");
export const isError = (v: unknown): v is Error =>
  v instanceof Error ||
  (typeof v === "object" && v !== null && Object.prototype.toString.call(v) === "[object Error]");
export const isBoxedNumber = (v: unknown) => typeof v === "object" && is(x => Number.prototype.valueOf.call(x), v);
export const isBoxedString = (v: unknown) => typeof v === "object" && is(x => String.prototype.valueOf.call(x), v);
export const isBoxedBoolean = (v: unknown) => typeof v === "object" && is(x => Boolean.prototype.valueOf.call(x), v);
export const isBoxedBigInt = (v: unknown) => typeof v === "object" && is(x => BigInt.prototype.valueOf.call(x), v);
export const isBoxedSymbol = (v: unknown) => typeof v === "object" && is(x => Symbol.prototype.valueOf.call(x), v);
export const isArrayBuffer = (v: unknown): v is ArrayBuffer =>
  is(x => getOwnPropertyDescriptor(ArrayBuffer.prototype, "byteLength")!.get!.call(x), v);
export const isSharedArrayBuffer = (v: unknown) =>
  typeof SharedArrayBuffer === "function" &&
  is(x => getOwnPropertyDescriptor(SharedArrayBuffer.prototype, "byteLength")!.get!.call(x), v);
export const isDataView = (v: unknown): v is DataView => ArrayBuffer.isView(v) && is(x => DataView.prototype.getInt8.call(x, 0), v) || (ArrayBuffer.isView(v) && !isTypedArray(v));
const TypedArray = getPrototypeOf(Uint8Array);
const typedArrayTag = getOwnPropertyDescriptor(TypedArray.prototype, Symbol.toStringTag)!.get!;
export const isTypedArray = (v: unknown): boolean => typedArrayTag.call(v) !== undefined;
export const typedArrayName = (v: unknown): string => typedArrayTag.call(v) as string;
export const isAsymmetric = (v: unknown): boolean => {
  try {
    return typeof v === "object" && v !== null && (v as any)[ASYMMETRIC] !== undefined;
  } catch {
    return false;
  }
};
export const isClass = (f: unknown): boolean => {
  if (typeof f !== "function") return false;
  try {
    return /^class\b/.test(FunctionToString.call(f));
  } catch {
    return false;
  }
};
const isIdentifier = (key: string) => /^[A-Za-z_$][\w$]*$/.test(key);
const isInt32 = (n: number) => Number.isInteger(n) && !Object.is(n, -0) && n >= -2147483648 && n <= 2147483647;

/** `JSC__JSValue__getNameProperty`: `Symbol.toStringTag`, else a function's declared name. */
export function nameProperty(v: any): string {
  if (v === null || (typeof v !== "object" && typeof v !== "function")) return "";
  try {
    const tag = v[Symbol.toStringTag];
    if (typeof tag === "string" && tag) return tag;
  } catch {
    return "";
  }
  return typeof v === "function" ? declaredName(v) : "";
}

/** `getClassName`: a function's name; an object's constructor name (JSC's calculatedClassName), else "Object". */
export function className(v: any): string {
  if (v === null || (typeof v !== "object" && typeof v !== "function")) return "";
  if (typeof v === "function") return nameProperty(v);
  try {
    let proto = getPrototypeOf(v);
    while (proto) {
      const ctor = getOwnPropertyDescriptor(proto, "constructor");
      if (ctor && "value" in ctor && typeof ctor.value === "function") {
        const name = ctor.value.name;
        if (typeof name === "string" && name) return name;
        break;
      }
      proto = getPrototypeOf(proto);
    }
    const tag = v[Symbol.toStringTag];
    if (typeof tag === "string" && tag) return tag;
  } catch {}
  return "Object";
}

/** A function's `name`, or `""`. */
export function functionName(f: any): string {
  try {
    const name = f.name;
    return typeof name === "string" ? name : "";
  } catch {
    return "";
  }
}

/** `getName`: the display name, else `Symbol.toStringTag`. */
export function displayName(o: any): string {
  const name = functionName(o);
  if (name) return name;
  try {
    const tag = o[Symbol.toStringTag];
    return typeof tag === "string" ? tag : "";
  } catch {
    return "";
  }
}

/**
 * The name a function has in its source (`function f`, `class C`, a method), not one inferred from where it was
 * assigned: JavaScriptCore's `function->name(vm)`, which the snapshot format prints.
 */
export function declaredName(f: any): string {
  let source: string;
  try {
    source = FunctionToString.call(f);
  } catch {
    return functionName(f);
  }
  if (/\{\s*\[native code\]\s*\}\s*$/.test(source)) return functionName(f);
  const declared = source.match(/^(?:async\s+)?function\s*\*?\s*([\w$]+)|^class\s+([\w$]+)/);
  if (declared) return declared[1] ?? declared[2];
  if (/^(?:async\s+)?function\b|^class\b/.test(source)) return "";
  // An arrow function.
  if (/^(?:async\s*)?(?:\([^)]*\)|[\w$]+)\s*=>/.test(source)) return "";
  // A method: `name(…) {`, `async name(`, `*name(`, `get name(`, `[computed](`.
  return functionName(f);
}

const numberText = (n: number) => (Object.is(n, -0) ? "-0" : String(n));

function jsonString(s: string): string {
  return JSON.stringify(s);
}

let errorPrinter: ((error: unknown, colors: Colors) => string) | undefined;
/** How `show()` prints an Error: as the runner prints a thrown error (its source and stack). */
export function setErrorPrinter(print: (error: unknown, colors: Colors) => string) {
  errorPrinter = print;
}

/** How an asymmetric matcher prints itself. Set by expect.ts. */
export type AsymmetricPrinter = (matcher: any, print: (v: unknown, as?: "object" | "string" | "array") => string) => string;
let asymmetricPrinter: AsymmetricPrinter | undefined;
export function setAsymmetricPrinter(print: AsymmetricPrinter) {
  asymmetricPrinter = print;
}

/** A mock function, which the snapshot format prints with its calls. Set by mock.ts. */
let isMockFunction: (v: unknown) => boolean = () => false;
export function setMockDetector(detect: (v: unknown) => boolean) {
  isMockFunction = detect;
}

// ── own properties, as JSC's forEachProperty lists them ───────────────────────
/** `callGetters`: "throws" calls them and lets what they throw through, "marks" puts a getter that throws as a marker. */
function ownEntries(object: any, ordered: boolean, skipIndices: boolean, callGetters: false | "throws" | "marks") {
  const out: { key: string | symbol; value: unknown; getter?: "get" | "set" | "both" }[] = [];
  let names: (string | symbol)[];
  try {
    names = [...getOwnPropertyNames(object), ...getOwnPropertySymbols(object)];
  } catch (e) {
    throw e;
  }
  if (ordered) {
    const strings = names.filter(k => typeof k === "string") as string[];
    const symbols = names.filter(k => typeof k === "symbol") as symbol[];
    names = [...strings, ...symbols].sort((a, b) => {
      const x = typeof a === "symbol" ? (a.description ?? "") : a;
      const y = typeof b === "symbol" ? (b.description ?? "") : b;
      return x < y ? -1 : x > y ? 1 : 0;
    });
  }
  for (const key of names) {
    if (skipIndices && typeof key === "string" && /^(0|[1-9]\d*)$/.test(key) && Number(key) < 2 ** 32 - 1) continue;
    const desc = getOwnPropertyDescriptor(object, key);
    if (!desc || !desc.enumerable) continue;
    if ("value" in desc) append(out, { key, value: desc.value });
    else if (callGetters === "throws" && desc.get) append(out, { key, value: desc.get.call(object) });
    else if (callGetters === "marks" && desc.get) {
      let entry;
      try {
        entry = { key, value: desc.get.call(object) };
      } catch {
        entry = { key, value: undefined, getter: desc.set ? ("both" as const) : ("get" as const) };
      }
      append(out, entry);
    } else if (callGetters === "marks") append(out, { key, value: undefined });
    else append(out, { key, value: undefined, getter: desc.get && desc.set ? "both" : desc.get ? "get" : "set" });
  }
  return out;
}

// ── the console's formatter ─────────────────────────────────────────────────
type Tag =
  | "string"
  | "undefined"
  | "double"
  | "integer"
  | "null"
  | "boolean"
  | "array"
  | "object"
  | "function"
  | "class"
  | "error"
  | "typedarray"
  | "map"
  | "set"
  | "bigint"
  | "symbol"
  | "custom"
  | "promise"
  | "json"
  | "getter"
  | "asymmetric";

const PRIMITIVE = new Set<Tag>(["string", "undefined", "double", "integer", "null", "boolean", "symbol", "bigint"]);

/** An own accessor whose getter threw, which the console prints as JSC's `GetterSetter` cell. */
class Accessor {
  constructor(readonly kind: "get" | "set" | "both") {}
}

function tagOf(v: unknown, custom: boolean): Tag {
  if (v instanceof Accessor) return "getter";
  if (v === undefined) return "undefined";
  if (v === null) return "null";
  switch (typeof v) {
    case "number":
      return isInt32(v) ? "integer" : "double";
    case "boolean":
      return "boolean";
    case "string":
      return "string";
    case "bigint":
      return "bigint";
    case "symbol":
      return "symbol";
  }
  if (custom) {
    try {
      if (typeof (v as any)[INSPECT_CUSTOM] === "function") return "custom";
    } catch {}
  }
  if (isAsymmetric(v)) return "asymmetric";
  if (typeof v === "function") return isClass(v) ? "class" : "function";
  if (isError(v)) return "error";
  if (Array.isArray(v)) return "array";
  if (isBoxedString(v) || isRegExp(v)) return "string";
  if (isBoxedNumber(v)) return "double";
  if (isBoxedBoolean(v)) return "boolean";
  if (isMap(v) || isWeakMap(v)) return "map";
  if (isSet(v) || isWeakSet(v)) return "set";
  if (isDate(v)) return "json";
  if (isPromise(v)) return "promise";
  if (isArrayBuffer(v) || isSharedArrayBuffer(v) || ArrayBuffer.isView(v)) return "typedarray";
  return "object";
}

export interface ShowOptions {
  colors?: Colors;
  quoteStrings?: boolean;
  singleLine?: boolean;
  maxDepth?: number;
}

class Console {
  out = "";
  indent = 0;
  depth = 0;
  maxDepth: number;
  quoteStrings: boolean;
  quoteKeys = false;
  lineLength = 0;
  alwaysNewlineScope = false;
  singleLine: boolean;
  visited = new Set<unknown>();
  c: Colors;

  constructor(options: ShowOptions) {
    this.c = options.colors ?? false;
    this.quoteStrings = options.quoteStrings ?? true;
    this.singleLine = options.singleLine ?? false;
    this.maxDepth = options.maxDepth ?? 8;
  }
  m(text: string) {
    return markup(text, this.c);
  }
  write(s: string) {
    this.out += s;
  }
  add(n: number) {
    this.lineLength += n;
  }
  resetLine() {
    this.lineLength = this.indent * 2;
  }
  goodTime() {
    if (this.lineLength > 80) {
      this.resetLine();
      return true;
    }
    return false;
  }
  writeIndent() {
    this.out += "  ".repeat(this.indent);
  }
  comma() {
    this.write(this.m("<r><d>,<r>"));
    this.lineLength += 1;
  }

  format(v: unknown) {
    const tag = tagOf(v, true);
    this.printAs(tag, v);
  }

  printAs(tag: Tag, v: any) {
    const circular = tag === "array" || tag === "object" || tag === "map" || tag === "set";
    if (circular) {
      if (this.visited.has(v)) {
        this.write(this.m("<r><cyan>[Circular]<r>"));
        return;
      }
      this.visited.add(v);
    }
    try {
      this.printTag(tag, v);
    } finally {
      if (circular) this.visited.delete(v);
    }
  }

  printTag(tag: Tag, v: any) {
    switch (tag) {
      case "undefined":
        this.add(9);
        return this.write(this.m("<r><d>undefined<r>"));
      case "null":
        this.add(4);
        return this.write(this.m("<r><yellow>null<r>"));
      case "string":
        return this.printString(v);
      case "integer": {
        const text = String(v);
        this.add(text.length);
        return this.write(this.m("<r><yellow>") + text + this.m("<r>"));
      }
      case "bigint": {
        const text = String(v);
        this.add(text.length);
        return this.write(this.m("<r><yellow>") + text + "n" + this.m("<r>"));
      }
      case "double": {
        if (typeof v === "object") {
          const name = className(v);
          const text = String(Number.prototype.valueOf.call(v));
          if (name !== "Number") {
            this.add(name.length + text.length + 11);
            return this.write(this.m("<r><yellow>") + `[Number (${name}): ${text}]` + this.m("<r>"));
          }
          this.add(name.length + text.length + 4);
          return this.write(this.m("<r><yellow>") + `[${name}: ${text}]` + this.m("<r>"));
        }
        const text = numberText(v);
        this.add(text.length);
        return this.write(this.m("<r><yellow>") + text + this.m("<r>"));
      }
      case "boolean": {
        if (typeof v === "object") {
          const name = className(v);
          const text = String(Boolean.prototype.valueOf.call(v));
          if (name !== "Boolean") return this.write(this.m("<r><yellow>") + `[Boolean (${name}): ${text}]` + this.m("<r>"));
          return this.write(this.m("<r><yellow>") + `[Boolean: ${text}]` + this.m("<r>"));
        }
        this.add(v ? 4 : 5);
        return this.write(this.m(`<r><yellow>${v}<r>`));
      }
      case "symbol": {
        const description = v.description ?? "";
        this.add(6 + (description ? description.length + 2 : 0));
        return this.write(this.m("<r><blue>") + `Symbol(${description})` + this.m("<r>"));
      }
      case "custom":
        return this.printCustom(v);
      case "error":
        if (errorPrinter) return this.write(errorPrinter(v, this.c));
        return this.write(`[${String(v?.name ?? "Error")}: ${String(v?.message ?? "")}]`);
      case "class":
        return this.printClass(v);
      case "function":
        return this.printFunction(v);
      case "array":
        return this.printArray(v);
      case "map":
        return this.printMap(v);
      case "set":
        return this.printSet(v);
      case "json": {
        // A Date.
        const time = Date.prototype.getTime.call(v);
        const text = Number.isNaN(time) ? "Invalid Date" : Date.prototype.toISOString.call(v);
        this.add(text.length + 2);
        return this.write(this.m("<r><magenta>") + text + this.m("<r>"));
      }
      case "promise":
        return this.printPromise(v);
      case "typedarray":
        return this.printTypedArray(v);
      case "asymmetric":
        if (asymmetricPrinter)
          return this.write(
            asymmetricPrinter(v, (value, as) => {
              const sub = this.child();
              if (as === "string") sub.printString(value);
              else if (as === "object") sub.printAs("object", value);
              else if (as === "array") sub.printAs("array", value);
              else sub.format(value);
              this.lineLength = sub.lineLength;
              return sub.out;
            }),
          );
        return this.printObject(v);
      case "object":
        return this.printObject(v);
      case "getter": {
        const kind = (v as Accessor).kind;
        return this.write(this.m("<cyan>") + (kind === "both" ? "[Getter/Setter]" : kind === "get" ? "[Getter]" : "[Setter]") + this.m("<r>"));
      }
    }
  }

  child(): Console {
    const sub = new Console({ colors: this.c, quoteStrings: this.quoteStrings, singleLine: this.singleLine, maxDepth: this.maxDepth });
    sub.indent = this.indent;
    sub.depth = this.depth;
    sub.lineLength = this.lineLength;
    sub.visited = this.visited;
    sub.alwaysNewlineScope = this.alwaysNewlineScope;
    sub.quoteKeys = this.quoteKeys;
    return sub;
  }

  printString(v: any) {
    const isRe = isRegExp(v);
    const str = isRe ? RegExp.prototype.toString.call(v) : typeof v === "object" ? String.prototype.valueOf.call(v) : String(v);
    this.add(str.length);
    if (this.quoteStrings && !isRe) {
      if (str.length === 0 && typeof v !== "object") return this.write('""');
      if (typeof v !== "object") {
        if (this.c) this.write(this.m("<r><green>"));
        this.write(jsonString(str));
        if (this.c) this.write(this.m("<r>"));
        return;
      }
    }
    if (typeof v === "object" && !isRe) {
      if (this.c) this.write(this.m("<r><green>"));
      this.write(`[String: ${jsonString(str)}]`);
      if (this.c) this.write(this.m("<r>"));
      return;
    }
    if (isRe && this.c) this.write(this.m("<r><red>"));
    this.write(str);
    if (isRe && this.c) this.write(this.m("<r>"));
  }

  printCustom(v: any) {
    const fn = v[INSPECT_CUSTOM];
    const result = fn.call(v, this.maxDepth - this.depth, { depth: this.maxDepth - this.depth, colors: this.c }, (x: unknown) =>
      show(x),
    );
    if (typeof result === "string") return this.write(result);
    if (result === v) return this.printAs(tagOf(result, false), result);
    this.format(result);
  }

  printClass(v: any) {
    const name = displayName(v);
    this.add(name.length);
    let parent = "";
    try {
      const proto = getPrototypeOf(v);
      if (typeof proto === "function" && proto !== Function.prototype) parent = displayName(proto);
    } catch {}
    this.add(parent.length);
    const body = name
      ? parent
        ? `[class ${name} extends ${parent}]`
        : `[class ${name}]`
      : parent
        ? `[class (anonymous) extends ${parent}]`
        : "[class (anonymous)]";
    this.write(this.m("<cyan>") + body + this.m("<r>"));
  }

  printFunction(v: any) {
    const name = displayName(v);
    let kind = "";
    try {
      kind = displayName(getPrototypeOf(v));
    } catch {}
    let body: string;
    if (!name || kind === name) body = kind ? `[${kind}]` : "[Function]";
    else if (!kind) body = `[Function: ${name}]`;
    else body = `[${kind}: ${name}]`;
    this.write(this.m("<cyan>") + body + this.m("<r>"));
  }

  printPromise(v: any) {
    if (!this.singleLine && this.goodTime()) {
      this.write("\n");
      this.writeIndent();
    }
    const state = promiseState?.(v);
    this.write("Promise { " + this.m("<r><cyan>"));
    this.write(state === 1 ? "<resolved>" : state === 2 ? "<rejected>" : "<pending>");
    this.write(this.m("<r>") + " }");
  }

  printTypedArray(v: any) {
    let name: string;
    let items: ArrayLike<number | bigint>;
    if (isArrayBuffer(v) || isSharedArrayBuffer(v)) {
      name = isSharedArrayBuffer(v) ? "SharedArrayBuffer" : "ArrayBuffer";
      items = new Uint8Array(v);
    } else if (isTypedArray(v)) {
      name = typedArrayName(v);
      if (name === "Uint8Array" && typeof Buffer === "function" && Buffer.isBuffer?.(v)) name = "Buffer";
      items = v;
    } else {
      name = "DataView";
      items = new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    }
    this.write(name);
    const len = isArrayBuffer(v) || isSharedArrayBuffer(v) ? v.byteLength : name === "DataView" ? v.byteLength : v.length;
    if (items.length === 0) return this.write(`(${len}) []`);
    this.write(`(${len}) [ `);
    const big = typeof items[0] === "bigint" ? "n" : "";
    const shown = Math.min(items.length, 513);
    for (let i = 0; i < shown; i++) {
      if (i > 0) {
        this.comma();
        this.write(" ");
        this.lineLength++;
      }
      this.write(this.m("<r><yellow>") + numberText(items[i] as number) + big + this.m("<r>"));
    }
    if (items.length > 513) this.write(this.m("<r><d>") + `${big}, ... ${items.length - 513} more` + this.m("<r>"));
    this.write(" ]");
  }

  printMap(v: any) {
    const weak = isWeakMap(v);
    const name = weak ? "WeakMap" : "Map";
    const size = weak ? 0 : getOwnPropertyDescriptor(Map.prototype, "size")!.get!.call(v);
    const saved = this.quoteStrings;
    this.quoteStrings = true;
    try {
      if (size === 0) return this.write(`${name} {}`);
      if (this.depth > this.maxDepth) return this.depthMarker(name);
      this.write(this.singleLine ? `${name}(${size}) { ` : `${name}(${size}) {\n`);
      this.indent++;
      this.depth++;
      let count = 0;
      for (const [key, value] of Map.prototype.entries.call(v) as Iterable<[unknown, unknown]>) {
        if (this.singleLine && count > 0) {
          this.comma();
          this.write(" ");
        }
        if (!this.singleLine) this.writeIndent();
        this.format(key);
        this.write(": ");
        this.format(value);
        count++;
        if (!this.singleLine) {
          this.comma();
          this.write("\n");
        }
      }
      if (this.singleLine && count > 0) this.write(" ");
      this.indent--;
      this.depth--;
      if (!this.singleLine) this.writeIndent();
      this.write("}");
    } finally {
      this.quoteStrings = saved;
    }
  }

  printSet(v: any) {
    const weak = isWeakSet(v);
    const name = weak ? "WeakSet" : "Set";
    const size = weak ? 0 : getOwnPropertyDescriptor(Set.prototype, "size")!.get!.call(v);
    const saved = this.quoteStrings;
    this.quoteStrings = true;
    try {
      if (size === 0) return this.write(`${name} {}`);
      if (this.depth > this.maxDepth) return this.depthMarker(name);
      this.write(this.singleLine ? `${name}(${size}) { ` : `${name}(${size}) {\n`);
      this.indent++;
      this.depth++;
      let first = true;
      for (const value of Set.prototype.values.call(v) as Iterable<unknown>) {
        if (this.singleLine) {
          if (!first) {
            this.comma();
            this.write(" ");
          }
          first = false;
        } else this.writeIndent();
        this.format(value);
        if (!this.singleLine) {
          this.comma();
          this.write("\n");
        }
      }
      if (this.singleLine && !first) this.write(" ");
      this.indent--;
      this.depth--;
      if (!this.singleLine) this.writeIndent();
      this.write("}");
    } finally {
      this.quoteStrings = saved;
    }
  }

  depthMarker(name: string) {
    this.add(name.length + 6);
    this.write(this.m("<r><cyan>") + `[${name} ...]` + this.m("<r>"));
  }

  printArray(v: any[]) {
    const len = v.length;
    if (len !== 0 && this.depth > this.maxDepth) return this.depthMarker("Array");
    if (len === 0) {
      this.write("[]");
      this.add(2);
      return;
    }
    let wasGoodTime = this.alwaysNewlineScope || len > 10;
    this.indent++;
    this.depth++;
    const savedQuote = this.quoteStrings;
    this.quoteStrings = true;
    let i = 1;
    try {
      this.add(2);
      let emptyStart: number | undefined;
      {
        const has = 0 in v;
        const element = v[0];
        const tag = has ? tagOf(element, true) : "undefined";
        wasGoodTime = wasGoodTime || (has && !PRIMITIVE.has(tag)) || this.goodTime();
        if (!this.singleLine && wasGoodTime) {
          this.resetLine();
          this.write("[\n");
          this.writeIndent();
          this.add(1);
        } else {
          this.write("[ ");
          this.add(2);
        }
        if (!has) emptyStart = 0;
        else this.printAs(tag, element);
      }
      let nonempty = 1;
      const writeEmpty = (count: number) => {
        if (count === 1) {
          this.add(10);
          this.write(this.m("<r><d>") + "empty item" + this.m("<r>"));
        } else {
          this.add(String(count).length + 14);
          this.write(this.m("<r><d>") + `${count} x empty items` + this.m("<r>"));
        }
      };
      const separate = () => {
        this.comma();
        if (!this.singleLine && this.goodTime()) {
          wasGoodTime = true;
          this.write("\n");
          this.writeIndent();
        } else {
          this.lineLength++;
          this.write(" ");
        }
      };
      for (; i < len; i++) {
        if (!(i in v)) {
          if (emptyStart === undefined) emptyStart = i;
          continue;
        }
        if (nonempty >= 100) {
          this.comma();
          if (this.singleLine) this.write(" ");
          else {
            this.write("\n");
            this.lineLength = 0;
            this.writeIndent();
          }
          this.add(16);
          this.write(this.m("<r><d>") + `... ${len - i} more items` + this.m("<r>"));
          break;
        }
        nonempty++;
        if (emptyStart !== undefined) {
          if (emptyStart > 0) separate();
          writeEmpty(i - emptyStart);
          emptyStart = undefined;
        }
        separate();
        this.printAs(tagOf(v[i], true), v[i]);
      }
      if (emptyStart !== undefined) {
        if (emptyStart > 0) separate();
        writeEmpty(len - emptyStart);
      }
      // Its other own properties.
      const extra = ownEntries(v, false, true, false).filter(e => e.key !== "length");
      let n = i;
      for (const { key, value } of extra) this.property(key, value, n++, v, !this.singleLine && (this.alwaysNewlineScope || this.goodTime()), true);
    } finally {
      this.quoteStrings = savedQuote;
      this.indent--;
      this.depth--;
    }
    if (!this.singleLine && (wasGoodTime || this.goodTime())) {
      this.resetLine();
      this.write("\n");
      this.writeIndent();
      this.write("]");
      this.resetLine();
      this.add(1);
    } else {
      this.write(" ]");
      this.add(2);
    }
  }

  /** One `key: value` of an object (PropertyIteratorCtx::for_each). Returns whether lines break. */
  property(key: string | symbol, value: unknown, i: number, parent: any, alwaysNewline: boolean, inArray = false) {
    if (key === "constructor") return alwaysNewline;
    const tag = tagOf(value, true);
    if (i === 0 && !inArray) {
      this.firstProperty(parent);
      alwaysNewline = !this.singleLine;
    }
    if (i > 0) {
      this.comma();
      if (!this.singleLine && (alwaysNewline || this.alwaysNewlineScope || this.goodTime())) {
        this.write("\n");
        this.writeIndent();
        this.resetLine();
      } else {
        this.lineLength++;
        this.write(" ");
      }
    }
    if (typeof key === "symbol") {
      const d = key.description ?? "";
      this.add(12 + d.length);
      this.write(this.m("<r><d>[<r><blue>") + `Symbol(${d})` + this.m("<r><d>]:<r> "));
    } else if (!this.quoteKeys && isIdentifier(key)) {
      this.add(key.length + 1);
      this.write(this.m("<r>") + key + this.m("<d>:<r> "));
    } else {
      this.add(key.length + 2);
      this.write(this.m("<r><green>") + jsonString(key) + this.m("<r><d>:<r> "));
    }
    this.printAs(tag, value);
    return alwaysNewline;
  }

  firstProperty(parent: any) {
    if (typeof parent !== "function") {
      const name = objectName(parent);
      if (name) this.write(name + " ");
    }
    this.lineLength = this.indent * 2 + 1;
    this.indent++;
    this.depth++;
    if (this.singleLine) this.write("{ ");
    else {
      this.write("{\n");
      this.writeIndent();
    }
  }

  printObject(v: any) {
    const savedQuote = this.quoteStrings;
    this.quoteStrings = true;
    const savedScope = this.alwaysNewlineScope;
    try {
      let alwaysNewline = !this.singleLine && (this.alwaysNewlineScope || this.goodTime());
      if (this.depth > this.maxDepth) {
        if (!this.singleLine && (this.alwaysNewlineScope || this.goodTime())) {
          this.write("\n");
          this.writeIndent();
          this.resetLine();
        }
        const name = nameProperty(v) || "Object";
        return this.write(this.m("<r><cyan>") + `[${name} ...]` + this.m("<r>"));
      }
      let i = 0;
      for (const entry of ownEntries(v, false, false, "marks")) {
        if (entry.key === "constructor") continue;
        const value = entry.getter ? new Accessor(entry.getter) : entry.value;
        alwaysNewline = this.property(entry.key, value, i, v, alwaysNewline);
        i++;
      }
      if (i === 0) {
        if (typeof v === "function") return this.printAs(isClass(v) ? "class" : "function", v);
        const name = objectName(v);
        if (name) this.write(name + " ");
        this.write("{}");
        return;
      }
      this.depth--;
      if (alwaysNewline) {
        this.indent = Math.max(0, this.indent - 1);
        this.comma();
        this.write("\n");
        this.writeIndent();
        this.write("}");
        this.lineLength++;
      } else {
        this.lineLength += 2;
        this.write(" }");
        this.indent = Math.max(0, this.indent - 1);
      }
    } finally {
      this.quoteStrings = savedQuote;
      this.alwaysNewlineScope = savedScope;
    }
  }
}

/** `get_object_name`: the class name unless it is "Object"; a null-prototype object says so. */
function objectName(v: any): string | undefined {
  const name = className(v);
  if (name !== "Object") return name;
  try {
    if (getPrototypeOf(v) === null) return "[Object: null prototype]";
  } catch {}
  return undefined;
}

let promiseState: ((p: Promise<unknown>) => number | undefined) | undefined;
export function setPromiseState(state: (p: Promise<unknown>) => number | undefined) {
  promiseState = state;
}

/** expect()'s way to print a value in a message (`show`). */
export function show(value: unknown, options: ShowOptions = {}): string {
  const f = new Console(options);
  f.format(value);
  return f.out;
}

/** `show_unquoted`. */
export function showUnquoted(value: unknown, options: ShowOptions = {}): string {
  return show(value, { ...options, quoteStrings: false });
}

// ── the snapshot format (JestPrettyFormat) ──────────────────────────────────
class Pretty {
  out = "";
  indent = 0;
  quoteStrings = false;
  lineLength = 0;
  alwaysNewlineScope = false;
  visited = new Set<unknown>();
  c: Colors;
  constructor(colors: Colors) {
    this.c = colors;
  }
  m(t: string) {
    return markup(t, this.c);
  }
  write(s: string) {
    this.out += s;
  }
  add(n: number) {
    this.lineLength += n;
  }
  resetLine() {
    this.lineLength = this.indent * 2;
  }
  goodTime() {
    if (this.lineLength > 80) {
      this.resetLine();
      return true;
    }
    return false;
  }
  writeIndent() {
    this.out += "  ".repeat(Math.min(this.indent, 32));
  }
  comma() {
    this.write(this.m("<r><d>,<r>"));
    this.lineLength += 1;
  }
  quoting<T>(q: boolean, f: () => T): T {
    const saved = this.quoteStrings;
    this.quoteStrings = q;
    try {
      return f();
    } finally {
      this.quoteStrings = saved;
    }
  }
  indented<T>(f: () => T): T {
    this.indent++;
    try {
      return f();
    } finally {
      this.indent--;
    }
  }

  format(v: unknown) {
    const tag = tagOf(v, false);
    const circular = tag === "array" || tag === "object" || tag === "map" || tag === "set";
    if (circular) {
      if (this.visited.has(v)) return this.write(this.m("<r><cyan>[Circular]<r>"));
      this.visited.add(v);
    }
    try {
      this.printAs(tag, v);
    } finally {
      if (circular) this.visited.delete(v);
    }
  }

  printAs(tag: Tag, v: any) {
    switch (tag) {
      case "string":
        return this.printString(v);
      case "integer": {
        const t = String(v);
        this.add(t.length);
        return this.write(this.m("<r><yellow>") + t + this.m("<r>"));
      }
      case "bigint": {
        const t = String(v);
        this.add(t.length);
        return this.write(this.m("<r><yellow>") + t + "n" + this.m("<r>"));
      }
      case "double":
        if (typeof v === "object") return this.printObject(v);
        this.add(numberText(v).length);
        return this.write(this.m("<r><yellow>") + numberText(v) + this.m("<r>"));
      case "undefined":
        this.add(9);
        return this.write(this.m("<r><d>undefined<r>"));
      case "null":
        this.add(4);
        return this.write(this.m("<r><yellow>null<r>"));
      case "symbol": {
        const d = v.description;
        this.add(6 + (d ? d.length + 2 : 0));
        return this.write(this.m("<r><blue>") + (d ? `Symbol(${d})` : "Symbol") + this.m("<r>"));
      }
      case "error": {
        const name = className(v);
        let message = "";
        try {
          message = String(v.message ?? "");
        } catch {}
        return this.write(message ? `[${name}: ${message}]` : `[${name}]`);
      }
      case "class": {
        const name = nameProperty(v);
        this.add(name.length);
        return this.write(this.m("<cyan>") + (name ? `[class ${name}]` : "[class]") + this.m("<r>"));
      }
      case "function": {
        const name = nameProperty(v);
        return this.write(this.m("<cyan>") + (name ? `[Function: ${name}]` : "[Function]") + this.m("<r>"));
      }
      case "array":
        return this.printArray(v);
      case "promise":
        if (this.goodTime()) {
          this.write("\n");
          this.writeIndent();
        }
        return this.write("Promise {}");
      case "boolean":
        if (typeof v === "object") return this.printObject(v);
        this.add(v ? 4 : 5);
        return this.write(this.m(`<r><yellow>${v}<r>`));
      case "map":
        return this.printMap(v);
      case "set":
        return this.printSet(v);
      case "json": {
        const text = Number.isNaN(Date.prototype.getTime.call(v)) ? "null" : Date.prototype.toISOString.call(v);
        this.add(text.length + 2);
        return this.write(this.m("<r><magenta>") + text + this.m("<r>"));
      }
      case "typedarray":
        return this.printTypedArray(v);
      case "asymmetric":
        if (asymmetricPrinter)
          return this.write(
            asymmetricPrinter(v, (value, as) => {
              const sub = new Pretty(this.c);
              sub.indent = this.indent;
              sub.lineLength = this.lineLength;
              sub.visited = this.visited;
              sub.quoteStrings = this.quoteStrings;
              if (as === "string") sub.printString(value);
              else if (as === "object") sub.printObject(value);
              else if (as === "array") sub.printArray(value);
              else sub.format(value);
              return sub.out;
            }),
          );
        return this.printObject(v);
      default:
        return this.printObject(v);
    }
  }

  printString(v: any) {
    const isRe = isRegExp(v);
    if (typeof v === "object" && !isRe) {
      const s = String.prototype.valueOf.call(v);
      if (s.length === 0) return this.write("String {}");
      if (this.indent === 0) this.write("\n");
      this.write("String {\n");
      this.indented(() => {
        this.resetLine();
        this.writeIndent();
        for (let i = 0; i < s.length; i++) {
          this.write(`"${i}": "${s[i]}",\n`);
          if (i !== s.length - 1) this.writeIndent();
        }
      });
      this.resetLine();
      return this.write("}\n");
    }
    const str = isRe ? RegExp.prototype.toString.call(v) : String(v);
    this.add(str.length);
    if (this.quoteStrings && !isRe) {
      if (str.length === 0) return this.write('""');
      if (this.c) this.write(this.m("<r><green>"));
      const hasNewline = /[\n\r]/.test(str);
      if (hasNewline) this.write("\n");
      this.write('"');
      // `\r\n` is `\n`, a lone `\r` is a line break.
      this.write(str.replace(/\r\n?/g, "\n"));
      this.write('"');
      if (hasNewline) this.write("\n");
      if (this.c) this.write(this.m("<r>"));
      return;
    }
    if (isRe && this.c) this.write(this.m("<r><red>"));
    this.write(str);
    if (isRe && this.c) this.write(this.m("<r>"));
  }

  printArray(v: any[]) {
    const len = v.length;
    if (len === 0) {
      this.write("[]");
      this.add(2);
      return;
    }
    if (this.indent === 0) this.write("\n");
    this.indented(() =>
      this.quoting(true, () => {
        this.add(2);
        this.resetLine();
        this.write("[\n");
        this.writeIndent();
        this.add(1);
        this.format(v[0]);
        if (len === 1) this.comma();
        for (let i = 1; i < len; i++) {
          this.comma();
          this.write("\n");
          this.writeIndent();
          this.format(v[i]);
          if (i === len - 1) this.comma();
        }
      }),
    );
    this.resetLine();
    this.write("\n");
    this.writeIndent();
    this.write("]");
    if (this.indent === 0) this.write("\n");
    this.resetLine();
    this.add(1);
  }

  printMap(v: any) {
    const weak = isWeakMap(v);
    const name = weak ? "WeakMap" : "Map";
    const size = weak ? 0 : getOwnPropertyDescriptor(Map.prototype, "size")!.get!.call(v);
    if (size === 0) return this.write(`${name} {}`);
    this.write(`\n${name} {\n`);
    this.indented(() =>
      this.quoting(true, () => {
        for (const [key, value] of Map.prototype.entries.call(v) as Iterable<[unknown, unknown]>) {
          this.writeIndent();
          this.format(key);
          this.write(" => ");
          this.format(value);
          this.comma();
          this.write("\n");
        }
      }),
    );
    this.writeIndent();
    this.write("}\n");
  }

  printSet(v: any) {
    const weak = isWeakSet(v);
    const name = weak ? "WeakSet" : "Set";
    const size = weak ? 0 : getOwnPropertyDescriptor(Set.prototype, "size")!.get!.call(v);
    this.writeIndent();
    if (size === 0) return this.write(`${name} {}`);
    this.write(`\n${name} {\n`);
    this.indented(() =>
      this.quoting(true, () => {
        for (const value of Set.prototype.values.call(v) as Iterable<unknown>) {
          this.writeIndent();
          this.format(value);
          this.comma();
          this.write("\n");
        }
      }),
    );
    this.writeIndent();
    this.write("}\n");
  }

  printTypedArray(v: any) {
    let bytes: ArrayLike<number | bigint>;
    let name: string;
    if (isArrayBuffer(v) || isSharedArrayBuffer(v)) {
      name = isSharedArrayBuffer(v) ? "SharedArrayBuffer" : "ArrayBuffer";
      bytes = new Uint8Array(v);
    } else if (isTypedArray(v)) {
      name = typedArrayName(v);
      bytes = v;
    } else {
      name = "DataView";
      bytes = new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
    }
    const empty = bytes.length === 0;
    if (this.indent === 0 && !empty) this.write("\n");
    if (name === "Uint8Array" && className(v) === "Buffer") {
      if (empty && this.indent === 0) this.write("\n");
      this.write("{\n");
      this.indented(() => {
        this.writeIndent();
        this.write('"data": [');
        this.indented(() => {
          for (let i = 0; i < bytes.length; i++) {
            this.write("\n");
            this.writeIndent();
            this.write(`${bytes[i]},`);
          }
        });
        if (!empty) {
          this.write("\n");
          this.writeIndent();
        }
        this.write("],\n");
        this.writeIndent();
        this.write('"type": "Buffer",\n');
      });
      this.writeIndent();
      this.write("}");
      if (this.indent === 0) this.write("\n");
      return;
    }
    this.write(name + " [");
    if (!empty) {
      this.indented(() => {
        for (let i = 0; i < bytes.length; i++) {
          this.write("\n");
          this.writeIndent();
          this.write(`${typeof bytes[i] === "number" ? numberText(bytes[i] as number) : bytes[i]},`);
        }
      });
      this.write("\n");
      this.writeIndent();
      this.write("]");
      if (this.indent === 0) this.write("\n");
    } else this.write("]");
  }

  firstProperty(parent: any) {
    if (typeof parent !== "function") {
      let name = nameProperty(parent);
      if (!name || name === "Object") {
        try {
          name = nameProperty(getPrototypeOf(parent));
        } catch {
          name = "";
        }
      }
      if (name && name !== "Object") this.write(name + " ");
    }
    this.lineLength = this.indent * 2 + 1;
    if (this.indent === 0) this.write("\n");
    const cls = className(parent);
    if (cls && cls !== "Object") this.write(cls + " ");
    this.write("{\n");
    this.indent++;
    this.writeIndent();
  }

  printObject(v: any) {
    let alwaysNewline = this.alwaysNewlineScope || this.goodTime();
    let i = 0;
    this.quoting(true, () => {
      for (const { key, value } of ownEntries(v, true, false, "throws")) {
        if (key === "constructor") continue;
        const tag = tagOf(value, false);
        if (i === 0) {
          this.firstProperty(v);
          alwaysNewline = true;
        } else this.comma();
        if (i > 0) {
          if (alwaysNewline || this.alwaysNewlineScope || this.goodTime()) {
            this.write("\n");
            this.writeIndent();
            this.resetLine();
          } else {
            this.lineLength++;
            this.write(" ");
          }
        }
        if (typeof key === "symbol") {
          const d = key.description ?? "";
          this.add(12 + d.length);
          this.write(this.m("<r><d>[<r><blue>") + `Symbol(${d})` + this.m("<r><d>]:<r> "));
        } else {
          this.add(key.length + 2);
          this.write(this.m(isIdentifier(key) ? "<r>" : "<r><green>") + jsonString(key) + this.m("<r><d>:<r> "));
        }
        this.format(value);
        i++;
      }
    });
    if (i === 0) {
      const name = className(v);
      if (name !== "Object") this.write(`${name} {}`);
      else this.write("{}");
      return;
    }
    this.comma();
    if (alwaysNewline) {
      this.indent = Math.max(0, this.indent - 1);
      this.write("\n");
      this.writeIndent();
      this.write("}");
      this.lineLength++;
    } else {
      this.lineLength += 2;
      this.write(" }");
    }
    if (this.indent === 0) this.write("\n");
  }
}

/** `JestPrettyFormat::format` with one value, as diffs and snapshots call it. */
export function pretty(value: unknown, options: { quoteStrings?: boolean; colors?: Colors } = {}): string {
  const p = new Pretty(options.colors ?? false);
  p.quoteStrings = options.quoteStrings ?? true;
  p.format(value);
  return p.out;
}

/** What a snapshot saves for `value` (`print_snapshot` with Bun's format). */
export function snapshotText(value: unknown): string {
  return pretty(value, { quoteStrings: true });
}

/** DiffFormatter::new: both sides printed by pretty-format, one newline trimmed at each end. */
export function diffText(value: unknown): string {
  let text = pretty(value, { quoteStrings: true });
  if (text.endsWith("\n")) text = text.slice(0, -1);
  if (text.startsWith("\n")) text = text.slice(1);
  return text;
}
