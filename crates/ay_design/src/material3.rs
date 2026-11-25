//! Pure Kotlin (`androidx.compose.material3.tokens.*`) JSON Material 3 token engine.
//!
//! Single source of truth: `configs/design/material3.json` (compiled directly from
//! upstream `androidx/androidx` Kotlin token files). Zero hardcoded hex tables, zero
//! legacy TOML, and direct projection into Taffy layout rules, 64-byte WGPU surface
//! uniforms, CSS custom properties, and all UI codegen targets.

use std::{collections::BTreeMap, fmt::Write, sync::OnceLock};

use serde::{Deserialize, Serialize};

/// Pure Kotlin-derived `androidx.compose.material3` JSON embedded as single source of truth.
pub const MATERIAL3_KOTLIN_JSON: &str = include_str!("../data/design/material3.json");

pub const WCAG_AA_NORMAL: f64 = 4.5;
pub const WCAG_AA_LARGE: f64 = 3.0;
pub const WCAG_AAA_NORMAL: f64 = 7.0;
pub const TOUCH_TARGET_MIN_DP: f32 = 48.0;
pub const POINTER_TARGET_MIN_DP: f32 = 44.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
    NeuralObsidian,
    OledPitch,
}

impl ThemeMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::NeuralObsidian => "neural_obsidian",
            Self::OledPitch => "oled_pitch",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "neural_obsidian" | "neural-obsidian" | "obsidian" => Self::NeuralObsidian,
            "oled_pitch" | "oled-pitch" | "oled" => Self::OledPitch,
            _ => Self::Dark,
        }
    }

    #[must_use]
    pub const fn is_dark(self) -> bool {
        !matches!(self, Self::Light)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeScaleEntry {
    pub size_sp: f64,
    pub line_height_sp: f64,
    pub tracking_sp: f64,
    pub weight_token: String,
}

impl TypeScaleEntry {
    #[must_use]
    pub fn css_weight(&self) -> u16 {
        match self.weight_token.as_str() {
            "WeightBold" => 700,
            "WeightMedium" => 500,
            _ => 400,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Material3KotlinBundle {
    #[serde(rename = "namespace")]
    pub namespace: String,
    #[serde(rename = "tokensNamespace")]
    pub tokens_namespace: String,
    #[serde(rename = "nowInAndroidNamespace")]
    pub now_in_android_namespace: String,
    pub color_light_tokens: BTreeMap<String, String>,
    pub color_dark_tokens: BTreeMap<String, String>,
    pub palette_tokens: BTreeMap<String, String>,
    pub shape_tokens_dp: BTreeMap<String, f32>,
    pub elevation_tokens_dp: BTreeMap<String, f32>,
    pub type_scale_tokens: BTreeMap<String, TypeScaleEntry>,
    pub standard_motion_tokens: BTreeMap<String, f64>,
    pub expressive_motion_tokens: BTreeMap<String, f64>,
    #[serde(default)]
    pub now_in_android_theme_palette: BTreeMap<String, String>,
}

/// 64-byte (`align(16)`) GPU uniform matching `WgpuMaterialSurfaceUniform` in Rust, WGSL, and Bun.
/// Verified at compile time via `google/zerocopy` (`IntoBytes`/`FromBytes`/`Immutable`/`KnownLayout`)
/// for zero-padding, sound zero-copy byte projection under `#![forbid(unsafe_code)]`.
#[repr(C)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
)]
pub struct WgpuMaterialSurfaceUniform {
    pub bg_rgba: [f32; 4],
    pub fg_rgba: [f32; 4],
    pub border_rgba: [f32; 4],
    pub corner_radius_dp: f32,
    pub elevation_dp: f32,
    pub padding_h_dp: f32,
    pub padding_v_dp: f32,
}

impl WgpuMaterialSurfaceUniform {
    pub const SIZE_BYTES: usize = 64;
    pub const ALIGN_BYTES: usize = 16;

    /// Zero-copy 64-byte slice view backed by `google/zerocopy` (compile-time proven padding-free).
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        zerocopy::IntoBytes::as_bytes(self)
    }

    /// Sound zero-copy decode from an exact 64-byte little-endian GPU uniform buffer.
    #[inline]
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        <Self as zerocopy::FromBytes>::read_from_bytes(bytes).ok()
    }

    #[must_use]
    pub fn to_le_bytes(self) -> [u8; 64] {
        let mut out = [0u8; 64];
        let floats = [
            self.bg_rgba[0],
            self.bg_rgba[1],
            self.bg_rgba[2],
            self.bg_rgba[3],
            self.fg_rgba[0],
            self.fg_rgba[1],
            self.fg_rgba[2],
            self.fg_rgba[3],
            self.border_rgba[0],
            self.border_rgba[1],
            self.border_rgba[2],
            self.border_rgba[3],
            self.corner_radius_dp,
            self.elevation_dp,
            self.padding_h_dp,
            self.padding_v_dp,
        ];
        for (idx, val) in floats.iter().enumerate() {
            out[idx * 4..idx * 4 + 4].copy_from_slice(&val.to_le_bytes());
        }
        out
    }
}

