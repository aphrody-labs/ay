// The runner of one test file: a port of src/runtime/test_runner/bun_test.rs (the scopes and entries a file declares),
// Collection.rs (describe callbacks, queued and run after the file), Order.rs (the groups and sequences the scopes
// become), Execution.rs (running them, hooks, timeouts, done callbacks, retries) and ScopeFunctions.rs (`test`,
// `describe` and their modifiers), with what test_command.rs prints for each test.
import { markup, show } from "./format.ts";
import { framesOf, sourcePreview } from "./source.ts";
import { append, native, prepend, runtime, type ExpectAssertions, type Sequence as ExpectSequence } from "./state.ts";

type Fn = (...args: any[]) => any;

// ── bun_test.rs: scopes and entries ─────────────────────────────────────────
type ScopeMode = "normal" | "skip" | "todo" | "failing" | "filtered_out";
const NO = 0,
  CONTAINS = 1,
  YES = 2;
type Only = typeof NO | typeof CONTAINS | typeof YES;
type Concurrent = "inherit" | "yes" | "no";

interface ScopeCfg {
  concurrent: Concurrent;
  mode: ScopeMode;
  only: boolean;
}
const defaultCfg = (): ScopeCfg => ({ concurrent: "inherit", mode: "normal", only: false });

interface Base {
  parent: Scope | undefined;
  name: string | undefined;
  concurrent: boolean;
  mode: ScopeMode;
  only: Only;
  hasCallback: boolean;
}
interface Hooks {
  beforeAll: Entry[];
  beforeEach: Entry[];
  afterEach: Entry[];
  afterAll: Entry[];
}
interface Inherited {
  timeout?: number;
  retry?: number;
  repeats?: number;
}
interface Scope extends Base {
  kind: "describe";
  entries: (Scope | Entry)[];
  hooks: Hooks;
  /** The describe callback threw: none of its tests run. */
  failed: boolean;
  inherited: Inherited;
}
type HookTag = keyof Hooks;
interface Entry extends Base {
  kind: "entry";
  callback: Fn | undefined;
  /** 0: no timeout. */
  timeout: number;
  /** The callback takes `done`. */
  done: boolean;
  retry: number;
  repeat: number;
  hook?: HookTag | "onTestFinished" | "onTestFailed";
  addedInExecution?: boolean;
}

function baseOf(cfg: ScopeCfg, name: string | undefined, parent: Scope | undefined, hasCallback: boolean): Base {
  return {
    parent,
    name,
    concurrent: cfg.concurrent === "yes" ? true : cfg.concurrent === "no" ? false : !!parent?.concurrent,
    mode: parent && parent.mode !== "normal" ? parent.mode : cfg.mode,
    only: cfg.only ? YES : NO,
    hasCallback,
  };
}

function newScope(base: Base, inherited: Inherited): Scope {
  return {
    ...base,
    kind: "describe",
    entries: [],
    hooks: { beforeAll: [], beforeEach: [], afterEach: [], afterAll: [] },
    failed: false,
    inherited,
  };
}

function newEntry(
  name: string | undefined,
  callback: Fn | undefined,
  options: { timeout: number; done: boolean; retry?: number; repeat?: number },
  parent: Scope | undefined,
  cfg: ScopeCfg,
): Entry {
  const base = baseOf(cfg, name, parent, callback !== undefined);
  const kept = base.mode === "skip" ? undefined : base.mode === "todo" ? (native.options.todo ? callback : undefined) : callback;
  return {
    ...base,
    kind: "entry",
    callback: kept,
    timeout: options.timeout,
    done: options.done,
    retry: options.retry ?? 0,
    repeat: options.repeat ?? 0,
  };
}

const root = newScope(baseOf(defaultCfg(), undefined, undefined, false), {});
let activeScope = root;
let phase: "collection" | "execution" | "done" = "collection";
let filterRegex: RegExp | undefined;

const scopesFrom = function* (scope: Scope | undefined) {
  for (let s = scope; s; s = s.parent) yield s;
};

function propagate(child: Base) {
  if (child.only !== NO)
    for (const scope of scopesFrom(child.parent)) {
      const was = scope.only;
      scope.only = CONTAINS;
      if (was === CONTAINS) break;
    }
  if (child.hasCallback)
    for (const scope of scopesFrom(child.parent)) {
      if (scope.hasCallback) break;
      scope.hasCallback = true;
    }
}

function appendDescribe(name: string | undefined, cfg: ScopeCfg, inherited: Inherited): Scope {
  const parent = activeScope;
  const child = newScope(baseOf(cfg, name, parent, false), inherited);
  propagate(child);
  append(parent.entries, child);
  return child;
}

function appendTest(name: string | undefined, callback: Fn | undefined, options: Parameters<typeof newEntry>[2], cfg: ScopeCfg) {
  const parent = activeScope;
  const entry = newEntry(name, callback, options, parent, cfg);
  entry.hasCallback = entry.callback !== undefined;
  propagate(entry);
  append(parent.entries, entry);
}

/** `with_async_context_if_needed`: a callback runs in the AsyncLocalStorage context it was registered in. */
let snapshot: (() => (fn: () => unknown) => unknown) | null | undefined;
function inAsyncContext(fn: Fn): Fn {
  if (snapshot === undefined)
    try {
      const { AsyncLocalStorage } = (import.meta as any).require("node:async_hooks");
      snapshot = () => AsyncLocalStorage.snapshot();
    } catch {
      snapshot = null;
    }
  if (!snapshot) return fn;
  const run = snapshot();
  return function (this: unknown, ...args: unknown[]) {
    return run(() => fn.apply(this, args));
  };
}

function appendHook(scope: Scope, tag: HookTag, callback: Fn, timeout: number) {
  const done = callback.length > 0;
  const entry = newEntry(undefined, inAsyncContext(callback), { timeout, done }, scope, defaultCfg());
  // A hook in a skipped scope keeps no callback, and an entry without one would be reported as a test.
  if (!entry.callback) return;
  entry.hook = tag;
  append(scope.hooks[tag], entry);
}

