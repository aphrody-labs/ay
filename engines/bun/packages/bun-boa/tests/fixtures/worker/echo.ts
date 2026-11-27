// What the parent posts comes back: the bytes of its buffer, and a word over the port it sent.
self.onmessage = (event: MessageEvent) => {
  const { buffer, port } = event.data;
  port.postMessage(`pong from ${Bun.isMainThread ? "main" : "worker"}`);
  postMessage([...new Uint8Array(buffer)]);
};
