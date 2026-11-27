// Bun.semver, which bun_semver_js binds through bun_js. The first case is test/regression/issue/08040.test.ts.
import { semver } from "bun";
import { expect, test } from "bun:test";

test("semver with multiple tags work properly", () => {
  expect(semver.satisfies("3.4.5", ">=3.3.0-beta.1 <3.4.0-beta.3")).toBeFalse();
});

test("satisfies", () => {
  expect(semver.satisfies("1.2.3", "^1.0.0")).toBeTrue();
  expect(semver.satisfies("2.0.0", "^1.0.0")).toBeFalse();
  expect(semver.satisfies("1.2.3", "1.2.3")).toBeTrue();
  expect(semver.satisfies("1.2.3-beta.1", ">=1.2.3-alpha <1.2.3")).toBeTrue();
  expect(semver.satisfies("not a version", "*")).toBeFalse();
  expect(Bun.semver.satisfies(123 as any, "*")).toBeFalse();
});

test("order", () => {
  expect(semver.order("1.0.0", "2.0.0")).toBe(-1);
  expect(semver.order("2.0.0", "1.0.0")).toBe(1);
  expect(semver.order("1.0.0+build", "1.0.0")).toBe(0);
  expect(["1.10.0", "1.2.0", "1.2.0-rc.1"].sort(semver.order)).toEqual(["1.2.0-rc.1", "1.2.0", "1.10.0"]);
  // From test/cli/install/semver.test.ts.
  for (const [left, right] of [
    ["1.x", "1.0.x"],
    ["2.x", "2.1"],
    ["3.2.*", "3.2.0"],
  ]) {
    expect(semver.order(left, right)).toBe(1);
    expect(semver.order(right, left)).toBe(-1);
    expect(semver.order(left, left)).toBe(0);
  }
  expect(() => (semver.order as any)()).toThrow("Invalid SemVer: undefined\n");
  for (const version of ["^1.0.0", "2x", "1.2*"]) {
    expect(() => semver.order(version, "1.0.0")).toThrow(`Invalid SemVer: ${version}\n`);
    expect(() => semver.order("1.0.0", version)).toThrow(`Invalid SemVer: ${version}\n`);
  }
  const log: string[] = [];
  const stringLike = (name: string, value: string) => ({ toString: () => (log.push(name), value) });
  expect(semver.order(stringLike("left", "1.2.3") as any, stringLike("right", "1.2.3") as any)).toBe(0);
  expect(log).toEqual(["left", "right"]);
  const throwing = {
    toString() {
      throw new RangeError("left");
    },
  };
  expect(() => semver.order(throwing as any, "1.0.0")).toThrow(RangeError);
});

test("functions", () => {
  expect(semver.satisfies.name).toBe("satisfies");
  expect(semver.order.length).toBe(2);
});