// ── Collection.rs: describe callbacks run after the file, depth first ──────
interface QueuedDescribe {
  callback: Fn;
  activeScope: Scope;
  newScope: Scope;
}
let describeQueue: QueuedDescribe[] = [];
let currentScopeQueue: QueuedDescribe[] = [];

function takeNextCallback(): QueuedDescribe | undefined {
  append(describeQueue, ...currentScopeQueue.reverse().filter(item => !item.newScope.failed));
  currentScopeQueue = [];
  for (let item = describeQueue.pop(); item; item = describeQueue.pop()) if (!item.activeScope.failed) return item;
  return undefined;
}

async function collect() {
  for (let next = takeNextCallback(); next; next = takeNextCallback()) {
    activeScope = next.newScope;
    try {
      const returned = next.callback();
      if (isThenable(returned)) await returned;
    } catch (error) {
      // Collection.handle_uncaught_exception: no test of a describe callback that threw runs.
      activeScope.failed = true;
      unhandled(error);
    }
    activeScope = next.activeScope;
  }
}

const isThenable = (v: unknown): v is PromiseLike<unknown> =>
  !!v && (typeof v === "object" || typeof v === "function") && typeof (v as any).then === "function";

// ── Order.rs: groups of sequences ──────────────────────────────────────────
interface SequenceEntry {
  entry: Entry;
  next: SequenceEntry | undefined;
  failureSkipPast: SequenceEntry | undefined;
}
type Result =
  | "pending"
  | "pass"
  | "skip"
  | "skipped_because_label"
  | "todo"
  | "fail"
  | "fail_because_timeout"
  | "fail_because_timeout_with_done_callback"
  | "fail_because_hook_timeout"
  | "fail_because_hook_timeout_with_done_callback"
  | "fail_because_failing_test_passed"
  | "fail_because_todo_passed"
  | "fail_because_expected_has_assertions"
  | "fail_because_expected_assertion_count";
type Basic = "pending" | "pass" | "fail" | "skip" | "todo";
const basicResult = (r: Result): Basic =>
  r === "pending" ? "pending" : r === "pass" ? "pass" : r === "skip" || r === "skipped_because_label" ? "skip" : r === "todo" ? "todo" : "fail";
const isPass = (r: Result, pendingIsPass = true) => {
  const b = basicResult(r);
  return b === "pending" ? pendingIsPass : b !== "fail";
};
const isFail = (r: Result) => !isPass(r);

class ExecutionSequence {
  active: SequenceEntry | undefined;
  remainingRepeat: number;
  remainingRetry: number;
  attempt = 0;
  result: Result = "pending";
  startedAt = 0;
  maybeSkip = false;
  expect: ExpectSequence;
  constructor(
    readonly first: SequenceEntry | undefined,
    readonly test: SequenceEntry | undefined,
    retry: number,
    repeat: number,
    concurrent: boolean,
  ) {
    this.active = first;
    this.remainingRetry = retry;
    this.remainingRepeat = repeat;
    const entry = test?.entry ?? first?.entry;
    const names = entry ? namesOf(entry) : [];
    this.expect = {
      expectCalls: 0,
      assertions: { kind: "none" },
      testName: names.join(" "),
      names,
      vitest: false,
      concurrent,
      softErrors: [],
      pendingMatchers: [],
    };
  }
  get entryMode(): ScopeMode {
    return this.test?.entry.mode ?? "normal";
  }
  setResultIfPending(result: Result) {
    if (this.result === "pending") this.result = result;
  }
  attemptFailed() {
    return this.entryMode === "failing" ? this.result === "pass" : isFail(this.result);
  }
  checkExpectAssertions() {
    const a: ExpectAssertions = this.expect.assertions;
    const count = this.expect.expectCalls;
    let failure: Result | undefined;
    if (a.kind === "none") return;
    if (a.kind === "atLeastOne" && count === 0) failure = "fail_because_expected_has_assertions";
    else if (a.kind === "exact" && count !== a.count) failure = "fail_because_expected_assertion_count";
    else {
      this.expect.assertions = { kind: "none" };
      return;
    }
    if (isPass(this.result) && this.result !== "skip") this.result = failure;
  }
  /** A hook a running test adds: afterEach/afterAll run after the test, onTestFinished after the sequence's last entry. */
  linkAfterActive(tag: "afterEach" | "afterAll" | "onTestFinished", fn: Fn, timeout: number) {
    let point = this.active;
    if (tag === "onTestFinished") while (point?.next) point = point.next;
    else for (let e = this.active; e; e = e.next) if (e === this.test) point = e;
    if (!point) throw new Error(`Cannot call ${tag}() here. Call it inside describe() instead.`);
    const entry = newEntry(undefined, fn, { timeout, done: fn.length > 0 }, undefined, defaultCfg());
    entry.hook = tag;
    entry.addedInExecution = true;
    point.next = { entry, next: point.next, failureSkipPast: undefined };
  }
  /** Execution.reset_sequence: the entries added while it ran are gone for the next attempt. */
  reset() {
    for (let e = this.first; e; e = e.next) while (e.next?.entry.addedInExecution) e.next = e.next.next;
    this.active = this.first;
    this.attempt++;
    this.result = "pending";
    this.startedAt = 0;
    this.maybeSkip = false;
    this.expect.expectCalls = 0;
    this.expect.assertions = { kind: "none" };
    this.expect.softErrors = [];
    this.expect.pendingMatchers = [];
  }
}

interface ConcurrentGroup {
  sequences: ExecutionSequence[];
  failureSkipTo: number;
  concurrent: boolean;
}

const namesOf = (entry: Base): string[] => {
  const names: string[] = [];
  for (const scope of scopesFrom(entry.parent)) if (scope.name) prepend(names, scope.name);
  if (entry.name !== undefined) append(names, entry.name);
  return names;
};

