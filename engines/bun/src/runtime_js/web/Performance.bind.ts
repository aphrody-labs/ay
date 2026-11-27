// The clock of `performance` (src/jsc/bindings/webcore/Performance.cpp over Bun__readOriginTimer and
// Bun__readOriginTimerStart). packages/bun-boa/src/js/performance.ts builds the Performance object model on it.
import * as b from "../../codegen/bindgen-lib.ts";

export default b.module({ rustPath: "crate::web::performance" });

export const performanceClock = b.namespace({
  /** `performance.now()`: milliseconds since the time origin, from a monotonic clock. */
  now: { returns: b.f64 },
  /** `performance.timeOrigin`: the Unix time of the time origin, in milliseconds. */
  timeOrigin: { returns: b.f64 },
});
