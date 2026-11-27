// Positions in the file as it is written: Boa's stack names those of the code the loader printed, which its
// source map (test.rs's natives) maps back, as Bun remaps a stack through its saved source maps.
import { append, native } from "./state.ts";

export interface Frame {
  /** `at name (path:line:column)`, with the position mapped. */
  text: string;
  path?: string;
  line?: number;
  column?: number;
}

const LOCATION = /^(.*?)\(?((?:\/|[A-Za-z]:\\)[^()]*?):(\d+):(\d+)\)?\s*$/;

/** The frames of `stack`, whose positions are mapped where the loader has a source map of the file. */
export function framesOf(stack: string): Frame[] {
  const frames: Frame[] = [];
  for (const raw of stack.split("\n")) {
    const line = raw.trim();
    if (!line.startsWith("at ")) continue;
    const m = LOCATION.exec(line);
    if (!m) {
      append(frames, { text: line });
      continue;
    }
    const path = m[2];
    let row = Number(m[3]);
    let column = Number(m[4]);
    const mapped = native.originalPosition(path, row, column);
    if (mapped) [row, column] = mapped;
    const text = line.replace(`${path}:${m[3]}:${m[4]}`, `${path}:${row}:${column}`);
    append(frames, { text, path, line: row, column });
  }
  return frames;
}

/** The line of `frame` and the five before it, and a caret under its column, as Bun prints them above an error. */
export function sourcePreview(frame: Frame, paint: (text: string) => string): string {
  if (frame.path === undefined || frame.line === undefined || frame.column === undefined) return "";
  const first = Math.max(1, frame.line - 5);
  const lines = native.originalLines(frame.path, first, frame.line);
  if (lines === undefined || lines.length !== frame.line - first + 1) return "";
  const width = String(frame.line).length;
  let out = "";
  lines.forEach((line, i) => {
    out += paint(`<r><d>${String(first + i).padStart(width)} | <r>`) + line + "\n";
  });
  out += " ".repeat(width + 3 + Math.max(0, frame.column - 1)) + paint("<r><red>^<r>") + "\n";
  return out;
}