/// Taffy-compatible layout metrics derived directly from Kotlin Material 3 density & shape tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaffyMaterialLayout {
    pub display: String,
    pub flex_direction: String,
    pub min_width_dp: f32,
    pub min_height_dp: f32,
    pub padding_h_dp: f32,
    pub padding_v_dp: f32,
    pub gap_dp: f32,
    pub corner_radius_dp: f32,
}

/// Resolved semantic scheme (`PascalCase` keys identical to Kotlin `ColorLightTokens` / `ColorDarkTokens`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedColorScheme {
    pub mode: ThemeMode,
    pub tokens: BTreeMap<String, String>,
}

impl ResolvedColorScheme {
    /// Resolve either a Kotlin `PascalCase` token (`SurfaceContainerLow`), a `snake_case` / `kebab-case`
    /// alias (`surface_container_low` / `surface-container-low`), or a friendly role (`panel`, `accent`).
    #[must_use]
    pub fn role_hex(&self, role: &str) -> Option<&str> {
        if let Some(exact) = self.tokens.get(role) {
            return Some(exact.as_str());
        }
        let key = canonical_kotlin_color_key(role);
        self.tokens.get(key.as_str()).map(String::as_str)
    }

    #[must_use]
    pub fn paired_on_role(bg_role: &str) -> &'static str {
        match canonical_kotlin_color_key(bg_role).as_str() {
            "Primary" => "OnPrimary",
            "PrimaryContainer" => "OnPrimaryContainer",
            "Secondary" => "OnSecondary",
            "SecondaryContainer" => "OnSecondaryContainer",
            "Tertiary" => "OnTertiary",
            "TertiaryContainer" => "OnTertiaryContainer",
            "Error" => "OnError",
            "ErrorContainer" => "OnErrorContainer",
            "InverseSurface" => "InverseOnSurface",
            "Background" => "OnBackground",
            _ => "OnSurface",
        }
    }

    #[must_use]
    pub fn resolve_rgba_f32(&self, role_or_hex: &str) -> [f32; 4] {
        let hex = self.role_hex(role_or_hex).unwrap_or(role_or_hex);
        let fallback = self
            .tokens
            .get("Surface")
            .and_then(|s| parse_hex_rgb(s))
            .unwrap_or([0, 0, 0]);
        let [r, g, b] = parse_hex_rgb(hex).unwrap_or(fallback);
        [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ]
    }
}

impl Material3KotlinBundle {
    pub fn from_json(raw: &str) -> Result<Self, String> {
        serde_json::from_str(raw).map_err(|e| format!("invalid Material 3 Kotlin JSON: {e}"))
    }

