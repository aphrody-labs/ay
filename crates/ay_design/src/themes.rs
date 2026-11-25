//! Selectable multi-OS desktop design presets, optical material shaders, and window tiling
//! engines (`Material3Googlebook`, `AppleLiquidGlass`, `FluentMica`, `Omarchy`).
//!
//! Declarative sources embedded at compile time (compatible with `wasm32-unknown-unknown`):
//! - `configs/design/desktop-systems.json`
//! - `configs/design/material3.json` (via `m3_bundle()`)
//! - `configs/themes/*/theme.toml` (all 28 Omarchy palettes)
//! - `configs/android/android.toml` and `configs/android/android-17.toml`

use std::{collections::BTreeMap, fmt::Write, sync::OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ir::{
        DesignA11y, DesignDocument, DesignNode, DesignNodeKind, DesignSourceKind, DesignStyle,
        SurfaceMode,
    },
    material3::{ResolvedColorScheme, ThemeMode, best_wcag_foreground, m3_bundle, parse_hex_rgb},
};

/// Declarative multi-OS desktop systems specification.
pub const DESKTOP_SYSTEMS_JSON: &str = include_str!("../data/design/desktop-systems.json");

const ANDROID_BASE_TOML: &str = include_str!("../data/android/android.toml");
const ANDROID_DESKTOP_TOML: &str = include_str!("../data/android/android-17.toml");

