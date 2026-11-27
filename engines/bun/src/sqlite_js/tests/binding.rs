//! The native side of bun:sqlite, called as src/js/bun/sqlite.ts calls it: `SQL` and the changes tuple of
//! `create_constructor`, and `field(tuple, i)` for `$getInternalField`.

use bun_js::{CallFrame, JSFunction, JSGlobalObject, JSValue, JsResult};
use bun_sema_standalone as _;

fn field(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let Some(object) = frame.argument(0).get_object() else {
        return Ok(JSValue::UNDEFINED);
    };
    object.get_internal_field(global, frame.argument(1).to_u32())
}

/// Runs `body` with `SQL`, `tuple` and `field` in scope; its result is JSON-stringified.
fn run(body: &str) -> String {
    JSGlobalObject::with_new(|global| {
        let exports = bun_sqlite_js::create_constructor(global).unwrap();
        let this = global.to_js_value();
        this.put(global, "exports", exports);
        this.put(global, "field", JSFunction::create(global, "field", field, 2));
        let source = format!(
            "(() => {{ const SQL = exports[0], tuple = exports[1]; \
             const changes = () => [field(tuple, 0), field(tuple, 1)]; \
             const error = f => {{ try {{ f(); return 'no error'; }} catch (e) {{ return {{ name: e.name, message: e.message, code: e.code, errno: e.errno, byteOffset: e.byteOffset }}; }} }}; \
             return JSON.stringify((() => {{ {body} }})(), (k, v) => typeof v === 'bigint' ? v + 'n' : v instanceof Uint8Array ? [...v] : v); }})()"
        );
        match global.evaluate_script(&source) {
            Ok(value) => value.as_string().unwrap_or_else(|| "undefined".into()),
            Err(error) => panic!(
                "threw {:?}",
                global.take_exception(error).to_utf8(global)
            ),
        }
    })
}

