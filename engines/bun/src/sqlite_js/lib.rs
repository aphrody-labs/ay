#![warn(unused_must_use)]
//! bun:sqlite on bun_js: what `$cpp("JSSQLStatement.cpp", "createJSSQLStatementConstructor")` gives
//! src/js/bun/sqlite.ts, ported from src/jsc/bindings/sqlite/JSSQLStatement.cpp onto Turso (`turso_core`), a SQLite
//! in Rust, so that the Boa build has no C.
//!
//! What Turso does not have, the functions say: `loadExtension` throws as Bun's does for a SQLite built without
//! extensions, `fcntl` answers `SQLITE_NOTFOUND` for every opcode (a VFS without file controls), `setCustomSQLite`
//! answers `false` (SQLite is linked in), update hooks throw, and an aggregate is not a window function.

// Only for its `std` feature (Cargo.toml).
use branches as _;

mod binder;
mod connection;
mod engine;
mod error;
mod function;
mod sql;
mod statement;

pub use sql::create_constructor;
