pub use aphrody_core;
pub use aphrody_craft;
pub use ay_macros::tw;
pub use ay_net;
pub use ay_ui;
pub use blitz_traits;
pub use bun_hpack;
pub use bun_wyhash;
pub use design_engine;
pub use pingora_ketama;
pub use rope;
pub use sum_tree;
pub use tinyufo;

pub fn cache_rope(cache: &tinyufo::TinyUfo<u64, String>, text: &rope::Rope) -> u64 {
    let body = text.to_string();
    let key = bun_wyhash::hash(body.as_bytes());
    let weight = body.len().clamp(1, u16::MAX as usize) as u16;
    cache.put(key, body, weight);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuses_zed_rope_bun_wyhash_tinyufo_tw_macro_and_design_engine() {
        let mut buffer = rope::Rope::new();
        buffer.push("fn main() {}\n");
        assert_eq!(buffer.len(), 13);

        let cache = tinyufo::TinyUfo::new(4096, 64);
        let key = cache_rope(&cache, &buffer);
        assert_eq!(key, bun_wyhash::hash(b"fn main() {}\n"));
        assert_eq!(cache.get(&key).as_deref(), Some("fn main() {}\n"));

        let style = tw!("flex-col gap-4 p-4 bg-surface text-fg rounded-lg");
        assert!(style.flex && style.column);
        assert_eq!(style.gap_dp, 16);
        assert_eq!(style.padding_dp, 16);
        assert_eq!(style.radius_token, "lg");

        let cfg = design_engine::DesignSystemConfig::from_preset(
            design_engine::DesignSystemPreset::Astryx,
            design_engine::ThemeMode::Dark,
        );
        assert_eq!(cfg.css_prefix, "--ax-");
    }
}
