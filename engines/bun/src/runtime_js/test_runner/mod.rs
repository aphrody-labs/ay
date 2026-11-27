//! bun:test on `bun_js`: what of src/runtime/test_runner needs no engine, compiled from Bun's own files. Its
//! JavaScript is packages/bun-boa/src/js/test.

#[path = "../../runtime/test_runner/diff/printDiff.rs"]
mod print_diff;
#[allow(non_snake_case)]
#[path = "TestRunner.rs"]
pub mod TestRunner;
