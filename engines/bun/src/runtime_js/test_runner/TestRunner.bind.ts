import * as b from "../../codegen/bindgen-lib.ts";

export default b.module({ rustPath: "crate::test_runner::TestRunner" });

export const testRunner = b.namespace({
  // The body of a failure of toEqual() and the other matchers that diff (diff_format.rs): received and expected as
  // pretty-format printed them.
  diff: {
    args: { not: b.bool, received: b.string.utf8, expected: b.string.utf8, colors: b.bool },
    returns: b.string.utf8,
  },
});
