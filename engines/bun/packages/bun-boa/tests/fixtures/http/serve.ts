const order: string[] = [];
setTimeout(() => order.push("timeout"), 0);
setImmediate(() => order.push("immediate"));
queueMicrotask(() => order.push("microtask"));
const interval = setInterval(() => {
  order.push("interval");
  clearInterval(interval);
}, 1);
await new Promise(resolve => setTimeout(resolve, 10));
console.log(order.join(" "));

using server = Bun.serve({
  port: 0,
  routes: {
    "/static": new Response("static body", { headers: { "x-route": "static" } }),
    "/users/:id": req => Response.json({ id: req.params.id }),
  },
  async fetch(req, server) {
    if (req.method === "POST") return new Response(`echo ${await req.text()}`, { status: 201 });
    if (new URL(req.url).pathname === "/throw") throw new Error("handler failed");
    return new Response(`hello ${req.headers.get("x-name")} from ${server.port === Number(new URL(req.url).port)}`);
  },
  error(error) {
    return new Response(`caught ${error.message}`, { status: 500 });
  },
});

// `port: 0` is an ephemeral port each time, so a second server starts too.
using second = Bun.serve({ port: 0, fetch: () => new Response("second") });
console.log("ephemeral", second.port > 0 && second.port !== server.port, await (await fetch(second.url)).text());

const get = await fetch(new URL("/", server.url), { headers: { "X-Name": "boa" } });
console.log(get.status, get.headers.get("content-type"), await get.text());
const post = await fetch(server.url, { method: "POST", body: "payload" });
console.log(post.status, post.statusText, await post.text());
const route = await fetch(`${server.url}users/42`);
console.log(route.headers.get("content-type"), JSON.stringify(await route.json()));
const fixed = await fetch(`${server.url}static`);
console.log(fixed.headers.get("x-route"), await fixed.text());
const thrown = await fetch(`${server.url}throw`);
console.log(thrown.status, await thrown.text());

const headers = new Headers([["B", "2"], ["a", "1"]]);
headers.append("a", "3");
console.log(JSON.stringify([...headers]), headers.get("A"));
const request = new Request("http://example.test/path", { method: "post", body: "x" });
console.log(request.method, request.url, request.headers.get("content-type"));
try {
  await fetch(`http://127.0.0.1:${server.port}/`.replace(String(server.port), "1"));
} catch (error) {
  console.log((error as any).code);
}
