//! node:fs's native binding on bun_js, called as src/js/node/fs.ts and fs.promises.ts call it: `fs.xSync(...)`,
//! `fs.x(...)` (a promise) and `fs.xCb(callback, ...)`, in a temporary directory.

use bun_js::JSGlobalObject;
use bun_runtime_js::EventLoop;
use bun_sema_standalone as _;

/// A realm on bun_runtime_js's event loop, in a process whose stdio is set up as Bun's `main` sets it up: the
/// threads of the work pool configure their output from it.
fn on_event_loop<R>(f: impl FnOnce(&JSGlobalObject) -> R) -> R {
    bun_core::output::stdio::init();
    let mut context = boa_engine::Context::builder()
        .job_executor(EventLoop::new().expect("an event loop"))
        .build()
        .expect("a realm");
    boa_runtime::abort::register(None, &mut context).expect("AbortController");
    bun_runtime_js::install(&mut context).expect("install");
    f(JSGlobalObject::from_context(&mut context))
}

/// Runs the loop of `global`'s realm until nothing is left.
fn run_loop(global: &JSGlobalObject) {
    // SAFETY: nothing else reaches the realm while its loop runs, but what the loop calls.
    let context = unsafe { global.context() };
    if let Err(error) = EventLoop::of(context).run(context) {
        let error = global.throw_engine_error(error);
        panic!("the loop threw {:?}", global.take_exception(error).to_utf8(global));
    }
}

/// Runs `body` with `fs` (the binding) and `dir` (a new directory) in scope, then the event loop, and returns what
/// `out` (a global the body sets) is as JSON.
fn run(body: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "bun-runtime-js-fs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir = dir.to_str().unwrap().to_owned();
    on_event_loop(|global| {
        let binding = bun_runtime_js::generated_bindgen::native(global, "node_fs_binding.bind.ts", "binding")
            .expect("the schema exports binding")
            .unwrap();
        let this = global.to_js_value();
        this.put(global, "fs", binding);
        this.put(global, "dir", bun_js::JSValue::from_utf8(dir.as_bytes()));
        let source = format!(
            "var out; fs.mkdirSync(dir, {{ recursive: true }}); try {{ {body} }} catch (e) {{ out = 'threw ' + (e.code ?? e.name) + ': ' + e.message; }}"
        );
        if let Err(error) = global.evaluate_script(&source) {
            panic!("{body}: threw {:?}", global.take_exception(error).to_utf8(global));
        }
        run_loop(global);
        let out = global.evaluate_script("JSON.stringify(out)").unwrap();
        let _ = global.evaluate_script("fs.rmSync(dir, { recursive: true, force: true })");
        out.to_utf8(global).unwrap()
    })
}

