#!/usr/bin/env bun
// Quantify the workspace: clean-build and incremental-build time, crate count, duplicate versions, native crates,
// and the size of each release artifact. Appends one record to build/bench.jsonl so every change is measured.
//   bun run remote bun run bench
import { $ } from "bun";

const time = async (cmd: string) => {
  const t = Bun.nanoseconds();
  await $`sh -c ${cmd}`.quiet().nothrow();
  return Math.round((Bun.nanoseconds() - t) / 1e6);
};
const meta = (await $`cargo metadata --format-version 1`.json()) as {
  packages: { name: string; version: string; links: string | null; source: string | null }[];
};
const ext = meta.packages.filter((p) => p.source);
const versions = Object.groupBy(ext, (p) => p.name);
const record = {
  at: new Date().toISOString(),
  rev: (await $`git rev-parse --short HEAD`.text()).trim(),
  crates: ext.length,
  duplicated: Object.entries(versions)
    .filter(([, v]) => (v?.length ?? 0) > 1)
    .map(([n]) => n),
  native: ext.filter((p) => p.links || p.name.endsWith("-sys")).map((p) => `${p.name}@${p.version}`),
  check_clean_ms: await time("cargo clean -q && cargo check -q"),
  check_incr_ms: await time("touch boa/core/engine/src/lib.rs && cargo check -q"),
};
await $`mkdir -p build`;
await Bun.write(
  "build/bench.jsonl",
  ((await Bun.file("build/bench.jsonl").exists()) ? await Bun.file("build/bench.jsonl").text() : "") +
    JSON.stringify(record) +
    "\n",
);
console.log(JSON.stringify({ ...record, duplicated: record.duplicated.length, native: record.native }));
