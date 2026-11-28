//! Cargo/Dioxus-consistent ANSI styling and TTY-aware output formatting (`#![forbid(unsafe_code)]`).

use std::io::IsTerminal;

use clap::builder::styling::{AnsiColor, Effects, Style, Styles};
use serde::Serialize;

pub const CARGO_STYLING: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .invalid(AnsiColor::Yellow.on_default().effects(Effects::BOLD));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Pretty human output on interactive TTY, compact JSON when piped.
    #[default]
    Auto,
    /// Structured human-readable output.
    Pretty,
    /// Compact machine-readable JSON output.
    Json,
}

impl OutputFormat {
    #[must_use]
    pub fn wants_json(self, json_flag: bool) -> bool {
        if json_flag {
            return true;
        }
        match self {
            Self::Json => true,
            Self::Pretty => false,
            Self::Auto => !std::io::stdout().is_terminal(),
        }
    }
}

pub const CYAN_BOLD: Style = AnsiColor::Cyan.on_default().effects(Effects::BOLD);
pub const RED_BOLD: Style = AnsiColor::Red.on_default().effects(Effects::BOLD);

#[derive(Serialize)]
pub struct CliError {
    pub ok: bool,
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

pub fn emit_error(
    code: &'static str,
    message: impl Into<String>,
    hint: impl Into<String>,
    as_json: bool,
) -> std::process::ExitCode {
    let message = message.into();
    let hint = hint.into();
    if as_json {
        let payload = CliError {
            ok: false,
            code,
            message,
            hint,
        };
        if let Ok(json) = serde_json::to_string(&payload) {
            eprintln!("{json}");
        }
    } else if std::io::stderr().is_terminal() {
        eprintln!("{RED_BOLD}error[{code}]{RED_BOLD:#}: {message}");
        if !hint.is_empty() {
            eprintln!("{CYAN_BOLD}hint{CYAN_BOLD:#}: {hint}");
        }
    } else {
        eprintln!("error[{code}]: {message}");
        if !hint.is_empty() {
            eprintln!("hint: {hint}");
        }
    }
    std::process::ExitCode::FAILURE
}
