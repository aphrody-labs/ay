// mock(), jest.fn(), vi.fn() and spyOn(): a port of src/jsc/bindings/JSMockFunction.cpp.
import { native } from "./state.ts";

type Fn = (...args: any[]) => any;

type Kind = "call" | "returnValue" | "returnThis" | "rejectedValue" | "throwValue" | "nothing";
interface Impl {
  kind: Kind;
  value: unknown;
  once: boolean;
  next?: Impl;
  fn?: Fn;
}

interface Result {
  type: "return" | "throw" | "incomplete";
  value: unknown;
}

const MOCK = Symbol("bun:test:mock");

interface MockData {
  calls: unknown[][] | undefined;
  contexts: unknown[] | undefined;
  instances: unknown[] | undefined;
  results: Result[] | undefined;
  invocationCallOrder: number[] | undefined;
  state: object | undefined;
  implementation: Impl | undefined;
  fallback: Impl | undefined;
  tail: Impl | undefined;
  mockName: string | undefined;
  initial: Fn | undefined;
  name: string;
  isVitest: boolean;
  isJest: boolean;
  isSpy: boolean;
  /** Made by vi.mockObject(): an instance it constructs gets mocks of the mocked methods of its prototype. */
  isAutomock?: boolean;
  /** Of such an instance's method mock: the mock of the same method on the class prototype, which also records. */
  prototypeMock?: Fn;
  spy?: { target: any; key: PropertyKey; original: PropertyDescriptor | undefined; accessor?: "get" | "set" | "value" };
  prototype?: object;
}

let invocationId = 0;
const called = new Set<Fn>();
const configured = new Set<Fn>();
const spies = new Set<Fn>();

const dataOf = (f: unknown): MockData | undefined => (typeof f === "function" ? (f as any)[MOCK] : undefined);
export const isMockFunction = (f: unknown): boolean => dataOf(f) !== undefined;

/** Node's `determineSpecificType`, as ErrorCode.cpp describes a receiver. */
function specificType(v: unknown): string {
  if (v === null) return "null";
  if (typeof v === "function") return v.name ? `function ${v.name}` : "function ";
  if (typeof v === "object") {
    const name = (v as any).constructor?.name;
    return name ? `an instance of ${name}` : "[object Object]";
  }
  const shown = typeof v === "string" ? `'${v.length > 28 ? v.slice(0, 25) + "..." : v}'` : String(v);
  return `type ${typeof v} (${shown})`;
}

/** `throwInvalidThisError(…, "Mock")` in JSMockFunction.cpp. */
function check(f: unknown): MockData {
  const data = dataOf(f);
  if (data) return data;
  const error = new TypeError(
    f === undefined ? "Expected this to be instanceof Mock" : `Expected this to be instanceof Mock, but received ${specificType(f)}`,
  );
  (error as any).code = "ERR_INVALID_THIS";
  throw error;
}

function setFallback(data: MockData, kind: Kind, value: unknown) {
  if (data.fallback) {
    data.fallback.kind = kind;
    data.fallback.value = value;
    data.fallback.fn = undefined;
    return;
  }
  const impl: Impl = { kind, value, once: false };
  data.fallback = impl;
  if (data.tail) data.tail.next = impl;
  else data.implementation = impl;
}
function pushImpl(f: Fn, kind: Kind, value: unknown) {
  const data = check(f);
  configured.add(f);
  setFallback(data, kind, value);
}
function pushImplOnce(f: Fn, kind: Kind, value: unknown) {
  const data = check(f);
  configured.add(f);
  const impl: Impl = { kind, value, once: true };
  if (data.tail) data.tail.next = impl;
  else data.implementation = impl;
  if (data.fallback) impl.next = data.fallback;
  data.tail = impl;
}
function takeNext(data: MockData): Impl | undefined {
  const impl = data.implementation;
  if (impl && impl.once) {
    data.implementation = impl.next;
    if (!impl.next || !impl.next.once) data.tail = undefined;
  }
  return impl;
}

