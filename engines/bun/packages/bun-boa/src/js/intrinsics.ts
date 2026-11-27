// The intrinsics of Bun's builtin modules (src/js) on Boa: what JavaScriptCore has as private names (`$call`,
// `$isPromise`, `$getInternalField`, …). src/builtins.rs puts its native operations on globalThis.__bun_intrinsics_ops__
// (a script: no top-level `const`, which would clash with bun.ts's), and reads
// back `{ require, esm }` from them. Each module's function takes the intrinsics it names as parameters (build.rs).
//
// A member intrinsic (`fn.$call(…)`, `map.$get(k)`) is a property named `__intrinsic__<name>`, which this file puts
// on the prototypes, not enumerable. Unlike a private name, Object.getOwnPropertyNames lists it.
(function install(native: {
  compile(id: number): ((...intrinsics: unknown[]) => () => unknown) | undefined;
  intrinsicNames(id: number): string[];
  lazy(id: number): unknown;
  nativeName(id: number): string;
  className(id: number): string | undefined;
  brand(value: unknown): string | undefined;
  functionName(value: unknown): string | undefined;
  promiseState(promise: Promise<unknown>): 0 | 1 | 2;
  promiseResult(promise: Promise<unknown>): unknown;
  proxyField(proxy: object, field: 0 | 1): object | null | undefined;
  makeError(code: number, message: string): Error;
  createRequire(dir: string): (specifier: string) => unknown;
  /** The `code` of each `Bun::ErrorCode`, by its number ("ERR_INVALID_ARG_TYPE"). */
  errorCodes: string[];
  /** The realm's async context (bun_js): what `$asyncContext`'s internal field 0 holds. */
  asyncContext(): unknown;
  setAsyncContext(value: unknown): void;
  isAsyncContextTracked(): boolean;
  internalFields: WeakMap<object, unknown[]>;
  /** `process.binding('constants').fs` and `.os` (bun_runtime_js `node_fs_constant`). */
  fsConstants: Record<string, number>;
  osConstants: Record<string, unknown>;
  specifiers: string[];
  native: boolean[];
  StringDecoder: new (encoding?: unknown) => object;
  /** The builtin functions of src/js/builtins, as `<file>.<name>`, and which of them are `$getter`s. */
  functions: string[];
  getters: boolean[];
  compileFunction(id: number): (...intrinsics: unknown[]) => Function;
  functionIntrinsicNames(id: number): string[];
  /** bun_runtime_js's natives of `Buffer`, and what js/buffer.ts installs it with. */
  buffer: object;
  installBuffer(env: object): Record<string, unknown>;
  installed?: { require(id: number): unknown; esm(id: number): [string, unknown][] };
}) {
  const G = globalThis as any;
  const { defineProperty, getOwnPropertyDescriptor, getOwnPropertyNames, getOwnPropertySymbols, keys } = Object;
  const ArrayIsArray = Array.isArray;
  const ReflectApply = Reflect.apply;

  // ───────────────────────── member intrinsics ─────────────────────────
  const member = (target: object, name: string, value: unknown) =>
    defineProperty(target, "__intrinsic__" + name, { value, writable: true, configurable: true, enumerable: false });
  member(Function.prototype, "call", Function.prototype.call);
  member(Function.prototype, "apply", Function.prototype.apply);
  member(Promise, "resolve", Promise.resolve);
  member(Promise, "reject", Promise.reject);
  member(Promise.prototype, "then", Promise.prototype.then);
  for (const C of [Map, WeakMap, Set, WeakSet] as any[])
    for (const name of ["get", "set", "has", "delete", "add", "clear", "forEach", "keys"])
      if (name in C.prototype) member(C.prototype, name, C.prototype[name]);
  member(String.prototype, "charCodeAt", String.prototype.charCodeAt);
  member(Object, "defineProperty", defineProperty);
  member(Object, "getOwnPropertyNames", getOwnPropertyNames);
  member(Object, "getOwnPropertySymbols", getOwnPropertySymbols);
  member(Object, "getOwnPropertyDescriptor", getOwnPropertyDescriptor);
  member(G, "Array", Array);

  // ───────────────────────── internal fields ─────────────────────────
  // Rust's (bun_js `JSObject::put_internal_field`): a native like bun:sqlite's `run` writes them.
  const fields = native.internalFields;
  const fieldsOf = (object: object) => {
    let list = fields.get(object);
    if (!list) fields.set(object, (list = []));
    return list;
  };
  // `$asyncContext`'s field 0 is the realm's, which bun_js's job executor carries into promise jobs.
  const asyncContext = {};
  const getInternalField = (object: object, index: number) =>
    object === asyncContext && index === 0 ? native.asyncContext() : fieldsOf(object)[index];
  const putInternalField = (object: object, index: number, value: unknown) => {
    if (object === asyncContext && index === 0) native.setAsyncContext(value);
    else fieldsOf(object)[index] = value;
  };
  // Bun's timers keep the async context they were set in, as its promise jobs do.
  for (const name of ["setTimeout", "setInterval", "setImmediate"]) {
    const original = G[name];
    if (typeof original !== "function") continue;
    G[name] = {
      [name](callback: unknown, ...rest: unknown[]) {
        if (typeof callback !== "function" || !native.isAsyncContextTracked()) return original(callback, ...rest);
        const captured = native.asyncContext();
        return original(function (this: unknown, ...args: unknown[]) {
          const previous = native.asyncContext();
          native.setAsyncContext(captured);
          try {
            return callback.apply(this, args);
          } finally {
            native.setAsyncContext(previous);
          }
        }, ...rest);
      },
    }[name];
  }

  // ───────────────────────── promises ─────────────────────────
  const settlers = new WeakMap<Promise<unknown>, { resolve(v: unknown): void; reject(e: unknown): void }>();
  function newPromise() {
    let settle: any;
    const promise = new Promise((resolve, reject) => (settle = { resolve, reject }));
    settlers.set(promise, settle);
    return promise;
  }
  const resolvePromise = (promise: Promise<unknown>, value: unknown) => settlers.get(promise)?.resolve(value);
  const rejectPromise = (promise: Promise<unknown>, reason: unknown) => settlers.get(promise)?.reject(reason);
  const noop = () => {};
  const pokePromiseAsHandled = (promise: Promise<unknown>) =>
    void ReflectApply(Promise.prototype.then, promise, [noop, noop]);

  // ───────────────────────── errors ─────────────────────────
  const describeFunction = (fn: Function) => (fn.name ? `function ${fn.name}` : "function ");
  function stringOf(value: unknown, quotes = false): string {
    if (typeof value === "string") {
      if (!quotes) return value;
      const quote = value.includes("'") ? '"' : "'";
      return quote + value.replaceAll(quote, "\\" + quote) + quote;
    }
    if (typeof value === "symbol") return value.toString();
    if (typeof value === "function") return value.name ? `[Function: ${value.name}]` : "[Function (anonymous)]";
    if (value === null || typeof value !== "object") return String(value);
    if (typeof G.Bun?.inspect === "function") return G.Bun.inspect(value, { compact: true, breakLength: Infinity });
    if (ArrayIsArray(value)) return `[ ${value.map(v => stringOf(v, true)).join(", ")} ]`;
    const entries = keys(value).map(k => `${k}: ${stringOf((value as any)[k], true)}`);
    const prefix = Object.getPrototypeOf(value) === null ? "[Object: null prototype] " : "";
    return prefix + (entries.length ? `{ ${entries.join(", ")} }` : "{}");
  }
  // determineSpecificType in src/jsc/bindings/ErrorCode.cpp, after Node's lib/internal/errors.js.
  function specificType(value: unknown): string {
    if (value === null) return "null";
    if (value === undefined) return "undefined";
    switch (typeof value) {
      case "number":
        return `type number (${Object.is(value, -0) ? "-0" : value})`;
      case "boolean":
        return `type boolean (${value})`;
      case "bigint":
        return `type bigint (${value}n)`;
      case "symbol":
        return `type symbol (${value.toString()})`;
      case "function":
        return describeFunction(value);
      case "string": {
        const shown = value.length > 28 ? value.slice(0, 25) + "..." : value;
        return value.includes("'") ? `type string ("${shown.replaceAll('"', '\\"')}")` : `type string ('${shown}')`;
      }
    }
    const constructor = (value as any).constructor;
    if (constructor) return `an instance of ${String(constructor.name)}`;
    return stringOf(value);
  }
  const list = (items: string[]) =>
    items.length <= 2
      ? items.join(" or ")
      : items.length === 3
        ? `${items[0]}, ${items[1]}, or ${items[2]}`
        : `${items.slice(0, -1).join(", ")}, or ${items.at(-1)}`;
  const parameter = (name: string) =>
    name.endsWith(" argument") ? name : `"${name}" ${name.includes(".") ? "property" : "argument"}`;
  const PRIMITIVES = ["string", "function", "number", "object", "Function", "Object", "boolean", "bigint", "symbol"];
  function invalidArgType(name: unknown, expected: unknown, actual: unknown) {
    let message = `The ${parameter(String(name))} must be `;
    if (ArrayIsArray(expected)) {
      const types: string[] = [];
      const instances: string[] = [];
      const other: string[] = [];
      for (const entry of expected.map(String)) {
        if (PRIMITIVES.includes(entry)) types.push(entry.toLowerCase());
        else if (/^[A-Z][a-zA-Z0-9]*$/.test(entry)) instances.push(entry);
        else other.push(entry);
      }
      if (instances.length && types.includes("object")) {
        types.splice(types.indexOf("object"), 1);
        instances.push("Object");
      }
      if (types.length) {
        message += `${types.length > 1 ? "one of type" : "of type"} ${list(types)}`;
        if (instances.length || other.length) message += " or ";
      }
      if (instances.length) {
        message += `an instance of ${list(instances)}`;
        if (other.length) message += " or ";
      }
      if (other.length)
        message +=
          other.length > 1 ? `one of ${list(other)}` : (other[0].toLowerCase() !== other[0] ? "an " : "") + other[0];
    } else {
      const text = String(expected);
      const phrase = text.includes(" ") && !text.includes(", ") && !text.includes(" or ");
      message += phrase ? (/[A-Z]/.test(text) ? "an " : "") + text : `of type ${text}`;
    }
    return `${message}. Received ${specificType(actual)}`;
  }
  const separated = (digits: string) => {
    const sign = digits.startsWith("-") ? "-" : "";
    let rest = sign ? digits.slice(1) : digits;
    let out = "";
    while (rest.length > 3) {
      out = "_" + rest.slice(-3) + out;
      rest = rest.slice(0, -3);
    }
    return sign + rest + out;
  };
  function outOfRangeReceived(input: unknown) {
    if (typeof input === "number") {
      if (Number.isInteger(input) && Math.abs(input) > 2 ** 32) return separated(String(input));
      if (Object.is(input, -0)) return "-0";
    } else if (typeof input === "bigint") {
      const digits = String(input);
      return (input > 2n ** 32n || input < -(2n ** 32n) ? separated(digits) : digits) + "n";
    }
    return stringOf(input);
  }
  // jsFunctionMakeErrorWithCode in src/jsc/bindings/ErrorCode.cpp.
  const MESSAGES: Record<string, (...args: any[]) => string> = {
    ERR_INVALID_ARG_TYPE: invalidArgType,
    ERR_INVALID_ARG_VALUE: (name, value, reason = "is invalid") =>
      `The ${String(name).includes(".") ? "property" : "argument"} '${String(name)}' ${reason}. Received ${stringOf(value, true)}`,
    ERR_OUT_OF_RANGE: (name, range, input) =>
      `The value of "${name}" is out of range. It must be ${range}. Received ${outOfRangeReceived(input)}`,
    ERR_MISSING_ARGS: (...names) => {
      const all = ArrayIsArray(names[0]) ? names[0] : names;
      const wrapped = all.map((name: unknown) =>
        ArrayIsArray(name) ? name.map(n => `"${n}"`).join(" or ") : `"${name}"`,
      );
      return `The ${list(wrapped)} argument${all.length > 1 ? "s" : ""} must be specified`;
    },
    ERR_UNKNOWN_ENCODING: encoding => `Unknown encoding: ${stringOf(encoding)}`,
    ERR_INVALID_IP_ADDRESS: address => `Invalid IP address: ${address}`,
    ERR_OPERATION_FAILED: reason => `Operation failed: ${stringOf(reason)}`,
    ERR_INVALID_THIS: type => `Value of "this" must be of type ${type}`,
    ERR_METHOD_NOT_IMPLEMENTED: method => `The ${method} method is not implemented`,
    ERR_STREAM_DESTROYED: method => `Cannot call ${method} after a stream was destroyed`,
    ERR_STREAM_ALREADY_FINISHED: method => `Cannot call ${method} after a stream was finished`,
    ERR_INVALID_STATE: state => `Invalid state: ${state}`,
    ERR_USE_AFTER_CLOSE: what => `${what} was closed`,
    ERR_INVALID_URL_SCHEME: scheme => `The URL must be of scheme ${scheme}`,
    ERR_DIR_CLOSED: () => "Directory handle was closed",
    ERR_BUFFER_OUT_OF_BOUNDS: name =>
      name ? `"${name}" is outside of buffer bounds` : "Attempt to access memory outside buffer bounds",
  };
  function makeErrorWithCode(code: number, ...args: unknown[]) {
    const format = MESSAGES[native.errorCodes[code]];
    return native.makeError(code, format ? format(...args) : args.map(arg => stringOf(arg)).join(" "));
  }
  function makeAbortError(message = "The operation was aborted", options?: { cause?: unknown }) {
    const error: any = new Error(message, options?.cause === undefined ? undefined : { cause: options.cause });
    error.name = "AbortError";
    error.code = "ABORT_ERR";
    return error;
  }

  // ───────────────────────── the module registry ─────────────────────────
  const internalModuleRegistry = {};
  // What Boa has no implementation of yet: a module that reads one still loads. Calling it, or what it holds
  // (`const { a, b } = $cpp(…)`, `[a, b] = $cpp(…)`), throws and names it.
  const missing = (what: string): any =>
    new Proxy(
      function () {
        throw new Error(`${what} is not implemented on Boa yet`);
      },
      {
        get(target, key) {
          if (key === Symbol.iterator)
            return function* () {
              for (let i = 0; ; i++) yield missing(`${what}[${i}]`);
            };
          if (typeof key === "symbol" || key === "then" || key === "prototype") return (target as any)[key];
          // `binding.readSync.bind(binding)` runs when a module loads; the function it makes is what throws.
          if (key === "bind") return () => missing(what);
          return missing(`${what}.${key}`);
        },
        construct() {
          throw new Error(`${what} is not implemented on Boa yet`);
        },
      },
    );
  const intrinsic = (name: string) => (name in intrinsics ? intrinsics[name] : missing("$" + name));

  // JSStringDecoderConstructor::call: called without `new` on an object, the constructor makes that object a
  // decoder, which holds one in its private `decode` (express's body-parser does it). A Boa constructor does not see
  // `this`, so this function stands in front of the Rust class (src/runtime_js/node/string_decoder.rs).
  function stringDecoder(Native: new (encoding?: unknown) => object) {
    const StringDecoder = function StringDecoder(this: unknown) {
      // `StringDecoder(encoding)` is given an undefined `this`, not globalThis.
      "use strict";
      const encoding = arguments[0];
      if (new.target) return Reflect.construct(Native, [encoding], new.target);
      const decoder: any = new Native(encoding);
      if (((typeof this === "object" && this !== null) || typeof this === "function") && this !== StringDecoder) {
        const hidden = { writable: true, enumerable: false, configurable: true };
        defineProperty(this, "encoding", { ...hidden, value: decoder.encoding });
        defineProperty(this, "__intrinsic__decode", { ...hidden, value: decoder });
        return this;
      }
      return decoder;
    } as unknown as { prototype: object };
    defineProperty(StringDecoder, "prototype", { value: Native.prototype, writable: false });
    defineProperty(Native.prototype, "constructor", {
      value: StringDecoder,
      writable: true,
      enumerable: false,
      configurable: true,
    });
    return StringDecoder;
  }

  const NATIVE_MODULES: Record<string, () => unknown> = {
    "bun": () => G.Bun,
    "node:process": () => G.process,
    "node:buffer": () => bufferModule,
    "node:util/types": () => utilTypes,
    // Node's lib/constants.js: the os constants flattened, then fs's and crypto's.
    "node:constants": () => {
      const { dlopen, errno, priority, signals } = processBindingConstants.os as Record<string, object>;
      return { ...dlopen, ...errno, ...priority, ...signals, ...processBindingConstants.fs, ...processBindingConstants.crypto };
    },
    "node:string_decoder": () => ({ StringDecoder: stringDecoder(native.StringDecoder) }),
    "bun:jsc": () => bunJSC,
    "node:module": () => nodeModule,
  };
  function createInternalModuleById(id: number) {
    const existing = getInternalField(internalModuleRegistry, id);
    if (existing !== undefined) return existing;
    let exports: unknown;
    if (native.native[id]) {
      const make = NATIVE_MODULES[native.specifiers[id]];
      if (!make) throw new Error(`${native.specifiers[id]} is not implemented on Boa yet`);
      exports = make();
    } else {
      const factory = native.compile(id)!;
      exports = ReflectApply(factory(...native.intrinsicNames(id).map(intrinsic)), undefined, []);
    }
    putInternalField(internalModuleRegistry, id, exports);
    return exports;
  }

  // The `$cpp` natives that are plain JavaScript here, by `file:symbol` (registry.json names them).
  const ONLY_ENUMERABLE = 2;
  const SKIP_SYMBOLS = 16;
  const CPP: Record<string, () => unknown> = {
    "UtilInspect.cpp:js2native_wrap_jsFunctionGetOwnNonIndexProperties": () => (object: object, filter: number) =>
      Reflect.ownKeys(object).filter(
        key =>
          !(typeof key === "string" && /^(0|[1-9]\d*)$/.test(key) && Number(key) < 2 ** 32 - 1) &&
          !(filter & SKIP_SYMBOLS && typeof key === "symbol") &&
          !(filter & ONLY_ENUMERABLE && !getOwnPropertyDescriptor(object, key)?.enumerable),
      ),
    "NodeUtilTypesModule.cpp:js2native_wrap_jsFunctionIsMapIterator": () => utilTypes.isMapIterator,
    "NodeUtilTypesModule.cpp:js2native_wrap_jsFunctionIsSetIterator": () => utilTypes.isSetIterator,
    "NodeUtilTypesModule.cpp:js2native_wrap_jsFunctionIsError": () => utilTypes.isNativeError,
    "NodeUtilTypesModule.cpp:js2native_wrap_jsFunctionIsDeepStrictEqual": () =>
      typeof G.Bun?.deepEquals === "function"
        ? (a: unknown, b: unknown) => G.Bun.deepEquals(a, b, true)
        : missing("util.isDeepStrictEqual"),
    "BunProcess.cpp:js2native_wrap_jsFunctionIsStandardOutputStream": () => () => false,
    "BunProcess.cpp:js2native_wrap_jsFunctionReportUncaughtException": () => (error: unknown) => console.error(error),
    "stringWidth.cpp:js2native_wrap_jsFunctionBunStringWidth": () =>
      typeof G.Bun?.stringWidth === "function" ? G.Bun.stringWidth : (s: string) => String(s).length,
    // There is no Bun.ModuleGraph on Boa: no frame is inside one.
    "ModuleGraph.cpp:js2native_wrap_jsFunctionModuleGraphOfFrame": () => () => undefined,
    "ModuleGraph.cpp:js2native_wrap_jsFunctionModuleGraphFrameOfFrame": () => () => undefined,
    // bun:sql loads every adapter. `init` hands the driver the callbacks of its queries; there is no driver of
    // PostgreSQL or MySQL on Boa yet, so only connecting throws.
    // js/worker.ts's.
    "Worker.cpp:createNodeWorkerThreadsBinding": () => G[Symbol.for("bun.boa.worker")].binding,
    "Worker.cpp:js2native_wrap_jsFunctionPostMessage": () => G[Symbol.for("bun.boa.worker")].postMessage,
    "postgres.bind.ts:binding": () => sqlDriverBinding("PostgreSQL"),
    "mysql.bind.ts:binding": () => sqlDriverBinding("MySQL"),
  };
  function sqlDriverBinding(name: string) {
    const unported = () => {
      throw new Error(`Bun.SQL's ${name} adapter is not implemented on Boa yet`);
    };
    return { init() {}, createConnection: unported, createQuery: unported };
  }

  const lazyValues = new Map<number, unknown>();
  function lazy(id: number) {
    if (lazyValues.has(id)) return lazyValues.get(id);
    const name = native.nativeName(id);
    const value = CPP[name]?.() ?? native.lazy(id) ?? missing(name);
    lazyValues.set(id, value);
    return value;
  }

  // ───────────────────────── node:util/types and process.binding('constants') ─────────────────────────
  const brand = native.brand;
  const isTypedArray = (value: unknown) => brand(value) === "TypedArray";
  const tag = (value: unknown) =>
    isTypedArray(value)
      ? (Object.getOwnPropertyDescriptor(Object.getPrototypeOf(Uint8Array.prototype), Symbol.toStringTag)!.get!.call(
          value,
        ) as string)
      : undefined;
  const utilTypes = {
    isPromise: (v: unknown) => brand(v) === "Promise",
    isMap: (v: unknown) => brand(v) === "Map",
    isSet: (v: unknown) => brand(v) === "Set",
    isWeakMap: (v: unknown) => brand(v) === "WeakMap",
    isWeakSet: (v: unknown) => brand(v) === "WeakSet",
    isRegExp: (v: unknown) => brand(v) === "RegExp",
    isProxy: (v: unknown) => brand(v) === "Proxy",
    isDataView: (v: unknown) => brand(v) === "DataView",
    isArrayBuffer: (v: unknown) => brand(v) === "ArrayBuffer",
    isAnyArrayBuffer: (v: unknown) => brand(v) === "ArrayBuffer" || brand(v) === "SharedArrayBuffer",
    isSharedArrayBuffer: (v: unknown) => brand(v) === "SharedArrayBuffer",
    isArrayBufferView: (v: unknown) => ArrayBuffer.isView(v),
    isTypedArray,
    isUint8Array: (v: unknown) => tag(v) === "Uint8Array",
    isUint8ClampedArray: (v: unknown) => tag(v) === "Uint8ClampedArray",
    isUint16Array: (v: unknown) => tag(v) === "Uint16Array",
    isUint32Array: (v: unknown) => tag(v) === "Uint32Array",
    isInt8Array: (v: unknown) => tag(v) === "Int8Array",
    isInt16Array: (v: unknown) => tag(v) === "Int16Array",
    isInt32Array: (v: unknown) => tag(v) === "Int32Array",
    isFloat16Array: (v: unknown) => tag(v) === "Float16Array",
    isFloat32Array: (v: unknown) => tag(v) === "Float32Array",
    isFloat64Array: (v: unknown) => tag(v) === "Float64Array",
    isBigInt64Array: (v: unknown) => tag(v) === "BigInt64Array",
    isBigUint64Array: (v: unknown) => tag(v) === "BigUint64Array",
    isDate: (v: unknown) => v instanceof Date,
    isNativeError: (v: unknown) => v instanceof Error,
    isGeneratorFunction: (v: unknown) =>
      typeof v === "function" && /^(async\s+)?function\s*\*/.test(Function.prototype.toString.call(v)),
    isAsyncFunction: (v: unknown) => typeof v === "function" && /^async\b/.test(Function.prototype.toString.call(v)),
    isBoxedPrimitive: (v: unknown) =>
      v instanceof Number || v instanceof String || v instanceof Boolean || v instanceof BigInt || v instanceof Symbol,
    isNumberObject: (v: unknown) => v instanceof Number,
    isStringObject: (v: unknown) => v instanceof String,
    isBooleanObject: (v: unknown) => v instanceof Boolean,
    isBigIntObject: (v: unknown) => v instanceof BigInt,
    isSymbolObject: (v: unknown) => v instanceof Symbol,
    isMapIterator: (v: unknown) => Object.prototype.toString.call(v) === "[object Map Iterator]",
    isSetIterator: (v: unknown) => Object.prototype.toString.call(v) === "[object Set Iterator]",
    isGeneratorObject: (v: unknown) => Object.prototype.toString.call(v) === "[object Generator]",
    isModuleNamespaceObject: (v: unknown) => Object.prototype.toString.call(v) === "[object Module]",
    isArgumentsObject: (v: unknown) => Object.prototype.toString.call(v) === "[object Arguments]",
    isExternal: () => false,
    isKeyObject: () => false,
    isCryptoKey: () => false,
  };
  const processBindingConstants: Record<string, Record<string, unknown>> = {
    os: native.osConstants,
    fs: native.fsConstants,
    crypto: {},
    zlib: {},
  };

  // ───────────────────────── bun:jsc and node:module ─────────────────────────
  // The exports of src/jsc/modules/BunJSCModule.h. What only JavaScriptCore can say (its JIT, its heap) is a stub.
  const identity = (value: unknown) => value;
  const heapStats = () => ({
    heapSize: 0,
    heapCapacity: 0,
    extraMemorySize: 0,
    objectCount: 0,
    protectedObjectCount: 0,
    globalObjectCount: 1,
    protectedGlobalObjectCount: 0,
    objectTypeCounts: {},
    protectedObjectTypeCounts: {},
  });
  const bunJSC: Record<string, unknown> = {
    heapStats,
    memoryUsage: () => ({ current: 0, peak: 0, currentCommit: 0, peakCommit: 0, pageFaults: 0 }),
    heapSize: () => 0,
    drainMicrotasks: () => {},
    edenGC: () => 0,
    fullGC: () => 0,
    gcAndSweep: () => 0,
    releaseWeakRefs: () => {},
    noInline: identity,
    noFTL: identity,
    noOSRExitFuzzing: identity,
    optimizeNextInvocation: identity,
    numberOfDFGCompiles: () => 0,
    reoptimizationRetryCount: () => 0,
    totalCompileTime: () => 0,
    isRope: () => false,
    describe: (value: unknown) => stringOf(value),
    describeArray: (value: unknown) => stringOf(value),
    jscDescribe: (value: unknown) => stringOf(value),
    jscDescribeArray: (value: unknown) => stringOf(value),
    serialize: (value: unknown) => structuredClone(value),
    deserialize: (value: unknown) => structuredClone(value),
    estimateShallowMemoryUsageOf: () => 0,
    percentAvailableMemoryInUse: () => 0,
    getProtectedObjects: () => [],
    setTimeZone: identity,
    setTimezone: identity,
    /** The URL of the module that called it: the first frame of the stack in a file (this one has none). */
    callerSourceOrigin() {
      const file = String(new Error().stack ?? "")
        .split("\n")
        .map(line => /\(?((?:\/|[A-Za-z]:\\)[^()]*?):\d+:\d+\)?\s*$/.exec(line)?.[1])
        .find(file => file !== undefined);
      return file === undefined ? "" : `file://${file}`;
    },
  };
  for (const name of [
    "bytecodeOrderStats",
    "getRandomSeed",
    "setRandomSeed",
    "startSamplingProfiler",
    "samplingProfilerStackTraces",
    "startRemoteDebugger",
    "generateHeapSnapshotForDebugging",
    "profile",
    "codeCoverageForFile",
  ])
    bunJSC[name] = missing(`bun:jsc ${name}`);

  const builtinModules = native.specifiers.filter(
    (s, id) =>
      /^(node|bun):/.test(s) && !native.specifiers[id].startsWith("node:_") && s !== "bun:internal-for-testing",
  );
  const nodeModule: Record<string, unknown> = {
    builtinModules: [...new Set(builtinModules.map(s => (s.startsWith("node:") ? s.slice(5) : s)))].sort(),
    isBuiltin: (name: string) => builtinModules.includes(name) || builtinModules.includes("node:" + name),
    createRequire: (from: string | URL) => {
      const path = String(from instanceof URL ? from.pathname : from).replace(/^file:\/\//, "");
      return native.createRequire(path.endsWith("/") ? path : path.slice(0, path.lastIndexOf("/") + 1) || "/");
    },
    _cache: {},
    _extensions: { ".js": () => {}, ".json": () => {}, ".node": () => {} },
    globalPaths: [],
    wrap: (script: string) => `(function (exports, require, module, __filename, __dirname) { ${script}\n});`,
    syncBuiltinESMExports: () => {},
    findSourceMap: () => undefined,
    register: missing("node:module register"),
    SourceMap: missing("node:module SourceMap"),
  };
  nodeModule.Module = nodeModule;
  nodeModule.default = nodeModule;

  // ───────────────────────── the intrinsics ─────────────────────────
  const natives = new Proxy({} as Record<string | symbol, unknown>, {
    get: (snapshot, key) => (key in snapshot ? snapshot[key] : G[key]),
  });
  for (const name of [
    "setTimeout",
    "setInterval",
    "setImmediate",
    "clearTimeout",
    "clearInterval",
    "clearImmediate",
    "queueMicrotask",
  ])
    if (name in G) (natives as any)[name] = G[name];

  const intrinsics: Record<string, unknown> = {
    undefined: undefined,
    Infinity,
    Array,
    ArrayBuffer,
    String,
    Object,
    Promise,
    Map,
    RegExp,
    Uint8Array,
    Uint8ClampedArray,
    Uint16Array,
    Uint32Array,
    Int8Array,
    Int16Array,
    Int32Array,
    Float16Array: G.Float16Array,
    Float32Array,
    Float64Array,
    BigInt64Array,
    BigUint64Array,
    Buffer: G.Buffer,
    AbortSignal: G.AbortSignal,
    ReadableStream: G.ReadableStream,
    WritableStream: G.WritableStream,
    TransformStream: G.TransformStream,
    min: Math.min,
    isFinite: Number.isFinite,
    natives,
    lazy,
    internalModuleRegistry,
    createInternalModuleById,
    getInternalField,
    putInternalField,
    asyncContext,
    makeErrorWithCode,
    makeAbortError,
    makeTypeError: (message: string) => new TypeError(message),
    throwTypeError: (message: string) => {
      throw new TypeError(message);
    },
    throwRangeError: (message: string) => {
      throw new RangeError(message);
    },
    isCallable: (v: unknown) => typeof v === "function",
    isObject: (v: unknown) => (typeof v === "object" && v !== null) || typeof v === "function",
    isArray: ArrayIsArray,
    isJSArray: (v: unknown) => brand(v) === "Array",
    isPromise: utilTypes.isPromise,
    isMap: utilTypes.isMap,
    isSet: utilTypes.isSet,
    isRegExpObject: utilTypes.isRegExp,
    isProxyObject: utilTypes.isProxy,
    isTypedArrayView: (v: unknown) => ArrayBuffer.isView(v),
    isUndefinedOrNull: (v: unknown) => v == null,
    proxyFieldTarget: 0,
    proxyFieldHandler: 1,
    getProxyInternalField: (proxy: object, field: 0 | 1) => native.proxyField(proxy, field),
    inherits: (id: number, value: unknown) => {
      const name = native.className(id);
      // JSArrayBuffer is the class of a SharedArrayBuffer too.
      if (name === "ArrayBuffer") return utilTypes.isAnyArrayBuffer(value);
      return name !== undefined && typeof G[name] === "function" && value instanceof G[name];
    },
    toClass(fn: Function, name: string, base?: Function | null) {
      defineProperty(fn, "name", { value: name, configurable: true });
      fn.prototype = Object.create(base?.prototype ?? Object.prototype, {
        constructor: { value: fn, writable: true, configurable: true },
      });
      Object.setPrototypeOf(fn, base ?? Function.prototype);
    },
    toLength: (v: unknown) => {
      const n = Math.trunc(Number(v)) || 0;
      return n <= 0 ? 0 : Math.min(n, Number.MAX_SAFE_INTEGER);
    },
    newArrayWithSize: (n: number) => new Array(n),
    arrayPush: (array: unknown[], value: unknown) =>
      void defineProperty(array, array.length, { value, writable: true, enumerable: true, configurable: true }),
    getByIdDirect: (object: any, key: PropertyKey) => object[key],
    putByValDirect: (object: object, key: PropertyKey, value: unknown) =>
      void defineProperty(object, key, { value, writable: true, enumerable: true, configurable: true }),
    putByIdDirectPrivate: (object: object, key: string, value: unknown) => member(object, key, value),
    newPromise,
    resolvePromise,
    rejectPromise,
    resolvePromiseWithFirstResolvingFunctionCallCheck: resolvePromise,
    rejectPromiseWithFirstResolvingFunctionCallCheck: rejectPromise,
    pokePromiseAsHandled,
    peekPromiseStatus: native.promiseState,
    peekPromiseSettledValue: native.promiseResult,
    processBindingConstants,
    requireMap: new Map(),
  };

  // Bun.inspect is Rust in JavaScriptCore's build (the console's formatter). Here it is util.inspect, from
  // src/js/internal/util/inspect.js, until that formatter runs on Boa.
  if (G.Bun && !G.Bun.inspect) {
    const id = native.specifiers.indexOf("internal:util/inspect");
    let inspecting = false;
    const inspect = function inspect(value: unknown, options?: object) {
      if (inspecting) return String(value);
      inspecting = true;
      try {
        const text: string = (createInternalModuleById(id) as any).inspect(value, options);
        // JavaScriptCore's formatter names a function as `getName` does, a bound one by its target: `[Function: f]`.
        const name = typeof value === "function" ? native.functionName(value) : undefined;
        const shown = typeof value === "function" ? (value as Function).name : undefined;
        if (name && typeof shown === "string" && name !== shown) {
          const head = text.slice(0, text.indexOf(": ") + 2);
          if (text.startsWith(`${head}${shown}]`)) return head + name + text.slice(head.length + shown.length);
        }
        return text;
      } finally {
        inspecting = false;
      }
    };
    inspect.custom = Symbol.for("nodejs.util.inspect.custom");
    inspect.table = missing("Bun.inspect.table");
    G.Bun.inspect = inspect;
  }

  // Bun.SQL, Bun.sql and Bun.postgres: bun:sql's exports, loaded the first time one is read (BunObject.cpp's
  // `createBunSQLConstructor` and `createBunSql`).
  if (G.Bun && !("SQL" in G.Bun)) {
    const id = native.specifiers.indexOf("bun:sql");
    const own = (name: string, value: unknown) =>
      defineProperty(G.Bun, name, { value, writable: true, enumerable: true, configurable: true });
    for (const [name, key] of [
      ["SQL", "SQL"],
      ["sql", "default"],
      ["postgres", "default"],
    ])
      defineProperty(G.Bun, name, {
        get() {
          const value = (createInternalModuleById(id) as any)[key];
          own(name, value);
          return value;
        },
        set: value => own(name, value),
        enumerable: true,
        configurable: true,
      });
  }

  // ───────────────────────── builtin functions and Buffer ─────────────────────────
  // A builtin function of src/js/builtins, made of the intrinsics it names, as JavaScriptCore links one.
  function builtinFunction(file: string, name: string): Function {
    const id = native.functions.indexOf(`${file}.${name}`);
    if (id < 0) throw new Error(`${file}.${name} is not a builtin function`);
    const fn = ReflectApply(native.compileFunction(id), undefined, native.functionIntrinsicNames(id).map(intrinsic));
    if (native.getters[id]) defineProperty(fn, "name", { value: `get ${name}`, configurable: true });
    return fn;
  }
  const codeNumber = (code: string) => {
    const number = native.errorCodes.indexOf(code);
    if (number < 0) throw new Error(`${code} is not an error code`);
    return number;
  };
  // `Buffer` exists before any module of src/js is compiled: `$Buffer` is it.
  const bufferModule = native.installBuffer({
    natives: native.buffer,
    makeError: (code: string, ...args: unknown[]) => makeErrorWithCode(codeNumber(code), ...args),
    error: (code: string, message: string) => native.makeError(codeNumber(code), message),
    specificType,
    brand,
    provide(Buffer: Function) {
      intrinsics.Buffer = Buffer;
      defineProperty(G, "Buffer", { value: Buffer, writable: true, enumerable: false, configurable: true });
    },
    builtinFunction,
  });

  native.installed = {
    require: createInternalModuleById,
    esm(id: number) {
      const exports: any = createInternalModuleById(id);
      const pairs: [string, unknown][] = [["default", exports]];
      if ((typeof exports === "object" && exports !== null) || typeof exports === "function")
        for (const key of keys(exports)) if (key !== "default") pairs.push([key, exports[key]]);
      return pairs;
    },
  };
})((globalThis as any).__bun_intrinsics_ops__);
delete (globalThis as any).__bun_intrinsics_ops__;