#[test]
fn write_then_read_a_file() {
    assert_eq!(
        run(r#"fs.writeFileSync(dir + "/a.txt", "héllo"); out = [fs.readFileSync(dir + "/a.txt", "utf8"), fs.readFileSync(dir + "/a.txt", { encoding: "hex" }), Array.from(fs.readFileSync(dir + "/a.txt")).length];"#),
        r#"["héllo","68c3a96c6c6f",6]"#
    );
}

#[test]
fn append_and_exists() {
    assert_eq!(
        run(r#"const f = dir + "/b"; fs.appendFileSync(f, "a"); fs.appendFileSync(f, new Uint8Array([98])); out = [fs.readFileSync(f, "latin1"), fs.existsSync(f), fs.existsSync(f + "x"), fs.existsSync(undefined)];"#),
        r#"["ab",true,false,false]"#
    );
}

#[test]
fn errors_are_node_system_errors() {
    assert_eq!(
        run(r#"try { fs.readFileSync(dir + "/missing") } catch (e) { out = [e.code, e.syscall, e.errno, e.path === dir + "/missing", e.message === "ENOENT: no such file or directory, open '" + dir + "/missing'"]; }"#),
        r#"["ENOENT","open",-2,true,true]"#
    );
}

#[test]
fn stat_and_its_classes() {
    assert_eq!(
        run(r#"fs.writeFileSync(dir + "/s", "12345"); const s = fs.statSync(dir + "/s"); const b = fs.statSync(dir + "/s", { bigint: true });
        out = [s instanceof fs.Stats, s.size, s.isFile(), s.isDirectory(), typeof s.mtimeMs, s.mtime instanceof Date, typeof b.size, b.isFile(), Object.keys(s).slice(0, 3),
               fs.statSync(dir + "/none", { throwIfNoEntry: false }), fs.lstatSync(dir).isDirectory()];"#),
        r#"[true,5,true,false,"number",true,"bigint",true,["dev","ino","mode"],null,true]"#
    );
}

#[test]
fn mkdir_recursive_returns_the_first_directory_it_made() {
    assert_eq!(
        run(r#"out = [fs.mkdirSync(dir + "/x/y/z", { recursive: true }) === dir + "/x", fs.mkdirSync(dir + "/x/y", { recursive: true })];
        try { fs.mkdirSync(dir + "/x") } catch (e) { out.push(e.code) }"#),
        r#"[true,null,"EEXIST"]"#
    );
}

#[test]
fn readdir_names_dirents_and_recursive() {
    assert_eq!(
        run(r#"fs.mkdirSync(dir + "/d/e", { recursive: true }); fs.writeFileSync(dir + "/d/f", ""); fs.writeFileSync(dir + "/d/e/g", "");
        const dirents = fs.readdirSync(dir + "/d", { withFileTypes: true }).sort((a, b) => a.name < b.name ? -1 : 1);
        out = [fs.readdirSync(dir + "/d").sort(), dirents.map(d => [d.name, d.isDirectory(), d.isFile(), d.parentPath === dir + "/d"]),
               fs.readdirSync(dir + "/d", { recursive: true }).sort(), dirents[0] instanceof fs.Dirent];"#),
        r#"[["e","f"],[["e",true,false,true],["f",false,true,true]],["e","e/g","f"],true]"#
    );
}

#[test]
fn rm_rmdir_rename_unlink() {
    assert_eq!(
        run(r#"fs.mkdirSync(dir + "/r/s", { recursive: true }); fs.writeFileSync(dir + "/r/s/t", "");
        out = [];
        try { fs.rmdirSync(dir + "/r") } catch (e) { out.push(e.code) }
        try { fs.rmSync(dir + "/r") } catch (e) { out.push(e.code) }
        fs.rmSync(dir + "/nothing", { force: true });
        fs.renameSync(dir + "/r/s/t", dir + "/r/u"); fs.unlinkSync(dir + "/r/u");
        fs.rmSync(dir + "/r", { recursive: true }); out.push(fs.existsSync(dir + "/r"));
        try { fs.rmSync(dir + "/r", { recursive: "yes" }) } catch (e) { out.push(e.code) }"#),
        r#"["ENOTEMPTY","EISDIR",false,"ERR_INVALID_ARG_TYPE"]"#
    );
}

#[test]
fn fds_open_read_write_close() {
    assert_eq!(
        run(r#"const fd = fs.openSync(dir + "/fd", "w+"); out = [fs.writeSync(fd, "abcdef"), fs.writeSync(fd, new Uint8Array([120, 121]), 0, 2, 0)];
        const buf = new Uint8Array(4); out.push(fs.readSync(fd, buf, 0, 4, 1), String.fromCharCode(...buf));
        out.push(fs.fstatSync(fd).size); fs.ftruncateSync(fd, 3); out.push(fs.fstatSync(fd).size); fs.closeSync(fd);
        try { fs.closeSync(-1) } catch (e) { out.push(e.code) }"#),
        r#"[6,2,4,"ycde",6,3,"ERR_OUT_OF_RANGE"]"#
    );
}

#[test]
fn mkdtemp_copy_file_realpath_symlink() {
    assert_eq!(
        run(r#"const t = fs.mkdtempSync(dir + "/tmp-"); fs.writeFileSync(t + "/a", "x"); fs.copyFileSync(t + "/a", t + "/b");
        out = [t.startsWith(dir + "/tmp-") && t.length === dir.length + 11, fs.readFileSync(t + "/b", "utf8")];
        try { fs.copyFileSync(t + "/a", t + "/b", 1) } catch (e) { out.push(e.code) }
        fs.symlinkSync(t + "/a", t + "/l"); out.push(fs.readlinkSync(t + "/l") === t + "/a", fs.realpathSync(t + "/l") === fs.realpathSync(t + "/a"), fs.lstatSync(t + "/l").isSymbolicLink());"#),
        r#"[true,"x","EEXIST",true,true,true]"#
    );
}

#[test]
fn arguments_are_validated_as_node_does() {
    assert_eq!(
        run(r#"out = [];
        for (const f of [() => fs.readFileSync(), () => fs.statSync(1), () => fs.openSync(dir + "/x", "nope"), () => fs.mkdirSync(dir + "/m", { mode: "9" }), () => fs.readFileSync(dir + "/a\0b"), () => fs.accessSync(dir, 8), () => fs.mkdirSync(dir + "/r", { recursive: null }), () => fs.statSync(dir, { bigint: 1 })])
          try { f(); out.push("ok") } catch (e) { out.push(e.code) }"#),
        r#"["ERR_INVALID_ARG_TYPE","ERR_INVALID_ARG_TYPE","ERR_INVALID_ARG_VALUE","ERR_INVALID_ARG_VALUE","ERR_INVALID_ARG_VALUE","ERR_OUT_OF_RANGE","ERR_INVALID_ARG_TYPE","ERR_INVALID_ARG_TYPE"]"#
    );
}

#[test]
fn promises_resolve_on_the_event_loop() {
    assert_eq!(
        run(r#"out = [];
        fs.writeFile(dir + "/p", "promised").then(() => fs.readFile(dir + "/p", "utf8")).then(text => out.push(text))
          .then(() => fs.stat(dir + "/p")).then(s => out.push(s.size))
          .then(() => fs.readdir(dir)).then(names => out.push(names))
          .then(() => fs.access(dir + "/nope")).catch(e => out.push(e.code));"#),
        r#"["promised",8,["p"],"ENOENT"]"#
    );
}

#[test]
fn callbacks_get_the_error_or_null_and_the_result() {
    assert_eq!(
        run(r#"const got = {}; let left = 2; const done = () => { if (--left === 0) out = [got.sync, got.read, got.stat] };
        fs.writeFileSync(dir + "/c", "cb");
        fs.readFileCb((err, data) => { got.read = [err, data.length]; done() }, dir + "/c");
        fs.statCb((err, s) => { got.stat = [err && err.code, s]; done() }, dir + "/none");
        try { fs.statCb(null, dir) } catch (e) { got.sync = e.code }"#),
        r#"["ERR_INVALID_ARG_TYPE",[null,2],["ENOENT",null]]"#
    );
}

#[test]
fn async_read_fills_the_buffer_when_it_completes() {
    assert_eq!(
        run(r#"fs.writeFileSync(dir + "/r", "0123456789"); const fd = fs.openSync(dir + "/r", "r"); const buf = new Uint8Array(4); out = [];
        fs.read(fd, buf, 1, 3, 5).then(n => { out.push(n, Array.from(buf)); fs.closeSync(fd); });"#),
        r#"[3,[0,53,54,55]]"#
    );
}

#[test]
fn utimes_chmod_truncate() {
    assert_eq!(
        run(r#"const f = dir + "/u"; fs.writeFileSync(f, "abcdef"); fs.utimesSync(f, 1000, new Date(2000000)); const s = fs.statSync(f);
        fs.chmodSync(f, 0o600); fs.truncateSync(f, 2);
        out = [s.atimeMs, s.mtimeMs, fs.statSync(f).mode & 0o777, fs.readFileSync(f, "utf8")];"#),
        r#"[1000000,2000000,384,"ab"]"#
    );
}

#[test]
fn rm_options_and_flags_for_testing() {
    JSGlobalObject::with_new(|global| {
        let native = |symbol| {
            bun_runtime_js::generated_bindgen::native(global, "node_fs_binding.bind.ts", symbol)
                .unwrap()
                .unwrap()
        };
        let this = global.to_js_value();
        this.put(global, "rmOptions", native("rmOptionsForTesting"));
        this.put(global, "flags", native("stringToFlagsForTesting"));
        let out = global
            .evaluate_script(r#"JSON.stringify([rmOptions("/x", { recursive: true }), flags("a+"), flags(undefined)])"#)
            .unwrap();
        let expected = format!(
            r#"[{{"retryDelay":100,"maxRetries":0,"recursive":true,"force":false}},{},0]"#,
            libc::O_APPEND | libc::O_CREAT | libc::O_RDWR
        );
        assert_eq!(out.to_utf8(global).unwrap(), expected);
    });
}

/// test/js/node/fs/fs-stats-truncate.test.ts: an ino above `i64::MAX` is a number as `u64 as f64` is, and a BigInt
/// as `u64 as i64` is.
#[test]
fn create_stats_for_ino() {
    JSGlobalObject::with_new(|global| {
        let create = bun_runtime_js::generated_bindgen::native(global, "Stat.bind.ts", "createStatsForIno")
            .unwrap()
            .unwrap();
        global.to_js_value().put(global, "createStatsForIno", create);
        let out = global
            .evaluate_script(
                r#"const cases = [0n, 1n, (1n << 53n) - 1n, (1n << 63n) - 1n, 1n << 63n, 9225185599684229422n, (1n << 64n) - 1n];
                JSON.stringify([
                  cases.every(ino => createStatsForIno(ino, false).ino === Number(ino)),
                  cases.every(ino => createStatsForIno(ino, true).ino === BigInt.asIntN(64, ino)),
                  createStatsForIno(2 ** 64, false).ino === 2 ** 64,
                  createStatsForIno(-1, true).ino === -1n,
                  createStatsForIno(7, false).size,
                  (() => { try { createStatsForIno("1", false) } catch (e) { return e.code } })(),
                ])"#,
            )
            .unwrap();
        assert_eq!(out.to_utf8(global).unwrap(), r#"[true,true,true,true,0,"ERR_INVALID_ARG_TYPE"]"#);
    });
}

/// `fs.watch` on inotify: the first event, then `close`, after which nothing is delivered.
#[test]
fn watch_reports_a_change_then_closes() {
    assert_eq!(
        run(r#"const f = dir + "/w.txt"; fs.writeFileSync(f, "a"); out = [];
        const w = fs.watch(dir, {}, (type, name) => { out.push(type === "close" ? type : type + ":" + name); if (out.length === 1) w.close(); });
        fs.writeFileSync(f, "b");"#),
        r#"["change:w.txt","close"]"#
    );
}

#[test]
fn watch_reports_a_new_file_as_rename_and_a_buffer_name() {
    assert_eq!(
        run(r#"out = [];
        const w = fs.watch(dir, { encoding: "buffer" }, (type, name) => { if (type === "close") return; out.push(type, name instanceof Uint8Array, String.fromCharCode(...name)); w.close(); });
        fs.writeFileSync(dir + "/new.txt", "x");"#),
        r#"["rename",true,"new.txt"]"#
    );
}

#[test]
fn watch_recursive_sees_a_subdirectory() {
    assert_eq!(
        run(r#"fs.mkdirSync(dir + "/sub"); out = [];
        const w = fs.watch(dir, { recursive: true }, (type, name) => { if (type === "close") return; out.push(type + ":" + name); w.close(); });
        fs.writeFileSync(dir + "/sub/x", "x");"#),
        r#"["rename:sub/x"]"#
    );
}

#[test]
fn watch_errors_and_options() {
    assert_eq!(
        run(r#"out = [];
        try { fs.watch(dir + "/missing", {}, () => {}) } catch (e) { out.push(e.code, e.syscall, e.message === "ENOENT: no such file or directory, watch '" + dir + "/missing'") }
        try { fs.watch(dir, { persistent: 1 }, () => {}) } catch (e) { out.push(e.message) }
        try { fs.watch(dir, {}) } catch (e) { out.push(e.message) }
        // An unref'd watcher does not keep the loop alive.
        fs.watch(dir, { persistent: false }, () => { out.push("event") });
        const w = fs.watch(dir, {}, () => {}); w.unref();"#),
        r#"["ENOENT","watch",true,"persistent must be a boolean","Expected \"listener\" callback"]"#
    );
}

#[test]
fn watch_closes_on_abort() {
    assert_eq!(
        run(r#"out = []; const c = new AbortController();
        fs.watch(dir, { signal: c.signal }, (type, error, fromAbort) => { out.push(type === "error" ? [type, error.name, fromAbort].join() : type) });
        c.abort();
        fs.watch(dir, { signal: AbortSignal.abort() }, (type, error, fromAbort) => { out.push(type === "error" ? "aborted before:" + fromAbort : type) });"#),
        r#"["error,AbortError,true","close","aborted before:true","close"]"#
    );
}

/// `fs.watchFile`: a stat on the timer, the listener with the stats after and before.
#[test]
fn watch_file_calls_the_listener_with_both_stats() {
    assert_eq!(
        run(r#"const f = dir + "/s.txt"; fs.writeFileSync(f, "a");
        const w = fs.watchFile(f, { interval: 10 }, (current, previous) => { out = [current.size, previous.size, current instanceof fs.Stats]; w.close(); });
        fs.writeFileSync(f, "longer");"#),
        r#"[6,1,true]"#
    );
    assert_eq!(
        run(r#"const w = fs.watchFile(dir + "/none", { interval: 10, bigint: true }, (current, previous) => { out = [current.size === 0n, current === previous]; w.close(); });"#),
        r#"[true,true]"#
    );
    assert_eq!(
        run(r#"try { fs.watchFile(dir, { persistent: 1 }, () => {}) } catch (e) { out = [e.code, e.message] }
        fs.watchFile(dir, { persistent: false, interval: 10 }, () => {}); const w = fs.watchFile(dir, {}, () => {}); w.unref();"#),
        r#"["ERR_INVALID_ARG_TYPE","The \"persistent\" property must be of type boolean, got number"]"#
    );
}
