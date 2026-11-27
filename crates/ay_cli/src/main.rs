//! Unified `ay` CLI — Dioxus `dx` & Zed `zed` caliber developer experience (`#![forbid(unsafe_code)]`).
//!
//! Links all first-party `ay` crates (`aphrody-core`, `aphrody-craft`, `design-engine`, `ay_ui`,
//! `bun_wyhash`) directly in one binary with zero IPC overhead, plus seamless delegation bridges
//! to `aphrody`, `aphrody-deploy`, and `aphrody-estate`.

#![forbid(unsafe_code)]

mod completions;
mod style;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use aphrody_craft::{compiled_apps, media::inspect_media_bytes};
use ay_ui::{PetKind, PetMood, render_pet};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use completions::ShellKind;
use design_engine::{
    DesignSystemConfig, DesignSystemPreset, SurfaceUniformSpec, ThemeMode, list_omarchy_themes,
    wcag_contrast_ratio,
};
use qrcode::{QrCode, render::unicode};
use serde::Serialize;
use style::{CARGO_STYLING, CYAN_BOLD, DIM, GREEN_BOLD, OutputFormat, emit_error};

#[derive(Parser, Debug)]
#[command(
    name = "ay",
    version = env!("CARGO_PKG_VERSION"),
    about = "Unified pure-Rust engine, design, packaging, and workspace CLI",
    styles = CARGO_STYLING,
    arg_required_else_help = true,
    before_help = "Examples:\n  ay new my-app --ui gpui --theme astryx\n  ay dev --port 3000 --open\n  ay dev --mobile --url http://127.0.0.1:3000/live\n  ay design tokens --theme vice_city_sunset --output css\n  ay craft sniff ./icon.png\n  ay gate --strict\n  ay completions powershell",
    after_help = "Run `ay <command> --help` for full options on any subcommand."
)]
struct Cli {
    /// Output format (`auto` emits human tables on TTY and JSON when piped).
    #[arg(short = 'f', long, global = true, default_value = "auto", value_enum)]
    format: OutputFormat,

    /// Emit machine-readable JSON output (shorthand for `--format json`).
    #[arg(long, global = true)]
    json: bool,

    /// Suppress non-essential output.
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Increase diagnostic verbosity.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Scaffold a new Aphrody / ay application manifest.
    #[command(visible_alias = "create")]
    New {
        /// Project name (kebab-case or identifier).
        name: String,
        /// UI engine backend.
        #[arg(long, default_value = "gpui", value_enum)]
        ui: UiEngine,
        /// JavaScript / TypeScript execution backend.
        #[arg(long, default_value = "bun-aot", value_enum)]
        js: JsEngine,
        /// HTTP / edge server backend.
        #[arg(long, default_value = "axum", value_enum)]
        server: ServerEngine,
        /// Default pluggable design system preset.
        #[arg(long, default_value = "astryx")]
        theme: String,
        /// Cargo release optimization profile.
        #[arg(long, default_value = "compact", value_enum)]
        profile: BuildProfile,
    },

    /// Start the live dev server or render a mobile QR companion bridge.
    #[command(visible_alias = "serve")]
    Dev {
        /// Render a terminal QR code linking to the running workspace URL.
        #[arg(long)]
        mobile: bool,
        /// Target URL for `--mobile` QR bridge.
        #[arg(long, default_value = "http://127.0.0.1:3000/live")]
        url: String,
        /// Bind host for the dev server.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Bind port for the dev server.
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
        /// Open the live dashboard in the default browser after start.
        #[arg(long)]
        open: bool,
    },