interface OpenDescribe {
  scope: Scope;
  children: (Scope | Entry)[];
  next: number;
  hooks: Hooks;
  useHooks: boolean;
  beforeAll: [number, number];
}

class Order {
  groups: ConcurrentGroup[] = [];
  previousGroupWasConcurrent = false;
  constructor(readonly alwaysUseHooks: boolean) {}

  allOrder(entries: Entry[]): [number, number] {
    const start = this.groups.length;
    for (const entry of entries) this.appendGroupOf({ entry, next: undefined, failureSkipPast: undefined });
    return [start, this.groups.length];
  }
  appendGroupOf(entry: SequenceEntry) {
    const sequence = new ExecutionSequence(entry, undefined, 0, 0, false);
    append(this.groups, { sequences: [sequence], failureSkipTo: this.groups.length + 1, concurrent: false });
    this.previousGroupWasConcurrent = false;
  }
  setFailureSkipTo([start, end]: [number, number]) {
    if (start === 0 && end === 0) return;
    for (const group of this.groups.slice(start, end)) group.failureSkipTo = this.groups.length;
  }

  generate(scope: Scope) {
    const open: OpenDescribe[] = [];
    const entered = this.enter(scope);
    if (entered) append(open, entered);
    while (open.length) {
      const frame = open[open.length - 1];
      const child = frame.children[frame.next];
      if (child === undefined) {
        this.exit(open.pop()!);
        continue;
      }
      frame.next++;
      if (frame.scope.only === CONTAINS && child.only === NO) continue;
      if (child.kind === "describe") {
        const next = this.enter(child);
        if (next) append(open, next);
      } else this.test(child, open);
    }
  }
  enter(scope: Scope): OpenDescribe | undefined {
    if (scope.failed) return undefined;
    const useHooks = this.alwaysUseHooks || scope.hasCallback;
    const beforeAll = useHooks ? this.allOrder(scope.hooks.beforeAll) : ([0, 0] as [number, number]);
    let children = scope.entries;
    if (native.options.randomize) children = shuffle(children);
    return { scope, children, next: 0, hooks: scope.hooks, useHooks, beforeAll };
  }
  exit(frame: OpenDescribe) {
    this.setFailureSkipTo(frame.beforeAll);
    const afterAll = frame.useHooks ? this.allOrder(frame.hooks.afterAll) : ([0, 0] as [number, number]);
    this.setFailureSkipTo(afterAll);
  }
  test(test: Entry, open: OpenDescribe[]) {
    const useEachHooks = test.hasCallback;
    const parents = open.map(frame => frame.hooks).reverse();
    const list: Entry[] = [];
    if (useEachHooks) for (const p of parents) prepend(list, ...p.beforeEach);
    const testIndex = list.length;
    append(list, test);
    if (useEachHooks) for (const p of parents) append(list, ...p.afterEach);
    const entries: SequenceEntry[] = list.map(entry => ({ entry, next: undefined, failureSkipPast: undefined }));
    entries.forEach((e, i) => {
      e.next = entries[i + 1];
      e.failureSkipPast = i <= testIndex ? entries[testIndex] : undefined;
    });
    const sequence = new ExecutionSequence(entries[0], entries[testIndex], test.retry, test.repeat, test.concurrent);
    const previous = this.groups[this.groups.length - 1];
    if (test.concurrent && previous && this.previousGroupWasConcurrent) append(previous.sequences, sequence);
    else append(this.groups, { sequences: [sequence], failureSkipTo: this.groups.length + 1, concurrent: test.concurrent });
    this.previousGroupWasConcurrent = test.concurrent;
  }
}

