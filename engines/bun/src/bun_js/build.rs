// Build scripts run on the host before bun_* crates are compiled; std is the only option.
//! - `error_codes.rs`: `ErrorCode`'s constants, from src/jsc/bindings/ErrorCode.ts, the list `bun_jsc`'s come from.
//! - `BUN_CODEGEN_DIR` for tests/bindgen.rs, which `include!`s what src/codegen/bindgen-js.ts writes from
//!   tests/fixture.bind.ts (packages/bun-boa/scripts/setup.ts runs it).

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let codegen_dir = env::var("BUN_CODEGEN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../../build/debug/codegen"));
    println!("cargo:rustc-env=BUN_CODEGEN_DIR={}", codegen_dir.display());
    println!("cargo:rerun-if-env-changed=BUN_CODEGEN_DIR");
    println!(
        "cargo:rerun-if-changed={}",
        codegen_dir.join("generated_bun_js_bun_js.rs").display()
    );

    let list = manifest.join("../jsc/bindings/ErrorCode.ts");
    println!("cargo:rerun-if-changed={}", list.display());
    let source = fs::read_to_string(&list).unwrap_or_else(|e| panic!("{}: {e}", list.display()));
    let mut out = String::from("#[allow(non_upper_case_globals)]\nimpl ErrorCode {\n");
    // Each entry is one line: `["CODE", Constructor]` or `["CODE", Constructor, "name", …]`.
    let entries: Vec<Vec<&str>> = source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("[\""))
        .map(|line| {
            line.trim_start_matches('[')
                .split(']')
                .next()
                .unwrap()
                .split(',')
                .map(|field| field.trim().trim_matches('"'))
                .collect()
        })
        .collect();
    let codes: std::collections::HashSet<&str> = entries.iter().map(|fields| fields[0]).collect();
    let kind_of = |code: &str, constructor: &str| match constructor {
        "Error" | "TypeError" | "RangeError" | "SyntaxError" | "URIError" => constructor.to_owned(),
        _ => panic!(
            "{}: {code} is a {constructor}, which ErrorKind has no variant for",
            list.display()
        ),
    };
    let value = |code: &str, kind: &str, name: Option<&str>| {
        let name = name.map_or("None".to_owned(), |name| format!("Some({name:?})"));
        format!("ErrorCode {{ code: {code:?}, kind: ErrorKind::{kind}, name: {name} }}")
    };
    // As src/codegen/generate-node-errors.ts, which writes `bun_jsc`'s. `ALL` is in its order, which is what
    // `$makeErrorWithCode(<number>, …)` in src/js numbers a code by.
    let mut all = Vec::new();
    for fields in &entries {
        let (code, constructor) = (fields[0], fields[1]);
        let name = fields
            .get(2)
            .copied()
            .filter(|name| !name.is_empty() && *name != "undefined");
        let kind = kind_of(code, constructor);
        all.push(code.to_owned());
        writeln!(
            out,
            "    pub const {code}: ErrorCode = {};",
            value(code, &kind, name)
        )
        .unwrap();
        // `INVALID_ARG_TYPE` for `ERR_INVALID_ARG_TYPE`, unless a code is named that already (`MODULE_NOT_FOUND`).
        let short = code
            .strip_prefix("ERR_")
            .filter(|short| !codes.contains(short));
        if let Some(short) = short {
            writeln!(out, "    pub const {short}: ErrorCode = ErrorCode::{code};").unwrap();
        }
        // `["ERR_INVALID_STATE", Error, undefined, TypeError]`: `INVALID_STATE_TypeError` too, named for its class.
        for other in fields.iter().skip(3).filter(|other| **other != "undefined") {
            let kind = kind_of(code, other);
            let variant = format!("{}_{other}", short.unwrap_or(code));
            all.push(variant.clone());
            writeln!(
                out,
                "    pub const {variant}: ErrorCode = {};",
                value(code, &kind, None)
            )
            .unwrap();
        }
    }
    assert!(!entries.is_empty(), "{}: no error codes", list.display());
    writeln!(
        out,
        "    pub const ALL: &[ErrorCode] = &[{}];",
        all.iter()
            .map(|c| format!("ErrorCode::{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
    .unwrap();
    out.push_str("}\n");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("error_codes.rs"), out).unwrap();
}
