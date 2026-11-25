//! Pluggable, configurable, and stylable design system engine.
//!
//! Replaces hardcoded Material 3 assumptions with a first-class [`DesignSystemConfig`]
//! that supports any design language (`Astryx`, `Material3`, `AppleLiquidGlass`,
//! `FluentMica`, `ViceCitySunset`, `Omarchy`, `Minimal`, or fully `Custom`),
//! custom CSS variable prefixes (`--ax-`, `--md-sys-`, `--aphrody-`, etc.),
//! runtime token overrides, typography/radius/spacing/elevation/motion scales,
//! and direct projection to CSS custom properties, WGSL, Taffy, and 64-byte WGPU uniforms.

use std::{collections::BTreeMap, fmt::Write};

use serde::{Deserialize, Serialize};

use crate::{
    material3::{
        ResolvedColorScheme, SurfaceUniformSpec, ThemeMode, WgpuMaterialSurfaceUniform,
        best_wcag_foreground, m3_bundle, parse_hex_rgb, pascal_to_kebab,
    },
    themes::load_omarchy_theme,
};

/// Built-in or custom design system preset.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DesignSystemPreset {
    /// Meta Astryx open-source customizable & agent-ready design system.
    #[default]
    Astryx,
    /// Google Material 3 Expressive (`androidx.compose.material3.tokens`).
    Material3,
    /// Apple Liquid Glass / macOS adaptive translucent system.
    AppleLiquidGlass,
    /// Microsoft Fluent 2 / Mica & Acrylic system.
    FluentMica,
    /// `aphrody-comp` signature Vice City Sunset palette.
    ViceCitySunset,
    /// Any of the 28 Omarchy desktop palettes (e.g. `"tokyo-night"`, `"catppuccin"`, `"aphrody"`).
    Omarchy(String),
    /// Minimal neutral Swiss-grid token baseline.
    Minimal,
    /// Fully user-defined custom design system.
    Custom(String),
}

impl DesignSystemPreset {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let norm = raw.trim().to_ascii_lowercase().replace('-', "_");
        match norm.as_str() {
            "astryx" | "meta_astryx" | "ax" => Self::Astryx,
            "material3" | "m3" | "material_3" | "material" => Self::Material3,
            "apple_liquid_glass" | "liquid_glass" | "apple" | "macos" => Self::AppleLiquidGlass,
            "fluent_mica" | "fluent" | "mica" | "winui" => Self::FluentMica,
            "vice_city_sunset" | "vice_city" | "vice" => Self::ViceCitySunset,
            "minimal" | "swiss" | "neutral" => Self::Minimal,
            other if other.starts_with("omarchy:") => {
                Self::Omarchy(other.trim_start_matches("omarchy:").to_owned())
            }
            other => Self::Custom(other.to_owned()),
        }
    }

    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Astryx => "astryx".to_owned(),
            Self::Material3 => "material3".to_owned(),
            Self::AppleLiquidGlass => "apple_liquid_glass".to_owned(),
            Self::FluentMica => "fluent_mica".to_owned(),
            Self::ViceCitySunset => "vice_city_sunset".to_owned(),
            Self::Omarchy(name) => format!("omarchy:{name}"),
            Self::Minimal => "minimal".to_owned(),
            Self::Custom(name) => name.clone(),
        }
    }
}

/// UI density scale multiplier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DensityScale {
    Compact,
    #[default]
    Comfortable,
    Spacious,
}

impl DensityScale {
    #[must_use]
    pub const fn factor(self) -> f32 {
        match self {
            Self::Compact => 0.85,
            Self::Comfortable => 1.0,
            Self::Spacious => 1.2,
        }
    }
}

/// Typography token configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypographyConfig {
    pub font_sans: String,
    pub font_display: String,
    pub font_mono: String,
    pub base_size_px: f32,
    pub line_height: f32,
}

impl Default for TypographyConfig {
    fn default() -> Self {
        Self {
            font_sans: "Inter, system-ui, -apple-system, sans-serif".to_owned(),
            font_display: "Inter, system-ui, -apple-system, sans-serif".to_owned(),
            font_mono: "JetBrains Mono, ui-monospace, monospace".to_owned(),
            base_size_px: 14.0,
            line_height: 1.5,
        }
    }
}

