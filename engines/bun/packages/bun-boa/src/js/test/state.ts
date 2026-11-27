// What the runner and expect() share: the running test, the file, and the natives test.rs gives bun:test.
const NATIVE = "__bun_test_native__";

// The runner's own arrays grow by defining their elements, so that what a test puts on Array.prototype (an index
// accessor, another push) does not reach them: Bun's runner is native.
const { defineProperty } = Object;

/** `object[key] = value` as an own data property, whatever `object`'s prototypes have at `key`. */
export function put(object: object, key: PropertyKey, value: unknown): void {
  defineProperty(object, key, { value, writable: true, enumerable: true, configurable: true });
}

/** `list.push(...values)`. */
export function append<T>(list: T[], ...values: T[]): number {
  for (let i = 0; i < values.length; i++) put(list, list.length, values[i]);
  return list.length;
}

/** `list.unshift(...values)`. */
export function prepend<T>(list: T[], ...values: T[]): number {
  const n = values.length;
  for (let i = list.length - 1; i >= 0; i--) put(list, i + n, list[i]);
  for (let i = 0; i < n; i++) put(list, i, values[i]);
  return list.length;
}

export interface Native {
  /** printDiff.rs (src/runtime/test_runner/diff), compiled into bun_runtime_js. */
  diff(not: boolean, received: string, expected: string, colors: boolean): string;
  /** 0 pending, 1 fulfilled, 2 rejected; `undefined` if `value` is not a promise. */
  promiseState(value: unknown): 0 | 1 | 2 | undefined;
  promiseResult(promise: Promise<unknown>): unknown;
  readFile(path: string): string | undefined;
  writeFile(path: string, text: string): boolean;
  /** `mock.module()`: what imports or requires `specifier`, resolved from `fromDir`, gets `exports` from now on. */
  mockModule(specifier: string, fromDir: string, exports: unknown): void;
  /** Through the loader's source map: a 1-based position of the printed code, in the file as it is written. */
  originalPosition(path: string, line: number, column: number): [line: number, column: number] | undefined;
  /** The file as it is written, if the loader transpiled it. */
  originalSource(path: string): string | undefined;
  /** Its 1-based lines `first` to `last`. */
  originalLines(path: string, first: number, last: number): string[] | undefined;
  /** Bun's `enable_ansi_colors_stderr`. */
  colors: boolean;
  isAgent: boolean;
  isCI: boolean;
  /** The flags of `bun test`. */
  options: {
    timeout: number;
    updateSnapshots: boolean;
    onlyFailures: boolean;
    todo: boolean;
    namePattern?: string;
    randomize: boolean;
    retry: number;
    concurrent: boolean;
    maxConcurrency: number;
    /** `--bail`: the number of failures that ends the run, 0 for none. */
    bail: number;
    /** The failures of the files that ran before this one. */
    failuresBefore: number;
  };
  /** Set by js/test.ts for test.rs: runs the file's tests once it is evaluated. */
  run?: (path: string) => Promise<object>;
  /** Set by js/test.ts for test.rs: prints an error the file threw while it loaded. */
  unhandled?: (error: unknown) => void;
}

const g = globalThis as any;
export const native: Native = g[NATIVE];
delete g[NATIVE];

export type ExpectAssertions = { kind: "none" } | { kind: "exact"; count: number } | { kind: "atLeastOne" };

/** The test that runs: what Bun's ExecutionSequence counts for it. */
export interface Sequence {
  expectCalls: number;
  assertions: ExpectAssertions;
  /** The test's full name, the way snapshots and getState() name it. */
  testName: string;
  /** describe names, outermost first, and the test's. */
  names: string[];
  vitest: boolean;
  concurrent: boolean;
  /** Errors of expect.soft(), which fail the test when its callback is done. */
  softErrors: unknown[];
  /** The promises of matchers that wait on a pending promise (expect_deferred.rs): the entry ends once they settle. */
  pendingMatchers: Promise<unknown>[];
}

export const runtime = {
  sequence: undefined as Sequence | undefined,
  /** The absolute path of the test file. */
  path: "",
  phase: "collection" as "collection" | "execution" | "done",
  /** expect() calls outside of a sequence (concurrent tests, between tests). */
  strayExpects: 0,
};
