import { describe, expect, mock, test, beforeEach, afterAll } from "bun:test";

let n = 0;
beforeEach(() => {
  n++;
});
afterAll(() => {
  if (n !== 7) throw new Error(`beforeEach ran ${n} times`);
});

describe("math", () => {
  test("adds", () => {
    expect(1 + 1).toBe(2);
    expect({ a: [1, { b: 2 }], c: undefined }).toEqual({ a: [1, { b: 2 }] });
    expect([1, 2, 3]).toContain(2);
    expect("hello").toMatch(/ell/);
  });
  test("throws", () => {
    expect(() => JSON.parse("{")).toThrow(SyntaxError);
    expect(() => 1).not.toThrow();
  });
  describe("nested", () => {
    test("async", async () => {
      await Bun.sleep(1);
      await expect(Promise.resolve(3)).resolves.toBe(3);
      await expect(Promise.reject(new Error("no"))).rejects.toThrow("no");
    });
  });
});

test("mocks", () => {
  const f = mock((x: number) => x * 2);
  expect(f(2)).toBe(4);
  expect(f).toHaveBeenCalledTimes(1);
  expect(f).toHaveBeenCalledWith(2);
  expect({ id: 1, name: "x" }).toMatchObject({ name: expect.any(String) });
});

test.each([
  [1, 2, 3],
  [2, 3, 5],
])("%d + %d = %d", (a, b, c) => {
  expect(a + b).toBe(c);
});

test("fails on purpose", () => {
  expect(2 + 2).toBe(5);
});

test.skip("skipped", () => {
  throw new Error("must not run");
});
test.todo("later");
