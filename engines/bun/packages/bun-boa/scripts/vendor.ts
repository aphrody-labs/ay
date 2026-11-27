// Materialises each patches/<crate>-<version>/ series into vendor/<crate>-<version>/, every crate concurrently.
// The crates.io tarball becomes a git repo tagged `vendor-base` with the series applied on top by `git am`
// (authors and messages kept); a `.patch-sha8` marker skips a series that did not change.
// Boa and Blitz are not vendored: they are subtrees in src/boa and src/blitz, patched by commits there.
// To change a crate: commit in vendor/<crate>-<version>/, then `git format-patch vendor-base -o ../../patches/<crate>-<version>/`.
import { $ } from "bun";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
export const env = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_SYSTEM: "/dev/null",
  GIT_AUTHOR_NAME: "vendor",
  GIT_AUTHOR_EMAIL: "vendor@localhost",
  GIT_COMMITTER_NAME: "vendor",
  GIT_COMMITTER_EMAIL: "vendor@localhost",
  GIT_AUTHOR_DATE: "2000-01-01T00:00:00Z",
  GIT_COMMITTER_DATE: "2000-01-01T00:00:00Z",
};

export async function materialise(name: string): Promise<string> {
  const [, crate, version] = /^(.+)-(\d+\.\d+\.\d+.*)$/.exec(name) ?? [];
  if (!crate || !version) throw new Error(`patches/${name}: expected <crate>-<version>`);
  const series = [...new Bun.Glob("*.patch").scanSync(join(root, "patches", name))].sort();
  const hasher = new Bun.CryptoHasher("sha256");
  hasher.update("crates.io");
  for (const patch of series) hasher.update(await Bun.file(join(root, "patches", name, patch)).bytes());
  const sha8 = hasher.digest("hex").slice(0, 8);
  const dir = join(root, "vendor", name);
  const marker = Bun.file(join(dir, ".patch-sha8"));
  if ((await marker.exists()) && (await marker.text()).trim() === sha8) return `${name}: up to date`;

  await $`rm -rf ${dir} && mkdir -p ${dir}`;
  const res = await fetch(`https://static.crates.io/crates/${crate}/${crate}-${version}.crate`);
  if (!res.ok) throw new Error(`${name}: crates.io answered ${res.status}`);
  await Bun.write(`${dir}.crate`, res);
  await $`tar -xzf ${`${dir}.crate`} -C ${dir} --strip-components=1 && rm ${`${dir}.crate`}`;
  await $`git init -q && git add -A && git commit -q -m ${`${crate} ${version} from crates.io`} && git tag vendor-base`
    .cwd(dir)
    .env(env);
  if (series.length)
    await $`git am -q --committer-date-is-author-date ${series.map(p => join(root, "patches", name, p))}`
      .cwd(dir)
      .env(env);
  await Bun.write(marker, `${sha8}\n`);
  return `${name}: ${series.length} patches`;
}

// Cargo.toml's [patch.crates-io] table (the end of the file) lists every series, so a vendored crate is never left
// on its crates.io tarball while a patched dependent expects its changes. Rows that are not into vendor/ (the
// in-tree crates) are kept first, as written.
async function patchTable(names: string[]) {
  const manifest = Bun.file(join(root, "Cargo.toml"));
  const text = await manifest.text();
  const header = "[patch.crates-io]\n";
  const at = text.indexOf(header);
  if (at < 0) throw new Error(`Cargo.toml: no ${header.trim()} table`);
  const kept = text
    .slice(at + header.length)
    .split("\n")
    .filter(row => row && !row.includes(`path = "vendor/`))
    .map(row => `${row}\n`);
  const rows = names
    .toSorted()
    .map(name => `${/^(.+)-\d+\.\d+\.\d+/.exec(name)![1]} = { path = "vendor/${name}" }\n`);
  const next = text.slice(0, at + header.length) + [...kept, ...rows].join("");
  if (next !== text) await Bun.write(manifest, next);
}

if (import.meta.main) {
  const names = [...new Bun.Glob("*/*.patch").scanSync(join(root, "patches"))]
    .map(p => p.split("/")[0]!)
    .filter((n, i, all) => all.indexOf(n) === i);
  const [results] = await Promise.all([Promise.allSettled(names.map(materialise)), patchTable(names)]);
  for (const r of results) {
    if (r.status === "fulfilled") console.log(r.value);
    else console.error(String(r.reason));
  }
  process.exit(results.some(r => r.status === "rejected") ? 1 : 0);
}
