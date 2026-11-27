// mock.module(): a package that does not exist, a builtin module, and a factory that returns a promise.
import { expect, mock, test } from "bun:test";

test("a module that does not exist, for import and require", async () => {
  mock.module("a-package-that-does-not-exist", () => ({ default: 42, named: "n" }));
  const imported = await import("a-package-that-does-not-exist" as string);
  expect([imported.default, imported.named]).toEqual([42, "n"]);
  expect(require("a-package-that-does-not-exist").named).toBe("n");
});

test("a builtin module", async () => {
  mock.module("node:path", () => ({ join: () => "mocked" }));
  expect(require("node:path").join("a", "b")).toBe("mocked");
  expect((await import("node:path" as string)).join("a", "b")).toBe("mocked");
});

test("a factory that returns a promise", async () => {
  await mock.module("an-async-package", async () => ({ a: 123 }));
  expect((await import("an-async-package" as string)).a).toBe(123);
});

test("the arguments", () => {
  expect(() => mock.module(123 as any, () => ({}))).toThrow("mock(module, fn) requires a module name string");
  expect(() => (mock.module as any)("x")).toThrow("mock(module, fn) requires a function");
});