/// Motion / spring timing configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionConfig {
    pub fast_ms: u32,
    pub normal_ms: u32,
    pub slow_ms: u32,
    pub easing_standard: String,
    pub easing_emphasized: String,
    pub reduced_motion: bool,
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self {
            fast_ms: 120,
            normal_ms: 220,
            slow_ms: 360,
            easing_standard: "cubic-bezier(0.2, 0, 0, 1)".to_owned(),
            easing_emphasized: "cubic-bezier(0.22, 1, 0.36, 1)".to_owned(),
            reduced_motion: false,
        }
    }
}

/// Fully configurable, stylable design system specification.
///
/// Nothing is hardcoded to Material 3: callers can pick any [`DesignSystemPreset`],
/// override any semantic color, radius, spacing, elevation, typography, motion, or
/// CSS variable prefix (`--ax-`, `--ds-`, `--md-sys-`, etc.), or load directly from JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignSystemConfig {
    pub name: String,
    pub preset: DesignSystemPreset,
    pub mode: ThemeMode,
    pub css_prefix: String,
    pub density: DensityScale,
    /// Semantic color tokens (e.g. `Primary`, `Surface`, `OnSurface`, `Outline`, `Accent`, ...).
    pub colors: BTreeMap<String, String>,
    /// Corner radius scale in dp/px (e.g. `none`, `xs`, `sm`, `md`, `lg`, `xl`, `full`).
    pub radii_dp: BTreeMap<String, f32>,
    /// Spacing scale in dp/px (e.g. `1`, `2`, `3`, `4`, `6`, `8`).
    pub spacing_dp: BTreeMap<String, f32>,
    /// Elevation levels in dp (e.g. `0`, `1`, `2`, `3`, `4`, `5`).
    pub elevation_dp: BTreeMap<String, f32>,
    pub typography: TypographyConfig,
    pub motion: MotionConfig,
    /// Arbitrary extra custom tokens (e.g. blur, glass opacity, border width).
    #[serde(default)]
    pub custom_tokens: BTreeMap<String, String>,
}

impl Default for DesignSystemConfig {
    fn default() -> Self {
        Self::from_preset(DesignSystemPreset::Astryx, ThemeMode::Dark)
    }
}

