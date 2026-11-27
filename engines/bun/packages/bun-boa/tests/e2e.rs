use std::path::PathBuf;
use std::process::{Command, Output};

fn bun() -> Command {
    Command::new(std::env::var_os("NEXTEST_BIN_EXE_bun").unwrap_or(env!("CARGO_BIN_EXE_bun").into()))
}

fn fixtures() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or(env!("CARGO_MANIFEST_DIR").into())).join("tests/fixtures")
}

/// An empty directory of this test's own, removed on drop: other runs of this suite on the host (other trees, other
/// sessions) share `temp_dir()`.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("bun-boa-e2e-{}", std::process::id())).join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for Scratch {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_dir(self.0.parent().unwrap());
    }
}

fn text(output: &Output) -> (String, String) {
    (String::from_utf8_lossy(&output.stdout).into_owned(), String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn version() {
    let out = bun().arg("--version").output().unwrap();
    let (stdout, stderr) = text(&out);
    assert!(stdout.trim().starts_with(|c: char| c.is_ascii_digit()), "{stdout}{stderr}");
    assert!(out.status.success());
    let out = bun().arg("--revision").output().unwrap();
    assert!(text(&out).0.contains('+'), "{:?}", text(&out));
}

#[test]
fn runs_typescript_modules_with_bun_apis() {
    let _ = std::fs::remove_file(fixtures().join("out.txt"));
    let out = bun()
        .args(["run", "main.ts", "a", "b"])
        .current_dir(fixtures())
        .env("BUN_BOA_TEST", "1")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    let expected = "hello bun!
json boa 3
write 20 written by Bun.write true
json-file boa
missing false
timer done, version string, env true
argv a,b
main true
";
    assert_eq!(stdout, expected, "stderr: {stderr}");
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
}

#[test]
fn file_without_run_and_eval() {
    let out = bun().arg(fixtures().join("greet.ts")).output().unwrap();
    assert!(out.status.success(), "{:?}", text(&out));
    let out = bun().args(["-e", "const n: number = 40 + 2; console.log(n); process.exit(7)"]).output().unwrap();
    assert_eq!(text(&out).0, "42\n");
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn reports_errors() {
    let out = bun().args(["-e", "throw new TypeError('boom')"]).output().unwrap();
    let (_, stderr) = text(&out);
    assert!(stderr.contains("TypeError") && stderr.contains("boom"), "{stderr}");
    assert_eq!(out.status.code(), Some(1));
}

/// `ldd` + `nm`, in Rust: no C++ runtime among the shared libraries, and no C++ ABI or `std::` symbol defined or
/// imported. glibc's `__cxa_atexit`/`__cxa_finalize`/`__cxa_thread_atexit_impl` are C runtime and Rust's std uses them.
#[test]
fn binary_contains_no_cxx() {
    use object::{Object, ObjectSymbol};
    const GLIBC: [&str; 3] = ["__cxa_atexit", "__cxa_finalize", "__cxa_thread_atexit_impl"];
    const CXX_LIBS: [&str; 3] = ["libstdc++", "libc++", "libc++abi"];
    let bytes = std::fs::read(std::env::var_os("NEXTEST_BIN_EXE_bun").unwrap_or(env!("CARGO_BIN_EXE_bun").into())).unwrap();
    let file = object::File::parse(&*bytes).unwrap();

    let object::File::Elf64(elf) = &file else { panic!("expected a 64-bit ELF") };
    let needed: Vec<String> = elf_needed(elf);
    let cxx_libs: Vec<&String> = needed.iter().filter(|l| CXX_LIBS.iter().any(|c| l.starts_with(c))).collect();
    assert!(cxx_libs.is_empty(), "links C++ runtime: {cxx_libs:?} (needed: {needed:?})");

    let cxx_symbols: Vec<String> = file
        .symbols()
        .chain(file.dynamic_symbols())
        .filter_map(|s| s.name().ok().map(str::to_owned))
        .filter(|n| {
            (n.starts_with("__cxa_") && !GLIBC.contains(&n.as_str()))
                || n.starts_with("__gxx_personality")
                || ["_ZNSt", "_ZSt", "_ZTI", "_ZTV", "_ZTS", "_Znw", "_Zdl", "_Zna", "_Zda"].iter().any(|p| n.starts_with(p))
        })
        .collect();
    assert!(cxx_symbols.is_empty(), "C++ symbols: {:?}", &cxx_symbols[..cxx_symbols.len().min(20)]);
}

fn elf_needed(elf: &object::read::elf::ElfFile64) -> Vec<String> {
    use object::read::elf::{Dyn, FileHeader, SectionHeader};
    let endian = elf.endian();
    let data = elf.data();
    let sections = elf.elf_header().sections(endian, data).unwrap();
    let mut out = Vec::new();
    for section in sections.iter() {
        let Some((entries, link)) = section.dynamic(endian, data).unwrap() else { continue };
        let strings = sections.strings(endian, data, link).unwrap();
        for d in entries.iter().filter(|d| d.tag32(endian) == Some(object::elf::DT_NEEDED)) {
            out.push(String::from_utf8_lossy(d.string(endian, strings).unwrap()).into_owned());
        }
    }
    out
}

const MESSY_FORMATTED: &str = "const greeting: string = \"hi\";
function add(a: number, b: number) {
  return a + b;
}
export const obj = { a: 1, b: [1, 2, 3] };
";

#[test]
fn fmt_formats_files_and_stdin() {
    let dir = Scratch::new("fmt");
    std::fs::copy(fixtures().join("fmt/messy.ts"), dir.join("messy.ts")).unwrap();

    let out = bun().args(["fmt", "--check", "messy.ts"]).current_dir(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", text(&out));

    let out = bun().args(["fmt", "messy.ts"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{:?}", text(&out));
    assert_eq!(std::fs::read_to_string(dir.join("messy.ts")).unwrap(), MESSY_FORMATTED);

    let out = bun()
        .args(["fmt", "--stdin-filepath", "messy.ts"])
        .current_dir(&dir)
        .stdin(std::fs::File::open(fixtures().join("fmt/messy.ts")).unwrap())
        .output()
        .unwrap();
    assert_eq!(text(&out).0, MESSY_FORMATTED, "{:?}", text(&out));
    assert!(out.status.success());
}

#[test]
fn fmt_reads_bunfig() {
    let dir = Scratch::new("fmt-bunfig");
    std::fs::write(dir.join("bunfig.toml"), "[fmt]\nsemi = false\n").unwrap();
    let out = bun()
        .args(["fmt", "--stdin-filepath", "a.ts"])
        .current_dir(&dir)
        .stdin(std::fs::File::open(fixtures().join("fmt/messy.ts")).unwrap())
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(stdout.starts_with("const greeting: string = \"hi\"\n"), "{stdout}{stderr}");
    assert!(out.status.success(), "{stderr}");
}

#[test]
fn lint_reports_and_passes() {
    let dir = Scratch::new("lint");
    std::fs::write(dir.join("bad.ts"), "debugger;\nif (/a+b/.test(\"aab\")) console.log(1);\n").unwrap();
    std::fs::write(dir.join("good.ts"), "export const x = /a(b)?/.exec(\"ab\");\n").unwrap();
    let out = bun().args(["lint", "--rule", r#"{"no-debugger":"error"}"#, "bad.ts"]).current_dir(&dir).output().unwrap();
    let (stdout, stderr) = text(&out);
    assert!(format!("{stdout}{stderr}").contains("no-debugger"), "{stdout}{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    let out = bun().args(["lint", "good.ts"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{:?}", text(&out));
}

/// A rule whose option is a regular expression: only the identifier that the pattern rejects is reported, so the
/// pattern ran, through `bun_yarr`'s C ABI on regress (`src/yarr.rs`).
#[test]
fn lint_runs_regular_expressions_through_yarr() {
    let dir = Scratch::new("lint-regex");
    std::fs::write(dir.join("ids.ts"), "export const snake_ok = 1;\nexport const BadName = 2;\n").unwrap();
    let rule = r#"{"id-match":["error","^[a-z]+(_[a-z]+)*$"]}"#;
    let out = bun().args(["lint", "--rule", rule, "ids.ts"]).current_dir(&dir).output().unwrap();
    let (stdout, stderr) = text(&out);
    let all = format!("{stdout}{stderr}");
    assert!(all.contains("id-match") && all.contains("BadName"), "{all}");
    // Not `snake_ok` alone: the code frame above `BadName` shows its line.
    assert!(!all.contains("'snake_ok'"), "{all}");
    assert_eq!(out.status.code(), Some(1), "{all}");
    let rule = r#"{"id-match":["error","^([a-z]+|[A-Z][a-zA-Z]+)(_[a-z]+)*$"]}"#;
    let out = bun().args(["lint", "--rule", rule, "ids.ts"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{:?}", text(&out));
}

#[test]
fn runs_package_json_scripts() {
    let dir = Scratch::new("scripts");
    std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"p","scripts":{"prehi":"echo pre","hi":"echo hi $npm_lifecycle_event","posthi":"echo post","tool":"mytool","fail":"exit 4","code":"bun -e 'console.log(6*7)'"}}"#,
    )
    .unwrap();
    let tool = dir.join("node_modules/.bin/mytool");
    std::fs::write(&tool, "#!/bin/sh\necho tool \"$@\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = bun().args(["run", "hi", "a b"]).current_dir(&dir).output().unwrap();
    assert_eq!(text(&out).0, "pre\nhi hi a b\npost\n", "{:?}", text(&out));
    assert!(out.status.success());
    let out = bun().args(["tool", "x"]).current_dir(&dir).output().unwrap();
    assert_eq!(text(&out).0, "tool x\n", "{:?}", text(&out));
    let out = bun().args(["run", "fail"]).current_dir(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(4));
    let out = bun().args(["run", "code"]).current_dir(&dir).output().unwrap();
    assert_eq!(text(&out).0, "42\n", "{:?}", text(&out));
    let out = bun().arg("run").current_dir(&dir).output().unwrap();
    assert!(text(&out).0.contains("bun run hi"), "{:?}", text(&out));
}

#[test]
fn a_path_is_a_file_not_a_script() {
    let dir = Scratch::new("script-path");
    std::fs::write(dir.join("package.json"), r#"{"scripts":{"./x.js":"echo script","x.js":"echo script"}}"#).unwrap();
    std::fs::write(dir.join("x.js"), "console.log('file');\n").unwrap();
    for args in [&["./x.js"][..], &["run", "./x.js"]] {
        let out = bun().args(args).current_dir(&dir).output().unwrap();
        assert_eq!(text(&out).0, "file\n", "{args:?}: {:?}", text(&out));
    }
    // A bare name is still a script first.
    let out = bun().args(["run", "x.js"]).current_dir(&dir).output().unwrap();
    assert_eq!(text(&out).0, "script\n", "{:?}", text(&out));
}

/// `bun build` bundles TypeScript across files through Bun's bundler, and the bundle runs on Boa.
#[cfg(feature = "build")]
#[test]
fn build_bundles_and_runs() {
    let dir = Scratch::new("build");
    std::fs::write(dir.join("a.ts"), "export const x: number = 2;\n").unwrap();
    std::fs::write(dir.join("e.ts"), "import { x } from \"./a.ts\";\nconsole.log(x * 21);\n").unwrap();
    let out = bun().args(["build", "e.ts", "--outfile", "out/e.js"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "{:?}", text(&out));
    let js = std::fs::read_to_string(dir.join("out/e.js")).unwrap();
    assert!(!js.contains("import ") && !js.contains(": number"), "{js}");
    let out = bun().arg("out/e.js").current_dir(&dir).output().unwrap();
    assert_eq!(text(&out).0, "42\n", "{:?}", text(&out));
    let out = bun().args(["build", "e.ts", "--minify"]).current_dir(&dir).output().unwrap();
    assert!(out.status.success() && text(&out).0.len() < js.len(), "{:?}", text(&out));
    let out = bun().args(["build", "e.ts", "a.ts"]).current_dir(&dir).output().unwrap();
    assert!(!out.status.success(), "{:?}", text(&out));
}

/// `bun install`, `bun add` and `bun remove` and their aliases reach Bun's package manager (`bun_install`), which parses
/// their flags. `bun install -g` is `bun add`, as in `Command::which`.
#[cfg(feature = "install")]
#[test]
fn install_add_remove_reach_the_package_manager() {
    for (args, usage) in [
        (&["install", "--help"][..], "Usage: bun install "),
        (&["i", "--help"], "Usage: bun install "),
        (&["ci", "--help"], "Usage: bun install "),
        (&["install", "-g", "--help"], "Usage: bun add "),
        (&["add", "--help"], "Usage: bun add "),
        (&["a", "--help"], "Usage: bun add "),
        (&["remove", "--help"], "Usage: bun remove "),
        (&["rm", "--help"], "Usage: bun remove "),
        (&["r", "--help"], "Usage: bun remove "),
        (&["uninstall", "--help"], "Usage: bun remove "),
    ] {
        let out = bun().args(args).env("NO_COLOR", "1").output().unwrap();
        assert!(text(&out).0.trim_start().starts_with(usage), "{args:?}: {:?}", text(&out));
        assert!(out.status.success(), "{args:?}: {:?}", text(&out));
    }
}

/// File errors read as Bun's (bun_sys): errno name, message, syscall and path.
#[test]
fn file_errors_read_as_bun() {
    let code = "try { await Bun.file('./no-such-file.txt').text() } catch (e) { console.log(String(e.message)) }";
    let out = bun().args(["-e", code]).output().unwrap();
    let (stdout, _) = text(&out);
    assert!(stdout.contains("ENOENT") && stdout.contains("no-such-file.txt"), "{stdout:?}");
}

/// Blob, File, Bun.file and Bun.write from bun_runtime_js, through `bun test`.
#[test]
fn blob_file_and_write_are_bound_through_bun_js() {
    let dir = fixtures().join("blob");
    let out = bun().args(["test", "blob.test.ts"]).current_dir(&dir).output().unwrap();
    let all = format!("{}{}", text(&out).0, text(&out).1);
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with(".out-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    assert!(all.contains(" 4 pass") && all.contains(" 0 fail"), "{all}");
    assert!(out.status.success(), "{all}");
}

/// `Bun.stdout` is a file of descriptor 1, which `Bun.write` writes to as it is.
#[test]
fn bun_write_to_stdout() {
    let code = "const n = await Bun.write(Bun.stdout, 'to stdout\\n'); console.log(n, Bun.stdout.type, Bun.file(1) instanceof Blob)";
    let out = bun().args(["-e", code]).output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(stdout, "to stdout\n10 application/octet-stream true\n", "stderr: {stderr}");
    assert!(out.status.success(), "stderr: {stderr}");
}

/// `bun test` on Boa: discovery, `bun:test` imports and globals, hooks, async and `done` tests, mocks, `test.each`,
/// skip/todo, a failing test that sets the exit code, `-t`, and Bun's summary.
#[test]
fn test_runs_bun_test_files() {
    let dir = fixtures().join("test");
    let out = bun().arg("test").current_dir(&dir).output().unwrap();
    let (stdout, stderr) = text(&out);
    let all = format!("{stdout}{stderr}");
    for line in [
        "(pass) math > adds",
        "(pass) math > nested > async",
        "(pass) mocks",
        "(pass) 2 + 3 = 5",
        "(pass) globals > works",
        "(fail) fails on purpose",
        "(skip) skipped",
        "(todo) later",
        " 7 pass",
        " 1 skip",
        " 1 todo",
        " 1 fail",
        "Ran 10 tests across 2 files.",
    ] {
        assert!(all.contains(line), "missing {line:?} in:\n{all}");
    }
    assert!(all.contains("Expected: 5") && all.contains("Received: 4"), "{all}");
    assert!(!all.contains("must not run") && !all.contains("beforeEach ran"), "{all}");
    assert_eq!(out.status.code(), Some(1), "{all}");

    let out = bun().args(["test", "nested"]).current_dir(&dir).output().unwrap();
    let all = format!("{}{}", text(&out).0, text(&out).1);
    assert!(all.contains(" 1 pass") && all.contains("across 1 file."), "{all}");
    assert!(out.status.success(), "{all}");

    let out = bun().args(["test", "math.test.ts", "-t", "^mocks$"]).current_dir(&dir).output().unwrap();
    let all = format!("{}{}", text(&out).0, text(&out).1);
    assert!(all.contains("(pass) mocks") && !all.contains("(pass) math > adds"), "{all}");

    let out = bun().args(["test", "no-such-filter"]).current_dir(&dir).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", text(&out));
    assert!(text(&out).1.contains("No tests found"), "{:?}", text(&out));
}

/// js/test/runner.ts as Bun's runner orders a file: describe callbacks after the file, hooks around each test, done
/// callbacks, a beforeAll that throws, test.failing, test.each labels, retries, expect.assertions, inline
/// snapshots, onTestFinished and timeouts.
#[test]
fn test_runner_orders_as_bun() {
    let out = bun()
        .args(["test", "order.test.ts"])
        .current_dir(fixtures().join("runner"))
        .env("AGENT", "0")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(stdout.starts_with("bun test "), "{stdout}");
    for line in [
        "(pass) outer > inner > one",
        "(pass) outer > two",
        "(pass) async describe > declared after an await",
        "(pass) order",
        "(pass) done callback",
        "(pass) failing passes when it throws",
        "(pass) each 1 x 0",
        "(pass) each 2 y 1",
        "(pass) retried (attempt 2)",
        "(pass) assertions",
        "(pass) inline snapshot",
        "(pass) after onTestFinished",
        "(fail) beforeAll throws > (unnamed)",
        "error: setup failed",
        "(fail) timeout",
        "this test timed out after 20ms.",
        " 13 pass",
        " 2 fail",
        "Ran 15 tests across 1 file.",
    ] {
        assert!(stderr.contains(line), "missing {line:?} in:\n{stderr}");
    }
    assert!(!stderr.contains("must not run"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
}

/// `--bail` stops at the failure it counts to, inside the file; with more than 20 tests passing, the failed ones are
/// listed again after the last file.
#[test]
fn test_runner_bails_mid_file_and_repeats_failures() {
    let run = |args: &[&str]| {
        let out = bun()
            .args(args)
            .current_dir(fixtures().join("runner"))
            .env("AGENT", "0")
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        (text(&out).1, out.status.code())
    };
    let (stderr, code) = run(&["test", "order.test.ts", "--bail"]);
    assert!(stderr.contains("(fail) beforeAll throws > (unnamed)"), "{stderr}");
    assert!(!stderr.contains("(fail) timeout"), "{stderr}");
    assert!(stderr.contains("Ran 14 tests across 1 file.") && stderr.contains("Bailed out after 1 failure"), "{stderr}");
    assert_eq!(code, Some(1), "{stderr}");

    let (stderr, _) = run(&["test", "order.test.ts", "repeat.test.ts"]);
    let listed = stderr.split_once("tests failed:").map(|(_, rest)| rest).unwrap_or_default();
    assert!(listed.contains("(fail) beforeAll throws > (unnamed)") && listed.contains("(fail) timeout"), "{stderr}");
}

/// A getter that a test puts at `Array.prototype[1]` does not reach the runner's own arrays: results, hooks, matcher
/// messages and the error of a failure, as with Bun's native runner.
#[test]
fn test_runner_is_immune_to_array_prototype() {
    let out = bun()
        .args(["test", "prototype.test.ts"])
        .current_dir(fixtures().join("runner"))
        .env("AGENT", "0")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let (_, stderr) = text(&out);
    for line in [
        "(pass) with a getter at Array.prototype[1] > matchers",
        "(pass) with a getter at Array.prototype[1] > a failing matcher's message",
        "(pass) with a getter at Array.prototype[1] > each 3 4",
        "error: reported",
        "(fail) with a getter at Array.prototype[1] > a failure",
        "(pass) after",
        " 5 pass",
        " 1 fail",
        "Ran 6 tests across 1 file.",
    ] {
        assert!(stderr.contains(line), "missing {line:?} in:\n{stderr}");
    }
    assert_eq!(out.status.code(), Some(1), "{stderr}");
}

/// Through the loader's source maps: new inline snapshots are written into the test file where the matcher is called,
/// and an error shows the lines of the file as it is written, at their numbers.
#[test]
fn inline_snapshots_are_written_and_errors_show_their_source() {
    let dir = std::env::temp_dir().join(format!("bun-boa-inline-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let test = r#"import { expect, test } from "bun:test";

type Shape = { a: number };
test("writes", () => {
  const value: Shape = { a: 1 };
  expect(value).toMatchInlineSnapshot();
  expect("one line").toMatchInlineSnapshot();
  expect({ b: 2 }).toMatchInlineSnapshot({ b: expect.any(Number) });
});

test("fails", () => {
  const typed: number = 1;
  expect(typed).toBe(2);
});
"#;
    std::fs::write(dir.join("inline.test.ts"), test).unwrap();
    let out = bun()
        .args(["test", "inline.test.ts"])
        .current_dir(&dir)
        .env("CI", "false")
        .env("AGENT", "0")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stderr = text(&out).1;
    let written = std::fs::read_to_string(dir.join("inline.test.ts")).unwrap();
    let expected = test
        .replace(
            "expect(value).toMatchInlineSnapshot();",
            "expect(value).toMatchInlineSnapshot(`\n    {\n      \"a\": 1,\n    }\n  `);",
        )
        .replace(
            "expect(\"one line\").toMatchInlineSnapshot();",
            "expect(\"one line\").toMatchInlineSnapshot(`\"one line\"`);",
        )
        .replace(
            "toMatchInlineSnapshot({ b: expect.any(Number) });",
            "toMatchInlineSnapshot({ b: expect.any(Number) }, `\n    {\n      \"b\": Any<Number>,\n    }\n  `);",
        );
    assert_eq!(written, expected, "stderr: {stderr}");
    assert!(stderr.contains("13 |   expect(typed).toBe(2);"), "{stderr}");
    assert!(stderr.contains("inline.test.ts:13:"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that does not load is an unhandled error and a failure, as in Bun.
#[test]
fn test_file_that_does_not_load() {
    let dir = std::env::temp_dir().join(format!("bun-boa-bad-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.test.js"), "throw new Error('top level');\n").unwrap();
    let out = bun().args(["test", "bad.test.js"]).current_dir(&dir).env("NO_COLOR", "1").output().unwrap();
    let (_, stderr) = text(&out);
    for line in ["# Unhandled error between tests", "top level", " 0 pass", " 1 fail", " 1 error", "Ran 1 test across 1 file."] {
        assert!(stderr.contains(line), "missing {line:?} in:\n{stderr}");
    }
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `performance` (js/performance.ts) on the clock of bun_runtime_js: now() is monotonic from the time origin, and
/// marks and measures are kept as WebCore's PerformanceUserTiming keeps them.
#[test]
fn performance_marks_and_measures() {
    let code = r#"
const a = performance.now(), b = performance.now();
console.log(a <= b, Math.abs(performance.timeOrigin + performance.now() - Date.now()) < 1000);
const start = performance.mark("start", { startTime: 1, detail: { x: 1 } });
performance.mark("end", { startTime: 5 });
const m = performance.measure("span", "start", "end");
console.log(start instanceof PerformanceEntry, start.detail.x, m.entryType, m.startTime, m.duration);
console.log(performance.getEntries().map(e => e.name).join(), performance.getEntriesByName("span").length);
try { performance.measure("x", "missing") } catch (e) { console.log(e.name, e.message) }
try { new PerformanceEntry() } catch (e) { console.log(e.constructor.name, e.message) }
performance.clearMarks(); console.log(performance.getEntriesByType("mark").length);
"#;
    let out = bun().args(["-e", code]).output().unwrap();
    let (stdout, stderr) = text(&out);
    let expected = "true true
true 1 measure 1 4
start,span,end 1
SyntaxError No mark named 'missing' exists
TypeError Illegal constructor
0
";
    assert_eq!(stdout, expected, "stderr: {stderr}");
}

/// AsyncLocalStorage (src/js/node/async_hooks.ts) on bun_js's async context: a store set by run() is there after an
/// await, in a microtask and in a timer that the callback queued, and not outside of it.
#[test]
fn async_local_storage_carries_across_await_and_timers() {
    let code = r#"
import { AsyncLocalStorage } from "node:async_hooks";
const als = new AsyncLocalStorage();
als.run("a", async () => {
  await null;
  console.log("await", als.getStore());
  queueMicrotask(() => console.log("microtask", als.getStore()));
  setTimeout(() => console.log("timer", als.getStore()), 0);
  Promise.resolve().then(() => als.run("b", () => console.log("nested", als.getStore())));
});
console.log("outside", als.getStore());
"#;
    let out = bun().args(["-e", code]).output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(stdout, "outside undefined\nawait a\nmicrotask a\nnested b\ntimer a\n", "stderr: {stderr}");
}

/// mock.module() through the loader: import and require see the mock, of a missing package and of a builtin.
#[test]
fn mock_module_replaces_modules() {
    let out = bun()
        .args(["test", "mock-module.test.ts"])
        .current_dir(fixtures().join("mock-module"))
        .env("AGENT", "0")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stderr = text(&out).1;
    assert!(stderr.contains(" 4 pass") && stderr.contains(" 0 fail"), "{stderr}");
}

#[test]
fn semver_is_bound_through_bun_js() {
    let out = bun()
        .args(["test", "semver.test.ts"])
        .current_dir(fixtures().join("semver"))
        .output()
        .unwrap();
    let all = format!("{}{}", text(&out).0, text(&out).1);
    assert!(all.contains(" 4 pass") && all.contains(" 0 fail"), "{all}");
    assert!(out.status.success(), "{all}");
}

/// bun:sqlite: src/js/bun/sqlite.ts on bun_sqlite_js, over Turso, and Bun.SQL's sqlite adapter on it.
#[test]
fn sqlite_runs_on_turso() {
    let out = bun()
        .args(["test", "sqlite.test.ts"])
        .current_dir(fixtures().join("sqlite"))
        .output()
        .unwrap();
    let all = format!("{}{}", text(&out).0, text(&out).1);
    assert!(all.contains(" 6 pass") && all.contains(" 0 fail"), "{all}");
    assert!(out.status.success(), "{all}");
}

/// Bun's builtin modules (src/js), `require` and `__dirname` in an ES module, CommonJS modules, tsconfig.json
/// `paths`, the helpers the transpiler imports from `bun:wrap` (`using`), `./x.js` that is `x.ts`, and what of `process`,
/// `Bun` and `AbortSignal` js/bun.ts and runtime_js add.
#[test]
fn builtin_modules_require_and_commonjs() {
    let out = bun().args(["run", "main.ts"]).current_dir(fixtures().join("modules")).output().unwrap();
    let (stdout, stderr) = text(&out);
    let expected = "function / function true true
mapped 1 2 counter.cjs
true 3 modules /
using true
rewritten ts
process number 2 bigint true user,system number true object
bun function /bin/sh number function AbortError
";
    assert_eq!(stdout, expected, "stderr: {stderr}");
    assert!(out.status.success(), "stderr: {stderr}");
}

/// TypeScript drops the imports it does not use, as Bun's runtime transpiler does: a type-only name and a name
/// imported twice.
#[test]
fn typescript_drops_unused_imports() {
    let out = bun().args(["run", "unused_imports.ts"]).current_dir(fixtures().join("modules")).output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(stdout, "string mapped\n", "stderr: {stderr}");
    assert!(out.status.success(), "stderr: {stderr}");
}

/// Workers on threads of their own, and `MessageChannel`/`BroadcastChannel` within a thread and across threads
/// (tests/fixtures/worker/main.ts says what each line checks).
#[test]
fn workers_and_channels() {
    let out = bun().args(["run", "main.ts"]).current_dir(fixtures().join("worker")).output().unwrap();
    let (stdout, stderr) = text(&out);
    let expected = r#"local {"a":[1,2],"m":1}
main true
detached 0
echo [1,2,3]
port pong from worker
terminated 1
busy 1
exit 7
error ["worker failed","TypeError"]
broadcast hi!
node main true 0
node ["ping",{"n":1},false,true]
node exit 0
"#;
    assert_eq!(stdout, expected, "stderr: {stderr}");
    assert!(out.status.success(), "stderr: {stderr}");
}

/// The event loop's order, `Bun.serve` (routes, `fetch`, `error`) answering `fetch()` over loopback, `Headers`,
/// `Request`, and a refused connection's code. The process exits once `using` stops the server.
#[test]
fn serve_and_fetch_on_the_event_loop() {
    let out = bun().args(["run", "serve.ts"]).current_dir(fixtures().join("http")).output().unwrap();
    let (stdout, stderr) = text(&out);
    let expected = r#"microtask immediate timeout interval
ephemeral true second
200 text/plain;charset=utf-8 hello boa from true
201 Created echo payload
application/json;charset=utf-8 {"id":"42"}
static static body
500 caught handler failed
[["a","1, 3"],["b","2"]] 1, 3
POST http://example.test/path text/plain;charset=utf-8
ECONNREFUSED
"#;
    assert_eq!(stdout, expected, "stderr: {stderr}");
    assert!(out.status.success(), "stderr: {stderr}");
}

/// Bun.spawn, Bun.spawnSync, Bun.which and process.kill from bun_runtime_js: fixtures/spawn/spawn.ts prints what
/// Bun on JavaScriptCore prints, expected.txt.
#[test]
fn spawn_and_spawn_sync_answer_as_javascriptcore() {
    let dir = fixtures().join("spawn");
    let out = bun().arg("spawn.ts").current_dir(&dir).output().unwrap();
    let (stdout, stderr) = text(&out);
    let expected = std::fs::read_to_string(dir.join("expected.txt")).unwrap();
    let differ: Vec<String> = expected
        .lines()
        .zip(stdout.lines())
        .filter(|(expected, actual)| expected != actual)
        .map(|(expected, actual)| format!("expected: {expected}\n  actual: {actual}"))
        .collect();
    assert!(differ.is_empty(), "{}\nstderr: {stderr}", differ.join("\n"));
    assert_eq!(stdout.lines().count(), expected.lines().count(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(out.status.success(), "stderr: {stderr}");
}

/// `Buffer` and `node:buffer`: js/buffer.ts on the encoders of bun_runtime_js, and Bun's builtin functions of
/// src/js/builtins (`Buffer.from`, `toJSON`); `TextEncoder` and `TextDecoder` of bun_runtime_js; node:string_decoder
/// over `Buffer`.
#[test]
fn buffer_is_nodes() {
    let out = bun()
        .args(["test", "buffer.test.ts"])
        .current_dir(fixtures().join("buffer"))
        .output()
        .unwrap();
    let all = format!("{}{}", text(&out).0, text(&out).1);
    assert!(all.contains(" 9 pass") && all.contains(" 0 fail"), "{all}");
    assert!(out.status.success(), "{all}");
}
