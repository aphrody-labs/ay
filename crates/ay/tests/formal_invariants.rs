//! Formal core invariants, non-vacuity witnesses, and differential oracles for `ay`.
//!
//! Follows the AlphaProof Code specification discipline (`TRUSTED.toml`):
//! every theorem-like property pairs (a) universal checks over edge/adversarial inputs
//! with (b) concrete non-vacuity witnesses and (c) independent reference oracles.

use std::hash::Hasher;

use ay::aphrody_core::{Arena, DotEnvLoader, GlobPattern, SlotId, Version};
use ay::bun_hpack::{Decoder, Encoder, Indexing};
use ay::rope::Rope;

/// Deterministic splitmix64 PRNG so every differential trace is 100 % reproducible.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() as usize) % bound
    }
}

#[test]
fn wyhash_determinism_boundary_lengths_and_avalanche_criterion() {
    // 1. Determinism and streaming vs one-shot equivalence over all boundary lengths 0..=256.
    for len in 0..=256usize {
        let data: Vec<u8> = (0..len).map(|i| ((i * 37 + 13) & 0xff) as u8).collect();
        let h_once = ay::bun_wyhash::hash(&data);
        let h_twice = ay::bun_wyhash::hash(&data);
        assert_eq!(h_once, h_twice, "wyhash must be deterministic at len={len}");

        let mut hasher = ay::bun_wyhash::Wyhash::default();
        hasher.write(&data);
        assert_eq!(
            hasher.finish(),
            h_once,
            "streaming Wyhash must equal one-shot hash at len={len}"
        );
    }

    // 2. Strict Avalanche Criterion (SAC): single-bit flip in 64-byte inputs flips ~50 % of output bits.
    let mut rng = SplitMix64(0x5eed_cafe_beef_0001);
    let mut total_flips = 0u64;
    let trials = 512u64;
    for _ in 0..trials {
        let mut buf = [0u8; 64];
        for chunk in buf.as_chunks_mut::<8>().0 {
            *chunk = rng.next_u64().to_le_bytes();
        }
        let h1 = ay::bun_wyhash::hash(&buf);
        let byte_idx = rng.next_usize(buf.len());
        let bit_idx = rng.next_usize(8);
        buf[byte_idx] ^= 1u8 << bit_idx;
        let h2 = ay::bun_wyhash::hash(&buf);
        assert_ne!(h1, h2, "single-bit input flip must not collide");
        total_flips += u64::from((h1 ^ h2).count_ones());
    }
    let mean_bits = total_flips as f64 / trials as f64;
    assert!(
        (28.0..=36.0).contains(&mean_bits),
        "expected mean avalanche ~32.0 bits (50 %), observed {mean_bits:.2}"
    );
}

