//! The DNS cache that connects resolve through: src/runtime/dns_jsc/dns.rs's `internal` module and its
//! `Bun__addrinfo_*` C ABI, which uSockets (and HTTP/3's connect) calls. A name is looked up once per TTL
//! (`BUN_CONFIG_DNS_TIME_TO_LIVE_SECONDS`, 30 by default) on a small pool of threads, concurrent connects to it share
//! that lookup, and the addresses come back with the families interleaved (RFC 8305). `localhost` names are
//! `::1, 127.0.0.1` without the resolver, and a name that is not a hostname fails without it.

use core::ffi::{CStr, c_char, c_int, c_void};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use std::collections::VecDeque;
use std::ffi::CString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::context::us_internal_dns_callback_threadsafe;
use crate::*;

/// `struct addrinfo_result_entry`: the addrinfo uSockets reads, and the address it points at.
#[repr(C)]
pub struct ResultEntry {
    info: libc::addrinfo,
    addr: libc::sockaddr_storage,
}

// SAFETY: `info` points only into the slice of entries that it is in, which nothing writes after it is made.
unsafe impl Send for ResultEntry {}
// SAFETY: as above.
unsafe impl Sync for ResultEntry {}

/// `struct addrinfo_result`. Borrows the request it was made from.
#[repr(C)]
pub struct RequestResult {
    pub(crate) entries: *const ResultEntry,
    pub(crate) error: c_int,
}

/// A connecting socket that waits for a request, as an address: it is only touched on its loop's thread.
struct Waiter(usize);

/// Shared by the cache, the lookup that fills it and the sockets that use or wait for it.
pub struct Request {
    host: Option<CString>,
    /// For getaddrinfo (glibc's UDP port 0 bug); not part of the key: results are port-agnostic.
    port: u16,
    /// Set with `notify` locked.
    result: OnceLock<Result<Box<[ResultEntry]>, c_int>>,
    notify: Mutex<Vec<Waiter>>,
    /// Seconds since the epoch.
    created_at: u32,
    /// Only written with the cache locked.
    valid: AtomicBool,
}

impl Request {
    fn matches(&self, host: Option<&[u8]>) -> bool {
        self.host.as_deref().map(CStr::to_bytes) == host
    }

