// Installs `Bun` and `process` on top of the native operations that runtime.rs puts on `globalThis[OPS]`.
// It is a call, not a bare function expression: the parser drops expression statements without side effects.
const OPS = "__bun_native_ops__";
(function install(native: {
  printStdout(text: string): void;
  printStderr(text: string): void;
  exit(code: number): never;
  cwd(): string;
  nanoseconds(): number;
  semver: { satisfies(version: unknown, range: unknown): boolean; order(left: unknown, right: unknown): -1 | 0 | 1 };
  // bun_runtime_js::webcore::blob::exports
  Blob: typeof globalThis.Blob;
  File: typeof globalThis.File;
  FormData: typeof globalThis.FormData;
  file(path: unknown, options?: unknown): Blob;
  write(destination: unknown, data: unknown, options?: unknown): Promise<number>;
  stdin: Blob;
  stdout: Blob;
  stderr: Blob;
  createObjectURL(blob: unknown): string;
  revokeObjectURL(url: unknown): void;
  resolveObjectURL(url: unknown): Blob | undefined;
  serve(options: unknown): unknown;
  gc(): void;
  // bun_runtime_js::bun_object
  Glob: new (pattern: string) => unknown;
  deepEquals(a: unknown, b: unknown, strict?: boolean, skipPrototype?: boolean): boolean;
  sleepSync(milliseconds: number): void;
  which(command: string, options?: { PATH?: string; cwd?: string }): string | null;
  unsafe: Record<string, (...args: any[]) => unknown>;
  // bun_runtime_js::node::process
  process: {
    umask(mask?: unknown): number;
    hrtime(time?: unknown): [number, number];
    hrtimeBigint(): bigint;
    memoryUsage(): Record<string, number>;
    memoryUsageRss(): number;
    cpuUsage(previous?: unknown): { user: number; system: number };
    resourceUsage(): Record<string, number>;
    getReport(): object;
    execPath: string;
  };
  readableStreamToText(stream: unknown): Promise<string>;
  readableStreamToBytes(stream: unknown): Promise<Uint8Array>;
  readableStreamToArrayBuffer(stream: unknown): Promise<ArrayBuffer>;
  readableStreamToJSON(stream: unknown): Promise<unknown>;
  readableStreamToBlob(stream: unknown): Promise<Blob>;
  readableStreamToArray(stream: unknown): Promise<unknown[]>;
  spawn(cmdOrOptions: unknown, options?: unknown): unknown;
  spawnSync(cmdOrOptions: unknown, options?: unknown): unknown;
  which(command: unknown, options?: unknown): string | null;
  // bun_runtime_js::process_object
  process: { kill(pid: unknown, signal?: unknown): boolean; _kill(pid: number, signal: number): number };
  env: Record<string, string>;
  argv: string[];
  main: string;
  version: string;
  revision: string;
  platform: string;
  arch: string;
  pid: number;
  cryptoBun: Record<string, unknown>;
  isMainThread: boolean;
}) {
  const Bun = {
    isMainThread: native.isMainThread,
    version: native.version,
    revision: native.revision,
    env: native.env,
    argv: native.argv,
    main: native.main,
    file: native.file,
    write: native.write,
    stdin: native.stdin,
    stdout: native.stdout,
    stderr: native.stderr,
    sleep: (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms)),
    nanoseconds: native.nanoseconds,
    cwd: native.cwd,
    semver: native.semver,
    Glob: native.Glob,
    deepEquals: native.deepEquals,
    sleepSync: native.sleepSync,
    which: native.which,
    unsafe: native.unsafe,
    readableStreamToText: native.readableStreamToText,
    readableStreamToBytes: native.readableStreamToBytes,
    readableStreamToArrayBuffer: native.readableStreamToArrayBuffer,
    readableStreamToJSON: native.readableStreamToJSON,
    readableStreamToBlob: native.readableStreamToBlob,
    readableStreamToArray: native.readableStreamToArray,
    spawn: native.spawn,
    spawnSync: native.spawnSync,
    which: native.which,
    fileURLToPath(url: unknown): string {
      const parsed = url instanceof URL ? url : new URL(String(url));
      if (parsed.protocol !== "file:") throw new TypeError("The URL must be of scheme file");
      return decodeURIComponent(parsed.pathname);
    },
    pathToFileURL(path: unknown): URL {
      return new URL(`file://${encodeURI(String(path)).replace(/[?#]/g, encodeURIComponent)}`);
    },
    serve: native.serve,
    fetch: globalThis.fetch,
    gc: (_force?: boolean) => native.gc(),
    // CryptoHasher, SHA1…, sha, hash, randomUUIDv7, randomUUIDv5 (js/crypto.ts).
    ...native.cryptoBun,
  };

  const process: any = {
    argv: native.argv,
    env: native.env,
    exitCode: undefined as number | undefined,
    platform: native.platform,
    arch: native.arch,
    pid: native.pid,
    version: "v24.3.0",
    versions: { bun: native.version },
    release: { name: "node" },
    cwd: native.cwd,
    kill: native.process.kill,
    _kill: native.process._kill,
    exit(code?: number): never {
      return native.exit(code ?? process.exitCode ?? 0);
    },
    nextTick(fn: (...args: unknown[]) => void, ...args: unknown[]) {
      queueMicrotask(() => fn(...args));
    },
    execPath: native.process.execPath,
    umask: native.process.umask,
    hrtime: Object.assign(native.process.hrtime, { bigint: native.process.hrtimeBigint }),
    memoryUsage: Object.assign(native.process.memoryUsage, { rss: native.process.memoryUsageRss }),
    cpuUsage: native.process.cpuUsage,
    resourceUsage: native.process.resourceUsage,
    // BunProcess.cpp's `constructProcessReportObject`.
    report: {
      compact: false,
      directory: "",
      filename: "",
      getReport: native.process.getReport,
      reportOnFatalError: false,
      reportOnSignal: false,
      reportOnUncaughtException: false,
      // C++ puts `excludeEnv` twice: the second, the signal's name, is what it ends up with.
      excludeEnv: "SIGUSR2",
      // As C++'s `Process_functionWriteReport`, which writes nothing yet.
      writeReport: (fileName?: unknown) => fileName,
    },
    stdout: { write: (s: unknown) => (native.printStdout(String(s)), true) },
    stderr: { write: (s: unknown) => (native.printStderr(String(s)), true) },
  };

  Object.defineProperties(globalThis, {
    Bun: { value: Bun, writable: true, configurable: true },
    process: { value: process, writable: true, configurable: true },
    global: { value: globalThis, writable: true, configurable: true },
    Blob: { value: native.Blob, writable: true, configurable: true },
    File: { value: native.File, writable: true, configurable: true },
    FormData: { value: native.FormData, writable: true, configurable: true },
  });
  if (typeof URL === "function")
    Object.defineProperties(URL, {
      createObjectURL: { value: native.createObjectURL, writable: true, enumerable: true, configurable: true },
      revokeObjectURL: { value: native.revokeObjectURL, writable: true, enumerable: true, configurable: true },
    });
  // Boa's runtime has timers but no immediates: one is a timer that is due at once.
  if (typeof (globalThis as any).setImmediate !== "function")
    Object.defineProperties(globalThis, {
      setImmediate: {
        value: function setImmediate(fn: (...args: unknown[]) => void, ...args: unknown[]) {
          return setTimeout(() => fn(...args), 0);
        },
        writable: true,
        configurable: true,
      },
      clearImmediate: {
        value: function clearImmediate(id: ReturnType<typeof setTimeout>) {
          clearTimeout(id);
        },
        writable: true,
        configurable: true,
      },
    });
})((globalThis as any)[OPS]);
delete (globalThis as any)[OPS];
