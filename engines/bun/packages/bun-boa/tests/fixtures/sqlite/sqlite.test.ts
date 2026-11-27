// bun:sqlite: src/js/bun/sqlite.ts as is, on bun_sqlite_js over Turso.
import { Database, SQLiteError, constants } from "bun:sqlite";
import { expect, test } from "bun:test";

test("run, query and the changes of a run", () => {
  const db = new Database(":memory:");
  db.exec("CREATE TABLE cats (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE, age INTEGER)");
  expect(db.run("INSERT INTO cats (name, age) VALUES (?, ?)", ["Joey", 2])).toEqual({ changes: 1, lastInsertRowid: 1 });
  const insert = db.prepare("INSERT INTO cats (name, age) VALUES ($name, $age)");
  expect(insert.run({ $name: "Sally", $age: 4 })).toEqual({ changes: 1, lastInsertRowid: 2 });
  expect(db.query("SELECT name, age FROM cats ORDER BY id").all()).toEqual([
    { name: "Joey", age: 2 },
    { name: "Sally", age: 4 },
  ]);
  expect(db.query("SELECT name FROM cats WHERE age > ?").get(3)).toEqual({ name: "Sally" });
  expect(db.query("SELECT name FROM cats WHERE age > ?").get(10)).toBeNull();
  expect(db.query("SELECT id, name FROM cats ORDER BY id").values()).toEqual([
    [1, "Joey"],
    [2, "Sally"],
  ]);
  expect([...db.query("SELECT age FROM cats ORDER BY id").iterate()].map(row => row.age)).toEqual([2, 4]);
  expect(db.query("SELECT ?1 AS a").paramsCount).toBe(1);
});

test("transactions roll back on a SQLiteError", () => {
  const db = new Database(":memory:", { strict: true });
  db.run("CREATE TABLE cats (name TEXT UNIQUE)");
  const insert = db.prepare("INSERT INTO cats (name) VALUES ($name)");
  const insertMany = db.transaction(names => {
    for (const name of names) insert.run({ name });
  });
  let error;
  try {
    insertMany(["a", "b", "a"]);
  } catch (e) {
    error = e;
  }
  expect(error).toBeInstanceOf(SQLiteError);
  expect(error.code).toBe("SQLITE_CONSTRAINT_UNIQUE");
  expect(error.errno).toBe(2067);
  expect(db.inTransaction).toBe(false);
  expect(db.query("SELECT count(*) AS n FROM cats").get()).toEqual({ n: 0 });
  insertMany(["a", "b"]);
  expect(db.query("SELECT count(*) AS n FROM cats").get()).toEqual({ n: 2 });
});

test("safeIntegers, blobs and column types", () => {
  const db = new Database(":memory:", { safeIntegers: true });
  db.run("CREATE TABLE t (big INTEGER, data BLOB, x REAL)");
  db.run("INSERT INTO t VALUES (?, ?, ?)", [2n ** 62n, new Uint8Array([7, 8]), 1.5]);
  const row = db.query("SELECT * FROM t").get();
  expect(row.big).toBe(2n ** 62n);
  expect(row.data).toEqual(new Uint8Array([7, 8]));
  const stmt = db.prepare("SELECT * FROM t");
  expect(stmt.columnNames).toEqual(["big", "data", "x"]);
  expect(stmt.declaredTypes).toEqual(["INTEGER", "BLOB", "REAL"]);
  expect(stmt.columnTypes).toEqual(["INTEGER", "BLOB", "FLOAT"]);
  expect(() => db.query("SELECT ?").get(2n ** 64n)).toThrow("BigInt value '18446744073709551616' is out of range");
});

test("serialize and deserialize", () => {
  const db = new Database(":memory:");
  db.run("CREATE TABLE t (x TEXT)");
  db.run("INSERT INTO t VALUES ('kept')");
  const copy = Database.deserialize(db.serialize());
  expect(copy.query("SELECT x FROM t").all()).toEqual([{ x: "kept" }]);
});

test("Bun.SQL on the sqlite adapter", async () => {
  const sql = new Bun.SQL({ adapter: "sqlite", filename: ":memory:" });
  expect(sql.options.adapter).toBe("sqlite");
  await sql`CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)`;
  await sql`INSERT INTO t ${sql([{ name: "a" }, { name: "b" }])}`;
  expect([...(await sql`SELECT name FROM t WHERE id = ${2}`)]).toEqual([{ name: "b" }]);
  const rows = await sql.begin(async tx => {
    await tx`INSERT INTO t (name) VALUES (${"c"})`;
    return tx`SELECT count(*) AS n FROM t`;
  });
  expect(rows[0].n).toBe(3);
  await sql.close();
});

test("what Turso cannot do says so", () => {
  const db = new Database(":memory:");
  expect(() => db.loadExtension("nope")).toThrow("This build of sqlite3 does not support dynamic extension loading");
  expect(db.fileControl(constants.SQLITE_FCNTL_PERSIST_WAL, 0)).toBe(12);
  expect(Database.setCustomSQLite("/nope")).toBe(false);
  db.close();
  expect(() => db.query("SELECT 1")).toThrow("Cannot use a closed database");
});
