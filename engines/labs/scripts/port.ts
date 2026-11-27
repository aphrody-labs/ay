#!/usr/bin/env bun
// Ports a tree's vendor patch series onto the in-tree crates with real three-way merges.
//
// A series in <tree>/scripts/patches/<crate>-<version>/ is a git-format-patch run against the crates.io tarball, so
// its `index` lines name blobs that only exist in that tarball's history. Each series is first replayed on its
// tarball (build/port/<crate>-<version>, a scratch repo, as the tree's own materialiser does), those objects are
// fetched into labs, and then `git am -3 --directory=<in-tree path>` has a base for every hunk.
//
//   bun run port [tree] [crate…]     → materialise + fetch (default tree: kitesurf, every series with an in-tree crate)
//   bun run port [tree] --list       → the series, their in-tree paths and patch counts
import { $, Glob, TOML } from "bun";
import { mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const [tree = "kitesurf", ...rest] = Bun.argv.slice(2);
const list = rest.includes("--list");
const only = rest.filter((a) => !a.startsWith("--"));
const patches = join(ROOT, tree, "scripts/patches");
const env = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_SYSTEM: "/dev/null",
  GIT_AUTHOR_NAME: "kitesurf-vendor",
  GIT_AUTHOR_EMAIL: "vendor@kitesurf.invalid",
  GIT_AUTHOR_DATE: "2000-01-01T00:00:00 +0000",
  GIT_COMMITTER_NAME: "kitesurf-vendor",
  GIT_COMMITTER_EMAIL: "vendor@kitesurf.invalid",
  GIT_COMMITTER_DATE: "2000-01-01T00:00:00 +0000",
};

// crate name → in-tree dir, from every parked or active tree's manifests.
const config = TOML.parse(await Bun.file(join(ROOT, "labs.toml")).text()) as { trees: Record<string, unknown> };
const inTree = new Map<string, string>();
for (const name of Object.keys(config.trees)) {
  if (name === tree) continue;
  for (const p of new Glob("**/Cargo.toml").scanSync({ cwd: join(ROOT, name) })) {
    if (p.split("/").some((s) => s === "target" || s === "vendor")) continue;
    const pkg = (TOML.parse(await Bun.file(join(ROOT, name, p)).text()) as { package?: { name?: string } }).package;
    if (pkg?.name && !inTree.has(pkg.name)) inTree.set(pkg.name, join(name, dirname(p)));
  }
}

const series = [...new Glob("*/").scanSync({ cwd: patches, onlyFiles: false })]
  .map((d) => d.replace(/\/$/, ""))
  .map((d) => /^(.+)-(\d+\.\d+\.\d+.*)$/.exec(d))
  .filter((m): m is RegExpExecArray => !!m && inTree.has(m[1]!) && (!only.length || only.includes(m[1]!)))
  .map(([dir, name, version]) => ({ dir: dir!, name: name!, version: version!, path: inTree.get(name!)! }))
  .sort((a, b) => a.dir.localeCompare(b.dir));

for (const s of series) {
  const files = [...new Glob("*.patch").scanSync({ cwd: join(patches, s.dir) })].sort();
  if (list) {
    console.log(`${s.dir}\t${s.path}\t${files.length}`);
    continue;
  }
  const out = join(ROOT, "build/port", s.dir);
  await rm(out, { recursive: true, force: true });
  await mkdir(out, { recursive: true });
  const url = `https://static.crates.io/crates/${s.name}/${s.name}-${s.version}.crate`;
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: ${res.status}`);
  const crate = `${out}.crate`;
  await Bun.write(crate, res);
  await $`tar xzf ${crate} --strip-components=1 -C ${out}`.quiet();
  const manifest = Bun.file(join(out, "Cargo.toml"));
  const text = await manifest.text();
  if (!/^\[workspace\]/m.test(text)) await Bun.write(manifest, text + "\n[workspace]\n");
  await $`git init -q -b main && git add -A && git commit -q -m ${`pristine ${s.dir}`} && git tag vendor-base`
    .cwd(out)
    .env(env);
  await $`git am -q --whitespace=nowarn ${files.map((f) => join(patches, s.dir, f))}`.cwd(out).env(env);
  await $`git fetch -q --no-tags ${out} +main:refs/port/${s.dir}`.cwd(ROOT);
  console.log(`${s.dir}\t${s.path}\t${files.length} patches → refs/port/${s.dir}`);
}
