//! A non-blocking connection: TCP or a Unix socket, with or without TLS (rustls on RustCrypto).

use std::cell::OnceCell;
use std::io::{self, Read, Write};
use std::sync::Arc;

use mio::event::Source;
use mio::net::{TcpStream, UnixStream};
use mio::{Interest, Registry, Token};
use pki_types::pem::PemObject;
use pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};

pub enum Socket {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Socket::Tcp(s) => s.read(buf),
            Socket::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Socket::Tcp(s) => s.write(buf),
            Socket::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Source for Socket {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            Socket::Tcp(s) => s.register(registry, token, interests),
            Socket::Unix(s) => s.register(registry, token, interests),
        }
    }

    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            Socket::Tcp(s) => s.reregister(registry, token, interests),
            Socket::Unix(s) => s.reregister(registry, token, interests),
        }
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        match self {
            Socket::Tcp(s) => s.deregister(registry),
            Socket::Unix(s) => s.deregister(registry),
        }
    }
}

impl Socket {
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        match self {
            Socket::Tcp(s) => s.take_error(),
            Socket::Unix(s) => s.take_error(),
        }
    }

    /// Whether a TCP connect has completed. A Unix socket connects at once.
    pub fn is_connected(&self) -> bool {
        match self {
            Socket::Tcp(s) => s.peer_addr().is_ok(),
            Socket::Unix(_) => true,
        }
    }

    pub fn shutdown_write(&self) {
        let _ = match self {
            Socket::Tcp(s) => s.shutdown(std::net::Shutdown::Write),
            Socket::Unix(s) => s.shutdown(std::net::Shutdown::Write),
        };
    }

    pub fn peer_addr(&self) -> Option<std::net::SocketAddr> {
        match self {
            Socket::Tcp(s) => s.peer_addr().ok(),
            Socket::Unix(_) => None,
        }
    }
}

/// What a read brought.
#[derive(Default, Debug)]
pub struct ReadOutcome {
    pub eof: bool,
}

/// A connection's bytes in and out: what is queued to be written, and TLS between them and the socket.
pub struct Transport {
    pub socket: Socket,
    tls: Option<rustls::Connection>,
    out: Vec<u8>,
    written: usize,
}

impl Transport {
    pub fn new(socket: Socket, tls: Option<rustls::Connection>) -> Transport {
        Transport {
            socket,
            tls,
            out: Vec::new(),
            written: 0,
        }
    }

    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// Reads what is available into `into` until the socket would block.
    pub fn read(&mut self, into: &mut Vec<u8>) -> io::Result<ReadOutcome> {
        let mut chunk = [0u8; 16 * 1024];
        let Some(tls) = self.tls.as_mut() else {
            loop {
                match self.socket.read(&mut chunk) {
                    Ok(0) => return Ok(ReadOutcome { eof: true }),
                    Ok(n) => into.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(ReadOutcome::default()),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
        };
        let mut eof = false;
        loop {
            let mut would_block = false;
            match tls.read_tls(&mut self.socket) {
                Ok(0) => eof = true,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => would_block = true,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            let state = tls
                .process_new_packets()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut plaintext = state.plaintext_bytes_to_read();
            while plaintext > 0 {
                let want = plaintext.min(chunk.len());
                let n = tls.reader().read(&mut chunk[..want])?;
                if n == 0 {
                    break;
                }
                into.extend_from_slice(&chunk[..n]);
                plaintext -= n;
            }
            if state.peer_has_closed() {
                eof = true;
            }
            if eof || would_block {
                break;
            }
        }
        self.flush()?;
        Ok(ReadOutcome { eof })
    }

    /// Queues `data` and writes what the socket takes.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        match self.tls.as_mut() {
            Some(tls) => tls.writer().write_all(data)?,
            None => self.out.extend_from_slice(data),
        }
        self.flush()
    }

    /// Writes what is queued until the socket would block.
    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(tls) = self.tls.as_mut() {
            while tls.wants_write() {
                match tls.write_tls(&mut self.socket) {
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            return Ok(());
        }
        while self.written < self.out.len() {
            match self.socket.write(&self.out[self.written..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.written += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        self.out.clear();
        self.written = 0;
        Ok(())
    }

    /// Whether bytes wait to be written.
    pub fn has_pending_output(&self) -> bool {
        match &self.tls {
            Some(tls) => tls.wants_write(),
            None => self.written < self.out.len(),
        }
    }

    pub fn is_handshaking(&self) -> bool {
        self.tls.as_ref().is_some_and(|tls| tls.is_handshaking())
    }

    /// Sends TLS's close_notify, then ends the write side.
    pub fn close(&mut self) {
        if let Some(tls) = self.tls.as_mut() {
            tls.send_close_notify();
        }
        let _ = self.flush();
        self.socket.shutdown_write();
    }
}

pub fn provider() -> Arc<CryptoProvider> {
    thread_local! {
        static PROVIDER: OnceCell<Arc<CryptoProvider>> = const { OnceCell::new() };
    }
    PROVIDER.with(|p| p.get_or_init(|| Arc::new(rustls_rustcrypto::provider())).clone())
}

/// How a client checks the server's certificate.
#[derive(Clone, Debug, Default)]
pub struct TlsOptions {
    /// `rejectUnauthorized: false`: any certificate.
    pub insecure: bool,
    /// PEM certificates to trust instead of the bundled roots.
    pub ca: Vec<Vec<u8>>,
    /// SNI and the name the certificate must have, if not the URL's host.
    pub server_name: Option<String>,
}

#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

pub fn client_config(options: &TlsOptions) -> io::Result<Arc<ClientConfig>> {
    thread_local! {
        static DEFAULT: OnceCell<Arc<ClientConfig>> = const { OnceCell::new() };
        static INSECURE: OnceCell<Arc<ClientConfig>> = const { OnceCell::new() };
    }
    let build = |options: &TlsOptions| -> io::Result<Arc<ClientConfig>> {
        let builder = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(tls_error)?;
        let mut config = if options.insecure {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider())))
                .with_no_client_auth()
        } else {
            let mut roots = RootCertStore::empty();
            if options.ca.is_empty() {
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
            for pem in &options.ca {
                for cert in CertificateDer::pem_slice_iter(pem) {
                    roots.add(cert.map_err(tls_error)?).map_err(tls_error)?;
                }
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    };
    if !options.ca.is_empty() {
        return build(options);
    }
    let cell = if options.insecure { &INSECURE } else { &DEFAULT };
    cell.with(|cell| {
        if let Some(config) = cell.get() {
            return Ok(config.clone());
        }
        let config = build(options)?;
        let _ = cell.set(config.clone());
        Ok(config)
    })
}

pub fn client_connection(options: &TlsOptions, host: &str) -> io::Result<rustls::Connection> {
    let name = options.server_name.as_deref().unwrap_or(host);
    let name = name.trim_start_matches('[').trim_end_matches(']');
    let server_name = ServerName::try_from(name.to_owned()).map_err(tls_error)?;
    let connection =
        rustls::ClientConnection::new(client_config(options)?, server_name).map_err(tls_error)?;
    Ok(connection.into())
}

/// A server's certificate chain and key, in PEM.
pub fn server_config(cert: &[u8], key: &[u8]) -> io::Result<Arc<ServerConfig>> {
    let certs = CertificateDer::pem_slice_iter(cert)
        .collect::<Result<Vec<_>, _>>()
        .map_err(tls_error)?;
    let key = PrivateKeyDer::from_pem_slice(key).map_err(tls_error)?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(tls_error)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
