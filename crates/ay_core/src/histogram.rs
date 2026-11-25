//! Compact logarithmic latency histogram (modeled on `oven-sh/bun`'s `bun_histogram`)
//! for real p50 / p95 / p99 / p99.9 microbenchmarking (`perf-math` discipline).

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// 64 log2 buckets x 8 linear sub-buckets = 512 fixed stack slots (4 KiB, zero heap).
const SUB_BUCKET_BITS: u32 = 3;
const SUB_BUCKETS: usize = 1 << SUB_BUCKET_BITS;
const NUM_BUCKETS: usize = 64 * SUB_BUCKETS;

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

/// Zero-heap HDR-style latency histogram covering 0 ns to `u64::MAX`.
pub struct HdrHistogram {
    counts: [u64; NUM_BUCKETS],
    total_count: u64,
    sum_ns: u128,
    min_ns: u64,
    max_ns: u64,
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
    pub const fn new() -> Self {
        Self {
            counts: [0; NUM_BUCKETS],
            total_count: 0,
            sum_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
        }
    }

    #[inline]
    const fn bucket_of(value: u64) -> usize {
        if value < SUB_BUCKETS as u64 {
            return value as usize;
        }
        let lz = value.leading_zeros();
        let msb = 63 - lz; // >= 3
        let shift = msb - SUB_BUCKET_BITS;
        let sub = ((value >> shift) as usize) & (SUB_BUCKETS - 1);
        let group = (shift + 1) as usize;
        let idx = group * SUB_BUCKETS + sub;
        if idx < NUM_BUCKETS {
            idx
        } else {
            NUM_BUCKETS - 1
        }
    }

    #[inline]
    const fn bucket_upper_bound(idx: usize) -> u64 {
        if idx < SUB_BUCKETS {
            return idx as u64;
        }
        let group = idx / SUB_BUCKETS;
        let sub = (idx % SUB_BUCKETS) as u64;
        let shift = (group - 1) as u32;
        ((SUB_BUCKETS as u64 + sub) << shift).saturating_add((1u64 << shift).saturating_sub(1))
    }

    #[inline]
    pub fn record_ns(&mut self, ns: u64) {
        let idx = Self::bucket_of(ns);
        self.counts[idx] += 1;
        self.total_count += 1;
        self.sum_ns += u128::from(ns);
        if ns < self.min_ns {
            self.min_ns = ns;
        }
        if ns > self.max_ns {
            self.max_ns = ns;
        }
    }

    /// Compute the latency value at percentile `q` in `[0.0, 100.0]`.
    #[must_use]
    pub fn percentile(&self, q: f64) -> u64 {
        if self.total_count == 0 {
            return 0;
        }
        let clamped = q.clamp(0.0, 100.0);
        let rank = ((clamped / 100.0) * (self.total_count as f64))
            .ceil()
            .max(1.0) as u64;
        let mut acc = 0u64;
        for (idx, &c) in self.counts.iter().enumerate() {
            if c == 0 {
                continue;
            }
            acc += c;
            if acc >= rank {
                return Self::bucket_upper_bound(idx).min(self.max_ns);
            }
        }
        self.max_ns
    }

    #[must_use]
    pub fn summary(&self) -> LatencySummary {
        if self.total_count == 0 {
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
            count: self.total_count,
            min_ns: self.min_ns,
            mean_ns: (self.sum_ns / u128::from(self.total_count)) as u64,
            p50_ns: self.percentile(50.0),
            p95_ns: self.percentile(95.0),
            p99_ns: self.percentile(99.0),
            p999_ns: self.percentile(99.9),
            max_ns: self.max_ns,
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
        assert_eq!(s.max_ns, 10_000);
        assert!(s.p50_ns >= 4_500 && s.p50_ns <= 6_000);
        assert!(s.p95_ns >= 9_000 && s.p95_ns <= 10_000);
        assert!(s.p99_ns >= 9_500 && s.p99_ns <= 10_000);
        assert!(s.p50_ns <= s.p95_ns && s.p95_ns <= s.p99_ns && s.p99_ns <= s.max_ns);
    }
}
