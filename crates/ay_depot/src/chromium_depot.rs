//! Zero-copy CEF / Chromium Stable (Dawn WebGPU, Skia, Blink, V8) prebuilt artifact depot
//! & polyglot `#[repr(C)]` shared-surface descriptor (`#![forbid(unsafe_code)]`, `zerocopy 0.8.66`)
//! consumable bit-for-bit across Rust, Zig, Python, and Bun without vendoring or compiling Chromium.

use serde::Serialize;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const CHROMIUM_SURFACE_MAGIC: [u8; 4] = *b"AYCR";

/// Zero-copy 32-byte polyglot ABI header bridging CEF/Dawn/Skia/V8 shared GPU textures & buffers
/// across Rust, Zig, Python, and Bun (`#[repr(C)]`, zero padding).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout)]
pub struct ChromiumSurfaceDesc {
    pub magic: [u8; 4],
    pub abi_version: u32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub flags: u32,
    pub shared_handle: u64,
}

impl ChromiumSurfaceDesc {
    /// Flag bit: Dawn WebGPU shared texture backed by Vulkan DMA-BUF or Win32 WDDM handle.
    pub const FLAG_DAWN_WEBGPU: u32 = 1 << 0;
    /// Flag bit: Skia Ganesh/Graphite GPU surface.
    pub const FLAG_SKIA_GRAPHITE: u32 = 1 << 1;
    /// Flag bit: Blink off-screen compositor frame.
    pub const FLAG_BLINK_OSR: u32 = 1 << 2;
    /// Flag bit: V8 external ArrayBuffer zero-copy backing store.
    pub const FLAG_V8_EXTERNAL_BUF: u32 = 1 << 3;

    #[must_use]
    pub const fn new(width: u32, height: u32, flags: u32, shared_handle: u64) -> Self {
        Self {
            magic: CHROMIUM_SURFACE_MAGIC,
            abi_version: 1,
            width,
            height,
            pixel_format: 0x0000_0057, // BGRA8Unorm
            flags,
            shared_handle,
        }
    }

    #[must_use]
    pub fn from_wire_bytes(bytes: &[u8]) -> Option<Self> {
        let (desc, _) = Self::read_from_prefix(bytes).ok()?;
        (desc.magic == CHROMIUM_SURFACE_MAGIC && desc.abi_version == 1).then_some(desc)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChromiumSubsystemSpec {
    pub id: &'static str,
    pub upstream: &'static str,
    pub prebuilt_source: &'static str,
    pub consumers: &'static [&'static str],
    pub role: &'static str,
}

/// Canonical prebuilt-binary depot catalog for Chromium stable subsystems (zero source vendoring).
pub const CHROMIUM_DEPOT_SUBSYSTEMS: &[ChromiumSubsystemSpec] = &[
    ChromiumSubsystemSpec {
        id: "cef-stable",
        upstream: "https://github.com/chromiumembedded/cef",
        prebuilt_source: "https://cef-builds.spotifycdn.com/index.json",
        consumers: &["rust", "zig", "bun"],
        role: "Prebuilt Chromium Embedded Framework (minimal/standard) with OSR + DevTools CDP",
    },
    ChromiumSubsystemSpec {
        id: "dawn-webgpu",
        upstream: "https://dawn.googlesource.com/dawn",
        prebuilt_source: "https://github.com/google/dawn/releases",
        consumers: &["rust", "zig", "bun", "python"],
        role: "WebGPU native C ABI (`webgpu.h`) + Tint WGSL compiler sharing DMA-BUF/WDDM handles with wgpu v30",
    },
    ChromiumSubsystemSpec {
        id: "skia-graphite",
        upstream: "https://skia.googlesource.com/skia",
        prebuilt_source: "https://github.com/rust-skia/skia-binaries/releases",
        consumers: &["rust", "zig"],
        role: "Prebuilt Skia 2D/Graphite GPU vector/text rasterizer",
    },
    ChromiumSubsystemSpec {
        id: "v8-prebuilt",
        upstream: "https://chromium.googlesource.com/v8/v8",
        prebuilt_source: "https://github.com/denoland/rusty_v8/releases",
        consumers: &["zig", "python"],
        role: "Isolated external V8/workerd artifact reference (never linked into zero-JSC Rust crates)",
    },
    ChromiumSubsystemSpec {
        id: "blink-cdp",
        upstream: "https://chromium.googlesource.com/chromium/src/+/main/third_party/blink",
        prebuilt_source: "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json",
        consumers: &["rust", "bun", "python"],
        role: "Chrome-for-Testing headless Blink/CDP binary for visual verification & Lighthouse",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_surface_desc_roundtrips_32_bytes_zerocopy() {
        let d = ChromiumSurfaceDesc::new(
            1920,
            1080,
            ChromiumSurfaceDesc::FLAG_DAWN_WEBGPU | ChromiumSurfaceDesc::FLAG_SKIA_GRAPHITE,
            0xDEAD_BEEF_CAFE_0001,
        );
        let bytes = d.as_bytes();
        assert_eq!(bytes.len(), 32);
        let decoded = ChromiumSurfaceDesc::from_wire_bytes(bytes).expect("valid wire header");
        assert_eq!(decoded, d);
        assert_eq!(CHROMIUM_DEPOT_SUBSYSTEMS.len(), 5);
    }
}
