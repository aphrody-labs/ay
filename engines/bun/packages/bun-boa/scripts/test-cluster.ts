// Tests across the whole cluster: n1 builds one nextest archive, then every host runs its own partition of it.
// Only n1 holds a Bun clone; the others get the archive and n1's cargo-nextest (from n1) and this package's tests/
// (from here), which --workspace-remap stands in for the package root (CARGO_MANIFEST_DIR).
//   bun run test:cluster [--features build]
import { $ } from "bun";
import { join } from "node:path";

const hosts = ["n1", "n2", "n3"];
const features = Bun.argv.slice(2);
const repo = "bun-nojsc-test";
const dir = `/srv/xbuild/scratch/${repo}`;
const archive = `${dir}/archive.tar.zst`;
const nextest = `${dir}/cargo-nextest`;
const workspace = `${dir}/workspace`;
// The script goes over stdin to sh: login shells differ (n3's is fish) and would reparse a command line.
const sh = (host: string, script: string) => $`ssh -o BatchMode=yes ${host} sh -s < ${Buffer.from(script)}`;

// nextest only checks that the remapped workspace has a Cargo.toml; an empty one does.
await Promise.all(
  hosts.map(host =>
    sh(host, `rm -rf ${workspace} && mkdir -p ${workspace}/tests ${dir}/extract && touch ${workspace}/Cargo.toml`),
  ),
);
await $`bun ${import.meta.dir}/remote.ts cargo nextest archive ${features} --archive-file ${archive}`.env({
  ...Bun.env,
  BUN_BUILD_HOST: "n1",
  BUN_REMOTE_REPO: repo,
  GIT_NO_LAZY_FETCH: "1",
});
// n1 knows the others under other aliases, so it is pointed at the address this machine resolves them to.
const address = async (host: string) => {
  const config = Object.fromEntries((await $`ssh -G ${host}`.text()).split("\n").map(line => line.split(" ", 2)));
  return `${config.user}@${config.hostname}`;
};
await sh(hosts[0], `cp "$(command -v cargo-nextest)" ${nextest}`);
await Promise.all(
  hosts.map(async (host, i) => {
    // Owned by the host's user, not this machine's uid: n2 runs as root, and root in the test's user namespace
    // cannot write to a file of a uid the namespace does not map.
    await $`rsync -rlptz ${join(import.meta.dir, "../tests")}/ ${host}:${workspace}/tests/`;
    if (i)
      await sh(hosts[0], `rsync -az -e 'ssh -o BatchMode=yes' ${archive} ${nextest} ${await address(host)}:${dir}/`);
  }),
);
// Every partition runs with the host's Bun clones (/srv/xbuild/src) hidden, so a test that reads one fails instead
// of passing on n1 and on whichever host still has a stale tree. The inner namespace maps back to the host's own uid.
const hidden = "/srv/xbuild/src";
const results = await Promise.all(
  hosts.map((host, i) =>
    sh(
      host,
      `u=$(id -u) g=$(id -g)
exec unshare -rm sh -c 'if [ -d ${hidden} ]; then mount -t tmpfs none ${hidden} || exit; fi
  u=$1 g=$2; shift 2; exec unshare -U --map-user="$u" --map-group="$g" "$@"' sh "$u" "$g" \
  ${nextest} nextest run --archive-file ${archive} --workspace-remap ${workspace} \
  --extract-to ${dir}/extract --extract-overwrite --partition hash:${i + 1}/${hosts.length} --no-tests=pass </dev/null
`,
    )
      .nothrow()
      .then(r => ({ host, code: r.exitCode })),
  ),
);
console.log(results.map(r => `${r.host}:${r.code}`).join(" "));
process.exit(results.some(r => r.code) ? 1 : 0);