const OMARCHY_THEME_TOMLS: &[(&str, &str)] = &[
    ("aphrody", include_str!("../data/themes/aphrody/theme.toml")),
    ("arch", include_str!("../data/themes/arch/theme.toml")),
    ("cachyos", include_str!("../data/themes/cachyos/theme.toml")),
    (
        "catppuccin",
        include_str!("../data/themes/catppuccin/theme.toml"),
    ),
    (
        "catppuccin-latte",
        include_str!("../data/themes/catppuccin-latte/theme.toml"),
    ),
    (
        "ethereal",
        include_str!("../data/themes/ethereal/theme.toml"),
    ),
    (
        "everforest",
        include_str!("../data/themes/everforest/theme.toml"),
    ),
    (
        "flexoki-light",
        include_str!("../data/themes/flexoki-light/theme.toml"),
    ),
    ("gruvbox", include_str!("../data/themes/gruvbox/theme.toml")),
    (
        "hackerman",
        include_str!("../data/themes/hackerman/theme.toml"),
    ),
    (
        "jade-horse",
        include_str!("../data/themes/jade-horse/theme.toml"),
    ),
    (
        "kanagawa",
        include_str!("../data/themes/kanagawa/theme.toml"),
    ),
    (
        "last-horizon",
        include_str!("../data/themes/last-horizon/theme.toml"),
    ),
    ("lumon", include_str!("../data/themes/lumon/theme.toml")),
    ("lupine", include_str!("../data/themes/lupine/theme.toml")),
    (
        "matte-black",
        include_str!("../data/themes/matte-black/theme.toml"),
    ),
    ("miasma", include_str!("../data/themes/miasma/theme.toml")),
    ("nord", include_str!("../data/themes/nord/theme.toml")),
    (
        "osaka-jade",
        include_str!("../data/themes/osaka-jade/theme.toml"),
    ),
    (
        "retro-82",
        include_str!("../data/themes/retro-82/theme.toml"),
    ),
    (
        "ristretto",
        include_str!("../data/themes/ristretto/theme.toml"),
    ),
    (
        "rose-pine",
        include_str!("../data/themes/rose-pine/theme.toml"),
    ),
    (
        "solitude",
        include_str!("../data/themes/solitude/theme.toml"),
    ),
    (
        "starship",
        include_str!("../data/themes/starship/theme.toml"),
    ),
    (
        "synthwave84",
        include_str!("../data/themes/synthwave84/theme.toml"),
    ),
    (
        "tokyo-night",
        include_str!("../data/themes/tokyo-night/theme.toml"),
    ),
    (
        "vantablack",
        include_str!("../data/themes/vantablack/theme.toml"),
    ),
    ("white", include_str!("../data/themes/white/theme.toml")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopThemeKind {
    Material3Googlebook,
    AppleLiquidGlass,
    FluentMica,
    Omarchy,
    /// `aphrody-comp` signature Vice City Sunset theme (neon magenta/cyan/amber on midnight ocean).
    ViceCitySunset,
}

impl DesktopThemeKind {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "material3_googlebook" | "googlebook_m3" | "googlebook" | "m3" | "material3" => {
                Ok(Self::Material3Googlebook)
            }
            "apple_liquid_glass" | "apple" | "liquid_glass" | "macos" | "golden_gate"
            | "macbook_neo" => Ok(Self::AppleLiquidGlass),
            "fluent_mica" | "fluent" | "mica" | "acrylic" | "windows" | "winui" => {
                Ok(Self::FluentMica)
            }
            "omarchy" | "omarchy_hyprland" | "hyprland" => Ok(Self::Omarchy),
            "vice_city_sunset" | "vice_city" | "vice" | "gta6" | "aphrody_comp" => {
                Ok(Self::ViceCitySunset)
            }
            other => Err(format!(
                "unsupported desktop theme '{other}'; expected: material3_googlebook, apple_liquid_glass, fluent_mica, omarchy, vice_city_sunset"
            )),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Material3Googlebook => "material3_googlebook",
            Self::AppleLiquidGlass => "apple_liquid_glass",
            Self::FluentMica => "fluent_mica",
            Self::Omarchy => "omarchy",
            Self::ViceCitySunset => "vice_city_sunset",
        }
    }

    #[must_use]
    pub const fn default_tiling(self) -> WindowTilingEngine {
        match self {
            Self::Material3Googlebook => WindowTilingEngine::AndroidFreeform,
            Self::AppleLiquidGlass => WindowTilingEngine::NativeGridSnap,
            Self::FluentMica => WindowTilingEngine::FluentSnapLayout,
            Self::Omarchy | Self::ViceCitySunset => WindowTilingEngine::HyprMacBsp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowTilingEngine {
    NativeGridSnap,
    HyprMacBsp,
    AndroidFreeform,
    FluentSnapLayout,
}

impl WindowTilingEngine {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "native_grid_snap" | "native_grid" | "native" | "macos_grid" | "grid" => {
                Ok(Self::NativeGridSnap)
            }
            "hyprmac_bsp" | "hyprmac" | "hyprmac_dwindle" | "bsp" | "dwindle" => {
                Ok(Self::HyprMacBsp)
            }
            "android_freeform" | "android" | "freeform" => Ok(Self::AndroidFreeform),
            "fluent_snap_layout" | "fluent_snap" | "windows_snap" | "snap" => {
                Ok(Self::FluentSnapLayout)
            }
            other => Err(format!(
                "unsupported tiling engine '{other}'; expected: native_grid_snap, hyprmac_bsp, android_freeform, fluent_snap_layout"
            )),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeGridSnap => "native_grid_snap",
            Self::HyprMacBsp => "hyprmac_bsp",
            Self::AndroidFreeform => "android_freeform",
            Self::FluentSnapLayout => "fluent_snap_layout",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopColorMode {
    Light,
    #[default]
    Dark,
    NeuralObsidian,
    OledPitch,
    HighContrast,
}

impl DesktopColorMode {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "light" => Self::Light,
            "neural_obsidian" | "obsidian" => Self::NeuralObsidian,
            "oled_pitch" | "oled" => Self::OledPitch,
            "high_contrast" | "hc" | "contrast" => Self::HighContrast,
            _ => Self::Dark,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::NeuralObsidian => "neural_obsidian",
            Self::OledPitch => "oled_pitch",
            Self::HighContrast => "high_contrast",
        }
    }

    #[must_use]
    pub const fn to_theme_mode(self) -> ThemeMode {
        match self {
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
            Self::NeuralObsidian => ThemeMode::NeuralObsidian,
            Self::OledPitch | Self::HighContrast => ThemeMode::OledPitch,
        }
    }

    #[must_use]
    pub const fn is_light(self) -> bool {
        matches!(self, Self::Light)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmarchyThemePalette {
    pub name: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub mode: String,
    pub uses_m3_brand: bool,
    pub accent: String,
    pub background: String,
    pub darker_background: String,
    pub lighter_background: String,
    pub foreground: String,
    pub dark_foreground: String,
    pub bright_foreground: String,
    pub selection: String,
    pub muted: String,
    pub red: String,
    pub green: String,
    pub blue: String,
    pub yellow: String,
    pub magenta: String,
    pub cyan: String,
    pub orange: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndroidDesktopSpec {
    pub android_version: String,
    pub api_level: u32,
    pub arch: String,
    pub guest_channel: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopPresetsCatalog {
    pub schema: String,
    pub ecosystems: Vec<String>,
    pub tiling_engines: Vec<String>,
    pub android_desktop: AndroidDesktopSpec,
    pub omarchy_themes: Vec<OmarchyThemePalette>,
    pub spec: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilingOptions {
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default = "default_true")]
    pub margins_enabled: bool,
    #[serde(default)]
    pub margin_px: Option<f32>,
    #[serde(default)]
    pub top_inset_px: Option<f32>,
    #[serde(default)]
    pub bottom_inset_px: Option<f32>,
    #[serde(default)]
    pub split_ratio: Option<f32>,
    #[serde(default = "default_workspace")]
    pub workspace: u8,
    #[serde(default)]
    pub scratchpad_open: bool,
    #[serde(default)]
    pub floating_apps: Vec<String>,
}

const fn default_true() -> bool {
    true
}

const fn default_workspace() -> u8 {
    1
}

impl Default for TilingOptions {
    fn default() -> Self {
        Self {
            variant: None,
            margins_enabled: true,
            margin_px: None,
            top_inset_px: None,
            bottom_inset_px: None,
            split_ratio: None,
            workspace: 1,
            scratchpad_open: false,
            floating_apps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TiledWindowRect {
    pub index: usize,
    pub zone_id: String,
    pub role: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub corner_radius: f32,
    pub workspace: u8,
    pub floating: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilingLayoutResult {
    pub engine: WindowTilingEngine,
    pub variant: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub work_area: TiledWindowRect,
    pub rects: Vec<TiledWindowRect>,
    pub preview_drop_zone: Option<TiledWindowRect>,
    pub scratchpad_rect: Option<TiledWindowRect>,
    pub workspace_count: u8,
    pub active_workspace: u8,
    pub fullscreen_allows_tiling: bool,
    pub margins_enabled: bool,
}

fn desktop_systems_spec() -> &'static Value {
    static SPEC: OnceLock<Value> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_json::from_str(DESKTOP_SYSTEMS_JSON)
            .expect("valid configs/design/desktop-systems.json")
    })
}

fn parse_toml_kv(raw: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let key = k.trim().to_owned();
        let val = v.trim().trim_matches('"').trim_matches('\'').to_owned();
        map.insert(key, val);
    }
    map
}

fn parse_omarchy_toml(
    id: &str,
    raw: &str,
    mode_override: Option<DesktopColorMode>,
) -> OmarchyThemePalette {
    let kv = parse_toml_kv(raw);
    let uses_m3_brand = kv.get("seed").is_some_and(|s| s == "brand");
    let scheme_mode = mode_override
        .unwrap_or_else(|| {
            DesktopColorMode::parse(kv.get("mode").map(String::as_str).unwrap_or("dark"))
        })
        .to_theme_mode();
    let m3 = m3_bundle().color_scheme(scheme_mode, None);

    let pick = |toml_key: &str, m3_role: &str| -> String {
        if uses_m3_brand {
            return m3.role_hex(m3_role).unwrap_or_default().to_owned();
        }
        kv.get(toml_key)
            .cloned()
            .unwrap_or_else(|| m3.role_hex(m3_role).unwrap_or_default().to_owned())
    };

    let bg = pick("background", "Background");
    let fg = pick("foreground", "OnSurface");
    OmarchyThemePalette {
        name: kv.get("name").cloned().unwrap_or_else(|| id.to_owned()),
        title: kv.get("title").cloned().unwrap_or_else(|| id.to_owned()),
        author: kv.get("author").cloned().unwrap_or_default(),
        description: kv.get("description").cloned().unwrap_or_default(),
        mode: kv.get("mode").cloned().unwrap_or_else(|| "dark".to_owned()),
        uses_m3_brand,
        accent: pick("accent", "Primary"),
        background: bg.clone(),
        darker_background: kv
            .get("darker_background")
            .cloned()
            .unwrap_or_else(|| pick("background", "SurfaceContainerLowest")),
        lighter_background: kv
            .get("lighter_background")
            .cloned()
            .unwrap_or_else(|| pick("background", "SurfaceContainerHigh")),
        foreground: fg.clone(),
        dark_foreground: kv
            .get("dark_foreground")
            .cloned()
            .unwrap_or_else(|| pick("foreground", "OnSurfaceVariant")),
        bright_foreground: kv.get("bright_foreground").cloned().unwrap_or(fg),
        selection: kv
            .get("selection")
            .cloned()
            .unwrap_or_else(|| pick("lighter_background", "SecondaryContainer")),
        muted: kv
            .get("muted")
            .cloned()
            .unwrap_or_else(|| pick("dark_foreground", "Outline")),
        red: pick("red", "Error"),
        green: pick("green", "Tertiary"),
        blue: pick("blue", "Primary"),
        yellow: pick("yellow", "Secondary"),
        magenta: pick("magenta", "TertiaryContainer"),
        cyan: pick("cyan", "SecondaryContainer"),
        orange: pick("orange", "ErrorContainer"),
    }
}

/// Return all 28 Omarchy theme palettes parsed from `configs/themes/*/theme.toml`.
#[must_use]
pub fn list_omarchy_themes() -> Vec<OmarchyThemePalette> {
    OMARCHY_THEME_TOMLS
        .iter()
        .map(|(id, raw)| parse_omarchy_toml(id, raw, None))
        .collect()
}

/// Load a specific Omarchy theme palette by directory name (case-insensitive).
pub fn load_omarchy_theme(name: &str) -> Result<OmarchyThemePalette, String> {
    load_omarchy_theme_with_mode(name, None)
}

fn load_omarchy_theme_with_mode(
    name: &str,
    mode: Option<DesktopColorMode>,
) -> Result<OmarchyThemePalette, String> {
    let needle = name.trim().to_ascii_lowercase().replace('_', "-");
    OMARCHY_THEME_TOMLS
        .iter()
        .find(|(id, _)| *id == needle)
        .map(|(id, raw)| parse_omarchy_toml(id, raw, mode))
        .ok_or_else(|| {
            let names: Vec<&str> = OMARCHY_THEME_TOMLS.iter().map(|(id, _)| *id).collect();
            format!(
                "unknown Omarchy theme '{name}'; available: {}",
                names.join(", ")
            )
        })
}

fn load_android_desktop_spec() -> AndroidDesktopSpec {
    let base = parse_toml_kv(ANDROID_BASE_TOML);
    let desktop = parse_toml_kv(ANDROID_DESKTOP_TOML);
    let api_level = desktop
        .get("api_level")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(37);
    AndroidDesktopSpec {
        android_version: desktop
            .get("android_version")
            .cloned()
            .unwrap_or_else(|| "17".to_owned()),
        api_level,
        arch: desktop
            .get("arch")
            .or_else(|| base.get("arch"))
            .cloned()
            .unwrap_or_else(|| "x86_64".to_owned()),
        guest_channel: base
            .get("system")
            .cloned()
            .unwrap_or_else(|| "VANILLA".to_owned()),
    }
}

/// Return the declarative multi-OS desktop presets catalog, tiling engines, Android specs, and
/// all 28 Omarchy themes.
#[must_use]
pub fn desktop_presets_catalog() -> DesktopPresetsCatalog {
    DesktopPresetsCatalog {
        schema: "aphrody.desktop-systems.v1".to_owned(),
        ecosystems: vec![
            DesktopThemeKind::Material3Googlebook.as_str().to_owned(),
            DesktopThemeKind::AppleLiquidGlass.as_str().to_owned(),
            DesktopThemeKind::FluentMica.as_str().to_owned(),
            DesktopThemeKind::Omarchy.as_str().to_owned(),
            DesktopThemeKind::ViceCitySunset.as_str().to_owned(),
        ],
        tiling_engines: vec![
            WindowTilingEngine::NativeGridSnap.as_str().to_owned(),
            WindowTilingEngine::HyprMacBsp.as_str().to_owned(),
            WindowTilingEngine::AndroidFreeform.as_str().to_owned(),
            WindowTilingEngine::FluentSnapLayout.as_str().to_owned(),
        ],
        android_desktop: load_android_desktop_spec(),
        omarchy_themes: list_omarchy_themes(),
        spec: desktop_systems_spec().clone(),
    }
}

// ============================================================================
// Windowing & Tiling Geometry Solver
// ============================================================================

fn work_area_for(
    engine: WindowTilingEngine,
    viewport_w: u32,
    viewport_h: u32,
    opts: &TilingOptions,
) -> (TiledWindowRect, f32, f32) {
    let vw = viewport_w.max(320) as f32;
    let vh = viewport_h.max(240) as f32;
    let gap = if opts.margins_enabled {
        opts.margin_px.unwrap_or(match engine {
            WindowTilingEngine::HyprMacBsp => 8.0,
            WindowTilingEngine::NativeGridSnap | WindowTilingEngine::FluentSnapLayout => 8.0,
            WindowTilingEngine::AndroidFreeform => 12.0,
        })
    } else {
        0.0
    };
    let top = opts.top_inset_px.unwrap_or(match engine {
        WindowTilingEngine::NativeGridSnap => 28.0,
        WindowTilingEngine::AndroidFreeform => 28.0,
        WindowTilingEngine::HyprMacBsp => 32.0,
        WindowTilingEngine::FluentSnapLayout => 0.0,
    });
    let bottom = opts.bottom_inset_px.unwrap_or(match engine {
        WindowTilingEngine::AndroidFreeform => 68.0,
        WindowTilingEngine::FluentSnapLayout => 48.0,
        WindowTilingEngine::NativeGridSnap => 64.0,
        WindowTilingEngine::HyprMacBsp => 0.0,
    });
    let radius = match engine {
        WindowTilingEngine::NativeGridSnap => 16.0,
        WindowTilingEngine::HyprMacBsp => 10.0,
        WindowTilingEngine::AndroidFreeform => 20.0,
        WindowTilingEngine::FluentSnapLayout => 8.0,
    };
    let x = gap;
    let y = top + gap;
    let w = (vw - gap * 2.0).max(120.0);
    let h = (vh - top - bottom - gap * 2.0).max(120.0);
    let area = TiledWindowRect {
        index: 0,
        zone_id: "work_area".to_owned(),
        role: "work_area".to_owned(),
        x,
        y,
        w,
        h,
        corner_radius: radius,
        workspace: opts.workspace.clamp(1, 10),
        floating: false,
    };
    (area, gap, radius)
}

fn split_pair(a: f32, total: f32, ratio: f32, gap: f32) -> (f32, f32, f32, f32) {
    let usable = (total - gap).max(2.0);
    let first = (usable * ratio).round().clamp(1.0, usable - 1.0);
    let second = (usable - first).max(1.0);
    (a, first, a + first + gap, second)
}

fn rect_entry(
    index: usize,
    zone_id: impl Into<String>,
    role: impl Into<String>,
    dims: [f32; 4],
    radius: f32,
    workspace: u8,
    floating: bool,
) -> TiledWindowRect {
    TiledWindowRect {
        index,
        zone_id: zone_id.into(),
        role: role.into(),
        x: dims[0],
        y: dims[1],
        w: dims[2],
        h: dims[3],
        corner_radius: radius,
        workspace,
        floating,
    }
}

/// Compute exact window geometry for any `WindowTilingEngine` and `window_count`.
#[must_use]
pub fn compute_tiling_layout(
    engine: WindowTilingEngine,
    viewport_w: u32,
    viewport_h: u32,
    window_count: usize,
    opts: &TilingOptions,
) -> TilingLayoutResult {
    let count = window_count.clamp(1, 32);
    let ws = opts.workspace.clamp(1, 10);
    let (area, gap, radius) = work_area_for(engine, viewport_w, viewport_h, opts);
    let ratio = opts.split_ratio.unwrap_or(0.5).clamp(0.2, 0.8);

    let (variant, rects, preview_drop_zone, scratchpad_rect) = match engine {
        WindowTilingEngine::NativeGridSnap => {
            compute_native_grid(&area, count, gap, radius, ws, opts.variant.as_deref())
        }
        WindowTilingEngine::HyprMacBsp => {
            compute_hyprmac_bsp(&area, count, gap, radius, ratio, ws, opts)
        }
        WindowTilingEngine::AndroidFreeform => compute_android_freeform(&area, count, radius, ws),
        WindowTilingEngine::FluentSnapLayout => {
            compute_fluent_snap(&area, count, gap, radius, ws, opts.variant.as_deref())
        }
    };

    TilingLayoutResult {
        engine,
        variant,
        viewport_width: viewport_w.max(320),
        viewport_height: viewport_h.max(240),
        work_area: area,
        rects,
        preview_drop_zone,
        scratchpad_rect,
        workspace_count: if engine == WindowTilingEngine::HyprMacBsp {
            10
        } else {
            1
        },
        active_workspace: ws,
        fullscreen_allows_tiling: matches!(
            engine,
            WindowTilingEngine::NativeGridSnap | WindowTilingEngine::HyprMacBsp
        ),
        margins_enabled: opts.margins_enabled,
    }
}

fn compute_native_grid(
    area: &TiledWindowRect,
    count: usize,
    gap: f32,
    radius: f32,
    ws: u8,
    requested: Option<&str>,
) -> (
    String,
    Vec<TiledWindowRect>,
    Option<TiledWindowRect>,
    Option<TiledWindowRect>,
) {
    let variant = requested.map(str::to_owned).unwrap_or_else(|| match count {
        1 => "fill".to_owned(),
        2 => "halves".to_owned(),
        3 => "three_primary_right_stack".to_owned(),
        4 => "quarters".to_owned(),
        _ => "cascade".to_owned(),
    });

    let mut rects = Vec::with_capacity(count);
    match variant.as_str() {
        "fill" => {
            for i in 0..count {
                rects.push(rect_entry(
                    i,
                    "fill",
                    "ideal_fill",
                    [area.x, area.y, area.w, area.h],
                    radius,
                    ws,
                    false,
                ));
            }
        }
        "center" => {
            let cw = (area.w * 0.82).round();
            let ch = (area.h * 0.82).round();
            let cx = area.x + ((area.w - cw) * 0.5).round();
            let cy = area.y + ((area.h - ch) * 0.5).round();
            for i in 0..count {
                rects.push(rect_entry(
                    i,
                    "center",
                    "ideal_center",
                    [cx, cy, cw, ch],
                    radius,
                    ws,
                    true,
                ));
            }
        }
        "halves" => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
            let zones = [
                ("left_half", [lx, area.y, lw, area.h]),
                ("right_half", [rx, area.y, rw, area.h]),
            ];
            for i in 0..count {
                let (zid, dims) = zones[i % 2];
                rects.push(rect_entry(i, zid, "snap_half", dims, radius, ws, false));
            }
        }
        "halves_vertical" => {
            let (ty, th, by, bh) = split_pair(area.y, area.h, 0.5, gap);
            let zones = [
                ("top_half", [area.x, ty, area.w, th]),
                ("bottom_half", [area.x, by, area.w, bh]),
            ];
            for i in 0..count {
                let (zid, dims) = zones[i % 2];
                rects.push(rect_entry(
                    i,
                    zid,
                    "snap_half_vertical",
                    dims,
                    radius,
                    ws,
                    false,
                ));
            }
        }
        "three_columns" => {
            let cw = ((area.w - gap * 2.0) / 3.0).floor().max(1.0);
            for i in 0..count {
                let col = i % 3;
                let x = area.x + (cw + gap) * col as f32;
                let w = if col == 2 { area.x + area.w - x } else { cw };
                rects.push(rect_entry(
                    i,
                    format!("col_{col}"),
                    "snap_column",
                    [x, area.y, w, area.h],
                    radius,
                    ws,
                    false,
                ));
            }
        }
        "quarters" => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
            let (ty, th, by, bh) = split_pair(area.y, area.h, 0.5, gap);
            let zones = [
                ("top_left", [lx, ty, lw, th]),
                ("top_right", [rx, ty, rw, th]),
                ("bottom_left", [lx, by, lw, bh]),
                ("bottom_right", [rx, by, rw, bh]),
            ];
            for i in 0..count {
                let (zid, dims) = zones[i % 4];
                rects.push(rect_entry(i, zid, "snap_quarter", dims, radius, ws, false));
            }
        }
        "three_primary_right_stack" => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
            let (ty, th, by, bh) = split_pair(area.y, area.h, 0.5, gap);
            let zones = [
                ("left_half", [lx, area.y, lw, area.h]),
                ("top_right", [rx, ty, rw, th]),
                ("bottom_right", [rx, by, rw, bh]),
            ];
            for i in 0..count {
                let (zid, dims) = zones[i % 3];
                rects.push(rect_entry(i, zid, "snap_grid", dims, radius, ws, false));
            }
        }
        _ => {
            let base_w = (area.w * 0.68).max(240.0).min(area.w);
            let base_h = (area.h * 0.72).max(180.0).min(area.h);
            for i in 0..count {
                let step = (i % 5) as f32;
                let x = (area.x + step * 36.0).min(area.x + area.w - base_w);
                let y = (area.y + step * 28.0).min(area.y + area.h - base_h);
                rects.push(rect_entry(
                    i,
                    format!("cascade_{i}"),
                    "ideal_cascade",
                    [x, y, base_w, base_h],
                    radius,
                    ws,
                    true,
                ));
            }
        }
    }

    let (_, _, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
    let preview = Some(rect_entry(
        0,
        "option_drag_right_half",
        "drop_zone_preview",
        [rx, area.y, rw, area.h],
        radius,
        ws,
        false,
    ));
    (variant, rects, preview, None)
}

fn compute_hyprmac_bsp(
    area: &TiledWindowRect,
    count: usize,
    gap: f32,
    radius: f32,
    ratio: f32,
    ws: u8,
    opts: &TilingOptions,
) -> (
    String,
    Vec<TiledWindowRect>,
    Option<TiledWindowRect>,
    Option<TiledWindowRect>,
) {
    let mut rects = Vec::with_capacity(count);
    let mut rem = [area.x, area.y, area.w, area.h];
    for i in 0..count {
        if i + 1 == count {
            rects.push(rect_entry(
                i,
                format!("bsp_leaf_{i}"),
                "dwindle_leaf",
                rem,
                radius,
                ws,
                false,
            ));
            break;
        }
        let [rx, ry, rw, rh] = rem;
        if rw >= rh {
            let (x0, w0, x1, w1) = split_pair(rx, rw, ratio, gap);
            rects.push(rect_entry(
                i,
                format!("bsp_left_{i}"),
                "dwindle_vsplit",
                [x0, ry, w0, rh],
                radius,
                ws,
                false,
            ));
            rem = [x1, ry, w1, rh];
        } else {
            let (y0, h0, y1, h1) = split_pair(ry, rh, ratio, gap);
            rects.push(rect_entry(
                i,
                format!("bsp_top_{i}"),
                "dwindle_hsplit",
                [rx, y0, rw, h0],
                radius,
                ws,
                false,
            ));
            rem = [rx, y1, rw, h1];
        }
    }

    let scratchpad = if opts.scratchpad_open {
        let sw = (area.w * 0.70).round();
        let sh = (area.h * 0.65).round();
        let sx = area.x + ((area.w - sw) * 0.5).round();
        let sy = area.y + ((area.h - sh) * 0.5).round();
        Some(rect_entry(
            0,
            "hyprmac_scratchpad",
            "scratchpad",
            [sx, sy, sw, sh],
            radius,
            ws,
            true,
        ))
    } else {
        None
    };

    let preview = rects.last().cloned().map(|mut last| {
        last.zone_id = "bsp_next_split_preview".to_owned();
        last.role = "drop_zone_preview".to_owned();
        last
    });
    ("dwindle".to_owned(), rects, preview, scratchpad)
}

fn compute_android_freeform(
    area: &TiledWindowRect,
    count: usize,
    radius: f32,
    ws: u8,
) -> (
    String,
    Vec<TiledWindowRect>,
    Option<TiledWindowRect>,
    Option<TiledWindowRect>,
) {
    let min_w = 360.0_f32.min(area.w);
    let min_h = 280.0_f32.min(area.h);
    let win_w = (area.w * 0.66).max(min_w).min(area.w);
    let win_h = (area.h * 0.78).max(min_h).min(area.h);
    let mut rects = Vec::with_capacity(count);
    for i in 0..count {
        let step = (i % 6) as f32;
        let x = (area.x + step * 44.0).min(area.x + area.w - win_w);
        let y = (area.y + step * 36.0).min(area.y + area.h - win_h);
        rects.push(rect_entry(
            i,
            format!("freeform_{i}"),
            "android_freeform_window",
            [x, y, win_w, win_h],
            radius,
            ws,
            true,
        ));
    }
    ("freeform".to_owned(), rects, None, None)
}

fn compute_fluent_snap(
    area: &TiledWindowRect,
    count: usize,
    gap: f32,
    radius: f32,
    ws: u8,
    requested: Option<&str>,
) -> (
    String,
    Vec<TiledWindowRect>,
    Option<TiledWindowRect>,
    Option<TiledWindowRect>,
) {
    let variant = requested.map(str::to_owned).unwrap_or_else(|| match count {
        1 | 2 => "2_zone".to_owned(),
        3 => "3_zone".to_owned(),
        _ => "4_zone".to_owned(),
    });

    let zones: Vec<(&str, [f32; 4])> = match variant.as_str() {
        "2_zone_priority" => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.66, gap);
            vec![
                ("zone_1_primary", [lx, area.y, lw, area.h]),
                ("zone_2_secondary", [rx, area.y, rw, area.h]),
            ]
        }
        "3_zone" => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
            let (ty, th, by, bh) = split_pair(area.y, area.h, 0.5, gap);
            vec![
                ("zone_1_left", [lx, area.y, lw, area.h]),
                ("zone_2_top_right", [rx, ty, rw, th]),
                ("zone_3_bottom_right", [rx, by, rw, bh]),
            ]
        }
        "3_zone_center" => {
            let w_side = ((area.w - gap * 2.0) * 0.25).floor().max(1.0);
            let w_mid = (area.w - gap * 2.0 - w_side * 2.0).max(1.0);
            vec![
                ("zone_1_left", [area.x, area.y, w_side, area.h]),
                (
                    "zone_2_center",
                    [area.x + w_side + gap, area.y, w_mid, area.h],
                ),
                (
                    "zone_3_right",
                    [area.x + w_side + gap + w_mid + gap, area.y, w_side, area.h],
                ),
            ]
        }
        "3_zone_columns" => {
            let cw = ((area.w - gap * 2.0) / 3.0).floor().max(1.0);
            let last_w = (area.w - gap * 2.0 - cw * 2.0).max(1.0);
            vec![
                ("zone_1_col", [area.x, area.y, cw, area.h]),
                ("zone_2_col", [area.x + cw + gap, area.y, cw, area.h]),
                (
                    "zone_3_col",
                    [area.x + (cw + gap) * 2.0, area.y, last_w, area.h],
                ),
            ]
        }
        "4_zone" => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
            let (ty, th, by, bh) = split_pair(area.y, area.h, 0.5, gap);
            vec![
                ("zone_1_tl", [lx, ty, lw, th]),
                ("zone_2_tr", [rx, ty, rw, th]),
                ("zone_3_bl", [lx, by, lw, bh]),
                ("zone_4_br", [rx, by, rw, bh]),
            ]
        }
        _ => {
            let (lx, lw, rx, rw) = split_pair(area.x, area.w, 0.5, gap);
            vec![
                ("zone_1_left", [lx, area.y, lw, area.h]),
                ("zone_2_right", [rx, area.y, rw, area.h]),
            ]
        }
    };

    let mut rects = Vec::with_capacity(count);
    for i in 0..count {
        let (zid, dims) = zones[i % zones.len()];
        rects.push(rect_entry(
            i,
            zid,
            "fluent_snap_zone",
            dims,
            radius,
            ws,
            false,
        ));
    }
    let preview = zones
        .first()
        .map(|(zid, dims)| rect_entry(0, *zid, "snap_picker_highlight", *dims, radius, ws, false));
    (variant, rects, preview, None)
}

