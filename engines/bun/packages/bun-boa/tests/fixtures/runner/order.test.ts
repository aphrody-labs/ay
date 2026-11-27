// What Bun's runner does, step by step: describe callbacks run after the file, in order and depth first; hooks
// run outermost first before a test and innermost first after it; done callbacks; a failing beforeAll skips its
// scope's tests; test.failing; test.each labels; retries; expect.assertions; inline snapshots; onTestFinished.
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, onTestFinished, test } from "bun:test";

const log: string[] = [];
log.push("file");

describe("outer", () => {
  log.push("outer callback");
  beforeAll(() => void log.push("outer beforeAll"));
  beforeEach(() => void log.push("outer beforeEach"));
  afterEach(() => void log.push("outer afterEach"));
  afterAll(() => void log.push("outer afterAll"));
  describe("inner", () => {
    log.push("inner callback");
    beforeEach(() => void log.push("inner beforeEach"));
    afterEach(() => void log.push("inner afterEach"));
    test("one", () => {
      log.push("one");
    });
  });
  test("two", () => {
    log.push("two");
  });
});
log.push("file end");

describe("async describe", async () => {
  await Promise.resolve();
  test("declared after an await", () => {
    expect(log.slice(0, 4)).toEqual(["file", "file end", "outer callback", "inner callback"]);
  });
});

test("order", () => {
  expect(log).toEqual([
    "file",
    "file end",
    "outer callback",
    "inner callback",
    "outer beforeAll",
    "outer beforeEach",
    "inner beforeEach",
    "one",
    "inner afterEach",
    "outer afterEach",
    "outer beforeEach",
    "two",
    "outer afterEach",
    "outer afterAll",
  ]);
});

test("done callback", done => {
  setTimeout(() => done(), 1);
});

test.failing("failing passes when it throws", () => {
  throw new Error("expected");
});

test.each([
  { a: 1, b: "x" },
  { a: 2, b: "y" },
])("each $a $b %#", ({ a }) => {
  expect(a).toBeGreaterThan(0);
});

let attempts = 0;
test("retried", { retry: 2 }, () => {
  attempts++;
  if (attempts < 2) throw new Error("first attempt fails");
});

test("assertions", () => {
  expect.assertions(2);
  expect(1).toBe(1);
  expect(2).toBe(2);
});

test("inline snapshot", () => {
  expect({ a: 1, b: [2] }).toMatchInlineSnapshot(`
    {
      "a": 1,
      "b": [
        2,
      ],
    }
  `);
});

let finished = false;
test("onTestFinished", () => {
  onTestFinished(() => {
    finished = true;
  });
});
test("after onTestFinished", () => {
  expect(finished).toBe(true);
});

describe("beforeAll throws", () => {
  beforeAll(() => {
    throw new Error("setup failed");
  });
  test("never runs", () => {
    throw new Error("must not run");
  });
});

test("timeout", async () => {
  await new Promise(resolve => setTimeout(resolve, 200));
}, 20);