#[test]
fn runs_and_reads_rows() {
    let out = run(r#"
        const db = SQL.open(":memory:", 6);
        SQL.run(db, 0, tuple, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, data BLOB)", []);
        SQL.run(db, 0, tuple, "INSERT INTO t (name, score, data) VALUES (?, ?, ?)", ["a", 1.5, new Uint8Array([1, 2, 3])]);
        const inserted = changes();
        SQL.run(db, 0, tuple, "INSERT INTO t (name, score, data) VALUES ($name, $score, NULL); INSERT INTO t (name) VALUES ('c')", [{ $name: "b", score: 2 }]);
        const s = SQL.prepare(db, "SELECT id, name, score, data FROM t WHERE id >= ? ORDER BY id", undefined, 0, 0);
        return {
            inserted,
            twoStatements: changes(),
            params: s.paramsCount,
            columns: s.columns,
            declared: s.declaredTypes,
            all: s.all([1]),
            get: s.get([3]),
            values: s.values([2]),
            raw: s.raw([1]).map(row => row[0]),
            types: s.columnTypes,
            text: s.toString(),
        };
    "#);
    assert_eq!(
        out,
        r#"{"inserted":[1,1],"twoStatements":[2,3],"params":1,"columns":["id","name","score","data"],"declared":["INTEGER","TEXT","REAL","BLOB"],"all":[{"id":1,"name":"a","score":1.5,"data":[1,2,3]},{"id":2,"name":"b","score":2,"data":null},{"id":3,"name":"c","score":null,"data":null}],"get":{"id":3,"name":"c","score":null,"data":null},"values":[[2,"b",2,null],[3,"c",null,null]],"raw":[[1,0,0,0,0,0,0,0],[2,0,0,0,0,0,0,0],[3,0,0,0,0,0,0,0]],"types":["INTEGER","TEXT","FLOAT","BLOB"],"text":"SELECT id, name, score, data FROM t WHERE id >= 1 ORDER BY id"}"#
    );
}

#[test]
fn binds_by_bun_rules() {
    let out = run(r#"
        const db = SQL.open(":memory:", 6);
        const strict = 1 << 2, safe = 1 << 1;
        const q = (sql, flags) => SQL.prepare(db, sql, undefined, 0, flags);
        return {
            prefixed: q("SELECT $a AS a, :b AS b, @c AS c", 0).get([{ $a: 1, ":b": 2, "@c": 3 }]),
            bare: q("SELECT $a AS a, :b AS b", 0).get([{ a: 1, b: 2 }]),
            strict: q("SELECT $a AS a", strict).get([{ a: 1, $a: 2 }]),
            missing: error(() => q("SELECT $a AS a, $b AS b", strict).get([{ a: 1 }])),
            numbered: q("SELECT ?2 AS two, ?1 AS one", 0).get([10, 20]),
            positional: q("SELECT ? AS x, ? AS y", 0).get([[1, 2]]),
            count: error(() => q("SELECT ? AS x", 0).get([1, 2])),
            unsupported: error(() => q("SELECT ? AS x", 0).get([Symbol()])),
            bigint: q("SELECT ? AS x, 9007199254740993 AS big", safe).get([2n ** 63n - 1n]),
            wrapped: q("SELECT ? AS x", 0).get([2n ** 64n + 5n]),
            outOfRange: error(() => q("SELECT ? AS x", safe).get([2n ** 63n])),
            negativeZero: q("SELECT typeof(?) AS t", 0).get([-0]),
            booleans: q("SELECT ? AS t, ? AS f", 0).get([true, false]),
            notObject: error(() => q("SELECT ? AS x", 0).get(5)),
            keepsBindings: (() => { const s = q("SELECT ? AS x", 0); s.get([7]); return s.get([]); })(),
            prepareIgnoresIndex: SQL.prepare(db, "SELECT 1 AS x", [3], 0, 0).get(),
        };
    "#);
    assert_eq!(
        out,
        r#"{"prefixed":{"a":1,"b":2,"c":3},"bare":{"a":1,"b":2},"strict":{"a":1},"missing":{"name":"Error","message":"Missing parameter \"b\""},"numbered":{"two":20,"one":10},"positional":{"x":1,"y":2},"count":{"name":"Error","message":"SQLite query expected 1 values, received 2"},"unsupported":{"name":"TypeError","message":"Binding expected string, TypedArray, boolean, number, bigint or null"},"bigint":{"x":"9223372036854775807n","big":"9007199254740993n"},"wrapped":{"x":5},"outOfRange":{"name":"RangeError","message":"BigInt value '9223372036854775808' is out of range"},"negativeZero":{"t":"real"},"booleans":{"t":1,"f":0},"notObject":{"name":"TypeError","message":"Expected object or array"},"keepsBindings":{"x":7},"prepareIgnoresIndex":{"x":1}}"#
    );
}

#[test]
fn errors_are_sqlite_errors() {
    let out = run(r#"
        const db = SQL.open(":memory:", 6);
        SQL.run(db, 0, tuple, "CREATE TABLE u (name TEXT UNIQUE)", []);
        SQL.run(db, 0, tuple, "INSERT INTO u VALUES ('x')", []);
        return {
            unique: error(() => SQL.run(db, 0, tuple, "INSERT INTO u VALUES ('x')", [])),
            syntax: error(() => SQL.prepare(db, "SELECT * FROM foo!!", undefined, 0, 0)),
            table: error(() => SQL.prepare(db, "SELECT * FROM nope", undefined, 0, 0)).code,
            empty: error(() => SQL.prepare(db, "  -- nothing", undefined, 0, 0)),
            cantOpen: error(() => SQL.open("/definitely/not/found/x.db", 6)).code,
            handle: error(() => SQL.prepare({}, "SELECT 1", undefined, 0, 0)).message,
            extension: error(() => SQL.loadExtension(db, "x")).message,
            fcntl: SQL.fcntl(db, null, 10, 0),
            custom: SQL.setCustomSQLite("/x"),
            hook: error(() => SQL.setUpdateHook(db, 18, () => {}, {}, 0)).name,
            noHook: SQL.updateHook(db, 18),
        };
    "#);
    assert_eq!(
        out,
        r#"{"unique":{"name":"SQLiteError","message":"UNIQUE constraint failed: u.name","code":"SQLITE_CONSTRAINT_UNIQUE","errno":2067,"byteOffset":-1},"syntax":{"name":"SQLiteError","message":"unrecognized token: \"!\"","code":"SQLITE_ERROR","errno":1,"byteOffset":17},"table":"SQLITE_ERROR","empty":{"name":"RangeError","message":"Invalid SQL statement"},"cantOpen":"SQLITE_CANTOPEN","handle":"Invalid database handle","extension":"This build of sqlite3 does not support dynamic extension loading","fcntl":12,"custom":false,"hook":"Error","noHook":null}"#
    );
}

#[test]
fn statements_iterate_finalize_and_close() {
    let out = run(r#"
        const db = SQL.open(":memory:", 6);
        SQL.run(db, 0, tuple, "CREATE TABLE t (x INTEGER)", []);
        SQL.run(db, 0, tuple, "INSERT INTO t VALUES (1), (2), (3)", []);
        const s = SQL.prepare(db, "SELECT x FROM t ORDER BY x", undefined, 0, 0);
        const seen = [];
        let row = s.iterate();
        const run = s.runCount;
        for (; row; row = s.iterateNext(run)) seen.push(row.x);
        s.iterateEnd(run);
        class Row { get double() { return this.x * 2; } }
        s.as(Row);
        const typed = s.get();
        const owned = SQL.prepare(db, "SELECT 1", undefined, 0, 1 << 3);
        const kept = SQL.prepare(db, "SELECT x FROM t", undefined, 0, 0);
        const inTransaction = (SQL.run(db, 0, tuple, "BEGIN", []), SQL.isInTransaction(db));
        SQL.run(db, 0, tuple, "COMMIT", []);
        SQL.close(db, false);
        const afterSoftClose = { owned: owned.isFinalized, kept: kept.isFinalized, rows: kept.all().length };
        const closedPrepare = error(() => SQL.prepare(db, "SELECT 1", undefined, 0, 0));
        SQL.close(db, true);
        return {
            seen, double: typed.double, isRow: typed instanceof Row,
            inTransaction, afterSoftClose, closedPrepare,
            afterClose: error(() => kept.all()).message,
            finalize: (() => { const db2 = SQL.open(":memory:", 6); const f = SQL.prepare(db2, "SELECT 1", undefined, 0, 0); f.finalize(); return [f.isFinalized, error(() => f.get()).message]; })(),
        };
    "#);
    assert_eq!(
        out,
        r#"{"seen":[1,2,3],"double":2,"isRow":true,"inTransaction":true,"afterSoftClose":{"owned":true,"kept":false,"rows":3},"closedPrepare":{"name":"RangeError","message":"Cannot use a closed database"},"afterClose":"Database has closed","finalize":[true,"Statement has finalized"]}"#
    );
}

#[test]
fn serializes_and_files() {
    let dir = std::env::temp_dir().join(format!("bun-sqlite-js-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("file.db");
    let _ = std::fs::remove_file(&file);
    let out = run(&format!(
        r#"
        const db = SQL.open(":memory:", 6);
        SQL.run(db, 0, tuple, "CREATE TABLE t (x TEXT)", []);
        SQL.run(db, 0, tuple, "INSERT INTO t VALUES ('kept')", []);
        const bytes = SQL.serialize(db, "main");
        const copy = SQL.deserialize(bytes, 0);
        const fromCopy = SQL.prepare(copy, "SELECT x FROM t", undefined, 0, 0).all();
        const readonly = SQL.deserialize(bytes, 4);
        const write = error(() => SQL.run(readonly, 0, tuple, "INSERT INTO t VALUES ('no')", [])).code;
        const path = {path:?};
        const a = SQL.open(path, 6);
        SQL.run(a, 0, tuple, "CREATE TABLE f (x INTEGER)", []);
        SQL.run(a, 0, tuple, "INSERT INTO f VALUES (42)", []);
        SQL.close(a, true);
        const b = SQL.open(path, 1);
        const fromFile = SQL.prepare(b, "SELECT x FROM f", undefined, 0, 0).get();
        const readonlyFile = error(() => SQL.run(b, 0, tuple, "INSERT INTO f VALUES (1)", [])).code;
        SQL.close(b, true);
        return {{ header: String.fromCharCode(...bytes.slice(0, 15)), fromCopy, write, fromFile, readonlyFile }};
    "#,
        path = file.to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out,
        r#"{"header":"SQLite format 3","fromCopy":[{"x":"kept"}],"write":"SQLITE_READONLY","fromFile":{"x":42},"readonlyFile":"SQLITE_READONLY"}"#
    );
}

#[test]
fn calls_user_functions_and_aggregates() {
    let out = run(r#"
        const db = SQL.open(":memory:", 6);
        SQL.defineFunction(db, "twice", 1, 0, false, x => x * 2);
        SQL.defineFunction(db, "big", 0, 0, true, () => 2n ** 40n);
        SQL.defineFunction(db, "boom", 0, 0, false, () => { throw new RangeError("from js"); });
        SQL.defineFunction(db, "bad", 0, 0, false, () => ({}));
        SQL.defineFunction(db, "reenter", 0, 0, false, () => SQL.isInTransaction(db));
        SQL.defineFunction(db, "sum_from_ten", 1, 0, false, 10, (acc, x) => acc + x, null, acc => acc * 100);
        SQL.defineFunction(db, "count_calls", 1, 0, false, () => 0, (acc, x) => acc + 1, null, null);
        SQL.run(db, 0, tuple, "CREATE TABLE t (x INTEGER)", []);
        SQL.run(db, 0, tuple, "INSERT INTO t VALUES (1), (2), (3)", []);
        const one = q => SQL.prepare(db, q, undefined, 0, 0).values()[0][0];
        return {
            twice: one("SELECT twice(21)"),
            big: typeof one("SELECT big()"),
            boom: error(() => one("SELECT boom()")),
            bad: error(() => one("SELECT bad()")).message,
            reenter: error(() => one("SELECT reenter()")).message,
            total: one("SELECT sum_from_ten(x) FROM t"),
            empty: one("SELECT sum_from_ten(x) FROM t WHERE x > 5"),
            counted: one("SELECT count_calls(x) FROM t"),
            afterThrow: one("SELECT twice(2)"),
            tooMany: error(() => SQL.defineFunction(db, "f", 200, 0, false, () => 1)).message,
        };
    "#);
    assert_eq!(
        out,
        r#"{"twice":42,"big":"number","boom":{"name":"RangeError","message":"from js"},"bad":"User-defined function bad() returned an invalid value","reenter":"This database connection is busy executing a query","total":1600,"empty":1000,"counted":3,"afterThrow":4,"tooMany":"bad parameter or other API misuse"}"#
    );
}