function shuffle<T>(list: T[]): T[] {
  const out = [...list];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

// ── Execution.rs ───────────────────────────────────────────────────────────
const timers = {
  setTimeout: globalThis.setTimeout,
  clearTimeout: globalThis.clearTimeout,
};
const now = () => Bun.nanoseconds();

export const stats = {
  pass: 0,
  fail: 0,
  skip: 0,
  todo: 0,
  filteredOut: 0,
  expects: 0,
  errors: 0,
  /** `--bail`'s count of failures was reached: no further test runs. */
  bailed: false,
};

/** The sequence whose callback is running, which an uncaught error is blamed on. */
let running: { sequence: ExecutionSequence; entry: SequenceEntry } | undefined;

type Outcome = { error: unknown } | undefined;

/** Calls an entry's callback with `done` when it takes one, and settles once it returned and `done` was called. */
function runCallback(entry: Entry): Promise<Outcome> | Outcome {
  let doneCalled = false;
  let resolveDone: ((o: Outcome) => void) | undefined;
  const donePromise = entry.done
    ? new Promise<Outcome>(resolve => {
        resolveDone = resolve;
      })
    : undefined;
  const done = (error?: unknown) => {
    if (doneCalled) return;
    doneCalled = true;
    resolveDone!(error === undefined || error === null ? undefined : { error });
  };
  let returned: unknown;
  try {
    returned = entry.done ? entry.callback!(done) : entry.callback!();
  } catch (error) {
    return { error };
  }
  const thenable = isThenable(returned);
  if (!thenable && !donePromise) return undefined;
  const fromPromise = thenable
    ? Promise.resolve(returned).then(
        () => undefined,
        (error: unknown) => ({ error }) as Outcome,
      )
    : undefined;
  if (!donePromise) return fromPromise;
  if (!fromPromise) return donePromise;
  // Both: an error of either ends it; otherwise it ends once both did.
  return new Promise<Outcome>(resolve => {
    let left = 2;
    const one = (o: Outcome) => {
      if (o) resolve(o);
      else if (--left === 0) resolve(undefined);
    };
    donePromise.then(one);
    fromPromise.then(one);
  });
}

function withTimeout(pending: Promise<Outcome>, timeout: number): Promise<Outcome | "timeout"> {
  if (!(timeout > 0) || !Number.isFinite(timeout)) return pending;
  return new Promise(resolve => {
    const timer = timers.setTimeout(() => resolve("timeout"), timeout);
    pending.then(o => {
      timers.clearTimeout(timer);
      resolve(o);
    });
  });
}

/** Execution.handle_uncaught_exception: whether the error is shown. */
function failSequence(sequence: ExecutionSequence, atTest: boolean): boolean {
  sequence.maybeSkip = true;
  if (!atTest) {
    sequence.setResultIfPending("fail");
    return true;
  }
  switch (sequence.entryMode) {
    case "failing":
      sequence.setResultIfPending("pass");
      return false;
    case "todo":
      sequence.setResultIfPending("todo");
      return true;
    default:
      sequence.setResultIfPending("fail");
      return true;
  }
}

async function runEntry(sequence: ExecutionSequence, current: SequenceEntry) {
  const entry = current.entry;
  if (sequence.first === current && sequence.startedAt === 0 && (!sequence.test || sequence.test.entry.callback))
    sequence.startedAt = now();
  const atTest = current === sequence.test;
  // Concurrent sequences interleave: what runs is not known, as in Bun, where they have no current sequence.
  const concurrent = sequence.expect.concurrent;
  if (!concurrent) {
    running = { sequence, entry: current };
    runtime.sequence = sequence.expect;
  }
  const started = runCallback(entry);
  let outcome = started instanceof Promise ? await withTimeout(started, entry.timeout) : started;
  // `waits_for_matchers`: a matcher that the callback did not await still ends inside its entry.
  while (outcome !== "timeout" && sequence.expect.pendingMatchers.length) {
    const pending = sequence.expect.pendingMatchers.splice(0);
    const settled = await withTimeout(
      Promise.allSettled(pending).then(() => undefined),
      entry.timeout,
    );
    if (settled === "timeout") outcome = "timeout";
  }
  // expect.soft() failures: each is an error of the test, which fails once its callback is done.
  for (const error of sequence.expect.softErrors.splice(0)) if (failSequence(sequence, atTest)) printError(error);
  if (outcome === "timeout") {
    sequence.result = atTest
      ? entry.done
        ? "fail_because_timeout_with_done_callback"
        : "fail_because_timeout"
      : entry.done
        ? "fail_because_hook_timeout_with_done_callback"
        : "fail_because_hook_timeout";
    sequence.maybeSkip = true;
  } else if (outcome) {
    if (failSequence(sequence, atTest)) printError(outcome.error);
  }
  if (!concurrent) {
    running = undefined;
    runtime.sequence = undefined;
  }
}

async function runSequence(sequence: ExecutionSequence) {
  for (;;) {
    while (sequence.active) {
      const current = sequence.active;
      const entry = current.entry;
      if (entry.callback) await runEntry(sequence, current);
      else if (entry.mode === "skip") sequence.setResultIfPending("skip");
      else if (entry.mode === "todo") sequence.setResultIfPending("todo");
      else if (entry.mode === "filtered_out") sequence.setResultIfPending("skipped_because_label");
      sequence.active = sequence.maybeSkip ? current.failureSkipPast?.next : current.next;
      sequence.maybeSkip = false;
    }
    const failed = isFail(sequence.result);
    if (failed && sequence.remainingRetry > 0) {
      sequence.remainingRetry--;
      sequence.reset();
      continue;
    }
    if (!failed && sequence.remainingRepeat > 0 && sequence.result !== "skip") {
      sequence.remainingRepeat--;
      sequence.remainingRetry = sequence.test?.entry.retry ?? 0;
      sequence.reset();
      continue;
    }
    break;
  }
  onSequenceCompleted(sequence);
}

function onSequenceCompleted(sequence: ExecutionSequence) {
  const elapsed = sequence.startedAt === 0 ? 0 : now() - sequence.startedAt;
  sequence.checkExpectAssertions();
  sequence.setResultIfPending(
    sequence.entryMode === "failing" ? "fail_because_failing_test_passed" : sequence.entryMode === "todo" ? "fail_because_todo_passed" : "pass",
  );
  if (sequence.first && (sequence.test || sequence.result !== "pass"))
    report(sequence, (sequence.test ?? sequence.first).entry, elapsed);
}

async function runGroup(group: ConcurrentGroup) {
  if (group.sequences.length === 1) return runSequence(group.sequences[0]);
  const limit = native.options.maxConcurrency > 0 ? native.options.maxConcurrency : group.sequences.length;
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(limit, group.sequences.length) }, async () => {
      while (next < group.sequences.length) await runSequence(group.sequences[next++]);
    }),
  );
}

async function execute() {
  const order = new Order(root.only === NO && filterRegex === undefined);
  order.generate(root);
  const groups = order.groups;
  phase = "execution";
  runtime.phase = "execution";
  let index = 0;
  while (index < groups.length && !stats.bailed) {
    const group = groups[index];
    await runGroup(group);
    index = group.sequences.every(sequence => isFail(sequence.result)) ? group.failureSkipTo : index + 1;
  }
  runtime.sequence = undefined;
  phase = "done";
  runtime.phase = "done";
}

// ── test_command.rs: what each test prints ─────────────────────────────────
const colors = () => native.colors;
const pretty = (text: string) => markup(text, colors());
const err = (text: string) => process.stderr.write(text);

function elapsedText(ns: number): string {
  const ms = ns / 1e6;
  return pretty(`<d>[${ms.toFixed(2)}ms]<r>`);
}

function statusText(result: Result): string {
  switch (basicResult(result)) {
    case "pending":
      return pretty("<r><d>(pending)<r>");
    case "pass":
      return pretty("<r><green>(pass)<r>");
    case "fail":
      return pretty("<r><red>(fail)<r>");
    case "skip":
      return pretty("<r><yellow>(skip)<d>");
    case "todo":
      return pretty("<r><magenta>(todo)<r>");
  }
}

