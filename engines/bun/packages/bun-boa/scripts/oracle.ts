// Differential oracle: every input through upstream Bun (JavaScriptCore) and bun-boa, compared per checkpoint
// (transpile, bundle, run: stdout, stderr, exit). Differences are grouped into fingerprints; the match rate per
// checkpoint and the fingerprints go to build/oracle.json at the root of the repository (and to this package's
// tracked build/oracle.json), one line per input and checkpoint to build/oracle-inputs.jsonl. Fingerprints absent
// from the tracked copy are printed as new.
//
// Corpus: test/js/**/*.test.ts, the sources under test/bundler, and the most downloaded npm packages.
// n1 builds bun-boa and holds the upstream binary; both binaries, the corpus and this script go to every host
// that answers, and each runs its hash partition of the inputs.
//
//   bun run oracle [--hosts n1,n2,n3] [--npm 100] [--limit N] [--no-build]
//   bun scripts/oracle.ts worker <dir> <i>/<n> [limit]          (on a host)
import { $, Glob } from "bun";
import { availableParallelism } from "node:os";
import { join, relative } from "node:path";
import { parseArgs } from "node:util";

const CHECKPOINTS = ["transpile", "bundle", "run"] as const;
type Checkpoint = (typeof CHECKPOINTS)[number];
type Kind = "test" | "fixture" | "npm";
type Input = { id: string; kind: Kind; cwd: string; file: string };
type Output = { exit: string; stdout: string; stderr: string };
type Record = {
  id: string;
  checkpoint: Checkpoint;
  match: boolean;
  stream?: keyof Output;
  first_byte?: number;
  len_a?: number;
  len_b?: number;
  a?: string;
  b?: string;
  da?: string;
  db?: string;
};
type Fingerprint = {
  id: string;
  checkpoint: Checkpoint;
  stream: keyof Output;
  first_byte: number;
  len_a: number;
  len_b: number;
  count: number;
  example: string;
  a: string;
  b: string;
  da: string;
  db: string;
};

const DIR = "/srv/xbuild/scratch/oracle";
const TIMEOUT_MS = Number(Bun.env.BUN_ORACLE_TIMEOUT ?? 20_000);
const SOURCE = /\.(m|c)?(j|t)sx?$/;

if (Bun.argv[2] === "worker") await worker(Bun.argv[3], Bun.argv[4], Number(Bun.argv[5] ?? Infinity));
else await coordinate();

// ---------------------------------------------------------------- worker (runs on each host)