impl DesignSystemConfig {
    /// Construct a configurable design system from a built-in preset and theme mode.
    #[must_use]
    pub fn from_preset(preset: DesignSystemPreset, mode: ThemeMode) -> Self {
        let mut radii_dp = BTreeMap::from([
            ("none".to_owned(), 0.0),
            ("xs".to_owned(), 4.0),
            ("sm".to_owned(), 8.0),
            ("md".to_owned(), 12.0),
            ("lg".to_owned(), 16.0),
            ("xl".to_owned(), 24.0),
            ("full".to_owned(), 9999.0),
        ]);
        let spacing_dp = BTreeMap::from([
            ("1".to_owned(), 4.0),
            ("2".to_owned(), 8.0),
            ("3".to_owned(), 12.0),
            ("4".to_owned(), 16.0),
            ("5".to_owned(), 20.0),
            ("6".to_owned(), 24.0),
            ("8".to_owned(), 32.0),
            ("12".to_owned(), 48.0),
        ]);
        let elevation_dp = BTreeMap::from([
            ("Level0".to_owned(), 0.0),
            ("Level1".to_owned(), 1.0),
            ("Level2".to_owned(), 3.0),
            ("Level3".to_owned(), 6.0),
            ("Level4".to_owned(), 8.0),
            ("Level5".to_owned(), 12.0),
        ]);

        match &preset {
            DesignSystemPreset::Material3 => {
                let scheme = m3_bundle().color_scheme(mode, None);
                Self {
                    name: "Material 3".to_owned(),
                    preset,
                    mode,
                    css_prefix: "--md-sys-".to_owned(),
                    density: DensityScale::Comfortable,
                    colors: scheme.tokens,
                    radii_dp,
                    spacing_dp,
                    elevation_dp,
                    typography: TypographyConfig {
                        font_sans: "Roboto Flex, Google Sans Flex, system-ui, sans-serif"
                            .to_owned(),
                        font_display: "Google Sans Flex, Roboto Flex, system-ui, sans-serif"
                            .to_owned(),
                        font_mono: "JetBrains Mono, monospace".to_owned(),
                        base_size_px: 14.0,
                        line_height: 1.43,
                    },
                    motion: MotionConfig::default(),
                    custom_tokens: BTreeMap::new(),
                }
            }
            DesignSystemPreset::Astryx => {
                let colors = if mode.is_dark() {
                    BTreeMap::from([
                        ("Background".to_owned(), "#0B0D12".to_owned()),
                        ("OnBackground".to_owned(), "#F4F6FA".to_owned()),
                        ("Surface".to_owned(), "#11141C".to_owned()),
                        ("SurfaceContainerLow".to_owned(), "#161A24".to_owned()),
                        ("SurfaceContainer".to_owned(), "#1C212E".to_owned()),
                        ("SurfaceContainerHigh".to_owned(), "#242A3B".to_owned()),
                        ("OnSurface".to_owned(), "#F4F6FA".to_owned()),
                        ("OnSurfaceVariant".to_owned(), "#A6B0C3".to_owned()),
                        ("Primary".to_owned(), "#4F8CFF".to_owned()),
                        ("OnPrimary".to_owned(), "#061126".to_owned()),
                        ("PrimaryContainer".to_owned(), "#1A2E57".to_owned()),
                        ("OnPrimaryContainer".to_owned(), "#DCE8FF".to_owned()),
                        ("Secondary".to_owned(), "#7C6BFF".to_owned()),
                        ("OnSecondary".to_owned(), "#FFFFFF".to_owned()),
                        ("Tertiary".to_owned(), "#22D3EE".to_owned()),
                        ("OnTertiary".to_owned(), "#04232A".to_owned()),
                        ("Outline".to_owned(), "#394256".to_owned()),
                        ("OutlineVariant".to_owned(), "#262D3D".to_owned()),
                        ("Error".to_owned(), "#F87171".to_owned()),
                        ("OnError".to_owned(), "#2A0808".to_owned()),
                    ])
                } else {
                    BTreeMap::from([
                        ("Background".to_owned(), "#F8FAFC".to_owned()),
                        ("OnBackground".to_owned(), "#0F172A".to_owned()),
                        ("Surface".to_owned(), "#FFFFFF".to_owned()),
                        ("SurfaceContainerLow".to_owned(), "#F1F5F9".to_owned()),
                        ("SurfaceContainer".to_owned(), "#E2E8F0".to_owned()),
                        ("SurfaceContainerHigh".to_owned(), "#CBD5E1".to_owned()),
                        ("OnSurface".to_owned(), "#0F172A".to_owned()),
                        ("OnSurfaceVariant".to_owned(), "#475569".to_owned()),
                        ("Primary".to_owned(), "#2563EB".to_owned()),
                        ("OnPrimary".to_owned(), "#FFFFFF".to_owned()),
                        ("PrimaryContainer".to_owned(), "#DBEAFE".to_owned()),
                        ("OnPrimaryContainer".to_owned(), "#1E3A8A".to_owned()),
                        ("Secondary".to_owned(), "#6366F1".to_owned()),
                        ("OnSecondary".to_owned(), "#FFFFFF".to_owned()),
                        ("Tertiary".to_owned(), "#0891B2".to_owned()),
                        ("OnTertiary".to_owned(), "#FFFFFF".to_owned()),
                        ("Outline".to_owned(), "#94A3B8".to_owned()),
                        ("OutlineVariant".to_owned(), "#CBD5E1".to_owned()),
                        ("Error".to_owned(), "#DC2626".to_owned()),
                        ("OnError".to_owned(), "#FFFFFF".to_owned()),
                    ])
                };
                Self {
                    name: "Astryx".to_owned(),
                    preset,
                    mode,
                    css_prefix: "--ax-".to_owned(),
                    density: DensityScale::Comfortable,
                    colors,
                    radii_dp,
                    spacing_dp,
                    elevation_dp,
                    typography: TypographyConfig::default(),
                    motion: MotionConfig::default(),
                    custom_tokens: BTreeMap::from([
                        ("source".to_owned(), "meta/astryx".to_owned()),
                        ("agent-ready".to_owned(), "true".to_owned()),
                    ]),
                }
            }
            DesignSystemPreset::ViceCitySunset => {
                // Authoritative DESIGN.md — Vice City (Leonida x Aera M3) WCAG AAA palette.
                radii_dp.insert("md".to_owned(), 10.0);
                let colors = BTreeMap::from([
                    ("Background".to_owned(), "#141218".to_owned()),
                    ("OnBackground".to_owned(), "#FEF7FF".to_owned()),
                    ("Surface".to_owned(), "#141218".to_owned()),
                    ("SurfaceContainerLow".to_owned(), "#1D1B20".to_owned()),
                    ("SurfaceContainer".to_owned(), "#211F26".to_owned()),
                    ("SurfaceContainerHigh".to_owned(), "#2B2930".to_owned()),
                    ("OnSurface".to_owned(), "#FEF7FF".to_owned()),
                    ("OnSurfaceVariant".to_owned(), "#D0BCFF".to_owned()),
                    ("Primary".to_owned(), "#6750A4".to_owned()),
                    ("OnPrimary".to_owned(), "#FEF7FF".to_owned()),
                    ("PrimaryContainer".to_owned(), "#4F378B".to_owned()),
                    ("OnPrimaryContainer".to_owned(), "#EADDFF".to_owned()),
                    ("Secondary".to_owned(), "#EFB8C8".to_owned()),
                    ("OnSecondary".to_owned(), "#141218".to_owned()),
                    ("Tertiary".to_owned(), "#7D5260".to_owned()),
                    ("OnTertiary".to_owned(), "#FEF7FF".to_owned()),
                    ("Outline".to_owned(), "#7D5260".to_owned()),
                    ("OutlineVariant".to_owned(), "#49454F".to_owned()),
                    ("Error".to_owned(), "#F2B8B5".to_owned()),
                    ("OnError".to_owned(), "#601410".to_owned()),
                ]);
                let custom_tokens = BTreeMap::from([
                    ("bg".to_owned(), "#141218".to_owned()),
                    ("primary".to_owned(), "#6750A4".to_owned()),
                    ("primary-lt".to_owned(), "#D0BCFF".to_owned()),
                    ("rose".to_owned(), "#EFB8C8".to_owned()),
                    ("plum".to_owned(), "#7D5260".to_owned()),
                    ("halo".to_owned(), "#EADDFF".to_owned()),
                    ("ink".to_owned(), "#FEF7FF".to_owned()),
                ]);
                Self {
                    name: "Vice City (Leonida x Aera M3)".to_owned(),
                    preset,
                    mode,
                    css_prefix: "--vc-".to_owned(),
                    density: DensityScale::Comfortable,
                    colors,
                    radii_dp,
                    spacing_dp,
                    elevation_dp,
                    typography: TypographyConfig::default(),
                    motion: MotionConfig::default(),
                    custom_tokens,
                }
            }
            DesignSystemPreset::AppleLiquidGlass => {
                radii_dp.insert("md".to_owned(), 14.0);
                radii_dp.insert("lg".to_owned(), 20.0);
                let mut base = Self::from_preset(DesignSystemPreset::Astryx, mode);
                base.name = "Apple Liquid Glass".to_owned();
                base.preset = preset;
                base.css_prefix = "--glass-".to_owned();
                base.radii_dp = radii_dp;
                base.custom_tokens
                    .insert("backdrop-blur".to_owned(), "24px".to_owned());
                base.custom_tokens
                    .insert("surface-alpha".to_owned(), "0.72".to_owned());
                base
            }
            DesignSystemPreset::FluentMica => {
                radii_dp.insert("md".to_owned(), 8.0);
                radii_dp.insert("lg".to_owned(), 12.0);
                let mut base = Self::from_preset(DesignSystemPreset::Astryx, mode);
                base.name = "Fluent Mica".to_owned();
                base.preset = preset;
                base.css_prefix = "--fluent-".to_owned();
                base.radii_dp = radii_dp;
                base.custom_tokens
                    .insert("mica-tint-opacity".to_owned(), "0.80".to_owned());
                base
            }
            DesignSystemPreset::Omarchy(theme_name) => {
                let mut base = Self::from_preset(DesignSystemPreset::Astryx, mode);
                base.name = format!("Omarchy ({theme_name})");
                base.css_prefix = "--omarchy-".to_owned();
                if let Ok(palette) = load_omarchy_theme(theme_name) {
                    base.colors
                        .insert("Background".to_owned(), palette.background.clone());
                    base.colors
                        .insert("Surface".to_owned(), palette.background.clone());
                    base.colors
                        .insert("OnBackground".to_owned(), palette.foreground.clone());
                    base.colors
                        .insert("OnSurface".to_owned(), palette.foreground.clone());
                    base.colors
                        .insert("Primary".to_owned(), palette.accent.clone());
                    if let Some(bg) = parse_hex_rgb(&palette.accent) {
                        base.colors.insert(
                            "OnPrimary".to_owned(),
                            best_wcag_foreground(bg, [255, 255, 255], [15, 23, 42]),
                        );
                    }
                }
                base.preset = preset;
                base
            }
            DesignSystemPreset::Minimal | DesignSystemPreset::Custom(_) => {
                let mut base = Self::from_preset(DesignSystemPreset::Astryx, mode);
                base.name = match &preset {
                    DesignSystemPreset::Minimal => "Minimal".to_owned(),
                    DesignSystemPreset::Custom(name) => name.clone(),
                    _ => unreachable!(),
                };
                base.css_prefix = "--ds-".to_owned();
                base.preset = preset;
                base
            }
        }
    }

