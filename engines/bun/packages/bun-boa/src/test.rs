//! `bun test [filters…]`: Bun's test runner on Boa. Each test file runs in a realm of its own with `bun:test`
//! (js/test.ts) loaded first, so `describe`/`test`/`expect` are globals as in Bun; js/test/runner.ts runs the file's
//! tests and prints each, and the summary across files is printed here the way src/runtime/cli/test_command.rs does.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module, NativeFunction,
    js_string,
};

use crate::loader::BunModuleLoader;

const SUFFIXES: &[&str] = &[".test", "_test", ".spec", "_spec"];
const EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];
const SKIP_DIRS: &[&bun_paths::OSPathSlice] = &[b"node_modules", b".git"];
const NATIVE: &str = "__bun_test_native__";
const DEFAULT_TIMEOUT_MS: f64 = 5000.0;

/// The flags of `bun test` (src/runtime/cli/Arguments.rs).
#[derive(Default)]
struct Options {
    filters: Vec<String>,
    name_pattern: Option<String>,
    bail: u64,
    timeout: Option<f64>,
    update_snapshots: bool,
    only_failures: bool,
    todo: bool,
    randomize: bool,
    retry: f64,
    concurrent: bool,
    max_concurrency: Option<f64>,
}

impl Options {
    fn parse(args: &[String]) -> Options {
        let mut options = Options::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
                _ => (arg.as_str(), None),
            };
            let mut value = || inline.clone().or_else(|| iter.next().cloned());
            match flag {
                "-t" | "--test-name-pattern" => options.name_pattern = value(),
                "--bail" => options.bail = inline.as_deref().map_or(1, |n| n.parse().unwrap_or(1)),
                "--timeout" => options.timeout = value().and_then(|n| n.parse().ok()),
                "-u" | "--update-snapshots" => options.update_snapshots = true,
                "--only-failures" => options.only_failures = true,
                "--todo" => options.todo = true,
                "--randomize" => options.randomize = true,
                "--retry" => options.retry = value().and_then(|n| n.parse().ok()).unwrap_or(0.0),
                "--concurrent" => options.concurrent = true,
                "--max-concurrency" => options.max_concurrency = value().and_then(|n| n.parse().ok()),
                "--preload" | "--rerun-each" | "--reporter" | "--reporter-outfile" | "--coverage-dir"
                | "--coverage-reporter" | "--seed" | "--path-ignore-patterns" => {
                    if inline.is_none() {
                        iter.next();
                    }
                }
                a if a.starts_with('-') => {}
                a => options.filters.push(a.to_owned()),
            }
        }
        options
    }
}

/// What a file's run counted, and the totals across files.
#[derive(Default, Clone, Copy)]
struct Counts {
    pass: u64,
    fail: u64,
    skip: u64,
    todo: u64,
    filtered_out: u64,
    expects: u64,
    errors: u64,
    snapshots: u64,
    snapshots_passed: u64,
    snapshots_failed: u64,
    snapshots_added: u64,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Counts) {
        self.pass += o.pass;
        self.fail += o.fail;
        self.skip += o.skip;
        self.todo += o.todo;
        self.filtered_out += o.filtered_out;
        self.expects += o.expects;
        self.errors += o.errors;
        self.snapshots += o.snapshots;
        self.snapshots_passed += o.snapshots_passed;
        self.snapshots_failed += o.snapshots_failed;
        self.snapshots_added += o.snapshots_added;
    }
}

fn is_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    EXTENSIONS.contains(&ext) && SUFFIXES.iter().any(|s| stem.ends_with(s))
}

#[cfg(not(windows))]
fn scan(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(root) = bun_sys::Dir::open(dir.as_os_str().as_encoded_bytes()) else {
        return;
    };
    let Ok(mut walker) = bun_sys::walker_skippable::walk_owned(root, &[], SKIP_DIRS) else {
        return;
    };
    while let Ok(Some(entry)) = walker.next() {
        if entry.kind != bun_sys::FileKind::File {
            continue;
        }
        let rel = String::from_utf8_lossy(entry.path.as_bytes()).into_owned();
        if is_test_file(&rel) {
            files.push(dir.join(rel));
        }
    }
}

#[cfg(windows)]
fn scan(_: &Path, _: &mut Vec<PathBuf>) {}

