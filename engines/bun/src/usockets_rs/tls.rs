//! TLS (crypto/openssl.cpp) on rustls with RustCrypto: clients, servers, SNI, client certificates and resumption.
//!
//! `SSL_CTX` and `SSL` are this module's `SslCtx` and `Ssl`, behind the BoringSSL entry points that `bun_http` and
//! `bun_runtime` call on them. A chain that does not verify fails no handshake unless a client asked for
//! `us_socket_set_inline_reject` (or its context for `reject_unauthorized`): the error goes to the handshake
//! callback, as BoringSSL's with `us_verify_callback`, and the owner decides. The name check is rustls's, and reports
//! as openssl.cpp's identity check does (`ERR_TLS_CERT_ALTNAME_INVALID`). A server reads the ClientHello with
//! rustls's `Acceptor`, then picks the context of its name (`sni.rs`) before it answers.

use core::cell::Cell;
use core::ffi::{CStr, c_char, c_int, c_long, c_uint, c_void};
use core::ptr;
use core::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, Ordering};
use std::ffi::CString;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};

use pki_types::pem::PemObject;
use pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{ClientSessionMemoryCache, ResolvesClientCert, Resumption, WebPkiServerVerifier};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{Accepted, Acceptor, ClientHello, ResolvesServerCert, ServerSessionMemoryCache, WebPkiClientVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, HandshakeKind, RootCertStore, SignatureScheme,
    SupportedProtocolVersion,
};

use crate::sni::ServerNames;
use crate::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct VerifyFailure {
    error: c_int,
    code: &'static CStr,
    reason: &'static CStr,
}

impl VerifyFailure {
    const fn of(error: c_int, code: &'static CStr, reason: &'static CStr) -> Self {
        Self { error, code, reason }
    }

    fn to_c(failure: Option<Self>) -> us_bun_verify_error_t {
        match failure {
            None => us_bun_verify_error_t { error: 0, code: ptr::null(), reason: ptr::null() },
            Some(f) => us_bun_verify_error_t { error: f.error, code: f.code.as_ptr(), reason: f.reason.as_ptr() },
        }
    }
}

/// What a finished handshake reports when the peer sent no certificate: a server's, unless it asked for one.
const NO_PEER_CERTIFICATE: VerifyFailure =
    VerifyFailure::of(2, c"UNABLE_TO_GET_ISSUER_CERT", c"unable to get issuer certificate");

/// rustls's verdict as the X509_V_ERR_* code, name and text that OpenSSL gives.
fn failure_of(error: &rustls::Error) -> VerifyFailure {
    use CertificateError::*;
    match error {
        rustls::Error::InvalidCertificate(cert) => match cert {
            NotValidForName | NotValidForNameContext { .. } => VerifyFailure::of(
                62,
                c"ERR_TLS_CERT_ALTNAME_INVALID",
                c"Hostname/IP does not match certificate's altnames",
            ),
            Expired | ExpiredContext { .. } => VerifyFailure::of(10, c"CERT_HAS_EXPIRED", c"certificate has expired"),
            NotValidYet | NotValidYetContext { .. } => {
                VerifyFailure::of(9, c"CERT_NOT_YET_VALID", c"certificate is not yet valid")
            }
            Revoked => VerifyFailure::of(23, c"CERT_REVOKED", c"certificate revoked"),
            BadSignature => VerifyFailure::of(7, c"CERT_SIGNATURE_FAILURE", c"certificate signature failure"),
            InvalidPurpose | InvalidPurposeContext { .. } => {
                VerifyFailure::of(26, c"INVALID_PURPOSE", c"unsupported certificate purpose")
            }
            _ => VerifyFailure::of(
                20,
                c"UNABLE_TO_GET_ISSUER_CERT_LOCALLY",
                c"unable to get local issuer certificate",
            ),
        },
        _ => VerifyFailure::of(1, c"UNSPECIFIED", c"unspecified certificate verification error"),
    }
}

type Failure = Arc<Mutex<Option<VerifyFailure>>>;

fn record_failure(failure: &Mutex<Option<VerifyFailure>>, error: &rustls::Error) {
    *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure_of(error));
}

thread_local! {
    /// The client connection whose server chain rustls checks in this `process_new_packets`: where its failure goes,
    /// and whether one fails the handshake.
    static VERIFYING: Cell<Option<(*const Mutex<Option<VerifyFailure>>, bool)>> = const { Cell::new(None) };
}

/// The webpki check of a server's chain, one per context: rustls resumes a session only under the verifier (and
/// client certificate resolver) that it began with. A failure is recorded for the handshake callback of the
/// connection in `VERIFYING`, and fails the handshake only if that connection says so.
#[derive(Debug)]
struct Recording {
    inner: Arc<WebPkiServerVerifier>,
}