    /// Override a semantic color token (chainable builder).
    #[must_use]
    pub fn with_color(mut self, role: impl Into<String>, hex: impl Into<String>) -> Self {
        self.colors.insert(role.into(), hex.into());
        self
    }

    /// Override a corner radius token in dp (chainable builder).
    #[must_use]
    pub fn with_radius(mut self, name: impl Into<String>, dp: f32) -> Self {
        self.radii_dp.insert(name.into(), dp);
        self
    }

    /// Override a spacing token in dp (chainable builder).
    #[must_use]
    pub fn with_spacing(mut self, name: impl Into<String>, dp: f32) -> Self {
        self.spacing_dp.insert(name.into(), dp);
        self
    }

    /// Override the CSS custom property prefix (e.g. `"--brand-"`).
    #[must_use]
    pub fn with_css_prefix(mut self, prefix: impl Into<String>) -> Self {
        let mut p = prefix.into();
        if !p.starts_with("--") {
            p = format!("--{p}");
        }
        if !p.ends_with('-') {
            p.push('-');
        }
        self.css_prefix = p;
        self
    }

    /// Set UI density scale.
    #[must_use]
    pub const fn with_density(mut self, density: DensityScale) -> Self {
        self.density = density;
        self
    }

    /// Override the primary sans-serif font family.
    #[must_use]
    pub fn with_font_sans(mut self, family: impl Into<String>) -> Self {
        self.typography.font_sans = family.into();
        self
    }

