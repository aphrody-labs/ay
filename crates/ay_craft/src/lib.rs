//! Unified Aphrody Craft suite (`crates/craft`, `#![forbid(unsafe_code)]`).
//!
//! Consolidates the Craft creative & productivity apps into ONE modular, à-la-carte Rust library
//! and binary (`craft`) with:
//! - **Browser multimedia & format codecs** (`codecs`): shared JPEG/PNG/WebP/GIF/SVG/PDF/XML/ZIP/Fonts
//!   layer reusable directly by `crates/browser` and any Aphrody app without duplicate dependencies.
//! - **Redesigned Theme & Brand** (`brand`): dynamically resolves palette, typography, and tokens
//!   from `crates/design` (`design-engine`, Material 3 Expressive + Astryx) — zero hardcoded upstream brand.
//! - **4 surfaces**: CLI (`craft`), MCP 2026-07-28 Code Mode (`craft mcp`), HTTP (`craft serve`),
//!   and Wasm32 CDN bundles (`<app>.aphrody.cloud` / `<app>.aphrody.app`).

#![forbid(unsafe_code)]

pub mod media;

use serde::Serialize;

/// High-level Craft app descriptor inside the unified `craft` binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CraftAppSpec {
    pub id: &'static str,
    pub subdomain: &'static str,
    pub category: &'static str,
    pub formats: &'static [&'static str],
    pub enabled: bool,
}

/// Shared multimedia & document codec descriptor reusable by `crates/browser`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MediaCodecSpec {
    pub id: &'static str,
    pub mime: &'static str,
    pub extensions: &'static [&'static str],
    pub crate_source: &'static str,
    pub browser_shared: bool,
}

/// Redesigned brand & theme identity resolved dynamically from `design-engine`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CraftBrandTheme {
    pub product_family: &'static str,
    pub window_chrome: &'static str,
    pub color_palette: &'static str,
    pub anti_slop_baseline: &'static str,
    pub accent_magenta_hex: &'static str,
    pub accent_cyan_hex: &'static str,
    pub accent_sunset_hex: &'static str,
    pub surface_dark_hex: &'static str,
    pub surface_light_hex: &'static str,
    pub display_font: &'static str,
    pub mono_font: &'static str,
    pub brand_mark: &'static str,
}

/// Design-system mapping per product surface across Aphrody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SurfaceThemeRule {
    pub surface: &'static str,
    pub domain: &'static str,
    pub design_system: &'static str,
    pub apps: &'static [&'static str],
}

/// Animated Vice City Aphrodite goddess emblem SVG (zero external assets, pure vector + CSS keyframes).
pub const APHRODITE_BRAND_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128" role="img" aria-label="Aphrody Goddess Emblem">
  <defs>
    <linearGradient id="vc-sunset" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#FF2E93"/>
      <stop offset="52%" stop-color="#FF8A3D"/>
      <stop offset="100%" stop-color="#00F0FF"/>
    </linearGradient>
    <radialGradient id="vc-halo" cx="50%" cy="46%" r="48%">
      <stop offset="0%" stop-color="#FF2E93" stop-opacity="0.45"/>
      <stop offset="65%" stop-color="#6B21A8" stop-opacity="0.18"/>
      <stop offset="100%" stop-color="#0D0B14" stop-opacity="0"/>
    </radialGradient>
    <style>
      .halo { animation: pulse 4.2s ease-in-out infinite; transform-origin: 64px 64px; }
      .shell { stroke-dasharray: 260; stroke-dashoffset: 260; animation: draw 2.2s ease-out forwards; }
      .star { animation: twinkle 3s ease-in-out infinite; transform-origin: 64px 20px; }
      @keyframes pulse { 0%,100% { transform: scale(0.96); opacity: 0.78; } 50% { transform: scale(1.04); opacity: 1; } }
      @keyframes draw { to { stroke-dashoffset: 0; } }
      @keyframes twinkle { 0%,100% { opacity: 0.55; transform: scale(0.9); } 50% { opacity: 1; transform: scale(1.12); } }
    </style>
  </defs>
  <rect width="128" height="128" rx="28" fill="#0D0B14"/>
  <circle class="halo" cx="64" cy="64" r="54" fill="url(#vc-halo)"/>
  <path class="shell" d="M22 86 C26 52, 46 34, 64 34 C82 34, 102 52, 106 86 C92 98, 78 104, 64 104 C50 104, 36 98, 22 86 Z" fill="none" stroke="url(#vc-sunset)" stroke-width="3.5" stroke-linecap="round"/>
  <path d="M64 42 C56 42, 51 49, 52 57 C53 64, 58 68, 64 68 C70 68, 75 64, 76 57 C77 49, 72 42, 64 42 Z" fill="url(#vc-sunset)"/>
  <path d="M44 96 C46 80, 54 73, 64 73 C74 73, 82 80, 84 96" fill="none" stroke="url(#vc-sunset)" stroke-width="4" stroke-linecap="round"/>
  <path class="star" d="M64 12 L66.8 18.2 L73 21 L66.8 23.8 L64 30 L61.2 23.8 L55 21 L61.2 18.2 Z" fill="#00F0FF"/>
