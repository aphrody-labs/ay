//! What `$bindgen(file, symbol)` and `$cpp(file, symbol)` in src/js find on Boa: the functions of the crates on
//! `bun_js`, by the schema and export that name them. `None` is a native Boa has no implementation of yet.

use bun_js::{JSGlobalObject, JSValue, JsResult};

pub fn bindgen(global: &JSGlobalObject, file: &str, symbol: &str) -> Option<JsResult<JSValue>> {
    bun_runtime_js::generated_bindgen::native(global, file, symbol)
}

pub fn cpp(global: &JSGlobalObject, file: &str, symbol: &str) -> Option<JsResult<JSValue>> {
    match (file, symbol) {
        ("JSSQLStatement.cpp", "createJSSQLStatementConstructor") => {
            Some(bun_sqlite_js::create_constructor(global))
        }
        _ => bun_runtime_js::cpp(global, file, symbol),
    }
}
