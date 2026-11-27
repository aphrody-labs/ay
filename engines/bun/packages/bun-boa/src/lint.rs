//! `bun lint`: Bun's linter (`bun_lint_driver::run`) with what `src/runtime/cli/lint_command.rs` gives it, minus
//! JavaScriptCore: JavaScript plugins and configuration files that are programs can't run. Its regular
//! expressions run on regress (`crate::yarr`).

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use bun_core::Output;
use bun_lint_driver::cli::{HELP, Options, UsageError};
use bun_lint_driver::{Environment, Outcome, Stream, bunfig};

use crate::fmt::{NoEngine, columns_of_stdout, run_git, run_script, typescript_libs};

/// The `[lint]` (and `[fmt]`, for the rule that compares with `bun fmt`) of the nearest bunfig.toml that has one.
fn bunfig_defaults(cwd: &[u8]) -> Result<Option<Options>, String> {
    let mut shown = b"bunfig.toml".to_vec();
    let mut at = Some(std::path::Path::new(OsStr::from_bytes(cwd)));
    let (mut lint, mut format) = (None, None);
    while let Some(directory) = at {
        if let Ok(contents) = bun_sys::File::read_from(
            bun_sys::Fd::cwd(),
            directory.join("bunfig.toml").as_os_str().as_encoded_bytes(),
        ) {
            let name = String::from_utf8_lossy(&shown).into_owned();
            bun_ast::initialize_store();
            let bump = bun_alloc::Arena::borrowing_default();
            let source = bun_ast::Source::init_path_string(&shown, &contents);
            let mut log = bun_ast::Log::init();
            let Ok(file) = bun_parsers::toml::TOML::parse(&source, &mut log, &bump, true) else {
                let mut why = String::new();
                let _ = log.print(&mut why);
                return Err(format!("{name}: invalid TOML\n{why}"));
            };
            let fail = |err: bunfig::BunfigError| {
                format!("{name}: {}", String::from_utf8_lossy(&err.message))
            };
            if lint.is_none() {
                lint = bunfig::lint(&file)
                    .map_err(fail)?
                    .map(|it| it.from_bunfig_at(&shown, false, cwd));
            }
            if format.is_none() {
                format = bunfig::format(&file)
                    .map_err(fail)?
                    .map(|it| it.from_bunfig_at(&shown, false));
            }
            if lint.is_some() && format.is_some() {
                break;
            }
        }
        shown.splice(0..0, *b"../");
        at = directory.parent();
    }
    if let Some(format) = format {
        lint.get_or_insert_default().bun_format_defaults = Some(Box::new(format));
    }
    Ok(lint)
}

/// `args` is everything after `lint`. Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    bun_core::output::stdio::init();
    let args: Vec<&[u8]> = args.iter().map(|arg| arg.as_bytes()).collect();
    let parsed = Options::parse(&args);
    if let Ok(options) = &parsed {
        if options.help {
            bun_core::pretty!(
                "<b>Usage<r>: <b><green>bun lint<r> <cyan>[flags]<r> <blue>[...files or directories]<r>\n"
            );
            for group in HELP.groups {
                bun_core::pretty!("\n<b>{}:<r>", group.title);
                bun_clap::simple_help(group.params);
                bun_core::pretty!("\n");
            }
            Output::flush();
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
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd.into_os_string().into_vec(),
        Err(err) => {
            eprintln!("error: Could not read the working directory: {err}");
            return 1;
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
    } = match parsed {
        Err(UsageError(message)) => bun_lint_driver::reject_command_line(&message, &environment),
        Ok(options) => match bunfig_defaults(&environment.cwd) {
            Err(why) => {
                eprintln!("error: {why}");
                return 2;
            }
            Ok(None) => bun_lint_driver::run(&options, &environment),
            Ok(Some(defaults)) => match Options::parse_with_defaults(defaults, &args) {
                Ok(options) => bun_lint_driver::run(&options, &environment),
                Err(UsageError(message)) => {
                    bun_lint_driver::reject_command_line(&message, &environment)
                }
            },
        },
    };
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