function record(f: Fn, data: MockData, args: unknown[], thisValue: unknown, result: Result, isNew: boolean) {
  called.add(f);
  (data.calls ??= []).push(args);
  (data.contexts ??= []).push(thisValue);
  if (isNew || data.instances) (data.instances ??= [...data.contexts.slice(0, -1)]).push(thisValue);
  (data.invocationCallOrder ??= []).push(invocationId);
  (data.results ??= []).push(result);
}

function run(impl: Impl, thisValue: unknown, args: unknown[]): unknown {
  switch (impl.kind) {
    case "returnValue":
    case "nothing":
      return impl.value;
    case "returnThis":
      return thisValue;
    case "rejectedValue":
      return Promise.reject(impl.value);
    case "throwValue":
      throw impl.value;
    case "call":
      if (typeof impl.value !== "function") throw new TypeError("Expected mock implementation to be callable");
      return Reflect.apply(impl.value as Fn, thisValue, args);
  }
}

const isClassConstructor = (f: unknown) => {
  try {
    return typeof f === "function" && /^class\b/.test(Function.prototype.toString.call(f));
  } catch {
    return false;
  }
};

const isConstructor = (f: unknown) => {
  if (typeof f !== "function") return false;
  try {
    Reflect.construct(String, [], f);
    return true;
  } catch {
    return false;
  }
};
/** A `function` written in JavaScript: Jest calls it on the new object instead of constructing it. */
const isPlainFunction = (f: unknown) => {
  if (!isConstructor(f) || isClassConstructor(f)) return false;
  try {
    return !/\{\s*\[native code\]\s*\}\s*$/.test(Function.prototype.toString.call(f));
  } catch {
    return false;
  }
};

function createMock(isVitest: boolean): Fn {
  const data: MockData = {
    calls: undefined,
    contexts: undefined,
    instances: undefined,
    results: undefined,
    invocationCallOrder: undefined,
    state: undefined,
    implementation: undefined,
    fallback: undefined,
    tail: undefined,
    mockName: undefined,
    initial: undefined,
    name: "mockConstructor",
    isVitest,
    isJest: false,
    isSpy: false,
  };
  const f = function (this: unknown, ...args: unknown[]) {
    const isNew = new.target !== undefined;
    const prototypeMock = data.prototypeMock;
    const impl = takeNext(data) ?? (prototypeMock ? takeNext(check(prototypeMock)) : undefined);
    if (!isNew) {
      invocationId++;
      const runs = impl && (impl.kind === "call" || impl.kind === "throwValue");
      const result: Result = { type: runs ? "incomplete" : "return", value: undefined };
      let value: unknown;
      if (impl && !runs) value = run(impl, this, args);
      result.value = value;
      record(f, data, args, this, result, false);
      if (prototypeMock) record(prototypeMock, check(prototypeMock), args, this, result, false);
      if (!runs) return value;
      try {
        value = run(impl!, this, args);
      } catch (error) {
        result.type = "throw";
        result.value = error;
        throw error;
      }
      result.type = "return";
      result.value = value;
      return value;
    }
    // `new mock()`: Jest calls the implementation on the new object, except a class, which it constructs.
    invocationId++;
    const constructor = impl && impl.kind === "call" ? (impl.value as Fn) : undefined;
    const constructs = isConstructor(constructor) && (isVitest || !isPlainFunction(constructor));
    const thisValue = constructs ? undefined : this;
    const result: Result = { type: "incomplete", value: undefined };
    record(f, data, args, thisValue, result, true);
    if (prototypeMock) record(prototypeMock, check(prototypeMock), args, thisValue, result, true);
    let returned: unknown;
    try {
      if (constructs) returned = Reflect.construct(constructor!, args, new.target);
      else if (impl) returned = run(impl, thisValue, args);
    } catch (error) {
      result.type = "throw";
      result.value = error;
      throw error;
    }
    let resultValue = returned;
    if (typeof returned !== "object" || returned === null) returned = thisValue;
    if (isVitest) resultValue = returned;
    if (data.isAutomock) mockInstanceMethods(f, returned as object);
    result.type = "return";
    result.value = resultValue;
    if (constructs) {
      data.contexts![data.contexts!.length - 1] = returned;
      data.instances![data.instances!.length - 1] = returned;
      if (prototypeMock) {
        const protoData = check(prototypeMock);
        protoData.contexts![protoData.contexts!.length - 1] = returned;
        protoData.instances![protoData.instances!.length - 1] = returned;
      }
    }
    return returned;
  } as Fn & Record<PropertyKey, any>;
  Object.defineProperty(f, MOCK, { value: data });
  Object.setPrototypeOf(f, MockPrototype);
  setName(f, "mockConstructor");
  return f;
}

