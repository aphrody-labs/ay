// No import: describe/test/expect are globals in a test file, as in Bun.
describe("globals", () => {
  it("works", done => {
    setTimeout(() => {
      expect(typeof Bun.version).toBe("string");
      done();
    }, 1);
  });
});
