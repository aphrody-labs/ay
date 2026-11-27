//! HTTP/1.1 without a JavaScript engine, for the Boa build: an I/O [`reactor`] on mio, a [`client`] (TCP, Unix
//! sockets, rustls with RustCrypto, redirects, content codings) and a [`server`] transport. `fetch` and `Bun.serve`
//! on bun_js wrap them; a Rust caller drives the client with [`reactor::Reactor::block_on`].

pub mod client;
pub mod decompress;
pub mod reactor;
pub mod server;
pub mod transport;

#[cfg(test)]
mod tests;

pub use client::{Error, Head, Redirect, Request};
pub use reactor::Reactor;
pub use transport::TlsOptions;
pub use url::Url;
pub use rustls;
