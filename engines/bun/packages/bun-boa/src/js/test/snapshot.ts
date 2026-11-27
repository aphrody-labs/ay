// toMatchSnapshot() and toMatchInlineSnapshot(): a port of src/runtime/test_runner/snapshot.rs for Bun's format and
// of expect.rs's `snapshot` and `inline_snapshot`. A file's snapshots are `__snapshots__/<file>.snap` next to it.
import { deepMatch } from "./equals.ts";
import { diffStrings, type M, type SnapshotHooks } from "./expect.ts";
import { isAsymmetric, isError, showUnquoted, snapshotText } from "./format.ts";
import { framesOf } from "./source.ts";
import { append, native, put, runtime } from "./state.ts";

const HEADER = "// Bun Snapshot v1, https://bun.sh/docs/test/snapshots\n";

export const counts = { total: 0, passed: 0, failed: 0, added: 0 };

let file: { path: string; text: string; values: Map<string, string>; dirty: boolean } | undefined;
const seen = new Map<string, number>();

function snapshotPath(test: string): string {
  const slash = test.lastIndexOf("/");
  return `${test.slice(0, slash)}/__snapshots__/${test.slice(slash + 1)}.snap`;
}

/** The entries of a snapshot file: it is a script that assigns `exports[name] = text`. */
function load() {
  if (file) return file;
  const path = snapshotPath(runtime.path);
  const text = native.readFile(path);
  const values = new Map<string, string>();
  if (text !== undefined) {
    const exports: Record<string, string> = {};
    try {
      new Function("exports", text)(exports);
    } catch {
      throw new Error(`Failed to parse snapshot file for: ${runtime.path}`);
    }
    for (const [key, value] of Object.entries(exports)) values.set(key, String(value));
  }
  file = { path, text: text || HEADER, values, dirty: false };
  return file;
}

