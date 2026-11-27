// `MessagePort`, `MessageChannel`, `BroadcastChannel`, `Worker` and a worker's global scope on Boa (WebCore's
// MessagePort.cpp, MessageChannel.cpp, BroadcastChannel.cpp, Worker.cpp and WorkerGlobalScope in
// src/jsc/bindings/webcore), over the natives of bun_runtime_js::web::{message_port, worker} that runtime.rs puts on
// globalThis.__bun_worker_ops__, and the binding of node:worker_threads
// (`$cpp("Worker.cpp", "createNodeWorkerThreadsBinding")`). A script: it runs after js/events.ts and js/bun.ts.
(function install(ops: any) {
  const G = globalThis as any;
  const { defineEventHandler, LISTENERS_CHANGED } = G[Symbol.for("bun.boa.events")];
  const HANDLE: symbol = ops.handleKey;
  const ILLEGAL = Symbol("illegal constructor");
  const illegal = () => new TypeError("Illegal constructor");
  const toStringTag = (target: object, tag: string) =>
    Object.defineProperty(target, Symbol.toStringTag, { value: tag, configurable: true });
  const domException = (message: string, name: string) => new G.DOMException(message, name);

  const isObject = (value: unknown) => (typeof value === "object" && value !== null) || typeof value === "function";
  /** A `postMessage`'s second argument, as WebIDL resolves its overloads: `sequence<object>` (an iterable object)
   * or `StructuredSerializeOptions` (`{ transfer }`). Anything else, or an entry that is not an object, throws a
   * TypeError before anything is serialized. */
  function transferList(options: unknown): unknown[] {
    if (options === undefined || options === null) return [];
    if (!isObject(options)) throw new TypeError("The \"options\" argument must be an object or a sequence");
    const iterable = typeof (options as any)[Symbol.iterator] === "function";
    const list = iterable ? options : (options as any).transfer;
    if (list === undefined) return [];
    if (!isObject(list) || typeof list[Symbol.iterator] !== "function")
      throw new TypeError("Value of \"transfer\" is not iterable");
    // Entry by entry, as WebIDL converts a sequence: a hole in a sparse array throws there, before the rest.
    const items: unknown[] = [];
    for (const item of list as Iterable<unknown>) {
      if (!isObject(item)) throw new TypeError("Value of \"transfer\" must contain objects");
      items.push(item);
    }
    return items;
  }

  // ───────────────────────── MessagePort ─────────────────────────
  class MessagePort extends EventTarget {
    constructor(...args: unknown[]) {
      if (args[0] !== ILLEGAL) throw illegal();
      super();
      Object.defineProperty(this, HANDLE, { value: args[1] });
      ops.bindPort(args[1], this);
    }
    postMessage(message: unknown, options?: unknown) {
      if (arguments.length === 0) throw new TypeError("postMessage requires at least 1 argument");
      ops.post(handleOf(this), message, transferList(options));
    }
    start() {
      ops.start(handleOf(this));
    }
    close() {
      ops.close(handleOf(this));
    }
    ref() {
      ops.ref(handleOf(this), true);
      return this;
    }
    unref() {
      ops.ref(handleOf(this), false);
      return this;
    }
    hasRef() {
      return ops.hasRef(handleOf(this));
    }
    // WebCore's MessagePort::addEventListener: a 'message' listener starts the port; node's: it refs the loop
    // while there is one.
    [LISTENERS_CHANGED](type: string, count: number) {
      if (type !== "message") return;
      if (count > 0) {
        ops.start(handleOf(this));
        ops.ref(handleOf(this), true);
      } else ops.ref(handleOf(this), false);
    }
  }
  function handleOf(port: any) {
    const handle = port?.[HANDLE];
    if (handle === undefined) throw new TypeError("Illegal invocation");
    return handle;
  }
  defineEventHandler(MessagePort.prototype, "message");
  defineEventHandler(MessagePort.prototype, "messageerror");
  defineEventHandler(MessagePort.prototype, "close");
  toStringTag(MessagePort.prototype, "MessagePort");

  // ───────────────────────── MessageChannel ─────────────────────────
  class MessageChannel {
    #port1: MessagePort;
    #port2: MessagePort;
    constructor() {
      ops.markUncloneable(this);
      const [a, b] = ops.channel();
      this.#port1 = new MessagePort(ILLEGAL, a);
      this.#port2 = new MessagePort(ILLEGAL, b);
    }
    get port1() {
      return this.#port1;
    }
    get port2() {
      return this.#port2;
    }
  }
  toStringTag(MessageChannel.prototype, "MessageChannel");

  // ───────────────────────── BroadcastChannel ─────────────────────────
  class BroadcastChannel extends EventTarget {
    #name: string;
    #closed = false;
    constructor(name: unknown) {
      if (arguments.length === 0) throw new TypeError("BroadcastChannel requires 1 argument");
      super();
      ops.markUncloneable(this);
      this.#name = String(name);
      const handle = ops.broadcast(this.#name);
      Object.defineProperty(this, HANDLE, { value: handle });
      ops.bindPort(handle, this);
      ops.start(handle);
      ops.ref(handle, false);
    }
    get name() {
      return this.#name;
    }
    postMessage(message: unknown) {
      if (arguments.length === 0) throw new TypeError("postMessage requires 1 argument");
      if (this.#closed) throw domException("BroadcastChannel is closed.", "InvalidStateError");
      ops.post(handleOf(this), message, []);
    }
    close() {
      this.#closed = true;
      ops.close(handleOf(this));
    }
    ref() {
      ops.ref(handleOf(this), true);
      return this;
    }
    unref() {
      ops.ref(handleOf(this), false);
      return this;
    }
    [LISTENERS_CHANGED](type: string, count: number) {
      if (type === "message") ops.ref(handleOf(this), count > 0);
    }
  }
  defineEventHandler(BroadcastChannel.prototype, "message");
  defineEventHandler(BroadcastChannel.prototype, "messageerror");
  toStringTag(BroadcastChannel.prototype, "BroadcastChannel");

  // ───────────────────────── Worker ─────────────────────────
  const workerPorts = new WeakMap<Worker, MessagePort>();
  class Worker extends EventTarget {
    #handle: unknown;
    #threadId: number;
    #exited = false;
    constructor(specifier: unknown, options?: any, nodeWorker?: unknown) {
      if (arguments.length === 0) throw new TypeError("Worker requires at least 1 argument");
      super();
      ops.markUncloneable(this);
      options ??= {};
      const href = specifier instanceof URL ? specifier.href : String(specifier);
      const preload =
        options.preload === undefined ? [] : Array.isArray(options.preload) ? options.preload : [options.preload];
      const env = options.env === undefined || options.env === null ? undefined : options.env;
      const [handle, portHandle] = ops.spawn(this, {
        specifier: href,
        name: options.name,
        env,
        argv: Array.isArray(options.argv) ? options.argv.map(String) : [],
        preload: preload.map((p: unknown) => (p instanceof URL ? p.href : String(p))),
        workerData: options.workerData,
        transferList: options.transferList,
        node: nodeWorker !== undefined,
      });
      this.#handle = handle;
      this.#threadId = ops.workerThreadId(handle);
      const port = new MessagePort(ILLEGAL, portHandle);
      workerPorts.set(this, port);
      port.addEventListener("message", (event: any) =>
        this.dispatchEvent(new MessageEvent("message", { data: event.data, ports: event.ports })),
      );
      port.addEventListener("messageerror", (event: any) =>
        this.dispatchEvent(new MessageEvent("messageerror", { data: event.data })),
      );
      // The worker's handle keeps the loop alive, not its port.
      port.unref();
      if (options.ref === false) ops.workerRef(handle, false);
    }
    get threadId() {
      return this.#threadId;
    }
    postMessage(message: unknown, options?: unknown) {
      if (arguments.length === 0) throw new TypeError("postMessage requires at least 1 argument");
      workerPorts.get(this)!.postMessage(message, options);
    }
    terminate() {
      if (!this.#exited) ops.terminate(this.#handle);
    }
    ref() {
      ops.workerRef(this.#handle, true);
    }
    unref() {
      ops.workerRef(this.#handle, false);
    }
    getHeapSnapshot() {
      return Promise.reject(new Error("getHeapSnapshot is not implemented on Boa yet"));
    }
    /** What the worker's thread reported: 0 it runs, 1 it threw, 2 it exited (with its code). */
    static event(worker: Worker, kind: number, payload: any) {
      if (kind === 0) {
        worker.dispatchEvent(new Event("open"));
      } else if (kind === 1) {
        const error = new Error(payload.message);
        if (payload.name) Object.defineProperty(error, "name", { value: payload.name, writable: true, configurable: true });
        if (payload.stack) Object.defineProperty(error, "stack", { value: payload.stack, writable: true, configurable: true });
        worker.dispatchEvent(new ErrorEvent("error", { message: payload.message, error }));
      } else {
        // What the worker posted before it exited comes first.
        const port = workerPorts.get(worker)!;
        let entry;
        while ((entry = ops.receive(handleOf(port))) !== undefined)
          worker.dispatchEvent(new MessageEvent("message", { data: entry.message }));
        port.close();
        worker.#exited = true;
        worker.dispatchEvent(new CloseEvent("close", { code: payload }));
      }
    }
  }
  const workerEvent = Worker.event;
  delete (Worker as any).event;
  for (const type of ["message", "messageerror", "error", "open", "close"])
    defineEventHandler(Worker.prototype, type);
  toStringTag(Worker.prototype, "Worker");

  ops.install({
    dispatch(port: EventTarget, kind: number, data: unknown, ports: unknown[]) {
      if (kind === 0) port.dispatchEvent(new MessageEvent("message", { data, ports }));
      else if (kind === 1) port.dispatchEvent(new MessageEvent("messageerror"));
      else port.dispatchEvent(new Event("close"));
    },
    makePort: (handle: unknown) => new MessagePort(ILLEGAL, handle),
    domException,
    workerEvent,
  });

  for (const [name, value] of Object.entries({ MessagePort, MessageChannel, BroadcastChannel, Worker }))
    Object.defineProperty(G, name, { value, writable: true, enumerable: false, configurable: true });

  // ───────────────────────── a worker's global scope ─────────────────────────
  const info = ops.threadInfo();
  const ownPort: MessagePort | null = info.isMainThread ? null : info.port;
  function postMessage(message: unknown, options?: unknown) {
    if (ownPort === null) throw new TypeError("postMessage is only available in a Worker");
    if (arguments.length === 0) throw new TypeError("postMessage requires at least 1 argument");
    ownPort.postMessage(message, options);
  }
  if (ownPort !== null) {
    // The worker's own port: what the parent posts goes to the global scope's listeners, after the entry module.
    const scope = new EventTarget();
    for (const name of ["addEventListener", "removeEventListener", "dispatchEvent"])
      Object.defineProperty(G, name, { value: (scope as any)[name].bind(scope), writable: true, configurable: true });
    (scope as any)[LISTENERS_CHANGED] = (type: string, count: number) => {
      if (type === "message") count > 0 ? ownPort.ref() : ownPort.unref();
    };
    ownPort.addEventListener("message", (event: any) =>
      scope.dispatchEvent(new MessageEvent("message", { data: event.data, ports: event.ports })),
    );
    ownPort.addEventListener("messageerror", (event: any) => scope.dispatchEvent(new MessageEvent("messageerror", { data: event.data })));
    ownPort.unref();
    for (const type of ["message", "messageerror", "error"]) defineEventHandler(G, type);
    Object.defineProperties(G, {
      self: { value: G, writable: true, configurable: true },
      name: { value: info.name, writable: true, configurable: true },
      postMessage: { value: postMessage, writable: true, configurable: true },
      close: { value: () => G.process.exit(0), writable: true, configurable: true },
    });
  } else if (!("self" in G)) Object.defineProperty(G, "self", { value: G, writable: true, configurable: true });

  // ───────────────────────── node:console's Console ─────────────────────────
  // What node:worker_threads makes a node worker's console of (`new Console(process.stdout, process.stderr)`), as
  // node's lib/internal/console/constructor.js: the methods write what they format to the streams it is given.
  if (typeof G.console === "object" && G.console !== null && typeof G.console.Console !== "function") {
    const format = (args: unknown[]) => {
      const inspect = (v: unknown) => (typeof G.Bun?.inspect === "function" ? G.Bun.inspect(v) : String(v));
      if (typeof args[0] !== "string") return args.map(inspect).join(" ");
      let rest = args.slice(1);
      const head = (args[0] as string).replace(/%[sdifjoOc%]/g, spec => {
        if (spec === "%%") return "%";
        if (rest.length === 0) return spec;
        const value = rest.shift();
        switch (spec) {
          case "%s":
            return typeof value === "string" ? value : inspect(value);
          case "%d":
          case "%i":
            return String(spec === "%i" ? parseInt(String(value)) : Number(value));
          case "%f":
            return String(parseFloat(String(value)));
          case "%j":
            return JSON.stringify(value);
          case "%c":
            return "";
          default:
            return inspect(value);
        }
      });
      return [head, ...rest.map(v => (typeof v === "string" ? v : inspect(v)))].join(" ");
    };
    class Console {
      #out: any;
      #err: any;
      #indent = "";
      #counts = new Map<string, number>();
      #timers = new Map<string, number>();
      constructor(stdout: any, stderr?: any) {
        if (stdout && typeof stdout.write !== "function" && typeof stdout === "object") ({ stdout, stderr } = stdout);
        if (!stdout || typeof stdout.write !== "function") throw new TypeError("The \"stdout\" argument must be a writable stream");
        this.#out = stdout;
        this.#err = stderr && typeof stderr.write === "function" ? stderr : stdout;
        for (const name of Object.getOwnPropertyNames(Console.prototype))
          if (name !== "constructor") (this as any)[name] = (this as any)[name].bind(this);
      }
      #write(stream: any, args: unknown[]) {
        const text = format(args);
        stream.write(this.#indent + text.replaceAll("\n", "\n" + this.#indent) + "\n");
      }
      log(...args: unknown[]) {
        this.#write(this.#out, args);
      }
      info(...args: unknown[]) {
        this.#write(this.#out, args);
      }
      debug(...args: unknown[]) {
        this.#write(this.#out, args);
      }
      warn(...args: unknown[]) {
        this.#write(this.#err, args);
      }
      error(...args: unknown[]) {
        this.#write(this.#err, args);
      }
      trace(...args: unknown[]) {
        this.#write(this.#err, ["Trace" + (args.length ? ": " + format(args) : ""), new Error().stack?.split("\n").slice(2).join("\n") ?? ""]);
      }
      dir(value: unknown) {
        this.#write(this.#out, [value]);
      }
      assert(condition: unknown, ...args: unknown[]) {
        if (!condition) this.#write(this.#err, ["Assertion failed" + (args.length ? ": " + format(args) : "")]);
      }
      count(label = "default") {
        const n = (this.#counts.get(label) ?? 0) + 1;
        this.#counts.set(label, n);
        this.#write(this.#out, [`${label}: ${n}`]);
      }
      countReset(label = "default") {
        this.#counts.delete(label);
      }
      group(...args: unknown[]) {
        if (args.length) this.#write(this.#out, args);
        this.#indent += "  ";
      }
      groupEnd() {
        this.#indent = this.#indent.slice(2);
      }
      time(label = "default") {
        this.#timers.set(label, performance.now());
      }
      timeLog(label = "default", ...args: unknown[]) {
        const start = this.#timers.get(label);
        if (start !== undefined) this.#write(this.#out, [`${label}: ${(performance.now() - start).toFixed(3)}ms`, ...args]);
      }
      timeEnd(label = "default") {
        this.timeLog(label);
        this.#timers.delete(label);
      }
      table(data: unknown) {
        this.#write(this.#out, [data]);
      }
    }
    for (const alias of ["groupCollapsed"]) (Console.prototype as any)[alias] = Console.prototype.group;
    Object.defineProperty(G.console, "Console", { value: Console, writable: true, configurable: true });
  }

  // ───────────────────────── node:worker_threads ─────────────────────────
  const untransferable = new WeakSet<object>();
  let entryEvaluated: (() => void) | undefined;
  const binding = [
    info.isMainThread ? null : info.workerData,
    info.threadId,
    (port: MessagePort) => ops.receive(handleOf(port)),
    new Map(),
    info.isMainThread ? "" : info.name,
    (port: MessagePort) => ops.isActive(handleOf(port)),
    (value: unknown) => {
      if (typeof value === "object" && value !== null) untransferable.add(value);
    },
    (value: unknown) => typeof value === "object" && value !== null && untransferable.has(value),
    (value: unknown) => {
      if (typeof value === "object" && value !== null) ops.markUncloneable(value);
    },
    (hook: () => void) => {
      entryEvaluated = hook;
    },
    !info.isMainThread && info.node,
    (port: MessagePort) => ops.defer(handleOf(port)),
    (_ports: object) => {},
    MessagePort,
    MessageChannel,
    BroadcastChannel,
    Worker,
  ];
  Object.defineProperty(G, Symbol.for("bun.boa.worker"), {
    value: { binding, postMessage, entryEvaluated: () => entryEvaluated?.() },
    configurable: true,
  });
})((globalThis as any).__bun_worker_ops__);
delete (globalThis as any).__bun_worker_ops__;
