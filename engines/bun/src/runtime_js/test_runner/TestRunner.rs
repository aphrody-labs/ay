//! The bodies of `TestRunner.bind.ts`.

use super::print_diff::{DiffConfig, print_diff_main};

pub(crate) fn diff(not: bool, received: &[u8], expected: &[u8], colors: bool) -> String {
    let config = DiffConfig::default(bun_core::Output::is_ai_agent(), colors);
    let mut out = String::new();
    // Writing to a String does not fail.
    let _ = print_diff_main(not, received, expected, &mut out, &config);
    out
}
