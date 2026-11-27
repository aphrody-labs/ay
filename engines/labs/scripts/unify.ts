#!/usr/bin/env bun
// One Cargo workspace from the subtrees in labs.toml.
//
// Cargo allows a single workspace root, so each tree's own `[workspace*]` tables are moved out of its Cargo.toml
// (kept verbatim in workspaces/<tree>.toml, refreshed whenever an upstream pull brings them back), and every
// manifest that inherited from them gets the values inline: `version.workspace = true` → `version = "…"`,
// `[lints] workspace = true` → the tree's lints. Dependencies stay `workspace = true`; the root
// `[workspace.dependencies]` is the union of every tree's, deduplicated: an in-tree crate is always the path copy,
// otherwise the highest version wins, features are merged. `[patch.crates-io]` points every in-tree crate at its
// path, so crates.io users of boa/blitz/parley/… in any tree build against the one copy here.
//
// `[lock]` in labs.toml holds crates at an exact version in Cargo.lock (`cargo update --precise`), for a release
// that keeps one version of a shared dependency.
//
//   bun run unify            → Cargo.toml, workspaces/*.toml, edited manifests, Cargo.lock pins, build/unify.json
import { $, Glob, TOML, semver } from "bun";
import { dirname, join, relative } from "node:path";

// Parsed TOML, untyped: manifests and workspace tables are read and rewritten key by key.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Table = Record<string, any>;
type Tree = { remote: string; branch: string; keep?: string[]; drop?: string[]; parked?: string; lints?: Table };
const ROOT = join(import.meta.dir, "..");
const config = TOML.parse(await Bun.file(join(ROOT, "labs.toml")).text()) as {
  purge: string[];
  cut?: string[];
  lock?: Record<string, string>;
  lints?: Table;
  trees: Record<string, Tree>;
};
const report: { conflicts: string[]; dropped: string[]; edited: string[] } = { conflicts: [], dropped: [], edited: [] };

const read = async (p: string) => ((await Bun.file(p).exists()) ? await Bun.file(p).text() : null);
const manifests = (cwd: string) =>
  [...new Glob("**/Cargo.toml").scanSync({ cwd, dot: false })].filter(
    (p) => !p.split("/").some((s) => s === "target" || s === "vendor" || s === "node_modules"),
  );

// The workspace tables of a tree root, split from the rest of its manifest (by top-level table header).
function split(text: string) {
  const own: string[] = [];
  const ws: string[] = [];
  let into = own;
  for (const line of text.split("\n")) {
    const h = /^\s*\[\[?\s*([^\]]+?)\s*\]\]?/.exec(line)?.[1];
    if (h) into = /^(workspace|profile|patch|replace)\b/.test(h) ? ws : own;
    into.push(line);
  }
  return { own: own.join("\n").trim(), ws: ws.join("\n").trim() };
}

const trees: Record<string, { ws: Table; members: string[] }> = {};
const inTree = new Map<string, string>(); // crate name → root-relative dir

for (const [name, tree] of Object.entries(config.trees)) {
  const dir = join(ROOT, name);
  if (tree.parked) continue;
  const saved = join(ROOT, "workspaces", `${name}.toml`);
  // A virtual tree root is deleted once its tables are saved: later runs read workspaces/<tree>.toml.
  const top = (await read(join(dir, "Cargo.toml"))) ?? "";
  if (!top && !(await Bun.file(saved).exists())) continue;
  const { own, ws } = split(top);
  if (ws) {
    await Bun.write(saved, ws + "\n");
    if (own) await Bun.write(join(dir, "Cargo.toml"), own + "\n");
    else await Bun.file(join(dir, "Cargo.toml")).delete();
  }
  const wsTable = TOML.parse((await read(saved)) ?? "") as Table;
  const w = (wsTable.workspace ?? {}) as Table;
  // Members: the tree's own list (or its root package), minus purged/dropped; `keep` lists the only members instead.
  const listed = (w.members as string[] | undefined) ?? (own ? ["."] : []);
  const excluded = new Set(((w.exclude as string[] | undefined) ?? []).map((e) => e.replace(/\/$/, "")));
  const found = new Set<string>();
  for (const m of [...listed, ...(tree.keep ?? [])]) {
    if (m === ".") found.add(".");
    else for (const p of new Glob(`${m}/Cargo.toml`).scanSync({ cwd: dir })) found.add(dirname(p));
  }
  const members = [...found].filter((m) => {
    if (tree.keep) {
      if (!tree.keep.includes(m)) report.dropped.push(`${name}/${m}`);
      return tree.keep.includes(m);
    }
    const gone =
      excluded.has(m) ||
      tree.drop?.some((d) => m === d || m.startsWith(d + "/")) ||
      m.split("/").some((s) => config.purge.some((p) => s === p || s.endsWith(`_${p}`) || s.endsWith(`-${p}`)));
    if (gone) report.dropped.push(`${name}/${m}`);
    return !gone;
  });
  trees[name] = { ws: w, members };
  for (const p of manifests(dir)) {
    const pkg = (TOML.parse((await read(join(dir, p)))!) as Table).package as Table | undefined;
    if (pkg?.name && !inTree.has(pkg.name)) inTree.set(pkg.name, join(name, dirname(p)));
  }
}
const members = new Set(Object.entries(trees).flatMap(([t, { members }]) => members.map((m) => join(t, m))));

