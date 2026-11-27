//! `bun install`, `bun add` and `bun remove` against a registry in this process: an HTTP/1.1 server on 127.0.0.1,
//! port 0, that serves abbreviated packuments and gzipped tarballs.
#![cfg(feature = "install")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bun() -> Command {
    Command::new(std::env::var_os("NEXTEST_BIN_EXE_bun").unwrap_or(env!("CARGO_BIN_EXE_bun").into()))
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// gzip of `data` in stored deflate blocks.
fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let mut chunks = data.chunks(0xffff).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// A ustar archive of `files` under `package/`.
fn tar(files: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, contents) in files {
        let mut header = [0u8; 512];
        let path = format!("package/{name}");
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", contents.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(contents.as_bytes());
        out.resize(out.len().next_multiple_of(512), 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

struct Registry {
    url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

fn respond(mut stream: impl Read + Write, origin: &str, requests: &std::sync::Mutex<Vec<String>>) {
    let mut request = Vec::new();
    let mut buf = [0u8; 4096];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let request = String::from_utf8_lossy(&request).into_owned();
    let path = request.split(' ').nth(1).unwrap_or("/").to_owned();
    requests.lock().unwrap().push(path.clone());
    let packages = ["foo", "bar"];
    let (status, body): (&str, Vec<u8>) = if let Some(name) = packages.iter().find(|name| path == format!("/{name}")) {
        let packument = format!(
            r#"{{"name":"{name}","dist-tags":{{"latest":"1.0.0"}},"versions":{{"1.0.0":{{"name":"{name}","version":"1.0.0","dist":{{"tarball":"{origin}{name}/-/{name}-1.0.0.tgz"}}}}}}}}"#
        );
        ("200 OK", packument.into_bytes())
    } else if let Some(name) = packages.iter().find(|name| path == format!("/{name}/-/{name}-1.0.0.tgz")) {
        let package_json = format!(r#"{{"name":"{name}","version":"1.0.0"}}"#);
        let index = format!("module.exports = \"{name}\";\n");
        ("200 OK", gzip(&tar(&[("package.json", &package_json), ("index.js", &index)])))
    } else {
        ("404 Not Found", b"{}".to_vec())
    };
    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn registry() -> Registry {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (log, served) = (requests.clone(), origin.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (log, origin) = (log.clone(), served.clone());
            std::thread::spawn(move || respond(stream, &origin, &log));
        }
    });
    Registry { url: origin, requests }
}

fn tls_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls").join(name)
}

/// The same registry over TLS, as `https://localhost:<port>/`, with a certificate of tests/fixtures/tls's CA.
fn tls_registry() -> Registry {
    use rustls::pki_types::pem::PemObject;
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(tls_fixture("server.crt"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(tls_fixture("server-key.txt")).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls_rustcrypto::provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    let config = std::sync::Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("https://localhost:{}/", listener.local_addr().unwrap().port());
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (log, served) = (requests.clone(), origin.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (log, origin, config) = (log.clone(), served.clone(), config.clone());
            std::thread::spawn(move || {
                let connection = rustls::ServerConnection::new(config).unwrap();
                let mut tls = rustls::StreamOwned::new(connection, stream);
                respond(&mut tls, &origin, &log);
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    Registry { url: origin, requests }
}

fn project(name: &str, package_json: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("bun-boa-install").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::write(dir.join("package.json"), package_json).unwrap();
    dir
}

fn run(dir: &Path, registry: &Registry, args: &[&str]) -> Output {
    bun()
        .args(args)
        .current_dir(dir)
        .env("BUN_CONFIG_REGISTRY", &registry.url)
        .env("BUN_INSTALL_CACHE_DIR", dir.join("cache"))
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home"))
        .env("NO_COLOR", "1")
        .env_remove("npm_config_registry")
        .output()
        .unwrap()
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Resolves from the registry, downloads and extracts the tarball, links node_modules, writes bun.lock, and runs the
/// project's postinstall script through `posix_spawn_bun`. Then `bun add` and `bun remove` edit package.json.
#[test]
fn installs_adds_and_removes_from_a_local_registry() {
    let registry = registry();
    let dir = project(
        "registry",
        r#"{"name":"app","version":"0.0.0","dependencies":{"foo":"^1.0.0"},"scripts":{"postinstall":"echo ran > postinstall.txt"}}"#,
    );

    let out = run(&dir, &registry, &["install"]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(read(dir.join("node_modules/foo/index.js")), "module.exports = \"foo\";\n");
    assert!(read(dir.join("node_modules/foo/package.json")).contains(r#""version":"1.0.0""#));
    let lock = read(dir.join("bun.lock"));
    assert!(lock.contains(r#""foo": ["foo@1.0.0""#), "{lock}");
    assert_eq!(read(dir.join("postinstall.txt")), "ran\n");
    let requests = registry.requests.lock().unwrap().clone();
    assert!(requests.contains(&"/foo".to_owned()) && requests.contains(&"/foo/-/foo-1.0.0.tgz".to_owned()), "{requests:?}");

    let out = run(&dir, &registry, &["add", "bar"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(read(dir.join("package.json")).contains(r#""bar": "^1.0.0""#), "{}", read(dir.join("package.json")));
    assert_eq!(read(dir.join("node_modules/bar/index.js")), "module.exports = \"bar\";\n");

    let out = run(&dir, &registry, &["remove", "foo"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!read(dir.join("package.json")).contains("\"foo\""), "{}", read(dir.join("package.json")));
    assert!(!dir.join("node_modules/foo").exists());
    assert!(!read(dir.join("bun.lock")).contains("foo@1.0.0"));
}

/// A `file:` dependency installs without the network.
#[test]
fn installs_a_file_dependency() {
    let registry = registry();
    let dir = project("file", r#"{"name":"app","dependencies":{"local":"file:./local"}}"#);
    std::fs::create_dir_all(dir.join("local")).unwrap();
    std::fs::write(dir.join("local/package.json"), r#"{"name":"local","version":"2.0.0"}"#).unwrap();
    let out = run(&dir, &registry, &["install"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(read(dir.join("node_modules/local/package.json")).contains("2.0.0"));
    assert!(read(dir.join("bun.lock")).contains("local@file:local"), "{}", read(dir.join("bun.lock")));
}

/// HTTPS on rustls: the registry's certificate is trusted with `--cafile`, and without it the install fails on it.
#[test]
fn installs_over_tls_with_a_ca_file() {
    let registry = tls_registry();
    let dir = project("tls", r#"{"name":"app","dependencies":{"foo":"^1.0.0"}}"#);
    let cafile = format!("--cafile={}", tls_fixture("ca.crt").display());

    let out = run(&dir, &registry, &["install", &cafile]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(read(dir.join("node_modules/foo/index.js")), "module.exports = \"foo\";\n");
    let requests = registry.requests.lock().unwrap().clone();
    assert!(requests.contains(&"/foo/-/foo-1.0.0.tgz".to_owned()), "{requests:?}");

    let dir = project("tls-untrusted", r#"{"name":"app","dependencies":{"foo":"^1.0.0"}}"#);
    let out = run(&dir, &registry, &["install"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("UNABLE_TO_GET_ISSUER_CERT_LOCALLY"), "{}", text(&out));
    assert!(!dir.join("node_modules/foo").exists());
}
