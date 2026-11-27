//! SNI on listen sockets (crypto/openssl.cpp's `us_server_names_t` and crypto/sni_tree.h): the contexts by host name
//! pattern, and the resolver that runs before them. Loop thread only.

use core::ffi::{CStr, c_char, c_int};
use core::ptr;
use std::collections::BTreeMap;

use crate::tls::{SSL_CTX_free, SSL_CTX_up_ref, SslCtx};
use crate::us_listen_socket_t;
use crate::us_socket_t;

/// Returns the context for a name on the in-flight handshake (a ref the caller takes), or null for the tree's.
pub(crate) type OnServerName =
    unsafe extern "C" fn(*mut us_listen_socket_t, *const c_char, *mut c_int, *mut us_socket_t) -> *mut SslCtx;

/// A ClientHello can arrive after its listen socket closed: one ref for the listen socket, one per accepted SSL.
pub(crate) struct ServerNames {
    refs: u32,
    /// Null once the listen socket closed.
    pub(crate) ls: *mut us_listen_socket_t,
    tree: Tree,
    pub(crate) on_server_name: Option<OnServerName>,
}

/// sni_tree.h's `sni_node`, in a vector: labels left to right, `*` matching any one label, a literal label tried
/// before the wildcard. Each node holds a ref on its context, or null.
struct Tree(Vec<Node>);

#[derive(Default)]
struct Node {
    ctx: *mut SslCtx,
    /// By the label in ASCII lower case.
    children: BTreeMap<Vec<u8>, usize>,
}

/// The first label of `rest`, and `rest` past the dot after it.
fn next_label<'a>(rest: &mut &'a [u8]) -> &'a [u8] {
    let end = bun_core::strings::index_of_char_usize(rest, b'.').unwrap_or(rest.len());
    let label = &rest[..end];
    *rest = &rest[(end + 1).min(rest.len())..];
    label
}

impl Tree {
    fn find(&self, node: usize, mut rest: &[u8]) -> *mut SslCtx {
        let node = &self.0[node];
        if rest.is_empty() {
            return node.ctx;
        }
        let label = next_label(&mut rest);
        if let Some(&child) = node.children.get(&label.to_ascii_lowercase()) {
            let found = self.find(child, rest);
            if !found.is_null() {
                return found;
            }
            if label == b"*" {
                return ptr::null_mut();
            }
        }
        match node.children.get(b"*".as_slice()) {
            Some(&child) => self.find(child, rest),
            None => ptr::null_mut(),
        }
    }

    /// The last registration of a name wins. `ctx` is a ref that the tree takes.
    fn add(&mut self, mut rest: &[u8], ctx: *mut SslCtx) {
        if self.0.is_empty() {
            self.0.push(Node::default());
        }
        let mut node = 0;
        while !rest.is_empty() {
            let label = next_label(&mut rest).to_ascii_lowercase();
            node = match self.0[node].children.get(&label) {
                Some(&child) => child,
                None => {
                    let child = self.0.len();
                    self.0.push(Node::default());
                    self.0[node].children.insert(label, child);
                    child
                }
            };
        }
        let old = core::mem::replace(&mut self.0[node].ctx, ctx);
        // SAFETY: the tree's ref on the context that this name had.
        unsafe { SSL_CTX_free(old) };
    }
}

impl Drop for ServerNames {
    fn drop(&mut self) {
        for node in &self.tree.0 {
            // SAFETY: the tree's refs.
            unsafe { SSL_CTX_free(node.ctx) };
        }
    }
}

impl ServerNames {
    /// The context of `hostname` in the tree, borrowed.
    pub(crate) fn find(names: *const ServerNames, hostname: &CStr) -> *mut SslCtx {
        // SAFETY: null, or names that a ref keeps alive.
        match unsafe { names.as_ref() } {
            Some(names) if !names.tree.0.is_empty() => names.tree.find(0, hostname.to_bytes()),
            _ => ptr::null_mut(),
        }
    }
}

