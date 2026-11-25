//! Compile-time and runtime capability bitset (`u64`, zero heap allocation).

/// Compact 64-bit capability mask describing active pure-Rust Aphrody engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CapabilityFlags(u64);

impl CapabilityFlags {
    pub const ZERO_JSC: Self = Self(1 << 0);
    pub const ZERO_V8: Self = Self(1 << 1);
    pub const WHPX_VMM: Self = Self(1 << 2);
    pub const LINUX_CACHYOS: Self = Self(1 << 3);
    pub const LINUX_WASM64: Self = Self(1 << 4);
    pub const UNIXWIN: Self = Self(1 << 5);
    pub const UUTILS: Self = Self(1 << 6);
    pub const JUST: Self = Self(1 << 7);
    pub const C11_LLVM: Self = Self(1 << 8);
    pub const WASM_P2: Self = Self(1 << 9);
    pub const TAFFY: Self = Self(1 << 10);
    pub const ANYRENDER: Self = Self(1 << 11);
    pub const DIOXUS_BRIDGE: Self = Self(1 << 12);
    pub const APHRODY_COMP: Self = Self(1 << 13);
    pub const DESIGN_ENGINE: Self = Self(1 << 14);
    pub const ZED_EXT: Self = Self(1 << 15);
    pub const STANDARDS: Self = Self(1 << 16);
    pub const GIT_TOOLS: Self = Self(1 << 17);
    pub const DEPOT_TOOLS: Self = Self(1 << 18);
    pub const AGENT_PROTOCOLS: Self = Self(1 << 19);
    pub const BROWSER_ENGINE: Self = Self(1 << 20);
    pub const BROWSER_GPUI: Self = Self(1 << 21);
    pub const TREE_SITTER_IN_TREE: Self = Self(1 << 22);
    pub const ASTERINAS_FRAMEKERNEL: Self = Self(1 << 23);
    pub const KIRI_PROTO: Self = Self(1 << 24);
    pub const GGML_AI: Self = Self(1 << 25);
    pub const STEAM_CLIENT: Self = Self(1 << 26);
    pub const ANDROID_17: Self = Self(1 << 27);

    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[inline]
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[inline]
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    #[must_use]
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bitset_is_const_and_exact() {
        let mask = CapabilityFlags::ZERO_JSC
            .union(CapabilityFlags::ZERO_V8)
            .union(CapabilityFlags::WASM_P2);
        assert!(mask.contains(CapabilityFlags::ZERO_JSC));
        assert!(mask.contains(CapabilityFlags::WASM_P2));
        assert!(!mask.contains(CapabilityFlags::WHPX_VMM));
        assert_eq!(mask.count(), 3);
    }
}
