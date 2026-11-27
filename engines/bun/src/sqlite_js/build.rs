// Build scripts run on the host before bun_* crates are compiled; std is the only option.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]
//! `error_codes.rs`: SQLite's result codes and their names, as JSSQLStatement.cpp's `createSQLiteError` reads them,
//! from the `#define`s and `FOR_EACH_SQLITE_ERROR` of src/jsc/bindings/sqlite/sqlite3_error_codes.h.

use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let header = manifest.join("../jsc/bindings/sqlite/sqlite3_error_codes.h");
    println!("cargo:rerun-if-changed={}", header.display());
    let text = std::fs::read_to_string(&header)
        .unwrap_or_else(|error| panic!("{}: {error}", header.display()));

    // `#define SQLITE_X 1 /* … */` and `#define SQLITE_X_Y (SQLITE_X | (2 << 8))`.
    let mut values = HashMap::<String, i64>::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("#define ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(' ') else {
            continue;
        };
        let value = value.split("/*").next().unwrap().trim();
        if let Some(value) = evaluate(value, &values) {
            values.insert(name.to_owned(), value);
        }
    }

    let list = text
        .split_once("#define FOR_EACH_SQLITE_ERROR(MACRO)")
        .expect("FOR_EACH_SQLITE_ERROR")
        .1;
    let mut out = String::from("/// What `FOR_EACH_SQLITE_ERROR` names, with each code.\npub const CODES: &[(i32, &str)] = &[\n");
    for name in list.split("MACRO(").skip(1) {
        let name = name.split(')').next().unwrap().trim();
        let value = values
            .get(name)
            .unwrap_or_else(|| panic!("{name} has no #define"));
        writeln!(out, "    ({value}, {name:?}),").unwrap();
    }
    out.push_str("];\n");
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("error_codes.rs");
    std::fs::write(path, out).unwrap();
}

/// A number, a name defined before, or `(A | (n << 8))`.
fn evaluate(text: &str, values: &HashMap<String, i64>) -> Option<i64> {
    let text = text.trim();
    if let Ok(number) = text.parse() {
        return Some(number);
    }
    if let Some(&value) = values.get(text) {
        return Some(value);
    }
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    if let Some((left, right)) = inner.split_once('|') {
        return Some(evaluate(left, values)? | evaluate(right, values)?);
    }
    let (left, right) = inner.split_once("<<")?;
    Some(evaluate(left, values)? << evaluate(right, values)?)
}