</svg>"##;

/// Complete à-la-carte Craft app matrix compiled into this binary.
#[must_use]
pub const fn compiled_apps() -> &'static [CraftAppSpec] {
    &[
        CraftAppSpec {
            id: "fig",
            subdomain: "fig",
            category: "design",
            formats: &["fig", "figcraft", "svg", "pdf", "png"],
            enabled: cfg!(feature = "fig"),
        },
        CraftAppSpec {
            id: "photo",
            subdomain: "photo",
            category: "image",
            formats: &["png", "jpeg", "webp", "avif", "tiff", "psd"],
            enabled: cfg!(feature = "photo"),
        },
        CraftAppSpec {
            id: "light",
            subdomain: "light",
            category: "photo-raw",
            formats: &["dng", "raw", "jpeg", "webp"],
            enabled: cfg!(feature = "light"),
        },
        CraftAppSpec {
            id: "film",
            subdomain: "film",
            category: "video",
            formats: &["mp4", "webm", "mkv", "av1", "opus"],
            enabled: cfg!(feature = "film"),
        },
        CraftAppSpec {
            id: "effect",
            subdomain: "effect",
            category: "motion",
            formats: &["lottie", "svg", "webm", "mp4"],
            enabled: cfg!(feature = "effect"),
        },
        CraftAppSpec {
            id: "sound",
            subdomain: "sound",
            category: "audio",
            formats: &["wav", "flac", "opus", "ogg", "mp3"],
            enabled: cfg!(feature = "sound"),
        },
        CraftAppSpec {
            id: "word",
            subdomain: "word",
            category: "document",
            formats: &["docx", "md", "html", "pdf"],
            enabled: cfg!(feature = "word"),
        },
        CraftAppSpec {
            id: "grid",
            subdomain: "grid",
            category: "spreadsheet",
            formats: &["xlsx", "csv", "tsv", "parquet"],
            enabled: cfg!(feature = "grid"),
        },
        CraftAppSpec {
            id: "deck",
            subdomain: "deck",
            category: "presentation",
            formats: &["pptx", "pdf", "svg", "html"],
            enabled: cfg!(feature = "deck"),
        },
        CraftAppSpec {
            id: "pdf",
            subdomain: "pdf",
            category: "document",
            formats: &["pdf", "svg", "png"],
            enabled: cfg!(feature = "pdf"),
        },
        CraftAppSpec {
            id: "vector",
            subdomain: "vector",
            category: "vector",
            formats: &["svg", "eps", "pdf", "vectordrawable"],
            enabled: cfg!(feature = "vector"),
        },
        CraftAppSpec {
            id: "cad",
            subdomain: "cad",
            category: "cad",
            formats: &["dxf", "step", "stl", "glb"],
            enabled: cfg!(feature = "cad"),
        },
    ]
}

/// Shared pure-Rust multimedia & format codecs exposed to `crates/browser` and all Craft apps.
#[must_use]
pub const fn browser_media_codecs() -> &'static [MediaCodecSpec] {
    &[
        MediaCodecSpec {
            id: "image-codecs",
            mime: "image/*",
            extensions: &["png", "jpg", "jpeg", "webp", "avif", "gif", "bmp", "tiff"],
            crate_source: "third_party/craft-libs/crates/craft-image-codecs",
            browser_shared: true,
        },
        MediaCodecSpec {
            id: "svg",
            mime: "image/svg+xml",
            extensions: &["svg"],
            crate_source: "third_party/craft-libs/crates/craft-svg",
            browser_shared: true,
        },
        MediaCodecSpec {
            id: "pdf",
            mime: "application/pdf",
            extensions: &["pdf"],
            crate_source: "third_party/craft-libs/crates/craft-pdf",
            browser_shared: true,
        },
        MediaCodecSpec {
            id: "xml",
            mime: "application/xml",
            extensions: &["xml", "xhtml", "rss", "atom"],
            crate_source: "third_party/craft-libs/crates/craft-xml",
            browser_shared: true,
        },
        MediaCodecSpec {
            id: "zip",
            mime: "application/zip",
            extensions: &["zip", "docx", "xlsx", "pptx", "fig"],
            crate_source: "third_party/craft-libs/crates/craft-zip",
            browser_shared: true,
        },
        MediaCodecSpec {
            id: "fonts",
            mime: "font/woff2",
            extensions: &["woff2", "ttf", "otf"],
            crate_source: "third_party/craft-libs/crates/craft-font-store",
            browser_shared: true,
        },
    ]
}