/// The files `bun test` runs: an argument that is a file runs as is, a directory is scanned, anything else filters
/// the test files under the working directory by substring, as Bun does.
fn files(filters: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut substrings = Vec::new();
    for filter in filters {
        let path = cwd.join(filter);
        if path.is_file() {
            out.push(path);
        } else if path.is_dir() && (filter.starts_with('.') || filter.starts_with('/')) {
            scan(&path, &mut out);
        } else {
            substrings.push(filter.as_str());
        }
    }
    if out.is_empty() || !substrings.is_empty() {
        let mut all = Vec::new();
        scan(cwd, &mut all);
        all.retain(|p| {
            let rel = p.strip_prefix(cwd).unwrap_or(p).to_string_lossy();
            substrings.is_empty() || substrings.iter().any(|s| rel.contains(s))
        });
        out.extend(all);
    }
    out.sort();
    out.dedup();
    out
}

fn settle(promise: JsPromise, context: &mut Context) -> JsResult<JsValue> {
    let event_loop = bun_runtime_js::EventLoop::of(context);
    event_loop.run_until(context, || !matches!(promise.state(), PromiseState::Pending))?;
    match promise.state() {
        PromiseState::Fulfilled(value) => Ok(value),
        PromiseState::Rejected(reason) => Err(JsError::from_opaque(reason)),
        PromiseState::Pending => Err(JsNativeError::error()
            .with_message("the test file never finished (a promise that never settles)")
            .into()),
    }
}

fn get(object: &JsValue, key: &str, context: &mut Context) -> JsValue {
    object
        .as_object()
        .and_then(|o| o.get(JsString::from(key), context).ok())
        .unwrap_or_default()
}

fn number(object: &JsValue, key: &str, context: &mut Context) -> u64 {
    get(object, key, context).as_number().map_or(0, |n| n as u64)
}

fn path_arg(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    Ok(args
        .get_or_undefined(0)
        .to_string(context)?
        .to_std_string_escaped())
}

fn read_file(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    Ok(
        match bun_sys::File::read_from(bun_sys::Fd::cwd(), path.as_bytes()) {
            Ok(bytes) => JsString::from(String::from_utf8_lossy(&bytes).as_ref()).into(),
            Err(_) => JsValue::undefined(),
        },
    )
}

fn write_file(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let text = args
        .get_or_undefined(1)
        .to_string(context)?
        .to_std_string_escaped();
    // make_open creates the parent directories.
    let written = bun_sys::File::make_open(
        path.as_bytes(),
        bun_sys::O::WRONLY | bun_sys::O::CREAT | bun_sys::O::TRUNC,
        0o664,
    )
    .and_then(|file| file.write_all(text.as_bytes()));
    Ok(written.is_ok().into())
}

/// `mockModule(specifier, fromDir, exports)`: what `mock.module()` gives the loader, keyed by the path that the
/// specifier resolves to from `fromDir`, or by the specifier when it resolves to nothing.
fn mock_module(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let specifier = path_arg(args, context)?;
    let dir = args
        .get_or_undefined(1)
        .to_string(context)?
        .to_std_string_escaped();
    let loader = crate::loader::installed(context)
        .ok_or_else(|| JsNativeError::error().with_message("no module loader"))?;
    let key = crate::loader::resolve(&specifier, Path::new(&dir)).unwrap_or_else(|| PathBuf::from(&specifier));
    loader.mock(key, args.get_or_undefined(2).clone(), context)?;
    Ok(JsValue::undefined())
}

/// `originalPosition(path, line, column)`: `[line, column]` in the file as it is written, of a 1-based position in
/// the code the loader printed for it; `undefined` without a source map.
fn original_position(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let line = args.get_or_undefined(1).to_u32(context)?;
    let column = args.get_or_undefined(2).to_u32(context)?;
    Ok(match crate::source_maps::original_position(&path, line, column) {
        Some((line, column)) => boa_engine::object::builtins::JsArray::from_iter([line.into(), column.into()], context).into(),
        None => JsValue::undefined(),
    })
}

/// `originalSource(path)`: the file as it is written, if the loader transpiled it.
fn original_source(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    Ok(crate::source_maps::original_source(&path).map_or(JsValue::undefined(), |source| JsString::from(&*source).into()))
}

/// `originalLines(path, first, last)`: the 1-based lines `first..=last` of the file as it is written, for an error's
/// preview: what of a long file one error shows.
fn original_lines(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let first = args.get_or_undefined(1).to_u32(context)?.max(1) as usize;
    let last = args.get_or_undefined(2).to_u32(context)? as usize;
    let Some(source) = crate::source_maps::original_source(&path) else {
        return Ok(JsValue::undefined());
    };
    let lines: Vec<JsValue> = source
        .split('\n')
        .skip(first - 1)
        .take(last.saturating_sub(first - 1))
        .map(|line| JsString::from(line).into())
        .collect();
    Ok(boa_engine::object::builtins::JsArray::from_iter(lines, context).into())
}

