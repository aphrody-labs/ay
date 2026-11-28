//! Canonical 10-ecosystem package IR, ECMA-427 PURL parser, and live host backend prober (`#![forbid(unsafe_code)]`).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use packageurl::PackageUrl;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// All 10 supported package, store, and plugin ecosystems in `ay_depot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum Ecosystem {
    Winget = 1,
    Pacman = 2,
    Nix = 3,
    Flatpak = 4,
    FDroid = 5,
    GooglePlay = 6,
    ChromeStore = 7,
    PowerShell = 8,
    Fish = 9,
    Hyprpm = 10,
}

impl Ecosystem {
    pub const ALL: [Self; 10] = [
        Self::Winget,
        Self::Pacman,
        Self::Nix,
        Self::Flatpak,
        Self::FDroid,
        Self::GooglePlay,
        Self::ChromeStore,
        Self::PowerShell,
        Self::Fish,
        Self::Hyprpm,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Winget => "winget",
            Self::Pacman => "alpm",
            Self::Nix => "nix",
            Self::Flatpak => "flatpak",
            Self::FDroid => "fdroid",
            Self::GooglePlay => "play",
            Self::ChromeStore => "chrome",
            Self::PowerShell => "psresource",
            Self::Fish => "fish",
            Self::Hyprpm => "hyprpm",
        }
    }

    #[must_use]
    pub fn from_scheme(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "winget" => Some(Self::Winget),
            "alpm" | "pacman" | "pamac" | "aur" | "cachyos" => Some(Self::Pacman),
            "nix" | "lix" => Some(Self::Nix),
            "flatpak" | "flathub" => Some(Self::Flatpak),
            "fdroid" | "f-droid" | "izzy" => Some(Self::FDroid),
            "play" | "googleplay" | "google-play" | "apk" => Some(Self::GooglePlay),
            "chrome" | "cws" | "crx" => Some(Self::ChromeStore),
            "psresource" | "powershell" | "psgallery" => Some(Self::PowerShell),
            "fish" | "fisher" => Some(Self::Fish),
            "hyprpm" | "hyprland" => Some(Self::Hyprpm),
            _ => None,
        }
    }

    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Winget),
            2 => Some(Self::Pacman),
            3 => Some(Self::Nix),
            4 => Some(Self::Flatpak),
            5 => Some(Self::FDroid),
            6 => Some(Self::GooglePlay),
            7 => Some(Self::ChromeStore),
            8 => Some(Self::PowerShell),
            9 => Some(Self::Fish),
            10 => Some(Self::Hyprpm),
            _ => None,
        }
    }

    #[must_use]
    pub const fn probe_binaries(self) -> &'static [&'static str] {
        match self {
            Self::Winget => &["winget.exe", "winget"],
            Self::Pacman => &["pacman", "paru", "yay"],
            Self::Nix => &["nix", "lix"],
            Self::Flatpak => &["flatpak"],
            Self::FDroid => &["fdroidcl", "adb"],
            Self::GooglePlay => &["gplay", "apkeep"],
            Self::ChromeStore => &[
                "google-chrome-stable",
                "google-chrome",
                "chromium",
                "chrome.exe",
            ],
            Self::PowerShell => &["pwsh", "pwsh.exe"],
            Self::Fish => &["fish"],
            Self::Hyprpm => &["hyprpm", "hyprctl"],
        }
    }

    #[must_use]
    pub const fn supports_publish(self) -> bool {
        matches!(
            self,
            Self::GooglePlay | Self::ChromeStore | Self::PowerShell
        )
    }
}

/// Strictly separates consumer install flows from store/registry publisher CI flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum ActionPlane {
    Consume = 1,
    Publish = 2,
}

impl ActionPlane {
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Consume),
            2 => Some(Self::Publish),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DepotError {
    #[error("invalid PURL `{0}`: {1}")]
    InvalidPurl(String, &'static str),
    #[error("unknown package ecosystem `{0}`")]
    UnknownEcosystem(String),
    #[error("invalid depot.lock binary: {0}")]
    InvalidLock(&'static str),
    #[error(
        "depot.lock integrity check failed (expected hash {expected:#010x}, got {actual:#010x})"
    )]
    LockTampered { expected: u32, actual: u32 },
}