// Inline what a manifest inherits from its tree's workspace (only line edits: comments and layout stay).
const PATH_KEYS = new Set(["readme", "license-file"]);
for (const [name, { ws }] of Object.entries(trees)) {
  const dir = join(ROOT, name);
  const pkgWs = (ws.package ?? {}) as Table;
  for (const p of manifests(dir)) {
    const file = join(dir, p);
    const text = (await read(file))!;
    let section = "";
    let changed = false;
    const out: string[] = [];
    for (const line of text.split("\n")) {
      const h = /^\s*\[\[?\s*([^\]]+?)\s*\]\]?/.exec(line)?.[1];
      if (h) section = h;
      const inherit =
        /^\s*([A-Za-z][\w-]*)\s*\.\s*workspace\s*=\s*true\s*(#.*)?$/.exec(line) ??
        /^\s*([A-Za-z][\w-]*)\s*=\s*\{\s*workspace\s*=\s*true\s*\}\s*(#.*)?$/.exec(line);
      if (section === "package" && inherit) {
        const key = inherit[1]!;
        let value = pkgWs[key];
        if (value === undefined) {
          report.conflicts.push(`${name}/${p}: package.${key} not in the tree workspace`);
          out.push(line);
          continue;
        }
        if (PATH_KEYS.has(key)) value = relative(dirname(file), join(dir, value));
        out.push(TOML.stringify({ [key]: value }).trim());
        changed = true;
        continue;
      }
      if (section === "package" && /^\s*workspace\s*=/.test(line)) {
        changed = true;
        continue;
      }
      if (section === "lints" && /^\s*workspace\s*=\s*true\s*(#.*)?$/.test(line)) {
        out.pop(); // the [lints] header: the tree's tables replace it
        out.push(TOML.stringify({ lints: ws.lints ?? {} }).trim());
        changed = true;
        continue;
      }
      out.push(line);
    }
    if (changed) {
      await Bun.write(file, out.join("\n"));
      report.edited.push(`${name}/${p}`);
    }
  }
}

// The union of the trees' [workspace.dependencies].
// The lowest version a requirement accepts, padded to three components: semver.order ranks a bare "0.13" above
// "0.13.1", which would let the vaguer spec win.
const order = (v: string | undefined) => {
  const [core = "0", pre] = (v ?? "0")
    .replace(/^[\^=~<>\s]+/, "")
    .split(/[\s,]/)[0]!
    .split(/-(.*)/);
  const parts = core.split(".");
  while (parts.length < 3) parts.push("0");
  return parts.join(".") + (pre ? `-${pre}` : "");
};
const deps: Record<string, Table> = {};
const pinned: Record<string, Record<string, Table>> = {}; // tree → dep → the tree's own spec
const owner: Record<string, string> = {};
for (const [name, { ws }] of Object.entries(trees)) {
  for (const [dep, raw] of Object.entries((ws.dependencies ?? {}) as Table)) {
    const spec: Table = typeof raw === "string" ? { version: raw } : { ...raw };
    const crate = (spec.package as string | undefined) ?? dep;
    if (inTree.has(crate)) {
      for (const k of ["version", "git", "branch", "tag", "rev", "registry"]) delete spec[k];
      spec.path = inTree.get(crate);
    } else if (spec.path) spec.path = join(name, spec.path);
    const prev = deps[dep];
    if (!prev) {
      deps[dep] = spec;
      owner[dep] = name;
      continue;
    }
    if ((prev.package ?? dep) !== crate) {
      report.conflicts.push(`${dep}: ${owner[dep]} names ${prev.package ?? dep}, ${name} names ${crate}`);
      continue;
    }
    let base = prev;
    if (!prev.path && !spec.path && spec.version && prev.version !== spec.version) {
      const [a, b] = [order(prev.version), order(spec.version)];
      const newer = semver.order(a, b) < 0;
      report.conflicts.push(
        `${dep}: ${owner[dep]} ${prev.version}, ${name} ${spec.version} → ${newer ? spec.version : prev.version}`,
      );
      const key = (v: string) => {
        const [maj = "0", min = "0"] = v.split(".");
        return maj === "0" ? `0.${min}` : maj;
      };
      if (key(a) !== key(b)) {
        // Not caret-compatible (0.9 vs 0.10, 1 vs 2): the newer spec wins whole (its features name that version's
        // features) and the older tree keeps its own spec inline.
        const [loser, lspec] = newer ? [owner[dep]!, prev] : [name, spec];
        (pinned[loser] ??= {})[dep] = lspec;
        if (newer) {
          deps[dep] = spec;
          owner[dep] = name;
        }
        continue;
      }
      // Compatible: the newer requirement, with both trees' features.
      if (newer) {
        base = spec;
        owner[dep] = name;
      }
    }
    const other = base === prev ? spec : prev;
    const merged: Table = { ...base };
    const features = [...new Set([...(base.features ?? []), ...(other.features ?? [])])];
    if (features.length) merged.features = features;
    if (base["default-features"] === false && other["default-features"] !== false) delete merged["default-features"];
    deps[dep] = merged;
  }
}
// Path deps first, then by name, as inline tables.
// Members of a tree whose version lost to an incompatible one: `dep.workspace = true` → the tree's spec inline.
const inlineTable = (t: Table) =>
  TOML.stringify({ x: t })
    .replace(/^\[x\]\n/, "")
    .trim()
    .split("\n")
    .join(", ");
for (const [name, deps] of Object.entries(pinned)) {
  const dir = join(ROOT, name);
  for (const p of manifests(dir)) {
    const file = join(dir, p);
    const text = (await read(file))!;
    let section = "";
    let changed = false;
    const out = text.split("\n").map((line) => {
      const h = /^\s*\[\[?\s*([^\]]+?)\s*\]\]?/.exec(line)?.[1];
      if (h) section = h;
      if (!/(^|\.)(dev-|build-)?dependencies$/.test(section)) return line;
      const m =
        /^(\s*)([A-Za-z0-9_-]+)\s*(?:\.\s*workspace\s*=\s*true|=\s*(\{.*workspace\s*=\s*true.*\}))\s*(#.*)?$/.exec(
          line,
        );
      const spec = m && deps[m[2]!];
      if (!m || !spec) return line;
      const own = m[3] ? ((TOML.parse(`x = ${m[3]}`) as Table).x as Table) : {};
      delete own.workspace;
      const features = [...new Set([...(spec.features ?? []), ...(own.features ?? [])])];
      const merged: Table = { ...spec, ...own, ...(features.length ? { features } : {}) };
      changed = true;
      return `${m[1]}${m[2]} = { ${inlineTable(merged)} }`;
    });
    if (changed) {
      await Bun.write(file, out.join("\n"));
      report.edited.push(`${name}/${p} (pinned)`);
    }
  }
}
// A member's dev- and optional dependencies on an in-tree crate that is not a member (dropped, purged, outside
// `keep`) are cut, with the features that name them, so the dropped crates stay out of the lockfile too; so are
// the dev-dependencies listed in labs.toml `cut`.
const outside = new Set([...inTree].filter(([, d]) => !members.has(d)).map(([n]) => n));
for (const member of members) {
  const file = join(ROOT, member, "Cargo.toml");
  const text = await read(file);
  if (!text) continue;
  const parsed = TOML.parse(text) as Table;
  const tables: [string, Table][] = [];
  for (const k of ["dependencies", "dev-dependencies", "build-dependencies"]) {
    if (parsed[k]) tables.push([k, parsed[k]]);
    for (const [t, v] of Object.entries((parsed.target ?? {}) as Table))
      if (v[k]) tables.push([`target.${t}.${k}`, v[k]]);
  }
  const cut = new Set<string>(); // dependency keys
  const needed = new Set<string>();
  for (const [section, table] of tables) {
    for (const [key, raw] of Object.entries(table)) {
      const spec: Table = typeof raw === "string" ? { version: raw } : raw;
      const crate = (spec.package as string | undefined) ?? (spec.workspace ? (deps[key]?.package ?? key) : key);
      if (section.endsWith("dev-dependencies") && config.cut?.includes(crate)) {
        cut.add(key);
        continue;
      }
      if (!outside.has(crate)) continue;
      if (section.endsWith("dev-dependencies") || spec.optional) cut.add(key);
      else {
        needed.add(key);
        report.conflicts.push(`${member}: needs ${crate}, which is not a member`);
      }
    }
  }
  for (const k of needed) cut.delete(k);
  if (!cut.size) continue;
  let section = "";
  let skip = false;
  const names = [...cut].map((k) => k.replace(/[-]/g, "\\-")).join("|");
  const featureRef = new RegExp(`\\s*"(?:dep:(?:${names})|(?:${names})\\??/[^"]*)"\\s*,?`, "g");
  const out: string[] = [];
  for (const line of text.split("\n")) {
    const h = /^\s*\[\[?\s*([^\]]+?)\s*\]\]?/.exec(line)?.[1];
    if (h) {
      section = h;
      const dotted = /(?:^|\.)(?:dev-|build-)?dependencies\.([\w-]+)$/.exec(h)?.[1];
      skip = !!dotted && cut.has(dotted);
    }
    if (skip) continue;
    if (/(^|\.)(dev-|build-)?dependencies$/.test(section)) {
      const key = /^\s*([\w-]+)\s*(?:\.|=)/.exec(line)?.[1];
      if (key && cut.has(key)) continue;
    }
    if (section === "features" || section === "package.metadata.docs.rs") {
      const kept = line
        .replace(featureRef, "")
        .replace(/\[\s*,/, "[")
        .replace(/,\s*\]/, "]");
      if (kept.trim() || !line.trim()) out.push(kept);
      continue;
    }
    out.push(line);
  }
  const next = out.join("\n");
  if (next !== text) {
    await Bun.write(file, next);
    report.edited.push(`${member}/Cargo.toml (cut ${[...cut].join(", ")})`);
  }
}

