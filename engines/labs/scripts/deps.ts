#!/usr/bin/env bun
// Count the dependencies: Cargo (registry/git crates in Cargo.lock, names in several versions there and in the
// normal+build graph) and Bun (packages in bun.lock, names in several versions). Appends one record to
// build/deps.jsonl so every bump or dedupe is measured. Resolution only: runs anywhere, compiles nothing.
//   bun run deps [label]
import { $, JSONC } from "bun";

const label = Bun.argv[2] ?? "";
const meta = (await $`cargo metadata --format-version 1 --locked`.quiet().json()) as {
  packages: { name: string; version: string; source: string | null }[];
};
const ext = meta.packages.filter((p) => p.source);
const multi = (pkgs: { name: string; version: string }[]) =>
  Object.entries(Object.groupBy(pkgs, (p) => p.name))
    .filter(([, v]) => new Set(v!.map((p) => p.version)).size > 1)
    .map(([n]) => n)
    .sort();
// `cargo tree -d` lists each duplicated crate's versions at depth 0, one "name vX" per line; one version built
// twice (host and target, other features) shows up too, so count distinct versions.
const tree = (await $`cargo tree --workspace -d -e normal,build --depth 0 --prefix none`.quiet().text())
  .split("\n")
  .map((l) => /^([\w-]+) v(\S+)/.exec(l))
  .filter((m) => !!m)
  .map(([, name, version]) => ({ name: name!, version: version! }));

const lock = Bun.file("bun.lock");
const bun = (await lock.exists())
  ? Object.values(
      (JSONC.parse(await lock.text()) as { packages?: Record<string, [string, ...unknown[]]> }).packages ?? {},
    ).map(([id]) => ({ name: id.slice(0, id.lastIndexOf("@")), version: id.slice(id.lastIndexOf("@") + 1) }))
  : [];

const record = {
  at: new Date().toISOString(),
  rev: (await $`git rev-parse --short HEAD`.text()).trim(),
  label,
  cargo: { crates: ext.length, duplicated: multi(ext), tree_duplicated: multi(tree) },
  bun: { packages: bun.length, duplicated: multi(bun) },
};
await $`mkdir -p build`;
const out = Bun.file("build/deps.jsonl");
await Bun.write(out, ((await out.exists()) ? await out.text() : "") + JSON.stringify(record) + "\n");
console.log(
  JSON.stringify({
    label,
    crates: record.cargo.crates,
    duplicated: record.cargo.duplicated.length,
    tree_duplicated: record.cargo.tree_duplicated.length,
    packages: record.bun.packages,
    bun_duplicated: record.bun.duplicated.length,
  }),
);
