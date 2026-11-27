// DOMException (src/jsc/bindings/webcore/JSDOMException.cpp, DOMException.cpp) and what Boa's AbortSignal lacks of
// src/jsc/bindings/webcore/AbortSignal.cpp: AbortSignal.abort(), AbortSignal.timeout(), AbortSignal.any(), and the
// DOMException a signal is aborted with by default. Evaluated once per realm by abort.rs, over Boa's
// AbortController and AbortSignal.
(function () {
  "use strict";
  const G = globalThis;
  const { defineProperty, defineProperties } = Object;

  // DOMException.cpp's descriptions: a name and its legacy code. A name that is not here has code 0.
  const legacyCodes = {
    __proto__: null,
    IndexSizeError: 1,
    HierarchyRequestError: 3,
    WrongDocumentError: 4,
    InvalidCharacterError: 5,
    NoModificationAllowedError: 7,
    NotFoundError: 8,
    NotSupportedError: 9,
    InUseAttributeError: 10,
    InvalidStateError: 11,
    SyntaxError: 12,
    InvalidModificationError: 13,
    NamespaceError: 14,
    InvalidAccessError: 15,
    TypeMismatchError: 17,
    SecurityError: 18,
    NetworkError: 19,
    AbortError: 20,
    URLMismatchError: 21,
    QuotaExceededError: 22,
    TimeoutError: 23,
    InvalidNodeTypeError: 24,
    DataCloneError: 25,
  };
  const constants = [
    ["INDEX_SIZE_ERR", 1],
    ["DOMSTRING_SIZE_ERR", 2],
    ["HIERARCHY_REQUEST_ERR", 3],
    ["WRONG_DOCUMENT_ERR", 4],
    ["INVALID_CHARACTER_ERR", 5],
    ["NO_DATA_ALLOWED_ERR", 6],
    ["NO_MODIFICATION_ALLOWED_ERR", 7],
    ["NOT_FOUND_ERR", 8],
    ["NOT_SUPPORTED_ERR", 9],
    ["INUSE_ATTRIBUTE_ERR", 10],
    ["INVALID_STATE_ERR", 11],
    ["SYNTAX_ERR", 12],
    ["INVALID_MODIFICATION_ERR", 13],
    ["NAMESPACE_ERR", 14],
    ["INVALID_ACCESS_ERR", 15],
    ["VALIDATION_ERR", 16],
    ["TYPE_MISMATCH_ERR", 17],
    ["SECURITY_ERR", 18],
    ["NETWORK_ERR", 19],
    ["ABORT_ERR", 20],
    ["URL_MISMATCH_ERR", 21],
    ["QUOTA_EXCEEDED_ERR", 22],
    ["TIMEOUT_ERR", 23],
    ["INVALID_NODE_TYPE_ERR", 24],
    ["DATA_CLONE_ERR", 25],
  ];

  const slots = new WeakMap();
  const isObject = value => (typeof value === "object" && value !== null) || typeof value === "function";
  const withCode = (error, code) => {
    defineProperty(error, "code", { value: code, writable: true, enumerable: false, configurable: true });
    return error;
  };
  // What Node's `ERR_INVALID_ARG_TYPE` says it received (ErrorCode.cpp's `determineSpecificType`).
  const received = value => {
    if (value === null || value === undefined) return String(value);
    if (typeof value === "function") return `function ${value.name}`;
    if (typeof value === "object") {
      const name = value.constructor?.name;
      return name ? `an instance of ${name}` : "[Object: null prototype]";
    }
    let shown = typeof value === "string" ? `'${value.length > 28 ? value.slice(0, 25) + "..." : value}'` : String(value);
    if (typeof value === "bigint") shown += "n";
    return `type ${typeof value} (${shown})`;
  };

  // `new DOMException(message = "", nameOrOptions)`: the options are `{ name, cause }`.
  const DOMException = function DOMException(message, options) {
    if (!new.target)
      throw withCode(
        new TypeError("Use `new DOMException(...)` instead of `DOMException(...)`"),
        "ERR_ILLEGAL_CONSTRUCTOR",
      );
    message = message === undefined ? "" : String(message);
    let name = "Error";
    let cause;
    let hasCause = false;
    if (isObject(options)) {
      const value = options.name;
      if (value !== undefined) name = String(value);
      if ("cause" in options) {
        hasCause = true;
        cause = options.cause;
      }
    } else if (options !== undefined) {
      name = String(options);
    }
    // An Error, for its stack, whose name and message are the prototype's getters.
    const self = Reflect.construct(Error, [], new.target);
    slots.set(self, { name, message, code: legacyCodes[name] ?? 0 });
    if (hasCause) defineProperty(self, "cause", { value: cause, writable: true, enumerable: false, configurable: true });
    return self;
  };
  defineProperty(DOMException, "length", { value: 0 });
  const prototype = Object.create(Error.prototype);
  defineProperty(prototype, "constructor", { value: DOMException, writable: true, enumerable: false, configurable: true });
  defineProperty(DOMException, "prototype", { value: prototype, writable: false, enumerable: false, configurable: false });
  for (const key of ["code", "name", "message"])
    defineProperty(prototype, key, {
      get: {
        [key]() {
          const slot = slots.get(this);
          if (!slot) throw new TypeError(`The DOMException.${key} getter can only be used on instances of DOMException`);
          return slot[key];
        },
      }[key],
      enumerable: true,
      configurable: true,
    });
  for (const [key, value] of constants) {
    const descriptor = { value, writable: false, enumerable: true, configurable: false };
    defineProperty(DOMException, key, descriptor);
    defineProperty(prototype, key, descriptor);
  }
  defineProperty(prototype, Symbol.toStringTag, { value: "DOMException", configurable: true });
  defineProperty(G, "DOMException", { value: DOMException, writable: true, enumerable: false, configurable: true });

  const { AbortController, AbortSignal } = G;
  if (typeof AbortController !== "function" || typeof AbortSignal !== "function") return;

  // `CommonAbortReason`s, as ErrorCode.cpp's `toJS` makes them.
  const userAbort = () => new DOMException("The operation was aborted.", "AbortError");
  const timedOut = () => new DOMException("The operation timed out.", "TimeoutError");

  // https://dom.spec.whatwg.org/#abortsignal-dependent-signals: what `any()` made of a signal is aborted with it, in
  // the same call. Boa runs a signal's `abort` listeners in a later job, so it is done here, where every abort goes.
  const controllerOf = new WeakMap();
  const dependentsOf = new WeakMap();
  const abort = AbortController.prototype.abort;
  defineProperty(AbortController.prototype, "abort", {
    value: function abort_(reason) {
      const signal = this.signal;
      const wasAborted = signal.aborted;
      const result = abort.call(this, reason === undefined ? userAbort() : reason);
      if (!wasAborted)
        for (const ref of dependentsOf.get(signal) ?? []) {
          const dependent = ref.deref();
          if (dependent && !dependent.aborted) controllerOf.get(dependent).abort(signal.reason);
        }
      return result;
    },
    writable: true,
    enumerable: true,
    configurable: true,
  });
  defineProperty(AbortController.prototype.abort, "name", { value: "abort" });

  const isSignal = value => isObject(value) && value instanceof AbortSignal;
  const statics = {
    // https://dom.spec.whatwg.org/#dom-abortsignal-abort
    abort(reason) {
      const controller = new AbortController();
      controller.abort(reason);
      return controller.signal;
    },
    // https://dom.spec.whatwg.org/#dom-abortsignal-timeout: `[EnforceRange] unsigned long long`. The timer does not
    // keep the process alive.
    timeout(milliseconds) {
      if (arguments.length < 1) throw new TypeError("Not enough arguments");
      const ms = Number(milliseconds);
      if (!Number.isFinite(ms) || Math.trunc(ms) < 0 || Math.trunc(ms) > Number.MAX_SAFE_INTEGER)
        throw new TypeError(`Value ${ms} is outside the range [0, ${Number.MAX_SAFE_INTEGER}]`);
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(timedOut()), Math.trunc(ms));
      timer?.unref?.();
      return controller.signal;
    },
    // https://dom.spec.whatwg.org/#dom-abortsignal-any
    any(signals) {
      if (arguments.length < 1) throw new TypeError("Not enough arguments");
      if (!isObject(signals) || typeof signals[Symbol.iterator] !== "function") throw new TypeError("Type error");
      const sources = [];
      for (const signal of signals) {
        if (!isSignal(signal))
          throw withCode(
            new TypeError(
              `The "signals[${sources.length}]" argument must be an instance of AbortSignal. Received ${received(signal)}`,
            ),
            "ERR_INVALID_ARG_TYPE",
          );
        sources.push(signal);
      }
      const controller = new AbortController();
      const aborted = sources.find(signal => signal.aborted);
      if (aborted) {
        controller.abort(aborted.reason);
        return controller.signal;
      }
      const signal = controller.signal;
      controllerOf.set(signal, controller);
      for (const source of sources) {
        let dependents = dependentsOf.get(source);
        if (!dependents) dependentsOf.set(source, (dependents = []));
        dependents.push(new WeakRef(signal));
      }
      return signal;
    },
  };
  for (const [name, value] of Object.entries(statics))
    defineProperty(AbortSignal, name, { value, writable: true, enumerable: true, configurable: true });
  defineProperties(AbortSignal.abort, { length: { value: 0 } });
})();
