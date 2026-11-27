pub(crate) use web_time as time;

/// 128 bits from the browser's `crypto.getRandomValues`.
pub(crate) fn seed() -> [u64; 2] {
    [getrandom::u64().unwrap_or(0), getrandom::u64().unwrap_or(0)]
}