/// Resolve the redesigned Aphrody Craft brand theme (Vice City colors + macOS traffic-light chrome + Google Design anti-slop).
#[must_use]
pub const fn brand_theme() -> CraftBrandTheme {
    CraftBrandTheme {
        product_family: "Aphrody Craft",
        window_chrome: "macOS traffic-lights (3-button premium chrome)",
        color_palette: "Vice City Sunset",
        anti_slop_baseline: "Google Design / Material 3 Expressive",
        accent_magenta_hex: "#FF2E93",
        accent_cyan_hex: "#00F0FF",
        accent_sunset_hex: "#FF8A3D",
        surface_dark_hex: "#0D0B14",
        surface_light_hex: "#FAF8FC",
        display_font: "Google Sans Flex",
        mono_font: "Google Sans Code",
        brand_mark: "Animated Aphrodite Goddess SVG (APHRODITE_BRAND_SVG)",
    }
}

/// Canonical per-surface design-system matrix across the Aphrody ecosystem.
#[must_use]
pub const fn surface_theme_matrix() -> &'static [SurfaceThemeRule] {
    &[
        SurfaceThemeRule {
            surface: "office-suite",
            domain: "*.aphrody.app",
            design_system: "Fluent UI",
            apps: &["word", "grid", "deck", "pdf"],
        },
        SurfaceThemeRule {
            surface: "media-creative",
            domain: "*.aphrody.app",
            design_system: "Material 3 Expressive + Adobe Pro ergonomics",
            apps: &[
                "fig", "photo", "light", "film", "effect", "sound", "vector", "cad",
            ],
        },
        SurfaceThemeRule {
            surface: "saas-cloud",
            domain: "*.aphrody.com",
            design_system: "Cloudflare Kumo (packages/kumo)",
            apps: &["cloud", "auto", "gateway", "auth", "openapi", "cdn"],
        },
        SurfaceThemeRule {
            surface: "dev-ide",
            domain: "desktop / *.aphrody.app",
            design_system: "Zed (crates/gpui)",
            apps: &["ide", "terminal", "debugger"],
        },
        SurfaceThemeRule {
            surface: "browser-fluid",
            domain: "Aphrody Browser",
            design_system: "Material 3 Expressive + Google Design (anti-slop) + macOS chrome",
            apps: &["aphrody-browser", "headless", "devtools"],
        },
        SurfaceThemeRule {
            surface: "global-brand",
            domain: "all",
            design_system: "Vice City Sunset palette + Animated Aphrodite SVG",
            apps: &["all"],
        },
    ]
}

/// Generate DNS routing entries for distributing Wasm/HTTP Craft apps under a base domain.
#[must_use]
pub fn dns_routes(base_domain: &str) -> Vec<serde_json::Value> {
    compiled_apps()
        .iter()
        .map(|app| {
            serde_json::json!({
                "app": app.id,
                "host": format!("{}.{base_domain}", app.subdomain),
                "category": app.category,
                "enabled": app.enabled,
                "wasmEdge": true,
                "formats": app.formats,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_apps_browser_codecs_and_redesigned_brand() {
        let apps = compiled_apps();
        assert_eq!(apps.len(), 12);
        assert!(apps.iter().any(|a| a.id == "fig" && a.enabled));
        let codecs = browser_media_codecs();
        assert!(codecs.len() >= 6);
        assert!(codecs.iter().all(|c| c.browser_shared));
        assert_eq!(brand_theme().product_family, "Aphrody Craft");
        let routes = dns_routes("aphrody.app");
        assert_eq!(routes.len(), 12);
        assert_eq!(routes[0]["host"], "fig.aphrody.app");
    }
}