unsafe fn names_of(ls: *mut us_listen_socket_t) -> *mut ServerNames {
    // SAFETY: the caller's live listener.
    unsafe {
        if (*ls).server_names.is_null() {
            (*ls).server_names = Box::into_raw(Box::new(ServerNames {
                refs: 1,
                ls,
                tree: Tree(Vec::new()),
                on_server_name: None,
            }))
            .cast();
        }
        (*ls).server_names.cast()
    }
}

/// A ref on the names of `ls`, for a socket that it accepts.
pub(crate) unsafe fn names_ref(ls: *mut us_listen_socket_t) -> *mut ServerNames {
    // SAFETY: the caller's live listener.
    unsafe {
        let names = names_of(ls);
        (*names).refs += 1;
        names
    }
}

pub(crate) unsafe fn names_unref(names: *mut ServerNames) {
    // SAFETY: null, or the caller's ref.
    unsafe {
        if names.is_null() {
            return;
        }
        (*names).refs -= 1;
        if (*names).refs == 0 {
            drop(Box::from_raw(names));
        }
    }
}

/// `us_internal_listen_socket_ssl_free`: the listener's refs, when it closes.
pub(crate) unsafe fn listener_free(ls: *mut us_listen_socket_t) {
    // SAFETY: the caller's listener.
    unsafe {
        SSL_CTX_free(core::mem::replace(&mut (*ls).ssl_ctx, ptr::null_mut()));
        let names = core::mem::replace(&mut (*ls).server_names, ptr::null_mut()).cast::<ServerNames>();
        if !names.is_null() {
            (*names).ls = ptr::null_mut();
            names_unref(names);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_add_server_name(ls: *mut us_listen_socket_t, hostname_pattern: *const c_char, ctx: *mut SslCtx) {
    // SAFETY: the caller's listener, NUL-terminated pattern and live context.
    unsafe {
        if (*ls).ssl_ctx.is_null() || ctx.is_null() {
            return;
        }
        let names = names_of(ls);
        SSL_CTX_up_ref(ctx);
        (*names).tree.add(CStr::from_ptr(hostname_pattern).to_bytes(), ctx);
    }
}

/// The context registered for `hostname_pattern`, as a ref that the caller gives back.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_find_server_name_ctx(ls: *mut us_listen_socket_t, hostname_pattern: *const c_char) -> *mut SslCtx {
    // SAFETY: the caller's listener and NUL-terminated pattern.
    unsafe {
        let ctx = ServerNames::find((*ls).server_names.cast(), CStr::from_ptr(hostname_pattern));
        if !ctx.is_null() {
            SSL_CTX_up_ref(ctx);
        }
        ctx
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_set_default_ssl_ctx(ls: *mut us_listen_socket_t, ctx: *mut SslCtx) {
    // SAFETY: the caller's listener and live context.
    unsafe {
        if (*ls).ssl_ctx == ctx {
            return;
        }
        SSL_CTX_up_ref(ctx);
        SSL_CTX_free(core::mem::replace(&mut (*ls).ssl_ctx, ctx));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_on_server_name(ls: *mut us_listen_socket_t, cb: OnServerName) {
    // SAFETY: the caller's listener.
    unsafe {
        if (*ls).ssl_ctx.is_null() {
            return;
        }
        (*names_of(ls)).on_server_name = Some(cb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(n: usize) -> *mut SslCtx {
        n as *mut SslCtx
    }

    #[test]
    fn literal_labels_win_over_wildcards() {
        let mut tree = Tree(Vec::new());
        tree.add(b"*.example.com", ctx(1));
        tree.add(b"api.example.com", ctx(2));
        tree.add(b"*.*.test", ctx(3));
        assert_eq!(tree.find(0, b"www.example.com"), ctx(1));
        assert_eq!(tree.find(0, b"API.Example.COM"), ctx(2));
        assert_eq!(tree.find(0, b"example.com"), ptr::null_mut());
        assert_eq!(tree.find(0, b"a.b.example.com"), ptr::null_mut());
        assert_eq!(tree.find(0, b"a.b.test"), ctx(3));
        assert_eq!(tree.find(0, b"*.example.com"), ctx(1));
    }
}
