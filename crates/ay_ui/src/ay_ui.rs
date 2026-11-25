use std::fmt::Write;

use design_engine::{DesignSystemConfig, DesignSystemPreset, ThemeMode};
use serde::{Deserialize, Serialize};

/// Structured fail-fast UI error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiError {
    pub code: &'static str,
    pub message: String,
    pub hint: &'static str,
}

impl std::fmt::Display for UiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {} ({})", self.code, self.message, self.hint)
    }
}

impl std::error::Error for UiError {}

/// Compile-time style specification produced by `ay_macros::tw!`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleSpec {
    pub flex: bool,
    pub column: bool,
    pub gap_dp: u16,
    pub padding_dp: u16,
    pub radius_token: &'static str,
    pub bg_role: &'static str,
    pub fg_role: &'static str,
}

impl Default for StyleSpec {
    fn default() -> Self {
        Self {
            flex: true,
            column: true,
            gap_dp: 12,
            padding_dp: 16,
            radius_token: "md",
            bg_role: "Surface",
            fg_role: "OnSurface",
        }
    }
}

/// Generational reactive signal bridging GPUI entities and SSR state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal<T> {
    value: T,
    version: u64,
}

impl<T> Signal<T> {
    pub const fn new(value: T) -> Self {
        Self { value, version: 1 }
    }

    pub const fn get(&self) -> &T {
        &self.value
    }

    pub const fn version(&self) -> u64 {
        self.version
    }

    pub fn set(&mut self, value: T) -> u64 {
        self.value = value;
        self.version = self.version.saturating_add(1);
        self.version
    }
}

/// Base UI / Astryx / Kumo headless node kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Section,
    Stack,
    Row,
    Card,
    Heading,
    Text,
    Button,
    Badge,
    Switch,
    Slider,
    DockPanel,
    Pet,
}

/// Cross-platform UI node rendered natively by GPUI or SSR HTML5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    pub id: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub aria_role: Option<String>,
    #[serde(default = "default_min_height")]
    pub min_height_dp: f32,
    #[serde(default)]
    pub children: Vec<UiNode>,
}

const fn default_min_height() -> f32 {
    44.0
}

impl UiNode {
    pub fn new(id: impl Into<String>, kind: NodeKind, label: impl Into<String>) -> Self {
        let aria_role = match kind {
            NodeKind::Button => Some("button".to_owned()),
            NodeKind::Switch => Some("switch".to_owned()),
            NodeKind::Slider => Some("slider".to_owned()),
            NodeKind::Section => Some("region".to_owned()),
            _ => None,
        };
        Self {
            id: id.into(),
            kind,
            label: label.into(),
            aria_role,
            min_height_dp: 44.0,
            children: Vec::new(),
        }
    }

    pub fn child(mut self, node: UiNode) -> Self {
        self.children.push(node);
        self
    }

    /// Enforce WCAG / Impeccable minimum pointer targets (>= 44dp) on interactive controls.
    pub fn validate(&self) -> Result<(), UiError> {
        let interactive = matches!(
            self.kind,
            NodeKind::Button | NodeKind::Switch | NodeKind::Slider
        );
        if interactive && self.min_height_dp < 44.0 {
            return Err(UiError {
                code: "UI_TARGET_TOO_SMALL",
                message: format!(
                    "control '{}' height {}dp < 44dp",
                    self.id, self.min_height_dp
                ),
                hint: "Raise min_height_dp to at least 44.0",
            });
        }
        if interactive && self.label.trim().is_empty() {
            return Err(UiError {
                code: "UI_MISSING_LABEL",
                message: format!(
                    "interactive control '{}' has an empty accessible label",
                    self.id
                ),
                hint: "Provide a concise user-facing label",
            });
        }
        for c in &self.children {
            c.validate()?;
        }
        Ok(())
    }
}

/// Real-time AG-UI 1.0 / Vercel json-render materializer (zero rebuild).
pub fn materialize_agui_json(raw: &str) -> Result<UiNode, UiError> {
    if raw.trim().is_empty() {
        return Err(UiError {
            code: "AGUI_EMPTY_PAYLOAD",
            message: "AG-UI JSON payload is empty".to_owned(),
            hint: "Pass a valid UiNode JSON object with id, kind, and label",
        });
    }
    let node: UiNode = serde_json::from_str(raw).map_err(|err| UiError {
        code: "AGUI_INVALID_JSON",
        message: err.to_string(),
        hint: "Provide valid AG-UI JSON matching UiNode schema",
    })?;
    node.validate()?;
    Ok(node)
}