// ============================================================================
// DesignDocument Shell Presets (Googlebook M3, Apple Liquid Glass, Fluent, Omarchy)
// ============================================================================

fn spec_str<'a>(pointer: &str, fallback: &'a str) -> &'a str {
    desktop_systems_spec()
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
}

fn wcag_fg_for(bg_hex: &str, light_hex: &str, dark_hex: &str) -> String {
    let bg = parse_hex_rgb(bg_hex).unwrap_or([24, 24, 28]);
    let light = parse_hex_rgb(light_hex).unwrap_or([255, 255, 255]);
    let dark = parse_hex_rgb(dark_hex).unwrap_or([17, 17, 17]);
    best_wcag_foreground(bg, light, dark)
}

fn role_button(id: &str, label: &str, bg_role: &str, radius: f32) -> DesignNode {
    let mut btn = DesignNode::new(id, DesignNodeKind::Button)
        .with_name(label)
        .with_text(label);
    btn.style = DesignStyle {
        height: Some(44.0),
        padding: Some(12.0),
        corner_radius: Some(radius),
        bg_role: Some(bg_role.to_owned()),
        fg_role: Some(ResolvedColorScheme::paired_on_role(bg_role).to_owned()),
        font_size: Some(14.0),
        ..Default::default()
    };
    btn.a11y = DesignA11y {
        role: Some("button".to_owned()),
        aria_label: Some(label.to_owned()),
        focusable: true,
        action: Some(format!("action::{id}")),
    };
    btn
}