/// Bun's `enable_ansi_colors_stderr`: a terminal, unless NO_COLOR, or FORCE_COLOR.
fn colors() -> bool {
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0");
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    set("FORCE_COLOR") || std::io::stderr().is_terminal()
}

/// `globalThis.__bun_test_native__`, which js/test/state.ts takes: printDiff.rs from bun_runtime_js, promise
/// inspection, files, and the flags.
fn install_natives(options: &Options, failures_before: u64, context: &mut Context) -> JsResult<JsObject> {
    let global = bun_js::JSGlobalObject::from_context(context);
    let mut diff = JsValue::undefined();
    for (name, value) in bun_runtime_js::generated_bindgen::test_runner::namespaces(global)
        .map_err(|error| global.into_engine_error(error))?
    {
        if name == "testRunner" {
            diff = get(&value.into_engine(), "diff", context);
        }
    }
    let flags = {
        let mut flags = ObjectInitializer::new(context);
        flags
            .property(
                js_string!("timeout"),
                options.timeout.unwrap_or(DEFAULT_TIMEOUT_MS),
                Attribute::all(),
            )
            .property(js_string!("updateSnapshots"), options.update_snapshots, Attribute::all())
            .property(js_string!("onlyFailures"), options.only_failures, Attribute::all())
            .property(js_string!("todo"), options.todo, Attribute::all())
            .property(js_string!("randomize"), options.randomize, Attribute::all())
            .property(js_string!("retry"), options.retry, Attribute::all())
            .property(js_string!("concurrent"), options.concurrent, Attribute::all())
            .property(
                js_string!("maxConcurrency"),
                options.max_concurrency.unwrap_or(20.0),
                Attribute::all(),
            )
            .property(js_string!("bail"), options.bail as f64, Attribute::all())
            .property(js_string!("failuresBefore"), failures_before as f64, Attribute::all());
        if let Some(pattern) = &options.name_pattern {
            flags.property(js_string!("namePattern"), JsString::from(pattern.as_str()), Attribute::all());
        }
        flags.build()
    };
    let mut native = ObjectInitializer::new(context);
    native
        .property(js_string!("diff"), diff, Attribute::all())
        .property(js_string!("colors"), colors(), Attribute::all())
        .property(js_string!("isAgent"), bun_core::output::is_ai_agent(), Attribute::all())
        .property(js_string!("isCI"), bun_core::ci_info::is_ci(), Attribute::all())
        .property(js_string!("options"), flags, Attribute::all());
    for (name, f, len) in [
        (
            "promiseState",
            crate::builtins::promise_state as fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>,
            1,
        ),
        ("promiseResult", crate::builtins::promise_result, 1),
        ("readFile", read_file, 1),
        ("writeFile", write_file, 2),
        ("mockModule", mock_module, 3),
        ("originalPosition", original_position, 3),
        ("originalSource", original_source, 1),
        ("originalLines", original_lines, 3),
    ] {
        native.function(NativeFunction::from_fn_ptr(f), JsString::from(name), len);
    }
    let native = native.build();
    context.register_global_property(JsString::from(NATIVE), native.clone(), Attribute::all())?;
    Ok(native)
}

/// A function js/test.ts put on the native object: `run` and `unhandled`.
fn callback(native: &JsObject, name: &str, context: &mut Context) -> JsResult<JsObject> {
    native
        .get(JsString::from(name), context)?
        .as_callable()
        .ok_or_else(|| JsNativeError::typ().with_message(format!("bun:test has no {name}")).into())
}

/// The error the file threw while it loaded, printed by js/test/runner.ts as an unhandled error.
fn unhandled(err: JsError, native: &JsObject, context: &mut Context) {
    let value = err
        .into_opaque(context)
        .unwrap_or_else(|e| JsValue::from(JsString::from(e.to_string().as_str())));
    match callback(native, "unhandled", context) {
        Ok(print) => {
            let _ = print.call(&JsValue::undefined(), &[value], context);
        }
        Err(_) => eprintln!("error: {}", value.display()),
    }
}

/// The lines of the tests that failed, were skipped or are todo, which `bun test` prints again after the last file
/// (`failures_to_repeat_buf` and the others).
#[derive(Default)]
struct Repeats {
    fail: String,
    skip: String,
    todo: String,
}

/// What one file's run gave.
#[derive(Default)]
struct FileRun {
    counts: Counts,
    repeats: Repeats,
    /// `--bail`'s count was reached in this file.
    bailed: bool,
}