    /// Add an arbitrary custom token.
    #[must_use]
    pub fn with_custom_token(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.custom_tokens.insert(key.into(), value.into());
        self
    }

    /// Parse a custom design system configuration from JSON.
    pub fn from_json(json_str: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json_str)
    }

    /// Serialize this design system configuration to pretty JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Convert to a [`ResolvedColorScheme`] compatible with all existing IR/WGPU/audit code.
    #[must_use]
    pub fn to_color_scheme(&self) -> ResolvedColorScheme {
        ResolvedColorScheme {
            mode: self.mode,
            tokens: self.colors.clone(),
        }
    }

    /// Pack a 64-byte WGPU surface uniform using this configurable design system's tokens.
    #[must_use]
    pub fn surface_uniform(&self, spec: SurfaceUniformSpec<'_>) -> WgpuMaterialSurfaceUniform {
        let scheme = self.to_color_scheme();
        let bg_rgba = scheme.resolve_rgba_f32(spec.bg_role);
        let fg_key = spec
            .fg_role
            .unwrap_or_else(|| ResolvedColorScheme::paired_on_role(spec.bg_role));
        let fg_rgba = scheme.resolve_rgba_f32(fg_key);
        let border_rgba = scheme.resolve_rgba_f32(spec.border_role.unwrap_or("OutlineVariant"));
        let factor = self.density.factor();
        let corner_radius_dp = self.radii_dp.get(spec.shape_token).copied().unwrap_or(12.0);
        let elevation_dp = self
            .elevation_dp
            .get(spec.elevation_level)
            .copied()
            .unwrap_or(0.0);

        WgpuMaterialSurfaceUniform {
            bg_rgba,
            fg_rgba,
            border_rgba,
            corner_radius_dp,
            elevation_dp,
            padding_h_dp: spec.padding_h_dp * factor,
            padding_v_dp: spec.padding_v_dp * factor,
        }
    }

    /// Emit CSS custom properties using this system's configurable `css_prefix`
    /// (zero hardcoded `--md-sys-` unless Material 3 preset is chosen).
    #[must_use]
    pub fn to_css_variables(&self) -> String {
        let mut out = String::with_capacity(2048);
        let p = &self.css_prefix;
        let _ = writeln!(
            out,
            "/* Generated by design-engine — {} ({:?}, prefix: {}) */",
            self.name, self.mode, p
        );
        let _ = writeln!(out, ":root {{");
        let _ = writeln!(
            out,
            "  color-scheme: {};",
            if self.mode.is_dark() { "dark" } else { "light" }
        );
        for (k, v) in &self.colors {
            let _ = writeln!(out, "  {p}color-{}: {v};", pascal_to_kebab(k));
        }
        for (k, v) in &self.radii_dp {
            let _ = writeln!(out, "  {p}radius-{k}: {v}px;");
        }
        let factor = self.density.factor();
        for (k, v) in &self.spacing_dp {
            let _ = writeln!(out, "  {p}space-{k}: {:.1}px;", v * factor);
        }
        for (k, v) in &self.elevation_dp {
            let _ = writeln!(out, "  {p}elevation-{}: {v}px;", pascal_to_kebab(k));
        }
        let _ = writeln!(out, "  {p}font-sans: {};", self.typography.font_sans);
        let _ = writeln!(out, "  {p}font-display: {};", self.typography.font_display);
        let _ = writeln!(out, "  {p}font-mono: {};", self.typography.font_mono);
        let _ = writeln!(
            out,
            "  {p}font-size-base: {}px;",
            self.typography.base_size_px
        );
        let _ = writeln!(out, "  {p}line-height: {};", self.typography.line_height);
        let fast_ms = if self.motion.reduced_motion {
            0
        } else {
            self.motion.fast_ms
        };
        let normal_ms = if self.motion.reduced_motion {
            0
        } else {
            self.motion.normal_ms
        };
        let slow_ms = if self.motion.reduced_motion {
            0
        } else {
            self.motion.slow_ms
        };
        let _ = writeln!(out, "  {p}motion-fast: {fast_ms}ms;");
        let _ = writeln!(out, "  {p}motion-normal: {normal_ms}ms;");
        let _ = writeln!(out, "  {p}motion-slow: {slow_ms}ms;");
        let _ = writeln!(
            out,
            "  {p}easing-standard: {};",
            self.motion.easing_standard
        );
        let _ = writeln!(
            out,
            "  {p}easing-emphasized: {};",
            self.motion.easing_emphasized
        );
        for (k, v) in &self.custom_tokens {
            let _ = writeln!(out, "  {p}{k}: {v};");
        }
        let _ = writeln!(out, "}}");
        out
    }

    /// Emit WGSL color & surface constants for this design system.
    #[must_use]
    pub fn to_wgsl_tokens(&self) -> String {
        let mut out = String::with_capacity(1024);
        let _ = writeln!(out, "// WGSL tokens for {} ({:?})", self.name, self.mode);
        for (k, hex) in &self.colors {
            if let Some([r, g, b]) = parse_hex_rgb(hex) {
                let const_name = pascal_to_kebab(k).replace('-', "_").to_ascii_uppercase();
                let _ = writeln!(
                    out,
                    "const DS_COLOR_{const_name}: vec4<f32> = vec4<f32>({:.4}, {:.4}, {:.4}, 1.0);",
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0,
                );
            }
        }
        out
    }
}