    fn is_expired(&self, now: u32) -> bool {
        if self.result.get().is_none() {
            return false;
        }
        if now.saturating_sub(self.created_at) > ttl_seconds() {
            self.valid.store(false, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Gives the waiter back if the result is already set.
    fn join(&self, waiter: Waiter) -> Result<(), Waiter> {
        let mut notify = self.notify.lock().unwrap_or_else(|e| e.into_inner());
        if self.result.get().is_some() {
            return Err(waiter);
        }
        notify.push(waiter);
        Ok(())
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn ttl_seconds() -> u32 {
    static TTL: OnceLock<u32> = OnceLock::new();
    *TTL.get_or_init(|| {
        std::env::var("BUN_CONFIG_DNS_TIME_TO_LIVE_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(30)
    })
}

fn now_seconds() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)
}

const MAX_ENTRIES: usize = 256;

static CACHE: Mutex<Vec<Arc<Request>>> = Mutex::new(Vec::new());

fn cache() -> std::sync::MutexGuard<'static, Vec<Arc<Request>>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Only the cache holds it.
fn is_unused(entry: &Arc<Request>) -> bool {
    Arc::strong_count(entry) == 1
}

/// A valid entry for `host`; stale ones that nobody uses are dropped on the way.
fn find(cache: &mut Vec<Arc<Request>>, host: Option<&[u8]>, now: u32) -> Option<Arc<Request>> {
    let mut i = 0;
    while i < cache.len() {
        let entry = &cache[i];
        if entry.matches(host) {
            if entry.valid.load(Ordering::Relaxed) && !entry.is_expired(now) {
                return Some(entry.clone());
            }
            if is_unused(entry) {
                cache.swap_remove(i);
                continue;
            }
        }
        i += 1;
    }
    None
}

/// Into the cache if there is room or an entry that nobody uses.
fn try_push(cache: &mut Vec<Arc<Request>>, entry: Arc<Request>) -> bool {
    if cache.len() < MAX_ENTRIES {
        cache.push(entry);
        return true;
    }
    match cache.iter_mut().find(|e| is_unused(e)) {
        Some(unused) => {
            *unused = entry;
            true
        }
        None => false,
    }
}

fn hints() -> libc::addrinfo {
    // SAFETY: all-zero is the empty hints of getaddrinfo(3).
    let mut hints: libc::addrinfo = unsafe { core::mem::zeroed() };
    hints.ai_family = libc::AF_UNSPEC;
    // Only the families the system has: https://github.com/nodejs/node/commit/54dd7c38e507b35ee0ffadc41a716f1782b0d32f
    hints.ai_flags = libc::AI_ADDRCONFIG;
    hints.ai_socktype = libc::SOCK_STREAM;
    if env_flag("BUN_FEATURE_FLAG_DISABLE_ADDRCONFIG") {
        hints.ai_flags &= !libc::AI_ADDRCONFIG;
    }
    if env_flag("BUN_FEATURE_FLAG_DISABLE_IPV6") {
        hints.ai_family = libc::AF_INET;
    } else if env_flag("BUN_FEATURE_FLAG_DISABLE_IPV4") {
        hints.ai_family = libc::AF_INET6;
    }
    hints
}

/// Chrome's `IsLocalHostname` (RFC 6761 §6.3): `localhost` or `*.localhost`, one trailing dot allowed.
fn is_localhost_name(host: &[u8]) -> bool {
    let host = host.strip_suffix(b".").unwrap_or(host);
    host.eq_ignore_ascii_case(b"localhost")
        || (host.len() >= b".localhost".len() && host[host.len() - b".localhost".len()..].eq_ignore_ascii_case(b".localhost"))
}

pub(crate) unsafe fn sockaddr_to_std(addr: *const libc::sockaddr) -> Option<SocketAddr> {
    // SAFETY: the caller passes a sockaddr of the family it says.
    unsafe {
        match c_int::from((*addr).sa_family) {
            libc::AF_INET => {
                let a = &*addr.cast::<libc::sockaddr_in>();
                Some(SocketAddr::from((u32::from_be(a.sin_addr.s_addr).to_be_bytes(), u16::from_be(a.sin_port))))
            }
            libc::AF_INET6 => {
                let a = &*addr.cast::<libc::sockaddr_in6>();
                Some(SocketAddr::V6(std::net::SocketAddrV6::new(
                    a.sin6_addr.s6_addr.into(),
                    u16::from_be(a.sin6_port),
                    a.sin6_flowinfo,
                    a.sin6_scope_id,
                )))
            }
            _ => None,
        }
    }
}

/// The addresses, the families interleaved for as long as both last (RFC 8305 §4), so that an unroutable family
/// cannot take all of a connect's parallel attempts. Linked as getaddrinfo's list is.
fn process_results(addresses: &[SocketAddr]) -> Box<[ResultEntry]> {
    let first_is_v4 = addresses.first().map(SocketAddr::is_ipv4);
    let (first, other): (Vec<&SocketAddr>, Vec<&SocketAddr>) =
        addresses.iter().partition(|a| Some(a.is_ipv4()) == first_is_v4);
    let mut ordered = Vec::with_capacity(addresses.len());
    let pairs = first.len().min(other.len());
    for i in 0..pairs {
        ordered.push(*first[i]);
        ordered.push(*other[i]);
    }
    ordered.extend(first[pairs..].iter().copied());
    ordered.extend(other[pairs..].iter().copied());
    let mut entries: Box<[ResultEntry]> = ordered
        .iter()
        .map(|address| {
            let (addr, len) = crate::bsd::sockaddr_from_std(address);
            // SAFETY: all-zero is an empty addrinfo.
            let mut info: libc::addrinfo = unsafe { core::mem::zeroed() };
            info.ai_family = if address.is_ipv4() { libc::AF_INET } else { libc::AF_INET6 };
            info.ai_socktype = libc::SOCK_STREAM;
            info.ai_addrlen = len;
            ResultEntry { info, addr }
        })
        .collect();
    let mut next: *mut libc::addrinfo = ptr::null_mut();
    for entry in entries.iter_mut().rev() {
        entry.info.ai_next = next;
        entry.info.ai_addr = (&raw mut entry.addr).cast();
        next = &raw mut entry.info;
    }
    entries
}

fn localhost_results(port: u16) -> Box<[ResultEntry]> {
    let family = hints().ai_family;
    let addresses: Vec<SocketAddr> = [IpAddr::V6(Ipv6Addr::LOCALHOST), IpAddr::V4(Ipv4Addr::LOCALHOST)]
        .into_iter()
        .filter(|ip| family == libc::AF_UNSPEC || (family == libc::AF_INET) == ip.is_ipv4())
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    process_results(&addresses)
}

fn lookup(host: Option<&CStr>, port: u16, hints: &libc::addrinfo) -> Result<Vec<SocketAddr>, c_int> {
    let service = (port > 0).then(|| CString::new(port.to_string()).expect("digits"));
    let mut result: *mut libc::addrinfo = ptr::null_mut();
    // SAFETY: NUL-terminated strings or null, hints and an out-pointer.
    let rc = unsafe {
        libc::getaddrinfo(
            host.map_or(ptr::null(), CStr::as_ptr),
            service.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            hints,
            &mut result,
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    let mut addresses = Vec::new();
    let mut entry = result;
    while !entry.is_null() {
        // SAFETY: a node of getaddrinfo's list.
        unsafe {
            if let Some(mut address) = sockaddr_to_std((*entry).ai_addr) {
                address.set_port(port);
                addresses.push(address);
            }
            entry = (*entry).ai_next;
        }
    }
    // SAFETY: the list getaddrinfo returned.
    unsafe { libc::freeaddrinfo(result) };
    Ok(addresses)
}

/// Chrome's `IsAllLocalhostOfOneFamily`: what AI_ADDRCONFIG leaves of a hosts-file loopback name.
fn is_all_loopback_of_one_family(addresses: &[SocketAddr]) -> bool {
    let Some(first) = addresses.first() else { return false };
    addresses.iter().all(|a| a.ip().is_loopback() && a.is_ipv4() == first.is_ipv4())
}

fn resolve(req: &Request) -> Result<Box<[ResultEntry]>, c_int> {
    let mut hints = hints();
    let host = req.host.as_deref();
    let mut result = lookup(host, req.port, &hints);
    // Chrome's retries: AI_ADDRCONFIG left nothing, or only one family's loopback.
    let retry = match &result {
        Ok(list) => is_all_loopback_of_one_family(list),
        Err(e) => *e == libc::EAI_NONAME,
    };
    if hints.ai_flags & libc::AI_ADDRCONFIG != 0 && retry {
        hints.ai_flags &= !libc::AI_ADDRCONFIG;
        let unfiltered = lookup(host, req.port, &hints);
        if unfiltered.is_ok() || result.is_err() {
            result = unfiltered;
        }
    }
    result.map(|list| process_results(&list))
}

/// The result, then every waiter to its loop.
fn complete(req: Arc<Request>, result: Result<Box<[ResultEntry]>, c_int>) {
    let guard = cache();
    let mut notify = req.notify.lock().unwrap_or_else(|e| e.into_inner());
    let _ = req.result.set(result);
    for waiter in notify.drain(..) {
        // SAFETY: the contract of `Bun__addrinfo_set`; `Bun__addrinfo_cancel` waits for `notify`.
        unsafe { us_internal_dns_callback_threadsafe(waiter.0 as *mut us_connecting_socket_t) };
    }
    drop(notify);
    // Before the unlock: `Bun__addrinfo_freeRequest` evicts only if its ref and the cache's are the last two.
    drop(req);
    drop(guard);
}

/// getaddrinfo on a few threads of their own, not one per lookup.
fn run_on_pool(req: Arc<Request>) {
    struct Pool {
        queue: Mutex<VecDeque<Arc<Request>>>,
        ready: Condvar,
    }
    static POOL: OnceLock<Pool> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(4, usize::from).clamp(2, 8);
        for i in 0..threads {
            std::thread::Builder::new()
                .name(format!("usockets dns {i}"))
                .spawn(|| {
                    let pool = POOL.get().expect("the pool is set before its threads look");
                    loop {
                        let req = {
                            let mut queue = pool.queue.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                if let Some(req) = queue.pop_front() {
                                    break req;
                                }
                                queue = pool.ready.wait(queue).unwrap_or_else(|e| e.into_inner());
                            }
                        };
                        let result = resolve(&req);
                        complete(req, result);
                    }
                })
                .expect("usockets: cannot start a thread for name resolution");
        }
        Pool { queue: Mutex::new(VecDeque::new()), ready: Condvar::new() }
    });
    pool.queue.lock().unwrap_or_else(|e| e.into_inner()).push_back(req);
    pool.ready.notify_one();
}

/// The request for `host` (a ref), and whether its result is already there. A prefetch takes no ref.
fn getaddrinfo(host: Option<&[u8]>, port: u16, want_ref: bool) -> (Option<Arc<Request>>, bool) {
    let mut guard = cache();
    let now = now_seconds();
    if !env_flag("BUN_FEATURE_FLAG_DISABLE_DNS_CACHE")
        && let Some(entry) = find(&mut guard, host, now)
    {
        let hit = entry.result.get().is_some();
        return (want_ref.then_some(entry), hit);
    }
    let req = Arc::new(Request {
        host: host.map(|h| CString::new(h).unwrap_or_default()),
        port,
        result: OnceLock::new(),
        notify: Mutex::new(Vec::new()),
        created_at: now,
        valid: AtomicBool::new(true),
    });
    let _ = try_push(&mut guard, req.clone());
    drop(guard);
    let caller = want_ref.then(|| req.clone());
    if host.is_some_and(|h| !bun_dns::is_valid_hostname(h)) {
        complete(req, Err(libc::EAI_NONAME));
        return (caller, true);
    }
    if host.is_some_and(is_localhost_name) {
        complete(req, Ok(localhost_results(port)));
        return (caller, true);
    }
    run_on_pool(req);
    (caller, false)
}

/// Starts the lookup, so that a connect later finds it in the cache.
#[unsafe(no_mangle)]
fn __bun_dns_prefetch(_loop: *const c_void, hostname: &[u8], port: u16) {
    let host = (!hostname.is_empty()).then_some(hostname);
    let _ = getaddrinfo(host, port, false);
}

/// Writes a ref to `*request`: 0 if its result is there, 1 if it is still coming.
///
/// # Safety
/// `host` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Bun__addrinfo_get(_loop: *mut us_loop_t, host: *const c_char, port: u16, request: *mut *const Request) -> c_int {
    // SAFETY: the caller's host and out-pointer.
    unsafe {
        let host = (!host.is_null()).then(|| CStr::from_ptr(host).to_bytes());
        let (req, hit) = getaddrinfo(host, port, true);
        *request = Arc::into_raw(req.expect("a ref was asked for"));
        c_int::from(!hit)
    }
}

/// `socket` hears of the result on its loop (`dns_ready_head`), now if it is already there.
///
/// # Safety
/// On `socket`'s loop's thread; `socket` lives until it is notified or has cancelled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Bun__addrinfo_set(request: *const Request, socket: *mut us_connecting_socket_t) {
    // SAFETY: a ref from `Bun__addrinfo_get`.
    let request = unsafe { &*request };
    if request.join(Waiter(socket as usize)).is_err() {
        // SAFETY: the caller's socket.
        unsafe { us_internal_dns_callback_threadsafe(socket) };
    }
}

/// 0 once there is a result (the socket is queued on its loop already), 1 if it was taken off the waiters.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Bun__addrinfo_cancel(request: *const Request, socket: *mut us_connecting_socket_t) -> c_int {
    // SAFETY: a ref from `Bun__addrinfo_get`.
    let request = unsafe { &*request };
    let mut notify = request.notify.lock().unwrap_or_else(|e| e.into_inner());
    match notify.iter().position(|w| w.0 == socket as usize) {
        Some(i) => {
            notify.swap_remove(i);
            1
        }
        None => 0,
    }
}

