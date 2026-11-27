#!/usr/bin/env bun
// Raise version requirements to the latest releases: `cargo upgrade` (cargo-edit) over the workspace, then the
// requirements it raised in the generated root `[workspace.dependencies]` are written back into each tree's
// workspaces/<tree>.toml (the sources unify regenerates the root from), and `bun run unify`.
// Member manifests with their own requirements are edited by cargo upgrade in place (patches on the subtree).
// One toolchain (rust-toolchain.toml) builds every member, so members' rust-version does not hold versions back.
//   bun run bump [cargo upgrade args…]     e.g. bun run bump --incompatible allow -p rand -p itertools
import { $, Glob, TOML } from "bun";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
$.cwd(ROOT);
type Spec = string | { version?: string };
const wsDeps = async () =>
  (
    TOML.parse(await Bun.file(join(ROOT, "Cargo.toml")).text()) as {
      workspace?: { dependencies?: Record<string, Spec> };
    }
  ).workspace?.dependencies ?? {};
const req = (spec: Spec | undefined) => (typeof spec === "string" ? spec : spec?.version);

const before = await wsDeps();
await $`cargo upgrade --ignore-rust-version ${Bun.argv.slice(2)}`;
const after = await wsDeps();
const raised = Object.fromEntries(
  Object.entries(after)
    .filter(([d, s]) => req(s) && req(s) !== req(before[d]))
    .map(([d, s]) => [d, req(s)!]),
);

// Line edits in `[workspace.dependencies]`: `dep = "…"`, `dep = { version = "…", … }`, `[workspace.dependencies.dep]`.
for (const file of new Glob("workspaces/*.toml").scanSync({ cwd: ROOT })) {
  const path = join(ROOT, file);
  const text = await Bun.file(path).text();
  let section = "";
  const out = text.split("\n").map((line) => {
    const h = /^\s*\[\s*([^\]]+?)\s*\]/.exec(line)?.[1];
    if (h) section = h.replace(/"/g, "");
    if (section === "workspace.dependencies") {
      const m = /^(\s*"?([\w-]+)"?\s*=\s*)(.*)$/.exec(line);
      const to = m && raised[m[2]!];
      if (!to) return line;
      if (/^"/.test(m[3]!)) return `${m[1]}"${to}"${m[3]!.replace(/^"[^"]*"/, "")}`;
      return line.replace(/(version\s*=\s*)"[^"]*"/, `$1"${to}"`);
    }
    const dotted = /^workspace\.dependencies\.([\w-]+)$/.exec(section)?.[1];
    if (dotted && raised[dotted])
      return line.replace(new RegExp(`^(\\s*version\\s*=\\s*)"[^"]*"`), `$1"${raised[dotted]}"`);
    return line;
  });
  if (out.join("\n") !== text) await Bun.write(path, out.join("\n"));
}
await $`bun run unify`;
console.log(
  `raised ${Object.keys(raised).length} workspace requirements: ${Object.entries(raised)
    .map(([d, v]) => `${d} ${v}`)
    .join(", ")}`,
);
