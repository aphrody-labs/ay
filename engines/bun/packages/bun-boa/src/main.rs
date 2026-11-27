// The C and C++ symbols Bun's crates link against (Highway, simdutf, mimalloc, WTF), written in Rust.
use bun_sema_standalone as _;

fn main() {
    std::process::exit(bun_boa::cli::main());
}