// Targets follow `purge`: a purged examples/ or benches/ directory builds no target (`autoexamples = false`,
// `[[example]]` tables dropped), since what those targets need is cut above (criterion) or dropped (window shells).
// An integration test that `use`s a crate the member does not depend on (a dropped one: anyrender_skia) is left out
// with `autotests = false` and the other tests listed. So `cargo check --workspace --all-targets` builds everything.
const TARGETS = [
  ["example", "autoexamples", "examples"],
  ["bench", "autobenches", "benches"],
].filter(([, , d]) => config.purge.includes(d!)) as [string, string, string][];
const BUILTIN = new Set(["std", "core", "alloc", "proc_macro", "test", "crate", "self", "super"]);
const isDir = async (p: string) => (await $`test -d ${p}`.nothrow().quiet()).exitCode === 0;
for (const member of members) {
  const dir = join(ROOT, member);
  const file = join(dir, "Cargo.toml");
  const text = await read(file);
  if (!text) continue;
  const parsed = TOML.parse(text) as Table;
  const pkg = (parsed.package ?? {}) as Table;
  const off = new Set<string>();
  const dropKinds = new Set<string>();
  for (const [kind, auto, d] of TARGETS) {
    if (parsed[kind]) dropKinds.add(kind);
    if (pkg[auto] !== false && (parsed[kind] || (await isDir(join(dir, d))))) off.add(auto);
  }
  // Tests the member's dependencies cannot build.
  const known = new Set([...BUILTIN, String(((parsed.lib ?? {}) as Table).name ?? pkg.name ?? "").replace(/-/g, "_")]);
  const tables: Table[] = [];
  for (const k of ["dependencies", "dev-dependencies", "build-dependencies"]) {
    tables.push((parsed[k] ?? {}) as Table);
    for (const t of Object.values((parsed.target ?? {}) as Table)) tables.push(((t as Table)[k] ?? {}) as Table);
  }
  for (const t of tables) for (const key of Object.keys(t)) known.add(key.replace(/-/g, "_"));
  const tests: { name: string; path: string }[] = [];
  if (pkg.autotests !== false && !parsed.test && (await isDir(join(dir, "tests")))) {
    // Bun's Glob does not expand a brace group holding a "/": one pattern per form.
    for (const p of [
      ...new Glob("tests/*.rs").scanSync({ cwd: dir }),
      ...new Glob("tests/*/main.rs").scanSync({ cwd: dir }),
    ].sort())
      tests.push({ name: p.endsWith("/main.rs") ? p.split("/")[1]! : p.slice(6, -3), path: p });
  }
  const broken = new Set<string>();
  for (const t of tests) {
    const src = (await read(join(dir, t.path)))!;
    const mods = new Set([...src.matchAll(/^\s*(?:pub\s+)?mod\s+(\w+)/gm)].map((m) => m[1]!));
    const used = [
      ...src.matchAll(/^\s*(?:pub\s+)?(?:use\s+(?:::)?(\w+)(?=::|;|\s+as\s)|extern\s+crate\s+(\w+))/gm),
    ].map((m) => (m[1] ?? m[2])!);
    const missing = used.filter((c) => !known.has(c) && !mods.has(c));
    if (missing.length) {
      broken.add(t.name);
      report.dropped.push(`${member}: test ${t.name} (${[...new Set(missing)].join(", ")})`);
    }
  }
  if (broken.size) off.add("autotests");
  if (!off.size && !dropKinds.size) continue;
  const out: string[] = [];
  let section = "";
  let skip = false;
  for (const line of text.split("\n")) {
    const h = /^\s*\[\[?\s*([^\]]+?)\s*\]\]?/.exec(line);
    if (h) {
      section = h[1]!;
      skip = /^\s*\[\[/.test(line) && dropKinds.has(section);
    }
    if (skip) continue;
    const auto = /^\s*(auto(?:examples|benches|tests))\s*=/.exec(line)?.[1];
    if (section === "package" && auto && off.has(auto)) continue;
    out.push(line);
    if (h && section === "package") for (const a of [...off].sort()) out.push(`${a} = false`);
  }
  while (out.at(-1) === "") out.pop();
  for (const t of tests.filter((t) => broken.size && !broken.has(t.name)))
    out.push("", "[[test]]", `name = "${t.name}"`, `path = "${t.path}"`);
  const next = out.join("\n") + "\n";
  if (next !== text) {
    await Bun.write(file, next);
    report.edited.push(
      `${member}/Cargo.toml (targets: ${[...off, ...[...dropKinds].map((k) => `[[${k}]]`)].join(", ")})`,
    );
  }
}

// Lint levels from labs.toml: `[lints]` for every member, then `[trees.<tree>.lints]`, over the member's own.
// A member's [lints] tables are rewritten at the end of its manifest, one tool per table.
const LEVEL = /^(allow|warn|deny|forbid|expect)$/;
const lintLine = (k: string, v: unknown) =>
  `${/^[\w-]+$/.test(k) ? k : JSON.stringify(k)} = ${typeof v === "string" ? JSON.stringify(v) : `{ ${inlineTable(v as Table)} }`}`;
for (const member of members) {
  const tree = member.split("/")[0]!;
  const policy = [config.lints ?? {}, config.trees[tree]?.lints ?? {}];
  if (!policy.some((p) => Object.keys(p).length)) continue;
  const file = join(ROOT, member, "Cargo.toml");
  const text = await read(file);
  if (!text) continue;
  const own = ((TOML.parse(text) as Table).lints ?? {}) as Table;
  if (own.workspace) continue;
  const want: Record<string, Table> = {};
  for (const layer of [own, ...policy])
    for (const [tool, levels] of Object.entries(layer)) Object.assign((want[tool] ??= {}), levels as Table);
  for (const [tool, levels] of Object.entries(want))
    for (const [k, v] of Object.entries(levels))
      if (typeof v === "string" ? !LEVEL.test(v) : !LEVEL.test(String((v as Table).level)))
        report.conflicts.push(`${member}: lints.${tool}.${k} has no level`);
  if (Bun.deepEquals(own, want)) continue;
  const out: string[] = [];
  let inLints = false;
  for (const line of text.split("\n")) {
    const h = /^\s*\[\s*([^\]]+?)\s*\]/.exec(line)?.[1];
    if (h) inLints = h === "lints" || h.startsWith("lints.");
    if (!inLints) out.push(line);
  }
  while (out.at(-1) === "") out.pop();
  for (const [tool, levels] of Object.entries(want))
    out.push("", `[lints.${tool}]`, ...Object.entries(levels).map(([k, v]) => lintLine(k, v)));
  await Bun.write(file, out.join("\n") + "\n");
  report.edited.push(`${member}/Cargo.toml (lints)`);
}

