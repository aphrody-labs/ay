//! Shell completion generator modeled on `zed/crates/cli/src/completions.rs` and `dx`.

use clap::Command;
use clap_complete::{Generator, shells};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ShellKind {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
}

impl ShellKind {
    pub fn generate(self, cmd: &mut Command, out: &mut dyn std::io::Write) {
        let name = cmd.get_name().to_owned();
        match self {
            Self::Bash => shells::Bash.generate(cmd, &mut *out),
            Self::Elvish => shells::Elvish.generate(cmd, &mut *out),
            Self::Fish => shells::Fish.generate(cmd, &mut *out),
            Self::PowerShell => shells::PowerShell.generate(cmd, &mut *out),
            Self::Zsh => shells::Zsh.generate(cmd, &mut *out),
        }
        let _ = name;
    }
}
