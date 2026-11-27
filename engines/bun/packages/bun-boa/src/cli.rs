//! `bun [run] <file> [args…]`, `bun -e <code>`, `bun run <script>`, `bun fmt`, `bun lint`, `bun test`, `bun --version`,
//! `bun --revision`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::builtins::promise::PromiseState;
use boa_engine::{Context, JsError, JsValue, Module, Source, js_string};

use crate::loader::BunModuleLoader;

const USAGE: &str = "Usage: bun <command> [...flags] [...args]

Commands:
  run       ./my-script.ts       Execute a file with Bun
  <file>    ./my-script.ts       Same as `bun run`
  -e        'console.log(1)'     Evaluate source code
  run       dev                  Run a package.json script
  fmt       ./src                Format files with Bun's formatter
  lint      ./src                Lint files with Bun's linter
  test      [filters]            Run tests with bun:test
  --version                      Print the version
  --revision                     Print the version with its git revision
";

fn report(err: &JsError, context: &mut Context) {
    let value = err
        .clone()
        .into_opaque(context)
        .unwrap_or_else(|e| JsValue::from(js_string!(e.to_string())));
    let stack = value
        .as_object()
        .and_then(|o| o.get(js_string!("stack"), context).ok())
        .filter(JsValue::is_string);
    let text = match stack {
        Some(stack) => stack.display().to_string(),
        None => value.display().to_string(),
    };
    eprintln!("error: {}", text.trim_matches('"'));
}

fn exit_code(context: &mut Context) -> i32 {
    let process = context
        .global_object()
        .get(js_string!("process"), context)
        .ok();
    process
        .and_then(|p| {
            p.as_object()
                .and_then(|o| o.get(js_string!("exitCode"), context).ok())
        })
        .and_then(|code| code.as_number())
        .map_or(0, |code| code as i32)
}

/// Evaluates the entry point, then runs the event loop until nothing keeps it alive, as Bun does.
fn evaluate(module: Module, context: &mut Context) -> i32 {
    let promise = module.load_link_evaluate(context);
    let event_loop = bun_runtime_js::EventLoop::of(context);
    let settled = || !matches!(promise.state(), PromiseState::Pending);
    if let Err(err) = event_loop.run_until(context, settled) {
        report(&err, context);
        return 1;
    }
    match promise.state() {
        PromiseState::Fulfilled(_) => {
            if let Err(err) = event_loop.run(context) {
                report(&err, context);
                return 1;
            }
            exit_code(context)
        }
        PromiseState::Rejected(reason) => {
            report(&JsError::from_opaque(reason), context);
            1
        }
        PromiseState::Pending => {
            eprintln!("error: top-level await never resolved");
            13
        }
    }
}

fn run_file(file: &str, rest: &[String]) -> i32 {
    let Some(path) = crate::loader::resolve(file, &std::env::current_dir().unwrap_or_default())
        .or_else(|| Path::new(file).canonicalize().ok())
    else {
        eprintln!("error: Module not found \"{file}\"");
        return 1;
    };
    let argv = [
        std::env::args().next().unwrap_or_else(|| "bun".into()),
        path.display().to_string(),
    ]
    .into_iter()
    .chain(rest.iter().cloned())
    .collect();
    let loader = Rc::new(BunModuleLoader::default());
    let mut context = match crate::runtime::context(loader.clone(), Some(path.clone()), argv) {
        Ok(context) => context,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };
    match loader.load(&path, &mut context) {
        Ok(module) => evaluate(module, &mut context),
        Err(err) => {
            report(&err, &mut context);
            1
        }
    }
}

fn eval(code: &str, rest: &[String]) -> i32 {
    let argv = [std::env::args().next().unwrap_or_else(|| "bun".into())]
        .into_iter()
        .chain(rest.iter().cloned())
        .collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let path: PathBuf = cwd.join("[eval].ts");
    let loader = Rc::new(BunModuleLoader::default());
    let mut context = match crate::runtime::context(loader, None, argv) {
        Ok(context) => context,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };
    let js = match crate::transpile::transpile(path.as_os_str().as_encoded_bytes(), code.as_bytes())
    {
        Ok(js) => js,
        Err(err) => {
            eprint!("{err}");
            return 1;
        }
    };
    match Module::parse(Source::from_bytes(&js).with_path(&path), None, &mut context) {
        Ok(module) => evaluate(module, &mut context),
        Err(err) => {
            report(&err, &mut context);
            1
        }
    }
}

/// A `package.json` script of that name first, as `bun run` does, then a file. A path (`./x`, `../x`, `/x`)
/// is always a file, without reading `package.json`, as `RunCommand` does.
fn script_or_file(name: &str, rest: &[String]) -> i32 {
    if crate::scripts::is_path(name) {
        return run_file(name, rest);
    }
    crate::scripts::run(name, rest).unwrap_or_else(|| run_file(name, rest))
}

pub fn main() -> i32 {
    // As Bun's `main`: the threads of the work pool configure their output from it.
    bun_core::output::stdio::init();
    crate::worker::install();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            0
        }
        Some("-v" | "--version") => {
            println!("{}", bun_core::package_json_version);
            0
        }
        Some("--revision") => {
            println!("{}", bun_core::package_json_version_with_revision);
            0
        }
        Some("-e" | "--eval" | "-p" | "--print") => match args.get(1) {
            Some(code) => eval(code, &args[2..]),
            None => {
                eprintln!("error: {} requires an argument", args[0]);
                1
            }
        },
        Some("fmt") => crate::fmt::main(&args[1..]),
        Some("lint") => crate::lint::main(&args[1..]),
        Some("test") => crate::test::main(&args[1..]),
        #[cfg(feature = "build")]
        Some("build") => crate::build::main(&args[1..]),
        #[cfg(feature = "install")]
        Some(command) if let Some(subcommand) = crate::install::subcommand(command, &args[1..]) => {
            crate::install::main(subcommand)
        }
        Some("run") => match args.get(1) {
            Some(name) => script_or_file(name, &args[2..]),
            None => crate::scripts::list(),
        },
        Some(name) => script_or_file(name, &args[1..]),
    }
}