    /// Resolve an official or dark-derived `ResolvedColorScheme` purely from Kotlin `Color*Tokens` + `PaletteTokens`.
    #[must_use]
    pub fn color_scheme(
        &self,
        mode: ThemeMode,
        seed_override: Option<&str>,
    ) -> ResolvedColorScheme {
        let mut tokens = match mode {
            ThemeMode::Light => self.color_light_tokens.clone(),
            ThemeMode::Dark => self.color_dark_tokens.clone(),
            ThemeMode::NeuralObsidian => {
                let mut t = self.color_dark_tokens.clone();
                if let Some(n4) = self.palette_tokens.get("Neutral4").cloned() {
                    t.insert("Background".to_owned(), n4.clone());
                    t.insert("Surface".to_owned(), n4.clone());
                    t.insert("SurfaceDim".to_owned(), n4);
                }
                if let Some(n0) = self.palette_tokens.get("Neutral0").cloned() {
                    t.insert("SurfaceContainerLowest".to_owned(), n0);
                }
                if let Some(n6) = self.palette_tokens.get("Neutral6").cloned() {
                    t.insert("SurfaceContainerLow".to_owned(), n6);
                }
                if let Some(n10) = self.palette_tokens.get("Neutral10").cloned() {
                    t.insert("SurfaceContainer".to_owned(), n10);
                }
                if let Some(n12) = self.palette_tokens.get("Neutral12").cloned() {
                    t.insert("SurfaceContainerHigh".to_owned(), n12);
                }
                if let Some(n17) = self.palette_tokens.get("Neutral17").cloned() {
                    t.insert("SurfaceContainerHighest".to_owned(), n17);
                }
                t
            }
            ThemeMode::OledPitch => {
                let mut t = self.color_dark_tokens.clone();
                if let Some(black) = self.palette_tokens.get("Black").cloned() {
                    t.insert("Background".to_owned(), black.clone());
                    t.insert("Surface".to_owned(), black.clone());
                    t.insert("SurfaceDim".to_owned(), black.clone());
                    t.insert("SurfaceContainerLowest".to_owned(), black);
                }
                if let Some(n4) = self.palette_tokens.get("Neutral4").cloned() {
                    t.insert("SurfaceContainerLow".to_owned(), n4);
                }
                if let Some(n6) = self.palette_tokens.get("Neutral6").cloned() {
                    t.insert("SurfaceContainer".to_owned(), n6);
                }
                t
            }
        };

        if let Some(seed_hex) = seed_override
            && let Some(seed_rgb) = parse_hex_rgb(seed_hex)
        {
            let white = self
                .palette_tokens
                .get("White")
                .and_then(|h| parse_hex_rgb(h))
                .unwrap_or([255, 255, 255]);
            let black = self
                .palette_tokens
                .get("Black")
                .and_then(|h| parse_hex_rgb(h))
                .unwrap_or([0, 0, 0]);
            let dark_ink = self
                .palette_tokens
                .get("Neutral10")
                .and_then(|h| parse_hex_rgb(h))
                .unwrap_or(black);
            let (primary, container) = if mode.is_dark() {
                (
                    blend_rgb(seed_rgb, white, 0.35),
                    blend_rgb(seed_rgb, black, 0.35),
                )
            } else {
                (
                    blend_rgb(seed_rgb, black, 0.15),
                    blend_rgb(seed_rgb, white, 0.78),
                )
            };
            tokens.insert("Primary".to_owned(), format_hex_rgb(primary));
            tokens.insert(
                "OnPrimary".to_owned(),
                best_wcag_foreground(primary, white, dark_ink),
            );
            tokens.insert("PrimaryContainer".to_owned(), format_hex_rgb(container));
            tokens.insert(
                "OnPrimaryContainer".to_owned(),
                best_wcag_foreground(container, white, dark_ink),
            );
        }

        ResolvedColorScheme { mode, tokens }
    }

    #[must_use]
    pub fn shape_dp(&self, token: &str) -> Option<f32> {
        if let Some(v) = self.shape_tokens_dp.get(token) {
            return Some(*v);
        }
        let key = match token.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "none" | "corner_none" => "CornerNone",
            "xs" | "extra_small" | "corner_extra_small" => "CornerExtraSmall",
            "sm" | "small" | "corner_small" => "CornerSmall",
            "md" | "medium" | "corner_medium" => "CornerMedium",
            "lg" | "large" | "corner_large" => "CornerLarge",
            "lg_inc" | "large_increased" | "corner_large_increased" => "CornerLargeIncreased",
            "xl" | "extra_large" | "corner_extra_large" => "CornerExtraLarge",
            "xl_inc" | "extra_large_increased" | "corner_extra_large_increased" => {
                "CornerExtraLargeIncreased"
            }
            "xxl" | "extra_extra_large" | "corner_extra_extra_large" => "CornerExtraExtraLarge",
            "full" | "pill" | "corner_full" => "CornerFull",
            _ => return None,
        };
        self.shape_tokens_dp.get(key).copied()
    }

    #[must_use]
    pub fn elevation_dp(&self, level: &str) -> f32 {
        self.elevation_tokens_dp.get(level).copied().unwrap_or(0.0)
    }

    #[must_use]
    pub fn type_role(&self, role: &str) -> Option<&TypeScaleEntry> {
        if let Some(exact) = self.type_scale_tokens.get(role) {
            return Some(exact);
        }
        let pascal = to_pascal_case(role);
        self.type_scale_tokens.get(&pascal)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SurfaceUniformSpec<'a> {
    pub mode: ThemeMode,
    pub seed_override: Option<&'a str>,
    pub bg_role: &'a str,
    pub fg_role: Option<&'a str>,
    pub border_role: Option<&'a str>,
    pub shape_token: &'a str,
    pub elevation_level: &'a str,
    pub padding_h_dp: f32,
    pub padding_v_dp: f32,
}