impl ServerCertVerifier for Recording {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let Err(error) = self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) else {
            return Ok(ServerCertVerified::assertion());
        };
        match VERIFYING.get() {
            Some((failure, fatal)) => {
                // SAFETY: `drive` sets it to the failure of the connection that it keeps alive for the call.
                record_failure(unsafe { &*failure }, &error);
                if fatal { Err(error) } else { Ok(ServerCertVerified::assertion()) }
            }
            None => Err(error),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A server's check of a client's chain: asked for with `request_cert`, required with `reject_unauthorized` too. A
/// chain that does not verify is recorded and never fails the handshake, as with `us_verify_callback`.
#[derive(Debug)]
struct RecordingClient {
    inner: Arc<dyn ClientCertVerifier>,
    failure: Failure,
    offer: bool,
    mandatory: bool,
}

impl ClientCertVerifier for RecordingClient {
    fn offer_client_auth(&self) -> bool {
        self.offer
    }

    fn client_auth_mandatory(&self) -> bool {
        self.mandatory
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        match self.inner.verify_client_cert(end_entity, intermediates, now) {
            Ok(verified) => Ok(verified),
            Err(error) => {
                record_failure(&self.failure, &error);
                Ok(ClientCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// The context's certificates and keys: the first whose key signs with a scheme that the peer offers.
#[derive(Debug)]
struct Identities(Arc<[Arc<CertifiedKey>]>);

impl Identities {
    fn choose(&self, schemes: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        self.0.iter().find(|identity| identity.key.choose_scheme(schemes).is_some()).cloned()
    }
}

impl ResolvesServerCert for Identities {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.choose(hello.signature_schemes())
    }
}

impl ResolvesClientCert for Identities {
    fn resolve(&self, _root_hint_subjects: &[&[u8]], schemes: &[SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        self.choose(schemes)
    }

    fn has_certs(&self) -> bool {
        !self.0.is_empty()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    PROVIDER.get_or_init(|| Arc::new(rustls_rustcrypto::provider())).clone()
}

/// `us_ssl_ctx_set_sni_policy`'s bits.
const SNI_REQUEST_CERT: u8 = 1;
const SNI_REJECT_UNAUTHORIZED: u8 = 2;

/// `SSL_CTX`: what to trust, what to present, and the sessions to resume, refcounted.
pub struct SslCtx {
    refs: AtomicU32,
    /// The user's CAs, or the default roots: webpki's and `NODE_EXTRA_CA_CERTS`; and a client's verifier on them.
    roots: Mutex<(Arc<RootCertStore>, Arc<dyn ServerCertVerifier>)>,
    identities: Arc<Identities>,
    versions: Vec<&'static SupportedProtocolVersion>,
    reject_unauthorized: bool,
    request_cert: bool,
    sni_policy: AtomicU8,
    client_sessions: Arc<ClientSessionMemoryCache>,
    server_sessions: Arc<ServerSessionMemoryCache>,
}

impl SslCtx {
    fn roots(&self) -> Arc<RootCertStore> {
        self.roots.lock().unwrap_or_else(|e| e.into_inner()).0.clone()
    }

    fn server_verifier(&self) -> Arc<dyn ServerCertVerifier> {
        self.roots.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
    }
}

fn server_verifier_of(roots: Arc<RootCertStore>) -> Option<Arc<dyn ServerCertVerifier>> {
    let inner = WebPkiServerVerifier::builder_with_provider(roots, provider()).build().ok()?;
    Some(Arc::new(Recording { inner }))
}

static LIVE_CONTEXTS: AtomicI64 = AtomicI64::new(0);

/// `SSL`: one connection's settings until its handshake starts, then its rustls connection.
pub struct Ssl {
    ctx: *mut SslCtx,
    is_server: bool,
    /// A client's SNI, or the name in a server's ClientHello.
    host_name: Option<CString>,
    connect_host: Option<String>,
    alpn: Vec<Vec<u8>>,
    alpn_selected: Vec<u8>,
    chain: Chain,
    options: u32,
    session: Session,
    failure: Failure,
    inline_reject: bool,
    handshake_done: bool,
    resumed: bool,
    write_parked: bool,
    /// Ciphertext that the kernel did not take yet.
    out: Vec<u8>,
    shut_down: bool,
    session_sink: *mut c_void,
    session_on_free: Option<extern "C" fn(*mut c_void)>,
    /// A server's: the names of the listener that accepted it (a ref), or null.
    server_names: *mut ServerNames,
    /// A server's verify mode: SSL_VERIFY_PEER, and SSL_VERIFY_FAIL_IF_NO_PEER_CERT.
    request_cert: bool,
    require_cert: bool,
    /// `us_socket_on_server_name`: the socket's owner picks the context of a name.
    sni_resolver: bool,
    /// What a server read after the ClientHello, while it waits for the context of the name.
    pending_in: Vec<u8>,
}

enum Session {
    /// A client before its first flight.
    NotStarted,
    Accepting(Box<Acceptor>),
    WaitingForContext(Box<Accepted>),
    Open(rustls::Connection),
}

impl Ssl {
    fn new(ctx: *mut SslCtx, is_server: bool) -> Self {
        Ssl {
            ctx,
            is_server,
            host_name: None,
            connect_host: None,
            alpn: Vec::new(),
            alpn_selected: Vec::new(),
            chain: Chain(Vec::new()),
            options: 0,
            session: if is_server { Session::Accepting(Box::default()) } else { Session::NotStarted },
            failure: Arc::new(Mutex::new(None)),
            inline_reject: false,
            handshake_done: false,
            resumed: false,
            write_parked: false,
            out: Vec::new(),
            shut_down: false,
            session_sink: ptr::null_mut(),
            session_on_free: None,
            server_names: ptr::null_mut(),
            request_cert: false,
            require_cert: false,
            sni_resolver: false,
            pending_in: Vec::new(),
        }
    }
}

/// `create_bun_socket_error_t`.
const ERROR_NONE: c_int = 0;
const ERROR_LOAD_CA_FILE: c_int = 1;
const ERROR_INVALID_CA_FILE: c_int = 2;
const ERROR_INVALID_CA: c_int = 3;

const TLS1_2_VERSION: c_int = 0x0303;
const TLS1_3_VERSION: c_int = 0x0304;

fn add_pem(roots: &mut RootCertStore, pem: &[u8]) -> bool {
    let mut any = false;
    for cert in CertificateDer::pem_slice_iter(pem) {
        let Ok(cert) = cert else { return false };
        if roots.add(cert).is_err() {
            return false;
        }
        any = true;
    }
    any
}

unsafe fn read_file(path: *const c_char) -> Option<Vec<u8>> {
    // SAFETY: the caller's NUL-terminated path.
    let path = unsafe { CStr::from_ptr(path) };
    // SAFETY: a path is bytes for the OS.
    std::fs::read(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(path.to_bytes()) }).ok()
}

/// webpki's roots, and the certificates in `NODE_EXTRA_CA_CERTS`, read once.
fn default_roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let Some(extra) = std::env::var_os("NODE_EXTRA_CA_CERTS").filter(|path| !path.is_empty())
                && let Ok(pem) = std::fs::read(extra)
            {
                for cert in CertificateDer::pem_slice_iter(&pem).flatten() {
                    let _ = roots.add(cert);
                }
            }
            Arc::new(roots)
        })
        .clone()
}

/// The certificates of one `cert` entry, leaf first.
fn parse_chain(pem: &[u8]) -> Option<Vec<CertificateDer<'static>>> {
    let chain = CertificateDer::pem_slice_iter(pem).collect::<Result<Vec<_>, _>>().ok()?;
    (!chain.is_empty()).then_some(chain)
}

/// A key in PEM: PKCS#8, PKCS#1 or SEC1, or PKCS#8 under `passphrase` (an empty one without, as `passphrase_cb`).
fn parse_key(pem: &[u8], passphrase: &[u8]) -> Option<PrivateKeyDer<'static>> {
    if let Ok(key) = PrivateKeyDer::from_pem_slice(pem) {
        return Some(key);
    }
    let (label, der) = pkcs8::der::pem::decode_vec(pem).ok()?;
    if label != "ENCRYPTED PRIVATE KEY" {
        return None;
    }
    let encrypted = pkcs8::EncryptedPrivateKeyInfoRef::try_from(der.as_slice()).ok()?;
    let key = encrypted.decrypt(passphrase).ok()?;
    Some(PrivateKeyDer::Pkcs8(key.as_bytes().to_vec().into()))
}

/// Whether the public key of `leaf` verifies what `key` signs. RustCrypto's keys do not give their public half, so
/// `CertifiedKey::keys_match` cannot tell.
fn key_signs_for(leaf: &CertificateDer<'_>, key: &dyn rustls::sign::SigningKey) -> bool {
    let algorithms = provider().signature_verification_algorithms;
    let schemes: Vec<SignatureScheme> = algorithms.mapping.iter().map(|&(scheme, _)| scheme).collect();
    let Some(signer) = key.choose_scheme(&schemes) else { return false };
    let message = b"usockets_rs: does this key belong to this certificate";
    let Ok(signature) = signer.sign(message) else { return false };
    let Ok(cert) = webpki::EndEntityCert::try_from(leaf) else { return false };
    algorithms
        .mapping
        .iter()
        .filter(|&&(scheme, _)| scheme == signer.scheme())
        .flat_map(|&(_, verifiers)| verifiers)
        .any(|verifier| cert.verify_signature(*verifier, message, &signature).is_ok())
}

/// Each key with the chain whose leaf it belongs to; a key that none matches fails, as SSL_CTX_use_PrivateKey's
/// "key values mismatch".
fn pair_identities(chains: Vec<Vec<CertificateDer<'static>>>, keys: Vec<PrivateKeyDer<'static>>) -> Option<Vec<Arc<CertifiedKey>>> {
    if chains.is_empty() || keys.is_empty() {
        return Some(Vec::new());
    }
    let provider = provider();
    let mut identities = Vec::new();
    for key in keys {
        let key = provider.key_provider.load_private_key(key).ok()?;
        let chain = chains.iter().find(|chain| key_signs_for(&chain[0], &*key))?;
        identities.push(Arc::new(CertifiedKey::new(chain.clone(), key)));
    }
    Some(identities)
}

/// `us_ssl_ctx_build_raw`'s semantics: `ca` or `ca_file_name` replace the default roots; `cert` and `key` (or their
/// files) are what a server presents, or a client when asked; `request_cert` and `reject_unauthorized` are a
/// server's verify mode. Ciphers, signature algorithms, curves, DH parameters and CRLs are rustls's own.
unsafe fn build_ctx(o: &bun_uws_sys::BunSocketContextOptions) -> Result<SslCtx, c_int> {
    // SAFETY: the options' strings are null or NUL-terminated, and each array has its count of them.
    unsafe {
        let strings = |list: *const *const c_char, count: u32| -> Vec<&[u8]> {
            if list.is_null() {
                return Vec::new();
            }
            (0..count as usize).map(|i| CStr::from_ptr(*list.add(i)).to_bytes()).collect()
        };
        let roots = if !o.ca_file_name.is_null() {
            let pem = read_file(o.ca_file_name).ok_or(ERROR_LOAD_CA_FILE)?;
            let mut roots = RootCertStore::empty();
            if !add_pem(&mut roots, &pem) {
                return Err(ERROR_INVALID_CA_FILE);
            }
            Arc::new(roots)
        } else if o.ca_count > 0 && !o.ca.is_null() {
            let mut roots = RootCertStore::empty();
            for pem in strings(o.ca, o.ca_count) {
                if !add_pem(&mut roots, pem) {
                    return Err(ERROR_INVALID_CA);
                }
            }
            Arc::new(roots)
        } else {
            default_roots()
        };

        let chains = if !o.cert_file_name.is_null() {
            vec![read_file(o.cert_file_name).as_deref().and_then(parse_chain).ok_or(ERROR_NONE)?]
        } else {
            strings(o.cert, o.cert_count).into_iter().map(|pem| parse_chain(pem).ok_or(ERROR_NONE)).collect::<Result<_, _>>()?
        };
        let passphrase = if o.passphrase.is_null() { &[][..] } else { CStr::from_ptr(o.passphrase).to_bytes() };
        let keys = if !o.key_file_name.is_null() {
            vec![read_file(o.key_file_name).and_then(|pem| parse_key(&pem, passphrase)).ok_or(ERROR_NONE)?]
        } else {
            strings(o.key, o.key_count)
                .into_iter()
                .map(|pem| parse_key(pem, passphrase).ok_or(ERROR_NONE))
                .collect::<Result<_, _>>()?
        };
        let identities = pair_identities(chains, keys).ok_or(ERROR_NONE)?;

        let min = if o.ssl_min_version != 0 { o.ssl_min_version } else { TLS1_2_VERSION };
        let max = if o.ssl_max_version != 0 { o.ssl_max_version } else { TLS1_3_VERSION };
        let versions: Vec<&'static SupportedProtocolVersion> = [(&rustls::version::TLS13, TLS1_3_VERSION), (&rustls::version::TLS12, TLS1_2_VERSION)]
            .into_iter()
            .filter(|&(_, v)| min <= v && v <= max)
            .map(|(version, _)| version)
            .collect();
        if versions.is_empty() {
            return Err(ERROR_NONE);
        }

        let verifier = server_verifier_of(roots.clone()).ok_or(ERROR_INVALID_CA)?;
        Ok(SslCtx {
            refs: AtomicU32::new(1),
            roots: Mutex::new((roots, verifier)),
            identities: Arc::new(Identities(identities.into())),
            versions,
            reject_unauthorized: o.reject_unauthorized != 0,
            request_cert: o.request_cert != 0,
            sni_policy: AtomicU8::new(0),
            client_sessions: Arc::new(ClientSessionMemoryCache::new(256)),
            server_sessions: ServerSessionMemoryCache::new(256),
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_ssl_ctx_from_options(
    options: bun_uws_sys::BunSocketContextOptions,
    err: *mut c_int,
) -> *mut SslCtx {
    // SAFETY: the caller's options, and null or its out-int (`create_bun_socket_error_t`, an int-sized enum).
    unsafe {
        let result = build_ctx(&options);
        if !err.is_null() {
            *err = result.as_ref().err().copied().unwrap_or(ERROR_NONE);
        }
        match result {
            Ok(ctx) => {
                LIVE_CONTEXTS.fetch_add(1, Ordering::Relaxed);
                Box::into_raw(Box::new(ctx))
            }
            Err(_) => ptr::null_mut(),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn us_ssl_ctx_live_count() -> c_long {
    LIVE_CONTEXTS.load(Ordering::Relaxed) as c_long
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_CTX_up_ref(ctx: *mut SslCtx) -> c_int {
    // SAFETY: the caller holds a ref.
    unsafe { (*ctx).refs.fetch_add(1, Ordering::Relaxed) };
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_CTX_free(ctx: *mut SslCtx) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: the caller gives back a ref; the last one frees the context.
    unsafe {
        if (*ctx).refs.fetch_sub(1, Ordering::AcqRel) == 1 {
            drop(Box::from_raw(ctx));
            LIVE_CONTEXTS.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// node:tls `secureContext.context.addCACert(pem)`: the certificates of `content` join the context's roots. 0 if
/// `content` has none, or a malformed one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_ssl_ctx_add_ca_cert(ctx: *mut SslCtx, content: *const c_char) -> c_int {
    if ctx.is_null() || content.is_null() {
        return 0;
    }
    // SAFETY: the caller's live context and NUL-terminated PEM.
    unsafe {
        let mut roots = (*ctx).roots.lock().unwrap_or_else(|e| e.into_inner());
        let mut extended = RootCertStore::clone(&roots.0);
        if !add_pem(&mut extended, CStr::from_ptr(content).to_bytes()) {
            return 0;
        }
        let extended = Arc::new(extended);
        let Some(verifier) = server_verifier_of(extended.clone()) else { return 0 };
        *roots = (extended, verifier);
        1
    }
}

/// The client-certificate policy of a per-name context: the SNI switch adds it to the connection's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_ssl_ctx_set_sni_policy(ctx: *mut SslCtx, request_cert: c_int, reject_unauthorized: c_int) {
    if ctx.is_null() {
        return;
    }
    let policy = if request_cert != 0 { SNI_REQUEST_CERT } else { 0 }
        | if reject_unauthorized != 0 { SNI_REJECT_UNAUTHORIZED } else { 0 };
    // SAFETY: the caller's live context.
    unsafe { (*ctx).sni_policy.store(policy, Ordering::Relaxed) };
}

/// Whether the context that SNI picked for this connection closes it on a client certificate that does not verify.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_server_name_reject_unauthorized(s: *mut us_socket_t) -> c_int {
    // SAFETY: the caller's live socket.
    unsafe {
        if (*s).ssl.is_null() {
            return 0;
        }
        let policy = (*(*(*s).ssl).ctx).sni_policy.load(Ordering::Relaxed);
        c_int::from(policy & SNI_REQUEST_CERT != 0 && policy & SNI_REJECT_UNAUTHORIZED != 0)
    }
}

/// Buffer pools share certificate bytes between connections in BoringSSL; rustls has none.
#[unsafe(no_mangle)]
pub extern "C" fn CRYPTO_BUFFER_POOL_new() -> *mut c_void {
    static POOL: u8 = 0;
    (&raw const POOL).cast_mut().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn SSL_CTX_set0_buffer_pool(_ctx: *mut SslCtx, _pool: *mut c_void) {}

// ── the SSL entry points ───────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_get_SSL_CTX(ssl: *const Ssl) -> *mut SslCtx {
    // SAFETY: the caller's live SSL.
    unsafe { (*ssl).ctx }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_set_tlsext_host_name(ssl: *mut Ssl, name: *const c_char) -> c_int {
    // SAFETY: the caller's live SSL and NUL-terminated name.
    unsafe { (*ssl).host_name = Some(CStr::from_ptr(name).to_owned()) };
    1
}

/// A client's SNI, or the name that a server's client asked for.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_get_servername(ssl: *const Ssl, _type: c_int) -> *const c_char {
    // SAFETY: the caller's live SSL; the name lives as long as it.
    unsafe { (*ssl).host_name.as_ref().map_or(ptr::null(), |name| name.as_ptr()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_session_reused(ssl: *const Ssl) -> c_int {
    // SAFETY: the caller's live SSL.
    c_int::from(unsafe { (*ssl).resumed })
}

/// `protos` in the wire format: each name after its length byte. 0 is success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_set_alpn_protos(ssl: *mut Ssl, protos: *const u8, len: usize) -> c_int {
    // SAFETY: the caller's live SSL and `len` bytes at `protos`.
    let mut wire = if len == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(protos, len) } };
    let mut alpn = Vec::new();
    while let Some((&n, rest)) = wire.split_first() {
        let Some((name, rest)) = rest.split_at_checked(n as usize) else { return 1 };
        alpn.push(name.to_vec());
        wire = rest;
    }
    // SAFETY: as above.
    unsafe { (*ssl).alpn = alpn };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_get0_alpn_selected(ssl: *const Ssl, out_data: *mut *const u8, out_len: *mut c_uint) {
    // SAFETY: the caller's live SSL and out-pointers; the bytes live as long as the SSL.
    unsafe {
        let selected = &(*ssl).alpn_selected;
        *out_data = if selected.is_empty() { ptr::null() } else { selected.as_ptr() };
        *out_len = selected.len() as c_uint;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_set_options(ssl: *mut Ssl, options: u32) -> u32 {
    // SAFETY: the caller's live SSL. rustls has no legacy renegotiation to allow, so options are only kept.
    unsafe {
        (*ssl).options |= options;
        (*ssl).options
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_clear_options(ssl: *mut Ssl, options: u32) -> u32 {
    // SAFETY: as above.
    unsafe {
        (*ssl).options &= !options;
        (*ssl).options
    }
}

/// OCSP and SCTs are only asked for: nothing reads them.
#[unsafe(no_mangle)]
pub extern "C" fn SSL_enable_ocsp_stapling(_ssl: *mut Ssl) {}

#[unsafe(no_mangle)]
pub extern "C" fn SSL_enable_signed_cert_timestamps(_ssl: *mut Ssl) {}

/// An `X509`: the DER that the peer sent.
pub struct X509 {
    der: Vec<u8>,
}

/// `STACK_OF(X509)`, with `sk_num` and `sk_value`: the peer's chain, leaf first, once the handshake is done.
pub struct Chain(Vec<X509>);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_get_peer_cert_chain(ssl: *mut Ssl) -> *mut Chain {
    // SAFETY: the caller's live SSL.
    unsafe {
        if (&(*ssl).chain.0).is_empty() { ptr::null_mut() } else { &raw mut (*ssl).chain }
    }
}

/// Only peer chains are stacks in this build.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sk_num(stack: *const Chain) -> usize {
    // SAFETY: a chain from `SSL_get_peer_cert_chain`.
    unsafe { (&(*stack).0).len() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sk_value(stack: *const Chain, i: usize) -> *mut X509 {
    // SAFETY: as above.
    let chain = unsafe { &(*stack).0 };
    chain.get(i).map_or(ptr::null_mut(), |x| ptr::from_ref(x).cast_mut())
}

/// The DER of `x`: its length, and with a buffer at `*outp`, the bytes there and `*outp` past them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn i2d_X509(x: *const X509, outp: *mut *mut u8) -> c_int {
    // SAFETY: the caller's certificate, and null or room for its DER.
    unsafe {
        let der = &(*x).der;
        if !outp.is_null() && !(*outp).is_null() {
            ptr::copy_nonoverlapping(der.as_ptr(), *outp, der.len());
            *outp = (*outp).add(der.len());
        }
        der.len() as c_int
    }
}

/// openssl.cpp's: whether the verify step of this handshake checked the server's name. rustls's always does, so
/// `SSL_get_verify_result` already says whether it matched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_ssl_identity_checked(ssl: *mut Ssl) -> c_int {
    // SAFETY: the caller's live SSL.
    c_int::from(unsafe { (*ssl).handshake_done })
}

/// rustls resumes sessions from the context's store; BoringSSL sessions never exist here.
#[unsafe(no_mangle)]
pub extern "C" fn SSL_set_session(_ssl: *mut Ssl, _session: *mut c_void) -> c_int {
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn SSL_get_verify_result(ssl: *const Ssl) -> c_long {
    // SAFETY: the caller's live SSL.
    c_long::from(unsafe { verify_failure(ssl) }.map_or(0, |f| f.error))
}

unsafe fn verify_failure(ssl: *const Ssl) -> Option<VerifyFailure> {
    // SAFETY: the caller's live SSL.
    unsafe { *(*ssl).failure.lock().unwrap_or_else(|e| e.into_inner()) }
}

/// `us_ssl_socket_verify_error_from_ssl`: the chain's verdict, or after a handshake without a peer certificate,
/// that there was none.
unsafe fn verify_verdict(ssl: *const Ssl) -> Option<VerifyFailure> {
    // SAFETY: the caller's live SSL.
    unsafe {
        verify_failure(ssl).or(((*ssl).handshake_done && (&(*ssl).chain.0).is_empty()).then_some(NO_PEER_CERTIFICATE))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_ssl_socket_verify_error_from_ssl(ssl: *mut Ssl) -> us_bun_verify_error_t {
    // SAFETY: the caller's live SSL.
    VerifyFailure::to_c(unsafe { verify_verdict(ssl) })
}

thread_local! {
    static LAST_ERROR: core::cell::RefCell<Option<String>> = const { core::cell::RefCell::new(None) };
}

/// The thread's last TLS failure, into `reason` (NUL-terminated): 1 if there was one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_ssl_take_error_reason(reason: *mut c_char, length: usize) -> c_int {
    let Some(text) = LAST_ERROR.with(|e| e.borrow_mut().take()) else { return 0 };
    if length == 0 {
        return 1;
    }
    let n = text.len().min(length - 1);
    // SAFETY: the caller's buffer of `length` bytes.
    unsafe {
        ptr::copy_nonoverlapping(text.as_ptr().cast::<c_char>(), reason, n);
        *reason.add(n) = 0;
    }
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_set_session_sink(s: *mut us_socket_t, sink: *mut c_void, on_free: extern "C" fn(*mut c_void)) {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        let ssl = (*s).ssl;
        if ssl.is_null() {
            on_free(sink);
            return;
        }
        if let Some(previous) = (*ssl).session_on_free.replace(on_free) {
            previous((*ssl).session_sink);
        }
        (*ssl).session_sink = sink;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_session_sink(s: *mut us_socket_t) -> *mut c_void {
    // SAFETY: the caller's live socket.
    unsafe { if (*s).ssl.is_null() { ptr::null_mut() } else { (*(*s).ssl).session_sink } }
}

/// A chain that does not verify fails the handshake itself.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_set_inline_reject(s: *mut us_socket_t) {
    // SAFETY: the caller's live socket.
    unsafe {
        if !(*s).ssl.is_null() {
            (*(*s).ssl).inline_reject = true;
        }
    }
}

/// The owner of this server-side socket picks the context of the client's name (`us_dispatch_socket_server_name`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_on_server_name(s: *mut us_socket_t) {
    // SAFETY: the caller's live socket.
    unsafe {
        if !s.is_null() && !(*s).ssl.is_null() {
            (*(*s).ssl).sni_resolver = true;
        }
    }
}

// ── the socket side ────────────────────────────────────────────────────────

/// `us_internal_ssl_attach` for a client: the SSL takes a ref on `ctx`.
pub(crate) unsafe fn attach_client(s: *mut us_socket_t, ctx: *mut SslCtx, connect_host: Option<&CStr>) {
    // SAFETY: the caller's live socket and context.
    unsafe {
        SSL_CTX_up_ref(ctx);
        let mut ssl = Ssl::new(ctx, false);
        ssl.connect_host = connect_host.map(|h| h.to_string_lossy().into_owned());
        (*s).ssl = Box::into_raw(Box::new(ssl));
    }
}

/// `us_internal_ssl_attach` for a server: the context's verify mode, and the names of `listener` (null for an
/// adopted socket).
pub(crate) unsafe fn attach_server(s: *mut us_socket_t, ctx: *mut SslCtx, listener: *mut us_listen_socket_t) {
    // SAFETY: the caller's live socket, context and listener.
    unsafe {
        SSL_CTX_up_ref(ctx);
        let mut ssl = Ssl::new(ctx, true);
        ssl.request_cert = (*ctx).request_cert;
        ssl.require_cert = (*ctx).request_cert && (*ctx).reject_unauthorized;
        if !listener.is_null() {
            ssl.server_names = crate::sni::names_ref(listener);
        }
        (*s).ssl = Box::into_raw(Box::new(ssl));
    }
}

/// `us_internal_ssl_detach`: after the socket's last callback.
pub(crate) unsafe fn free(s: *mut us_socket_t) {
    // SAFETY: the caller's socket, whose SSL nothing uses any more.
    unsafe {
        let ssl = core::mem::replace(&mut (*s).ssl, ptr::null_mut());
        if ssl.is_null() {
            return;
        }
        let ssl = Box::from_raw(ssl);
        if let Some(on_free) = ssl.session_on_free {
            on_free(ssl.session_sink);
        }
        crate::sni::names_unref(ssl.server_names);
        SSL_CTX_free(ssl.ctx);
    }
}

pub(crate) unsafe fn native_handle(s: *mut us_socket_t) -> *mut c_void {
    // SAFETY: the caller's live socket.
    unsafe { (*s).ssl.cast() }
}

pub(crate) unsafe fn verify_error(s: *mut us_socket_t) -> us_bun_verify_error_t {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        if (*s).is_closed {
            return VerifyFailure::to_c(None);
        }
        VerifyFailure::to_c(verify_verdict((*s).ssl))
    }
}

pub(crate) unsafe fn is_shut_down(s: *const us_socket_t) -> bool {
    // SAFETY: the caller's live TLS socket.
    unsafe { (*(*s).ssl).shut_down }
}

/// Sends what rustls has for the peer; false if the kernel left some for the writable event.
unsafe fn flush(s: *mut us_socket_t) -> bool {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        let ssl = &mut *(*s).ssl;
        if let Session::Open(conn) = &mut ssl.session {
            while conn.wants_write() {
                if conn.write_tls(&mut ssl.out).is_err() {
                    break;
                }
            }
        }
        if ssl.out.is_empty() {
            return true;
        }
        let sent = crate::socket::raw_send(s, &ssl.out);
        ssl.out.drain(..sent);
        ssl.out.is_empty()
    }
}

/// Flushes, and waits for writable if the kernel did not take it all.
unsafe fn flush_or_rearm(s: *mut us_socket_t) {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        if !flush(s) {
            (*s).last_write_failed = true;
            crate::socket::rearm_writable(s);
        }
    }
}

fn record_error(error: &rustls::Error) {
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(error.to_string()));
}

/// The handshake failed: tell the owner, then close.
unsafe fn handshake_failed(s: *mut us_socket_t, error: Option<&rustls::Error>) {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        if let Some(error) = error {
            record_error(error);
        }
        flush(s);
        let failure = verify_failure((*s).ssl);
        us_dispatch_handshake(s, 0, VerifyFailure::to_c(failure));
        if !(*s).is_closed {
            crate::socket::us_internal_socket_close_raw(s, LIBUS_SOCKET_CLOSE_CODE_CONNECTION_RESET, ptr::null_mut());
        }
    }
}

/// `us_internal_ssl_on_open`: the owner hears of the socket, then a client sends its ClientHello (with the SNI and
/// ALPN that on_open set); a server waits for one.
pub(crate) unsafe fn on_open(s: *mut us_socket_t, is_client: bool, ip: *mut c_char, ip_length: c_int) {
    // SAFETY: the caller's live TLS socket, which on_open may close or adopt.
    unsafe {
        let s = us_dispatch_open(s, c_int::from(is_client), ip, ip_length);
        if s.is_null() || (*s).is_closed || (*s).ssl.is_null() {
            return;
        }
        if is_client {
            start_client(s);
        }
    }
}

/// The ClientHello, with the context's roots, identities and session store.
unsafe fn start_client(s: *mut us_socket_t) {
    // SAFETY: the caller's live TLS client socket.
    unsafe {
        let ssl = &mut *(*s).ssl;
        if !matches!(ssl.session, Session::NotStarted) {
            return;
        }
        let ctx = &*ssl.ctx;
        let name = match &ssl.host_name {
            Some(name) => name.to_string_lossy().into_owned(),
            None => ssl.connect_host.clone().unwrap_or_default(),
        };
        let name = name.trim_start_matches('[').trim_end_matches(']').to_owned();
        let Ok(server_name) = ServerName::try_from(name) else {
            handshake_failed(s, None);
            return;
        };
        let conn = rustls::ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&ctx.versions)
            .and_then(|builder| {
                let mut config = builder
                    .dangerous()
                    .with_custom_certificate_verifier(ctx.server_verifier())
                    .with_client_cert_resolver(ctx.identities.clone());
                config.alpn_protocols = ssl.alpn.clone();
                config.resumption = Resumption::store(ctx.client_sessions.clone());
                rustls::ClientConnection::new(Arc::new(config), server_name)
            });
        match conn {
            Ok(conn) => ssl.session = Session::Open(conn.into()),
            Err(error) => {
                handshake_failed(s, Some(&error));
                return;
            }
        }
        flush_or_rearm(s);
    }
}

/// What a server answers with: the context's identities and session store, and the connection's verify mode.
fn server_config(ctx: &SslCtx, ssl: &Ssl) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
    let inner = if ssl.request_cert {
        WebPkiClientVerifier::builder_with_provider(ctx.roots(), provider())
            .allow_unauthenticated()
            .build()
            .map_err(|error| rustls::Error::General(error.to_string()))?
    } else {
        WebPkiClientVerifier::no_client_auth()
    };
    let verifier = Arc::new(RecordingClient {
        inner,
        failure: ssl.failure.clone(),
        offer: ssl.request_cert,
        mandatory: ssl.request_cert && ssl.require_cert,
    });
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&ctx.versions)?
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(ctx.identities.clone());
    config.session_storage = ctx.server_sessions.clone();
    Ok(Arc::new(config))
}

/// The context that SNI picked (a ref that the SSL takes), and its client-certificate policy on top of the
/// connection's.
unsafe fn apply_selected_ctx(ssl: &mut Ssl, ctx: *mut SslCtx) {
    // SAFETY: a live context.
    unsafe {
        SSL_CTX_free(core::mem::replace(&mut ssl.ctx, ctx));
        let policy = (*ctx).sni_policy.load(Ordering::Relaxed);
        if policy & SNI_REQUEST_CERT != 0 {
            ssl.request_cert = true;
            ssl.require_cert |= policy & SNI_REJECT_UNAUTHORIZED != 0;
        }
    }
}

/// The context of the name in the listener's tree, if it has one.
unsafe fn apply_tree_ctx(ssl: &mut Ssl) {
    // SAFETY: the SSL's names, if any, are alive while it holds its ref.
    unsafe {
        let Some(name) = &ssl.host_name else { return };
        let ctx = ServerNames::find(ssl.server_names, name);
        if !ctx.is_null() {
            SSL_CTX_up_ref(ctx);
            apply_selected_ctx(ssl, ctx);
        }
    }
}

enum Step {
    /// The connection exists.
    Open,
    /// The ClientHello is not all here, or the owner has not picked a context yet.
    Wait,
    /// The handshake failed, or the owner closed the socket.
    Gone,
}

/// A server's ClientHello: then the context of its name, and the connection.
unsafe fn accept_hello(s: *mut us_socket_t, mut data: &[u8]) -> Step {
    // SAFETY: the caller's live TLS server socket.
    unsafe {
        loop {
            let ssl = &mut *(*s).ssl;
            let Session::Accepting(acceptor) = &mut ssl.session else { return Step::Open };
            if !data.is_empty() && acceptor.read_tls(&mut data).is_err() {
                handshake_failed(s, None);
                return Step::Gone;
            }
            match acceptor.accept() {
                Ok(None) if data.is_empty() => return Step::Wait,
                Ok(None) => {}
                Ok(Some(accepted)) => {
                    ssl.pending_in.extend_from_slice(data);
                    return choose_context(s, Box::new(accepted));
                }
                Err((error, mut alert)) => {
                    let _ = alert.write_all(&mut ssl.out);
                    handshake_failed(s, Some(&error));
                    return Step::Gone;
                }
            }
        }
    }
}

/// `us_select_cert_cb` and `sni_cb`: the owner's resolver first (the listener's, else the socket's), then the
/// listener's tree, else the default context.
unsafe fn choose_context(s: *mut us_socket_t, accepted: Box<Accepted>) -> Step {
    // SAFETY: the caller's live TLS server socket; the resolver may close it.
    unsafe {
        let ssl = &mut *(*s).ssl;
        // A name that does not fit openssl.cpp's 256-byte buffer is no name.
        ssl.host_name = accepted
            .client_hello()
            .server_name()
            .filter(|name| name.len() < 256)
            .and_then(|name| CString::new(name).ok());
        let Some(name) = ssl.host_name.clone() else { return open_server(s, accepted) };
        let names = ssl.server_names;
        let listener_resolver = (!names.is_null()).then(|| (*names).on_server_name).flatten();
        if listener_resolver.is_some() || ssl.sni_resolver {
            let mut abort = 0;
            let chosen = match listener_resolver {
                Some(resolve) => resolve((*names).ls, name.as_ptr(), &mut abort, s),
                None => us_dispatch_socket_server_name(s, name.as_ptr(), &mut abort),
            };
            if (*s).is_closed || (*s).ssl.is_null() {
                SSL_CTX_free(chosen);
                return Step::Gone;
            }
            let ssl = &mut *(*s).ssl;
            match abort {
                1 => {
                    SSL_CTX_free(chosen);
                    crate::socket::us_socket_close(s, 0, ptr::null_mut());
                    return Step::Gone;
                }
                2 => {
                    SSL_CTX_free(chosen);
                    ssl.session = Session::WaitingForContext(accepted);
                    return Step::Wait;
                }
                _ if !chosen.is_null() => {
                    apply_selected_ctx(ssl, chosen);
                    return open_server(s, accepted);
                }
                _ => {}
            }
        }
        apply_tree_ctx(&mut *(*s).ssl);
        open_server(s, accepted)
    }
}

/// The ServerHello and the rest of the server's flight.
unsafe fn open_server(s: *mut us_socket_t, accepted: Box<Accepted>) -> Step {
    // SAFETY: the caller's live TLS server socket.
    unsafe {
        let ssl = &mut *(*s).ssl;
        let conn = match server_config(&*ssl.ctx, ssl) {
            Ok(config) => (*accepted).into_connection(config),
            Err(error) => {
                handshake_failed(s, Some(&error));
                return Step::Gone;
            }
        };
        match conn {
            Ok(conn) => {
                ssl.session = Session::Open(conn.into());
                flush_or_rearm(s);
                Step::Open
            }
            Err((error, mut alert)) => {
                let _ = alert.write_all(&mut ssl.out);
                handshake_failed(s, Some(&error));
                Step::Gone
            }
        }
    }
}

/// An async SNICallback answered: `owned_ctx` (a ref, or null for the tree's or the default), or `error`, which
/// drops the connection without an alert.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_sni_resolve(s: *mut us_socket_t, owned_ctx: *mut SslCtx, error: c_int) {
    // SAFETY: null, or the caller's socket; the context is a ref that this takes.
    unsafe {
        if s.is_null() || (*s).is_closed || (*s).ssl.is_null() {
            SSL_CTX_free(owned_ctx);
            return;
        }
        let ssl = &mut *(*s).ssl;
        if !matches!(ssl.session, Session::WaitingForContext(_)) {
            SSL_CTX_free(owned_ctx);
            return;
        }
        let Session::WaitingForContext(accepted) = core::mem::replace(&mut ssl.session, Session::NotStarted) else {
            return;
        };
        if error != 0 {
            SSL_CTX_free(owned_ctx);
            crate::socket::us_socket_close(s, 0, ptr::null_mut());
            return;
        }
        if owned_ctx.is_null() {
            apply_tree_ctx(ssl);
        } else {
            apply_selected_ctx(ssl, owned_ctx);
        }
        if let Step::Open = open_server(s, accepted) {
            let pending = core::mem::take(&mut (*(*s).ssl).pending_in);
            drive(s, &pending);
        }
    }
}

/// Ciphertext from the peer: a server's ClientHello first, then the handshake goes on, and what it decrypts goes
/// to on_data.
pub(crate) unsafe fn on_data(s: *mut us_socket_t, data: &[u8]) -> *mut us_socket_t {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        let ssl = &mut *(*s).ssl;
        match ssl.session {
            Session::Accepting(_) => match accept_hello(s, data) {
                Step::Open => {
                    let pending = core::mem::take(&mut (*(*s).ssl).pending_in);
                    drive(s, &pending)
                }
                Step::Wait | Step::Gone => s,
            },
            Session::WaitingForContext(_) => {
                ssl.pending_in.extend_from_slice(data);
                s
            }
            _ => drive(s, data),
        }
    }
}

unsafe fn drive(mut s: *mut us_socket_t, data: &[u8]) -> *mut us_socket_t {
    // SAFETY: the caller's live TLS socket; each callback may close it, after which it is only read.
    unsafe {
        let mut rest = data;
        let mut plaintext = [0u8; 16 * 1024];
        loop {
            let ssl = &mut *(*s).ssl;
            let Session::Open(conn) = &mut ssl.session else { return s };
            if !rest.is_empty() && conn.read_tls(&mut rest).is_err() {
                break;
            }
            VERIFYING.set(Some((Arc::as_ptr(&ssl.failure), ssl.inline_reject || (*ssl.ctx).reject_unauthorized)));
            let processed = conn.process_new_packets();
            VERIFYING.set(None);
            if let Err(error) = processed {
                if !ssl.handshake_done {
                    handshake_failed(s, Some(&error));
                } else {
                    record_error(&error);
                    flush(s);
                    crate::socket::us_internal_socket_close_raw(s, libc::ECONNRESET, ptr::null_mut());
                }
                return s;
            }
            if !ssl.handshake_done && !conn.is_handshaking() {
                ssl.handshake_done = true;
                ssl.resumed = conn.handshake_kind() == Some(HandshakeKind::Resumed);
                ssl.alpn_selected = conn.alpn_protocol().map(<[u8]>::to_vec).unwrap_or_default();
                let peer = conn.peer_certificates().unwrap_or_default();
                ssl.chain = Chain(peer.iter().map(|der| X509 { der: der.to_vec() }).collect());
                flush(s);
                us_dispatch_handshake(s, 1, VerifyFailure::to_c(verify_verdict((*s).ssl)));
                if (*s).is_closed {
                    return s;
                }
                if core::mem::take(&mut (*(*s).ssl).write_parked) {
                    s = us_dispatch_writable(s);
                    if s.is_null() || (*s).is_closed {
                        return s;
                    }
                }
            }
            let mut peer_closed = false;
            loop {
                let Session::Open(conn) = &mut (*(*s).ssl).session else { return s };
                match conn.reader().read(&mut plaintext) {
                    Ok(0) => {
                        peer_closed = true;
                        break;
                    }
                    Ok(n) => {
                        s = us_dispatch_data(s, plaintext.as_mut_ptr().cast(), n as c_int);
                        if s.is_null() || (*s).is_closed {
                            return s;
                        }
                    }
                    Err(_) => break,
                }
            }
            flush(s);
            if peer_closed {
                // close_notify: the end of the stream.
                s = us_dispatch_end(s);
                if !s.is_null() && !(*s).is_closed {
                    crate::socket::us_internal_socket_close_raw(s, LIBUS_SOCKET_CLOSE_CODE_CLEAN_SHUTDOWN, ptr::null_mut());
                }
                return s;
            }
            if rest.is_empty() {
                break;
            }
        }
        s
    }
}

/// The kernel has room: what rustls held back goes first, then the owner may write.
pub(crate) unsafe fn on_writable(s: *mut us_socket_t) -> *mut us_socket_t {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        if !flush(s) {
            (*s).last_write_failed = true;
            return s;
        }
        let ssl = &*(*s).ssl;
        if !ssl.handshake_done {
            return s;
        }
        us_dispatch_writable(s)
    }
}

/// Plaintext from the owner: 0 until the handshake is done, else what rustls took.
pub(crate) unsafe fn write(s: *mut us_socket_t, data: &[u8]) -> c_int {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        if (*s).is_closed || (*(*s).ssl).shut_down {
            return 0;
        }
        let ssl = &mut *(*s).ssl;
        if !ssl.handshake_done {
            ssl.write_parked = true;
            return 0;
        }
        if !ssl.out.is_empty() && !flush(s) {
            (*s).last_write_failed = true;
            crate::socket::rearm_writable(s);
            return 0;
        }
        let Session::Open(conn) = &mut (*(*s).ssl).session else { return 0 };
        let written = conn.writer().write(data).unwrap_or(0);
        if !flush(s) || written < data.len() {
            (*s).last_write_failed = true;
            crate::socket::rearm_writable(s);
        }
        written as c_int
    }
}

/// close_notify, without waiting for the peer's.
pub(crate) unsafe fn send_close_notify(s: *mut us_socket_t) {
    // SAFETY: the caller's live TLS socket.
    unsafe {
        let ssl = &mut *(*s).ssl;
        if ssl.shut_down {
            return;
        }
        ssl.shut_down = true;
        if let Session::Open(conn) = &mut ssl.session {
            conn.send_close_notify();
        }
        flush(s);
    }
}

// ── STARTTLS ───────────────────────────────────────────────────────────────

/// A plain socket into `group` with TLS on it: a client (whose SNI is `sni`), or a server with this verify mode.
/// Nothing is dispatched: the owner repoints its ext, then calls `us_socket_start_tls_handshake`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_adopt_tls(
    s: *mut us_socket_t,
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut SslCtx,
    sni: *const c_char,
    is_client: c_int,
    request_cert: c_int,
    reject_unauthorized: c_int,
    old_ext_size: c_int,
    ext_size: c_int,
) -> *mut us_socket_t {
    // SAFETY: the caller's live socket, group and context, and a NUL-terminated name or null.
    unsafe {
        if (*s).is_closed || crate::socket::us_socket_is_shut_down(s) != 0 {
            return ptr::null_mut();
        }
        let new_s = crate::adopt::us_socket_adopt(s, group, kind, old_ext_size, ext_size);
        if new_s.is_null() {
            return ptr::null_mut();
        }
        if is_client != 0 {
            attach_client(new_s, ssl_ctx, None);
            if !sni.is_null() {
                SSL_set_tlsext_host_name((*new_s).ssl, sni);
            }
        } else {
            attach_server(new_s, ssl_ctx, ptr::null_mut());
            let ssl = &mut *(*new_s).ssl;
            ssl.request_cert = request_cert != 0;
            ssl.require_cert = request_cert != 0 && reject_unauthorized != 0;
        }
        crate::socket::us_socket_resume(new_s);
        new_s
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_start_tls_handshake(s: *mut us_socket_t) {
    // SAFETY: the caller's live socket.
    unsafe {
        if (*s).ssl.is_null() || (*s).is_closed {
            return;
        }
        if !(*(*s).ssl).is_server {
            start_client(s);
        }
    }
}

/// Bytes that the plain socket read before it was adopted into TLS, through the TLS read path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_tls_feed(s: *mut us_socket_t, data: *const c_char, length: c_int) -> *mut us_socket_t {
    // SAFETY: the caller's live socket and `length` bytes at `data`.
    unsafe {
        if (*s).is_closed || (*s).ssl.is_null() || length <= 0 {
            return s;
        }
        on_data(s, core::slice::from_raw_parts(data.cast(), length as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tls/");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES}{name}")).unwrap()
    }

    #[test]
    fn rustcrypto_provider_builds_a_config() {
        assert!(WebPkiServerVerifier::builder_with_provider(default_roots(), provider()).build().is_ok());
    }

    #[test]
    fn a_key_under_a_passphrase_opens_with_it() {
        let plain = parse_key(&fixture("server-key.txt"), b"").unwrap();
        let encrypted = fixture("server-key-encrypted.txt");
        assert_eq!(parse_key(&encrypted, b"hunter2").unwrap().secret_der(), plain.secret_der());
        assert!(parse_key(&encrypted, b"wrong").is_none());
        assert!(parse_key(&encrypted, b"").is_none());
    }

    #[test]
    fn keys_pair_with_their_own_certificates() {
        let chains = vec![parse_chain(&fixture("sni.crt")).unwrap(), parse_chain(&fixture("server.crt")).unwrap()];
        let key = parse_key(&fixture("server-key.txt"), b"").unwrap();
        let identities = pair_identities(chains.clone(), vec![key]).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].cert[0], chains[1][0]);
        let stranger = parse_key(&fixture("client-key.txt"), b"").unwrap();
        assert!(pair_identities(chains, vec![stranger]).is_none());
    }
}
