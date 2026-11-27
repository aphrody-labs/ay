// Prepares a checkout for `cargo` on the build host, every step concurrently: Bun's Rust vendor deps (from
// scripts/build/deps/*.ts), this package's patched crates (vendor.ts), the generated Rust sources that Bun's
// crates `include!` (the codegen commands of build/debug/build.ninja, run without building anything native), and
// the bindings of the workspace's crates on bun_js (src/codegen/bindgen-js.ts).
import { $, Glob } from "bun";
import { join } from "node:path";
import { lolhtml } from "../../../scripts/build/deps/lolhtml.ts";
import { rustArgon2 } from "../../../scripts/build/deps/rust-argon2.ts";
// config.ts first: webkit.ts and config.ts import each other.
import "../../../scripts/build/config.ts";
import { WEBKIT_VERSION } from "../../../scripts/build/deps/webkit.ts";
import type { Dependency } from "../../../scripts/build/source.ts";

const repo = join(import.meta.dir, "../../..");
const codegen = join(repo, "build/debug/codegen");
const GENERATORS = ["generate-removable-features.ts", "generate-node-errors.ts"];

async function rustDep(dep: Dependency, dir: string) {
  if (await Bun.file(join(repo, "vendor", dir, "Cargo.toml")).exists()) return `vendor/${dir}: present`;
  const source = dep.source(undefined as never);
  if (source.kind !== "github-archive") throw new Error(`${dep.name}: unsupported source ${source.kind}`);
  const cache = join(repo, "build/vendor-cache");
  await $`bun scripts/build/fetch-cli.ts dep ${dep.name} ${source.repo} ${source.commit} vendor/${dir} ${cache} ${dep.patches ?? []}`
    .cwd(repo)
    .quiet();
  return `vendor/${dir}: fetched`;
}

async function generated() {
  // build.ninja, which a configure that failed half-way has not written (build_options.rs it may have).
  if (!(await Bun.file(join(repo, "build/debug/build.ninja")).exists()))
    await $`bun run build --configure-only`.cwd(repo).quiet();
  const commands = (await $`ninja -C build/debug -t commands`.cwd(repo).quiet().text())
    .split("\n")
    .filter(line => GENERATORS.some(g => line.includes(g)));
  await Promise.all([...commands.map(c => $`sh -c ${c}`.cwd(join(repo, "build/debug")).quiet()), aotSchema()]);
  return `codegen: ${commands.length + 1} generators`;
}

// src/js as the Boa build embeds it (build.rs): bundle-modules.ts writes build/debug/js-boa when BUN_BOA_JS_DIR
// names it. The trim's builtin-uses.json is an output of the native codegen, which this build does not run: without
// it every builtin is kept.
async function boaModules() {
  if (!(await Bun.file(join(repo, "build/debug/build.ninja")).exists()))
    await $`bun run build --configure-only`.cwd(repo).quiet();
  const command = (await $`ninja -C build/debug -t commands`.cwd(repo).quiet().text())
    .split("\n")
    .find(line => line.includes("src/codegen/bundle-modules.ts"));
  if (!command) throw new Error("build.ninja has no bundle-modules.ts command");
  let runnable = command;
  const uses = command.match(/ --builtin-uses=(\S+)/);
  if (uses && !(await Bun.file(uses[1]).exists())) runnable = runnable.replace(uses[0], "");
  // A host without the WebKit that configure found gets the engine's generators from engineAot().
  const engine = command.match(/ --engine-directory=(\S+)/);
  const env: Record<string, string> = { BUN_BOA_JS_DIR: join(repo, "build/debug/js-boa"), CLAUDECODE: "1" };
  if (engine && !(await Bun.file(join(engine[1], "builtins/generator/index.ts")).exists())) {
    runnable = runnable.replace(engine[0], "");
    env.AOT_ENGINE_DIRECTORY = await engineAot();
  }
  const result = await $`sh -c ${runnable}`
    .cwd(join(repo, "build/debug"))
    .env({ ...process.env, ...env })
    .quiet()
    .nothrow();
  if (result.exitCode !== 0) throw new Error(`bundle-modules.ts: ${result.stderr.toString().trim().slice(-2000)}`);
  const out = env.BUN_BOA_JS_DIR;
  return `codegen: src/js for Boa in ${out}`;
}