impl Material3KotlinBundle {
    /// Build a 64-byte WGPU uniform directly from Kotlin Material 3 JSON tokens.
    #[must_use]
    pub fn surface_uniform(&self, spec: SurfaceUniformSpec<'_>) -> WgpuMaterialSurfaceUniform {
        let scheme = self.color_scheme(spec.mode, spec.seed_override);
        let fg = spec
            .fg_role
            .unwrap_or_else(|| ResolvedColorScheme::paired_on_role(spec.bg_role));
        let border = spec.border_role.unwrap_or("OutlineVariant");
        WgpuMaterialSurfaceUniform {
            bg_rgba: scheme.resolve_rgba_f32(spec.bg_role),
            fg_rgba: scheme.resolve_rgba_f32(fg),
            border_rgba: scheme.resolve_rgba_f32(border),
            corner_radius_dp: self.shape_dp(spec.shape_token).unwrap_or(0.0),
            elevation_dp: self.elevation_dp(spec.elevation_level),
            padding_h_dp: spec.padding_h_dp,
            padding_v_dp: spec.padding_v_dp,
        }
    }
}

impl Material3KotlinBundle {
    /// Build a Taffy layout profile for a Material 3 component kind from Kotlin shape/density tokens.
    #[must_use]
    pub fn taffy_layout_for_component(&self, component: &str) -> TaffyMaterialLayout {
        match component.trim().to_ascii_lowercase().as_str() {
            "button" | "filled_tonal_button" | "outlined_button" => TaffyMaterialLayout {
                display: "flex".to_owned(),
                flex_direction: "row".to_owned(),
                min_width_dp: 64.0,
                min_height_dp: POINTER_TARGET_MIN_DP,
                padding_h_dp: 20.0,
                padding_v_dp: 10.0,
                gap_dp: 8.0,
                corner_radius_dp: self.shape_dp("CornerFull").unwrap_or(9999.0),
            },
            "card" => TaffyMaterialLayout {
                display: "flex".to_owned(),
                flex_direction: "column".to_owned(),
                min_width_dp: 160.0,
                min_height_dp: 80.0,
                padding_h_dp: 16.0,
                padding_v_dp: 16.0,
                gap_dp: 12.0,
                corner_radius_dp: self.shape_dp("CornerLarge").unwrap_or(16.0),
            },
            "dialog" => TaffyMaterialLayout {
                display: "flex".to_owned(),
                flex_direction: "column".to_owned(),
                min_width_dp: 280.0,
                min_height_dp: 160.0,
                padding_h_dp: 24.0,
                padding_v_dp: 24.0,
                gap_dp: 16.0,
                corner_radius_dp: self.shape_dp("CornerExtraLarge").unwrap_or(28.0),
            },
            "navigation_rail" | "nav_rail" | "sidebar" => TaffyMaterialLayout {
                display: "flex".to_owned(),
                flex_direction: "column".to_owned(),
                min_width_dp: 80.0,
                min_height_dp: 320.0,
                padding_h_dp: 12.0,
                padding_v_dp: 12.0,
                gap_dp: 8.0,
                corner_radius_dp: self.shape_dp("CornerNone").unwrap_or(0.0),
            },
            "toolbar" | "top_app_bar" => TaffyMaterialLayout {
                display: "flex".to_owned(),
                flex_direction: "row".to_owned(),
                min_width_dp: 320.0,
                min_height_dp: TOUCH_TARGET_MIN_DP,
                padding_h_dp: 16.0,
                padding_v_dp: 8.0,
                gap_dp: 8.0,
                corner_radius_dp: self.shape_dp("CornerLargeIncreased").unwrap_or(20.0),
            },
            _ => TaffyMaterialLayout {
                display: "flex".to_owned(),
                flex_direction: "column".to_owned(),
                min_width_dp: 0.0,
                min_height_dp: 0.0,
                padding_h_dp: 16.0,
                padding_v_dp: 16.0,
                gap_dp: 12.0,
                corner_radius_dp: self.shape_dp("CornerMedium").unwrap_or(12.0),
            },
        }
    }

