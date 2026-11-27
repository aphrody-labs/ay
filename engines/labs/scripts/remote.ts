#!/usr/bin/env bun
// Run a command in the n1 copy of this repo (builds never run locally): push HEAD, fast-forward there, run.
//   bun run remote <cmd…>        LABS_HOST (n1), LABS_DIR (/srv/xbuild/labs)
import { $ } from "bun";

const host = process.env.LABS_HOST ?? "n1";
const dir = process.env.LABS_DIR ?? "/srv/xbuild/labs";
const cmd = Bun.argv.slice(2).join(" ");
const branch = (await $`git branch --show-current`.text()).trim();
// --no-verify: git-lfs's pre-push hook walks every new commit, and a tree pull brings upstream's whole history;
// labs keeps LFS pointers only (sync's GIT_LFS_SKIP_SMUDGE), so there is nothing to upload.
await $`git push -q --no-verify origin HEAD`.nothrow();
// labs' own cargo home: the shared one's package-cache lock is held for whole builds by other trees' sessions.
const env =
  "RUSTUP_HOME=/srv/toolchain/rustup CARGO_HOME=/srv/xbuild/cargo-home-labs RUSTC_WRAPPER=sccache RUSTFLAGS='-C linker=clang -C link-arg=--ld-path=/usr/local/bin/wild -C target-cpu=x86-64-v3'";
const r =
  await $`ssh -o BatchMode=yes ${host} ${`cd ${dir} && git fetch -q origin ${branch} && git checkout -q ${branch} && git merge -q --ff-only origin/${branch} && ${env} ${cmd}`}`.nothrow();
process.exit(r.exitCode);
