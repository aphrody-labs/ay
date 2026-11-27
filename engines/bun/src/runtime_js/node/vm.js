// The JavaScript of node:vm's binding on bun_js (vm.rs): NodeVM.cpp's createContext, isContext and compileFunction,
// and NodeVMScript.cpp's Script, over a realm per context. A function of the natives that vm.rs makes.
(function makeNodeVMBinding(natives) {
  "use strict";
  const { createRealm, realmGlobal, evaluate, check, isModuleNamespaceObject } = natives;

  const DONT_CONTEXTIFY = Symbol("vm_dont_contextify");
  const USE_MAIN_CONTEXT_DEFAULT_LOADER = Symbol("vm_dynamic_import_main_context_default");

  function specificType(value) {
    if (value === null) return "null";
    if (value === undefined) return "undefined";
    if (typeof value === "function") return `function ${value.name}`;
    if (typeof value === "object") return value.constructor?.name ? `an instance of ${value.constructor.name}` : "[Object: null prototype]";
    if (typeof value === "string") return `type string ('${value}')`;
    if (typeof value === "symbol") return `type symbol (${String(value)})`;
    return `type ${typeof value} (${String(value)}${typeof value === "bigint" ? "n" : ""})`;
  }
  function invalidArgType(name, expected, value) {
    const kind = name.includes(".") ? "property" : "argument";
    const error = new TypeError(`The "${name}" ${kind} must be of type ${expected}. Received ${specificType(value)}`);
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }
  function notAContext(name) {
    const error = new TypeError(`The "${name}" argument must be an vm.Context`);
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }

  // ── contexts ──────────────────────────────────────────────────────────────
  /** The realm of each context, and its global object, which forwards to the context unless it is the context. */
  const contexts = new WeakMap();

  function newRealm(contextObject) {
    const handle = createRealm(contextObject);
    return { handle, global: realmGlobal(handle) };
  }

  function createContext(contextObject, options) {
    if (contextObject === DONT_CONTEXTIFY) {
      const realm = newRealm();
      contexts.set(realm.global, realm);
      return realm.global;
    }
    if (contextObject === undefined) contextObject = {};
    if ((typeof contextObject !== "object" && typeof contextObject !== "function") || contextObject === null)
      throw invalidArgType("contextObject", "object", contextObject);
    if (options !== undefined && (typeof options !== "object" || options === null)) throw invalidArgType("options", "object", options);
    if (options) {
      if (options.name !== undefined && typeof options.name !== "string") throw invalidArgType("options.name", "string", options.name);
      if (options.origin !== undefined && typeof options.origin !== "string")
        throw invalidArgType("options.origin", "string", options.origin);
    }
    if (!contexts.has(contextObject)) contexts.set(contextObject, newRealm(contextObject));
    return contextObject;
  }

  function isContext(object) {
    if ((typeof object !== "object" && typeof object !== "function") || object === null) throw invalidArgType("object", "object", object);
    return contexts.has(object);
  }

  /** Runs `code` in the realm of `context`; its global, as a script's result, is the context. */
  function runIn(context, code, filename) {
    const { handle, global } = contexts.get(context);
    const result = evaluate(handle, code, filename);
    return result === global ? context : result;
  }

  // ── Script ────────────────────────────────────────────────────────────────
  const kCode = Symbol("code");
  const kFilename = Symbol("filename");

  function scriptOf(script) {
    if (!(script instanceof Script)) {
      const error = new TypeError("The argument 'this' must be a Script");
      error.code = "ERR_INVALID_ARG_VALUE";
      throw error;
    }
    return script;
  }

  class Script {
    constructor(code, options) {
      code = code === undefined ? "" : String(code);
      if (typeof options === "string") options = { filename: options };
      else if (options !== undefined && (typeof options !== "object" || options === null))
        throw invalidArgType("options", "object", options);
      const filename = options?.filename === undefined ? "evalmachine.<anonymous>" : options.filename;
      if (typeof filename !== "string") throw invalidArgType("options.filename", "string", filename);
      if (options?.importModuleDynamically !== undefined && typeof options.importModuleDynamically !== "function" &&
        options.importModuleDynamically !== USE_MAIN_CONTEXT_DEFAULT_LOADER)
        throw invalidArgType("options.importModuleDynamically", "function", options.importModuleDynamically);
      check(code, filename);
      this[kCode] = code;
      this[kFilename] = filename;
      this.cachedDataRejected = options?.cachedData === undefined ? undefined : false;
      this.sourceMapURL = undefined;
    }
    runInThisContext(options) {
      const script = scriptOf(this);
      return evaluate(undefined, script[kCode], script[kFilename]);
    }
    runInContext(contextifiedObject, options) {
      const script = scriptOf(this);
      if (contextifiedObject !== DONT_CONTEXTIFY && !isContext(contextifiedObject)) throw notAContext("contextifiedObject");
      return runIn(contextifiedObject, script[kCode], script[kFilename]);
    }
    runInNewContext(contextObject, options) {
      const script = scriptOf(this);
      const context = createContext(contextObject, options);
      return runIn(context, script[kCode], script[kFilename]);
    }
    createCachedData() {
      scriptOf(this);
      return new Uint8Array(0);
    }
  }

  // ── compileFunction ───────────────────────────────────────────────────────
  function compileFunction(code, params, options) {
    if (typeof code !== "string") throw invalidArgType("code", "string", code);
    if (params !== undefined && !Array.isArray(params)) {
      const error = new TypeError(`The "params" argument must be an instance of Array. Received ${specificType(params)}`);
      error.code = "ERR_INVALID_ARG_TYPE";
      throw error;
    }
    const names = (params ?? []).map(String);
    const filename = options?.filename ?? "";
    const context = options?.parsingContext;
    if (context !== undefined && !isContext(context)) throw notAContext("options.parsingContext");
    const source = `(function (${names.join(", ")}) {\n${code}\n})`;
    check(source, filename);
    return context === undefined ? evaluate(undefined, source, filename) : runIn(context, source, filename);
  }

  // vm.SourceTextModule and vm.SyntheticModule (NodeVMModule.cpp) are not ported.
  function Module() {
    const error = new Error("vm.Module is not implemented on Boa yet");
    error.code = "ERR_NOT_IMPLEMENTED";
    throw error;
  }

  return {
    Script,
    Module,
    createContext,
    isContext,
    compileFunction,
    isModuleNamespaceObject,
    kLinked: 2,
    kEvaluated: 4,
    kErrored: 5,
    DONT_CONTEXTIFY,
    USE_MAIN_CONTEXT_DEFAULT_LOADER,
  };
});
