//! Latency histogram backed directly by verbatim upstream `oven-sh/bun` `bun_histogram`
//! (IEEE-exact HdrHistogram, zero hand-rolled bucketing math, `#![forbid(unsafe_code)]`).

pub use bun_histogram::Histogram;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LatencySummary {
    pub count: u64,
    pub min_ns: u64,
    pub mean_ns: u64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub p999_ns: u64,
    pub max_ns: u64,
}

/// Convenience wrapper over upstream `bun_histogram::Histogram` (1 ns .. `i64::MAX`, 3 significant digits).
#[derive(Clone)]
pub struct HdrHistogram {
    inner: Histogram,
}

impl Default for HdrHistogram {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl HdrHistogram {
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        // Parameters `lowest=1`, `highest=i64::MAX`, `figures=3` are compile-time valid for `bun_histogram::Histogram::new`.
        let inner = Histogram::new(1, i64::MAX as u64, 3).unwrap_or_else(|| {
            // Unreachable for (1, i64::MAX, 3), kept panic-free for `#![deny(clippy::unwrap_used)]`.
            unreachable!()
        });
        Self { inner }
    }

    #[inline]
    pub fn record_ns(&mut self, ns: u64) {
        let _ = self.inner.record(ns.min(i64::MAX as u64));
    }

    /// Access the underlying verbatim upstream `bun_histogram::Histogram`.
    #[inline]
    #[must_use]
    pub const fn as_bun_histogram(&self) -> &Histogram {
        &self.inner
    }

    #[inline]
    #[must_use]
    pub fn percentile(&self, q: f64) -> u64 {
        if self.inner.count() == 0 {
            return 0;
        }
        self.inner.value_at_percentile(q.clamp(0.0, 100.0))
    }

    #[must_use]
    pub fn summary(&self) -> LatencySummary {
        let count = self.inner.count();
        if count == 0 {
            return LatencySummary {
                count: 0,
                min_ns: 0,
                mean_ns: 0,
                p50_ns: 0,
                p95_ns: 0,
                p99_ns: 0,
                p999_ns: 0,
                max_ns: 0,
            };
        }
        LatencySummary {
            count,
            min_ns: self.inner.min(),
            mean_ns: self.inner.mean().round() as u64,
            p50_ns: self.percentile(50.0),
            p95_ns: self.percentile(95.0),
            p99_ns: self.percentile(99.0),
            p999_ns: self.percentile(99.9),
            max_ns: self.inner.max(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_histogram_tracks_exact_monotonic_percentiles() {
        let mut h = HdrHistogram::new();
        for i in 1..=1000u64 {
            h.record_ns(i * 10);
        }
        let s = h.summary();
        assert_eq!(s.count, 1000);
        assert_eq!(s.min_ns, 10);
        assert!(s.max_ns >= 10_000 && s.max_ns <= 10_015);
        assert!(s.p50_ns >= 4_950 && s.p50_ns <= 5_050);
        assert!(s.p95_ns >= 9_450 && s.p95_ns <= 9_550);
        assert!(s.p99_ns >= 9_850 && s.p99_ns <= 9_950);
        assert!(s.p50_ns <= s.p95_ns && s.p95_ns <= s.p99_ns && s.p99_ns <= s.max_ns);
    }
}