/// Immediate-mode (`egui`-style) builder accumulating GPUI/SSR nodes directly.
#[derive(Debug, Default)]
pub struct ImmediateUi {
    nodes: Vec<UiNode>,
    seq: u32,
}

impl ImmediateUi {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.seq = self.seq.saturating_add(1);
        format!("{prefix}-{}", self.seq)
    }

    pub fn heading(&mut self, text: impl Into<String>) {
        let id = self.next_id("heading");
        self.nodes.push(UiNode::new(id, NodeKind::Heading, text));
    }

    pub fn button(&mut self, label: impl Into<String>) {
        let id = self.next_id("btn");
        self.nodes.push(UiNode::new(id, NodeKind::Button, label));
    }

    pub fn toggle(&mut self, label: impl Into<String>) {
        let id = self.next_id("toggle");
        self.nodes.push(UiNode::new(id, NodeKind::Switch, label));
    }

    pub fn slider(&mut self, label: impl Into<String>) {
        let id = self.next_id("slider");
        self.nodes.push(UiNode::new(id, NodeKind::Slider, label));
    }

    pub fn dock_panel(&mut self, title: impl Into<String>, children: Vec<UiNode>) {
        let id = self.next_id("dock");
        let mut panel = UiNode::new(id, NodeKind::DockPanel, title);
        panel.children = children;
        self.nodes.push(panel);
    }

    pub fn finish(self, root_id: impl Into<String>) -> UiNode {
        let mut root = UiNode::new(root_id, NodeKind::Stack, "");
        root.children = self.nodes;
        root
    }
}

/// OpenPets Rust mascots reacting live to workspace and agent events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PetKind {
    Bun,
    Tux,
    Androidify,
    Aphrody,
    Rust,
}

