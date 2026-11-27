// Build scripts run on the host before bun_* crates are compiled; std is the only option.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]
//! `builtins.rs` in `OUT_DIR`: Bun's builtin modules (src/js) and builtin functions (src/js/builtins) as Boa runs
//! them, from what src/codegen/bundle-modules.ts writes to `build/debug/js-boa` (packages/bun-boa/scripts/setup.ts
//! asks it to).
//!
//! A module is `(function (){"use strict"; … })` whose free `__intrinsic__<name>` identifiers JavaScriptCore reads
//! as private names. Here each becomes a parameter of a function around it, which src/builtins.rs calls with the
//! intrinsics src/js/intrinsics.ts makes.

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

const PREFIX: &str = "__intrinsic__";

/// The names after every `__intrinsic__` that is not a member (`.__intrinsic__call` is a property).
fn free_intrinsics(source: &str) -> BTreeSet<&str> {
    let bytes = source.as_bytes();
    let mut names = BTreeSet::new();
    let mut from = 0;
    while let Some(at) = source[from..].find(PREFIX).map(|i| i + from) {
        let start = at + PREFIX.len();
        let end = source[start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .map_or(source.len(), |i| i + start);
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let member = matches!(before, Some(b'.'));
        let inside_word =
            before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
        if !member && !inside_word && end > start {
            names.insert(&source[start..end]);
        }
        from = end.max(start);
    }
    names
}

fn main() {
    let codegen = env::var("BUN_CODEGEN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../build/debug/codegen")
        });
    let dir = codegen.join("../js-boa");
    println!("cargo:rerun-if-env-changed=BUN_CODEGEN_DIR");
    println!(
        "cargo:rerun-if-changed={}",
        dir.join("registry.json").display()
    );
    let registry = fs::read_to_string(dir.join("registry.json")).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — run packages/bun-boa/scripts/setup.ts first",
            dir.join("registry.json").display()
        )
    });
    let registry: serde_json::Value =
        serde_json::from_str(&registry).expect("registry.json is JSON");

    let mut out = String::from("pub static MODULES: &[Builtin] = &[\n");
    for module in registry["modules"].as_array().expect("modules") {
        let id = module["id"].as_str().expect("id");
        let specifier = module["specifier"].as_str().expect("specifier");
        if module["native"].as_bool() == Some(true) {
            writeln!(out, "    Builtin {{ id: {id:?}, specifier: {specifier:?}, source: None, intrinsics: &[] }},").unwrap();
            continue;
        }
        let file = dir.join(id.replace(".ts", ".js"));
        println!("cargo:rerun-if-changed={}", file.display());
        let source =
            fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let names: Vec<&str> = free_intrinsics(&source).into_iter().collect();
        let params: Vec<String> = names.iter().map(|name| format!("{PREFIX}{name}")).collect();
        let wrapped = format!(
            "(function ({}) {{ return {} }})",
            params.join(", "),
            source.trim_end()
        );
        writeln!(
            out,
            "    Builtin {{ id: {id:?}, specifier: {specifier:?}, source: Some({wrapped:?}), intrinsics: &{names:?} }},"
        )
        .unwrap();
    }
    out.push_str("];\n\npub static NATIVES: &[Native] = &[\n");
    for (index, native) in registry["natives"]
        .as_array()
        .expect("natives")
        .iter()
        .enumerate()
    {
        assert_eq!(
            native["id"].as_u64(),
            Some(index as u64),
            "natives are in the order of their ids"
        );
        let kind = match native["type"].as_str() {
            Some("bind") => "Bind",
            Some("cpp") => "Cpp",
            other => panic!("native {index}: type {other:?}"),
        };
        let file = native["file"].as_str().expect("file");
        let symbol = native["symbol"].as_str().expect("symbol");
        writeln!(
            out,
            "    Native {{ kind: NativeKind::{kind}, file: {file:?}, symbol: {symbol:?} }},"
        )
        .unwrap();
    }
    out.push_str("];\n\npub static CLASSES: &[&str] = &[\n");
    for class in registry["classes"].as_array().expect("classes") {
        writeln!(out, "    {:?},", class.as_str().expect("class")).unwrap();
    }
    out.push_str("];\n\npub static FUNCTIONS: &[Function] = &[\n");
    // The builtin functions of src/js/builtins (`Buffer.from`, `toJSON`, …), each as a function of its intrinsics.
    let functions = dir.join("functions.json");
    println!("cargo:rerun-if-changed={}", functions.display());
    let functions = fs::read_to_string(&functions)
        .unwrap_or_else(|e| panic!("{}: {e} — run packages/bun-boa/scripts/setup.ts first", functions.display()));
    let functions: serde_json::Value = serde_json::from_str(&functions).expect("functions.json is JSON");
    for function in functions.as_array().expect("functions") {
        let file = function["file"].as_str().expect("file");
        let name = function["name"].as_str().expect("name");
        let source = function["source"].as_str().expect("source");
        let names: Vec<&str> = free_intrinsics(source).into_iter().collect();
        let params: Vec<String> = names.iter().map(|name| format!("{PREFIX}{name}")).collect();
        let wrapped = format!("(function ({}) {{ return {} }})", params.join(", "), source.trim_end());
        writeln!(
            out,
            "    Function {{ file: {file:?}, name: {name:?}, source: {wrapped:?}, intrinsics: &{names:?}, getter: {} }},",
            function["getter"].as_bool() == Some(true),
        )
        .unwrap();
    }
    out.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("builtins.rs"),
        out,
    )
    .unwrap();
}