#[test]
fn hpack_rfc7541_roundtrip_and_adversarial_truncation() {
    let mut encoder = Encoder::new();
    let mut decoder = Decoder::new();
    let mut wire = Vec::new();
    let mut scratch = Vec::new();

    type HeaderEntry<'a> = (&'a [u8], &'a [u8], Indexing);
    let header_sets: &[&[HeaderEntry<'_>]] = &[
        &[
            (b":method", b"GET", Indexing::Allowed),
            (b":scheme", b"https", Indexing::Allowed),
            (b":path", b"/live", Indexing::Allowed),
            (b":authority", b"aphrody.com", Indexing::Allowed),
            (
                b"authorization",
                b"Bearer secret-never-index",
                Indexing::Never,
            ),
        ],
        &[
            (b":method", b"POST", Indexing::Allowed),
            (b":path", b"/api/agents/broadcast", Indexing::Allowed),
            (b"content-type", b"application/json", Indexing::Allowed),
            (
                b"x-aphrody-trace",
                b"0193c9d0-7a1f-7000-8000-000000000001",
                Indexing::Without,
            ),
        ],
        &[
            (b":status", b"200", Indexing::Allowed),
            (
                b"cross-origin-opener-policy",
                b"same-origin",
                Indexing::Allowed,
            ),
            (
                b"cross-origin-embedder-policy",
                b"credentialless",
                Indexing::Allowed,
            ),
            (
                b"cross-origin-resource-policy",
                b"cross-origin",
                Indexing::Allowed,
            ),
        ],
    ];

    for (round, headers) in header_sets.iter().enumerate() {
        wire.clear();
        {
            let mut block = encoder.block(&mut wire);
            for &(name, value, indexing) in *headers {
                block.field(name, value, indexing);
            }
        }
        scratch.clear();
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut fields = decoder.decode(&wire, &mut scratch);
        while let Some(f) = fields.next().expect("valid HPACK block must decode") {
            pairs.push((f.name.to_vec(), f.value.to_vec()));
        }
        let expected: Vec<(Vec<u8>, Vec<u8>)> = headers
            .iter()
            .map(|&(n, v, _)| (n.to_vec(), v.to_vec()))
            .collect();
        assert_eq!(pairs, expected, "HPACK roundtrip mismatch on round {round}");
    }

    // Adversarially truncated wire block must yield an error, never panic.
    let truncated = &[0x40u8, 0x7f];
    scratch.clear();
    let mut bad_fields = decoder.decode(truncated, &mut scratch);
    assert!(
        bad_fields.next().is_err(),
        "truncated HPACK header block must fail closed"
    );
}

#[test]
fn rope_differential_model_check_against_std_string_over_500_utf8_edits() {
    let alphabet = [
        "a",
        "bc",
        "\n",
        "é",
        "à",
        "œ",
        "λ",
        "日",
        "🚀",
        "fn main() {}\n",
        "  // comment\n",
    ];
    let mut rng = SplitMix64(0x1234_5678_9abc_def0);
    let mut rope = Rope::new();
    let mut model = String::new();

    for step in 0..500usize {
        let op = rng.next_usize(3);
        let boundaries: Vec<usize> = model
            .char_indices()
            .map(|(idx, _)| idx)
            .chain(std::iter::once(model.len()))
            .collect();
        if op == 0 || model.is_empty() {
            let pos = boundaries[rng.next_usize(boundaries.len())];
            let frag = alphabet[rng.next_usize(alphabet.len())];
            rope.replace(pos..pos, frag);
            model.insert_str(pos, frag);
        } else if op == 1 {
            let a = boundaries[rng.next_usize(boundaries.len())];
            let b = boundaries[rng.next_usize(boundaries.len())];
            let (start, end) = if a <= b { (a, b) } else { (b, a) };
            let frag = if rng.next_usize(2) == 0 {
                ""
            } else {
                alphabet[rng.next_usize(alphabet.len())]
            };
            rope.replace(start..end, frag);
            model.replace_range(start..end, frag);
        } else {
            let pos = boundaries[rng.next_usize(boundaries.len())];
            let pt = rope.offset_to_point(pos);
            assert_eq!(
                rope.point_to_offset(pt),
                pos,
                "offset_to_point -> point_to_offset violated at step {step}"
            );
        }

        assert_eq!(rope.len(), model.len(), "len mismatch at step {step}");
        assert_eq!(rope.to_string(), model, "content mismatch at step {step}");
    }
    // Non-vacuity witness: trace exercised non-empty multi-byte state.
    assert!(!model.is_empty());
}

#[test]
fn arena_aba_generation_safety_and_semver_total_order() {
    // 1. Arena ABA generation rejection invariant.
    let mut arena = Arena::with_capacity(8);
    let stale_a = arena.insert("first-generation").expect("insert a");
    let live_b = arena.insert("live-neighbor").expect("insert b");
    assert_eq!(arena.remove(stale_a), Some("first-generation"));
    assert_eq!(arena.get(stale_a), None, "removed slot must return None");

    let reused_c = arena.insert("second-generation").expect("reuse slot");
    assert_eq!(
        stale_a.index(),
        reused_c.index(),
        "non-vacuity: free list must reuse the freed index"
    );
    assert_ne!(stale_a.generation(), reused_c.generation());
    assert_eq!(
        arena.get(stale_a),
        None,
        "stale SlotId MUST NOT alias reused slot (ABA defense)"
    );
    assert_eq!(arena.get(reused_c), Some(&"second-generation"));
    assert_eq!(arena.get(live_b), Some(&"live-neighbor"));
    assert_eq!(
        arena.remove(stale_a),
        None,
        "stale remove must not evict live occupant"
    );
    assert_eq!(arena.get(reused_c), Some(&"second-generation"));
    assert_eq!(arena.get(SlotId::new(999, 1)), None);

    // 2. SemVer total order axioms (reflexivity, antisymmetry, transitivity).
    let raw = [
        "0.0.1",
        "0.1.0-alpha.1",
        "0.1.0-beta.1",
        "0.1.0",
        "1.0.0-rc.1",
        "1.0.0",
        "1.2.3",
        "2.0.0",
        "2026.11.0",
    ];
    let parsed: Vec<Version> = raw
        .iter()
        .map(|s| Version::parse(s).expect("valid semver"))
        .collect();
    for (i, a) in parsed.iter().enumerate() {
        assert_eq!(a.cmp(a), std::cmp::Ordering::Equal, "reflexivity");
        for (j, b) in parsed.iter().enumerate() {
            assert_eq!(a.cmp(b), b.cmp(a).reverse(), "antisymmetry");
            if i < j {
                assert!(a < b, "expected {a} < {b}");
            }
            for c in &parsed {
                if a <= b && b <= c {
                    assert!(a <= c, "transitivity failed for {a}, {b}, {c}");
                }
            }
        }
    }

    // 3. DotEnv roundtrip & Glob soundness witnesses.
    let mut env_loader = DotEnvLoader::new();
    let count = env_loader.load_str("APP_NAME=\"aphrody\"\nPORT=3000\n# comment\nEMPTY=\n");
    assert_eq!(count, 3);
    assert_eq!(env_loader.get("APP_NAME"), Some("aphrody"));
    assert_eq!(env_loader.get("PORT"), Some("3000"));
    assert_eq!(env_loader.get("EMPTY"), Some(""));

    let glob = GlobPattern::compile("crates/**/src/*.rs");
    assert!(glob.matches("crates/ay_core/src/arena.rs"));
    assert!(!glob.matches("crates/ay_core/Cargo.toml"));
}