/// Gives back a ref of `Bun__addrinfo_get`. An error invalidates the entry: a refused connect may mean stale
/// addresses, and a failed lookup is never cached.
///
/// # Safety
/// `request` is a ref from `Bun__addrinfo_get`, given back once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Bun__addrinfo_freeRequest(request: *const Request, err: c_int) {
    let mut guard = cache();
    // SAFETY: the caller's ref. It drops before `guard`, so the next holder of the lock sees the count lowered.
    let req = unsafe { Arc::from_raw(request) };
    if err != 0 {
        req.valid.store(false, Ordering::Relaxed);
    }
    let nearly_full = guard.len() * 5 >= MAX_ENTRIES * 4;
    // 2: `req` and the cache's.
    if Arc::strong_count(&req) == 2 && (nearly_full || !req.valid.load(Ordering::Relaxed))
        && let Some(i) = guard.iter().position(|e| Arc::ptr_eq(e, &req))
    {
        guard.swap_remove(i);
    }
    drop(req);
}

/// The result of a request that has one. Its entries live as long as the request.
///
/// # Safety
/// `request` is a ref from `Bun__addrinfo_get` whose result is set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Bun__addrinfo_getRequestResult(request: *const Request) -> RequestResult {
    // SAFETY: the caller's ref.
    let request = unsafe { &*request };
    match request.result.get().expect("a request with a result") {
        Ok(entries) => RequestResult { entries: entries.first().map_or(ptr::null(), ptr::from_ref), error: 0 },
        Err(error) => RequestResult { entries: ptr::null(), error: *error },
    }
}