    /// Emit W3C CSS custom properties (`--md-sys-color-*`, `--md-sys-shape-*`, `--md-sys-elevation-*`,
    /// `--md-sys-typescale-*`, `--md-sys-motion-*`) straight from the Kotlin JSON tokens.
    #[must_use]
    pub fn to_css_variables(&self, mode: ThemeMode, seed_override: Option<&str>) -> String {
        let scheme = self.color_scheme(mode, seed_override);
        let mut out = String::with_capacity(3072);
        let _ = writeln!(
            out,
            "/* Generated purely from {} Kotlin JSON */",
            self.tokens_namespace
        );
        let _ = writeln!(out, ":root {{");
        for (name, hex) in &scheme.tokens {
            let _ = writeln!(out, "  --md-sys-color-{}: {hex};", pascal_to_kebab(name));
        }
        for (name, dp) in &self.shape_tokens_dp {
            let _ = writeln!(out, "  --md-sys-shape-{}: {dp}px;", pascal_to_kebab(name));
        }
        for (name, dp) in &self.elevation_tokens_dp {
            let _ = writeln!(
                out,
                "  --md-sys-elevation-{}: {dp}px;",
                pascal_to_kebab(name)
            );
        }
        for (role, spec) in &self.type_scale_tokens {
            let kebab = pascal_to_kebab(role);
            let _ = writeln!(
                out,
                "  --md-sys-typescale-{kebab}-size: {}px;",
                spec.size_sp
            );
            let _ = writeln!(
                out,
                "  --md-sys-typescale-{kebab}-line-height: {}px;",
                spec.line_height_sp
            );
            let _ = writeln!(
                out,
                "  --md-sys-typescale-{kebab}-weight: {};",
                spec.css_weight()
            );
        }
        for (name, val) in &self.expressive_motion_tokens {
            let _ = writeln!(
                out,
                "  --md-sys-motion-expressive-{}: {val};",
                pascal_to_kebab(name)
            );
        }
        out.push_str("}\n");
        out
    }

    /// Emit WGSL uniform struct & theme color constants straight from the Kotlin JSON tokens.
    #[must_use]
    pub fn to_wgsl_tokens(&self, mode: ThemeMode, seed_override: Option<&str>) -> String {
        let scheme = self.color_scheme(mode, seed_override);
        let mut out = String::with_capacity(2048);
        let _ = writeln!(
            out,
            "// Generated purely from {} Kotlin JSON ({} mode)",
            self.tokens_namespace,
            mode.as_str()
        );
        out.push_str(
            "struct MaterialSurfaceUniform {\n\
             \x20   bg_rgba: vec4<f32>,\n\
             \x20   fg_rgba: vec4<f32>,\n\
             \x20   border_rgba: vec4<f32>,\n\
             \x20   metrics: vec4<f32>, // corner_radius_dp, elevation_dp, padding_h_dp, padding_v_dp\n\
             };\n\n",
        );
        for key in [
            "Surface",
            "SurfaceContainerLow",
            "SurfaceContainer",
            "SurfaceContainerHigh",
            "OnSurface",
            "Primary",
            "OnPrimary",
            "SecondaryContainer",
            "OnSecondaryContainer",
            "OutlineVariant",
            "Error",
        ] {
            let c = scheme.resolve_rgba_f32(key);
            let const_name = pascal_to_screaming_snake(key);
            let _ = writeln!(
                out,
                "const MD_SYS_COLOR_{const_name}: vec4<f32> = vec4<f32>({:.4}, {:.4}, {:.4}, {:.4});",
                c[0], c[1], c[2], c[3]
            );
        }
        out
    }
}

/// Cached pure Kotlin Material 3 token bundle loaded once from `configs/design/material3.json`.
#[must_use]
pub fn m3_bundle() -> &'static Material3KotlinBundle {
    static BUNDLE: OnceLock<Material3KotlinBundle> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        Material3KotlinBundle::from_json(MATERIAL3_KOTLIN_JSON)
            .expect("valid pure Kotlin configs/design/material3.json")
    })
}

