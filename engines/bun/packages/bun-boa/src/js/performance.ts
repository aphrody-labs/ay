// `performance` and its classes on Boa: a port of WebCore's Performance.cpp, PerformanceUserTiming.cpp,
// PerformanceEntry.cpp, PerformanceMark.cpp, PerformanceMeasure.cpp, PerformanceObserver.cpp and
// PerformanceObserverEntryList.cpp (src/jsc/bindings/webcore), over the clock in bun_runtime_js (Performance.bind.ts),
// which runtime.rs puts on globalThis[OPS].performanceClock. A script: it runs before js/bun.ts.
(function install(clock: { now(): number; timeOrigin(): number }) {
  const G = globalThis as any;
  const ILLEGAL = Symbol("illegal constructor");
  const illegal = () => new TypeError("Illegal constructor");
  const defineMethods = (target: object, methods: Record<string, Function>) => {
    for (const [name, value] of Object.entries(methods))
      Object.defineProperty(target, name, { value, writable: true, enumerable: true, configurable: true });
  };
  const toStringTag = (target: object, tag: string) =>
    Object.defineProperty(target, Symbol.toStringTag, { value: tag, configurable: true });
  /** A DOMException where WebIDL throws one (`Exception { SyntaxError, … }`), else an Error of that name. */
  const domException = (message: string, name: string) => {
    if (typeof G.DOMException === "function") return new G.DOMException(message, name);
    const error = new Error(message);
    error.name = name;
    return error;
  };
  /** SerializedScriptValue::create: a deep copy, or the error that cloning throws. */
  const serialize = (value: unknown) => (typeof G.structuredClone === "function" ? G.structuredClone(value) : value);
  const now = () => clock.now();

  // ───────────────────────── entries ─────────────────────────
  type EntryType = "mark" | "measure";
  class PerformanceEntry {
    #name: string;
    #entryType: EntryType;
    #startTime: number;
    #duration: number;
    // `...args`: as WebIDL's, the constructor's length is 0.
    constructor(...args: unknown[]) {
      const [token, name, entryType, startTime, duration] = args as [symbol, string, EntryType, number, number];
      if (token !== ILLEGAL) throw illegal();
      this.#name = name;
      this.#entryType = entryType;
      this.#startTime = startTime;
      this.#duration = duration;
    }
    get name() {
      return this.#name;
    }
    get entryType() {
      return this.#entryType;
    }
    get startTime() {
      return this.#startTime;
    }
    get duration() {
      return this.#duration;
    }
    toJSON() {
      const json: Record<string, unknown> = {
        name: this.name,
        entryType: this.entryType,
        startTime: this.startTime,
        duration: this.duration,
      };
      if (this instanceof PerformanceMark || this instanceof PerformanceMeasure) json.detail = (this as any).detail;
      return json;
    }
  }
  toStringTag(PerformanceEntry.prototype, "PerformanceEntry");

  const details = new WeakMap<object, unknown>();
  /** `PerformanceMark::create`. */
  function createMark(name: string, options: any) {
    let startTime: number;
    let detail: unknown = null;
    if (options !== undefined && options !== null) {
      if (typeof options !== "object") throw new TypeError("The options of mark must be an object");
      if (options.startTime !== undefined) {
        startTime = Number(options.startTime);
        if (!(startTime >= 0)) throw new TypeError(`'${options.startTime}' is not a positive number`);
      } else startTime = now();
      detail = options.detail === undefined ? null : options.detail;
    } else startTime = now();
    return [startTime, detail === null ? null : serialize(detail)] as const;
  }
  class PerformanceMark extends PerformanceEntry {
    constructor(name: unknown, options?: unknown) {
      if (arguments.length < 1) throw new TypeError("Not enough arguments");
      const [startTime, detail] = createMark(String(name), options);
      super(ILLEGAL, String(name), "mark", startTime, 0);
      details.set(this, detail);
    }
    get detail() {
      return details.get(this) ?? null;
    }
  }
  toStringTag(PerformanceMark.prototype, "PerformanceMark");

  let constructingMeasure = false;
  class PerformanceMeasure extends PerformanceEntry {
    constructor(name: string, startTime: number, endTime: number, detail: unknown) {
      if (!constructingMeasure) throw illegal();
      super(ILLEGAL, name, "measure", startTime, endTime - startTime);
      details.set(this, detail);
    }
    get detail() {
      return details.get(this) ?? null;
    }
  }
  toStringTag(PerformanceMeasure.prototype, "PerformanceMeasure");
  const newMeasure = (name: string, startTime: number, endTime: number, detail: unknown) => {
    constructingMeasure = true;
    try {
      return new PerformanceMeasure(name, startTime, endTime, detail);
    } finally {
      constructingMeasure = false;
    }
  };

  // ───────────────────────── PerformanceUserTiming ─────────────────────────
  const marks = new Map<string, PerformanceEntry[]>();
  const measures = new Map<string, PerformanceEntry[]>();
  const add = (map: Map<string, PerformanceEntry[]>, entry: PerformanceEntry) => {
    let list = map.get(entry.name);
    if (!list) map.set(entry.name, (list = []));
    list.push(entry);
  };
  const all = (map: Map<string, PerformanceEntry[]>) => [...map.values()].flat();
  const byStartTime = (entries: PerformanceEntry[]) => entries.sort((a, b) => a.startTime - b.startTime);

  /** `convertMarkToTimestamp`: a mark's name is its last start time, a number is itself. */
  function timestampOf(mark: unknown): number {
    if (typeof mark === "number") {
      if (mark < 0) throw new TypeError(`'${mark}' is a negative timestamp`);
      return mark;
    }
    const name = String(mark);
    const list = marks.get(name);
    if (list) return list[list.length - 1].startTime;
    throw domException(`No mark named '${name}' exists`, "SyntaxError");
  }
  /** WebIDL's `(DOMString or DOMHighResTimeStamp)` of the measure options. */
  const markOf = (value: unknown) => (value === undefined ? undefined : typeof value === "number" ? value : String(value));

  function measureWithOptions(name: string, options: { start?: unknown; end?: unknown; duration?: unknown; detail?: unknown }) {
    const start = markOf(options.start);
    const end = markOf(options.end);
    const duration = options.duration === undefined ? undefined : Number(options.duration);
    let endTime: number;
    if (end !== undefined) endTime = timestampOf(end);
    else if (start !== undefined && duration !== undefined) endTime = timestampOf(start) + timestampOf(duration);
    else endTime = now();
    let startTime: number;
    if (start !== undefined) startTime = timestampOf(start);
    else if (duration !== undefined && end !== undefined) startTime = timestampOf(end) - timestampOf(duration);
    else startTime = 0;
    const detail = options.detail === undefined || options.detail === null ? null : serialize(options.detail);
    const measure = newMeasure(name, startTime, endTime, detail);
    add(measures, measure);
    return measure;
  }

  function measure(name: string, startOrOptions?: unknown, endMark?: unknown) {
    const end = endMark === undefined ? undefined : String(endMark);
    if (startOrOptions !== undefined && startOrOptions !== null && typeof startOrOptions === "object") {
      const options = startOrOptions as any;
      if (options.start !== undefined || options.end !== undefined) {
        if (end !== undefined) throw new TypeError("measure() takes an end mark or options with start or end, not both");
        if (options.start !== undefined && options.duration !== undefined && options.end !== undefined)
          throw new TypeError("measure() takes at most two of start, duration and end");
        return measureWithOptions(name, options);
      }
      if (end !== undefined) return measureWithOptions(name, { ...options, end, duration: undefined });
      return measureWithOptions(name, options);
    }
    const startTime = startOrOptions === undefined || startOrOptions === null ? 0 : timestampOf(String(startOrOptions));
    const endTime = end === undefined ? now() : timestampOf(end);
    const entry = newMeasure(name, startTime, endTime, null);
    add(measures, entry);
    return entry;
  }

  // ───────────────────────── observers ─────────────────────────
  const observers = new Set<PerformanceObserver>();
  function queueEntry(entry: PerformanceEntry) {
    for (const observer of observers) observer[QUEUE](entry);
  }

  // ───────────────────────── Performance ─────────────────────────
  let onresourcetimingbufferfull: unknown = null;
  const Base: any = typeof G.EventTarget === "function" ? G.EventTarget : Object;
  class Performance extends Base {
    constructor(...args: unknown[]) {
      if (args[0] !== ILLEGAL) throw illegal();
      super();
    }
  }
  toStringTag(Performance.prototype, "Performance");

  // PerformanceTiming.cpp: every time is 0, there being no navigation.
  const TIMING = [
    "navigationStart", "unloadEventStart", "unloadEventEnd", "redirectStart", "redirectEnd", "fetchStart",
    "domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd", "secureConnectionStart", "requestStart",
    "responseStart", "responseEnd", "domLoading", "domInteractive", "domContentLoadedEventStart",
    "domContentLoadedEventEnd", "domComplete", "loadEventStart", "loadEventEnd",
  ];
  class PerformanceTiming {
    constructor(...args: unknown[]) {
      if (args[0] !== ILLEGAL) throw illegal();
    }
    toJSON() {
      return Object.fromEntries(TIMING.map(name => [name, 0]));
    }
  }
  for (const name of TIMING)
    Object.defineProperty(PerformanceTiming.prototype, name, { get: () => 0, enumerable: true, configurable: true });
  toStringTag(PerformanceTiming.prototype, "PerformanceTiming");
  const timing = new PerformanceTiming(ILLEGAL);
  defineMethods(Performance.prototype, {
    toJSON() {
      return { timeOrigin: clock.timeOrigin(), timing };
    },
    getEntries() {
      return byStartTime([...all(marks), ...all(measures)]);
    },
    getEntriesByType(entryType: unknown) {
      const type = String(entryType);
      return byStartTime(type === "mark" ? all(marks) : type === "measure" ? all(measures) : []);
    },
    getEntriesByName(name: unknown, entryType?: unknown) {
      const n = String(name);
      const type = entryType === undefined ? undefined : String(entryType);
      const entries: PerformanceEntry[] = [];
      if (type === undefined || type === "mark") entries.push(...(marks.get(n) ?? []));
      if (type === undefined || type === "measure") entries.push(...(measures.get(n) ?? []));
      return byStartTime(entries);
    },
    clearResourceTimings() {},
    setResourceTimingBufferSize(_size: unknown) {},
    mark(markName: unknown, markOptions?: unknown) {
      if (arguments.length < 1) throw new TypeError("Not enough arguments");
      const mark = new PerformanceMark(String(markName), markOptions);
      add(marks, mark);
      queueEntry(mark);
      return mark;
    },
    clearMarks(markName?: unknown) {
      if (markName === undefined) marks.clear();
      else marks.delete(String(markName));
    },
    measure(measureName: unknown, startOrMeasureOptions?: unknown, endMark?: unknown) {
      if (arguments.length < 1) throw new TypeError("Not enough arguments");
      const entry = measure(String(measureName), startOrMeasureOptions, endMark);
      queueEntry(entry);
      return entry;
    },
    clearMeasures(measureName?: unknown) {
      if (measureName === undefined) measures.clear();
      else measures.delete(String(measureName));
    },
    markResourceTiming() {},
  });
  Object.defineProperties(Performance.prototype, {
    timeOrigin: { get: () => clock.timeOrigin(), enumerable: true, configurable: true },
    timing: { get: () => timing, enumerable: true, configurable: true },
    onresourcetimingbufferfull: {
      get: () => onresourcetimingbufferfull,
      set: (value: unknown) => {
        onresourcetimingbufferfull = typeof value === "function" || (typeof value === "object" && value !== null) ? value : null;
      },
      enumerable: true,
      configurable: true,
    },
  });
  const performance = new Performance(ILLEGAL);
  // JSPerformance::finishCreation: `now` is an own function that is not a constructor.
  Object.defineProperty(performance, "now", {
    value: { now: () => clock.now() }.now,
    writable: true,
    enumerable: true,
    configurable: true,
  });

  // ───────────────────────── PerformanceObserver ─────────────────────────
  const QUEUE = Symbol("queue");
  const SUPPORTED = Object.freeze(["mark", "measure"]);
  class PerformanceObserverEntryList {
    #entries: PerformanceEntry[];
    constructor(token: symbol, entries: PerformanceEntry[]) {
      if (token !== ILLEGAL) throw illegal();
      this.#entries = byStartTime([...entries]);
    }
    getEntries() {
      return [...this.#entries];
    }
    getEntriesByType(entryType: unknown) {
      const type = String(entryType);
      return this.#entries.filter(e => e.entryType === type);
    }
    getEntriesByName(name: unknown, entryType?: unknown) {
      const n = String(name);
      const type = entryType === undefined ? undefined : String(entryType);
      return this.#entries.filter(e => e.name === n && (type === undefined || e.entryType === type));
    }
  }
  toStringTag(PerformanceObserverEntryList.prototype, "PerformanceObserverEntryList");

  class PerformanceObserver {
    #callback: Function;
    #types = new Set<string>();
    #buffer: PerformanceEntry[] = [];
    #queued = false;
    constructor(callback: unknown) {
      if (typeof callback !== "function") throw new TypeError("The callback of PerformanceObserver must be a function");
      this.#callback = callback;
    }
    static get supportedEntryTypes() {
      return SUPPORTED;
    }
    observe(options: any = {}) {
      const types: unknown[] | undefined = options.entryTypes;
      if (types !== undefined && options.type !== undefined)
        throw new TypeError("observe() takes entryTypes or type, not both");
      if (types === undefined && options.type === undefined) throw new TypeError("observe() needs entryTypes or type");
      const wanted = (types ?? [options.type]).map(String).filter(type => SUPPORTED.includes(type));
      if (types !== undefined) this.#types = new Set(wanted);
      else for (const type of wanted) this.#types.add(type);
      if (this.#types.size === 0) return;
      observers.add(this);
      if (options.buffered && types === undefined) {
        for (const entry of [...all(marks), ...all(measures)]) if (this.#types.has(entry.entryType)) this[QUEUE](entry);
      }
    }
    disconnect() {
      observers.delete(this);
      this.#buffer = [];
    }
    takeRecords() {
      const records = this.#buffer;
      this.#buffer = [];
      return records;
    }
    [QUEUE](entry: PerformanceEntry) {
      if (!this.#types.has(entry.entryType)) return;
      this.#buffer.push(entry);
      if (this.#queued) return;
      this.#queued = true;
      // PerformanceObserver::queueEntry: the callback runs in a task of its own.
      setTimeout(() => {
        this.#queued = false;
        const records = this.takeRecords();
        if (records.length === 0 || !observers.has(this)) return;
        this.#callback.call(this, new PerformanceObserverEntryList(ILLEGAL, records), this);
      }, 0);
    }
  }
  toStringTag(PerformanceObserver.prototype, "PerformanceObserver");

  for (const [name, value] of Object.entries({
    Performance,
    PerformanceTiming,
    PerformanceEntry,
    PerformanceMark,
    PerformanceMeasure,
    PerformanceObserver,
    PerformanceObserverEntryList,
  }))
    Object.defineProperty(G, name, { value, writable: true, enumerable: false, configurable: true });
  Object.defineProperty(G, "performance", { value: performance, writable: true, enumerable: true, configurable: true });
})((globalThis as any).__bun_native_ops__.performanceClock);
