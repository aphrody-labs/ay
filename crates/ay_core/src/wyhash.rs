//! Byte-identical compile-time and runtime 64-bit Wyhash (`wyhash_const` == `wyhash`),
//! modeled on `oven-sh/bun`'s `bun_wyhash` (32 dependents across router/bundler/collections).

const P0: u64 = 0xa076_1d64_78bd_642f;
const P1: u64 = 0xe703_7ed1_a0b4_28db;
const P2: u64 = 0x8ebc_6af0_9c88_c6e3;
const P3: u64 = 0x5899_65cc_7537_4cc3;

#[inline]
const fn wymum(a: u64, b: u64) -> u64 {
    let r = (a as u128).wrapping_mul(b as u128);
    (r as u64) ^ ((r >> 64) as u64)
}

#[inline]
const fn read_u64_le(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        bytes[off],
        bytes[off + 1],
        bytes[off + 2],
        bytes[off + 3],
        bytes[off + 4],
        bytes[off + 5],
        bytes[off + 6],
        bytes[off + 7],
    ])
}

#[inline]
const fn read_u32_le(bytes: &[u8], off: usize) -> u64 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]) as u64
}

/// Const-evaluable 64-bit Wyhash (exact bit-for-bit match with runtime [`wyhash`]).
#[inline]
#[must_use]
pub const fn wyhash_const(seed: u64, bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut s = seed ^ wymum(seed ^ P0, P1);
    let (a, b): (u64, u64);

    if len <= 16 {
        if len >= 4 {
            let half = (len >> 3) << 2;
            a = (read_u32_le(bytes, 0) << 32) | read_u32_le(bytes, half);
            b = (read_u32_le(bytes, len - 4) << 32) | read_u32_le(bytes, len - 4 - half);
        } else if len > 0 {
            a = ((bytes[0] as u64) << 16)
                | ((bytes[len >> 1] as u64) << 8)
                | (bytes[len - 1] as u64);
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut i = len;
        let mut off = 0usize;
        if i > 48 {
            let mut see1 = s;
            let mut see2 = s;
            while i > 48 {
                s = wymum(
                    read_u64_le(bytes, off) ^ P1,
                    read_u64_le(bytes, off + 8) ^ s,
                );
                see1 = wymum(
                    read_u64_le(bytes, off + 16) ^ P2,
                    read_u64_le(bytes, off + 24) ^ see1,
                );
                see2 = wymum(
                    read_u64_le(bytes, off + 32) ^ P3,
                    read_u64_le(bytes, off + 40) ^ see2,
                );
                off += 48;
                i -= 48;
            }
            s ^= see1 ^ see2;
        }
        while i > 16 {
            s = wymum(
                read_u64_le(bytes, off) ^ P1,
                read_u64_le(bytes, off + 8) ^ s,
            );
            off += 16;
            i -= 16;
        }
        a = read_u64_le(bytes, len - 16);
        b = read_u64_le(bytes, len - 8);
    }

    wymum(P1 ^ (len as u64), wymum(a ^ P1, b ^ s))
}

/// Fast runtime 64-bit Wyhash with seed 0.
#[inline]
#[must_use]
pub const fn wyhash(bytes: &[u8]) -> u64 {
    wyhash_const(0, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE_HASH: u64 = wyhash_const(0, b"/api/v1/wayland/surfaces");

    #[test]
    fn wyhash_const_matches_runtime_across_all_length_tiers() {
        assert_eq!(ROUTE_HASH, wyhash(b"/api/v1/wayland/surfaces"));
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
            assert_eq!(wyhash_const(42, inp), wyhash_const(42, inp));
        }
        assert_ne!(wyhash(b"alpha"), wyhash(b"beta"));
    }
}