function setName(f: Fn, name: string) {
  check(f).name = name;
  Object.defineProperty(f, "name", { value: name, writable: false, enumerable: false, configurable: true });
}

function copyNameAndLength(mock: Fn, fn: Fn, name?: string) {
  if (name === undefined) {
    let n = "";
    try {
      n = typeof fn.name === "string" ? fn.name : "";
    } catch {}
    setName(mock, n);
  } else setName(mock, name);
  try {
    const length = fn.length;
    if (typeof length === "number") Object.defineProperty(mock, "length", { value: length, writable: false, enumerable: false, configurable: true });
  } catch {}
  // Static properties of the function (and of what it extends below Function.prototype).
  for (let holder: any = fn; holder && holder !== Function.prototype && holder !== Object.prototype; holder = Object.getPrototypeOf(holder)) {
    for (const key of Reflect.ownKeys(holder)) {
      if (key === "length" || key === "name" || key === "prototype") continue;
      if (key === Symbol.for("nodejs.util.promisify.custom")) continue;
      if (key in mock) continue;
      const desc = Object.getOwnPropertyDescriptor(holder, key);
      if (desc) Object.defineProperty(mock, key, desc);
    }
  }
}

function wrap(mock: Fn, fn: Fn, name?: string) {
  copyNameAndLength(mock, fn, name);
  const data = check(mock);
  if (!data.isVitest) pushImpl(mock, "call", fn);
  else {
    data.initial = fn;
    setFallback(data, "call", fn);
  }
}

function clear(f: Fn) {
  const data = check(f);
  data.calls = undefined;
  data.contexts = undefined;
  data.instances = undefined;
  data.results = undefined;
  data.invocationCallOrder = undefined;
  if (data.state && !data.isVitest) data.state = undefined;
  else if (data.state) {
    // The state keeps its getters (stateOf), which answer the new lists.
    data.calls = [];
    data.contexts = [];
    data.instances = [];
    data.results = [];
    data.invocationCallOrder = [];
  }
  called.delete(f);
}
function reset(f: Fn) {
  clear(f);
  const data = check(f);
  data.tail = undefined;
  data.mockName = undefined;
  data.implementation = undefined;
  if (!data.initial) data.fallback = undefined;
  else if (data.fallback) data.implementation = data.fallback;
  if (data.initial) setFallback(data, "call", data.initial);
  configured.delete(f);
}
function restore(f: Fn) {
  reset(f);
  const data = check(f);
  const spy = data.spy;
  if (spy) {
    if (spy.original) Object.defineProperty(spy.target, spy.key, spy.original);
    else delete spy.target[spy.key];
    data.spy = undefined;
  }
  spies.delete(f);
}

