//! Lean `ay` CLI — Pure-Rust zero-WebKit runtime, wyhash, Rust-lang depot & Cargo.lock auditor (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

mod completions;
mod style;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use ay_depot::{DOCS_SOURCES, RUST_DEPOT_REPOS, analyze_cargo_lock, tuned_cargo_config};
use clap::{CommandFactory, Parser, Subcommand};
use completions::ShellKind;
use serde::Serialize;
use style::{CARGO_STYLING, OutputFormat, emit_error};

#[derive(Parser, Debug)]
#[command(
    name = "ay",
    version = env!("CARGO_PKG_VERSION"),
    about = "Pure-Rust zero-WebKit runtime, depot tools, and workspace CLI",
    styles = CARGO_STYLING,
    arg_required_else_help = true
)]
struct Cli {
    /// Output format (`auto` emits JSON when piped).
    #[arg(short = 'f', long, global = true, default_value = "auto", value_enum)]
    format: OutputFormat,

    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Check toolchains, zero-WebKit invariant, and runtime health.
    Doctor,
    /// Compute Bun Wyhash (64-bit) of a file or UTF-8 string.
    Hash {
        /// File path or literal string (if `--text` is set).
        input: String,
        #[arg(long)]
        text: bool,
    },
    /// List canonical Rust-lang depot repositories and docs-provider sources.
    Depot {
        #[arg(long)]
        docs: bool,
        #[arg(long)]
        cargo_config: Option<String>,
    },
    /// Audit a Cargo.lock for duplicate crate versions and forbidden embedded JS engines.
    LockAudit {
        #[arg(default_value = "Cargo.lock")]
        path: PathBuf,
    },
    /// Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
    Check,
    /// Generate shell completions.
    Completions { shell: ShellKind },
}

#[derive(Serialize)]
struct DoctorOut {
    binary: &'static str,
    version: &'static str,
    webkit: bool,
    jsc: bool,
    cargo: bool,
    bun: bool,
    depot_repos: usize,
    docs_sources: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_mode = cli.format.wants_json(cli.json);

    match cli.command {
        Commands::Doctor => {
            let out = DoctorOut {
                binary: "ay",
                version: env!("CARGO_PKG_VERSION"),
                webkit: false,
                jsc: false,
                cargo: which::which("cargo").is_ok(),
                bun: which::which("bun").is_ok(),
                depot_repos: RUST_DEPOT_REPOS.len(),
                docs_sources: DOCS_SOURCES.len(),
            };
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Commands::Hash { input, text } => {
            let bytes = if text {
                input.into_bytes()
            } else {
                match fs::read(&input) {
                    Ok(b) => b,
                    Err(e) => {
                        return emit_error(
                            "HASH_READ_FAILED",
                            format!("{input}: {e}"),
                            "Pass --text for literal strings",
                            json_mode,
                        );
                    }
                }
            };
            let h = bun_wyhash::hash(&bytes);
            println!(
                "{}",
                serde_json::json!({ "wyhash": format!("0x{h:016x}"), "bytes": bytes.len() })
            );
            ExitCode::SUCCESS
        }
        Commands::Depot { docs, cargo_config } => {
            if let Some(cpu) = cargo_config {
                print!("{}", tuned_cargo_config(&cpu));
            } else if docs {
                println!("{}", serde_json::to_string_pretty(DOCS_SOURCES).unwrap_or_default());
            } else {
                println!("{}", serde_json::to_string_pretty(RUST_DEPOT_REPOS).unwrap_or_default());
            }
            ExitCode::SUCCESS
        }
        Commands::LockAudit { path } => {
            let raw = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return emit_error(
                        "LOCK_READ_FAILED",
                        format!("{}: {e}", path.display()),
                        "Pass path to a valid Cargo.lock",
                        json_mode,
                    );
                }
            };
            let audit = analyze_cargo_lock(&raw);
            let ok = audit.forbidden_engines.is_empty();
            println!("{}", serde_json::to_string_pretty(&audit).unwrap_or_default());
            if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Commands::Check => {
            for args in [
                &["fmt", "--all", "--", "--check"][..],
                &["clippy", "--workspace", "--all-targets", "--no-deps", "--", "-D", "warnings"][..],
                &["test", "--workspace"][..],
            ] {
                let status = Command::new("cargo").args(args).status();
                if !status.is_ok_and(|s| s.success()) {
                    return emit_error(
                        "CARGO_CHECK_FAILED",
                        format!("cargo {} failed", args.join(" ")),
                        "Fix reported diagnostics",
                        json_mode,
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            shell.generate(&mut cmd, &mut std::io::stdout());
            ExitCode::SUCCESS
        }
    }
}
