#!/usr/bin/env bun
// Add (first run, `git subtree add`) or pull every tree of labs.toml, then `bun run unify`.
// A pull is one commit: upstream's changes since the last recorded split (the `git-subtree-split` trailer), applied
// as a 3-way patch, with the new split in its trailers. Not a merge of upstream's history: GitHub refuses a push
// whose commits name LFS objects labs does not store (GH008; vello's snapshots), and labs' own history of the trees
// was rewritten without them, so upstream's commits are not ancestors of what is here.
// Conflicts in Cargo.toml files are ours to regenerate: take upstream's, unify re-applies. Any other conflict stops.
//   bun run sync [tree…]
import { $, TOML } from "bun";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const { trees } = TOML.parse(await Bun.file(join(ROOT, "labs.toml")).text()) as {
  trees: Record<string, { remote: string; branch: string }>;
};
const only = Bun.argv.slice(2);
$.cwd(ROOT);
// LFS assets (vello snapshots) are not needed to build: keep the pointers.
$.env({ ...process.env, GIT_LFS_SKIP_SMUDGE: "1" });

const conflicted = async () => (await $`git diff --name-only --diff-filter=U`.text()).split("\n").filter(Boolean);
// Cargo.toml conflicts take upstream's file (from `rev`, under `prefix`); any other conflict stops.
async function resolve(name: string, rev: string, prefix: string) {
  const files = await conflicted();
  const other = files.filter((f) => !f.endsWith("Cargo.toml"));
  if (other.length) throw new Error(`${name}: pull conflicts: ${other.join(" ")}`);
  for (const f of files) {
    const theirs = await $`git show ${`${rev}:${f.slice(prefix.length + 1)}`}`.quiet().nothrow();
    if (theirs.exitCode === 0) await Bun.write(join(ROOT, f), theirs.stdout);
    await $`git add -- ${f}`;
  }
}

// Files a pull touches that are Git LFS pointers on either side (vello's snapshot PNGs). They stay out of it: GitHub
// refuses a push naming LFS objects labs does not store (GH008), and no member reads them.
async function lfsPointers(from: string, to: string) {
  const changed = (await $`git diff --name-only ${from} ${to}`.text()).split("\n").filter(Boolean);
  const isPointer = async (rev: string, p: string) => {
    const r = await $`git cat-file blob ${`${rev}:${p}`}`.quiet().nothrow();
    return (
      r.exitCode === 0 &&
      r.stdout.byteLength < 1024 &&
      r.stdout.toString().startsWith("version https://git-lfs.github.com/spec/")
    );
  };
  const pointers: string[] = [];
  for (const p of changed) if ((await isPointer(to, p)) || (await isPointer(from, p))) pointers.push(p);
  return pointers;
}

for (const [name, { remote, branch }] of Object.entries(trees)) {
  if (only.length && !only.includes(name)) continue;
  const exists = (await $`test -d ${name}`.nothrow()).exitCode === 0;
  const message = `labs: ${exists ? "pull" : "add"} ${name} from ${remote} ${branch}`;
  if (!exists) {
    await $`git subtree add --prefix=${name} ${remote} ${branch} -m ${message}`;
    continue;
  }
  await $`git fetch -q ${remote} ${branch}`;
  const head = (await $`git rev-parse FETCH_HEAD^{commit}`.text()).trim();
  const split = (
    await $`git log -1 ${`--grep=^git-subtree-dir: ${name}$`} ${"--format=%(trailers:key=git-subtree-split,valueonly)"}`.text()
  ).trim();
  if (split === head || (split && (await $`git merge-base --is-ancestor ${head} ${split}`.nothrow()).exitCode === 0))
    continue;
  if (!split) throw new Error(`${name}: no git-subtree-split trailer for this tree`);
  // A virtual tree root unify deleted (its tables live in workspaces/<tree>.toml) comes back as upstream's
  // file, which unify splits again.
  const root = join(name, "Cargo.toml");
  const noRoot = (await $`test -f ${root}`.nothrow()).exitCode !== 0;
  if (noRoot && (await $`git diff --quiet ${split} ${head} -- Cargo.toml`.nothrow()).exitCode !== 0) {
    await Bun.write(join(ROOT, root), await $`git show ${`${head}:Cargo.toml`}`.quiet().text());
    await $`git add -- ${root}`;
  }
  const skip = [...(noRoot ? ["Cargo.toml"] : []), ...(await lfsPointers(split, head))].map((p) => `:(exclude)${p}`);
  const patch = await $`git diff --binary ${split} ${head} -- . ${skip}`.quiet().arrayBuffer();
  if (patch.byteLength) {
    const r = await $`git apply -3 --index --directory=${name} < ${new Response(patch)}`.nothrow();
    if (r.exitCode !== 0 && !(await conflicted()).length) throw new Error(`${name}: patch ${split}..${head} failed`);
    if (r.exitCode !== 0) await resolve(name, head, name);
  }
  const range = `${split.slice(0, 10)}..${head.slice(0, 10)}`;
  const trailers = `git-subtree-dir: ${name}\ngit-subtree-split: ${head}`;
  await $`git commit -q --allow-empty -m ${`${message}\n\nUpstream ${range}, as a patch.\n\n${trailers}`}`;
}
await $`bun run unify`;
await $`git add -A && git diff --cached --quiet || git commit -q -m ${"labs: unify"}`.nothrow();
