// Enough passing tests (more than 20) that bun test lists the failed ones again after the last file.
import { expect, test } from "bun:test";

test.each(Array.from({ length: 10 }, (_, i) => i))("passes %d", i => {
  expect(i).toBeGreaterThanOrEqual(0);
});
