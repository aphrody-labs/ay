// `EventTarget`, `Event`, `CustomEvent`, `MessageEvent`, `ErrorEvent`, `CloseEvent` and `DOMException` on Boa: the
// DOM standard's (WebCore's EventTarget.cpp, Event.cpp, MessageEvent.cpp, ErrorEvent.cpp, DOMException.cpp in
// src/jsc/bindings/webcore), for targets without a tree: dispatch reaches the target's own listeners only. A script:
// it runs before js/performance.ts and js/bun.ts. An exception a listener throws is reported, as WebCore's
// `reportException`, and the other listeners still run.
(function install() {
  const G = globalThis as any;
  const define = (name: string, value: unknown) =>
    Object.defineProperty(G, name, { value, writable: true, enumerable: false, configurable: true });
  const toStringTag = (target: object, tag: string) =>
    Object.defineProperty(target, Symbol.toStringTag, { value: tag, configurable: true });
  const report = (error: unknown) =>
    queueMicrotask(() => {
      throw error;
    });
  /** WebIDL's checks of a constructor's arguments: how many, and that a dictionary is an object. */
  const required = (count: number, length: number) => {
    if (count < length) throw new TypeError("Not enough arguments");
  };
  const dictionary = (init: unknown) => {
    if (init !== undefined && init !== null && typeof init !== "object" && typeof init !== "function")
      throw new TypeError("The \"eventInitDict\" argument must be an object");
    return init as any;
  };
  /** A value as WebCore's binding errors name it. */
  const describe = (value: unknown) => (typeof G.Bun?.inspect === "function" ? G.Bun.inspect(value) : String(value));

  // ───────────────────────── DOMException ─────────────────────────
  // The legacy codes of WebIDL's table, by name.
  const CODES: Record<string, number> = {
    IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5,
    NoModificationAllowedError: 7, NotFoundError: 8, NotSupportedError: 9, InUseAttributeError: 10,
    InvalidStateError: 11, SyntaxError: 12, InvalidModificationError: 13, NamespaceError: 14,
    InvalidAccessError: 15, TypeMismatchError: 17, SecurityError: 18, NetworkError: 19, AbortError: 20,
    URLMismatchError: 21, QuotaExceededError: 22, TimeoutError: 23, InvalidNodeTypeError: 24, DataCloneError: 25,
  };
  if (typeof G.DOMException !== "function") {
    class DOMException extends Error {
      #name: string;
      #code: number;
      constructor(message: unknown = "", name: unknown = "Error") {
        super(String(message));
        this.#name = String(name);
        this.#code = CODES[this.#name] ?? 0;
      }
      get name() {
        return this.#name;
      }
      get code() {
        return this.#code;
      }
    }
    toStringTag(DOMException.prototype, "DOMException");
    for (const [name, code] of Object.entries(CODES)) {
      const constant = name.replace(/([a-z])([A-Z])/g, "$1_$2").replace(/Error$/, "_ERR").toUpperCase();
      for (const target of [DOMException, DOMException.prototype])
        Object.defineProperty(target, constant, { value: code, enumerable: true });
    }
    define("DOMException", DOMException);
  }

  // ───────────────────────── Event ─────────────────────────
  const NONE = 0, AT_TARGET = 2;
  type Flags = { stop: boolean; stopImmediate: boolean; canceled: boolean; dispatching: boolean; passive: boolean };
  const flags = new WeakMap<Event, Flags>();
  const flagsOf = (event: Event) => {
    const f = flags.get(event);
    if (!f) throw new TypeError("Illegal invocation");
    return f;
  };

  class Event {
    #type: string;
    #bubbles: boolean;
    #cancelable: boolean;
    #composed: boolean;
    #timeStamp = performanceNow();
    #target: unknown = null;
    #currentTarget: unknown = null;
    #phase = NONE;
    constructor(type: unknown, init?: { bubbles?: boolean; cancelable?: boolean; composed?: boolean }) {
      required(arguments.length, 1);
      init = dictionary(init);
      this.#type = String(type);
      this.#bubbles = !!init?.bubbles;
      this.#cancelable = !!init?.cancelable;
      this.#composed = !!init?.composed;
      flags.set(this, { stop: false, stopImmediate: false, canceled: false, dispatching: false, passive: false });
    }
    get type() {
      return this.#type;
    }
    get bubbles() {
      return this.#bubbles;
    }
    get cancelable() {
      return this.#cancelable;
    }
    get composed() {
      return this.#composed;
    }
    get timeStamp() {
      return this.#timeStamp;
    }
    get target() {
      return this.#target;
    }
    get srcElement() {
      return this.#target;
    }
    get currentTarget() {
      return this.#currentTarget;
    }
    get eventPhase() {
      return this.#phase;
    }
    get isTrusted() {
      return false;
    }
    get defaultPrevented() {
      return flagsOf(this).canceled;
    }
    get returnValue() {
      return !flagsOf(this).canceled;
    }
    set returnValue(value) {
      if (!value) this.preventDefault();
    }
    get cancelBubble() {
      return flagsOf(this).stop;
    }
    set cancelBubble(value) {
      if (value) flagsOf(this).stop = true;
    }
    preventDefault() {
      const f = flagsOf(this);
      if (this.#cancelable && !f.passive) f.canceled = true;
    }
    stopPropagation() {
      flagsOf(this).stop = true;
    }
    stopImmediatePropagation() {
      const f = flagsOf(this);
      f.stop = f.stopImmediate = true;
    }
    composedPath() {
      return flagsOf(this).dispatching && this.#currentTarget ? [this.#currentTarget] : [];
    }
    initEvent(type: unknown, bubbles = false, cancelable = false) {
      if (flagsOf(this).dispatching) return;
      this.#type = String(type);
      this.#bubbles = !!bubbles;
      this.#cancelable = !!cancelable;
    }
    /** EventTarget's dispatch: the event at its target. */
    static set(event: Event, target: unknown, phase: number) {
      event.#target ??= target;
      event.#currentTarget = target;
      event.#phase = phase;
    }
    static clear(event: Event) {
      event.#currentTarget = null;
      event.#phase = NONE;
    }
  }
  const eventSet = Event.set, eventClear = Event.clear;
  delete (Event as any).set;
  delete (Event as any).clear;
  for (const [name, value] of [["NONE", 0], ["CAPTURING_PHASE", 1], ["AT_TARGET", 2], ["BUBBLING_PHASE", 3]] as const)
    for (const target of [Event, Event.prototype]) Object.defineProperty(target, name, { value, enumerable: true });
  toStringTag(Event.prototype, "Event");
  function performanceNow() {
    return typeof G.performance?.now === "function" ? G.performance.now() : Date.now();
  }

  class CustomEvent extends Event {
    #detail: unknown;
    constructor(type: unknown, init?: { detail?: unknown; bubbles?: boolean; cancelable?: boolean; composed?: boolean }) {
      required(arguments.length, 1);
      super(type, dictionary(init));
      this.#detail = init?.detail ?? null;
    }
    get detail() {
      return this.#detail;
    }
    initCustomEvent(type: unknown, bubbles?: boolean, cancelable?: boolean, detail?: unknown) {
      this.initEvent(type, bubbles, cancelable);
      this.#detail = detail;
    }
  }
  toStringTag(CustomEvent.prototype, "CustomEvent");

  class MessageEvent extends Event {
    #data: unknown;
    #origin: string;
    #lastEventId: string;
    #source: unknown;
    #ports: readonly unknown[];
    constructor(
      type: unknown,
      init?: { data?: unknown; origin?: unknown; lastEventId?: unknown; source?: unknown; ports?: Iterable<unknown> } & EventInit,
    ) {
      required(arguments.length, 1);
      init = dictionary(init);
      super(type, init);
      this.#data = init?.data ?? null;
      this.#origin = init?.origin === undefined ? "" : String(init.origin);
      this.#lastEventId = init?.lastEventId === undefined ? "" : String(init.lastEventId);
      const source = init?.source ?? null;
      if (source !== null && !isMessagePort(source))
        throw new TypeError(
          `MessageEvent constructor: Expected eventInitDict.source (${JSON.stringify(describe(source))}) to be an instance of MessagePort.`,
        );
      this.#source = source;
      let ports: unknown[] = [];
      if (init?.ports !== undefined) {
        const list = init.ports;
        if ((typeof list !== "object" && typeof list !== "function") || list === null || typeof list[Symbol.iterator] !== "function")
          throw new TypeError(`MessageEvent constructor: eventInitDict.ports (${describe(list)}) is not iterable.`);
        ports = [...list];
      }
      ports.forEach((port, i) => {
        if (!isMessagePort(port))
          throw new TypeError(
            `MessageEvent constructor: Expected eventInitDict.ports[${i}] (${JSON.stringify(describe(port))}) to be an instance of MessagePort.`,
          );
      });
      this.#ports = Object.freeze(ports);
    }
    get data() {
      return this.#data;
    }
    get origin() {
      return this.#origin;
    }
    get lastEventId() {
      return this.#lastEventId;
    }
    get source() {
      return this.#source;
    }
    get ports() {
      return this.#ports;
    }
    initMessageEvent(
      type: unknown,
      bubbles = false,
      cancelable = false,
      data: unknown = null,
      origin = "",
      lastEventId = "",
      source: unknown = null,
      ports: unknown[] = [],
    ) {
      this.initEvent(type, bubbles, cancelable);
      this.#data = data;
      this.#origin = String(origin);
      this.#lastEventId = String(lastEventId);
      this.#source = source;
      this.#ports = Object.freeze([...ports]);
    }
  }
  toStringTag(MessageEvent.prototype, "MessageEvent");
  const isMessagePort = (value: unknown) => typeof G.MessagePort === "function" && value instanceof G.MessagePort;

  class ErrorEvent extends Event {
    #message: string;
    #filename: string;
    #lineno: number;
    #colno: number;
    #error: unknown;
    constructor(
      type: unknown,
      init?: { message?: unknown; filename?: unknown; lineno?: unknown; colno?: unknown; error?: unknown } & EventInit,
    ) {
      required(arguments.length, 1);
      init = dictionary(init);
      super(type, init);
      this.#message = init?.message === undefined ? "" : String(init.message);
      this.#filename = init?.filename === undefined ? "" : String(init.filename);
      this.#lineno = Number(init?.lineno ?? 0) >>> 0;
      this.#colno = Number(init?.colno ?? 0) >>> 0;
      this.#error = init?.error;
    }
    get message() {
      return this.#message;
    }
    get filename() {
      return this.#filename;
    }
    get lineno() {
      return this.#lineno;
    }
    get colno() {
      return this.#colno;
    }
    get error() {
      return this.#error;
    }
  }
  toStringTag(ErrorEvent.prototype, "ErrorEvent");

  class CloseEvent extends Event {
    #wasClean: boolean;
    #code: number;
    #reason: string;
    constructor(type: unknown, init?: { wasClean?: boolean; code?: number; reason?: unknown } & EventInit) {
      required(arguments.length, 1);
      init = dictionary(init);
      super(type, init);
      this.#wasClean = !!init?.wasClean;
      this.#code = Number(init?.code ?? 0) & 0xffff;
      this.#reason = init?.reason === undefined ? "" : String(init.reason);
    }
    get wasClean() {
      return this.#wasClean;
    }
    get code() {
      return this.#code;
    }
    get reason() {
      return this.#reason;
    }
  }
  toStringTag(CloseEvent.prototype, "CloseEvent");

  // ───────────────────────── EventTarget ─────────────────────────
  type Listener = {
    callback: EventListenerOrEventListenerObject;
    capture: boolean;
    once: boolean;
    passive: boolean;
    removed: boolean;
  };
  const listeners = new WeakMap<object, Map<string, Listener[]>>();
  const isEventTarget = (value: unknown) => typeof value === "object" && value !== null && listeners.has(value);
  const flatten = (options: unknown) =>
    typeof options === "boolean" ? { capture: options } : ((options ?? {}) as AddEventListenerOptions);

  class EventTarget {
    constructor() {
      listeners.set(this, new Map());
    }
    addEventListener(type: unknown, callback: EventListenerOrEventListenerObject | null, options?: unknown) {
      const map = listenersOf(this);
      if (arguments.length < 2) throw new TypeError("addEventListener requires 2 arguments");
      if (callback === null || callback === undefined) return;
      if (typeof callback !== "function" && typeof callback !== "object")
        throw new TypeError("The \"listener\" argument must be a function or an object with handleEvent");
      const { capture = false, once = false, passive = false, signal } = flatten(options) as AddEventListenerOptions;
      if (signal?.aborted) return;
      const key = String(type);
      let list = map.get(key);
      if (!list) map.set(key, (list = []));
      if (list.some(l => l.callback === callback && l.capture === !!capture)) return;
      const listener: Listener = { callback, capture: !!capture, once: !!once, passive: !!passive, removed: false };
      list.push(listener);
      listenerAdded(this, key, list.length);
      if (signal && typeof (signal as any).addEventListener === "function")
        (signal as any).addEventListener("abort", () => this.removeEventListener(key, callback, { capture }), { once: true });
    }
    removeEventListener(type: unknown, callback: EventListenerOrEventListenerObject | null, options?: unknown) {
      const map = listenersOf(this);
      const key = String(type);
      const list = map.get(key);
      if (!list) return;
      const capture = !!flatten(options).capture;
      const index = list.findIndex(l => l.callback === callback && l.capture === capture);
      if (index === -1) return;
      list[index].removed = true;
      list.splice(index, 1);
      listenerRemoved(this, key, list.length);
    }
    dispatchEvent(event: Event) {
      listenersOf(this);
      if (!(event instanceof Event)) throw new TypeError("The \"event\" argument must be an instance of Event");
      const f = flagsOf(event);
      if (f.dispatching) throw new (G.DOMException)("The event is already being dispatched", "InvalidStateError");
      f.dispatching = true;
      f.stop = f.stopImmediate = false;
      try {
        eventSet(event, this, AT_TARGET);
        invoke(this, event, f);
      } finally {
        eventClear(event);
        f.dispatching = false;
      }
      return !f.canceled;
    }
  }
  toStringTag(EventTarget.prototype, "EventTarget");

  function listenersOf(target: object) {
    const map = listeners.get(target);
    if (!map) throw new TypeError("Illegal invocation");
    return map;
  }

  /** The `on<type>` handler, then the listeners, in the order they were added. */
  function invoke(target: object, event: Event, f: Flags) {
    const list = listeners.get(target)?.get(event.type);
    if (!list || list.length === 0) return;
    for (const listener of [...list]) {
      if (listener.removed) continue;
      if (listener.once) target.removeEventListener(event.type, listener.callback, { capture: listener.capture });
      f.passive = listener.passive;
      try {
        const callback = listener.callback as any;
        if (typeof callback === "function") callback.call(target, event);
        else {
          const handleEvent = callback.handleEvent;
          if (typeof handleEvent !== "function") throw new TypeError("handleEvent is not a function");
          handleEvent.call(callback, event);
        }
      } catch (error) {
        report(error);
      } finally {
        f.passive = false;
      }
      if (f.stopImmediate) break;
    }
  }

  // What a target's class learns of its listeners, to ref and unref what keeps the loop alive (a port's 'message').
  const LISTENERS_CHANGED = Symbol.for("bun.boa.listenersChanged");
  function listenerAdded(target: any, type: string, count: number) {
    target[LISTENERS_CHANGED]?.(type, count);
  }
  function listenerRemoved(target: any, type: string, count: number) {
    target[LISTENERS_CHANGED]?.(type, count);
  }

  /** `on<type>` as an IDL event handler attribute: a listener that takes the place of the previous handler. */
  function defineEventHandler(prototype: object, type: string) {
    const handlers = new WeakMap<object, { handler: unknown; listener: (event: Event) => unknown }>();
    Object.defineProperty(prototype, `on${type}`, {
      get() {
        return handlers.get(this)?.handler ?? null;
      },
      set(handler: unknown) {
        const current = handlers.get(this);
        if (typeof handler !== "function" && (typeof handler !== "object" || handler === null)) handler = null;
        if (current) {
          if (handler === null) {
            this.removeEventListener(type, current.listener);
            handlers.delete(this);
          } else current.handler = handler;
          return;
        }
        if (handler === null) return;
        const entry = {
          handler,
          listener: function (this: unknown, event: Event) {
            const h = entry.handler as any;
            if (typeof h !== "function") return;
            const result = h.call(this, event);
            if (result === false && type !== "error") event.preventDefault();
          },
        };
        handlers.set(this, entry);
        this.addEventListener(type, entry.listener);
      },
      enumerable: true,
      configurable: true,
    });
  }

  define("Event", Event);
  define("CustomEvent", CustomEvent);
  define("MessageEvent", MessageEvent);
  define("ErrorEvent", ErrorEvent);
  define("CloseEvent", CloseEvent);
  define("EventTarget", EventTarget);
  // For js/worker.ts.
  Object.defineProperty(G, Symbol.for("bun.boa.events"), {
    value: { defineEventHandler, LISTENERS_CHANGED, isEventTarget, report },
    configurable: true,
  });
})();
