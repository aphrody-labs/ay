//! WSL/WHPX <-> Windows native kernel, GPU, Wayland, Binder, `AF_VSOCK`/`AF_HYPERV`, and sysctl bridge (`#![forbid(unsafe_code)]`).

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Serialize;

/// Microsoft Hyper-V `AF_HYPERV` <-> Linux `AF_VSOCK` (`CID=2`) GUID suffix.
pub const HVSOCK_GUID_SUFFIX: &str = "facb-11e6-bd58-64006a7986d3";

#[derive(Debug, Clone, Serialize)]
pub struct SysctlMeshStatus {
    pub tcp_congestion_control: Option<String>,
    pub default_qdisc: Option<String>,
    pub tcp_mtu_probing: Option<u32>,
    pub tcp_slow_start_after_idle: Option<u32>,
    pub mesh_tuned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgeProbe {
    pub kernel: String,
    pub distro: String,
    pub dxg_directx12: bool,
    pub dri_devices: Vec<String>,
    pub binderfs_present: bool,
    pub kvm_present: bool,
    pub vsock_present: bool,
    pub wayland_display: Option<String>,
    pub wayland_socket_live: bool,
    pub wsl_interop: Option<String>,
    pub vulkan_available: bool,
    pub win32_interop_live: bool,
    pub sysctl_mesh: SysctlMeshStatus,
}

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn read_u32(path: &str) -> Option<u32> {
    read_trimmed(path)?.parse().ok()
}

/// Probe live kernel, GPU, Wayland, Android Binder, vsock, Win32 interop, and WireGuard/TCP sysctl state.
#[must_use]
pub fn probe() -> BridgeProbe {
    let distro = env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "Linux".to_owned());
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
    let win32_interop_live = wsl_interop
        .as_deref()
        .is_some_and(|p| Path::new(p).exists());

    let tcp_congestion_control = read_trimmed("/proc/sys/net/ipv4/tcp_congestion_control");
    let default_qdisc = read_trimmed("/proc/sys/net/core/default_qdisc");
    let tcp_mtu_probing = read_u32("/proc/sys/net/ipv4/tcp_mtu_probing");
    let tcp_slow_start_after_idle = read_u32("/proc/sys/net/ipv4/tcp_slow_start_after_idle");
    let mesh_tuned = tcp_congestion_control.as_deref() == Some("bbr")
        && tcp_mtu_probing == Some(1)
        && tcp_slow_start_after_idle == Some(0);

    BridgeProbe {
        kernel: read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".to_owned()),
        distro,
        dxg_directx12: Path::new("/dev/dxg").exists(),
        dri_devices,
        binderfs_present: Path::new("/dev/binderfs").exists() || Path::new("/dev/binder").exists(),
        kvm_present: Path::new("/dev/kvm").exists(),
        vsock_present: Path::new("/dev/vsock").exists(),
        wayland_display,
        wayland_socket_live,
        wsl_interop,
        vulkan_available,
        win32_interop_live,
        sysctl_mesh: SysctlMeshStatus {
            tcp_congestion_control,
            default_qdisc,
            tcp_mtu_probing,
            tcp_slow_start_after_idle,
            mesh_tuned,
        },
    }
}

/// Convert a POSIX WSL path (`/home/ay/src/ay`) to a Windows UNC path (`\\wsl.localhost\<distro>\home\ay\src\ay`).
#[must_use]
pub fn wsl_to_win_unc(posix_path: &str, distro: &str) -> String {
    let trimmed = posix_path.trim_start_matches('/');
    format!(
        "\\\\wsl.localhost\\{}\\{}",
        distro,
        trimmed.replace('/', "\\")
    )
}

/// Convert a Windows drive path (`C:\Users\ay\src`) to a WSL mount path (`/mnt/c/Users/ay/src`).
#[must_use]
pub fn win_to_wsl_path(win_path: &str) -> Option<String> {
    let bytes = win_path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = win_path[2..].replace('\\', "/");
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            return Some(format!("/mnt/{drive}"));
        }
        return Some(format!("/mnt/{drive}/{rest}"));
    }
    None
}

/// Map a Linux `AF_VSOCK` port to the deterministic Windows `AF_HYPERV` (`hvsock`) service GUID.
#[must_use]
pub fn hvsock_service_guid(vsock_port: u32) -> String {
    format!("{vsock_port:08x}-{HVSOCK_GUID_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_wsl_and_windows_paths_bidirectionally() {
        assert_eq!(
            wsl_to_win_unc("/home/ay/src/ay", "cachyos"),
            "\\\\wsl.localhost\\cachyos\\home\\ay\\src\\ay"
        );
        assert_eq!(
            win_to_wsl_path(r"C:\Users\ay\src\yoyo"),
            Some("/mnt/c/Users/ay/src/yoyo".to_owned())
        );
        assert_eq!(win_to_wsl_path("/already/posix"), None);
    }

    #[test]
    fn derives_canonical_microsoft_hvsock_guid_from_vsock_port() {
        assert_eq!(
            hvsock_service_guid(5000),
            "00001388-facb-11e6-bd58-64006a7986d3"
        );
        assert_eq!(
            hvsock_service_guid(0x0000_2710),
            "00002710-facb-11e6-bd58-64006a7986d3"
        );
    }
}
