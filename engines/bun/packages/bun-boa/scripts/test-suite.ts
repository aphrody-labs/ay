// Runs Bun's own test files with the Boa build's `bun test`, one process per file, all cores at once, and prints one
// JSON line: per file, the counts `bun test` printed. Runs on each cluster host (scripts/test-boa.ts).
//   bun scripts/test-suite.ts <bun-boa binary> [--partition hash:M/N] [dir or test file…]
// --partition is nextest's: a file belongs to shard hash(path) % N + 1, and this run takes shard M, or the shards
// M1-M2 (so a host's share can follow its core count).
import { Glob } from "bun";
import { availableParallelism } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

const { values, positionals } = parseArgs({
  args: Bun.argv.slice(2),
  options: { partition: { type: "string" } },
  allowPositionals: true,
});
const [exe, ...dirs] = positionals;
if (!exe) throw new Error("usage: bun scripts/test-suite.ts <bun-boa binary> [--partition hash:M/N] [dir…]");
const repo = join(import.meta.dir, "../../..");
const roots = dirs.length ? dirs : ["test/js/bun", "test/js/node", "test/js/web", "test/cli"];
const TIMEOUT_MS = Number(Bun.env.BUN_BOA_TEST_TIMEOUT ?? 30_000);
const JOBS = Number(Bun.env.BUN_BOA_TEST_JOBS ?? availableParallelism());

let inPartition: (file: string) => boolean = () => true;
if (values.partition) {
  const m = values.partition.match(/^hash:(\d+)(?:-(\d+))?\/(\d+)$/);
  if (!m) throw new Error(`--partition ${values.partition}: expected hash:M/N or hash:M1-M2/N`);
  const [lo, hi, n] = [Number(m[1]), Number(m[2] ?? m[1]), Number(m[3])];
  if (lo < 1 || hi < lo || hi > n) throw new Error(`--partition ${values.partition}: shards out of range`);
  inPartition = file => {
    const shard = Number(Bun.hash.xxHash64(file) % BigInt(n)) + 1;
    return shard >= lo && shard <= hi;
  };
}

type Result = { file: string; pass: number; fail: number; skip: number; todo: number; exit: number | null; ms: number };
const files: string[] = [];
const testFile = new Glob("**/*.{test,spec}.{ts,tsx,js,jsx,mjs,cjs,mts,cts}");
for (const root of roots) {
  if (testFile.match(root)) {
    files.push(root);
    continue;
  }
  for await (const f of new Glob(`${root}/**/*.{test,spec}.{ts,tsx,js,jsx,mjs,cjs,mts,cts}`).scan({ cwd: repo }))
    if (!f.includes("/node_modules/") && inPartition(f)) files.push(f);
}
files.sort();

const count = (text: string, label: string) => Number(text.match(new RegExp(`^ (\\d+) ${label}$`, "m"))?.[1] ?? 0);
async function run(file: string): Promise<Result> {
  const start = performance.now();
  const proc = Bun.spawn([exe, "test", `./${file}`], {
    cwd: repo,
    stdin: "ignore",
    stdout: "ignore",
    stderr: "pipe",
    timeout: TIMEOUT_MS,
    killSignal: "SIGKILL",
    env: { ...Bun.env, NO_COLOR: "1" },
  });
  const stderr = await proc.stderr.text();
  const exit = await proc.exited;
  const killed = proc.signalCode !== null;
  return {
    file,
    pass: count(stderr, "pass"),
    fail: killed ? Math.max(1, count(stderr, "fail")) : count(stderr, "fail") || (exit === 0 ? 0 : 1),
    skip: count(stderr, "skip"),
    todo: count(stderr, "todo"),
    exit: killed ? null : exit,
    ms: Math.round(performance.now() - start),
  };
}

const results: Result[] = [];
let next = 0;
await Promise.all(
  Array.from({ length: Math.min(JOBS, files.length) }, async () => {
    while (next < files.length) results.push(await run(files[next++]));
  }),
);
results.sort((a, b) => a.file.localeCompare(b.file));
console.log(JSON.stringify({ roots, results }));