function stateOf(f: Fn): object {
  const data = check(f);
  called.add(f);
  if (data.state) return data.state;
  data.calls ??= [];
  data.contexts ??= [];
  data.instances ??= [...data.contexts];
  data.results ??= [];
  data.invocationCallOrder ??= [];
  const state = {};
  const live = (key: "calls" | "contexts" | "instances" | "results" | "invocationCallOrder") => ({
    get: () => (data[key] ??= key === "instances" ? [...(data.contexts ?? [])] : []),
    enumerable: true,
    configurable: false,
  });
  Object.defineProperties(state, {
    calls: live("calls"),
    contexts: live("contexts"),
    instances: live("instances"),
    results: live("results"),
    invocationCallOrder: live("invocationCallOrder"),
    lastCall: {
      get() {
        const calls = (this as any).calls;
        return Array.isArray(calls) && calls.length ? calls[calls.length - 1] : undefined;
      },
      enumerable: false,
      configurable: true,
    },
    settledResults: {
      get() {
        const results = (this as any).results;
        if (!Array.isArray(results)) return undefined;
        // By index, not with map(): `results` can be nearly all holes, up to 2 ** 32 - 1 long.
        const settled: unknown[] = [];
        settled.length = results.length;
        for (const key of Object.keys(results)) if (String(Number(key) >>> 0) === key) settled[key as any] = settle(results[key as any]);
        return settled;
        function settle(r: any) {
          const type = r?.type;
          if (type === "throw") return { type: "rejected", value: r.value };
          if (type === "return") {
            const value = r.value;
            if (!(value instanceof Promise)) return { type: "fulfilled", value };
            const s = promiseState(value);
            if (s !== 0) return { type: s === 1 ? "fulfilled" : "rejected", value: promiseResult(value) };
          }
          return { type: "incomplete", value: undefined };
        }
      },
      enumerable: false,
      configurable: true,
    },
  });
  data.state = state;
  return state;
}

let promiseState: (p: Promise<unknown>) => number | undefined = () => 0;
let promiseResult: (p: Promise<unknown>) => unknown = () => undefined;
export function setPromiseInspector(state: (p: Promise<unknown>) => number | undefined, result: (p: Promise<unknown>) => unknown) {
  promiseState = state;
  promiseResult = result;
}

const returnThis = function (this: unknown) {
  return this;
};

const methods: Record<string, (this: Fn, ...args: any[]) => unknown> = {
  getMockImplementation() {
    const data = check(this);
    let impl = data.implementation;
    if (impl && impl.once) {
      const returnedBeforeCall = impl.kind === "call" || (data.isVitest && impl.kind !== "nothing");
      if (!returnedBeforeCall) impl = data.fallback;
    }
    if (!impl || impl.kind === "nothing") return undefined;
    if (impl.kind === "call") return impl.value;
    if (impl.fn) return impl.fn;
    const value = impl.value;
    impl.fn =
      impl.kind === "returnThis"
        ? returnThis
        : impl.kind === "rejectedValue"
          ? () => Promise.reject(value)
          : impl.kind === "throwValue"
            ? () => {
                throw value;
              }
            : () => value;
    return impl.fn;
  },
  getMockName() {
    const data = check(this);
    if (data.isJest || data.isVitest) {
      let name = data.mockName;
      if (!name && data.isVitest && data.isSpy) name = data.name;
      return name || (data.isVitest ? "vi.fn()" : "jest.fn()");
    }
    return data.name;
  },
  mockClear() {
    clear(this);
    return this;
  },
  mockReset() {
    reset(this);
    return this;
  },
  mockRestore() {
    restore(this);
    return this;
  },
  mockImplementation(fn?: unknown) {
    if (fn) pushImpl(this, "call", fn);
    else pushImpl(this, "nothing", undefined);
    return this;
  },
  mockImplementationOnce(fn?: unknown) {
    if (fn) pushImplOnce(this, "call", fn);
    else pushImplOnce(this, "nothing", undefined);
    return this;
  },
  withImplementation(fn: unknown, callback: unknown) {
    const data = check(this);
    if (typeof callback !== "function") throw new TypeError("Expected mock implementation to be callable");
    const saved = [data.implementation, data.tail, data.fallback] as const;
    data.implementation = { kind: "call", value: fn, once: false };
    data.tail = undefined;
    data.fallback = undefined;
    const back = () => {
      [data.implementation, data.tail, data.fallback] = saved;
    };
    let returned: unknown;
    try {
      returned = callback();
    } catch (error) {
      back();
      throw error;
    }
    if (returned instanceof Promise) return returned.finally(back);
    back();
    return data.isVitest ? this : undefined;
  },
  mockName(name?: unknown) {
    const data = check(this);
    configured.add(this);
    if (data.isVitest) {
      if (typeof name === "string") data.mockName = name;
    } else if (name) {
      if (data.isJest) data.mockName = String(name);
      else setName(this, String(name));
    }
    return this;
  },
  mockReturnThis() {
    pushImpl(this, "returnThis", undefined);
    return this;
  },
  mockReturnValue(value?: unknown) {
    pushImpl(this, "returnValue", value);
    return this;
  },
  mockReturnValueOnce(value?: unknown) {
    pushImplOnce(this, "returnValue", value);
    return this;
  },
  mockResolvedValue(value?: unknown) {
    pushImpl(this, "returnValue", Promise.resolve(value));
    return this;
  },
  mockResolvedValueOnce(value?: unknown) {
    pushImplOnce(this, "returnValue", Promise.resolve(value));
    return this;
  },
  mockRejectedValue(value?: unknown) {
    pushImpl(this, "rejectedValue", value);
    return this;
  },
  mockRejectedValueOnce(value?: unknown) {
    pushImplOnce(this, "rejectedValue", value);
    return this;
  },
  mockThrow(value?: unknown) {
    pushImpl(this, "throwValue", value);
    return this;
  },
  mockThrowOnce(value?: unknown) {
    pushImplOnce(this, "throwValue", value);
    return this;
  },
};