impl PetKind {
    pub const ALL: [Self; 5] = [
        Self::Bun,
        Self::Tux,
        Self::Androidify,
        Self::Aphrody,
        Self::Rust,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bun => "Bun",
            Self::Tux => "Tux",
            Self::Androidify => "Androidify",
            Self::Aphrody => "Aphrody",
            Self::Rust => "Rust",
        }
    }

    pub const fn accent_role(self) -> &'static str {
        match self {
            Self::Bun => "Secondary",
            Self::Tux => "OnSurface",
            Self::Androidify => "Tertiary",
            Self::Aphrody => "Primary",
            Self::Rust => "Error",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, UiError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bun" => Ok(Self::Bun),
            "tux" | "linux" => Ok(Self::Tux),
            "androidify" | "android" => Ok(Self::Androidify),
            "aphrody" => Ok(Self::Aphrody),
            "rust" | "ferris" => Ok(Self::Rust),
            other => Err(UiError {
                code: "PET_UNKNOWN",
                message: format!("unknown pet '{other}'"),
                hint: "Use one of: bun, tux, androidify, aphrody, rust",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PetMood {
    Idle,
    Running,
    Waving,
    Jumping,
    Celebrating,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PetSnapshot {
    pub kind: PetKind,
    pub mood: PetMood,
    pub accent_role: &'static str,
    pub status_text: String,
    pub svg: String,
}

pub fn render_pet(kind: PetKind, mood: PetMood, status_text: impl Into<String>) -> PetSnapshot {
    let status = status_text.into();
    let label = kind.name();
    let body_shape = match kind {
        PetKind::Bun => {
            r#"<ellipse cx="48" cy="54" rx="28" ry="22" fill="var(--ay-surface-high)" stroke="var(--ay-primary)" stroke-width="3"/><circle cx="38" cy="50" r="3.5" fill="var(--ay-fg)"/><circle cx="58" cy="50" r="3.5" fill="var(--ay-fg)"/><path d="M42 60q6 5 12 0" fill="none" stroke="var(--ay-primary)" stroke-width="2.5" stroke-linecap="round"/>"#
        }
        PetKind::Tux => {
            r#"<ellipse cx="48" cy="54" rx="24" ry="28" fill="var(--ay-fg)"/><ellipse cx="48" cy="58" rx="15" ry="18" fill="var(--ay-surface)"/><circle cx="41" cy="40" r="3" fill="var(--ay-surface)"/><circle cx="55" cy="40" r="3" fill="var(--ay-surface)"/><polygon points="48,44 43,50 53,50" fill="var(--ay-accent)"/>"#
        }
        PetKind::Androidify => {
            r#"<path d="M28 50a20 20 0 0 1 40 0v22a6 6 0 0 1-6 6H34a6 6 0 0 1-6-6Z" fill="var(--ay-tertiary)"/><circle cx="40" cy="44" r="3" fill="var(--ay-surface)"/><circle cx="56" cy="44" r="3" fill="var(--ay-surface)"/><path d="M36 26l-5-8M60 26l5-8" stroke="var(--ay-tertiary)" stroke-width="3" stroke-linecap="round"/>"#
        }
        PetKind::Aphrody => {
            r#"<circle cx="48" cy="48" r="28" fill="none" stroke="var(--ay-primary)" stroke-width="3"/><circle cx="48" cy="48" r="18" fill="var(--ay-primary-container)"/><path d="M36 52c4-12 20-12 24 0" fill="none" stroke="var(--ay-primary)" stroke-width="3" stroke-linecap="round"/><circle cx="42" cy="44" r="2.5" fill="var(--ay-fg)"/><circle cx="54" cy="44" r="2.5" fill="var(--ay-fg)"/>"#
        }
        PetKind::Rust => {
            r#"<ellipse cx="48" cy="54" rx="26" ry="18" fill="var(--ay-accent)"/><circle cx="40" cy="48" r="3.5" fill="var(--ay-surface)"/><circle cx="56" cy="48" r="3.5" fill="var(--ay-surface)"/><path d="M22 48l-8-8M74 48l8-8M26 64l-6 8M70 64l6 8" stroke="var(--ay-accent)" stroke-width="3.5" stroke-linecap="round"/>"#
        }
    };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 96 96" width="64" height="64" role="img" aria-label="{label} mascot">{body_shape}</svg>"#
    );
    PetSnapshot {
        kind,
        mood,
        accent_role: kind.accent_role(),
        status_text: status,
        svg,
    }
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_node_html(node: &UiNode, out: &mut String) {
    let label = escape_html(&node.label);
    let id = escape_html(&node.id);
    match node.kind {
        NodeKind::Section => {
            let _ = write!(out, r#"<section id="{id}" class="ay-section">"#);
            if !label.is_empty() {
                let _ = write!(out, "<h2>{label}</h2>");
            }
            for c in &node.children {
                render_node_html(c, out);
            }
            out.push_str("</section>");
        }
        NodeKind::Stack | NodeKind::DockPanel => {
            let _ = write!(out, r#"<div id="{id}" class="ay-stack">"#);
            if !label.is_empty() {
                let _ = write!(out, "<h3>{label}</h3>");
            }
            for c in &node.children {
                render_node_html(c, out);
            }
            out.push_str("</div>");
        }
        NodeKind::Row => {
            let _ = write!(out, r#"<div id="{id}" class="ay-row">"#);
            for c in &node.children {
                render_node_html(c, out);
            }
            out.push_str("</div>");
        }
        NodeKind::Card => {
            let _ = write!(out, r#"<article id="{id}" class="ay-card">"#);
            if !label.is_empty() {
                let _ = write!(out, "<h3>{label}</h3>");
            }
            for c in &node.children {
                render_node_html(c, out);
            }
            out.push_str("</article>");
        }
        NodeKind::Heading => {
            let _ = write!(out, r#"<h1 id="{id}">{label}</h1>"#);
        }
        NodeKind::Text => {
            let _ = write!(out, r#"<p id="{id}">{label}</p>"#);
        }
        NodeKind::Button => {
            let _ = write!(
                out,
                r#"<button id="{id}" type="button" class="ay-btn">{label}</button>"#
            );
        }
        NodeKind::Badge => {
            let _ = write!(out, r#"<span id="{id}" class="ay-badge">{label}</span>"#);
        }
        NodeKind::Switch => {
            let _ = write!(
                out,
                r#"<button id="{id}" type="button" role="switch" aria-checked="false" class="ay-btn">{label}</button>"#
            );
        }
        NodeKind::Slider => {
            let _ = write!(
                out,
                r#"<label class="ay-slider">{label}<input id="{id}" type="range" min="0" max="100" value="50"></label>"#
            );
        }
        NodeKind::Pet => {
            let pet = PetKind::parse(&node.label).unwrap_or(PetKind::Aphrody);
            let snap = render_pet(pet, PetMood::Waving, "");
            let _ = write!(
                out,
                r#"<figure id="{id}" class="ay-pet">{}</figure>"#,
                snap.svg
            );
        }
    }
}

/// Full semantic SSR HTML5 document readable by browsers and AI web agents (Gemini Web, Claude AI, Google AI Mode).
pub fn render_ssr_document(
    lang: &str,
    title: &str,
    description: &str,
    preset: DesignSystemPreset,
    root: &UiNode,
) -> String {
    let config = DesignSystemConfig::from_preset(preset, ThemeMode::Dark);
    let css_vars = config.to_css_variables();
    let esc_title = escape_html(title);
    let esc_desc = escape_html(description);
    let esc_lang = escape_html(lang);
    let mut body = String::with_capacity(1024);
    render_node_html(root, &mut body);

    format!(
        r#"<!doctype html>
<html lang="{esc_lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{esc_title}</title>
<meta name="description" content="{esc_desc}">
<meta property="og:title" content="{esc_title}">
<meta property="og:description" content="{esc_desc}">
<link rel="alternate" type="text/plain" href="/llms.txt" title="AI index">
<script type="application/ld+json">{{"@context":"https://schema.org","@type":"WebApplication","name":"{esc_title}","description":"{esc_desc}"}}</script>
<style>
{css_vars}
:root{{--ay-surface:var(--{pfx}surface,#141218);--ay-surface-high:var(--{pfx}surface-container-high,#2b2930);--ay-fg:var(--{pfx}on-surface,#e6e0e9);--ay-primary:var(--{pfx}primary,#d0bcff);--ay-primary-container:var(--{pfx}primary-container,#4f378b);--ay-tertiary:var(--{pfx}tertiary,#efb8c8);--ay-accent:var(--{pfx}secondary,#ccc2dc);}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--ay-surface);color:var(--ay-fg);font-family:system-ui,-apple-system,sans-serif;line-height:1.55}}
main{{max-inline-size:72rem;margin-inline:auto;padding:2rem 1.25rem}}.ay-stack{{display:flex;flex-direction:column;gap:1rem}}.ay-row{{display:flex;flex-wrap:wrap;gap:1rem;align-items:center}}
.ay-card{{background:var(--ay-surface-high);border-radius:1rem;padding:1.25rem}}.ay-btn{{min-block-size:44px;padding:0 1.125rem;border-radius:999px;border:1px solid var(--ay-primary);background:var(--ay-primary-container);color:var(--ay-fg);font:inherit;cursor:pointer;transition:transform 140ms ease}}.ay-btn:active{{transform:scale(.97)}}.ay-btn:focus-visible{{outline:2px solid var(--ay-primary);outline-offset:2px}}
@media(prefers-reduced-motion:reduce){{*{{transition:none!important;animation:none!important}}}}
</style>
</head>
<body>
<main>{body}</main>
</body>
</html>"#,
        pfx = config.css_prefix.trim_start_matches("--")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_increments_version_on_update() {
        let mut count = Signal::new(0u32);
        assert_eq!(*count.get(), 0);
        assert_eq!(count.version(), 1);
        assert_eq!(count.set(42), 2);
        assert_eq!(*count.get(), 42);
    }

    #[test]
    fn agui_materializer_rejects_undersized_button_and_accepts_valid_tree() {
        let bad = r#"{"id":"b1","kind":"button","label":"Save","min_height_dp":24.0}"#;
        let err = materialize_agui_json(bad).unwrap_err();
        assert_eq!(err.code, "UI_TARGET_TOO_SMALL");

        let good = r#"{"id":"b2","kind":"button","label":"Continue","min_height_dp":48.0}"#;
        let node = materialize_agui_json(good).unwrap();
        assert_eq!(node.label, "Continue");
    }

    #[test]
    fn ssr_and_openpets_emit_accessible_markup() {
        let mut ui = ImmediateUi::new();
        ui.heading("Welcome");
        ui.button("Start");
        let root = ui.finish("app");
        let html = render_ssr_document(
            "en",
            "Aphrody",
            "Creative workspace",
            DesignSystemPreset::Astryx,
            &root,
        );
        assert!(html.contains("application/ld+json"));
        assert!(html.contains("/llms.txt"));
        assert!(html.contains("<button"));

        for pet in PetKind::ALL {
            let snap = render_pet(pet, PetMood::Celebrating, "Ready");
            assert!(snap.svg.starts_with("<svg"));
        }
    }
}