unsafe extern "C" {
    fn inet_aton(cp: *const c_char, inp: *mut libc::in_addr) -> c_int;
}

/// What `Bun__parseIpAddress` accepts: an IPv4 address (with inet_aton's shorthands) or an IPv6 one with a zone.
pub(crate) fn parse_ip(host: &CStr, port: u16) -> Option<SocketAddr> {
    let mut v4: libc::in_addr = libc::in_addr { s_addr: 0 };
    // SAFETY: a NUL-terminated string and an in_addr to write.
    if unsafe { inet_aton(host.as_ptr(), &mut v4) } != 0 {
        return Some(SocketAddr::from((u32::from_be(v4.s_addr).to_be_bytes(), port)));
    }
    let text = host.to_str().ok()?;
    let text = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')).unwrap_or(text);
    let (address, zone) = match text.split_once('%') {
        Some((address, zone)) => (address, Some(zone)),
        None => (text, None),
    };
    let address: Ipv6Addr = address.parse().ok()?;
    let scope_id = match zone {
        None => 0,
        Some(zone) => zone.parse::<u32>().ok().or_else(|| {
            let name = CString::new(zone).ok()?;
            // SAFETY: a NUL-terminated interface name.
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            (index != 0).then_some(index)
        })?,
    };
    Some(SocketAddr::V6(std::net::SocketAddrV6::new(address, port, 0, scope_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(entries: &[ResultEntry]) -> Vec<bool> {
        entries.iter().map(|e| e.info.ai_family == libc::AF_INET).collect()
    }

    #[test]
    fn families_interleave_while_both_last() {
        let v4 = |n: u8| SocketAddr::from(([10, 0, 0, n], 80));
        let v6 = |n: u16| SocketAddr::from(([0xfd00, 0, 0, 0, 0, 0, 0, n], 80));
        let entries = process_results(&[v6(1), v6(2), v6(3), v4(1), v4(2)]);
        assert_eq!(order(&entries), [false, true, false, true, false]);
        // The list is linked in that order, as getaddrinfo's is.
        let mut node = &raw const entries[0].info;
        let mut seen = 0;
        while !node.is_null() {
            seen += 1;
            // SAFETY: the list in `entries`.
            node = unsafe { (*node).ai_next };
        }
        assert_eq!(seen, 5);
    }

    #[test]
    fn localhost_and_invalid_names_skip_the_resolver() {
        assert!(is_localhost_name(b"localhost") && is_localhost_name(b"a.LOCALHOST.") && !is_localhost_name(b"localhost.com"));
        let (req, hit) = getaddrinfo(Some(b"not a hostname"), 80, true);
        assert!(hit);
        let req = req.unwrap();
        assert_eq!(*req.result.get().unwrap().as_ref().err().unwrap(), libc::EAI_NONAME);
        let (req, hit) = getaddrinfo(Some(b"localhost"), 80, true);
        assert!(hit);
        assert_eq!(req.unwrap().result.get().unwrap().as_ref().unwrap().len(), 2);
        // A second lookup is a hit on the same request.
        let (again, hit) = getaddrinfo(Some(b"localhost"), 81, true);
        assert!(hit && again.is_some());
    }
}
