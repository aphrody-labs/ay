import path, { sep } from "node:path";
import * as util from "util/types";
import { mapped } from "mapped";
import counter from "lib/counter.cjs";
import { next } from "./lib/counter.cjs";
import { strictEqual } from "node:assert";
import { rewritten } from "./lib/rewritten.js";
import { sleepSync, unsafe, which } from "bun";

console.log(typeof path.join, sep, typeof util.isPromise, util.isPromise(Promise.resolve()), util.isMap(new Map()));
console.log(mapped, counter.next(), next(), counter.file);
console.log(
  require("node:path") === path,
  require("./lib/counter.cjs").next(),
  __dirname.split("/").pop(),
  counter.sep,
);
strictEqual(1, 1);
let using_ran = false;
{
  using _ = { [Symbol.dispose]: () => (using_ran = true) };
}
console.log("using", using_ran);
console.log("rewritten", rewritten);
console.log(
  "process",
  typeof process.umask(),
  process.hrtime().length,
  typeof process.hrtime.bigint(),
  process.memoryUsage.rss() > 0,
  Object.keys(process.cpuUsage()).join(),
  typeof process.resourceUsage().maxRSS,
  process.execPath.length > 0,
  typeof process.report.getReport().header,
);
console.log(
  "bun",
  typeof sleepSync,
  which("sh", { PATH: "/bin" }),
  typeof unsafe.gcAggressionLevel(),
  typeof DOMException,
  AbortSignal.abort().reason.name,
);
