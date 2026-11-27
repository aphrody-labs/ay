// Blob, File, Bun.file and Bun.write, which bun_runtime_js implements on bun_js.
import { file, write } from "bun";
import { expect, test } from "bun:test";

const dir = `${import.meta.dir}/.out-${process.pid}`;

test("Blob joins its parts", async () => {
  const blob = new Blob(["start-", new Uint8Array([65, 66, 67]), new Blob(["inner"]), "-end"], { type: "Text/Plain" });
  expect(await blob.text()).toBe("start-ABCinner-end");
  expect(blob.size).toBe(18);
  expect(blob.type).toBe("text/plain");
  expect(await blob.slice(6, 9).text()).toBe("ABC");
  expect(() => new Blob("ok" as any)).toThrow("new Blob() expects an Array");
  expect(Bun.inspect(new Blob(["x"], { type: "text/plain" }))).toBe('Blob (1 bytes) {\n  type: "text/plain"\n}');
  expect(Bun.inspect({ b: new Blob([]) })).toBe("{ b: Blob (0 KB) }");
  const form = await new Blob(["a=1&b=2"], { type: "application/x-www-form-urlencoded" }).formData();
  expect([form instanceof FormData, form.get("a"), form.getAll("b")]).toEqual([true, "1", ["2"]]);
  const clone = structuredClone(new File(["x"], "f.txt", { type: "text/plain" }));
  expect([clone instanceof File, clone.name, clone.type, await clone.text()]).toEqual([true, "f.txt", "text/plain", "x"]);
});

test("File is a Blob with a name", () => {
  const f = new File(["x"], "a.txt", { lastModified: 7 });
  expect([f.name, f.lastModified, f instanceof File, f instanceof Blob]).toEqual(["a.txt", 7, true, true]);
  expect(new Blob([]) instanceof File).toBe(false);
  const url = URL.createObjectURL(f);
  expect(url.startsWith("blob:")).toBe(true);
  URL.revokeObjectURL(url);
});

test("Bun.write and Bun.file", async () => {
  expect(await write(`${dir}/a/b.txt`, "hello")).toBe(5);
  const f = file(`${dir}/a/b.txt`);
  expect([f.size, f.type, await f.exists(), await f.text()]).toEqual([5, "text/plain;charset=utf-8", true, "hello"]);
  expect(await write(file(`${dir}/copy.txt`), f)).toBe(5);
  expect(await file(`${dir}/copy.txt`).text()).toBe("hello");
  expect(await f.slice(1).text()).toBe("ello");
  await f.delete();
  expect(await file(`${dir}/a/b.txt`).exists()).toBe(false);
  expect(await write(new URL(`file://${dir}/url.txt`), "u")).toBe(1);
  expect(await file(new URL(`file://${dir}/url.txt`)).text()).toBe("u");
  await file(`${dir}/copy.txt`).delete();
});

test("errors are Bun's", async () => {
  let error: any;
  try {
    await file(`${dir}/missing.txt`).text();
  } catch (e) {
    error = e;
  }
  expect([error.code, error.syscall, error.path]).toEqual(["ENOENT", "open", `${dir}/missing.txt`]);
  expect(error.message).toBe(`ENOENT: no such file or directory, open '${dir}/missing.txt'`);
  await expect(write(`${dir}/no/such.txt`, "x", { createPath: false })).rejects.toThrow("no such file or directory");
});