fn hex_button(
    id: &str,
    label: &str,
    bg_hex: &str,
    fg_hex: &str,
    border_hex: Option<&str>,
    radius: f32,
    opacity: Option<f32>,
) -> DesignNode {
    let mut btn = DesignNode::new(id, DesignNodeKind::Button)
        .with_name(label)
        .with_text(label);
    btn.style = DesignStyle {
        height: Some(44.0),
        padding: Some(12.0),
        corner_radius: Some(radius),
        bg_hex: Some(bg_hex.to_owned()),
        fg_hex: Some(fg_hex.to_owned()),
        border_role: border_hex.map(str::to_owned),
        border_width: border_hex.map(|_| 1.0),
        font_size: Some(14.0),
        opacity,
        ..Default::default()
    };
    btn.a11y = DesignA11y {
        role: Some("button".to_owned()),
        aria_label: Some(label.to_owned()),
        focusable: true,
        action: Some(format!("action::{id}")),
    };
    btn
}

fn build_hyprmac_overlay_card(
    bg_hex: &str,
    fg_hex: &str,
    accent_hex: &str,
    border_hex: &str,
    opacity: Option<f32>,
) -> DesignNode {
    let mut hud = DesignNode::new("hyprmac-shortcut-hud", DesignNodeKind::Card)
        .with_name("HyprMac BSP Overlay (Hypr+K)");
    hud.style = DesignStyle {
        padding: Some(16.0),
        gap: Some(8.0),
        corner_radius: Some(12.0),
        bg_hex: Some(bg_hex.to_owned()),
        fg_hex: Some(fg_hex.to_owned()),
        border_role: Some(border_hex.to_owned()),
        border_width: Some(2.0),
        opacity,
        material: Some("hyprmac_bsp_hud".to_owned()),
        ..Default::default()
    };
    let mut ws_row =
        DesignNode::new("hyprmac-workspaces", DesignNodeKind::TabBar).with_name("Workspaces 1..10");
    ws_row.a11y.aria_label = Some("Workspaces 1 through 10".to_owned());
    ws_row.style.gap = Some(8.0);
    ws_row.style.padding = Some(4.0);
    let accent_fg = wcag_fg_for(accent_hex, "#FFFFFF", "#0F1115");
    for ws in 1..=5 {
        let label = format!("WS {ws}");
        let (b_bg, b_fg) = if ws == 1 {
            (accent_hex, accent_fg.as_str())
        } else {
            (bg_hex, fg_hex)
        };
        ws_row.children.push(hex_button(
            &format!("ws-{ws}"),
            &label,
            b_bg,
            b_fg,
            Some(border_hex),
            8.0,
            opacity,
        ));
    }
    let mut keys = DesignNode::new("hyprmac-bindings", DesignNodeKind::Text)
        .with_text("Hypr (Caps Lock) | Hypr+K Overlay | Hypr+1..0 Workspaces | Hypr+Shift+Return Scratchpad | BSP Dwindle");
    keys.style.fg_hex = Some(fg_hex.to_owned());
    keys.style.font_size = Some(13.0);
    hud.children.push(ws_row);
    hud.children.push(keys);
    hud
}