const MockPrototype: object = Object.create(Function.prototype);
for (const [name, fn] of Object.entries(methods)) {
  const length = name.startsWith("mock") && name !== "mockClear" && name !== "mockReset" && name !== "mockRestore" ? 1 : name === "withImplementation" ? 1 : 0;
  Object.defineProperty(fn, "name", { value: name });
  Object.defineProperty(fn, "length", { value: length });
  Object.defineProperty(MockPrototype, name, { value: fn, writable: false, enumerable: false, configurable: false });
}
Object.defineProperty(MockPrototype, "mock", {
  get(this: Fn) {
    return stateOf(this);
  },
  enumerable: false,
  configurable: false,
});
Object.defineProperty(MockPrototype, "_isMockFunction", { value: true, enumerable: true, writable: true, configurable: true });
Object.defineProperty(MockPrototype, Symbol.toStringTag, { value: "Mock", configurable: true });
Object.defineProperty(MockPrototype, Symbol.dispose, { value: methods.mockRestore, writable: true, configurable: true });

function mockFn(isVitest: boolean, args: unknown[]): Fn {
  if (isVitest && isMockFunction(args[0])) return args[0] as Fn;
  const mock = createMock(isVitest);
  const name = isVitest ? "Mock" : "mockConstructor";
  if (args.length > 0) {
    const value = args[0];
    if (typeof value === "function") wrap(mock, value as Fn, isVitest ? name : undefined);
    else {
      pushImpl(mock, "returnValue", value);
      setName(mock, name);
    }
  } else setName(mock, name);
  return mock;
}

/** bun:test's `mock()`. */
export const mock: any = function mock(...args: unknown[]) {
  return mockFn(false, args);
};
export const jestFn = function fn(...args: unknown[]) {
  const made = mockFn(false, args);
  check(made).isJest = true;
  return made;
};
export const viFn = function fn(...args: unknown[]) {
  return mockFn(true, args);
};