/// Normalized ECMA-427 Package URL (`pkg:<type>/[namespace/]name[@version][?k=v][#subpath]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purl {
    pub kind: Ecosystem,
    pub namespace: Option<String>,
    pub name: String,
    pub version: Option<String>,
    pub qualifiers: BTreeMap<String, String>,
    pub subpath: Option<String>,
}

impl Purl {
    /// Parse a canonical `pkg:<ecosystem>/...` string via upstream `packageurl` (ECMA-427 spec).
    pub fn parse(raw: &str) -> Result<Self, DepotError> {
        let parsed = PackageUrl::from_str(raw)
            .map_err(|_| DepotError::InvalidPurl(raw.to_owned(), "invalid ECMA-427 Package URL"))?;
        let kind = Ecosystem::from_scheme(parsed.ty())
            .ok_or_else(|| DepotError::UnknownEcosystem(parsed.ty().to_owned()))?;
        let qualifiers: BTreeMap<String, String> = parsed
            .qualifiers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        Ok(Self {
            kind,
            namespace: parsed.namespace().map(str::to_owned),
            name: parsed.name().to_owned(),
            version: parsed.version().map(str::to_owned),
            qualifiers,
            subpath: parsed.subpath().map(str::to_owned),
        })
    }

    /// Deterministic 64-bit Bun Wyhash key of the canonical PURL string.
    #[must_use]
    pub fn wyhash(&self) -> u64 {
        bun_wyhash::hash(self.to_string().as_bytes())
    }
}

impl fmt::Display for Purl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pkg:{}/", self.kind.as_str())?;
        if let Some(ns) = &self.namespace {
            write!(f, "{ns}/")?;
        }
        write!(f, "{}", self.name)?;
        if let Some(ver) = &self.version {
            write!(f, "@{ver}")?;
        }
        if !self.qualifiers.is_empty() {
            write!(f, "?")?;
            for (i, (k, v)) in self.qualifiers.iter().enumerate() {
                if i > 0 {
                    write!(f, "&")?;
                }
                write!(f, "{k}={v}")?;
            }
        }
        if let Some(sub) = &self.subpath {
            write!(f, "#{sub}")?;
        }
        Ok(())
    }
}

/// Live detection result for one package/plugin ecosystem on the current host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendProbe {
    pub ecosystem: Ecosystem,
    pub scheme: &'static str,
    pub available: bool,
    pub detected_binary: Option<String>,
    pub supports_publish: bool,
}

/// Probe all 10 ecosystems against live PATH binaries (read-only, zero side effects).
#[must_use]
pub fn probe_all_backends() -> Vec<BackendProbe> {
    Ecosystem::ALL
        .iter()
        .map(|&eco| {
            let detected = eco
                .probe_binaries()
                .iter()
                .find_map(|bin| which::which(bin).ok().map(|p| p.display().to_string()));
            BackendProbe {
                ecosystem: eco,
                scheme: eco.as_str(),
                available: detected.is_some(),
                detected_binary: detected,
                supports_publish: eco.supports_publish(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_roundtrips_all_10_ecosystem_purls() {
        let samples = [
            "pkg:winget/BurntSushi.ripgrep.MSVC@14.1.1",
            "pkg:alpm/cachyos/git@2.56.1-1?arch=x86_64_v3",
            "pkg:nix/nixpkgs/ripgrep@14.1.1",
            "pkg:flatpak/app.zen_browser.zen@1.12.0",
            "pkg:fdroid/com.termux@1000?repo=official",
            "pkg:play/com.aphrody.app@37001",
            "pkg:chrome/ddkjiahejlhfcafbddmgiahcphecmpfh@1.64.0",
            "pkg:psresource/Microsoft.PowerShell.PSResourceGet@1.2.0",
            "pkg:fish/jorgebucaran/nvm.fish@2.2.15",
            "pkg:hyprpm/hyprwm/hyprbars@0.48.0#hyprbars",
        ];
        for raw in samples {
            let parsed = Purl::parse(raw).expect("valid purl");
            assert_eq!(parsed.to_string(), raw);
            assert_ne!(parsed.wyhash(), 0);
        }
        assert!(Purl::parse("http://bad").is_err());
        assert!(Purl::parse("pkg:unknown/foo").is_err());
    }

    #[test]
    fn probes_all_10_backends_without_panicking() {
        let probes = probe_all_backends();
        assert_eq!(probes.len(), 10);
        assert!(probes.iter().any(|p| p.ecosystem == Ecosystem::Pacman));
    }
}
