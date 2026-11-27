//! `bun fmt`: Bun's formatter (`bun_lint_driver::fmt`) with what it needs from the process, as
//! `src/runtime/cli/format_command.rs` and `lint_command.rs` provide it, minus JavaScriptCore: a configuration
//! file that is a program (`prettier.config.js`) and JavaScript plugins can't run.

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::Stdio;

use bun_core::Output;
use bun_lint_driver::fmt::cli::{HELP, Options, UsageError};
use bun_lint_driver::js_plugin::{Engine, Vm, Work};
use bun_lint_driver::{Environment, Outcome, Script, Stream, git};

#[path = "../../../src/runtime/cli/typescript_libs.rs"]
pub(crate) mod typescript_libs;

const NO_JS: &[u8] = b"JS plugins need a JS engine";

/// No realm can run JavaScript plugins here.
pub(crate) struct NoEngine;

impl Engine for NoEngine {
    fn thread_has_realm(&self) -> bool {
        false
    }
    fn with_vm_here(
        &self,
        _size: usize,
        _then: &mut dyn FnMut(&mut dyn Vm),
    ) -> Result<(), Vec<u8>> {
        Err(NO_JS.to_vec())
    }
    fn with_vm(
        &self,
        _size: usize,
        _then: &mut (dyn FnMut(&mut dyn Vm) + Send),
    ) -> Result<(), Vec<u8>> {
        Err(NO_JS.to_vec())
    }
    fn work_off(&self, _work: &dyn Work, _meanwhile: &mut dyn FnMut()) -> Result<(), Vec<u8>> {
        Err(NO_JS.to_vec())
    }
    fn realms_that_fit(&self) -> usize {
        0
    }
    fn busy(&self) -> usize {
        0
    }
}

pub(crate) fn run_script(_script: &Script) -> Result<Vec<u8>, Vec<u8>> {
    Err(b"This bun runs on Boa and can't run a configuration file that is a program yet. Use a JSON, YAML or TOML configuration.".to_vec())
}

