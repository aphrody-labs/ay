// Ports an upstream Boa or Blitz pull request into its subtree (src/boa, src/blitz) as one commit, named after the PR.
//
//   bun scripts/port-pr.ts check boa-dev/boa#5683      does it apply as is?
//   bun scripts/port-pr.ts apply boa-dev/boa#5683      PR diff → the subtree (rejected hunks land as *.rej)
//   bun scripts/port-pr.ts commit boa-dev/boa#5683 [note…]
//                                                      the subtree's changes → a commit (PR author, PR title)
//
// The fork's bun/<version> branch follows with `git subtree push --prefix=<subtree> <fork> <branch>`.
import { $ } from "bun";
import { join } from "node:path";

const repo = join(import.meta.dir, "../../..");

// Upstream repo → subtree.
const subtrees: Record<string, string> = {
  "boa-dev/boa": "src/boa",
  "DioxusLabs/blitz": "src/blitz",
};

const [command, ref, ...note] = Bun.argv.slice(2);
const [, upstream, number] = /^([\w.-]+\/[\w.-]+)#(\d+)$/.exec(ref ?? "") ?? [];
const prefix = upstream && subtrees[upstream];
if (!command || !prefix || !number) throw new Error("usage: port-pr.ts check|apply|commit <owner/repo>#<n> [note…]");

const pr = JSON.parse(await $`gh pr view ${number} -R ${upstream} --json title,url,commits`.quiet().text()) as {
  title: string;
  url: string;
  commits: { authors: { name: string; email: string }[]; committedDate: string }[];
};

if (command === "apply" || command === "check") {
  const file = join(repo, "packages/bun-boa/build", `${upstream!.replace("/", "-")}-${number}.diff`);
  await Bun.write(file, await $`gh pr diff ${number} -R ${upstream}`.quiet().text());
  const clean = await $`git apply --check --directory=${prefix} ${file}`.cwd(repo).nothrow().quiet();
  if (clean.exitCode === 0) {
    if (command === "apply") await $`git apply --directory=${prefix} ${file}`.cwd(repo);
    console.log(`${prefix}: applies`);
  } else {
    const res = command === "apply" ? await $`git apply --reject --directory=${prefix} ${file}`.cwd(repo).nothrow().quiet() : clean;
    console.log(`${prefix}: REJECTS (diff: ${file})\n${res.stderr.toString().trim()}`);
    if (command === "check") process.exit(1);
  }
} else if (command === "commit") {
  await $`git add -A -- ${prefix} ':(exclude)*.rej' ':(exclude)*.orig'`.cwd(repo);
  if ((await $`git diff --cached --quiet -- ${prefix}`.cwd(repo).nothrow()).exitCode === 0)
    throw new Error(`${prefix}: nothing staged`);
  const author = pr.commits[0]!.authors[0]!;
  const date = pr.commits.at(-1)!.committedDate;
  const body = [`Port of ${pr.url}.`, ...note].join("\n\n");
  await $`git commit -q -m ${`${prefix.slice("src/".length)}: ${pr.title}`} -m ${body} -- ${prefix}`.cwd(repo).env({
    ...process.env,
    GIT_AUTHOR_NAME: author.name,
    GIT_AUTHOR_EMAIL: author.email,
    GIT_AUTHOR_DATE: date,
  });
  console.log(`${prefix}: committed ${(await $`git rev-parse --short HEAD`.cwd(repo).text()).trim()}`);
} else {
  throw new Error(`unknown command ${command}`);
}