function spyOnImpl(isVitest: boolean, object: any, key: unknown, accessType?: unknown): Fn {
  if (arguments.length < 3 || !((typeof object === "object" && object !== null) || typeof object === "function"))
    throw new Error("spyOn(target, prop) expects a target object and a property key");
  const propertyKey = typeof key === "symbol" ? key : String(key);
  let access: "value" | "get" | "set" = "value";
  if (accessType) {
    if (accessType === "get") access = "get";
    else if (accessType === "set") access = "set";
    else throw new TypeError('spyOn(target, prop, accessType) expects accessType to be "get" or "set"');
  }
  let holder: any = object;
  let desc: PropertyDescriptor | undefined;
  while (holder) {
    desc = Object.getOwnPropertyDescriptor(holder, propertyKey);
    if (desc) break;
    holder = Object.getPrototypeOf(holder);
  }
  const hasValue = desc !== undefined;
  const isOwn = holder === object;
  const keyText = typeof propertyKey === "symbol" ? `Symbol(${propertyKey.description ?? ""})` : propertyKey;
  const ownDesc = isOwn ? desc : undefined;
  const isEasy = access === "value" && (!hasValue || "value" in desc!);

  if (isEasy) {
    const value = hasValue ? object[propertyKey] : undefined;
    if (isMockFunction(value)) return value;
    const mock = createMock(isVitest);
    const data = check(mock);
    data.isSpy = true;
    const configurable = !isOwn || desc!.configurable !== false;
    if (typeof value === "function") {
      wrap(mock, value);
      Object.defineProperty(object, propertyKey, {
        value: mock,
        writable: desc ? desc.writable !== false : true,
        enumerable: desc ? !!desc.enumerable && isOwn : true,
        configurable,
      });
    } else {
      if (propertyKey === "prototype" && typeof object === "function" && !isMockFunction(object))
        throw new Error("Cannot spy on the `prototype` property because it is not a function");
      pushImpl(mock, "returnValue", value);
      Object.defineProperty(object, propertyKey, {
        get: mock,
        set: mock,
        enumerable: desc ? !!desc.enumerable : true,
        configurable,
      });
    }
    data.spy = { target: object, key: propertyKey, original: ownDesc, accessor: "value" };
    spies.add(mock);
    return mock;
  }
  if (!hasValue) throw new TypeError(`spyOn(target, prop, accessType) expects target to have the property \`${keyText}\``);
  if (propertyKey === "prototype" && typeof object === "function" && !isMockFunction(object))
    throw new Error("Cannot spy on the `prototype` property because it is not a function");
  let getter = desc!.get;
  let setter = desc!.set;
  let original: unknown = access === "set" ? setter : getter;
  if (access === "value" && !(getter === setter && isMockFunction(getter))) {
    original = object[propertyKey];
    if (typeof original !== "function")
      throw new TypeError(
        `Cannot spy on the \`${keyText}\` property because it is not a function; ${original === null ? "null" : typeof original} given instead`,
      );
  }
  if (isMockFunction(original)) return original as Fn;
  const mock = createMock(isVitest);
  check(mock).isSpy = true;
  if (typeof original === "function") wrap(mock, original as Fn);
  else if (access === "get" && "value" in desc!) pushImpl(mock, "returnValue", desc!.value);
  const name = typeof propertyKey === "symbol" ? `[${propertyKey.description ?? ""}]` : propertyKey;
  if (access === "value") getter = () => mock;
  else if (access === "get") {
    setName(mock, `get ${name}`);
    getter = mock;
  } else {
    setName(mock, `set ${name}`);
    setter = mock;
  }
  Object.defineProperty(object, propertyKey, { get: getter, set: setter, enumerable: !!desc!.enumerable, configurable: true });
  check(mock).spy = { target: object, key: propertyKey, original: ownDesc, accessor: access };
  spies.add(mock);
  return mock;
}

export const spyOn = function spyOn(object: unknown, key: unknown, accessType?: unknown) {
  return spyOnImpl.apply(undefined, [false, ...arguments] as any);
} as (object: unknown, key: unknown, accessType?: unknown) => Fn;
export const jestSpyOn = function spyOn(object: unknown, key: unknown, accessType?: unknown) {
  const made = spyOnImpl.apply(undefined, [false, ...arguments] as any);
  const data = dataOf(made);
  if (data && !data.isVitest) data.isJest = true;
  return made;
};
export const viSpyOn = function spyOn(object: unknown, key: unknown, accessType?: unknown) {
  return spyOnImpl.apply(undefined, [true, ...arguments] as any);
};

export function clearAllMocks() {
  for (const f of [...called]) clear(f);
}
export function resetAllMocks() {
  for (const f of new Set([...called, ...configured])) reset(f);
}
export function restoreAllMocks() {
  for (const f of [...spies]) restore(f);
}

export const mocked = (value: unknown) => value;

