// Bun.spawn, Bun.spawnSync, Bun.which and process.kill: each line is what Bun on JavaScriptCore prints too
// (expected.txt). Nothing here depends on a pid or on time.
// The bytes of a Uint8Array (a Buffer, which has a toJSON of its own where there is Buffer) as their text.
const text = (v: unknown) => (v instanceof Uint8Array ? `<${new TextDecoder().decode(v)}>` : v);
const show = (value: unknown): string => JSON.stringify(text(value), (_, v) => (typeof v === "bigint" ? `${typeof v}` : v));
const r = (f: () => unknown): string => {
  try {
    return show(f());
  } catch (e: any) {
    // A NUL of the message as `\0`, so that this prints text.
    return `throws ${e?.name}: ${String(e?.message).replaceAll("\0", "\\0")} (${e?.code})`;
  }
};
const sync = (...args: any[]) => {
  const { pid, resourceUsage, ...rest } = (Bun.spawnSync as any)(...args);
  for (const key of ["stdout", "stderr"]) if (key in rest) rest[key] = text(rest[key]);
  return { ...rest, pid: typeof pid, resourceUsage: Object.keys(Object.getOwnPropertyDescriptors(Object.getPrototypeOf(resourceUsage))).filter(k => k !== "constructor") };
};
const cases: [string, () => unknown][] = [
  ["no arguments", () => Bun.spawnSync(undefined as any)],
  ["a number", () => Bun.spawnSync(1 as any)],
  ["no cmd", () => Bun.spawnSync({} as any)],
  ["empty cmd", () => Bun.spawnSync([])],
  ["not in PATH", () => Bun.spawnSync(["nonexistent-command-of-bun-boa"])],
  ["no such file", () => Bun.spawnSync(["/nonexistent/bun-boa"])],
  ["echo", () => sync(["echo", "hi"])],
  ["inherited stdout", () => sync({ cmd: ["true"], stdout: "inherit" })],
  ["exit code and stderr", () => sync(["sh", "-c", "echo out; echo err >&2; exit 3"])],
  ["killed by a signal", () => sync(["sh", "-c", "kill -9 $$"])],
  ["stdin bytes", () => sync(["cat"], { stdin: new TextEncoder().encode("in") })],
  ["stdin pipe", () => sync(["cat"], { stdin: "pipe" })],
  ["stdin Blob", () => sync(["cat"], { stdin: new Blob(["blob"]) })],
  ["stdin DataView", () => sync(["cat"], { stdin: new DataView(new Uint8Array([65, 66]).buffer) })],
  ["stdin Response", () => sync(["cat"], { stdin: new Response("response") })],
  ["timeout", () => sync(["sleep", "5"], { timeout: 100 })],
  ["maxBuffer", () => { const { stdout, ...rest } = sync(["sh", "-c", "yes | head -c 100000"], { maxBuffer: 10 }); return rest; }],
  ["killSignal SIGFOO", () => Bun.spawnSync(["true"], { killSignal: "SIGFOO" })],
  ["killSignal 0", () => Bun.spawnSync(["true"], { killSignal: 0 })],
  ["killSignal 9.5", () => Bun.spawnSync(["true"], { killSignal: 9.5 })],
  ["killSignal 40", () => Bun.spawnSync(["true"], { killSignal: 40 })],
  ["timeout -1", () => Bun.spawnSync(["true"], { timeout: -1 })],
  ["timeout NaN", () => Bun.spawnSync(["true"], { timeout: NaN })],
  ["timeout 1.5", () => Bun.spawnSync(["true"], { timeout: 1.5 })],
  ["timeout string", () => Bun.spawnSync(["true"], { timeout: "5" as any })],
  ["uid string", () => Bun.spawnSync(["true"], { uid: "x" as any })],
  ["uid 2**40", () => Bun.spawnSync(["true"], { uid: 2 ** 40 })],
  ["stdout bogus", () => Bun.spawnSync(["true"], { stdout: "bogus" as any })],
  ["stdout Blob", () => Bun.spawnSync(["true"], { stdout: new Blob(["x"]) as any })],
  ["stdout 0", () => Bun.spawnSync(["true"], { stdout: 0 })],
  ["stdin 1", () => Bun.spawnSync(["true"], { stdin: 1 })],
  ["stdin -1", () => Bun.spawnSync(["true"], { stdin: -1 })],
  ["stdin object", () => Bun.spawnSync(["true"], { stdin: {} as any })],
  ["stdio string", () => Bun.spawnSync(["true"], { stdio: "x" as any })],
  ["env number", () => Bun.spawnSync(["true"], { env: 1 as any })],
  ["env", () => sync(["sh", "-c", "echo $FOO"], { env: { FOO: "bar" } })],
  ["env without PATH", () => sync(["true"], { env: {} })],
  ["process.env", () => { process.env.BUN_BOA_SPAWN = "seen"; return sync(["sh", "-c", "echo $BUN_BOA_SPAWN"]); }],
  ["cwd", () => sync(["pwd"], { cwd: "/" })],
  ["missing cwd", () => Bun.spawnSync(["pwd"], { cwd: "/nonexistent-bun-boa" })],
  ["NUL in a command", () => Bun.spawnSync(["ech\0o"])],
  ["NUL in an argument", () => Bun.spawnSync(["echo", "a\0b"])],
  ["NUL in argv0", () => Bun.spawnSync({ cmd: ["true"], argv0: "a\0b" })],
  ["NUL in env", () => Bun.spawnSync(["true"], { env: { A: "1\0" } })],
  ["argv0", () => sync(["sh", "-c", "echo $0"], { argv0: "custom" })],
  ["arguments are strings", () => sync(["echo", 1, {}, null] as any)],
  ["onExit not a function", () => Bun.spawnSync({ cmd: ["true"], onExit: 1 as any })],
  ["terminal", () => Bun.spawnSync(["true"], { terminal: {} } as any)],
  ["ignored outputs", () => sync(["true"], { stdin: "inherit", stdout: "ignore", stderr: null })],
  ["keys", () => Object.keys(Bun.spawnSync(["true"]))],
  ["Buffer", () => Bun.spawnSync(["true"]).stdout instanceof Uint8Array],
  ["cpuTime", () => Object.values(Bun.spawnSync(["true"]).resourceUsage.cpuTime).map(v => typeof v)],
  ["which sh", () => Bun.which("sh") === "/usr/bin/sh" || Bun.which("sh") === "/bin/sh"],
  ["which nothing", () => Bun.which("nonexistent-command-of-bun-boa")],
  ["which with PATH", () => Bun.which("sh", { PATH: "/nonexistent" })],
  ["which()", () => Bun.which(undefined as any)],
  ["which.length", () => Bun.which.length],
  ["kill()", () => process.kill(undefined as any)],
  ["kill('x')", () => process.kill("x" as any)],
  ["kill(1.5)", () => process.kill(1.5, 0)],
  ["kill of no process", () => process.kill(2 ** 30, 0)],
  ["kill SIGFOO", () => process.kill(process.pid, "SIGFOO")],
  ["kill {}", () => process.kill(process.pid, {} as any)],
  ["kill 0", () => process.kill(process.pid, 0)],
];
for (const [name, f] of cases) console.log(`${name}: ${r(f)}`);