function report(sequence: ExecutionSequence, entry: Entry, elapsedNs: number) {
  const result = sequence.result;
  const basic = basicResult(result);
  if (result !== "skipped_because_label" && !(basic !== "fail" && native.options.onlyFailures)) {
    if (result === "fail_because_expected_assertion_count") {
      const a = sequence.expect.assertions;
      const expected = a.kind === "exact" ? a.count : 0;
      const count = sequence.expect.expectCalls;
      err(
        pretty(
          `<r><red>AssertionError<r><d>:<r> expected <green>${expected} assertion${expected === 1 ? "" : "s"}<r>, but test ended with <red>${count} assertion${count === 1 ? "" : "s"}<r>\n`,
        ),
      );
    } else if (result === "fail_because_expected_has_assertions")
      err(pretty("<r><red>AssertionError<r><d>:<r> received <red>0 assertions<r>, but expected <green>at least one assertion<r> to be called\n"));
    if (!native.isAgent || basic === "fail") {
      const dim = basic === "todo" ? !native.options.todo : basic === "skip" || basic === "pending";
      let line = statusText(result);
      const scopes = [...scopesFrom(entry.parent)].reverse();
      for (const scope of scopes) {
        if (!scope.name) continue;
        line += colors() ? ` ${pretty(dim ? "<r><d>" : "<r>")}${scope.name}${pretty("<d>")} >` : ` ${scope.name} >`;
      }
      const label = entry.name ?? "(unnamed)";
      line += colors() ? `${pretty(dim ? "<r><d> " : "<r><b> ")}${label}${pretty("<r>")}` : ` ${label}`;
      const test = sequence.test?.entry;
      const attempts = test ? test.retry - sequence.remainingRetry + 1 : 1;
      const runs = test ? test.repeat - sequence.remainingRepeat + 1 : 1;
      if (attempts > 1) line += pretty(` <d>(attempt ${attempts})<r>`);
      if (runs > 1) line += pretty(` <d>(run ${runs})<r>`);
      if (elapsedNs > 10_000) line += ` ${elapsedText(elapsedNs)}`;
      line += "\n";
      switch (result) {
        case "fail_because_failing_test_passed":
          line += pretty(
            `  <d>^<r> <red>this test is marked as failing but it passed.<r> <d>Remove \`.${entry.mode}\` if tested behavior now works<r>\n`,
          );
          break;
        case "fail_because_todo_passed":
          line += pretty("  <d>^<r> <red>this test is marked as todo but passes.<r> <d>Remove `.todo` if tested behavior now works<r>\n");
          break;
        case "fail_because_timeout":
          line += pretty(`  <d>^<r> <red>this test timed out after ${entry.timeout}ms.<r>\n`);
          break;
        case "fail_because_hook_timeout":
          line += pretty("  <d>^<r> <red>a beforeEach/afterEach hook timed out for this test.<r>\n");
          break;
        case "fail_because_timeout_with_done_callback":
          line += pretty(
            `  <d>^<r> <red>this test timed out after ${entry.timeout}ms, before its done callback was called.<r> <d>If a done callback was not intended, remove the last parameter from the test callback function<r>\n`,
          );
          break;
        case "fail_because_hook_timeout_with_done_callback":
          line += pretty(
            "  <d>^<r> <red>a beforeEach/afterEach hook timed out before its done callback was called.<r> <d>If a done callback was not intended, remove the last parameter from the hook callback function<r>\n",
          );
          break;
      }
      err(line);
      // `failures_to_repeat_buf` and the others, which bun test prints again after the last file.
      if (!native.options.onlyFailures) {
        if (basic === "fail") repeats.fail += line;
        else if (basic === "skip") repeats.skip += line;
        else if (basic === "todo") repeats.todo += line;
      }
    }
  }
  switch (result) {
    case "pending":
      break;
    case "pass":
      stats.pass++;
      break;
    case "skip":
      stats.skip++;
      break;
    case "todo":
      stats.todo++;
      break;
    case "skipped_because_label":
      stats.filteredOut++;
      break;
    default:
      stats.fail++;
      // `bail_out`: the failure that `--bail` counts to ends the run, the files before this one included.
      if (native.options.bail > 0 && native.options.failuresBefore + stats.fail >= native.options.bail) stats.bailed = true;
  }
  stats.expects += sequence.expect.expectCalls;
}

export const repeats = { fail: "", skip: "", todo: "" };


// ── errors ─────────────────────────────────────────────────────────────────
/** The lines around where an error was thrown, the message, and the stack, as Bun prints an error. */
export function errorText(error: unknown): string {
  if (!(error instanceof Error) && !(error && typeof error === "object" && "message" in error && "stack" in error)) {
    return `${pretty("<r><red>error<r><d>:<r> ")}${show(error, { colors: colors() })}\n`;
  }
  const e = error as Error;
  const name = (() => {
    try {
      return String(e.name ?? "Error");
    } catch {
      return "Error";
    }
  })();
  const message = (() => {
    try {
      return String(e.message ?? "");
    } catch {
      return "";
    }
  })();
  const stack = typeof e.stack === "string" ? e.stack : "";
  // The frames of the user's code, at their positions in the files as they are written: not bun:test's, nor those
  // of builtins and natives.
  const frames = framesOf(stack).filter(frame => !/\((?:bun:|builtin:\/\/|native\)|unknown at )/.test(frame.text));
  const label = name === "Error" ? "error" : name;
  // The lines where it was thrown, of the first frame in a file the loader read.
  let out = frames.length ? sourcePreview(frames[0], pretty) : "";
  out += pretty(`<r><red>${label}<r><d>:<r> <b>`) + message + pretty("<r>") + "\n";
  for (const frame of frames) out += pretty(`<r>      <d>${frame.text}<r>\n`);
  return out;
}

export function printError(error: unknown) {
  err(errorText(error));
}

/** An error no test claims: between tests, in a describe callback, or while the file loads. */
export function unhandled(error: unknown) {
  stats.errors++;
  err(pretty("<r>\n<b><d>#<r> <red><b>Unhandled error<r><d> between tests<r>\n<d>-------------------------------<r>\n"));
  printError(error);
  err(pretty("<r><d>-------------------------------<r>\n\n"));
}