/** `write_backtick_string`'s escapes: a backtick, a backslash and `${`. */
const escape = (text: string) => text.replace(/[`\\]|\$(?=\{)/g, c => `\\${c}`);

function nameOf(hint: string): string {
  const sequence = runtime.sequence;
  if (!sequence) throw new Error("Snapshot matchers cannot be used outside of a test");
  return hint ? `${sequence.testName}: ${hint}` : sequence.testName;
}

function addCount(name: string): number {
  counts.total++;
  const n = (seen.get(name) ?? 0) + 1;
  seen.set(name, n);
  return n;
}

/** The value with what `propertyMatchers` matched replaced by the matchers, as the snapshot shows it. */
function withMatchers(value: any, matchers: any): unknown {
  if (typeof value !== "object" || value === null || typeof matchers !== "object" || matchers === null) return value;
  const out: any = Array.isArray(value) ? [...value] : { ...value };
  for (const key of Object.keys(matchers)) {
    const matcher = matchers[key];
    put(out, key, isAsymmetric(matcher) ? matcher : withMatchers(value[key], matcher));
  }
  return out;
}

/** `value_to_snapshot` for Bun's format: what was thrown is its message, if it is an error that has one. */
function valueToSnapshot(received: { thrown: boolean; value: unknown }): unknown {
  if (!received.thrown) return received.value;
  if (!isError(received.value)) return undefined;
  const message = (received.value as Error).message;
  return message ? message : undefined;
}

function printed(m: M, value: unknown, propertyMatchers: unknown): string {
  if (propertyMatchers === undefined) return snapshotText(value);
  if (typeof value !== "object" || value === null)
    m.failf("<b>Matcher error: <red>received<r> values must be an object when the matcher has <green>properties<r>\n");
  if (!deepMatch(value, propertyMatchers))
    m.withParams("<green>propertyMatchers<r>").failf(
      "Expected <green>propertyMatchers<r> to match properties from received object\n\nReceived: {}\n",
      showUnquoted(value),
    );
  return snapshotText(withMatchers(value, propertyMatchers));
}

/** `trim_leading_whitespace_for_inline_snapshot`: the indentation of a template literal's lines removed. */
export function trimInlineSnapshot(saved: string): string {
  const firstNewline = saved.indexOf("\n");
  if (firstNewline === -1 || /[^ \t]/.test(saved.slice(0, firstNewline))) return saved;
  let src = saved.slice(firstNewline + 1);
  const indent = /^[ \t]*/.exec(src)![0].length;
  if (indent === 0) return saved;
  let out = "\n";
  src = src.slice(indent);
  const nl = src.indexOf("\n");
  if (nl === -1) return saved;
  out += src.slice(0, nl + 1);
  src = src.slice(nl + 1);
  while (src.length) {
    const max = Math.min(src.length, indent);
    const lineIndent = /^[ \t]*/.exec(src.slice(0, max))![0].length;
    src = src.slice(lineIndent);
    if (lineIndent < max) {
      if (!src.length) break;
      if (src[0] === "\n") {
        out += "\n";
        src = src.slice(1);
        continue;
      }
      return saved;
    }
    const lineEnd = src.indexOf("\n");
    if (lineEnd === -1) {
      if (/[^ \t]/.test(src)) return saved;
      break;
    }
    out += src.slice(0, lineEnd + 1);
    src = src.slice(lineEnd + 1);
  }
  const last = saved.lastIndexOf("\n");
  if (/[^ \t]/.test(saved.slice(last + 1))) return saved;
  return out;
}

const CI_INLINE =
  "<b>Matcher error<r>: Inline snapshot creation is disabled in CI environments unless --update-snapshots is used.\nTo override, set the environment variable CI=false.\n\nReceived: {}";

export const hooks: SnapshotHooks = {
  toMatchSnapshot(m, received, propertyMatchers, hint) {
    const text = printed(m, valueToSnapshot(received), propertyMatchers);
    const snapshots = load();
    const name = nameOf(hint);
    const key = `${name} ${addCount(name)}`;
    const saved = snapshots.values.get(key);
    if (saved !== undefined && !native.options.updateSnapshots) {
      if (saved === text) {
        counts.passed++;
        return;
      }
      counts.failed++;
      m.withParams("<green>expected<r>").fail(diffStrings(text, saved) + "\n");
    }
    if (saved === undefined && native.isCI && !native.options.updateSnapshots)
      throw new Error(
        `Snapshot creation is disabled in CI environments unless --update-snapshots is used\nTo override, set the environment variable CI=false.\n\nSnapshot name: "${key}"\nReceived: ${text}`,
      );
    if (saved === undefined) counts.added++;
    else counts.passed++;
    snapshots.values.set(key, text);
    snapshots.text += `\nexports[\`${escape(key)}\`] = \`${escape(text)}\`;\n`;
    snapshots.dirty = true;
  },
  toMatchInlineSnapshot(m, received, propertyMatchers, saved) {
    const text = printed(m, valueToSnapshot(received), propertyMatchers);
    if (runtime.sequence) addCount(nameOf(""));
    if (saved !== undefined) {
      const trimmed = trimInlineSnapshot(saved);
      if (trimmed === text) {
        counts.passed++;
        return;
      }
      if (!native.options.updateSnapshots) {
        counts.failed++;
        m.withParams("<green>expected<r>").fail(diffStrings(text, trimmed) + "\n");
      }
      counts.passed++;
    } else {
      if (native.isCI && !native.options.updateSnapshots) m.withParams("").failf(CI_INLINE, text);
      counts.added++;
    }
    // `add_inline_snapshot_to_write`: where in the test file the matcher was called, written at the end of the file.
    const site = framesOf(String(new Error().stack ?? "")).find(frame => frame.path === runtime.path);
    if (site?.line === undefined || site.column === undefined)
      m.withParams("").failf(
        "<b>Matcher error<r>: Inline snapshot matchers must be called from the test file:\n  Expected to be called from file: <green>{}<r>\n",
        JSON.stringify(runtime.path),
      );
    append(inlineWrites, { line: site!.line!, column: site!.column!, kind: m.name, value: text, hasMatchers: propertyMatchers !== undefined });
  },
};

// ── write_inline_snapshots ──────────────────────────────────────────────────
interface InlineWrite {
  line: number;
  column: number;
  kind: string;
  value: string;
  hasMatchers: boolean;
}
const inlineWrites: InlineWrite[] = [];

/** Where the value that starts at `i` ends: a string, a template literal, or anything up to a `,` or `)` at depth 0. */
function skipValue(source: string, i: number): number {
  let depth = 0;
  while (i < source.length) {
    const c = source[i];
    if (c === '"' || c === "'") {
      i = skipQuoted(source, i, c);
      continue;
    }
    if (c === "`") {
      i = skipTemplate(source, i);
      continue;
    }
    if (c === "/" && source[i + 1] === "/") {
      while (i < source.length && source[i] !== "\n") i++;
      continue;
    }
    if (c === "/" && source[i + 1] === "*") {
      const end = source.indexOf("*/", i + 2);
      i = end === -1 ? source.length : end + 2;
      continue;
    }
    if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") {
      if (depth === 0) return i;
      depth--;
    } else if (c === "," && depth === 0) return i;
    i++;
  }
  return i;
}
function skipQuoted(source: string, i: number, quote: string): number {
  for (i++; i < source.length; i++) {
    if (source[i] === "\\") i++;
    else if (source[i] === quote) return i + 1;
  }
  return i;
}
function skipTemplate(source: string, i: number): number {
  for (i++; i < source.length; i++) {
    if (source[i] === "\\") i++;
    else if (source[i] === "`") return i + 1;
    else if (source[i] === "$" && source[i + 1] === "{") {
      i = skipValue(source, i + 2);
    }
  }
  return i;
}
const skipSpace = (source: string, i: number) => {
  while (i < source.length && /\s/.test(source[i])) i++;
  return i;
};

