// Reexports `std::time` for all other platforms. This could cause panics on
// platforms that don't support `Instant::now()`.
pub(crate) use std::time;

/// 128 bits from the keys std draws from the OS for `HashMap` (fresh keys per `RandomState`).
pub(crate) fn seed() -> [u64; 2] {
    use std::hash::{BuildHasher, RandomState};
    let keys = RandomState::new();
    [keys.hash_one(0u8), keys.hash_one(1u8)]
}