/** An error thrown by a timer callback: it fails the test that runs, else it is unhandled (bun_test.rs). */
export function uncaught(error: unknown) {
  if (phase === "execution" && running) {
    if (failSequence(running.sequence, running.entry === running.sequence.test)) printError(error);
    return;
  }
  if (phase === "collection" && activeScope !== root) activeScope.failed = true;
  unhandled(error);
}

// ── ScopeFunctions.rs: test, describe and their modifiers ───────────────────
const defaultTimeout = () => (timeoutOverride ?? native.options.timeout) >>> 0;
let timeoutOverride: number | undefined;
export function setDefaultTimeout(ms: unknown) {
  if (typeof ms !== "number") throw new TypeError("setDefaultTimeout() expects a number (milliseconds)");
  timeoutOverride = ms;
}

type Kind = "test" | "describe";
interface ScopeFunctionsState {
  kind: Kind;
  cfg: ScopeCfg;
  each?: unknown[];
}

function signatureOf(s: ScopeFunctionsState): string {
  let name = s.kind;
  if (s.cfg.concurrent === "yes") name += ".concurrent";
  else if (s.cfg.concurrent === "no") name += ".serial";
  if (s.cfg.mode !== "normal" && s.cfg.mode !== "filtered_out") name += `.${s.cfg.mode}`;
  if (s.cfg.only) name += ".only";
  if (s.each) name += ".each";
  return name;
}

function nameOfCallable(f: Fn, signature: string): string {
  if (typeof f.name === "string" && f.name.length) return f.name;
  throw new Error(`${signature}() expects first argument to be a named class, named function, number, or string`);
}

interface Parsed {
  description: string | undefined;
  callback: Fn | undefined;
  timeout: number;
  retry: number;
  repeats: number;
  inherited: Inherited;
}

const isOptions = (v: unknown) => typeof v === "object" && v !== null;

function parseArguments(
  first: unknown,
  second: unknown,
  third: unknown,
  signature: string,
  kind: "testOrDescribe" | "hook",
  requireCallback: boolean,
  inherited: Inherited,
): Parsed {
  const classify = (v: unknown) =>
    v === undefined || v === null ? "none" : typeof v === "function" ? "callback" : typeof v === "number" ? "timeout" : isOptions(v) ? "options" : "other";
  const firstKind =
    first === undefined || first === null
      ? "none"
      : typeof first === "function"
        ? "function"
        : typeof first === "string"
          ? "text"
          : typeof first === "number"
            ? "number"
            : "other";
  if (firstKind === "other")
    throw invalidArgs(
      kind === "testOrDescribe" ? "The first argument must be a named class, named function, number, or string" : `${signature} expects a function as the first argument`,
    );
  for (const [v, ordinal] of [
    [second, "second"],
    [third, "third"],
  ] as const)
    if (classify(v) === "other")
      throw invalidArgs(
        kind === "testOrDescribe"
          ? `The ${ordinal} argument must be a function, number, or object`
          : `${signature} expects a number or object as the ${ordinal} argument`,
      );
  let description: unknown = first;
  let callback: unknown;
  let options: unknown;
  const k2 = classify(second),
    k3 = classify(third);
  if (firstKind === "function" && (k2 === "none" || k2 === "timeout" || k2 === "options") && k3 === "none") {
    description = undefined;
    callback = first;
    options = second;
  } else if (k2 === "callback" || k3 === "none") {
    callback = second;
    options = third;
  } else {
    options = second;
    callback = third;
  }
  if (typeof callback !== "function") {
    if (callback !== undefined && callback !== null) {
      throw new Error(`${signature} expects a function as the ${kind === "testOrDescribe" ? "second" : "first"} argument`);
    }
    if (requireCallback) throw new Error(`${signature} expects a function as the ${kind === "testOrDescribe" ? "second" : "first"} argument`);
    callback = undefined;
  }
  let timeout: number | undefined;
  let retry: number | undefined;
  let repeats: number | undefined;
  if (typeof options === "number") timeout = options;
  else if (typeof options === "function") throw new Error(`${signature}() expects options to be a number or object, not a function`);
  else if (isOptions(options)) {
    const o = options as any;
    if (o.timeout !== undefined) timeout = Number(o.timeout);
    if (o.retry !== undefined) {
      if (typeof o.retry !== "number") throw new Error(`${signature}() expects retry to be a number`);
      retry = o.retry;
    }
    if (o.repeats !== undefined) {
      if (retry) throw new Error(`${signature}(): Cannot set both retry and repeats`);
      repeats = Number(o.repeats);
    }
  }
  let name: string | undefined;
  if (typeof description === "string") name = description;
  else if (typeof description === "number") name = String(description);
  else if (typeof description === "function") name = nameOfCallable(description as Fn, signature);
  const merged: Inherited = {
    timeout: timeout !== undefined ? timeout >>> 0 : inherited.timeout,
    retry: retry ?? inherited.retry,
    repeats: repeats ?? inherited.repeats,
  };
  const finalRetry = merged.retry ?? native.options.retry;
  const finalRepeats = merged.repeats ?? 0;
  if (finalRetry && finalRepeats) throw new Error(`${signature}(): Cannot set both retry and repeats`);
  return {
    description: name,
    callback: callback as Fn | undefined,
    timeout: merged.timeout ?? defaultTimeout(),
    retry: finalRetry >>> 0,
    repeats: finalRepeats >>> 0,
    inherited: merged,
  };
}

function invalidArgs(message: string) {
  const error = new TypeError(message);
  (error as any).code = "ERR_INVALID_ARG_TYPE";
  return error;
}