/** mock.module(): replaces a module for the loader. */
/** The directory of the file that called: the first frame of the stack in a file outside of bun:test. */
function callerDir(): string {
  for (const line of String(new Error().stack ?? "").split("\n")) {
    const file = /\(?((?:\/|[A-Za-z]:\\)[^()]*?):\d+:\d+\)?\s*$/.exec(line)?.[1];
    if (file) return file.slice(0, Math.max(file.lastIndexOf("/"), file.lastIndexOf("\\")) + 1) || "/";
  }
  return "";
}

/**
 * `mock.module(specifier, factory)` (JSMock__jsModuleMock): what imports or requires `specifier`, resolved from the
 * calling file, gets what `factory` returns from now on, or what the promise it returns resolves to.
 */
export function moduleMock(specifier: unknown, factory: unknown) {
  if (typeof specifier !== "string") throw new TypeError("mock(module, fn) requires a module name string");
  if (typeof factory !== "function") throw new TypeError("mock(module, fn) requires a function");
  const dir = callerDir();
  const exports = factory();
  if (exports && (typeof exports === "object" || typeof exports === "function") && typeof exports.then === "function") {
    // Installed by the time the promise settles: an import that runs after an `await` of it sees the mock.
    return Promise.resolve(exports).then(value => void native.mockModule(specifier, dir, value));
  }
  native.mockModule(specifier, dir, exports);
}

// ── vi.mockObject(): the Automocker of JSMockFunction.cpp ─────────────────────
/** `mockInstanceMethods`: an instance an automocked class constructs gets a mock of each mocked method of the prototype. */
function mockInstanceMethods(fn: Fn, instance: object) {
  const prototype = (fn as any).prototype;
  if (typeof prototype !== "object" || prototype === null) return;
  for (const key of Reflect.ownKeys(prototype)) {
    if (key === "constructor") continue;
    const desc = Object.getOwnPropertyDescriptor(prototype, key);
    if (!desc || !("value" in desc)) continue;
    const method = desc.value;
    const methodData = dataOf(method);
    if (!methodData || (instance as any)[key] !== method) continue;
    const mock = createMock(methodData.isVitest);
    const data = check(mock);
    data.isJest = methodData.isJest;
    copyNameAndLength(mock, method);
    data.prototypeMock = method;
    (instance as any)[key] = mock;
  }
}

type AutomockKind = "keep" | "array" | "object" | "module" | "function";
type AutomockRole = "member" | "exports" | "prototype";

class Automocker {
  replacements = new Map<object, object>();
  pending: [object, object, AutomockKind][] = [];
  accessor: Fn | undefined;
  constructor(
    readonly spy: boolean,
    readonly leaveOriginalAlone: boolean,
  ) {}

  kindOf(value: unknown): AutomockKind {
    if ((typeof value !== "object" && typeof value !== "function") || value === null || isMockFunction(value)) return "keep";
    if (value === globalThis) return "keep";
    if (Array.isArray(value)) return "array";
    // Object.prototype.toString(): a Map, a Date, a Promise or an instance with a Symbol.toStringTag is kept.
    let tag: string;
    try {
      tag = Object.prototype.toString.call(value);
    } catch {
      // A Symbol.toStringTag getter can throw when it is read on the prototype itself: no tag, then.
      tag = typeof value === "function" ? "[object Function]" : "[object Object]";
    }
    if (tag === "[object Object]") return "object";
    if (tag === "[object Module]") return "module";
    if (typeof value === "function" && tag.includes("Function")) return "function";
    return "keep";
  }

  replacementFor(value: unknown, role: AutomockRole = "member"): unknown {
    if ((typeof value !== "object" && typeof value !== "function") || value === null) return value;
    const known = this.replacements.get(value);
    if (known !== undefined) return known;
    let kind = this.kindOf(value);
    const original = value as any;
    let replacement: any;
    switch (kind) {
      case "keep":
        return value;
      case "array":
        replacement = [];
        break;
      case "object": {
        // Spied in place, so that spied methods still find the private fields, slots and prototype of their `this`.
        let inPlace = this.spy;
        if (this.spy && this.leaveOriginalAlone && role === "prototype") {
          replacement = Object.create(original);
          break;
        }
        if (role === "exports" || (this.spy && this.leaveOriginalAlone)) {
          const prototype = Object.getPrototypeOf(original);
          if (typeof prototype !== "object" || prototype === null || prototype === Object.prototype) {
            inPlace = false;
            if (role === "exports") kind = "module";
          } else if (this.spy && this.leaveOriginalAlone) return value;
        }
        replacement = inPlace ? original : {};
        break;
      }
      case "module":
        replacement = moduleObject();
        break;
      case "function": {
        const mock = createMock(true);
        check(mock).isAutomock = true;
        copyNameAndLength(mock, original);
        if (this.spy) {
          const data = check(mock);
          data.initial = original;
          setFallback(data, "call", original);
        }
        replacement = mock;
        break;
      }
    }
    this.replacements.set(original, replacement);
    if (kind !== "array" || this.spy) this.pending.push([original, replacement, kind]);
    return replacement;
  }

