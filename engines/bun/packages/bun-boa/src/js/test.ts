// `bun:test` on Boa: the module jest.rs makes (`test`, `describe`, the hooks, `expect`, `mock`, `jest`, `vi`), over
// ./test. test.rs evaluates a test file, which declares its tests here, then calls the runner at globalThis[RUN].
import { expect, expectTypeOf, setIsMock, setSnapshotHooks } from "./test/expect.ts";
import { setMockDetector, setPromiseState } from "./test/format.ts";
import {
  clearAllMocks,
  isMockFunction,
  jestFn,
  jestSpyOn,
  mock,
  mocked,
  mockObject,
  moduleMock,
  resetAllMocks,
  restoreAllMocks,
  setPromiseInspector,
  spyOn,
  viFn,
  viSpyOn,
} from "./test/mock.ts";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  onTestFinished,
  repeats,
  run,
  setDefaultTimeout,
  test,
  uncaught,
  unhandled,
} from "./test/runner.ts";
import { counts as snapshots, hooks as snapshotHooks, save as saveSnapshots } from "./test/snapshot.ts";
import { native } from "./test/state.ts";

const G = globalThis as any;

setIsMock(isMockFunction);
setMockDetector(isMockFunction);
setPromiseState(native.promiseState);
setPromiseInspector(native.promiseState, native.promiseResult);
setSnapshotHooks(snapshotHooks);

// ── setSystemTime: Date as jest's modern fake timers set it ─────────────────
const RealDate = Date;
let systemTime: number | undefined;
const FakeDate = function Date(this: unknown, ...args: unknown[]) {
  if (!new.target) return new RealDate(systemTime ?? RealDate.now()).toString();
  return args.length === 0 && systemTime !== undefined
    ? Reflect.construct(RealDate, [systemTime], new.target)
    : Reflect.construct(RealDate, args, new.target);
} as unknown as DateConstructor;
Object.setPrototypeOf(FakeDate, RealDate);
Object.defineProperty(FakeDate, "prototype", { value: RealDate.prototype });
Object.defineProperty(FakeDate, "now", {
  value: function now() {
    return systemTime ?? RealDate.now();
  },
  writable: true,
  configurable: true,
});
function setSystemTime(time?: unknown) {
  const ms =
    time === undefined || time === null
      ? NaN
      : time instanceof RealDate
        ? time.getTime()
        : typeof time === "number"
          ? time
          : new RealDate(time as string).getTime();
  if (Number.isNaN(ms)) {
    systemTime = undefined;
    G.Date = RealDate;
    return jest;
  }
  systemTime = ms;
  G.Date = FakeDate;
  return jest;
}

// ── mock, jest, vi ─────────────────────────────────────────────────────────
Object.assign(mock, { module: moduleMock, restore: restoreAllMocks, clearAllMocks });
const jest: Record<string, unknown> = {
  fn: jestFn,
  mock: moduleMock,
  spyOn: jestSpyOn,
  restoreAllMocks,
  clearAllMocks,
  resetAllMocks,
  setSystemTime,
  now: () => (systemTime ?? RealDate.now()),
  setTimeout: setDefaultTimeout,
  isMockFunction,
  mocked,
};
const vi: Record<string, unknown> = {
  fn: viFn,
  mock: moduleMock,
  spyOn: viSpyOn,
  restoreAllMocks,
  resetAllMocks,
  clearAllMocks,
  setSystemTime,
  isMockFunction,
  mocked,
  mockObject,
};

const it = test;
const xtest = test.skip;
const xit = xtest;
const xdescribe = describe.skip;

// ── uncaught errors of timers: they fail the running test (bun_test.rs `on_uncaught_exception`) ──
for (const name of ["setTimeout", "setInterval", "setImmediate"] as const) {
  const original = G[name];
  if (typeof original !== "function") continue;
  G[name] = {
    [name](callback: unknown, ...rest: unknown[]) {
      if (typeof callback !== "function") return original(callback, ...rest);
      return original(function (this: unknown, ...args: unknown[]) {
        try {
          return callback.apply(this, args);
        } catch (error) {
          uncaught(error);
        }
      }, ...rest);
    },
  }[name];
}

// What test.rs calls once the file is evaluated, and with an error the file threw while it loaded.
native.run = async (path: string) => {
  const stats = await run(path);
  saveSnapshots();
  return { ...stats, snapshots: { ...snapshots }, repeats: { ...repeats } };
};
native.unhandled = unhandled;

Object.assign(G, { describe, test, it, expect, beforeAll, afterAll, beforeEach, afterEach, mock, spyOn, jest, vi });

/** `Bun.jest(path)` (jest.rs): the object of bun:test's exports, for a file that is not a test file. */
const testModuleObject = {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  expectTypeOf,
  it,
  jest,
  mock,
  onTestFinished,
  setDefaultTimeout,
  setSystemTime,
  spyOn,
  test,
  vi,
  xdescribe,
  xit,
  xtest,
};
if (G.Bun)
  G.Bun.jest = function jest(path?: unknown) {
    if (typeof path !== "string") throw new Error("Bun.jest() expects a string filename");
    if (!path.startsWith("/") && !/^[A-Za-z]:[\\/]/.test(path))
      throw new Error(`Bun.jest() expects an absolute file path, got '${path}'`);
    return testModuleObject;
  };

export {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  expectTypeOf,
  it,
  jest,
  mock,
  onTestFinished,
  setDefaultTimeout,
  setSystemTime,
  spyOn,
  test,
  vi,
  xdescribe,
  xit,
  xtest,
};
