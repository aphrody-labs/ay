//! Bun on Boa: Bun's own transpiler (`bun_js_parser` + `bun_js_printer`) in front of a pure-Rust JavaScript
//! engine, with no JavaScriptCore and no C++.

// Weak symbols in `yarr`.
#![feature(linkage)]

#[cfg(feature = "build")]
pub mod build;
pub mod builtins;
pub mod cli;
pub mod fmt;
#[cfg(feature = "install")]
pub mod install;
pub mod lint;
pub mod loader;
pub mod natives;
pub mod runtime;
pub mod scripts;
pub mod source_maps;
pub mod test;
pub mod transpile;
pub mod worker;
pub mod wtf;
pub mod yarr;