  mockElements(original: any, replacement: unknown[]) {
    const length = Number(original.length) >>> 0;
    // By index: an array can be nearly all holes.
    for (const key of Object.keys(original)) {
      const i = Number(key);
      if (String(i >>> 0) !== key || i >= length) continue;
      replacement[i] = this.replacementFor(original[i]) as unknown;
    }
    replacement.length = length;
  }

  mockMembers(original: any, replacement: any, kind: AutomockKind) {
    // A transpiled module (`__esModule`) exports through getters, so those are read. No other getter is called.
    let isModule = kind === "module";
    if (!isModule) isModule = !!original.__esModule;
    const isFunction = kind === "function";
    const inPlace = original === replacement;
    const seen = new Set<PropertyKey>();
    for (
      let holder: any = original;
      holder && holder !== Object.prototype && holder !== Function.prototype && holder !== RegExp.prototype;
      holder = Object.getPrototypeOf(holder)
    ) {
      for (const key of Reflect.ownKeys(holder)) {
        if (seen.has(key)) continue;
        seen.add(key);
        if (isFunction && (key === "length" || key === "name" || key === "arguments" || key === "caller")) continue;
        if (kind === "module" && key === Symbol.toStringTag) continue;
        const desc = Object.getOwnPropertyDescriptor(holder, key);
        if (!desc) continue;
        let value: unknown;
        if ("value" in desc) value = desc.value;
        else if (isModule || typeof desc.get !== "function") value = original[key];
        else {
          if (inPlace) continue;
          if (!this.spy) {
            this.accessor ??= function () {} as Fn;
            desc.get = this.accessor;
            if (typeof desc.set === "function") desc.set = this.accessor;
          }
          Object.defineProperty(replacement, key, desc);
          continue;
        }
        const mocked = this.replacementFor(value, isFunction && key === "prototype" ? "prototype" : "member");
        if (inPlace) {
          if (mocked === value) continue;
          replacement[key] = mocked;
        } else if (isFunction && key === "prototype") {
          Object.defineProperty(replacement, "prototype", { value: mocked, writable: true, enumerable: false, configurable: false });
        } else {
          Object.defineProperty(replacement, key, { value: mocked, writable: true, enumerable: true, configurable: true });
        }
      }
      // A module namespace object's prototype only serves `__esModule` to CommonJS interop.
      if (kind === "module") break;
    }
  }
}

function moduleObject() {
  const object = Object.create(null);
  Object.defineProperty(object, Symbol.toStringTag, { value: "Module", writable: false, enumerable: false, configurable: false });
  return object;
}

function automock(value: unknown, role: AutomockRole, spy: boolean, leaveOriginalAlone: boolean): unknown {
  const automocker = new Automocker(spy, leaveOriginalAlone);
  const result = automocker.replacementFor(value, role);
  while (automocker.pending.length) {
    const [original, replacement, kind] = automocker.pending.pop()!;
    if (kind === "array") automocker.mockElements(original, replacement as unknown[]);
    else automocker.mockMembers(original, replacement, kind);
  }
  return result;
}

/** `vi.mockObject(value, { spy })`. */
export function mockObject(value: unknown, options?: unknown) {
  const spy = typeof options === "object" && options !== null && !!(options as any).spy;
  return automock(value, "member", spy, false);
}