/** jest.rs `format_label`: `%s`, `%d`, `%i`, `%f`, `%j`, `%o`, `%p`, `%#`, `%$`, `%%` and `$path` of an object row. */
export function formatLabel(label: string, args: unknown[], index: number): string {
  const objectRow = typeof args[0] === "object" && args[0] !== null ? (args[0] as any) : undefined;
  let next = 0;
  let out = "";
  for (let i = 0; i < label.length; i++) {
    const c = label[i];
    const after = label[i + 1];
    if (c === "%" && after === "%") {
      out += "%";
      i++;
    } else if ((c === "%" && after === "#") || (c === "$" && after === "#" && objectRow)) {
      out += String(index);
      i++;
    } else if (c === "%" && after === "$") {
      out += String(index + 1);
      i++;
    } else if (c === "%" && after !== undefined && "sdifjoOpc".includes(after)) {
      if (next >= args.length) {
        out += "%";
        continue;
      }
      out += placeholder(after, args[next++]);
      i++;
    } else if (c === "$" && after !== undefined && objectRow) {
      const m = /^(?:[A-Za-z_$][\w$]*(?:\.[A-Za-z_$\d][\w$]*)*|\d+(?![\w$]))/.exec(label.slice(i + 1));
      const path = m ? m[0] : "";
      i += path.length;
      let value: any = objectRow;
      let found = path.length > 0;
      for (const key of path.split(".")) {
        if (!found || key === "" || value === undefined || value === null || !(key in Object(value))) {
          found = false;
          break;
        }
        value = value[key];
      }
      if (!found) out += "$" + path;
      else out += typeof value === "string" ? value : inspected(value);
    } else out += c;
  }
  return out;
}

function inspected(value: unknown): string {
  if (value instanceof Error) return `[${String(value)}]`;
  return show(value, { singleLine: true });
}

const BUILTIN_TO_STRING = [Object, Array, Date, RegExp, Boolean, Number, String, Symbol, BigInt].map(c => c.prototype.toString);
function hasBuiltinToString(value: object): boolean {
  const toString = (value as any).toString;
  return BUILTIN_TO_STRING.includes(toString);
}

function placeholder(specifier: string, value: unknown): string {
  if (
    specifier === "s" &&
    (typeof value === "string" ||
      typeof value === "function" ||
      value instanceof Error ||
      (typeof value === "object" && value !== null && !hasBuiltinToString(value)))
  )
    return String(value);
  if (specifier === "j") {
    try {
      const json = JSON.stringify(value);
      return json === undefined ? "undefined" : json;
    } catch (error) {
      if (String((error as any)?.message).includes("cyclic")) return "[Circular]";
      throw error;
    }
  }
  if (specifier === "c") return "";
  if ((specifier === "d" || specifier === "i") && typeof value === "bigint") return inspected(value);
  let number: number;
  if ((specifier === "d" || specifier === "i" || specifier === "f") && typeof value === "symbol") number = NaN;
  else if (specifier === "d") number = Number(value);
  else if (specifier === "i") number = parseInt(String(value));
  else if (specifier === "f") number = parseFloat(String(value));
  else return inspected(value);
  return inspected(number);
}

function enqueue(s: ScopeFunctionsState, callback: Fn | undefined, description: string | undefined, parsed: Parsed, done: boolean) {
  if (phase === "execution") throw new Error(`Cannot call ${signatureOf(s)}() inside a test. Call it inside describe() instead.`);
  if (phase === "done") throw new Error(`Cannot call ${signatureOf(s)}() after the test run has completed`);
  if (callback) callback = inAsyncContext(callback);
  const cfg: ScopeCfg = { ...s.cfg };
  if (callback === undefined && (cfg.mode === "normal" || cfg.mode === "failing")) cfg.mode = "todo";
  if (native.options.concurrent && cfg.concurrent === "inherit") cfg.concurrent = "yes";
  if (s.kind === "describe") {
    const scope = appendDescribe(description, cfg, parsed.inherited);
    if (callback) append(currentScopeQueue, { callback, activeScope, newScope: scope });
    return;
  }
  let matches = true;
  if (filterRegex) {
    const names: string[] = [description ?? ""];
    for (const scope of scopesFrom(activeScope)) if (scope.name !== undefined) prepend(names, scope.name);
    matches = filterRegex.test(names.join(" "));
  }
  if (!matches) cfg.mode = "filtered_out";
  appendTest(description, matches ? callback : undefined, { timeout: parsed.timeout, done, retry: parsed.retry, repeat: parsed.repeats }, cfg);
}

function callScopeFunctions(s: ScopeFunctionsState, first: unknown, second: unknown, third: unknown) {
  const signature = signatureOf(s);
  const requireCallback = !(s.cfg.mode === "skip" || s.cfg.mode === "todo");
  const parsed = parseArguments(first, second, third, signature, "testOrDescribe", requireCallback, activeScope.inherited);
  const callbackLength = parsed.callback ? parsed.callback.length : 0;
  if (s.each) {
    if (!Array.isArray(s.each)) throw new Error(`Expected array, got ${show(s.each)}`);
    s.each.forEach((item, index) => {
      const args = Array.isArray(item) ? [...item] : [item];
      const label = parsed.description === undefined ? undefined : formatLabel(parsed.description, args, index);
      const callback = parsed.callback;
      const bound = callback ? (...rest: unknown[]) => callback(...args, ...rest) : undefined;
      if (bound) Object.defineProperty(bound, "length", { value: Math.max(0, callbackLength - args.length) });
      enqueue(s, bound, label, parsed, callbackLength > args.length);
    });
    return;
  }
  enqueue(s, parsed.callback, parsed.description, parsed, callbackLength >= 1);
}

const modePrecedence = (mode: ScopeMode) => (mode === "normal" ? 0 : mode === "failing" ? 1 : mode === "todo" ? 2 : 3);