const sorted = Object.fromEntries(Object.entries(deps).sort(([a], [b]) => a.localeCompare(b)));

const patch = Object.fromEntries(
  [...inTree]
    .filter(([, d]) => members.has(d))
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([n, d]) => [n, { path: d }]),
);
for (const [name, { ws }] of Object.entries(trees)) {
  const tree = TOML.parse((await read(join(ROOT, "workspaces", `${name}.toml`))) ?? "") as Table;
  for (const [crate, spec] of Object.entries((tree.patch?.["crates-io"] ?? {}) as Table)) {
    if (crate in patch) continue;
    const p = (spec as Table).path as string | undefined;
    if (p && (await Bun.file(join(ROOT, name, p, "Cargo.toml")).exists())) patch[crate] = { path: join(name, p) };
    else report.dropped.push(`patch ${name}:${crate} (${p ?? "git"}: not materialized)`);
  }
  void ws;
}

const inline = (t: Table) =>
  TOML.stringify({ x: t })
    .replace(/^\[x\]\n/, "")
    .trim()
    .split("\n")
    .join(", ");
const lines = [
  "# Generated by `bun run unify` from labs.toml and workspaces/*.toml. Edit those, not this file.",
  "[workspace]",
  // "2": "3" plus an MSRV fallback, which would hold every dependency to the oldest member rust-version
  // (rubash 1.70) although rust-toolchain.toml pins one nightly for the whole workspace.
  'resolver = "2"',
  `members = [\n${[...members]
    .sort()
    .map((m) => `  "${m}",`)
    .join("\n")}\n]`,
  // Dev-dependencies on dropped crates still make them implicit members; plain `cargo` commands skip them.
  `default-members = [\n${[...members]
    .sort()
    .map((m) => `  "${m}",`)
    .join("\n")}\n]`,
  `exclude = [\n${Object.keys(trees)
    .map((t) => `  "${t}/target",`)
    .join("\n")}\n]`,
  "",
  "[workspace.dependencies]",
  ...Object.entries(sorted).map(([d, s]) => `${/^[\w-]+$/.test(d) ? d : JSON.stringify(d)} = { ${inline(s)} }`),
  "",
  "[patch.crates-io]",
  ...Object.entries(patch).map(([d, s]) => `${d} = { ${inline(s)} }`),
  "",
  (await read(join(ROOT, "profiles.toml")))?.trim() ?? "",
  "",
];
await Bun.write(join(ROOT, "Cargo.toml"), lines.join("\n"));
const locked = (await read(join(ROOT, "Cargo.lock"))) ?? "";
for (const [crate, version] of Object.entries(config.lock ?? {})) {
  if (locked.includes(`name = "${crate}"\nversion = "${version}"`)) continue;
  await $`cargo update -q -p ${crate} --precise ${version}`.cwd(ROOT);
  report.edited.push(`Cargo.lock: ${crate} ${version}`);
}
await Bun.write(join(ROOT, "build", "unify.json"), JSON.stringify(report, null, 1));
console.log(
  `${members.size} members, ${Object.keys(sorted).length} workspace deps, ${Object.keys(patch).length} patches; ` +
    `${report.edited.length} manifests edited, ${report.conflicts.length} conflicts, ${report.dropped.length} dropped`,
);