/** The template literal Bun writes: one line as it is, or the value's lines indented below the call's line. */
function literal(value: string, indent: string): string {
  if (!value.includes("\n")) return `\`${escape(value)}\``;
  const lines = value.replace(/^\n/, "").replace(/\n$/, "").split("\n");
  return `\`\n${lines.map(line => (line ? `${indent}  ${escape(line)}` : "")).join("\n")}\n${indent}\``;
}

function applyInlineWrites(source: string): string {
  const lineStarts = [0];
  for (let i = 0; i < source.length; i++) if (source[i] === "\n") append(lineStarts, i + 1);
  const edits: [start: number, end: number, text: string][] = [];
  for (const write of inlineWrites) {
    const lineStart = lineStarts[write.line - 1];
    if (lineStart === undefined) continue;
    const lineEnd = source.indexOf("\n", lineStart) === -1 ? source.length : source.indexOf("\n", lineStart);
    const at = lineStart + write.column - 1;
    const name = `${write.kind}(`;
    // The occurrence of the matcher's name on the call's line that is closest before its column, else the next one.
    let open = -1;
    for (let i = source.indexOf(name, lineStart); i !== -1 && i < lineEnd; i = source.indexOf(name, i + 1))
      if (i <= at || open === -1) open = i;
    if (open === -1) open = source.indexOf(name, at);
    if (open === -1) continue;
    const indent = /^[ \t]*/.exec(source.slice(lineStart))![0];
    let i = skipSpace(source, open + name.length);
    const text = literal(write.value, indent);
    if (write.hasMatchers) {
      i = skipValue(source, i);
      const next = skipSpace(source, i);
      if (source[next] === ",") {
        const start = skipSpace(source, next + 1);
        if (source[start] === ")") append(edits, [next + 1, next + 1, ` ${text}`]);
        else append(edits, [start, skipValue(source, start), text]);
      } else append(edits, [i, i, `, ${text}`]);
    } else if (source[i] === ")") append(edits, [i, i, text]);
    else append(edits, [i, skipValue(source, i), text]);
  }
  edits.sort((a, b) => b[0] - a[0]);
  let out = source;
  let last = Infinity;
  for (const [start, end, text] of edits) {
    // The same call twice (in a loop) is written once.
    if (end > last || start === last) continue;
    out = out.slice(0, start) + text + out.slice(end);
    last = start;
  }
  return out;
}

/** At the end of the file: the snapshots it added, written back. */
export function save() {
  if (inlineWrites.length) {
    const source = native.originalSource(runtime.path) ?? native.readFile(runtime.path);
    if (source !== undefined) {
      const written = applyInlineWrites(source);
      if (written !== source) native.writeFile(runtime.path, written);
    }
    inlineWrites.length = 0;
  }
  if (!file?.dirty) return;
  const dir = file.path.slice(0, file.path.lastIndexOf("/"));
  if (!native.writeFile(file.path, file.text)) throw new Error(`Failed to make snapshot directory for test file: ${dir}`);
}
