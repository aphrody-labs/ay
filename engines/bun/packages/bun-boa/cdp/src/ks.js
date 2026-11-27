// The page side of the DevTools protocol, installed as `__ks` in every document before its scripts run.
// Every entry point returns a JSON string, so the embedder only ever reads strings out of Boa.
(() => {
  const objects = new Map();
  const groups = new Map();
  const pending = new Map();
  const bindings = [];
  let nextObject = 1;
  let nextPending = 1;
  const nodeId = n => (n !== null && typeof n === "object" ? globalThis.__blitz_node_id(n) : null);
  const nodeById = id => globalThis.__blitz_node(id);

  function store(v, group) {
    const id = `ks.${nextObject++}`;
    objects.set(id, v);
    if (group) {
      if (!groups.has(group)) groups.set(group, []);
      groups.get(group).push(id);
    }
    return id;
  }

  function className(v) {
    try {
      const name = v.constructor && v.constructor.name;
      return typeof name === "string" && name ? name : "Object";
    } catch {
      return "Object";
    }
  }

  function subtype(v) {
    if (v === null) return "null";
    if (Array.isArray(v)) return "array";
    if (nodeId(v) !== null) return "node";
    if (v instanceof Error) return "error";
    if (v instanceof RegExp) return "regexp";
    if (v instanceof Date) return "date";
    if (v instanceof Promise) return "promise";
    if (v instanceof Map) return "map";
    if (v instanceof Set) return "set";
    if (typeof v.length === "number" && typeof v.item === "function") return "array";
    return undefined;
  }

  function nodeDescription(n) {
    if (n.nodeType !== 1) return n.nodeName;
    let d = String(n.localName || n.nodeName).toLowerCase();
    if (n.id) d += `#${n.id}`;
    const cls = typeof n.className === "string" ? n.className.trim() : "";
    if (cls) d += "." + cls.split(/\s+/).join(".");
    return d;
  }

  function description(v, st) {
    if (typeof v === "function") return String(v).slice(0, 200);
    if (st === "node") return nodeDescription(v);
    if (st === "array") return `${className(v)}(${v.length})`;
    if (st === "error") return String(v.stack || v);
    if (st === "regexp" || st === "date") return String(v);
    return className(v);
  }

  function unserializable(v) {
    if (typeof v === "number") {
      if (Number.isNaN(v)) return "NaN";
      if (v === Infinity) return "Infinity";
      if (v === -Infinity) return "-Infinity";
      if (Object.is(v, -0)) return "-0";
    }
    return undefined;
  }

  function byValue(v) {
    if (nodeId(v) !== null) return {};
    const s = JSON.stringify(v);
    return s === undefined ? undefined : JSON.parse(s);
  }

  function remote(v, returnByValue, group) {
    const t = typeof v;
    if (t === "undefined") return { type: "undefined" };
    if (t === "number") {
      const u = unserializable(v);
      return u ? { type: t, unserializableValue: u, description: u } : { type: t, value: v, description: String(v) };
    }
    if (t === "bigint") return { type: t, unserializableValue: `${v}n`, description: `${v}n` };
    if (t === "string" || t === "boolean") return { type: t, value: v };
    if (t === "symbol") return { type: t, description: String(v), objectId: store(v, group) };
    if (v === null) return { type: "object", subtype: "null", value: null };
    if (returnByValue && t !== "function") {
      try {
        return { type: t, value: byValue(v) };
      } catch {
        return { type: t, value: {} };
      }
    }
    const st = subtype(v);
    const r = { type: t, className: className(v), description: description(v, st), objectId: store(v, group) };
    if (st) r.subtype = st;
    return r;
  }

  function exception(e, group) {
    const text = e instanceof Error ? `Uncaught ${e.name}: ${e.message}` : "Uncaught";
    return { exceptionId: 1, text, lineNumber: 0, columnNumber: 0, exception: remote(e, false, group) };
  }

  const fail = (e, o) => JSON.stringify({ result: remote(e, false, o.group), exceptionDetails: exception(e, o.group) });

  function settle(v, o) {
    if (o.awaitPromise && v !== null && (typeof v === "object" || typeof v === "function") && typeof v.then === "function") {
      const id = nextPending++;
      pending.set(id, null);
      Promise.resolve(v).then(
        r => pending.set(id, JSON.stringify({ result: remote(r, o.returnByValue, o.group) })),
        e => pending.set(id, fail(e, o)),
      );
      return JSON.stringify({ pending: id });
    }
    return JSON.stringify({ result: remote(v, o.returnByValue, o.group) });
  }

  function argument(a) {
    if (!a) return undefined;
    if ("objectId" in a) return objects.get(a.objectId);
    if ("unserializableValue" in a) {
      const u = a.unserializableValue;
      if (u.endsWith("n")) return BigInt(u.slice(0, -1));
      return { NaN: NaN, Infinity: Infinity, "-Infinity": -Infinity, "-0": -0 }[u];
    }
    return a.value;
  }

  function cdpNode(n, depth, parent) {
    const id = nodeId(n);
    const r = {
      nodeId: id,
      backendNodeId: id,
      nodeType: n.nodeType,
      nodeName: n.nodeName,
      localName: n.nodeType === 1 ? String(n.localName || "").toLowerCase() : "",
      nodeValue: n.nodeType === 3 || n.nodeType === 8 ? String(n.nodeValue ?? "") : "",
    };
    if (parent) r.parentId = parent;
    if (n.nodeType === 1) {
      r.attributes = [];
      const names = typeof n.getAttributeNames === "function" ? n.getAttributeNames() : [];
      for (const name of names) r.attributes.push(name, String(n.getAttribute(name) ?? ""));
    }
    if (n.nodeType === 9) {
      r.documentURL = String(location.href);
      r.baseURL = String(location.href);
      r.xmlVersion = "";
      r.compatibilityMode = "NoQuirksMode";
    }
    const kids = n.childNodes ? Array.from(n.childNodes) : [];
    r.childNodeCount = kids.length;
    if (depth !== 0) r.children = kids.map(c => cdpNode(c, depth - 1, id));
    return r;
  }

  function quad(rect) {
    const { x, y, width: w, height: h } = rect;
    return [x, y, x + w, y, x + w, y + h, x, y + h];
  }

  const ROLES = {
    a: "link", article: "article", aside: "complementary", button: "button", dialog: "dialog", footer: "contentinfo",
    form: "form", h1: "heading", h2: "heading", h3: "heading", h4: "heading", h5: "heading", h6: "heading",
    header: "banner", hr: "separator", img: "image", li: "listitem", main: "main", nav: "navigation", ol: "list",
    option: "option", p: "paragraph", section: "region", select: "combobox", table: "table", tbody: "rowgroup",
    td: "cell", textarea: "textbox", th: "columnheader", thead: "rowgroup", tr: "row", ul: "list", label: "LabelText",
  };
  const NAMED_FROM_CONTENT = new Set(["link", "button", "heading", "option", "cell", "columnheader", "listitem", "LabelText"]);
  const SKIP = new Set(["script", "style", "noscript", "template", "head", "meta", "link", "title"]);

  function inputRole(el) {
    const type = String(el.getAttribute("type") || "text").toLowerCase();
    if (type === "hidden") return null;
    return { checkbox: "checkbox", radio: "radio", button: "button", submit: "button", reset: "button", range: "slider", search: "searchbox" }[type] || "textbox";
  }

  function axTree() {
    const nodes = [];
    let next = 1;
    const text = el => String(el.textContent || "").replace(/\s+/g, " ").trim();
    function visit(n, parentId) {
      if (n.nodeType === 3) {
        const value = String(n.nodeValue || "").replace(/\s+/g, " ").trim();
        if (!value) return [];
        const id = String(next++);
        nodes.push({ nodeId: id, parentId, ignored: false, role: { type: "role", value: "StaticText" }, name: { type: "computedString", value }, childIds: [], backendDOMNodeId: nodeId(n), properties: [] });
        return [id];
      }
      if (n.nodeType !== 1 && n.nodeType !== 9) return [];
      const tag = n.nodeType === 1 ? String(n.localName || "").toLowerCase() : "#document";
      if (SKIP.has(tag)) return [];
      if (n.nodeType === 1 && (n.hidden || n.getAttribute("aria-hidden") === "true")) return [];
      if (tag === "input" && inputRole(n) === null) return [];
      let role = n.nodeType === 9 ? "RootWebArea" : n.getAttribute("role") || (tag === "input" ? inputRole(n) : ROLES[tag]) || "generic";
      if (tag === "a" && !n.hasAttribute("href")) role = "generic";
      const id = String(next++);
      const node = { nodeId: id, ignored: false, role: { type: "role", value: role }, childIds: [], backendDOMNodeId: nodeId(n), properties: [] };
      if (parentId) node.parentId = parentId;
      nodes.push(node);
      let name = "";
      if (n.nodeType === 9) name = String(document.title || "");
      else name = n.getAttribute("aria-label") || n.getAttribute("alt") || n.getAttribute("title") || "";
      if (!name && tag === "input") name = n.getAttribute("placeholder") || (n.getAttribute("type") === "submit" ? n.value || "Submit" : "");
      if (!name && NAMED_FROM_CONTENT.has(role)) name = text(n);
      node.name = { type: "computedString", value: name };
      const props = node.properties;
      if (role === "heading") props.push({ name: "level", value: { type: "integer", value: Number(tag.slice(1)) || 2 } });
      if (role === "checkbox" || role === "radio") props.push({ name: "checked", value: { type: "tristate", value: n.checked ? "true" : "false" } });
      if (n.nodeType === 1 && n.hasAttribute("disabled")) props.push({ name: "disabled", value: { type: "boolean", value: true } });
      if (role === "textbox" || role === "searchbox" || role === "combobox" || role === "slider") node.value = { type: "string", value: String(n.value ?? "") };
      if (role === "link" && n.href) props.push({ name: "url", value: { type: "string", value: String(n.href) } });
      for (const c of Array.from(n.childNodes || [])) node.childIds.push(...visit(c, id));
      return [id];
    }
    visit(document, undefined);
    return nodes;
  }

  const ks = {
    evaluate(src, o) {
      let v;
      try {
        v = (0, eval)(src);
      } catch (e) {
        return fail(e, o);
      }
      return settle(v, o);
    },
    call(objectId, fnSrc, args, o) {
      if (objectId && !objects.has(objectId)) return JSON.stringify({ error: "Could not find object with given id" });
      let v;
      try {
        const fn = (0, eval)(`(${fnSrc})`);
        v = fn.apply(objectId ? objects.get(objectId) : globalThis, (args || []).map(argument));
      } catch (e) {
        return fail(e, o);
      }
      return settle(v, o);
    },
    take(id) {
      const r = pending.get(id);
      if (r === null || r === undefined) return "";
      pending.delete(id);
      return r;
    },
    release(id) {
      objects.delete(id);
      return "{}";
    },
    releaseGroup(group) {
      for (const id of groups.get(group) || []) objects.delete(id);
      groups.delete(group);
      return "{}";
    },
    properties(objectId, ownProperties) {
      if (!objects.has(objectId)) return JSON.stringify({ error: "Could not find object with given id" });
      const v = objects.get(objectId);
      const result = [];
      const seen = new Set();
      for (let o = v; o !== null && o !== undefined; o = ownProperties ? null : Object.getPrototypeOf(o)) {
        for (const name of Object.getOwnPropertyNames(o)) {
          if (seen.has(name)) continue;
          seen.add(name);
          const d = Object.getOwnPropertyDescriptor(o, name);
          const p = { name, configurable: !!d.configurable, enumerable: !!d.enumerable, isOwn: o === v };
          if ("value" in d) {
            p.value = remote(d.value, false);
            p.writable = !!d.writable;
          } else {
            if (d.get) p.get = remote(d.get, false);
            if (d.set) p.set = remote(d.set, false);
          }
          result.push(p);
        }
        if (typeof o !== "object" && typeof o !== "function") break;
      }
      return JSON.stringify({ result });
    },
    nodeObject(id, group) {
      const n = nodeById(id);
      return n ? JSON.stringify(remote(n, false, group)) : "";
    },
    nodeIdOf(objectId) {
      return String(nodeId(objects.get(objectId)) ?? 0);
    },
    document(depth) {
      return JSON.stringify(cdpNode(document, depth, 0));
    },
    describe(id, depth) {
      const n = nodeById(id);
      return n ? JSON.stringify(cdpNode(n, depth, n.parentNode ? nodeId(n.parentNode) : 0)) : "";
    },
    querySelector(id, selector) {
      const n = nodeById(id);
      return String((n && nodeId(n.querySelector(selector))) || 0);
    },
    querySelectorAll(id, selector) {
      const n = nodeById(id);
      return JSON.stringify(n ? Array.from(n.querySelectorAll(selector)).map(nodeId) : []);
    },
    outerHTML(id) {
      const n = nodeById(id);
      if (!n) return "";
      if (n.nodeType === 9) return n.documentElement ? n.documentElement.outerHTML : "";
      if (n.nodeType === 1) return n.outerHTML;
      return String(n.nodeValue ?? "");
    },
    boxModel(id) {
      const n = nodeById(id);
      if (!n || typeof n.getBoundingClientRect !== "function") return "";
      const r = n.getBoundingClientRect();
      const q = quad(r);
      return JSON.stringify({ model: { content: q, padding: q, border: q, margin: q, width: r.width, height: r.height } });
    },
    scrollIntoView(id) {
      const n = nodeById(id);
      if (n && typeof n.scrollIntoView === "function") n.scrollIntoView();
      return "{}";
    },
    focus(id) {
      const n = nodeById(id);
      if (n && typeof n.focus === "function") n.focus();
      return "{}";
    },
    setAttribute(id, name, value) {
      const n = nodeById(id);
      if (n) n.setAttribute(name, value);
      return "{}";
    },
    removeAttribute(id, name) {
      const n = nodeById(id);
      if (n) n.removeAttribute(name);
      return "{}";
    },
    setOuterHTML(id, html) {
      const n = nodeById(id);
      if (n) n.outerHTML = html;
      return "{}";
    },
    nodeAt(x, y) {
      const n = typeof document.elementFromPoint === "function" ? document.elementFromPoint(x, y) : null;
      return String(nodeId(n) || 0);
    },
    // Appends `text` to the focused text control after deleting `back` characters.
    editText(text, back) {
      const el = document.activeElement;
      const tag = el && el.nodeType === 1 ? String(el.localName).toLowerCase() : "";
      if (tag !== "textarea" && (tag !== "input" || (inputRole(el) !== "textbox" && inputRole(el) !== "searchbox"))) return "";
      const chars = Array.from(String(el.value ?? ""));
      el.value = chars.slice(0, Math.max(0, chars.length - back)).join("") + text;
      return JSON.stringify({ node: nodeId(el), value: el.value });
    },
    axTree() {
      return JSON.stringify({ nodes: axTree() });
    },
    addBinding(name) {
      globalThis[name] = payload => {
        bindings.push([name, String(payload)]);
      };
      return "{}";
    },
    takeBindings() {
      return JSON.stringify(bindings.splice(0));
    },
    title() {
      return String(document.title || "");
    },
  };
  Object.defineProperty(globalThis, "__ks", { value: ks, enumerable: false, configurable: false, writable: false });

  // Inline `on<event>="…"` attributes of the parsed document, as handler properties (the DOM bindings
  // only dispatch to properties and listeners). Runs before the page's scripts, which may replace them.
  for (const el of Array.from(document.querySelectorAll("*"))) {
    for (const name of el.getAttributeNames()) {
      if (!name.startsWith("on") || name === "onload" && el.localName === "body") continue;
      try {
        el[name] = new Function("event", el.getAttribute(name));
      } catch {}
    }
  }
})();
