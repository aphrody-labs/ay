//! Live 6-point Nix / Lix + CachyOS side-by-side conflict auditor (`#![forbid(unsafe_code)]`).

use std::env;
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConflictFinding {
    pub check: &'static str,
    pub severity: &'static str,
    pub detail: String,
    pub remediation: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NixCachyosAudit {
    pub nix_store_present: bool,
    pub pacman_present: bool,
    pub findings: Vec<ConflictFinding>,
    pub clean: bool,
}

/// Inspect the live host environment for known Nix + CachyOS conflicts:
/// 1. `PATH` shadowing CachyOS `x86-64-v3/v4` host binaries with generic Nixpkgs
/// 2. Global `LD_LIBRARY_PATH` glibc ABI contamination
/// 3. `LOCALE_ARCHIVE` cross-glibc mismatch risk
/// 4. Vulkan ICD / NVIDIA `nixGL` missing when `/nix/store` + GPU present
/// 5. `XDG_DATA_DIRS` portal shadowing
/// 6. `hyprpm` invoked on a read-only Nix Hyprland store
#[must_use]
pub fn audit_nix_cachyos() -> NixCachyosAudit {
    let nix_store_present = Path::new("/nix/store").exists();
    let pacman_present = which::which("pacman").is_ok();
    let mut findings = Vec::new();

    let path_var = env::var("PATH").unwrap_or_default();
    let entries: Vec<&str> = path_var.split(':').collect();
    let usr_bin_idx = entries.iter().position(|&p| p == "/usr/bin");
    let nix_idx = entries
        .iter()
        .position(|p| p.contains(".nix-profile/bin") || p.starts_with("/nix/var/nix/profiles"));
    if let (Some(n_i), Some(u_i)) = (nix_idx, usr_bin_idx)
        && pacman_present
        && n_i < u_i
    {
        findings.push(ConflictFinding {
            check: "path_precedence",
            severity: "warn",
            detail: format!("Nix profile at PATH index {n_i} shadows CachyOS /usr/bin at index {u_i}"),
            remediation: "Place /usr/local/bin:/usr/bin ahead of ~/.nix-profile/bin outside explicit devShells",
        });
    }

    if let Ok(ld) = env::var("LD_LIBRARY_PATH")
        && !ld.trim().is_empty()
        && nix_store_present
    {
        findings.push(ConflictFinding {
            check: "ld_library_path_leak",
            severity: "error",
            detail: "Global LD_LIBRARY_PATH is set alongside /nix/store (glibc dlopen ABI hazard)"
                .to_owned(),
            remediation: "Unset global LD_LIBRARY_PATH; scope per-binary via nixGL or RPATH",
        });
    }

    if nix_store_present && pacman_present && env::var("LOCALE_ARCHIVE").is_err() {
        findings.push(ConflictFinding {
            check: "locale_archive",
            severity: "info",
            detail: "LOCALE_ARCHIVE unset while both /nix/store and pacman exist".to_owned(),
            remediation: "Set LOCALE_ARCHIVE only inside Nix wrapper scripts, never globally for host binaries",
        });
    }

    if nix_store_present
        && Path::new("/dev/dri").exists()
        && which::which("nixGL").is_err()
        && which::which("nixVulkanNvidia").is_err()
    {
        findings.push(ConflictFinding {
            check: "gpu_nixgl",
            severity: "warn",
            detail: "/nix/store + /dev/dri detected without nixGL/nixVulkanNvidia wrapper".to_owned(),
            remediation: "Install nixGL or link host Vulkan ICDs before launching Nix GUI/WGPU apps on CachyOS",
        });
    }

    if let Ok(xdg) = env::var("XDG_DATA_DIRS") {
        let parts: Vec<&str> = xdg.split(':').collect();
        let nix_xdg = parts.iter().position(|p| p.contains("nix-profile"));
        let sys_xdg = parts.iter().position(|&p| p == "/usr/share");
        if let (Some(nx), Some(sx)) = (nix_xdg, sys_xdg)
            && pacman_present
            && nx < sx
        {
            findings.push(ConflictFinding {
                check: "xdg_portal_order",
                severity: "warn",
                detail: "Nix XDG_DATA_DIRS precedes /usr/share (may shadow xdg-desktop-portal)"
                    .to_owned(),
                remediation: "Order XDG_DATA_DIRS as /usr/share -> Flatpak -> ~/.nix-profile/share",
            });
        }
    }

    if let Ok(hypr_path) = which::which("Hyprland")
        && hypr_path.to_string_lossy().starts_with("/nix/store")
        && which::which("hyprpm").is_ok()
    {
        findings.push(ConflictFinding {
            check: "hyprpm_on_nix",
            severity: "error",
            detail: "Hyprland binary lives in read-only /nix/store while hyprpm is installed".to_owned(),
            remediation: "Do not use hyprpm on Nix; pin inputs.hyprland-plugins.inputs.hyprland.follows in flake.nix",
        });
    }

    let clean = !findings.iter().any(|f| f.severity == "error");
    NixCachyosAudit {
        nix_store_present,
        pacman_present,
        findings,
        clean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_nix_cachyos_audit_executes_cleanly() {
        let report = audit_nix_cachyos();
        // Smoke-check invariants without assuming host has Nix installed
        assert_eq!(
            report.clean,
            !report.findings.iter().any(|f| f.severity == "error")
        );
    }
}