async function corpus(dir: string): Promise<Input[]> {
  const test = join(dir, "corpus/test");
  const inputs: Input[] = [];
  for await (const file of new Glob("js/**/*.test.ts").scan({ cwd: test }))
    if (!file.includes("node_modules/")) inputs.push({ id: `test/${file}`, kind: "test", cwd: test, file });
  for await (const file of new Glob("bundler/**/*").scan({ cwd: test }))
    if (SOURCE.test(file) && !/\.(test|spec)\.[^/]+$/.test(file) && !/(node_modules|__snapshots__)\//.test(file))
      inputs.push({ id: `test/${file}`, kind: "fixture", cwd: test, file });
  const npm = join(dir, "corpus/npm");
  const deps = Object.keys(
    (
      await Bun.file(join(npm, "package.json"))
        .json()
        .catch(() => ({}))
    ).dependencies ?? {},
  );
  for (const name of deps) {
    const file = `entries/${name.replaceAll("/", "__")}.mjs`;
    await Bun.write(
      join(npm, file),
      `import * as m from ${JSON.stringify(name)};\nconsole.log(Object.keys(m).sort().join("\\n"));\n`,
    );
    inputs.push({ id: `npm/${name}`, kind: "npm", cwd: npm, file });
  }
  return inputs.sort((a, b) => a.id.localeCompare(b.id));
}

// The package's own entry is what gets transpiled; the wrapper that imports it is what gets bundled and run.
function argv(input: Input, checkpoint: Checkpoint): string[] {
  if (checkpoint === "run") return input.kind === "test" ? ["test", `./${input.file}`] : [`./${input.file}`];
  let file = `./${input.file}`;
  if (input.kind === "npm" && checkpoint === "transpile")
    try {
      file = relative(input.cwd, Bun.resolveSync(input.id.slice(4), input.cwd)) || file;
    } catch {}
  return checkpoint === "transpile" ? ["build", "--no-bundle", file] : ["build", file];
}

// Only what differs between two runs of the same binary: paths of this host, times, versions, temp names.
function normalize(text: string, dir: string): string {
  return Bun.stripANSI(text)
    .replaceAll(join(dir, "corpus"), "<root>")
    .replaceAll(join(dir, "bin"), "<bin>")
    .replaceAll(/\/tmp\/[^\s'"`)]*/g, "<tmp>")
    .replaceAll(/\bv?\d+\.\d+\.\d+(-[\w.]+)?(\+[0-9a-f]+)?( \([0-9a-f]+\))?/g, "<version>")
    .replaceAll(/\d+(\.\d+)?\s?(ns|µs|us|ms|s)\b/g, "<time>");
}

async function exec(exe: string, args: string[], cwd: string, dir: string): Promise<Output> {
  // Its own process group, so a timeout also kills what the input spawned and its pipes close.
  const proc = Bun.spawn([exe, ...args], {
    cwd,
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
    detached: true,
    env: {
      ...Bun.env,
      NO_COLOR: "1",
      FORCE_COLOR: "0",
      TZ: "UTC",
      BUN_DEBUG_QUIET_LOGS: "1",
      TMPDIR: "/tmp",
      BUN_INSTALL_CACHE_DIR: join(dir, "cache"),
    },
  });
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    try {
      process.kill(-proc.pid, "SIGKILL");
    } catch {}
  }, TIMEOUT_MS);
  const [stdout, stderr, code] = await Promise.all([proc.stdout.text(), proc.stderr.text(), proc.exited]);
  clearTimeout(timer);
  try {
    process.kill(-proc.pid, "SIGKILL");
  } catch {}
  const exit = timedOut ? "timeout" : proc.signalCode ? `signal ${proc.signalCode}` : String(code);
  return { exit, stdout: normalize(stdout, dir), stderr: normalize(stderr, dir) };
}

function firstDifference(a: Buffer, b: Buffer): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) if (a[i] !== b[i]) return i;
  return n;
}

// The line around the first differing byte, which is what identifies a divergence across inputs.
function around(text: Buffer, at: number): string {
  const start = text.lastIndexOf(10, Math.max(0, at - 1)) + 1;
  let end = text.indexOf(10, at);
  if (end < 0) end = text.length;
  return text.subarray(start, Math.min(end, start + 160)).toString();
}

function lineFrom(text: Buffer, at: number): Buffer {
  const end = text.indexOf(10, at);
  return text.subarray(at, end < 0 ? text.length : end);
}

// Grouped by the differing span alone (the line minus what both sides share before and after it), so one
// divergence is one fingerprint whatever identifiers surround it.
function compare(id: string, checkpoint: Checkpoint, a: Output, b: Output): Record {
  for (const stream of ["stdout", "stderr", "exit"] as const) {
    if (a[stream] === b[stream]) continue;
    const [x, y] = [Buffer.from(a[stream]), Buffer.from(b[stream])];
    const at = firstDifference(x, y);
    const [la, lb] = [lineFrom(x, at), lineFrom(y, at)];
    let suffix = 0;
    while (suffix < la.length && suffix < lb.length && la[la.length - 1 - suffix] === lb[lb.length - 1 - suffix])
      suffix++;
    return {
      da: la
        .subarray(0, la.length - suffix)
        .subarray(0, 80)
        .toString(),
      db: lb
        .subarray(0, lb.length - suffix)
        .subarray(0, 80)
        .toString(),
      id,
      checkpoint,
      match: false,
      stream,
      first_byte: at,
      len_a: x.length,
      len_b: y.length,
      a: around(x, at),
      b: around(y, at),
    };
  }
  return { id, checkpoint, match: true };
}

