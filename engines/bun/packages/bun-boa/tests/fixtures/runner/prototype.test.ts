import { afterEach, beforeEach, describe, expect, test } from "bun:test";

// What a test puts on Array.prototype (blob-array-fast-path.test.ts: a getter at index 1) must not reach the runner's
// own arrays: Bun's runner is native.
function poison() {
  Object.defineProperty(Array.prototype, 1, { get: () => "intercepted", configurable: true });
}
function cure() {
  delete (Array.prototype as any)[1];
}

describe("with a getter at Array.prototype[1]", () => {
  beforeEach(poison);
  afterEach(cure);
  test("matchers", async () => {
    expect({ a: [1, 2] }).toEqual({ a: [1, 2] });
    expect([1, 2, 3]).toContain(2);
    expect(() => {
      throw new Error("x");
    }).toThrow("x");
    await expect(Promise.resolve(1)).resolves.toBe(1);
    await expect(Promise.reject(new Error("y"))).rejects.toThrow("y");
    expect({ a: 1 }).toMatchInlineSnapshot(`
      {
        "a": 1,
      }
    `);
    await new Promise(resolve => setTimeout(resolve, 1));
  });
  test("a failing matcher's message", () => {
    expect(() => expect({ a: [1, 2] }).toEqual({ a: [1, 3] })).toThrow("toEqual");
  });
  test.each([
    [1, 2],
    [3, 4],
  ])("each %p %p", (a, b) => {
    expect(b - a).toBe(1);
  });
  test("a failure", () => {
    poison();
    throw new Error("reported");
  });
});

test("after", () => {
  expect([1, 2]).toHaveLength(2);
});
