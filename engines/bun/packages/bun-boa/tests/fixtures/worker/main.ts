// Workers on their own threads: messages both ways with structured clone, a transferred ArrayBuffer and MessagePort,
// terminate() of a busy worker, process.exit in a worker, an uncaught error, and MessageChannel/BroadcastChannel
// across threads. Each line is checked by tests/e2e.rs.
const log = (...parts: unknown[]) => console.log(parts.map(p => (typeof p === "string" ? p : JSON.stringify(p))).join(" "));

const local = new MessageChannel();
local.port2.onmessage = e => log("local", e.data);
local.port1.postMessage({ a: [1, 2], m: new Map([["k", 1]]).size });
await new Promise(r => setTimeout(r, 10));
local.port1.close();

log("main", Bun.isMainThread);
const echo = new Worker(new URL("echo.ts", import.meta.url).href);
const buffer = new Uint8Array([1, 2, 3]).buffer;
const channel = new MessageChannel();
const reply = new Promise<unknown>(resolve => (channel.port1.onmessage = e => resolve(e.data)));
const first = new Promise<MessageEvent>(resolve => (echo.onmessage = resolve));
echo.postMessage({ buffer, port: channel.port2 }, [buffer, channel.port2]);
log("detached", buffer.byteLength);
const event = await first;
log("echo", event.data);
log("port", await reply);
channel.port1.close();
const closed = new Promise<number>(resolve => echo.addEventListener("close", (e: any) => resolve(e.code)));
echo.terminate();
log("terminated", await closed);

const busy = new Worker("data:text/javascript,postMessage('spinning'); while (true) {}");
await new Promise(resolve => (busy.onmessage = resolve));
const busyClosed = new Promise<number>(resolve => (busy.onclose = (e: any) => resolve(e.code)));
busy.terminate();
log("busy", await busyClosed);

const exits = new Worker("data:text/javascript,setTimeout(() => process.exit(7), 1); setInterval(() => {}, 1000)");
log("exit", await new Promise(resolve => exits.addEventListener("close", (e: any) => resolve(e.code))));

const throws = new Worker("data:text/javascript,throw new TypeError('worker failed')");
log("error", await new Promise(resolve => (throws.onerror = (e: any) => resolve([e.message, e.error?.name]))));

const a = new BroadcastChannel("boa");
const b = new Worker("data:text/javascript,const c = new BroadcastChannel('boa'); c.onmessage = e => { c.postMessage(e.data + '!'); c.close() }; postMessage('ready')");
await new Promise(resolve => (b.onmessage = resolve));
const heard = new Promise(resolve => (a.onmessage = e => resolve(e.data)));
a.postMessage("hi");
log("broadcast", await heard);
a.close();
b.terminate();

// node:worker_threads: workerData, parentPort, isMainThread and threadId on both sides, and the exit code.
const { Worker: NodeWorker, isMainThread, threadId } = await import("node:worker_threads");
log("node main", isMainThread, threadId);
const source = `const wt = require("worker_threads");
wt.parentPort.once("message", m => wt.parentPort.postMessage([m, wt.workerData, wt.isMainThread, wt.threadId > 0]));`;
const node = new NodeWorker(new URL("data:text/javascript," + encodeURIComponent(source)), { workerData: { n: 1 } });
node.postMessage("ping");
log("node", await new Promise(resolve => node.once("message", resolve)));
log("node exit", await new Promise(resolve => node.once("exit", resolve)));