function extendCfg(s: ScopeFunctionsState, added: Partial<ScopeCfg>, name: string): ScopeFunctionsState {
  if (added.mode === "failing" && s.kind === "describe") throw new Error(`Cannot ${name} on ${signatureOf(s)}`);
  if (added.only && native.isCI)
    throw new Error(
      ".only is disabled in CI environments to prevent accidentally skipping tests. To override, set the environment variable CI=false.",
    );
  const base = { ...s.cfg };
  const add: ScopeCfg = { concurrent: added.concurrent ?? "inherit", mode: added.mode ?? "normal", only: added.only ?? false };
  base.only &&= !add.only;
  if (base.concurrent === add.concurrent) base.concurrent = "inherit";
  if (modePrecedence(base.mode) > modePrecedence(add.mode)) add.mode = "normal";
  else base.mode = "normal";
  if (add.concurrent !== "inherit" && base.concurrent !== "inherit") throw new Error(`Cannot ${name} on ${signatureOf(s)}`);
  if (add.mode !== "normal" && base.mode !== "normal") throw new Error(`Cannot ${name} on ${signatureOf(s)}`);
  if (add.only && base.only) throw new Error(`Cannot ${name} on ${signatureOf(s)}`);
  return {
    kind: s.kind,
    cfg: {
      concurrent: add.concurrent !== "inherit" ? add.concurrent : base.concurrent,
      mode: add.mode !== "normal" ? add.mode : base.mode,
      only: base.only || add.only,
    },
    each: s.each,
  };
}

function scopeFunctions(s: ScopeFunctionsState): any {
  const name = s.kind === "test" ? "test" : "describe";
  const f = {
    [name](first?: unknown, second?: unknown, third?: unknown) {
      return callScopeFunctions(s, first, second, third);
    },
  }[name];
  const extend = (added: Partial<ScopeCfg>, label: string) => () => scopeFunctions(extendCfg(s, added, label));
  const getters: Record<string, () => unknown> = {
    skip: extend({ mode: "skip" }, "skip"),
    todo: extend({ mode: "todo" }, "todo"),
    only: extend({ only: true }, "only"),
    concurrent: extend({ concurrent: "yes" }, "concurrent"),
    serial: extend({ concurrent: "no" }, "serial"),
    sequential: extend({ concurrent: "no" }, "sequential"),
  };
  if (s.kind === "test") getters.failing = extend({ mode: "failing" }, "failing");
  for (const [key, get] of Object.entries(getters)) Object.defineProperty(f, key, { get, configurable: true });
  const conditional = (added: Partial<ScopeCfg>, label: string, invert: boolean) => (condition: unknown) =>
    !!condition !== invert ? scopeFunctions(extendCfg(s, added, label)) : scopeFunctions({ ...s });
  Object.assign(f, {
    if: conditional({ mode: "skip" }, "if", true),
    skipIf: conditional({ mode: "skip" }, "skipIf", false),
    todoIf: conditional({ mode: "todo" }, "todoIf", false),
    concurrentIf: conditional({ concurrent: "yes" }, "concurrentIf", false),
    serialIf: conditional({ concurrent: "no" }, "serialIf", false),
    each(table: unknown, ...values: unknown[]) {
      const rows = templateRows(table, values) ?? table;
      if (rows === undefined || rows === null || !Array.isArray(rows)) throw new Error(`Expected array, got ${show(rows)}`);
      if (s.each) throw new Error(`Cannot each on ${signatureOf(s)}`);
      return scopeFunctions({ ...s, each: rows });
    },
  });
  if (s.kind === "test") (f as any).failingIf = conditional({ mode: "failing" }, "failingIf", false);
  return f;
}

/** `test.each\`a | b ${1} | ${2}\``: rows of objects keyed by the headings. */
function templateRows(strings: unknown, values: unknown[]): unknown[] | undefined {
  if (!Array.isArray(strings) || !("raw" in strings)) return undefined;
  const headings = strings[0]
    .split("|")
    .map((h: string) => h.trim())
    .filter(Boolean);
  if (headings.length === 0) return undefined;
  if (values.length % headings.length !== 0)
    throw new Error(`Not enough arguments supplied for given headings:\n${headings.join(" | ")}\n\nReceived:\n${show(values)}`);
  const rows: Record<string, unknown>[] = [];
  for (let i = 0; i < values.length; i += headings.length)
    append(rows, Object.fromEntries(headings.map((h: string, j: number) => [h, values[i + j]])));
  return rows;
}

export const test = scopeFunctions({ kind: "test", cfg: defaultCfg() });
export const describe = scopeFunctions({ kind: "describe", cfg: defaultCfg() });

// ── hooks ──────────────────────────────────────────────────────────────────
/** bun_test.rs `generic_hook_impl` for Jest's hooks. */
function hook(tag: HookTag | "onTestFinished") {
  return {
    [tag](first?: unknown, second?: unknown) {
      const parsed = parseArguments(first, second, undefined, `${tag}()`, "hook", true, {});
      const callback = parsed.callback!;
      if (phase === "collection") {
        if (tag === "onTestFinished")
          throw new Error(`Cannot call ${tag}() outside of a test. It can only be called inside a test.`);
        appendHook(activeScope, tag, callback, parsed.timeout);
        return;
      }
      if (phase === "done") throw new Error(`Cannot call ${tag}() after the test run has completed`);
      const sequence = running?.sequence;
      if (!sequence || sequence.expect.concurrent)
        throw new Error(
          tag === "onTestFinished"
            ? `Cannot call ${tag}() here. It cannot be called inside a concurrent test. Use test.serial or remove test.concurrent.`
            : `Cannot call ${tag}() here. It cannot be called inside a concurrent test. Call it inside describe() instead.`,
        );
      if (tag === "beforeAll" || tag === "beforeEach")
        throw new Error(`Cannot call ${tag}() inside a test. Call it inside describe() instead.`);
      sequence.linkAfterActive(tag, callback, parsed.timeout);
    },
  }[tag];
}
export const beforeAll = hook("beforeAll");
export const beforeEach = hook("beforeEach");
export const afterEach = hook("afterEach");
export const afterAll = hook("afterAll");
export const onTestFinished = hook("onTestFinished");

// ── the file ───────────────────────────────────────────────────────────────
/** After test.rs evaluated the file: its describe callbacks, then its tests. */
export async function run(path: string) {
  runtime.path = path;
  if (native.options.namePattern !== undefined) filterRegex = new RegExp(native.options.namePattern);
  await collect();
  await execute();
  stats.expects += runtime.strayExpects;
  return stats;
}