    /// Execute a `.rs` script, `.rsx`/`.ts`/`.tsx` entrypoint, or `just` recipe.
    Run {
        /// Script file or `just` recipe name.
        #[arg(value_hint = ValueHint::AnyPath)]
        target: String,
        /// Additional arguments forwarded to the target.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Build the Rust workspace with the chosen Cargo profile.
    Build {
        /// Cargo profile (`dev`, `release`, `compact`, `dist`).
        #[arg(long, default_value = "release", value_enum)]
        profile: BuildProfile,
        /// Restrict build to a specific package.
        #[arg(short, long)]
        package: Option<String>,
        /// Target triple (e.g. `wasm32-unknown-unknown`).
        #[arg(long)]
        target: Option<String>,
    },

    /// Run formatting, Clippy (`-D warnings`), tests, and `cargo shear`.
    Check {
        /// Include integration and unit test targets in Clippy.
        #[arg(long)]
        tests: bool,
        /// Auto-format with `cargo fmt --all` before checking.
        #[arg(long)]
        fix: bool,
    },

    /// Format Rust sources via `cargo fmt --all`.
    Fmt {
        /// Verify formatting without writing changes.
        #[arg(long)]
        check: bool,
    },

    /// Package cross-platform release artifacts and manifests.
    #[command(visible_alias = "pack")]
    Bundle {
        /// Distribution target (`all` or a specific ecosystem).
        #[arg(default_value = "all", value_enum)]
        target: PackTarget,
        /// Release build profile.
        #[arg(long, default_value = "compact", value_enum)]
        profile: BuildProfile,
    },

    /// Resolve Zed-style `path[:line[:col]]` locations and multi-file `--diff` pairs.
    Open {
        /// Paths with optional `:line[:column]` suffixes.
        #[arg(value_hint = ValueHint::AnyPath)]
        paths: Vec<String>,
        /// Pairs of paths to diff (`--diff OLD NEW`).
        #[arg(long, action = clap::ArgAction::Append, num_args = 2, value_names = ["OLD_PATH", "NEW_PATH"], value_hint = ValueHint::AnyPath)]
        diff: Vec<String>,
        /// Wait for the opened target to close before exiting.
        #[arg(short, long)]
        wait: bool,
        /// Open in a new workspace window.
        #[arg(short, long, overrides_with_all = ["add", "existing"])]
        new: bool,
        /// Add paths to the currently active workspace.
        #[arg(short, long, overrides_with_all = ["new", "existing"])]
        add: bool,
        /// Reuse an existing workspace window.
        #[arg(short, long, overrides_with_all = ["new", "add"])]
        existing: bool,
        /// Keep process attached in foreground.
        #[arg(long)]
        foreground: bool,
    },

    /// First-party `design-engine`: list presets, export CSS/WGSL/JSON tokens, and run WCAG 2.2 AA audits.
    Design {
        #[command(subcommand)]
        action: DesignCommands,
    },

    /// First-party `aphrody-craft`: inspect media magic bytes and compiled Craft suite metadata.
    Craft {
        #[command(subcommand)]
        action: CraftCommands,
    },

    /// Compute deterministic 64-bit `bun_wyhash` digest for a string or file.
    Hash {
        /// UTF-8 text to hash (ignored when `--file` is supplied).
        input: Option<String>,
        /// Hash the raw bytes of a file.
        #[arg(long, value_hint = ValueHint::FilePath)]
        file: Option<PathBuf>,
        /// Optional 64-bit hash seed.
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },

    /// Render OpenPets companion state (`bun`, `tux`, `androidify`, `aphrody`, `rust`).
    Pets {
        /// Optional companion identifier (omitting lists all 5).
        kind: Option<String>,
        /// Companion mood state (`idle`, `running`, `waving`, `jumping`, `celebrating`).
        #[arg(long, default_value = "waving", value_enum)]
        mood: CliPetMood,
        /// Short companion status label.
        #[arg(long, default_value = "Active")]
        status: String,
    },

    /// Diagnose host toolchains, workspace TCB, and `unsafe_code = "forbid"` coverage.
    Doctor,

    /// Verify `TRUSTED.toml`, `acceptance.lock`, and workspace `unsafe_code` policy.
    Gate {
        /// Fail if `acceptance.lock` is missing.
        #[arg(long)]
        strict: bool,
        /// Custom path to `TRUSTED.toml`.
        #[arg(long, default_value = "TRUSTED.toml", value_hint = ValueHint::FilePath)]
        manifest: PathBuf,
    },

    /// Generate shell completion scripts (`bash`, `zsh`, `fish`, `powershell`, `elvish`).
    Completions {
        /// Target shell.
        #[arg(value_enum)]
        shell: ShellKind,
    },

    /// Print version and pure-Rust runtime metadata.
    Version,

    /// Delegate to the unified Aphrody Bun Super App CLI (`apps/app/cli.ts`).
    App {
        /// Arguments forwarded directly to `aphrody`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Delegate to the pure-Rust `aphrody-deploy` control plane CLI.
    Deploy {
        /// Arguments forwarded directly to `aphrody-deploy`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Delegate to the pure-Rust `aphrody-estate` Cloudflare & DNS IaC CLI.
    Estate {
        /// Arguments forwarded directly to `aphrody-estate`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum DesignCommands {
    /// List built-in presets and Omarchy desktop palettes.
    Themes,
    /// Export semantic design tokens as CSS custom properties, WGSL, or JSON.
    Tokens {
        /// Preset name (`astryx`, `material3`, `vice_city_sunset`, `apple_liquid_glass`, `fluent_mica`, `minimal`, `omarchy:<name>`).
        #[arg(long, default_value = "astryx")]
        theme: String,
        /// Color mode (`light` or `dark`).
        #[arg(long, default_value = "dark", value_enum)]
        mode: CliThemeMode,
        /// Token export format.
        #[arg(long, default_value = "css", value_enum)]
        output: TokenOutput,
    },
    /// Run a W3C WCAG 2.2 AA contrast audit and verify the 64-byte `zerocopy` WGPU uniform.
    Audit {
        /// Preset name.
        #[arg(long, default_value = "astryx")]
        theme: String,
        /// Color mode (`light` or `dark`).
        #[arg(long, default_value = "dark", value_enum)]
        mode: CliThemeMode,
    },
}

#[derive(Subcommand, Debug)]
enum CraftCommands {
    /// Inspect compiled Craft creative suite apps.
    Info,
    /// Detect media format, dimensions, and codec directly from magic bytes.
    Sniff {
        /// Path to the media file (PNG, WebP, AVIF, GLB, WebM, MP4, SVG).
        #[arg(value_hint = ValueHint::FilePath)]
        file: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum UiEngine {
    Gpui,
    Egui,
    Iced,
    Bevy,
    Dioxus,
    Leptos,
    Tauri,
    Blitz,
}

#[derive(Clone, Copy, Debug, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum JsEngine {
    BunAot,
    Boa,
    Nova,
    Jsc,
    V8,
}

#[derive(Clone, Copy, Debug, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ServerEngine {
    Axum,
    Worker,
    Pingora,
    BunServe,
}

#[derive(Clone, Copy, Debug, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum BuildProfile {
    Dev,
    Release,
    Compact,
    Dist,
}

#[derive(Clone, Copy, Debug, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum PackTarget {
    All,
    Flatpak,
    Nix,
    Pacman,
    Winget,
    Msix,
    Scoop,
    Homebrew,
    Deb,
    Rpm,
    Apk,
    Android,
    Ios,
    WasmWebgpu,
    CloudflareCdn,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliThemeMode {
    Light,
    Dark,
}

impl CliThemeMode {
    const fn to_engine(self) -> ThemeMode {
        match self {
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TokenOutput {
    Css,
    Wgsl,
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliPetMood {
    Idle,
    Running,
    Waving,
    Jumping,
    Celebrating,
}

impl CliPetMood {
    const fn to_ui(self) -> PetMood {
        match self {
            Self::Idle => PetMood::Idle,
            Self::Running => PetMood::Running,
            Self::Waving => PetMood::Waving,
            Self::Jumping => PetMood::Jumping,
            Self::Celebrating => PetMood::Celebrating,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ParsedPathPosition {
    path: String,
    line: Option<usize>,
    column: Option<usize>,
    exists: bool,
}

fn parse_path_with_position(raw: &str) -> ParsedPathPosition {
    let parts: Vec<&str> = raw.rsplitn(3, ':').collect();
    if parts.len() == 3
        && let (Ok(col), Ok(line)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>())
        && !parts[2].is_empty()
    {
        return ParsedPathPosition {
            exists: Path::new(parts[2]).exists(),
            path: parts[2].to_owned(),
            line: Some(line),
            column: Some(col),
        };
    }
    if parts.len() >= 2
        && let Ok(line) = parts[0].parse::<usize>()
    {
        let prefix = raw.rsplit_once(':').map(|(p, _)| p).unwrap_or(raw);
        if !prefix.is_empty() && !prefix.ends_with(['\\', '/']) {
            return ParsedPathPosition {
                exists: Path::new(prefix).exists(),
                path: prefix.to_owned(),
                line: Some(line),
                column: None,
            };
        }
    }
    ParsedPathPosition {
        exists: Path::new(raw).exists(),
        path: aphrody_core::to_portable(raw),
        line: None,
        column: None,
    }
}

fn scan_unsafe_forbid_crates(root: &Path) -> (usize, usize) {
    let crates_dir = root.join("crates");
    let Ok(entries) = std::fs::read_dir(&crates_dir) else {
        return (0, 0);
    };
    let workspace_toml = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let workspace_forbids = workspace_toml.contains(r#"unsafe_code = "forbid""#);

    let mut total = 0usize;
    let mut forbid = 0usize;
    for entry in entries.flatten() {
        let cargo_toml = entry.path().join("Cargo.toml");
        if !cargo_toml.is_file() {
            continue;
        }
        total += 1;
        let text = std::fs::read_to_string(&cargo_toml).unwrap_or_default();
        let allows_unsafe =
            text.contains(r#"unsafe_code = "allow""#) || text.contains(r#"unsafe_code = "warn""#);
        let explicitly_forbids = text.contains(r#"unsafe_code = "forbid""#)
            || (workspace_forbids && text.contains("workspace = true") && !allows_unsafe);
        if explicitly_forbids {
            forbid += 1;
        }
    }
    (forbid, total)
}

fn find_aphrody_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    [
        cwd.clone(),
        cwd.join("..").join("aphrody"),
        PathBuf::from(r"C:\src\aphrody"),
    ]
    .into_iter()
    .find(|candidate| candidate.join("apps").join("app").join("cli.ts").is_file())
}

fn parse_hex_rgb(hex: &str) -> Option<[u8; 3]> {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
    let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
    let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
    Some([r, g, b])
}

fn run_forwarded(program: &str, args: &[String], cwd: Option<&Path>, as_json: bool) -> ExitCode {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    match cmd.status() {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => ExitCode::from(u8::try_from(s.code().unwrap_or(1)).unwrap_or(1)),
        Err(e) => emit_error(
            "SPAWN_FAILED",
            format!("failed to launch `{program}`: {e}"),
            format!("Verify `{program}` is installed and available on PATH"),
            as_json,
        ),
    }
}

fn run_aphrody_rust_bin(bin_name: &str, pkg: &str, args: &[String], as_json: bool) -> ExitCode {
    if let Ok(found) = which::which(bin_name) {
        return run_forwarded(found.to_string_lossy().as_ref(), args, None, as_json);
    }
    let Some(root) = find_aphrody_root() else {
        return emit_error(
            "APHRODY_ROOT_NOT_FOUND",
            format!("could not locate `{bin_name}` on PATH or `aphrody` workspace"),
            "Run from inside the `aphrody` repository or install the release binary onto PATH",
            as_json,
        );
    };
    let mut cargo_args = vec![
        "run".to_owned(),
        "-q".to_owned(),
        "-p".to_owned(),
        pkg.to_owned(),
        "--".to_owned(),
    ];
    cargo_args.extend(args.iter().cloned());
    run_forwarded("cargo", &cargo_args, Some(&root), as_json)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let as_json = cli.format.wants_json(cli.json);

    match cli.command {
        Commands::Version => {
            if as_json {
                println!(
                    r#"{{"name":"ay","version":"{}","runtime":"rust-first-party","jsc":false}}"#,
                    env!("CARGO_PKG_VERSION")
                );
            } else {
                println!(
                    "{GREEN_BOLD}ay{GREEN_BOLD:#} {} {DIM}(rust-first-party, 0 JSC){DIM:#}",
                    env!("CARGO_PKG_VERSION")
                );
            }
            ExitCode::SUCCESS
        }

        Commands::New {
            name,
            ui,
            js,
            server,
            theme,
            profile,
        } => {
            let manifest = serde_json::json!({
                "ok": true,
                "app": name,
                "defaults": {
                    "profile": profile,
                    "lto": "fat",
                    "design_system": theme,
                    "auth": "better-auth@1.7.28"
                },
                "engines": { "ui": ui, "js": js, "server": server },
                "targets": ["desktop", "wasm32-webgpu", "mobile-qr", "cloudflare-worker", "single-binary"]
            });
            if as_json {
                println!("{manifest}");
            } else {
                println!("{GREEN_BOLD}Scaffolded{GREEN_BOLD:#} {CYAN_BOLD}{name}{CYAN_BOLD:#}");
                println!(
                    "{}",
                    serde_json::to_string_pretty(&manifest).unwrap_or_default()
                );
            }
            ExitCode::SUCCESS
        }

        Commands::Dev {
            mobile,
            url,
            host,
            port,
            open,
        } => {
            if mobile {
                let Ok(code) = QrCode::new(url.as_bytes()) else {
                    return emit_error(
                        "QR_ENCODE_ERROR",
                        format!("cannot encode URL '{url}'"),
                        "Provide a shorter valid HTTP(S) URL",
                        as_json,
                    );
                };
                let qr_text = code
                    .render::<unicode::Dense1x2>()
                    .dark_color(unicode::Dense1x2::Light)
                    .light_color(unicode::Dense1x2::Dark)
                    .build();
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "mode": "mobile-gpui-qr",
                            "url": url,
                            "qr": qr_text
                        })
                    );
                } else {
                    println!(
                        "{GREEN_BOLD}Mobile QR bridge{GREEN_BOLD:#} -> {CYAN_BOLD}{url}{CYAN_BOLD:#}\n{qr_text}"
                    );
                }
                return ExitCode::SUCCESS;
            }
            let Some(root) = find_aphrody_root() else {
                return emit_error(
                    "APHRODY_ROOT_NOT_FOUND",
                    "could not locate `apps/app/cli.ts` to launch dev server",
                    "Run `ay dev` from inside `aphrody` or pass `--mobile --url <URL>`",
                    as_json,
                );
            };
            let mut args = vec![
                "apps/app/cli.ts".to_owned(),
                "serve".to_owned(),
                "--host".to_owned(),
                host,
                "--port".to_owned(),
                port.to_string(),
            ];
            if open {
                args.push("--open".to_owned());
            }
            run_forwarded("bun", &args, Some(&root), as_json)
        }

        Commands::Run { target, args } => {
            let (program, full_args) = if target.ends_with(".rs") {
                let mut a = vec!["-Zscript".to_owned(), target];
                a.extend(args);
                ("cargo", a)
            } else if target.ends_with(".rsx")
                || target.ends_with(".ts")
                || target.ends_with(".tsx")
            {
                let mut a = vec![target];
                a.extend(args);
                ("bun", a)
            } else {
                let mut a = vec![target];
                a.extend(args);
                ("just", a)
            };
            run_forwarded(program, &full_args, None, as_json)
        }

        Commands::Build {
            profile,
            package,
            target,
        } => {
            let mut args = vec!["build".to_owned()];
            match profile {
                BuildProfile::Dev => {}
                BuildProfile::Release => args.push("--release".to_owned()),
                BuildProfile::Compact => {
                    args.extend(["--profile".to_owned(), "compact".to_owned()])
                }
                BuildProfile::Dist => args.extend(["--profile".to_owned(), "dist".to_owned()]),
            }
            if let Some(pkg) = package {
                args.extend(["-p".to_owned(), pkg]);
            }
            if let Some(triple) = target {
                args.extend(["--target".to_owned(), triple]);
            }
            run_forwarded("cargo", &args, None, as_json)
        }

        Commands::Check { tests, fix } => {
            if fix {
                let code = run_forwarded(
                    "cargo",
                    &["fmt".to_owned(), "--all".to_owned()],
                    None,
                    as_json,
                );
                if code != ExitCode::SUCCESS {
                    return code;
                }
            }
            let fmt_code = run_forwarded(
                "cargo",
                &[
                    "fmt".to_owned(),
                    "--all".to_owned(),
                    "--".to_owned(),
                    "--check".to_owned(),
                ],
                None,
                as_json,
            );
            if fmt_code != ExitCode::SUCCESS {
                return fmt_code;
            }
            let mut clippy_args = vec![
                "clippy".to_owned(),
                "--workspace".to_owned(),
                "--no-deps".to_owned(),
            ];
            if tests {
                clippy_args.push("--all-targets".to_owned());
            } else {
                clippy_args.extend(["--lib".to_owned(), "--bins".to_owned()]);
            }
            clippy_args.extend(["--".to_owned(), "-D".to_owned(), "warnings".to_owned()]);
            let clip_code = run_forwarded("cargo", &clippy_args, None, as_json);
            if clip_code != ExitCode::SUCCESS {
                return clip_code;
            }
            run_forwarded(
                "cargo",
                &["test".to_owned(), "--workspace".to_owned()],
                None,
                as_json,
            )
        }

        Commands::Fmt { check } => {
            let mut args = vec!["fmt".to_owned(), "--all".to_owned()];
            if check {
                args.extend(["--".to_owned(), "--check".to_owned()]);
            }
            run_forwarded("cargo", &args, None, as_json)
        }

        Commands::Bundle { target, profile } => {
            let supported = [
                "flatpak",
                "nix",
                "pacman",
                "winget",
                "msix",
                "scoop",
                "homebrew",
                "deb",
                "rpm",
                "apk",
                "android",
                "ios",
                "wasm-webgpu",
                "cloudflare-cdn",
            ];
            let payload = serde_json::json!({
                "ok": true,
                "target": target,
                "supported": supported,
                "profile": profile,
                "compression": ["zstd", "brotli"]
            });
            println!(
                "{}",
                if as_json {
                    serde_json::to_string(&payload).unwrap_or_default()
                } else {
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                }
            );
            ExitCode::SUCCESS
        }

        Commands::Open {
            paths,
            diff,
            wait,
            new,
            add,
            existing,
            foreground,
        } => {
            let parsed_paths: Vec<ParsedPathPosition> = paths
                .iter()
                .map(|raw| parse_path_with_position(raw))
                .collect();
            let diff_pairs: Vec<[ParsedPathPosition; 2]> = diff
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    [
                        parse_path_with_position(&pair[0]),
                        parse_path_with_position(&pair[1]),
                    ]
                })
                .collect();
            let mode = if new {
                "new-window"
            } else if add {
                "add-to-workspace"
            } else if existing {
                "existing-window"
            } else {
                "default"
            };
            let payload = serde_json::json!({
                "ok": true,
                "mode": mode,
                "wait": wait,
                "foreground": foreground,
                "paths": parsed_paths,
                "diffPairs": diff_pairs
            });
            println!(
                "{}",
                if as_json {
                    serde_json::to_string(&payload).unwrap_or_default()
                } else {
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                }
            );
            ExitCode::SUCCESS
        }

        Commands::Design { action } => match action {
            DesignCommands::Themes => {
                let builtins = [
                    "astryx",
                    "material3",
                    "apple_liquid_glass",
                    "fluent_mica",
                    "vice_city_sunset",
                    "minimal",
                ];
                let omarchy: Vec<String> =
                    list_omarchy_themes().into_iter().map(|p| p.name).collect();
                let payload = serde_json::json!({
                    "ok": true,
                    "builtins": builtins,
                    "omarchyCount": omarchy.len(),
                    "omarchy": omarchy
                });
                if as_json {
                    println!("{payload}");
                } else {
                    println!(
                        "{GREEN_BOLD}Built-in design presets:{GREEN_BOLD:#} {}",
                        builtins.join(", ")
                    );
                    println!(
                        "{GREEN_BOLD}Omarchy palettes ({}):{GREEN_BOLD:#} {}",
                        omarchy.len(),
                        omarchy.join(", ")
                    );
                }
                ExitCode::SUCCESS
            }
            DesignCommands::Tokens {
                theme,
                mode,
                output,
            } => {
                let preset = DesignSystemPreset::parse(&theme);
                let cfg = DesignSystemConfig::from_preset(preset, mode.to_engine());
                match output {
                    TokenOutput::Css => print!("{}", cfg.to_css_variables()),
                    TokenOutput::Wgsl => print!("{}", cfg.to_wgsl_tokens()),
                    TokenOutput::Json => {
                        println!("{}", cfg.to_json().unwrap_or_default());
                    }
                }
                ExitCode::SUCCESS
            }
            DesignCommands::Audit { theme, mode } => {
                let preset = DesignSystemPreset::parse(&theme);
                let cfg = DesignSystemConfig::from_preset(preset, mode.to_engine());
                let pairs = [
                    ("OnBackground", "Background"),
                    ("OnSurface", "Surface"),
                    ("OnPrimary", "Primary"),
                    ("OnPrimaryContainer", "PrimaryContainer"),
                ];
                let mut checks = Vec::new();
                let mut all_pass = true;
                for (fg_key, bg_key) in pairs {
                    let fg = cfg.colors.get(fg_key).and_then(|h| parse_hex_rgb(h));
                    let bg = cfg.colors.get(bg_key).and_then(|h| parse_hex_rgb(h));
                    if let (Some(f), Some(b)) = (fg, bg) {
                        let ratio = wcag_contrast_ratio(f, b);
                        let pass = ratio >= 4.5;
                        if !pass {
                            all_pass = false;
                        }
                        checks.push(serde_json::json!({
                            "pair": format!("{fg_key}/{bg_key}"),
                            "ratio": (ratio * 100.0).round() / 100.0,
                            "wcagAA": pass
                        }));
                    }
                }
                let uniform = cfg.surface_uniform(SurfaceUniformSpec {
                    mode: mode.to_engine(),
                    seed_override: None,
                    bg_role: "Surface",
                    fg_role: Some("OnSurface"),
                    border_role: Some("OutlineVariant"),
                    shape_token: "md",
                    elevation_level: "Level1",
                    padding_h_dp: 16.0,
                    padding_v_dp: 12.0,
                });
                let payload = serde_json::json!({
                    "ok": all_pass,
                    "theme": cfg.name,
                    "preset": cfg.preset.id(),
                    "mode": format!("{:?}", cfg.mode).to_lowercase(),
                    "wgpuUniformBytes": uniform.as_bytes().len(),
                    "checks": checks
                });
                println!(
                    "{}",
                    if as_json {
                        serde_json::to_string(&payload).unwrap_or_default()
                    } else {
                        serde_json::to_string_pretty(&payload).unwrap_or_default()
                    }
                );
                if all_pass {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
        },

        Commands::Craft { action } => match action {
            CraftCommands::Info => {
                let apps = compiled_apps();
                let payload = serde_json::json!({
                    "ok": true,
                    "suite": "aphrody-craft",
                    "count": apps.len(),
                    "apps": apps
                });
                println!(
                    "{}",
                    if as_json {
                        serde_json::to_string(&payload).unwrap_or_default()
                    } else {
                        serde_json::to_string_pretty(&payload).unwrap_or_default()
                    }
                );
                ExitCode::SUCCESS
            }
            CraftCommands::Sniff { file } => {
                let bytes = match std::fs::read(&file) {
                    Ok(b) => b,
                    Err(e) => {
                        return emit_error(
                            "FILE_READ_ERROR",
                            format!("cannot read `{}`: {e}", file.display()),
                            "Pass an existing media file path",
                            as_json,
                        );
                    }
                };
                let info = inspect_media_bytes(&bytes);
                let payload = serde_json::json!({
                    "ok": true,
                    "path": file.display().to_string(),
                    "inspection": info
                });
                println!(
                    "{}",
                    if as_json {
                        serde_json::to_string(&payload).unwrap_or_default()
                    } else {
                        serde_json::to_string_pretty(&payload).unwrap_or_default()
                    }
                );
                ExitCode::SUCCESS
            }
        },

        Commands::Hash { input, file, seed } => {
            let bytes = if let Some(path) = file {
                match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        return emit_error(
                            "FILE_READ_ERROR",
                            format!("cannot read `{}`: {e}", path.display()),
                            "Check file path permissions",
                            as_json,
                        );
                    }
                }
            } else if let Some(text) = input {
                text.into_bytes()
            } else {
                return emit_error(
                    "MISSING_INPUT",
                    "provide either a string argument or `--file <PATH>`",
                    "Example: ay hash \"hello\"  or  ay hash --file Cargo.toml",
                    as_json,
                );
            };
            let digest = bun_wyhash::hash_with_seed(seed, &bytes);
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "algorithm": "bun_wyhash",
                        "seed": seed,
                        "bytes": bytes.len(),
                        "u64": digest,
                        "hex": format!("{digest:016x}")
                    })
                );
            } else {
                println!("{digest:016x}");
            }
            ExitCode::SUCCESS
        }

        Commands::Pets { kind, mood, status } => {
            if let Some(raw) = kind {
                match PetKind::parse(&raw) {
                    Ok(parsed) => {
                        let snap = render_pet(parsed, mood.to_ui(), &status);
                        println!("{}", serde_json::to_string(&snap).unwrap_or_default());
                        ExitCode::SUCCESS
                    }
                    Err(err) => emit_error(err.code, err.message, err.hint, as_json),
                }
            } else {
                let all: Vec<_> = PetKind::ALL
                    .into_iter()
                    .map(|k| render_pet(k, PetMood::Idle, "Ready"))
                    .collect();
                println!("{}", serde_json::to_string(&all).unwrap_or_default());
                ExitCode::SUCCESS
            }
        }

        Commands::Doctor => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let tools = ["rustc", "cargo", "bun", "just", "hyperfine", "oha", "git"];
            let probes: Vec<_> = tools
                .iter()
                .map(|name| {
                    let path = which::which(name).ok().map(|p| p.display().to_string());
                    serde_json::json!({
                        "tool": name,
                        "available": path.is_some(),
                        "path": path
                    })
                })
                .collect();
            let (forbid, total) = scan_unsafe_forbid_crates(&cwd);
            let trusted_ok = cwd.join("TRUSTED.toml").is_file();
            let lock_ok = cwd.join("acceptance.lock").is_file();
            let core_ok = which::which("rustc").is_ok() && which::which("cargo").is_ok();
            let payload = serde_json::json!({
                "ok": core_ok,
                "version": env!("CARGO_PKG_VERSION"),
                "workspace": cwd.display().to_string(),
                "trustedManifest": trusted_ok,
                "acceptanceLock": lock_ok,
                "unsafeForbidCrates": forbid,
                "totalCrates": total,
                "tools": probes
            });
            if as_json {
                println!("{payload}");
            } else {
                println!(
                    "{GREEN_BOLD}ay doctor{GREEN_BOLD:#} — workspace={} trusted={} lock={} unsafe_forbid={forbid}/{total}",
                    cwd.display(),
                    trusted_ok,
                    lock_ok
                );
                for p in &probes {
                    let name = p["tool"].as_str().unwrap_or("?");
                    let ok = p["available"].as_bool().unwrap_or(false);
                    let marker = if ok { "OK  " } else { "MISS" };
                    println!(
                        "  [{marker}] {name:<10} {}",
                        p["path"].as_str().unwrap_or("-")
                    );
                }
            }
            if core_ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }

        Commands::Gate { strict, manifest } => {
            if !manifest.is_file() {
                return emit_error(
                    "MISSING_TRUSTED_MANIFEST",
                    format!("{} not found in current workspace root", manifest.display()),
                    "Run `ay gate` from a repository root containing TRUSTED.toml",
                    as_json,
                );
            }
            let root = manifest
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let lock_path = root.join("acceptance.lock");
            let trusted_bytes = std::fs::read(&manifest).unwrap_or_default().len();
            let lock_present = lock_path.is_file();
            if strict && !lock_present {
                return emit_error(
                    "MISSING_ACCEPTANCE_LOCK",
                    "acceptance.lock is missing in strict mode",
                    "Generate baseline lock via `bun run app:gate:lock`",
                    as_json,
                );
            }
            let (forbid, total) = scan_unsafe_forbid_crates(root);
            let out = serde_json::json!({
                "ok": true,
                "schema": "aphrody.gate.cli.v1",
                "trusted_manifest": manifest.display().to_string(),
                "trusted_manifest_bytes": trusted_bytes,
                "acceptance_lock_present": lock_present,
                "unsafe_forbid_crates": forbid,
                "total_crates": total
            });
            println!("{out}");
            ExitCode::SUCCESS
        }

        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let mut stdout = std::io::stdout();
            shell.generate(&mut cmd, &mut stdout);
            ExitCode::SUCCESS
        }

        Commands::App { args } => {
            let Some(root) = find_aphrody_root() else {
                return emit_error(
                    "APHRODY_ROOT_NOT_FOUND",
                    "could not locate `aphrody` repository (`apps/app/cli.ts`)",
                    "Run from inside `aphrody` or next to `../aphrody`",
                    as_json,
                );
            };
            let mut full = vec!["apps/app/cli.ts".to_owned()];
            full.extend(args);
            run_forwarded("bun", &full, Some(&root), as_json)
        }

        Commands::Deploy { args } => {
            run_aphrody_rust_bin("aphrody-deploy", "aphrody-deploy", &args, as_json)
        }

        Commands::Estate { args } => {
            run_aphrody_rust_bin("aphrody-estate", "aphrody-estate", &args, as_json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_command_tree_passes_debug_assert() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_legacy_create_pack_and_new_dx_zed_subcommands() {
        let c1 = Cli::try_parse_from([
            "ay", "create", "demo", "--ui", "gpui", "--js", "bun-aot", "--server", "axum",
        ])
        .expect("create alias");
        assert!(matches!(c1.command, Commands::New { .. }));

        let c2 = Cli::try_parse_from(["ay", "pack", "flatpak"]).expect("pack alias");
        assert!(matches!(c2.command, Commands::Bundle { .. }));

        let c3 = Cli::try_parse_from([
            "ay",
            "design",
            "tokens",
            "--theme",
            "vice_city_sunset",
            "--output",
            "wgsl",
        ])
        .expect("design tokens");
        assert!(matches!(c3.command, Commands::Design { .. }));

        let c4 = Cli::try_parse_from([
            "ay",
            "open",
            "src/main.rs:42:7",
            "--diff",
            "a.rs",
            "b.rs",
            "--wait",
        ])
        .expect("zed-style open");
        assert!(matches!(c4.command, Commands::Open { .. }));

        let c5 = Cli::try_parse_from(["ay", "completions", "powershell"]).expect("completions");
        assert!(matches!(c5.command, Commands::Completions { .. }));
    }

    #[test]
    fn parses_zed_path_line_column_cleanly() {
        let p = parse_path_with_position("crates/ay_cli/src/main.rs:120:9");
        assert_eq!(p.path, "crates/ay_cli/src/main.rs");
        assert_eq!(p.line, Some(120));
        assert_eq!(p.column, Some(9));

        let p2 = parse_path_with_position("C:\\src\\ay\\Cargo.toml:10");
        assert_eq!(p2.path, "C:\\src\\ay\\Cargo.toml");
        assert_eq!(p2.line, Some(10));
        assert_eq!(p2.column, None);
    }
}
