//! Byte-identical compile-time and runtime 64-bit Wyhash (`wyhash_const` == `wyhash`),
//! delegated directly to workspace `bun_wyhash` (final4 variant from `oven-sh/bun`).

/// Const-evaluable 64-bit Wyhash (exact bit-for-bit match with `bun_wyhash::hash_with_seed`).
#[inline]
#[must_use]
pub const fn wyhash_const(seed: u64, bytes: &[u8]) -> u64 {
    bun_wyhash::hash_const(seed, bytes)
}

/// Fast 64-bit Wyhash with seed 0 (const-evaluable and bit-identical to `bun_wyhash::hash`).
#[inline]
#[must_use]
pub const fn wyhash(bytes: &[u8]) -> u64 {
    bun_wyhash::hash_const(0, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE_HASH: u64 = wyhash_const(0, b"/api/v1/wayland/surfaces");

    #[test]
    fn wyhash_const_matches_runtime_across_all_length_tiers() {
        assert_eq!(ROUTE_HASH, wyhash(b"/api/v1/wayland/surfaces"));
        assert_eq!(ROUTE_HASH, bun_wyhash::hash(b"/api/v1/wayland/surfaces"));
        let inputs: &[&[u8]] = &[
            b"",
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            b"0123456789abcdef",
            b"0123456789abcdef0123456789abcdef",
            b"linux-7.3.5-cachyos-whpx-openvmm-wayland-hyprland-gamescope-wasm64",
        ];
        for &inp in inputs {
            assert_eq!(wyhash_const(0, inp), wyhash(inp));
            assert_eq!(wyhash(inp), bun_wyhash::hash(inp));
            assert_eq!(wyhash_const(42, inp), bun_wyhash::hash_with_seed(42, inp));
        }
        assert_ne!(wyhash(b"alpha"), wyhash(b"beta"));
    }
}
