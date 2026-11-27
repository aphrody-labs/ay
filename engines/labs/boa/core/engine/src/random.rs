//! The engine's pseudo-random numbers (`Math.random`): xorshift128+, as in V8, with one state per thread seeded
//! from the platform. Not for cryptography.

use std::cell::Cell;

thread_local! {
    static STATE: Cell<[u64; 2]> = Cell::new(match crate::sys::seed() {
        [0, 0] => [0x9E37_79B9_7F4A_7C15, 0xBF58_476D_1CE4_E5B9],
        s => s,
    });
}

/// The next 64 random bits.
pub(crate) fn next_u64() -> u64 {
    STATE.with(|state| {
        let [mut s1, s0] = state.get();
        s1 ^= s1 << 23;
        s1 ^= s1 >> 17;
        s1 ^= s0;
        s1 ^= s0 >> 26;
        state.set([s0, s1]);
        s0.wrapping_add(s1)
    })
}

/// A uniform `f64` in `[0, 1)`, from the top 53 bits.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn next_f64() -> f64 {
    (next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn in_unit_interval_and_varied() {
        let values: Vec<f64> = (0..1000).map(|_| super::next_f64()).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
        let mean = values.iter().sum::<f64>() / 1000.0;
        assert!((0.4..0.6).contains(&mean), "mean {mean}");
        assert!(values.windows(2).all(|w| w[0].to_bits() != w[1].to_bits()));
    }
}