fn build_googlebook_doc(
    mode: DesktopColorMode,
    tiling: WindowTilingEngine,
    transparency: f32,
) -> DesignDocument {
    let m3 = m3_bundle();
    let dock_alpha = if transparency <= 0.01 {
        1.0
    } else {
        (0.45 + 0.35 * transparency).clamp(0.45, 0.95)
    };
    let glass_alpha = if transparency <= 0.01 {
        1.0
    } else {
        (0.55 + 0.35 * transparency).clamp(0.55, 0.96)
    };
    let win_radius = m3.shape_dp("CornerLargeIncreased").unwrap_or(20.0);

    let mut root = DesignNode::new("googlebook-desktop", DesignNodeKind::Column)
        .with_name("Googlebook M3 Expressive Desktop");
    root.style = DesignStyle {
        width: Some(1280.0),
        height: Some(800.0),
        padding: Some(12.0),
        gap: Some(12.0),
        bg_role: Some("Background".to_owned()),
        fg_role: Some("OnBackground".to_owned()),
        material: Some("prism_backdrop".to_owned()),
        ..Default::default()
    };

    // Compact top status bar matching goal.mp4 frame_01
    let mut top_bar =
        DesignNode::new("top-status-bar", DesignNodeKind::Toolbar).with_name("Status Bar");
    top_bar.style = DesignStyle {
        height: Some(32.0),
        padding: Some(8.0),
        gap: Some(12.0),
        justify: Some("space_between".to_owned()),
        align: Some("center".to_owned()),
        bg_role: Some("SurfaceContainerLowest".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        corner_radius: Some(12.0),
        ..Default::default()
    };
    let mut clock =
        DesignNode::new("status-clock", DesignNodeKind::Text).with_text("10:00  Mer., 20 mai");
    clock.style.font_size = Some(13.0);
    clock.style.fg_role = Some("OnSurface".to_owned());
    let mut status_tray = DesignNode::new("status-tray", DesignNodeKind::Badge)
        .with_text("Assistant | Bell | Wi-Fi | Battery 94%");
    status_tray.style.bg_role = Some("SurfaceContainerHigh".to_owned());
    status_tray.style.fg_role = Some("OnSurface".to_owned());
    status_tray.style.padding = Some(8.0);
    status_tray.style.corner_radius = Some(999.0);
    top_bar.children.push(clock);
    top_bar.children.push(status_tray);
    root.children.push(top_bar);

    // Freeform / tiled charcoal window stage matching goal.mp4 frame_01..02
    let mut stage = DesignNode::new("freeform-stage", DesignNodeKind::Row)
        .with_name(format!("Stage ({})", tiling.as_str()));
    stage.style.flex_grow = Some(1.0);
    stage.style.gap = Some(16.0);

    let mut photo_win =
        DesignNode::new("window-photos", DesignNodeKind::Card).with_name("Photos Freeform Window");
    photo_win.style = DesignStyle {
        width: Some(460.0),
        padding: Some(12.0),
        gap: Some(12.0),
        corner_radius: Some(win_radius),
        bg_role: Some("SurfaceContainerLowest".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        border_role: Some("OutlineVariant".to_owned()),
        elevation_level: Some("Level2".to_owned()),
        ..Default::default()
    };
    let mut photo_chrome =
        DesignNode::new("photos-titlebar", DesignNodeKind::Toolbar).with_name("Photos Titlebar");
    photo_chrome.style = DesignStyle {
        height: Some(44.0),
        padding: Some(8.0),
        gap: Some(8.0),
        bg_role: Some("SurfaceContainer".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        corner_radius: Some(8.0),
        ..Default::default()
    };
    photo_chrome.children.push(role_button(
        "photos-min",
        "Minimize",
        "SecondaryContainer",
        999.0,
    ));
    photo_chrome.children.push(role_button(
        "photos-close",
        "Close",
        "SecondaryContainer",
        999.0,
    ));
    let mut photo_canvas = DesignNode::new("photos-canvas", DesignNodeKind::ShaderCanvas)
        .with_name("Oak Meadow Viewer")
        .with_text("Oak Meadow Photo Preview");
    photo_canvas.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(12.0),
        corner_radius: Some(8.0),
        bg_role: Some("SurfaceContainerHigh".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        ..Default::default()
    };
    photo_win.children.push(photo_chrome);
    photo_win.children.push(photo_canvas);

    let mut notes_win =
        DesignNode::new("window-canvas", DesignNodeKind::Card).with_name("Keep Canvas Window");
    notes_win.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(12.0),
        gap: Some(12.0),
        corner_radius: Some(win_radius),
        bg_role: Some("SurfaceContainerLow".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        border_role: Some("OutlineVariant".to_owned()),
        elevation_level: Some("Level3".to_owned()),
        ..Default::default()
    };
    let mut notes_chrome =
        DesignNode::new("canvas-titlebar", DesignNodeKind::Toolbar).with_name("Canvas Chrome");
    notes_chrome.style = DesignStyle {
        height: Some(44.0),
        padding: Some(8.0),
        gap: Some(8.0),
        bg_role: Some("SurfaceContainerHighest".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        corner_radius: Some(8.0),
        ..Default::default()
    };
    notes_chrome.children.push(role_button(
        "canvas-pen",
        "Brush",
        "PrimaryContainer",
        999.0,
    ));
    notes_chrome.children.push(role_button(
        "canvas-undo",
        "Undo",
        "SecondaryContainer",
        999.0,
    ));
    let mut notes_body =
        DesignNode::new("canvas-sheet", DesignNodeKind::Frame).with_text("Treehouse Sketch Sheet");
    notes_body.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(16.0),
        corner_radius: Some(8.0),
        bg_role: Some("SurfaceContainer".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        ..Default::default()
    };
    notes_win.children.push(notes_chrome);
    notes_win.children.push(notes_body);

    stage.children.push(photo_win);
    stage.children.push(notes_win);
    root.children.push(stage);

    // Floating Gemini transcript + 4-color bottom edge glow capsule matching goal.mp4 frame_03..04
    let mut gemini =
        DesignNode::new("gemini-overlay", DesignNodeKind::Column).with_name("Assistant Overlay");
    gemini.style = DesignStyle {
        padding: Some(12.0),
        gap: Some(8.0),
        corner_radius: Some(16.0),
        bg_role: Some("SurfaceContainerHigh".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        border_role: Some("OutlineVariant".to_owned()),
        opacity: Some(glass_alpha),
        material: Some("gemini_glow".to_owned()),
        ..Default::default()
    };
    let mut transcript = DesignNode::new("gemini-transcript", DesignNodeKind::Text)
        .with_text("Vous : Ok Google  |  Gemini : Bonjour !");
    transcript.style.font_size = Some(14.0);
    transcript.style.fg_role = Some("OnSurface".to_owned());
    let mut capsule = DesignNode::new("gemini-capsule", DesignNodeKind::Toolbar)
        .with_name("Prompt Edge-Glow Capsule");
    capsule.style = DesignStyle {
        height: Some(56.0),
        padding: Some(8.0),
        gap: Some(8.0),
        corner_radius: Some(999.0),
        bg_role: Some("SurfaceContainerLowest".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        border_role: Some("Primary".to_owned()),
        opacity: Some(glass_alpha),
        material: Some("four_color_edge_stipple".to_owned()),
        ..Default::default()
    };
    capsule.children.push(role_button(
        "gemini-share",
        "Share",
        "PrimaryContainer",
        999.0,
    ));
    capsule.children.push(role_button(
        "gemini-mic",
        "Voice",
        "SecondaryContainer",
        999.0,
    ));
    capsule.children.push(role_button(
        "gemini-kbd",
        "Keyboard",
        "SurfaceContainer",
        999.0,
    ));
    capsule.children.push(role_button(
        "gemini-close",
        "Dismiss",
        "SurfaceContainer",
        999.0,
    ));
    gemini.children.push(transcript);
    gemini.children.push(capsule);
    root.children.push(gemini);

    // Bottom translucent app pill dock matching goal.mp4 frame_01..04
    let mut dock =
        DesignNode::new("bottom-app-dock", DesignNodeKind::TabBar).with_name("Googlebook App Dock");
    dock.a11y.aria_label = Some("Googlebook App Dock".to_owned());
    dock.style = DesignStyle {
        height: Some(60.0),
        padding: Some(8.0),
        gap: Some(12.0),
        justify: Some("center".to_owned()),
        align: Some("center".to_owned()),
        corner_radius: Some(16.0),
        bg_role: Some("SurfaceContainerHigh".to_owned()),
        fg_role: Some("OnSurface".to_owned()),
        opacity: Some(dock_alpha),
        material: Some("translucent_capsule".to_owned()),
        ..Default::default()
    };
    for (id, label, role) in [
        ("dock-search", "Search", "SurfaceContainer"),
        ("dock-gemini", "Gemini", "PrimaryContainer"),
        ("dock-browser", "Browser", "SecondaryContainer"),
        ("dock-files", "Files", "SecondaryContainer"),
        ("dock-photos", "Photos", "SecondaryContainer"),
        ("dock-canvas", "Canvas", "Primary"),
    ] {
        dock.children.push(role_button(id, label, role, 999.0));
    }
    root.children.push(dock);

    let mut doc = DesignDocument::new("GooglebookM3Desktop", root);
    doc.theme = mode.to_theme_mode();
    doc.surface_mode = SurfaceMode::Operate;
    doc.source = DesignSourceKind::Authored;
    doc
}

fn build_apple_liquid_glass_doc(
    mode: DesktopColorMode,
    tiling: WindowTilingEngine,
    transparency: f32,
) -> DesignDocument {
    let pal_key = if mode.is_light() { "light" } else { "dark" };
    let base = format!("/ecosystems/apple_liquid_glass/palettes/{pal_key}");
    let wallpaper = spec_str(&format!("{base}/wallpaperBase"), "#0A101D");
    let sidebar_bg = spec_str(&format!("{base}/sidebarGlass"), "#192130");
    let toolbar_bg = spec_str(&format!("{base}/toolbarGlass"), "#1E2738");
    let card_bg = spec_str(&format!("{base}/cardGlass"), "#232C3F");
    let control_bg = spec_str(&format!("{base}/controlFill"), "#2E3A52");
    let accent = spec_str(&format!("{base}/accent"), "#0A84FF");
    let text_primary = spec_str(&format!("{base}/textPrimary"), "#F5F7FA");
    let specular = spec_str(&format!("{base}/specularRim"), "#FFFFFF");
    let hairline = spec_str(&format!("{base}/borderHairline"), "#3D4A63");

    let t = transparency.clamp(0.0, 1.0);
    let glass_alpha = if t <= 0.01 {
        1.0
    } else {
        (1.0 - t * 0.38).clamp(0.58, 1.0)
    };
    let accent_fg = wcag_fg_for(accent, "#FFFFFF", "#0A101D");
    let card_fg = wcag_fg_for(card_bg, text_primary, "#111827");
    let sidebar_fg = wcag_fg_for(sidebar_bg, text_primary, "#111827");
    let control_fg = wcag_fg_for(control_bg, text_primary, "#111827");

    let mut root = DesignNode::new("apple-liquid-glass", DesignNodeKind::Column)
        .with_name("Apple Liquid Glass Workspace");
    root.style = DesignStyle {
        width: Some(1280.0),
        height: Some(800.0),
        padding: Some(12.0),
        gap: Some(12.0),
        corner_radius: Some(24.0),
        bg_hex: Some(wallpaper.to_owned()),
        fg_hex: Some(text_primary.to_owned()),
        border_role: Some(specular.to_owned()),
        border_width: Some(1.0),
        material: Some(format!("liquid_glass:{t:.2}")),
        ..Default::default()
    };

    // Customizable NSToolbarItemGroup
    let mut toolbar = DesignNode::new("ns-toolbar-group", DesignNodeKind::Toolbar)
        .with_name("NSToolbarItemGroup");
    toolbar.style = DesignStyle {
        height: Some(52.0),
        padding: Some(8.0),
        gap: Some(8.0),
        corner_radius: Some(12.0),
        bg_hex: Some(toolbar_bg.to_owned()),
        fg_hex: Some(card_fg.clone()),
        border_role: Some(hairline.to_owned()),
        opacity: Some(glass_alpha),
        material: Some("liquid_glass_toolbar".to_owned()),
        ..Default::default()
    };
    toolbar.children.push(hex_button(
        "tb-sidebar",
        "Sidebar",
        control_bg,
        &control_fg,
        Some(hairline),
        999.0,
        Some(glass_alpha),
    ));
    toolbar.children.push(hex_button(
        "tb-tiling",
        tiling.as_str(),
        accent,
        &accent_fg,
        Some(specular),
        999.0,
        Some(glass_alpha),
    ));
    let mut glass_badge = DesignNode::new("glass-metric", DesignNodeKind::Badge).with_text(
        format!("Liquid Glass {t:.2} | NSScrollEdgeEffectStyle.hard"),
    );
    glass_badge.style = DesignStyle {
        padding: Some(8.0),
        corner_radius: Some(999.0),
        bg_hex: Some(control_bg.to_owned()),
        fg_hex: Some(control_fg.clone()),
        font_size: Some(13.0),
        ..Default::default()
    };
    toolbar.children.push(glass_badge);
    root.children.push(toolbar);

    // NavigationSplitView
    let mut split = DesignNode::new("navigation-split-view", DesignNodeKind::Row)
        .with_name("NavigationSplitView");
    split.style.flex_grow = Some(1.0);
    split.style.gap = Some(12.0);

    let mut sidebar =
        DesignNode::new("split-sidebar", DesignNodeKind::Sidebar).with_name("Liquid Glass Sidebar");
    sidebar.style = DesignStyle {
        width: Some(260.0),
        padding: Some(12.0),
        gap: Some(8.0),
        corner_radius: Some(12.0),
        bg_hex: Some(sidebar_bg.to_owned()),
        fg_hex: Some(sidebar_fg.clone()),
        border_role: Some(hairline.to_owned()),
        opacity: Some(glass_alpha),
        material: Some("liquid_glass_sidebar".to_owned()),
        ..Default::default()
    };
    for (id, label) in [
        ("nav-device-hub", "DeviceHub"),
        ("nav-tiling", "Window Tiling"),
        ("nav-vibrancy", "Specular Material"),
    ] {
        sidebar.children.push(hex_button(
            id,
            label,
            control_bg,
            &control_fg,
            Some(hairline),
            8.0,
            Some(glass_alpha),
        ));
    }
    split.children.push(sidebar);

    let mut detail = DesignNode::new("split-detail", DesignNodeKind::Column)
        .with_name("Detail Scroll Edge Surface");
    detail.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(12.0),
        gap: Some(12.0),
        corner_radius: Some(12.0),
        bg_hex: Some(card_bg.to_owned()),
        fg_hex: Some(card_fg.clone()),
        border_role: Some(specular.to_owned()),
        opacity: Some(glass_alpha),
        material: Some("scroll_edge_hard".to_owned()),
        ..Default::default()
    };

    let mut scroll_edge = DesignNode::new("scroll-edge-hard", DesignNodeKind::Separator)
        .with_name("NSScrollEdgeEffectStyle.hard");
    scroll_edge.style = DesignStyle {
        height: Some(4.0),
        bg_hex: Some(hairline.to_owned()),
        material: Some("NSScrollEdgeEffectStyle.hard".to_owned()),
        ..Default::default()
    };
    detail.children.push(scroll_edge);

    let mut device_hub = DesignNode::new("device-hub-card", DesignNodeKind::Card)
        .with_name("DeviceHub Resizable Simulator")
        .with_text(
            "DeviceHub | fullScreenAllowsTiling | windowIdealPlacement | Option-drag preview",
        );
    device_hub.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(16.0),
        gap: Some(12.0),
        corner_radius: Some(8.0),
        bg_hex: Some(sidebar_bg.to_owned()),
        fg_hex: Some(sidebar_fg),
        border_role: Some(hairline.to_owned()),
        opacity: Some(glass_alpha),
        material: Some("device_hub_viewport".to_owned()),
        ..Default::default()
    };
    detail.children.push(device_hub);

    if tiling == WindowTilingEngine::HyprMacBsp {
        detail.children.push(build_hyprmac_overlay_card(
            sidebar_bg,
            text_primary,
            accent,
            hairline,
            Some(glass_alpha),
        ));
    }

    split.children.push(detail);
    root.children.push(split);

    let mut doc = DesignDocument::new("AppleLiquidGlassDesktop", root);
    doc.theme = mode.to_theme_mode();
    doc.surface_mode = SurfaceMode::Operate;
    doc.source = DesignSourceKind::Authored;
    doc
}

fn build_fluent_mica_doc(
    mode: DesktopColorMode,
    tiling: WindowTilingEngine,
    transparency: f32,
) -> DesignDocument {
    let pal_key = match mode {
        DesktopColorMode::Light => "light",
        DesktopColorMode::HighContrast => "high_contrast",
        _ => "dark",
    };
    let base = format!("/ecosystems/fluent_mica/palettes/{pal_key}");
    let mica_bg = spec_str(&format!("{base}/micaBackdrop"), "#1C1F26");
    let mica_alt = spec_str(&format!("{base}/micaAltBackdrop"), "#15181E");
    let acrylic = spec_str(&format!("{base}/acrylicSurface"), "#2C303A");
    let card_bg = spec_str(&format!("{base}/cardSurface"), "#272A32");
    let control_bg = spec_str(&format!("{base}/controlFill"), "#353944");
    let accent = spec_str(&format!("{base}/accent"), "#60CDFF");
    let text_primary = spec_str(&format!("{base}/textPrimary"), "#FFFFFF");
    let border = spec_str(&format!("{base}/borderStroke"), "#454A56");
    let focus_ring = spec_str(&format!("{base}/focusRing"), "#FFFFFF");

    let acrylic_alpha = if mode == DesktopColorMode::HighContrast || transparency <= 0.01 {
        1.0
    } else {
        (0.72 + 0.20 * (1.0 - transparency)).clamp(0.70, 0.96)
    };
    let accent_fg = wcag_fg_for(accent, "#FFFFFF", "#000000");
    let card_fg = wcag_fg_for(card_bg, text_primary, "#000000");
    let control_fg = wcag_fg_for(control_bg, text_primary, "#000000");

    let mut root = DesignNode::new("fluent-mica-shell", DesignNodeKind::Column)
        .with_name("Windows Fluent Mica Shell");
    root.style = DesignStyle {
        width: Some(1280.0),
        height: Some(800.0),
        padding: Some(12.0),
        gap: Some(12.0),
        corner_radius: Some(8.0),
        bg_hex: Some(mica_bg.to_owned()),
        fg_hex: Some(text_primary.to_owned()),
        border_role: Some(border.to_owned()),
        border_width: Some(1.0),
        material: Some("mica".to_owned()),
        ..Default::default()
    };

    let mut titlebar =
        DesignNode::new("dwm-titlebar", DesignNodeKind::Toolbar).with_name("Mica Alt Command Bar");
    titlebar.style = DesignStyle {
        height: Some(48.0),
        padding: Some(8.0),
        gap: Some(8.0),
        corner_radius: Some(4.0),
        bg_hex: Some(mica_alt.to_owned()),
        fg_hex: Some(text_primary.to_owned()),
        border_role: Some(border.to_owned()),
        material: Some("mica_alt".to_owned()),
        ..Default::default()
    };
    titlebar.children.push(hex_button(
        "snap-picker",
        "Snap Layouts (Win+Z)",
        accent,
        &accent_fg,
        Some(focus_ring),
        4.0,
        None,
    ));
    titlebar.children.push(hex_button(
        "tiling-mode",
        tiling.as_str(),
        control_bg,
        &control_fg,
        Some(border),
        4.0,
        None,
    ));
    root.children.push(titlebar);

    let mut body = DesignNode::new("fluent-body", DesignNodeKind::Row);
    body.style.flex_grow = Some(1.0);
    body.style.gap = Some(12.0);

    let mut nav =
        DesignNode::new("fluent-nav-rail", DesignNodeKind::NavRail).with_name("NavigationView");
    nav.style = DesignStyle {
        width: Some(240.0),
        padding: Some(8.0),
        gap: Some(8.0),
        corner_radius: Some(8.0),
        bg_hex: Some(mica_alt.to_owned()),
        fg_hex: Some(text_primary.to_owned()),
        border_role: Some(border.to_owned()),
        material: Some("mica".to_owned()),
        ..Default::default()
    };
    for (id, label) in [
        ("nav-home", "Mica Base"),
        ("nav-acrylic", "Acrylic Flyout"),
        ("nav-snap", "Snap 2/3/4"),
    ] {
        nav.children.push(hex_button(
            id,
            label,
            control_bg,
            &control_fg,
            Some(border),
            4.0,
            None,
        ));
    }
    body.children.push(nav);

    let mut stage =
        DesignNode::new("fluent-stage", DesignNodeKind::Column).with_name("Fluent Snap Stage");
    stage.style.flex_grow = Some(1.0);
    stage.style.gap = Some(12.0);

    let mut mica_card = DesignNode::new("theme-shadow-card", DesignNodeKind::Card)
        .with_name("ThemeShadow Surface")
        .with_text(
            "Mica wallpaper tint | opaque unfocused fallback | 2px high-contrast focus ring",
        );
    mica_card.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(16.0),
        gap: Some(8.0),
        corner_radius: Some(8.0),
        bg_hex: Some(card_bg.to_owned()),
        fg_hex: Some(card_fg.clone()),
        border_role: Some(focus_ring.to_owned()),
        border_width: Some(2.0),
        elevation_level: Some("Level2".to_owned()),
        material: Some("mica_card".to_owned()),
        ..Default::default()
    };

    let mut flyout = DesignNode::new("acrylic-flyout", DesignNodeKind::Card)
        .with_name("Acrylic Transient Flyout")
        .with_text("Frosted transient Acrylic noise grain + Smoke modal scrim support");
    flyout.style = DesignStyle {
        padding: Some(12.0),
        gap: Some(8.0),
        corner_radius: Some(8.0),
        bg_hex: Some(acrylic.to_owned()),
        fg_hex: Some(text_primary.to_owned()),
        border_role: Some(border.to_owned()),
        opacity: Some(acrylic_alpha),
        elevation_level: Some("Level4".to_owned()),
        material: Some("acrylic".to_owned()),
        ..Default::default()
    };
    flyout.children.push(hex_button(
        "flyout-apply",
        "Apply Snap Layout",
        accent,
        &accent_fg,
        Some(focus_ring),
        4.0,
        Some(acrylic_alpha),
    ));

    stage.children.push(mica_card);
    stage.children.push(flyout);
    body.children.push(stage);
    root.children.push(body);

    let mut taskbar =
        DesignNode::new("fluent-taskbar", DesignNodeKind::TabBar).with_name("Windows Taskbar");
    taskbar.a11y.aria_label = Some("Windows Taskbar".to_owned());
    taskbar.style = DesignStyle {
        height: Some(48.0),
        padding: Some(8.0),
        gap: Some(8.0),
        justify: Some("center".to_owned()),
        align: Some("center".to_owned()),
        corner_radius: Some(4.0),
        bg_hex: Some(mica_alt.to_owned()),
        fg_hex: Some(text_primary.to_owned()),
        border_role: Some(border.to_owned()),
        material: Some("mica_alt".to_owned()),
        ..Default::default()
    };
    taskbar.children.push(hex_button(
        "tb-start",
        "Start",
        control_bg,
        &control_fg,
        Some(border),
        4.0,
        None,
    ));
    taskbar.children.push(hex_button(
        "tb-snap",
        "2 / 3 / 4 Zones",
        control_bg,
        &control_fg,
        Some(border),
        4.0,
        None,
    ));
    root.children.push(taskbar);

    let mut doc = DesignDocument::new("FluentMicaDesktop", root);
    doc.theme = mode.to_theme_mode();
    doc.surface_mode = SurfaceMode::Operate;
    doc.source = DesignSourceKind::Authored;
    doc
}

fn build_omarchy_doc(
    theme_name: &str,
    mode: DesktopColorMode,
    tiling: WindowTilingEngine,
    transparency: f32,
) -> Result<DesignDocument, String> {
    let pal = load_omarchy_theme_with_mode(theme_name, Some(mode))?;
    let alpha = if transparency <= 0.01 {
        1.0
    } else {
        transparency.clamp(0.65, 1.0)
    };
    let fg_main = wcag_fg_for(&pal.background, &pal.bright_foreground, "#101216");
    let fg_pane = wcag_fg_for(&pal.lighter_background, &pal.bright_foreground, "#101216");

    let mut root = DesignNode::new(format!("omarchy-{}", pal.name), DesignNodeKind::Column)
        .with_name(format!("Omarchy {}", pal.title));
    root.style = DesignStyle {
        width: Some(1280.0),
        height: Some(800.0),
        padding: Some(12.0),
        gap: Some(12.0),
        bg_hex: Some(pal.darker_background.clone()),
        fg_hex: Some(fg_main.clone()),
        material: Some(format!("omarchy:{}", pal.name)),
        ..Default::default()
    };

    let mut waybar = DesignNode::new("omarchy-bar", DesignNodeKind::Toolbar)
        .with_name(format!("Omarchy {} Bar", pal.title));
    waybar.style = DesignStyle {
        height: Some(48.0),
        padding: Some(8.0),
        gap: Some(8.0),
        corner_radius: Some(12.0),
        bg_hex: Some(pal.background.clone()),
        fg_hex: Some(fg_main.clone()),
        border_role: Some(pal.accent.clone()),
        border_width: Some(1.0),
        opacity: Some(alpha),
        ..Default::default()
    };
    let accent_fg = wcag_fg_for(&pal.accent, "#FFFFFF", "#0A0C10");
    waybar.children.push(hex_button(
        "omarchy-theme-pill",
        &pal.title,
        &pal.accent,
        &accent_fg,
        Some(&pal.accent),
        8.0,
        None,
    ));
    waybar.children.push(hex_button(
        "omarchy-tiling-pill",
        tiling.as_str(),
        &pal.lighter_background,
        &fg_pane,
        Some(&pal.muted),
        8.0,
        None,
    ));
    root.children.push(waybar);

    let mut bsp_row = DesignNode::new("omarchy-bsp-row", DesignNodeKind::Row);
    bsp_row.style.flex_grow = Some(1.0);
    bsp_row.style.gap = Some(12.0);

    let mut left = DesignNode::new("omarchy-pane-left", DesignNodeKind::Card)
        .with_name("Primary Tiled Pane")
        .with_text(format!(
            "{} ({}) — Hyprland BSP Primary",
            pal.title, pal.mode
        ));
    left.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(16.0),
        gap: Some(8.0),
        corner_radius: Some(12.0),
        bg_hex: Some(pal.background.clone()),
        fg_hex: Some(fg_main.clone()),
        border_role: Some(pal.accent.clone()),
        border_width: Some(2.0),
        opacity: Some(alpha),
        ..Default::default()
    };

    let mut right = DesignNode::new("omarchy-pane-right", DesignNodeKind::Card)
        .with_name("Secondary Tiled Pane")
        .with_text(format!(
            "Accent {} | Surface {}",
            pal.accent, pal.lighter_background
        ));
    right.style = DesignStyle {
        flex_grow: Some(1.0),
        padding: Some(16.0),
        gap: Some(8.0),
        corner_radius: Some(12.0),
        bg_hex: Some(pal.lighter_background.clone()),
        fg_hex: Some(fg_pane),
        border_role: Some(pal.muted.clone()),
        border_width: Some(1.0),
        opacity: Some(alpha),
        ..Default::default()
    };
    if tiling == WindowTilingEngine::HyprMacBsp {
        right.children.push(build_hyprmac_overlay_card(
            &pal.background,
            &fg_main,
            &pal.accent,
            &pal.accent,
            Some(alpha),
        ));
    }

    bsp_row.children.push(left);
    bsp_row.children.push(right);
    root.children.push(bsp_row);

    let resolved_theme = if pal.mode == "light" {
        ThemeMode::Light
    } else {
        mode.to_theme_mode()
    };
    let mut doc = DesignDocument::new(format!("Omarchy{}", to_ident_pascal(&pal.name)), root);
    doc.theme = resolved_theme;
    doc.seed_hex = Some(pal.accent);
    doc.surface_mode = SurfaceMode::Operate;
    doc.source = DesignSourceKind::Authored;
    Ok(doc)
}

fn to_ident_pascal(s: &str) -> String {
    crate::material3::to_pascal_case(s)
}

/// Generate a complete, WCAG-AA clean `DesignDocument` for any desktop ecosystem preset.
pub fn select_theme_preset(
    kind: DesktopThemeKind,
    mode: DesktopColorMode,
    omarchy_name: Option<&str>,
    transparency: f32,
) -> Result<DesignDocument, String> {
    select_theme_preset_with_tiling(kind, mode, None, omarchy_name, transparency)
}

/// Generate a `DesignDocument` for any desktop ecosystem + optional explicit `WindowTilingEngine`.
pub fn select_theme_preset_with_tiling(
    kind: DesktopThemeKind,
    mode: DesktopColorMode,
    tiling: Option<WindowTilingEngine>,
    omarchy_name: Option<&str>,
    transparency: f32,
) -> Result<DesignDocument, String> {
    let resolved_tiling = tiling.unwrap_or_else(|| kind.default_tiling());
    match kind {
        DesktopThemeKind::Material3Googlebook => {
            Ok(build_googlebook_doc(mode, resolved_tiling, transparency))
        }
        DesktopThemeKind::AppleLiquidGlass => Ok(build_apple_liquid_glass_doc(
            mode,
            resolved_tiling,
            transparency,
        )),
        DesktopThemeKind::FluentMica => {
            Ok(build_fluent_mica_doc(mode, resolved_tiling, transparency))
        }
        DesktopThemeKind::Omarchy => build_omarchy_doc(
            omarchy_name.unwrap_or("aphrody"),
            mode,
            resolved_tiling,
            transparency,
        ),
        DesktopThemeKind::ViceCitySunset => {
            let mut doc = build_omarchy_doc(
                omarchy_name.unwrap_or("synthwave84"),
                mode,
                resolved_tiling,
                transparency,
            )?;
            doc.name = "AphrodyComp_ViceCitySunset".to_owned();
            doc.seed_hex = Some("#FF2E88".to_owned());
            Ok(doc)
        }
    }
}

// ============================================================================
// WGSL Material Functions (Liquid Glass Refraction + Fluent Mica Noise + Gemini Glow)
// ============================================================================

/// Pure Naga-validated WGSL material functions appended to `wgsl` codegen outputs.
#[must_use]
pub fn wgsl_desktop_material_helpers(transparency: f32) -> String {
    let t = transparency.clamp(0.0, 1.0);
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "\n// Desktop optical materials (Liquid Glass refraction, Fluent Mica noise, Gemini edge glow)"
    );
    let _ = writeln!(out, "const DESKTOP_DEFAULT_TRANSPARENCY: f32 = {t:.3};");
    out.push_str(
        r#"
struct DesktopMaterialUniform {
    tint_rgba: vec4<f32>,
    specular_rgba: vec4<f32>,
    params: vec4<f32>, // x: transparency, y: blur_px, z: refraction_ior, w: noise_amp
};

fn desktop_rounded_box_sdf(p: vec2<f32>, half_extents: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p) - half_extents + vec2<f32>(radius, radius);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

fn liquid_glass_refraction_edge(
    uv: vec2<f32>,
    half_extents: vec2<f32>,
    corner_r: f32,
    transparency: f32,
    tint: vec4<f32>,
    specular: vec4<f32>,
) -> vec4<f32> {
    let p = uv * 2.0 - vec2<f32>(1.0, 1.0);
    let d = desktop_rounded_box_sdf(p, half_extents, corner_r);
    let inside = 1.0 - smoothstep(-0.02, 0.02, d);
    let rim = smoothstep(0.18, 0.0, abs(d)) * inside;
    let fresnel = pow(max(rim, 0.0001), 3.2) * (0.35 + 0.45 * transparency);
    let refracted_rgb = mix(tint.rgb, specular.rgb, clamp(fresnel, 0.0, 1.0));
    let alpha = clamp(mix(1.0, tint.a, transparency), 0.15, 1.0) * inside;
    return vec4<f32>(refracted_rgb, alpha);
}

fn fluent_mica_acrylic_noise(
    uv: vec2<f32>,
    base_tint: vec4<f32>,
    wallpaper_sample: vec3<f32>,
    noise_amp: f32,
    is_focused: f32,
) -> vec4<f32> {
    let grain = fract(sin(dot(uv * 512.0, vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
    let mica_rgb = mix(base_tint.rgb, wallpaper_sample, 0.18 * step(0.5, is_focused));
    let acrylic_rgb = clamp(mica_rgb + vec3<f32>(grain * noise_amp), vec3<f32>(0.0), vec3<f32>(1.0));
    let alpha = mix(1.0, base_tint.a, step(0.5, is_focused));
    return vec4<f32>(acrylic_rgb, alpha);
}

fn googlebook_gemini_edge_glow(uv: vec2<f32>, time_s: f32) -> vec4<f32> {
    let edge_band = smoothstep(0.68, 1.0, uv.y);
    let wave = 0.5 + 0.5 * sin(uv.x * 6.28318 + time_s * 2.0);
    let blue = vec3<f32>(0.26, 0.52, 0.96);
    let magenta = vec3<f32>(0.76, 0.34, 0.86);
    let coral = vec3<f32>(0.92, 0.32, 0.26);
    let gold = vec3<f32>(0.98, 0.75, 0.18);
    let c1 = mix(blue, magenta, smoothstep(0.0, 0.45, uv.x));
    let c2 = mix(coral, gold, smoothstep(0.45, 1.0, uv.x));
    let glow_rgb = mix(c1, c2, smoothstep(0.35, 0.65, uv.x)) * (0.75 + 0.25 * wave);
    return vec4<f32>(glow_rgb * edge_band, edge_band);
}

fn vice_city_sunset_glass(uv: vec2<f32>, time_s: f32, transparency: f32) -> vec4<f32> {
    let midnight = vec3<f32>(0.043, 0.027, 0.098);
    let neon_pink = vec3<f32>(1.000, 0.180, 0.533);
    let electric_cyan = vec3<f32>(0.000, 0.941, 1.000);
    let sunset_amber = vec3<f32>(1.000, 0.702, 0.278);
    let horizon = smoothstep(0.15, 0.88, uv.y);
    let sky = mix(mix(midnight, neon_pink, horizon * 0.65), sunset_amber, smoothstep(0.65, 1.0, uv.y) * 0.45);
    let pulse = 0.5 + 0.5 * sin(uv.x * 6.28318 - time_s * 1.5);
    let rim = smoothstep(0.86, 1.0, max(abs(uv.x * 2.0 - 1.0), abs(uv.y * 2.0 - 1.0)));
    let border = mix(neon_pink, electric_cyan, pulse);
    let rgb = mix(sky, border, rim * (0.45 + 0.35 * transparency));
    return vec4<f32>(rgb, clamp(mix(0.82, 0.96, transparency), 0.25, 1.0));
}
"#,
    );
    out
}

/// Standalone fullscreen fragment shader compatible with `wgpu_bridge::render_fragment_wgsl_to_png`.
#[must_use]
pub fn desktop_material_fragment_wgsl(
    kind: DesktopThemeKind,
    mode: DesktopColorMode,
    transparency: f32,
) -> String {
    let t = transparency.clamp(0.0, 1.0);
    let mode_flag = if mode.is_light() { 1.0_f32 } else { 0.0_f32 };
    let preset_id = match kind {
        DesktopThemeKind::Material3Googlebook => 0_u32,
        DesktopThemeKind::AppleLiquidGlass => 1_u32,
        DesktopThemeKind::FluentMica => 2_u32,
        DesktopThemeKind::Omarchy => 3_u32,
        DesktopThemeKind::ViceCitySunset => 4_u32,
    };
    let helpers = wgsl_desktop_material_helpers(t);
    format!(
        r#"struct Uniforms {{
    resolution_time_aspect: vec4<f32>,
}};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {{
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}};

{helpers}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {{
    var verts = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0)
    );
    let p = verts[vid];
    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = p * 0.5 + vec2<f32>(0.5, 0.5);
    return out;
}}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {{
    let time_s = u.resolution_time_aspect.z;
    let is_light = {mode_flag:.1};
    let base_dark = vec3<f32>(0.06, 0.08, 0.12);
    let base_light = vec3<f32>(0.90, 0.93, 0.98);
    let backdrop = mix(base_dark, base_light, is_light);
    if ({preset_id}u == 1u) {{
        let glass = liquid_glass_refraction_edge(
            in.uv,
            vec2<f32>(0.78, 0.72),
            0.14,
            {t:.3},
            vec4<f32>(mix(vec3<f32>(0.12, 0.17, 0.26), vec3<f32>(0.88, 0.93, 0.99), is_light), 0.78),
            vec4<f32>(1.0, 1.0, 1.0, 0.9)
        );
        return vec4<f32>(mix(backdrop, glass.rgb, glass.a), 1.0);
    }}
    if ({preset_id}u == 2u) {{
        let mica = fluent_mica_acrylic_noise(
            in.uv,
            vec4<f32>(mix(vec3<f32>(0.14, 0.16, 0.21), vec3<f32>(0.94, 0.95, 0.98), is_light), 0.85),
            vec3<f32>(0.18, 0.42, 0.72),
            0.035,
            1.0
        );
        return vec4<f32>(mix(backdrop, mica.rgb, mica.a), 1.0);
    }}
    if ({preset_id}u == 4u) {{
        let vc = vice_city_sunset_glass(in.uv, time_s, {t:.3});
        return vec4<f32>(mix(backdrop, vc.rgb, vc.a), 1.0);
    }}
    let glow = googlebook_gemini_edge_glow(in.uv, time_s);
    return vec4<f32>(mix(backdrop, glow.rgb, glow.a), 1.0);
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::audit_document;

    #[test]
    fn loads_all_omarchy_themes_and_passes_wcag_audit_across_all_ecosystems() {
        let themes = list_omarchy_themes();
        assert_eq!(themes.len(), 28);
        assert!(
            themes
                .iter()
                .any(|t| t.name == "aphrody" && t.uses_m3_brand)
        );

        for (kind, mode) in [
            (
                DesktopThemeKind::Material3Googlebook,
                DesktopColorMode::Dark,
            ),
            (
                DesktopThemeKind::Material3Googlebook,
                DesktopColorMode::Light,
            ),
            (DesktopThemeKind::AppleLiquidGlass, DesktopColorMode::Dark),
            (DesktopThemeKind::AppleLiquidGlass, DesktopColorMode::Light),
            (DesktopThemeKind::FluentMica, DesktopColorMode::Dark),
            (DesktopThemeKind::FluentMica, DesktopColorMode::Light),
            (DesktopThemeKind::FluentMica, DesktopColorMode::HighContrast),
            (DesktopThemeKind::Omarchy, DesktopColorMode::Dark),
            (DesktopThemeKind::ViceCitySunset, DesktopColorMode::Dark),
        ] {
            let doc = select_theme_preset(kind, mode, Some("tokyo-night"), 0.78).expect("preset");
            let report = audit_document(&doc);
            assert!(
                report.passed,
                "{kind:?} {mode:?} failed audit: {:?}",
                report.findings
            );
        }

        for theme in &themes {
            let doc = select_theme_preset(
                DesktopThemeKind::Omarchy,
                DesktopColorMode::Dark,
                Some(&theme.name),
                0.85,
            )
            .expect("omarchy preset");
            let report = audit_document(&doc);
            assert!(
                report.passed,
                "omarchy '{}' failed audit: {:?}",
                theme.name, report.findings
            );
        }
    }

    #[test]
    fn computes_deterministic_tiling_rects_for_all_four_engines() {
        // 1. HyprMac BSP Dwindle across 4 windows: zero overlap & exact inside work area
        let hypr = compute_tiling_layout(
            WindowTilingEngine::HyprMacBsp,
            1920,
            1080,
            4,
            &TilingOptions {
                scratchpad_open: true,
                workspace: 3,
                ..Default::default()
            },
        );
        assert_eq!(hypr.rects.len(), 4);
        assert_eq!(hypr.workspace_count, 10);
        assert_eq!(hypr.active_workspace, 3);
        assert!(hypr.scratchpad_rect.is_some());
        for r in &hypr.rects {
            assert!(r.w > 100.0 && r.h > 100.0);
            assert!(r.x >= hypr.work_area.x && r.y >= hypr.work_area.y);
            assert!(r.x + r.w <= hypr.work_area.x + hypr.work_area.w + 0.5);
            assert!(r.y + r.h <= hypr.work_area.y + hypr.work_area.h + 0.5);
        }

        // 2. Native Grid Snap margins toggle
        let with_m = compute_tiling_layout(
            WindowTilingEngine::NativeGridSnap,
            1280,
            800,
            4,
            &TilingOptions::default(),
        );
        let no_m = compute_tiling_layout(
            WindowTilingEngine::NativeGridSnap,
            1280,
            800,
            4,
            &TilingOptions {
                margins_enabled: false,
                ..Default::default()
            },
        );
        assert_eq!(with_m.variant, "quarters");
        assert!(no_m.rects[0].w > with_m.rects[0].w);
        assert!(with_m.preview_drop_zone.is_some());

        // 3. Fluent Snap Layouts 2 / 3 / 4 zones
        for (n, v) in [(2, "2_zone"), (3, "3_zone"), (4, "4_zone")] {
            let snap = compute_tiling_layout(
                WindowTilingEngine::FluentSnapLayout,
                1600,
                900,
                n,
                &TilingOptions::default(),
            );
            assert_eq!(snap.variant, v);
            assert_eq!(snap.rects.len(), n);
        }

        // 4. Android Freeform safe area clamping
        let droid = compute_tiling_layout(
            WindowTilingEngine::AndroidFreeform,
            1280,
            800,
            3,
            &TilingOptions::default(),
        );
        assert_eq!(droid.rects.len(), 3);
        assert!(droid.rects.iter().all(|r| r.floating));
    }
}