fn run_file(path: &Path, rel: &Path, options: &Options, failures_before: u64, colors: bool) -> FileRun {
    let loader = Rc::new(BunModuleLoader::default());
    let argv = vec![
        std::env::args().next().unwrap_or_else(|| "bun".into()),
        path.display().to_string(),
    ];
    // As test_command.rs: a file that does not load has no test to list, so the file is listed.
    let failed_to_load = FileRun {
        counts: Counts {
            fail: 1,
            errors: 1,
            ..Counts::default()
        },
        repeats: Repeats {
            fail: format!("{} {}\n", pretty("<r><red>(fail)<r>", colors), rel.display()),
            ..Repeats::default()
        },
        bailed: false,
    };
    let mut context = match crate::runtime::context(loader.clone(), None, argv) {
        Ok(context) => context,
        Err(err) => {
            eprintln!("error: {err}");
            return failed_to_load;
        }
    };
    let loaded = (|| -> JsResult<JsObject> {
        let native = install_natives(options, failures_before, &mut context)?;
        let runner = loader
            .builtin("bun:test", &mut context)
            .expect("bun:test is built in")?;
        settle(runner.load_link_evaluate(&mut context), &mut context)?;
        Ok(native)
    })();
    let native = match loaded {
        Ok(native) => native,
        Err(err) => {
            eprintln!("error: bun:test: {err}");
            return failed_to_load;
        }
    };
    let evaluated = loader
        .load(path, &mut context)
        .and_then(|module: Module| settle(module.load_link_evaluate(&mut context), &mut context));
    if let Err(err) = evaluated {
        unhandled(err, &native, &mut context);
        return failed_to_load;
    }
    let finished = (|| -> JsResult<JsValue> {
        let run = callback(&native, "run", &mut context)?;
        let promise = run.call(
            &JsValue::undefined(),
            &[JsString::from(path.display().to_string().as_str()).into()],
            &mut context,
        )?;
        let promise = promise
            .as_object()
            .and_then(|o| JsPromise::from_object(o.clone()).ok())
            .ok_or_else(|| JsNativeError::typ().with_message("bun:test's runner did not return a promise"))?;
        settle(promise, &mut context)
    })();
    match finished {
        Ok(stats) => {
            let snapshots = get(&stats, "snapshots", &mut context);
            let repeats = get(&stats, "repeats", &mut context);
            let mut text = |key: &str| {
                get(&repeats, key, &mut context)
                    .as_string()
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_default()
            };
            let repeats = Repeats {
                fail: text("fail"),
                skip: text("skip"),
                todo: text("todo"),
            };
            let bailed = get(&stats, "bailed", &mut context).to_boolean();
            let counts = Counts {
                pass: number(&stats, "pass", &mut context),
                fail: number(&stats, "fail", &mut context),
                skip: number(&stats, "skip", &mut context),
                todo: number(&stats, "todo", &mut context),
                filtered_out: number(&stats, "filteredOut", &mut context),
                expects: number(&stats, "expects", &mut context),
                errors: number(&stats, "errors", &mut context),
                snapshots: number(&snapshots, "total", &mut context),
                snapshots_passed: number(&snapshots, "passed", &mut context),
                snapshots_failed: number(&snapshots, "failed", &mut context),
                snapshots_added: number(&snapshots, "added", &mut context),
            };
            FileRun { counts, repeats, bailed }
        }
        Err(err) => {
            unhandled(err, &native, &mut context);
            failed_to_load
        }
    }
}