async function worker(dir: string, partition: string, limit: number) {
  const [i, n] = partition.split("/").map(Number);
  const inputs = (await corpus(dir)).filter(input => Number(Bun.hash.wyhash(input.id) % BigInt(n)) === i - 1);
  const tasks = inputs.slice(0, limit).flatMap(input => CHECKPOINTS.map(checkpoint => ({ input, checkpoint })));
  const [jsc, boa] = [join(dir, "bin/bun-jsc"), join(dir, "bin/bun-boa")];
  let next = 0;
  await Promise.all(
    Array.from({ length: availableParallelism() }, async () => {
      while (next < tasks.length) {
        const { input, checkpoint } = tasks[next++];
        const args = argv(input, checkpoint);
        const [a, b] = [await exec(jsc, args, input.cwd, dir), await exec(boa, args, input.cwd, dir)];
        // The bundler reports errors from its parallel parse in no fixed order, upstream included.
        if (checkpoint === "bundle")
          for (const o of [a, b]) o.stderr = o.stderr.trimEnd().split("\n\n").sort().join("\n\n");
        process.stdout.write(JSON.stringify(compare(input.id, checkpoint, a, b)) + "\n");
      }
    }),
  );
}

// ---------------------------------------------------------------- coordinator (runs here)

async function coordinate() {
  const { values } = parseArgs({
    args: Bun.argv.slice(2),
    options: {
      hosts: { type: "string", default: "n1,n2,n3" },
      npm: { type: "string", default: "100" },
      limit: { type: "string" },
      "no-build": { type: "boolean", default: false },
    },
  });
  const pkg = join(import.meta.dir, "..");
  const repo = join(pkg, "../..");
  const remoteRepo = "bun-nojsc-oracle";
  const ssh = (host: string, script: string) => $`ssh -o BatchMode=yes -o ConnectTimeout=10 ${host} ${script}`;
  const all = values.hosts.split(",");
  const hosts = (
    await Promise.all(all.map(async host => ((await ssh(host, "true").nothrow().quiet()).exitCode ? null : host)))
  ).filter((h): h is string => !!h);
  const down = all.filter(h => !hosts.includes(h));
  if (down.length) console.error(`oracle: unreachable, left out: ${down.join(" ")}`);
  const [builder] = hosts;
  if (builder !== "n1") throw new Error("oracle: n1 builds bun-boa and must be reachable");

  // remote.ts brings n1's tree to this checkout, which is also where the corpus is copied from.
  const command = values["no-build"] ? ["true"] : ["cargo", "build", "--bin", "bun", "--features", "build"];
  await $`bun ${join(import.meta.dir, "remote.ts")} ${command}`.env({
    ...Bun.env,
    BUN_BUILD_HOST: builder,
    BUN_REMOTE_REPO: remoteRepo,
    GIT_NO_LAZY_FETCH: "1",
  });
  const boaBinary = `/srv/xbuild/target/bun-boa-${remoteRepo}/debug/bun`;
  const tree = `/srv/xbuild/src/${remoteRepo}`;
  // The revision a binary was built from stays next to it, so --no-build reports that one and not HEAD.
  if (!values["no-build"]) {
    const dirty = (await $`git -C ${repo} status --porcelain`.text()).trim() ? "-dirty" : "";
    const built = (await $`git -C ${repo} rev-parse HEAD`.text()).trim() + dirty;
    await ssh(builder, `echo ${built} > ${boaBinary}.rev`);
  }

  // Binaries on n1: the upstream one is the host's own Bun, so every host compares against the same build.
  await ssh(
    builder,
    `set -e; mkdir -p ${DIR}/bin ${DIR}/corpus/npm; ` +
      // A rename, not a copy over, so a binary still running from an earlier run does not block it.
      `cp -f ${boaBinary} ${DIR}/bin/.bun-boa && mv -f ${DIR}/bin/.bun-boa ${DIR}/bin/bun-boa; ` +
      `cp -f ${boaBinary}.rev ${DIR}/bin/bun-boa.rev 2>/dev/null || echo unknown > ${DIR}/bin/bun-boa.rev; ` +
      `cp -fL "$(command -v bun)" ${DIR}/bin/.bun-jsc && mv -f ${DIR}/bin/.bun-jsc ${DIR}/bin/bun-jsc`,
  );
  const revisions = {
    upstream: (await ssh(builder, `${DIR}/bin/bun-jsc --revision`).text()).trim(),
    boa: (await ssh(builder, `cat ${DIR}/bin/bun-boa.rev`).text()).trim(),
  };

  // The most downloaded npm packages, from ecosyste.ms (npm's own search has no download ranking).
  const top: { name: string; latest_release_number: string }[] = [];
  for (let page = 1; top.length < Number(values.npm); page++) {
    const url = `https://packages.ecosyste.ms/api/v1/registries/npmjs.org/packages?sort=downloads&order=desc&per_page=100&page=${page}`;
    const res = await fetch(url, { headers: { "User-Agent": "bun-boa-oracle/0 (+https://github.com/oven-sh/bun)" } });
    if (!res.ok) throw new Error(`oracle: ${url}: ${res.status}`);
    const batch = (await res.json()) as typeof top;
    if (!batch.length) break;
    top.push(...batch.slice(0, Number(values.npm) - top.length));
  }
  const npmPackage = {
    name: "oracle-npm",
    private: true,
    dependencies: Object.fromEntries(top.map(p => [p.name, p.latest_release_number])),
  };

  // The corpus is n1's tree; packages are installed there once, and n1 copies all of it to the others.
  await ssh(
    builder,
    `set -e; mkdir -p ${DIR}/corpus/packages; rsync -a --delete --exclude node_modules --exclude .git ${tree}/test/ ${DIR}/corpus/test/; ` +
      `cp -f ${tree}/bunfig.toml ${tree}/tsconfig.json ${tree}/tsconfig.base.json ${DIR}/corpus/; ` +
      // test/package.json links these from the root of the repository.
      `for p in bun-types bun-plugin-svelte; do rsync -a --delete --exclude node_modules ${tree}/packages/$p/ ${DIR}/corpus/packages/$p/; done`,
  );
  const root = (await Bun.file(join(repo, "package.json")).json()) as { devDependencies: { react: string } };
  const rootPackage = { private: true, devDependencies: { react: root.devDependencies.react } };
  await $`ssh -o BatchMode=yes ${builder} ${`cat > ${DIR}/corpus/package.json`} < ${Buffer.from(JSON.stringify(rootPackage))}`;
  await Promise.all(
    hosts.map(async host => {
      await ssh(host, `mkdir -p ${DIR}/corpus ${DIR}/bin ${DIR}/cache`);
      await $`rsync -az ${join(import.meta.dir, "oracle.ts")} ${`${host}:${DIR}/oracle.ts`}`;
    }),
  );
  await $`ssh -o BatchMode=yes ${builder} ${`cat > ${DIR}/corpus/npm/package.json`} < ${Buffer.from(JSON.stringify(npmPackage, null, 2))}`;
  await ssh(
    builder,
    `set -e; export BUN_INSTALL_CACHE_DIR=${DIR}/cache; ` +
      `cd ${DIR}/corpus && ${DIR}/bin/bun-jsc install --linker hoisted >/dev/null; ` +
      `cd ${DIR}/corpus/test && ${DIR}/bin/bun-jsc install --frozen-lockfile >/dev/null; ` +
      `cd ${DIR}/corpus/npm && ${DIR}/bin/bun-jsc install --linker hoisted >/dev/null`,
  );
  const address = async (host: string) => {
    const config = Object.fromEntries((await $`ssh -G ${host}`.text()).split("\n").map(line => line.split(" ", 2)));
    return `${config.user}@${config.hostname}`;
  };
  await Promise.all(
    hosts.slice(1).map(async host => {
      const to = await address(host);
      await ssh(
        builder,
        `set -e; cd ${DIR}; rsync -az -e 'ssh -o BatchMode=yes' bin/ ${to}:${DIR}/bin/; ` +
          `rsync -az --delete -e 'ssh -o BatchMode=yes' corpus/ ${to}:${DIR}/corpus/`,
      );
    }),
  );

  const started = performance.now();
  const parts = await Promise.all(
    hosts.map(async (host, i) => {
      const r = await ssh(
        host,
        `cd ${DIR} && bun oracle.ts worker ${DIR} ${i + 1}/${hosts.length} ${values.limit ?? ""}`,
      )
        .nothrow()
        .quiet();
      if (r.exitCode) console.error(`oracle: ${host} exited ${r.exitCode}: ${r.stderr.toString().slice(-2000)}`);
      return r.stdout
        .toString()
        .split("\n")
        .filter(Boolean)
        .map(line => JSON.parse(line) as Record);
    }),
  );
  const records = parts.flat().sort((a, b) => a.id.localeCompare(b.id) || a.checkpoint.localeCompare(b.checkpoint));

  // The run goes to the repository's build/; the copy in this package is tracked, and is the baseline that
  // tells new fingerprints from known ones.
  const out = join(repo, "build/oracle.json");
  const baseline = join(pkg, "build/oracle.json");
  const previous = new Set(
    (
      (
        await Bun.file(baseline)
          .json()
          .catch(() => ({}))
      ).fingerprints ?? []
    ).map((f: Fingerprint) => f.id),
  );
  // Digits are masked so one divergence across inputs (line numbers, counts, offsets) is one fingerprint.
  const mask = (s: string) => s.replaceAll(/\d+/g, "0");
  const groups = new Map<string, Fingerprint>();
  for (const r of records) {
    if (r.match) continue;
    const id = Bun.hash
      .wyhash(`${r.checkpoint}\0${r.stream}\0${mask(r.da!)}\0${mask(r.db!)}`)
      .toString(16)
      .padStart(16, "0");
    const group = groups.get(id);
    if (group) group.count++;
    else
      groups.set(id, {
        id,
        checkpoint: r.checkpoint,
        stream: r.stream!,
        first_byte: r.first_byte!,
        len_a: r.len_a!,
        len_b: r.len_b!,
        count: 1,
        example: r.id,
        a: r.a!,
        b: r.b!,
        da: r.da!,
        db: r.db!,
      });
  }
  const fingerprints = [...groups.values()].sort((a, b) => b.count - a.count || a.id.localeCompare(b.id));
  const rates = Object.fromEntries(
    CHECKPOINTS.map(checkpoint => {
      const of = records.filter(r => r.checkpoint === checkpoint);
      const matched = of.filter(r => r.match).length;
      return [checkpoint, { inputs: of.length, matched, rate: of.length ? +(matched / of.length).toFixed(4) : 0 }];
    }),
  );
  const kind = (id: string): Kind =>
    id.startsWith("npm/") ? "npm" : id.startsWith("test/bundler/") ? "fixture" : "test";
  const kinds: { [k in Kind]?: number } = {};
  for (const id of new Set(records.map(r => r.id))) kinds[kind(id)] = (kinds[kind(id)] ?? 0) + 1;
  const report =
    JSON.stringify(
      {
        meta: {
          date: new Date().toISOString(),
          ...revisions,
          rebuilt: !values["no-build"],
          hosts,
          down,
          timeout_ms: TIMEOUT_MS,
          seconds: Math.round((performance.now() - started) / 1000),
          inputs: kinds,
        },
        ...rates,
        fingerprints,
      },
      null,
      2,
    ) + "\n";
  await Promise.all([
    Bun.write(out, report),
    Bun.write(baseline, report),
    Bun.write(join(repo, "build/oracle-inputs.jsonl"), records.map(r => JSON.stringify(r)).join("\n") + "\n"),
  ]);

  for (const [checkpoint, r] of Object.entries(rates))
    console.log(`${checkpoint.padEnd(9)} ${r.matched}/${r.inputs} ${(r.rate * 100).toFixed(2)}%`);
  const fresh = fingerprints.filter(f => !previous.has(f.id));
  console.log(`${fingerprints.length} fingerprints, ${fresh.length} new`);
  for (const f of fresh.slice(0, 20))
    console.log(`  new ${f.checkpoint}/${f.stream} ×${f.count} ${f.example}\n    a: ${f.a}\n    b: ${f.b}`);
}