#[must_use]
pub fn canonical_kotlin_color_key(role: &str) -> String {
    match role.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "bg" | "background" => "Background".to_owned(),
        "on_background" => "OnBackground".to_owned(),
        "surface" => "Surface".to_owned(),
        "surface_dim" => "SurfaceDim".to_owned(),
        "surface_bright" => "SurfaceBright".to_owned(),
        "surface_container_lowest" => "SurfaceContainerLowest".to_owned(),
        "surface_container_low" | "card" => "SurfaceContainerLow".to_owned(),
        "surface_container" | "panel" | "toolbar" => "SurfaceContainer".to_owned(),
        "surface_container_high" | "dialog" => "SurfaceContainerHigh".to_owned(),
        "surface_container_highest" => "SurfaceContainerHighest".to_owned(),
        "on_surface" | "text" | "fg" => "OnSurface".to_owned(),
        "on_surface_variant" | "muted" => "OnSurfaceVariant".to_owned(),
        "outline" => "Outline".to_owned(),
        "outline_variant" | "border" => "OutlineVariant".to_owned(),
        "primary" | "accent" => "Primary".to_owned(),
        "on_primary" => "OnPrimary".to_owned(),
        "primary_container" => "PrimaryContainer".to_owned(),
        "on_primary_container" => "OnPrimaryContainer".to_owned(),
        "secondary" => "Secondary".to_owned(),
        "on_secondary" => "OnSecondary".to_owned(),
        "secondary_container" => "SecondaryContainer".to_owned(),
        "on_secondary_container" => "OnSecondaryContainer".to_owned(),
        "tertiary" => "Tertiary".to_owned(),
        "on_tertiary" => "OnTertiary".to_owned(),
        "tertiary_container" => "TertiaryContainer".to_owned(),
        "on_tertiary_container" => "OnTertiaryContainer".to_owned(),
        "error" | "danger" => "Error".to_owned(),
        "on_error" => "OnError".to_owned(),
        "error_container" => "ErrorContainer".to_owned(),
        "on_error_container" => "OnErrorContainer".to_owned(),
        other => to_pascal_case(other),
    }
}

#[must_use]
pub fn to_pascal_case(raw: &str) -> String {
    raw.split(['_', '-', ' '])
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.push_str(chars.as_str());
                    s
                }
                None => String::new(),
            }
        })
        .collect()
}

#[must_use]
pub fn pascal_to_kebab(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for (i, ch) in raw.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

#[must_use]
pub fn pascal_to_screaming_snake(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for (i, ch) in raw.chars().enumerate() {
        if ch.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

#[must_use]
pub fn parse_hex_rgb(hex: &str) -> Option<[u8; 3]> {
    let clean = hex.trim().trim_start_matches('#');
    if clean.len() == 3 {
        let r = u8::from_str_radix(&clean[0..1], 16).ok()? * 17;
        let g = u8::from_str_radix(&clean[1..2], 16).ok()? * 17;
        let b = u8::from_str_radix(&clean[2..3], 16).ok()? * 17;
        return Some([r, g, b]);
    }
    if clean.len() >= 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
        let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
        let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
        return Some([r, g, b]);
    }
    None
}

#[must_use]
pub fn format_hex_rgb([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

#[must_use]
pub fn blend_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

#[must_use]
pub fn relative_luminance([r, g, b]: [u8; 3]) -> f64 {
    fn channel(c: u8) -> f64 {
        let s = f64::from(c) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

#[must_use]
pub fn wcag_contrast_ratio(fg: [u8; 3], bg: [u8; 3]) -> f64 {
    let l1 = relative_luminance(fg);
    let l2 = relative_luminance(bg);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[must_use]
pub fn best_wcag_foreground(bg: [u8; 3], light_ink: [u8; 3], dark_ink: [u8; 3]) -> String {
    if wcag_contrast_ratio(light_ink, bg) >= wcag_contrast_ratio(dark_ink, bg) {
        format_hex_rgb(light_ink)
    } else {
        format_hex_rgb(dark_ink)
    }
}

#[must_use]
pub const fn window_size_class(width_px: u32) -> &'static str {
    if width_px < 600 {
        "compact"
    } else if width_px < 840 {
        "medium"
    } else if width_px < 1200 {
        "expanded"
    } else if width_px < 1600 {
        "large"
    } else {
        "extra_large"
    }
}

#[must_use]
pub fn nested_corner_radius(outer_radius_px: f32, parent_padding_px: f32) -> f32 {
    (outer_radius_px - parent_padding_px).max(0.0)
}