const p = Bun.spawn(["sh", "-c", "echo out; echo err >&2; exit 3"], { stderr: "pipe" });
console.log("spawn:", p.killed, p.exitCode, p.signalCode, typeof p.pid, p.stdin, show(p.stdio));
console.log("exited:", await p.exited, p.exitCode, p.killed);
console.log("output:", show(await p.stdout.text()), show(await p.stderr.text()));
const q = Bun.spawn(["sleep", "5"]);
q.kill();
console.log("kill:", q.killed, await q.exited, q.exitCode, q.signalCode, q.killed);
const k = Bun.spawn(["sleep", "5"]);
k.kill(9);
console.log("kill 9:", await k.exited, k.signalCode);
const s = Bun.spawn(["cat"], { stdin: "pipe" });
console.log("stdin:", s.stdin.write("hello"), s.stdin.end());
console.log("cat:", await s.stdout.text());
let args: any[] = [];
const t = Bun.spawn(["sh", "-c", "exit 4"], { onExit: (...a) => (args = a) });
await t.exited;
console.log("onExit before exited:", args.length);
await new Promise(resolve => setTimeout(resolve, 50));
console.log("onExit:", args.length, args[0] === t, args[1], args[2], args[3]);
console.log("resourceUsage:", typeof t.resourceUsage());
const u = Bun.spawn(["true"], { stdout: "inherit" });
console.log("inherit:", u.stdout, await u.exited);
const w = Bun.spawn({ cmd: ["true"], stdout: "ignore" });
console.log("ignore:", w.stdout, w.stdin, w.stderr, await w.exited);
const y = Bun.spawn(["cat"], { stdin: new Blob(["blobin"]) });
console.log("Blob stdin:", y.stdin, await y.stdout.text());
const j = Bun.spawn(["echo", '{"a":1}']);
console.log("json:", show(await j.stdout.json()));
const fd = Bun.spawn(["true"], { stdout: 2 });
console.log("fd:", fd.stdout, await fd.exited);
const timed = Bun.spawn(["sleep", "5"], { timeout: 100 });
console.log("timeout:", await timed.exited, timed.signalCode);
console.log("toStringTag:", Object.prototype.toString.call(p));
const streamed = Bun.spawn(["sh", "-c", "printf a; sleep 0.05; printf b"]);
console.log("stream:", streamed.stdout instanceof ReadableStream, streamed.stdout === streamed.stdout);
const reader = streamed.stdout.getReader();
let read = "";
const kinds = new Set<string>();
for (let chunk = await reader.read(); !chunk.done; chunk = await reader.read()) {
  kinds.add(chunk.value.constructor.name);
  read += new TextDecoder().decode(chunk.value);
}
console.log("getReader:", read, [...kinds].join());
let iterated = "";
for await (const chunk of Bun.spawn(["printf", "iterated"]).stdout) iterated += new TextDecoder().decode(chunk);
console.log("for await:", iterated);
console.log("Response:", await new Response(Bun.spawn(["printf", "response"]).stdout).text());
console.log("bytes:", show(await Bun.spawn(["printf", "xy"]).stdout.bytes()));
const big = Bun.spawn(["head", "-c", "300000", "/dev/zero"]);
console.log("large:", (await big.stdout.bytes()).length, await big.exited);