pub(crate) fn run_git(command: &git::Command) -> Result<git::Finished, git::NotRun> {
    let mut git = std::process::Command::new("git");
    git.args(command.args.iter().map(|arg| OsStr::from_bytes(arg)))
        .current_dir(OsStr::from_bytes(command.cwd))
        .envs(
            git::Command::VARIABLES
                .iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match git.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(git::NotRun::NotFound);
        }
        Err(err) => {
            return Err(git::NotRun::Failed(
                format!("Could not start git: {err}").into_bytes(),
            ));
        }
    };
    let mut stdin = child.stdin.take();
    let input = command.input;
    let output = std::thread::scope(|scope| {
        scope.spawn(move || stdin.as_mut().map(|it| it.write_all(input)));
        child.wait_with_output()
    });
    match output {
        Ok(output) => Ok(git::Finished {
            is_ok: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Err(err) => Err(git::NotRun::Failed(
            format!("git failed: {err}").into_bytes(),
        )),
    }
}

/// `RunCommand::columns_of_stdout`: `$COLUMNS`, then the terminal's width, then 80.
pub(crate) fn columns_of_stdout() -> u16 {
    if let Some(n) = std::env::var("COLUMNS")
        .ok()
        .and_then(|it| it.parse::<u16>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    match bun_core::output::File::from(bun_core::Fd::stdout()).winsize() {
        Some(size) if size.col > 0 => size.col,
        _ => 80,
    }
}

fn usage_error(message: &[u8]) -> i32 {
    eprintln!("error: {}", String::from_utf8_lossy(message));
    eprintln!("note: run 'bun fmt --help' for more information");
    2
}

fn print_help(all: bool) {
    bun_core::pretty!(
        "<b>Usage<r>: <b><green>bun fmt<r> <cyan>[flags]<r> <blue>[...files, directories or patterns]<r>\n  Format JavaScript, TypeScript, CSS, HTML, JSON, YAML, Markdown and more.\n"
    );
    if all {
        for group in HELP.groups {
            bun_core::pretty!("\n<b>{}:<r>", group.title);
            bun_clap::simple_help(group.params);
            bun_core::pretty!("\n");
        }
    } else {
        bun_core::pretty!("\n<b>Flags:<r>");
        bun_clap::simple_help(&HELP.common());
        bun_core::pretty!("\n");
    }
    Output::flush();
}

/// The `[fmt]` section of the nearest bunfig.toml that has one, starting in `cwd`, as defaults for the flags.
fn bunfig_defaults(cwd: &[u8]) -> Result<Option<Options>, String> {
    let mut shown = b"bunfig.toml".to_vec();
    let mut at = Some(std::path::Path::new(OsStr::from_bytes(cwd)));
    while let Some(directory) = at {
        let path = directory.join("bunfig.toml");
        if let Ok(contents) =
            bun_sys::File::read_from(bun_sys::Fd::cwd(), path.as_os_str().as_encoded_bytes())
        {
            if let Some(section) = bunfig_section(&shown, &contents)? {
                return Ok(Some(section.from_bunfig_at(&shown, false)));
            }
        }
        shown.splice(0..0, *b"../");
        at = directory.parent();
    }
    Ok(None)
}

pub(crate) fn bunfig_section(shown: &[u8], contents: &[u8]) -> Result<Option<Options>, String> {
    let name = String::from_utf8_lossy(shown);
    bun_ast::initialize_store();
    let bump = bun_alloc::Arena::borrowing_default();
    let source = bun_ast::Source::init_path_string(shown, contents);
    let mut log = bun_ast::Log::init();
    let file = match bun_parsers::toml::TOML::parse(&source, &mut log, &bump, true) {
        Ok(file) => file,
        Err(_) => {
            let mut why = String::new();
            let _ = log.print(&mut why);
            return Err(format!("{name}: invalid TOML\n{why}"));
        }
    };
    bun_lint_driver::bunfig::format(&file)
        .map_err(|err| format!("{name}: {}", String::from_utf8_lossy(&err.message)))
}

/// `args` is everything after `fmt`. Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    bun_core::output::stdio::init();
    let args: Vec<&[u8]> = args.iter().map(|arg| arg.as_bytes()).collect();
    let options = match Options::parse(&args) {
        Ok(options) => options,
        Err(UsageError(message)) => return usage_error(&message),
    };
    if options.help {
        print_help(options.all);
        return 0;
    }
    if let Some(cwd) = &options.cwd
        && let Err(err) = std::env::set_current_dir(OsStr::from_bytes(cwd))
    {
        eprintln!(
            "error: Could not change directory to \"{}\": {err}",
            String::from_utf8_lossy(cwd)
        );
        return 1;
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd.into_os_string().into_vec(),
        Err(err) => {
            eprintln!("error: Could not read the working directory: {err}");
            return 1;
        }
    };
    let options = match bunfig_defaults(&cwd) {
        Ok(None) => options,
        Ok(Some(defaults)) => match Options::parse_with_defaults(defaults, &args) {
            Ok(options) => options,
            Err(UsageError(message)) => return usage_error(&message),
        },
        Err(why) => {
            eprintln!("error: {why}");
            return 2;
        }
    };
    bun_paths::fs::FileSystem::init(&cwd);
    let js_engine = NoEngine;
    let environment = Environment {
        cwd: bun_lint_driver::from_native_path(&cwd),
        stdout: Stream {
            is_tty: Output::is_stdout_tty(),
            colors: Output::enable_ansi_colors_stdout(),
        },
        stderr: Stream {
            is_tty: Output::is_stderr_tty(),
            colors: Output::enable_ansi_colors_stderr(),
        },
        columns: columns_of_stdout,
        is_ai_agent: Output::is_ai_agent(),
        is_github_action: Output::is_github_action(),
        libs: bun_sema_driver::Libs::Bundled(typescript_libs::BUNDLED),
        run_script: &run_script,
        run_git: &run_git,
        command_line: &args,
        js_engine: &js_engine,
        version: bun_core::Global::package_json_version.as_bytes(),
        memory: 0,
    };
    let Outcome {
        stdout,
        stderr,
        exit_code,
    } = bun_lint_driver::fmt::run(&options, &environment);
    let _ = std::io::stderr().write_all(&stderr);
    if let Err(err) = std::io::stdout()
        .write_all(&stdout)
        .and_then(|()| std::io::stdout().flush())
        && err.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("error: Cannot write standard output: {err}");
        return 2;
    }
    i32::from(exit_code)
}