/// `Output::pretty_fmt` for the summary: Bun's markup, colored or not.
fn pretty(text: &str, colors: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let code = match &after[..end] {
            "r" => "\x1b[0m",
            "d" => "\x1b[2m",
            "b" => "\x1b[1m",
            "red" => "\x1b[31m",
            "green" => "\x1b[32m",
            "yellow" => "\x1b[33m",
            "magenta" => "\x1b[35m",
            _ => {
                out.push('<');
                rest = after;
                continue;
            }
        };
        if colors {
            out.push_str(code);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn summary(total: &Counts, files: usize, start: std::time::Instant, colors: bool) {
    let mut err = String::new();
    let mut line = |text: &str| err.push_str(&pretty(text, colors));
    if total.pass > 0 {
        line("<r><green>");
    }
    line(&format!(" {} pass<r>\n", total.pass));
    if total.skip > 0 {
        line(&format!("<r><yellow> {} skip<r>\n", total.skip));
    } else if total.filtered_out > 0 {
        line(&format!("<r><d> {} filtered out<r>\n", total.filtered_out));
    }
    if total.todo > 0 {
        line(&format!("<r><magenta> {} todo<r>\n", total.todo));
    }
    line(if total.fail > 0 { "<r><red>" } else { "<r><d>" });
    line(&format!(" {} fail<r>\n", total.fail));
    if total.errors > 0 {
        line(&format!(
            "<r><red> {} error{}<r>\n",
            total.errors,
            if total.errors > 1 { "s" } else { "" }
        ));
    }
    let mut print_expect_calls = total.expects > 0;
    if total.snapshots > 0 {
        if print_expect_calls && total.snapshots_added == 0 && total.snapshots_failed == 0 {
            print_expect_calls = false;
            line(&format!(" {} snapshots, {} expect() calls", total.snapshots, total.expects));
        } else {
            let mut parts = Vec::new();
            if total.snapshots_passed > 0 {
                parts.push(format!("<d>{} passed<r>", total.snapshots_passed));
            }
            if total.snapshots_added > 0 {
                parts.push(format!("<b>+{} added<r>", total.snapshots_added));
            }
            if total.snapshots_failed > 0 {
                parts.push(format!("<red>{} failed<r>", total.snapshots_failed));
            }
            line(&format!("<d>snapshots:<r> {}", parts.join(", ")));
        }
        line("\n");
    }
    if print_expect_calls {
        line(&format!(" {} expect() calls\n", total.expects));
    }
    let _ = std::io::stderr().write_all(err.as_bytes());
    ran(total, files, start, colors);
}

/// After the last file, as test_command.rs does when more than 20 tests passed: the skipped, todo and failed tests
/// again, so that they are not lost in what scrolled by.
fn print_repeats(total: &Counts, repeats: &Repeats, colors: bool) {
    let mut err = String::new();
    if total.skip > 0 {
        err += &pretty(&format!("\n<r><d>{} tests skipped:<r>\n", total.skip), colors);
        err += &repeats.skip;
    }
    if total.todo > 0 {
        if total.skip > 0 {
            err.push('\n');
        }
        err += &pretty(&format!("\n<r><d>{} tests todo:<r>\n", total.todo), colors);
        err += &repeats.todo;
    }
    if total.fail > 0 {
        if total.skip > 0 || total.todo > 0 {
            err.push('\n');
        }
        err += &pretty(&format!("\n<r><d>{} tests failed:<r>\n", total.fail), colors);
        err += &repeats.fail;
    }
    let _ = std::io::stderr().write_all(err.as_bytes());
}

/// `print_summary`: what ran, and how long it took.
fn ran(total: &Counts, files: usize, start: std::time::Instant, colors: bool) {
    let tests = total.pass + total.fail + total.skip + total.todo;
    let text = format!(
        "Ran {tests} test{} across {files} file{}. <d>[{:.2}ms]<r>\n",
        if tests == 1 { "" } else { "s" },
        if files == 1 { "" } else { "s" },
        start.elapsed().as_secs_f64() * 1000.0
    );
    let _ = std::io::stderr().write_all(pretty(&text, colors).as_bytes());
}

pub fn main(args: &[String]) -> i32 {
    let options = Options::parse(args);
    let colors = colors();
    println!("bun test {}", bun_core::package_json_version_with_sha);
    let _ = std::io::stdout().flush();
    let cwd = std::env::current_dir().unwrap_or_default();
    let files = files(&options.filters, &cwd);
    if files.is_empty() {
        eprintln!("No tests found!");
        return 1;
    }
    let start = std::time::Instant::now();
    let mut total = Counts::default();
    let mut repeats = Repeats::default();
    let mut files_run = 0;
    for file in &files {
        let rel = file.strip_prefix(&cwd).unwrap_or(file);
        eprintln!("\n{}:", rel.display());
        let run = run_file(file, rel, &options, total.fail, colors);
        total += run.counts;
        repeats.fail += &run.repeats.fail;
        repeats.skip += &run.repeats.skip;
        repeats.todo += &run.repeats.todo;
        files_run += 1;
        if run.bailed || (options.bail > 0 && total.fail >= options.bail) {
            ran(&total, files_run, start, colors);
            eprintln!(
                "\nBailed out after {} failure{}",
                options.bail,
                if options.bail == 1 { "" } else { "s" }
            );
            return 1;
        }
    }
    if total.pass > 20 && !bun_core::output::is_ai_agent() && !options.only_failures {
        print_repeats(&total, &repeats, colors);
    }
    eprintln!();
    summary(&total, files_run, start, colors);
    i32::from(total.fail > 0 || total.errors > 0)
}
