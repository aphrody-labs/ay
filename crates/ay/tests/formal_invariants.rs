//! Formal core invariants, non-vacuity witnesses, and differential oracles for `ay`.
//!
//! Follows the AlphaProof Code specification discipline (`TRUSTED.toml`):
//! every theorem-like property pairs (a) universal checks over edge/adversarial inputs
//! with (b) concrete non-vacuity witnesses and (c) independent reference oracles.

use std::collections::BTreeMap;
use std::hash::Hasher;

use ay::aphrody_core::{Arena, DotEnvLoader, GlobPattern, SlotId, Version};
use ay::ay_net::inject_seo_meta;
use ay::bun_hpack::{Decoder, Encoder, Indexing};
use ay::design_engine::{
    DesignSystemConfig, DesignSystemPreset, ThemeMode, WgpuMaterialSurfaceUniform,
    wcag_contrast_ratio,
};
use ay::pingora_ketama::{Bucket, Continuum};
use ay::rope::Rope;
use ay::tinyufo::TinyUfo;

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

/// Independent W3C WCAG 2.2 sRGB relative luminance oracle.
fn w3c_relative_luminance(rgb: [u8; 3]) -> f64 {
    let ch = |c: u8| -> f64 {
        let v = f64::from(c) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(rgb[0]) + 0.7152 * ch(rgb[1]) + 0.0722 * ch(rgb[2])
}

fn w3c_contrast_oracle(fg: [u8; 3], bg: [u8; 3]) -> f64 {
    let l1 = w3c_relative_luminance(fg);
    let l2 = w3c_relative_luminance(bg);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
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

#[test]
fn design_engine_wcag_differential_oracle_and_64b_gpu_uniform() {
    // 1. Static GPU layout invariant: 4 x vec4<f32> = exactly 64 bytes + google/zerocopy roundtrip.
    assert_eq!(std::mem::size_of::<WgpuMaterialSurfaceUniform>(), 64);
    let uniform = WgpuMaterialSurfaceUniform {
        bg_rgba: [0.05, 0.06, 0.09, 1.0],
        fg_rgba: [0.95, 0.96, 0.98, 1.0],
        border_rgba: [0.22, 0.28, 0.42, 0.75],
        corner_radius_dp: 16.0,
        elevation_dp: 4.0,
        padding_h_dp: 16.0,
        padding_v_dp: 12.0,
    };
    assert_eq!(uniform.as_bytes(), &uniform.to_le_bytes());
    assert_eq!(
        WgpuMaterialSurfaceUniform::from_bytes(uniform.as_bytes()),
        Some(uniform)
    );
    assert_eq!(WgpuMaterialSurfaceUniform::from_bytes(&[0u8; 63]), None);

    // SlotId google/zerocopy 8-byte wire/mmap roundtrip proof.
    let slot = SlotId::new(42, 7);
    assert_eq!(slot.as_bytes().len(), 8);
    assert_eq!(SlotId::from_bytes(slot.as_bytes()), Some(slot));
    assert_eq!(SlotId::from_bytes(&[0u8; 7]), None);

    // 2. Differential check of `wcag_contrast_ratio` against independent W3C WCAG 2.2 formula.
    let pairs: &[([u8; 3], [u8; 3])] = &[
        ([0, 0, 0], [255, 255, 255]),
        ([255, 255, 255], [0, 0, 0]),
        ([0x12, 0x13, 0x18], [0xf4, 0xf6, 0xfb]),
        ([0x0b, 0x10, 0x20], [0xff, 0x6b, 0x9d]),
        ([0x1e, 0x29, 0x3b], [0xe2, 0xe8, 0xf0]),
    ];
    for &(fg, bg) in pairs {
        let ours = wcag_contrast_ratio(fg, bg);
        let oracle = w3c_contrast_oracle(fg, bg);
        assert!(
            (ours - oracle).abs() < 1e-4,
            "WCAG oracle mismatch for ({fg:?}, {bg:?}): ours={ours:.6}, oracle={oracle:.6}"
        );
        // Symmetry law: contrast(a, b) == contrast(b, a).
        assert!((ours - wcag_contrast_ratio(bg, fg)).abs() < 1e-12);
    }

    // 3. Non-vacuity across all built-in presets.
    for preset in [
        DesignSystemPreset::Astryx,
        DesignSystemPreset::Material3,
        DesignSystemPreset::FluentMica,
        DesignSystemPreset::AppleLiquidGlass,
        DesignSystemPreset::ViceCitySunset,
        DesignSystemPreset::Minimal,
    ] {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let cfg = DesignSystemConfig::from_preset(preset.clone(), mode);
            let css = cfg.to_css_variables();
            assert!(
                css.contains(&cfg.css_prefix),
                "preset {preset:?} must emit CSS prefix"
            );
        }
    }
}

#[test]
fn ketama_consistent_hashing_tinyufo_bound_and_seo_xss_defense() {
    // 1. Ketama consistent hashing: determinism + minimal key movement when adding a node.
    let mk_ring = |nodes: &[&str]| {
        let buckets: Vec<Bucket> = nodes
            .iter()
            .map(|addr| Bucket::new(addr.parse().expect("valid socket addr"), 1))
            .collect();
        Continuum::new(&buckets)
    };
    let ring4 = mk_ring(&[
        "10.0.0.1:8080",
        "10.0.0.2:8080",
        "10.0.0.3:8080",
        "10.0.0.4:8080",
    ]);
    let ring5 = mk_ring(&[
        "10.0.0.1:8080",
        "10.0.0.2:8080",
        "10.0.0.3:8080",
        "10.0.0.4:8080",
        "10.0.0.5:8080",
    ]);

    let mut moved = 0usize;
    let mut dist: BTreeMap<String, usize> = BTreeMap::new();
    let total_keys = 2000usize;
    for i in 0..total_keys {
        let key = format!("session-key-{i}");
        let a = ring4.node(key.as_bytes()).expect("ring4 node");
        let a2 = ring4
            .node(key.as_bytes())
            .expect("deterministic ring4 node");
        assert_eq!(a, a2);
        let b = ring5.node(key.as_bytes()).expect("ring5 node");
        *dist.entry(b.to_string()).or_default() += 1;
        if a != b {
            moved += 1;
        }
    }
    let moved_pct = (moved as f64 / total_keys as f64) * 100.0;
    assert!(
        moved_pct < 35.0,
        "consistent hashing moved too many keys on +1 node: {moved_pct:.1}%"
    );
    assert_eq!(dist.len(), 5, "all 5 nodes must receive keys");

    // 2. TinyUFO bounded memory invariant.
    let cache = TinyUfo::new(16, 16);
    for i in 0u64..128 {
        cache.put(i, format!("val-{i}"), 1);
    }
    let mut hits = 0usize;
    for i in 0u64..128 {
        if cache.get(&i).is_some() {
            hits += 1;
        }
    }
    assert!(hits <= 32, "bounded cache retained unbounded items: {hits}");

    // 3. Security invariant: `inject_seo_meta` rejects unsafe schemes and escapes attribute breakout.
    assert!(inject_seo_meta("<html><head></head></html>", "javascript:alert(1)").is_err());
    assert!(
        inject_seo_meta(
            "<html><head></head></html>",
            "https://aphrody.com/\nX-Bad: 1"
        )
        .is_err()
    );
    let escaped = inject_seo_meta(
        "<html><head></head></html>",
        r#"https://aphrody.com/?q="><script>alert(1)</script>"#,
    )
    .expect("valid https URL should be safely escaped");
    assert!(
        !escaped.contains("<script>"),
        "raw <script> tag leaked into canonical link"
    );
    assert!(escaped.contains("&quot;&gt;&lt;script&gt;"));
}