// Every `.bind.ts` of a member of this workspace, as Rust against bun_js. The crate is the member's package. A member
// also lists schemas outside its own directory, which the JavaScriptCore build shares, relative to it:
// `[package.metadata.bun-js] bindings = ["../runtime/node/node_path.bind.ts"]`.
async function bunJsBindings() {
  const root = join(import.meta.dir, "..");
  const toml = async (path: string) => Bun.TOML.parse(await Bun.file(path).text()) as Record<string, any>;
  const sources: string[] = [];
  for (const member of (await toml(join(root, "Cargo.toml"))).workspace.members as string[]) {
    const dir = join(root, member);
    const { package: pkg } = await toml(join(dir, "Cargo.toml"));
    const crate = pkg.name as string;
    for await (const file of new Glob("**/*.bind.ts").scan({ cwd: dir })) sources.push(`${crate}=${join(dir, file)}`);
    for (const file of (pkg.metadata?.["bun-js"]?.bindings ?? []) as string[]) {
      const path = join(dir, file);
      if (!(await Bun.file(path).exists())) throw new Error(`${member}/Cargo.toml: bindings: ${file} does not exist`);
      sources.push(`${crate}=${path}`);
    }
  }
  // A schema can import src/codegen/builtins/lib.ts, which loads the engine's aot/builtins/generator.
  await $`bun src/codegen/bindgen-js.ts --sources=${sources.sort().join(",")} --codegen-path=${codegen}`
    .cwd(repo)
    .env({ ...process.env, AOT_ENGINE_DIRECTORY: await engineAot() })
    .quiet();
  return `codegen: bun_js bindings of ${sources.length} schemas`;
}

// WebKit's Source/JavaScriptCore/aot without its tests, at the pinned commit: the TypeScript generators of the AOT
// schema (ir/) and of the table of built-ins (builtins/), which Bun's codegen imports. Nothing native.
let engine: Promise<string> | undefined;
function engineAot(): Promise<string> {
  return (engine ??= (async () => {
    const dir = join(repo, `build/vendor-cache/webkit-aot-${WEBKIT_VERSION}/aot`);
    if (await Bun.file(join(dir, ".complete")).exists()) return dir;
    const tree = `repos/oven-sh/WebKit/git/trees/${WEBKIT_VERSION}:Source/JavaScriptCore/aot?recursive=1`;
    const files = ((await $`gh api ${tree}`.json()) as { tree: { type: string; path: string }[] }).tree
      .filter(e => e.type === "blob" && !e.path.startsWith("tests/"))
      .map(e => e.path);
    const raw = `https://raw.githubusercontent.com/oven-sh/WebKit/${WEBKIT_VERSION}/Source/JavaScriptCore/aot`;
    await Promise.all(
      files.map(async f =>
        Bun.write(
          join(dir, f),
          await fetch(`${raw}/${f}`).then(r => (r.ok ? r : Promise.reject(new Error(`${f}: ${r.status}`)))),
        ),
      ),
    );
    await Bun.write(join(dir, ".complete"), "");
    return dir;
  })());
}

// `bun_aot_interface` includes the Rust half of WebKit's AOT schema, which its generator writes.
async function aotSchema() {
  if (await Bun.file(join(codegen, "aot_schema_interface.rs")).exists()) return;
  const dir = await engineAot();
  await $`bun run ${join(dir, "ir/generator/index.ts")} --schema ${join(dir, "ir/AOTSchema.def")} --rust-out ${codegen}`
    .cwd(repo)
    .quiet();
}

// The OFL faces kitesurf-cdp embeds (no system fonts), from kitesurf's font set.
const FONTS_BASE =
  "https://raw.githubusercontent.com/cloudflare/kitesurf/0affe70c9202b80396e3c5bf147018cb760af633/packages/engine/public/fonts";
const FONTS = [
  "Arimo.ttf",
  "Arimo-Italic.ttf",
  "Tinos-Regular.ttf",
  "Tinos-Bold.ttf",
  "Tinos-Italic.ttf",
  "NotoSansMono.ttf",
  "NotoSans.ttf",
  "NotoSansSymbols2.ttf",
  "OFL.txt",
];

async function fonts() {
  const dir = join(import.meta.dir, "../vendor/fonts");
  const missing = [];
  for (const name of FONTS) if (!(await Bun.file(join(dir, name)).exists())) missing.push(name);
  await Promise.all(
    missing.map(async name => {
      const res = await fetch(`${FONTS_BASE}/${name}`);
      if (!res.ok) throw new Error(`fonts/${name}: ${res.status}`);
      await Bun.write(join(dir, name), res);
    }),
  );
  return `vendor/fonts: ${missing.length} fetched`;
}

const steps = [
  rustDep(lolhtml, "lolhtml"),
  rustDep(rustArgon2, "rust-argon2"),
  $`bun scripts/vendor.ts`.cwd(join(import.meta.dir, "..")).text(),
  generated(),
  boaModules(),
  bunJsBindings(),
  fonts(),
];
// Every step runs to its end; one that failed fails the setup, so that cargo does not build what it left stale.
let failed = 0;
for (const r of await Promise.allSettled(steps)) {
  if (r.status === "rejected") failed++;
  // A command run `.quiet()` failed: what it printed is the reason.
  const printed = r.status === "rejected" && r.reason instanceof $.ShellError ? `\n${r.reason.stderr}` : "";
  console.log(r.status === "fulfilled" ? String(r.value).trim() : `error: ${r.reason}${printed}`);
}
if (failed) {
  console.error(`setup: ${failed} of ${steps.length} steps failed`);
  process.exit(1);
}
