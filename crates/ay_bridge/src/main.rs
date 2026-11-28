//! WSL <-> Windows native kernel/GPU/Wayland/Win32 bridge (`#![forbid(unsafe_code)]`).
//!
//! Probes live `/dev/dxg` (DirectX 12 WDDM), `/dev/dri/renderD128` (Vulkan/DRM),
//! WSLg Wayland socket (`WAYLAND_DISPLAY`), and `WSL_INTEROP` (Windows Terminal / Chrome / Win32),
//! plus bidirectional WSL <-> Windows path translation (`\\wsl.localhost\<distro>\...`).

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Serialize)]
struct BridgeProbe {
    kernel: String,
    distro: String,
    dxg_directx12: bool,
    dri_devices: Vec<String>,
    wayland_display: Option<String>,
    wayland_socket_live: bool,
    wsl_interop: Option<String>,
    vulkan_available: bool,
    win32_interop_live: bool,
}

fn read_kernel() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned())
}

fn probe() -> BridgeProbe {
    let distro = env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Ubuntu".to_owned());
    let wayland_display = env::var("WAYLAND_DISPLAY").ok();
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_owned());
    let wayland_socket_live = wayland_display
        .as_deref()
        .is_some_and(|sock| Path::new(&runtime_dir).join(sock).exists());

    let dri_devices = fs::read_dir("/dev/dri")
        .map(|rd| {
            let mut v: Vec<String> = rd
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default();

    let vulkan_available = Command::new("vulkaninfo")
        .arg("--summary")
        .output()
        .is_ok_and(|o| o.status.success());

    let wsl_interop = env::var("WSL_INTEROP").ok();
    let win32_interop_live = wsl_interop.as_deref().is_some_and(|p| Path::new(p).exists());

    BridgeProbe {
        kernel: read_kernel(),
        distro,
        dxg_directx12: Path::new("/dev/dxg").exists(),
        dri_devices,
        wayland_display,
        wayland_socket_live,
        wsl_interop,
        vulkan_available,
        win32_interop_live,
    }
}

/// Convert a POSIX WSL path (`/home/ay/src/ay`) to a Windows UNC path (`\\wsl.localhost\<distro>\home\ay\src\ay`).
pub fn wsl_to_win_unc(posix_path: &str, distro: &str) -> String {
    let trimmed = posix_path.trim_start_matches('/');
    format!(
        "\\\\wsl.localhost\\{}\\{}",
        distro,
        trimmed.replace('/', "\\")
    )
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe") | None => {
            let p = probe();
            println!("{}", serde_json::to_string_pretty(&p).unwrap_or_default());
        }
        Some("win-path") => {
            let Some(path) = args.get(1) else {
                eprintln!("usage: ay_bridge win-path <posix_path>");
                std::process::exit(2);
            };
            let distro = env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Ubuntu".to_owned());
            println!("{}", wsl_to_win_unc(path, &distro));
        }
        Some(other) => {
            eprintln!("unknown subcommand `{other}`; expected `probe` or `win-path <path>`");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_wsl_path_to_windows_unc() {
        assert_eq!(
            wsl_to_win_unc("/home/ay/src/ay", "Arch"),
            "\\\\wsl.localhost\\Arch\\home\\ay\\src\\ay"
        );
    }
}
