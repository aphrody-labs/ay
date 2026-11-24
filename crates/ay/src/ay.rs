pub use blitz_traits;
pub use bun_hpack;
pub use bun_wyhash;
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
    fn caches_zed_rope_under_bun_wyhash_in_pingora_tinyufo() {
        let mut buffer = rope::Rope::new();
        buffer.push("fn main() {}\n");
        assert_eq!(buffer.len(), 13);

        let cache = tinyufo::TinyUfo::new(4096, 64);
        let key = cache_rope(&cache, &buffer);
        assert_eq!(key, bun_wyhash::hash(b"fn main() {}\n"));
        assert_eq!(cache.get(&key).as_deref(), Some("fn main() {}\n"));
    }
}
