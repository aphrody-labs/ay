//! `kitesurf-cdp [serve] --host 127.0.0.1 --port 9222`: a headless browser in pure Rust behind the
//! Chrome DevTools Protocol (the `lightpanda serve` contract: `GET /json/version` names the WebSocket).

mod cdp;
mod fonts;
mod net;
mod page;
mod server;

use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};

const USAGE: &str = "usage: kitesurf-cdp [serve] [--host 127.0.0.1] [--port 9222] [--width 1280] [--height 720] [--http_proxy URL]";

fn main() {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 9222;
    let (mut width, mut height) = (1280u32, 720u32);
    let mut proxy: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| {
            inline.clone().or_else(|| args.next()).unwrap_or_else(|| {
                eprintln!("{name} needs a value\n{USAGE}");
                std::process::exit(2)
            })
        };
        let number = |name: &str, v: String| {
            v.parse::<u32>().unwrap_or_else(|_| {
                eprintln!("{name}: not a number: {v}");
                std::process::exit(2)
            })
        };
        match flag.as_str() {
            "serve" => {}
            "--host" => host = value("--host"),
            "--port" => port = number("--port", value("--port")).try_into().unwrap_or(9222),
            "--width" => width = number("--width", value("--width")),
            "--height" => height = number("--height", value("--height")),
            "--http_proxy" | "--http-proxy" => proxy = Some(value("--http_proxy")),
            // Lightpanda's serve flags that have no meaning here.
            "--timeout" | "--log_level" | "--log-level" | "--log_format" | "--log-format" | "--user_agent_suffix" => {
                value(&flag);
            }
            "--insecure_disable_tls_host_verification" | "--obey_robots" => {}
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("kitesurf-cdp {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                eprintln!("unknown argument: {arg}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let _ = rustls::crypto::CryptoProvider::install_default(rustls_rustcrypto::provider());

    let listener = TcpListener::bind((host.as_str(), port)).unwrap_or_else(|e| {
        eprintln!("cannot listen on {host}:{port}: {e}");
        std::process::exit(1)
    });
    let addr = listener.local_addr().expect("bound address");
    let endpoint = Arc::new(server::Endpoint {
        addr,
        browser_id: format!("{:032x}", std::process::id() as u128 * 0x9E37_79B9_7F4A_7C15),
        targets: Arc::new(Mutex::new(Vec::new())),
    });
    eprintln!("DevTools listening on {}", endpoint.browser_url());

    let (incoming_tx, incoming_rx) = mpsc::channel();
    server::listen(listener, Arc::clone(&endpoint), incoming_tx);

    let net = net::Net::new(proxy.as_deref());
    let (net_tx, net_rx) = mpsc::channel();
    net.set_events(net_tx);
    let env = page::Env { net, navigations: Arc::new(Mutex::new(Vec::new())) };
    cdp::Browser::new(env, endpoint, incoming_rx, net_rx, width, height).run()
}
