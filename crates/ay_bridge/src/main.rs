//! Thin CLI wrapper for `ay_bridge` (`#![forbid(unsafe_code)]`).

use std::env;

use ay_bridge::{hvsock_service_guid, probe, win_to_wsl_path, wsl_to_win_unc};

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
            let distro = env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "cachyos".to_owned());
            println!("{}", wsl_to_win_unc(path, &distro));
        }
        Some("wsl-path") => {
            let Some(path) = args.get(1) else {
                eprintln!("usage: ay_bridge wsl-path <windows_path>");
                std::process::exit(2);
            };
            match win_to_wsl_path(path) {
                Some(p) => println!("{p}"),
                None => {
                    eprintln!("not a Windows drive path: `{path}`");
                    std::process::exit(2);
                }
            }
        }
        Some("hvsock-guid") => {
            let Some(port) = args.get(1).and_then(|s| s.parse::<u32>().ok()) else {
                eprintln!("usage: ay_bridge hvsock-guid <vsock_port_u32>");
                std::process::exit(2);
            };
            println!("{}", hvsock_service_guid(port));
        }
        Some(other) => {
            eprintln!(
                "unknown subcommand `{other}`; expected `probe`, `win-path`, `wsl-path`, or `hvsock-guid`"
            );
            std::process::exit(2);
        }
    }
}
