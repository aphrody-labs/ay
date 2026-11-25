//! Multi-target idiomatic code generators driven strictly from the canonical `DesignDocument` IR
//! and pure Kotlin `androidx.compose.material3` JSON tokens (`configs/design/material3.json`).
//!
//! Supported targets:
//! - `gpui`      (Rust GPUI)
//! - `rsx`       (Rust Dioxus RSX)
//! - `tsx`       (React / NSH / Base UI / Tailwind TSX)
//! - `ts` / `bun`(Native Bun `bun:aphrody` GPUI+WGPU+Dioxus)
//! - `iced`      (Rust Iced 0.14+)
//! - `bevy`      (Rust Bevy 0.20+ `bevy_ui` + `bevy_ui_widgets`)
//! - `egui`      (Rust Egui 0.34+ `egui::Panel` + `egui::Frame`)
//! - `taffy`     (Rust Taffy + 64-byte `WgpuMaterialSurfaceUniform` bindings)
//! - `wgsl`      (WGSL Material 3 token constants & surface uniform struct)
//! - `css`       (W3C CSS custom properties `--md-sys-*`)
//! - `design_md` (Full `DESIGN.md` specification + audit score)
//! - `figcraft`  (FigCraft JSON document)
//! - `swiftui`   (Apple SwiftUI + AppKit Liquid Glass, `NavigationSplitView`, `NSToolbarItemGroup`)

use std::fmt::Write;

use serde::{Deserialize, Serialize};

use crate::{
    audit::audit_document,
    ir::{DesignDocument, DesignNode, DesignNodeKind},
    material3::{
        ResolvedColorScheme, m3_bundle, parse_hex_rgb, pascal_to_kebab, to_pascal_case,
        window_size_class,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeTarget {
    Gpui,
    Rsx,
    Tsx,
    TsBun,
    Iced,
    Bevy,
    Egui,
    Taffy,
    Wgsl,
    Css,
    DesignMd,
    Figcraft,
    SwiftUi,
}

impl CodeTarget {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "gpui" | "rust_gpui" => Ok(Self::Gpui),
            "rsx" | "dioxus" | "dioxus_rsx" => Ok(Self::Rsx),
            "tsx" | "react" | "react_tsx" | "nsh" => Ok(Self::Tsx),
            "ts" | "bun" | "ts_bun" | "bun_ts" => Ok(Self::TsBun),
            "iced" | "rust_iced" => Ok(Self::Iced),
            "bevy" | "rust_bevy" | "bevy_ui" => Ok(Self::Bevy),
            "egui" | "rust_egui" => Ok(Self::Egui),
            "taffy" | "taffy_wgpu" | "wgpu" => Ok(Self::Taffy),
            "wgsl" => Ok(Self::Wgsl),
            "css" => Ok(Self::Css),
            "design_md" | "md" | "markdown" => Ok(Self::DesignMd),
            "figcraft" | "figcraft_json" | "json" => Ok(Self::Figcraft),
            "swiftui" | "swift" | "appkit" => Ok(Self::SwiftUi),
            other => Err(format!(
                "unsupported target '{other}'; expected one of: gpui, rsx, tsx, ts, iced, bevy, egui, taffy, wgsl, css, design_md, figcraft, swiftui"
            )),
        }
    }

    #[must_use]
    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::Gpui | Self::Rsx | Self::Iced | Self::Bevy | Self::Egui | Self::Taffy => "rs",
            Self::Tsx => "tsx",
            Self::TsBun => "ts",
            Self::Wgsl => "wgsl",
            Self::Css => "css",
            Self::DesignMd => "md",
            Self::Figcraft => "json",
            Self::SwiftUi => "swift",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeneratedArtifact {
    pub target: CodeTarget,
    pub extension: String,
    pub code: String,
}

#[must_use]
pub fn generate_code(doc: &DesignDocument, target: CodeTarget) -> GeneratedArtifact {
    let code = match target {
        CodeTarget::Gpui => emit_gpui(doc),
        CodeTarget::Rsx => emit_rsx(doc),
        CodeTarget::Tsx => emit_tsx(doc),
        CodeTarget::TsBun => emit_ts_bun(doc),
        CodeTarget::Iced => emit_iced(doc),
        CodeTarget::Bevy => emit_bevy(doc),
        CodeTarget::Egui => emit_egui(doc),
        CodeTarget::Taffy => emit_taffy_wgpu(doc),
        CodeTarget::Wgsl => {
            let mut wgsl = m3_bundle().to_wgsl_tokens(doc.theme, doc.seed_hex.as_deref());
            wgsl.push_str(&crate::themes::wgsl_desktop_material_helpers(0.78));
            wgsl
        }
        CodeTarget::Css => m3_bundle().to_css_variables(doc.theme, doc.seed_hex.as_deref()),
        CodeTarget::DesignMd => emit_design_md(doc),
        CodeTarget::Figcraft => serde_json::to_string_pretty(&doc.to_figcraft_json())
            .unwrap_or_else(|_| "{}".to_owned()),
        CodeTarget::SwiftUi => emit_swiftui(doc),
    };
    GeneratedArtifact {
        target,
        extension: target.file_extension().to_owned(),
        code,
    }
}

/// Generate code using a configurable [`crate::system::DesignSystemConfig`] instead of
/// hardcoded Material 3 tokens. Supports Astryx, Material 3, Apple Liquid Glass, Fluent Mica,
/// Vice City Sunset, Omarchy, Minimal, or any custom design system + CSS prefix.
#[must_use]
pub fn generate_code_with_system(
    doc: &DesignDocument,
    target: CodeTarget,
    system: &crate::system::DesignSystemConfig,
) -> GeneratedArtifact {
    let code = match target {
        CodeTarget::Css => system.to_css_variables(),
        CodeTarget::Wgsl => {
            let mut wgsl = system.to_wgsl_tokens();
            wgsl.push_str(&crate::themes::wgsl_desktop_material_helpers(0.78));
            wgsl
        }
        _ => generate_code(doc, target).code,
    };
    GeneratedArtifact {
        target,
        extension: target.file_extension().to_owned(),
        code,
    }
}

fn rust_ident(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let pascal = to_pascal_case(&out);
    if pascal.is_empty() || pascal.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("View{pascal}")
    } else {
        pascal
    }
}

fn hex_to_u32_literal(hex: &str) -> String {
    let [r, g, b] = parse_hex_rgb(hex).unwrap_or([0x14, 0x12, 0x18]);
    format!("0x{r:02X}{g:02X}{b:02X}")
}

fn hex_to_f32_tuple(hex: &str) -> (f32, f32, f32) {
    let [r, g, b] = parse_hex_rgb(hex).unwrap_or([0x14, 0x12, 0x18]);
    (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    )
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ============================================================================
// 1. Rust GPUI
// ============================================================================

fn emit_gpui(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let struct_name = rust_ident(&doc.name);
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(
        out,
        "use gpui::{{div, prelude::*, px, rgb, Context, IntoElement, Render, Window}};"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "pub struct {struct_name};");
    let _ = writeln!(out);
    let _ = writeln!(out, "impl Render for {struct_name} {{");
    let _ = writeln!(
        out,
        "    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {{"
    );
    if let Some(root) = doc.primary_root() {
        write_gpui_node(&mut out, root, &scheme, 2);
    } else {
        let _ = writeln!(out, "        div()");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
    out
}

fn write_gpui_node(
    out: &mut String,
    node: &DesignNode,
    scheme: &ResolvedColorScheme,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    let _ = writeln!(out, "{indent}div()");
    let _ = writeln!(out, "{indent}    .id(\"{}\")", escape_str(&node.id));
    let _ = writeln!(out, "{indent}    .flex()");
    let is_row = matches!(node.style.direction.as_deref(), Some("row"))
        || matches!(
            node.kind,
            DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
        );
    if is_row {
        let _ = writeln!(out, "{indent}    .flex_row()");
    } else {
        let _ = writeln!(out, "{indent}    .flex_col()");
    }
    if let Some(w) = node.style.width {
        let _ = writeln!(out, "{indent}    .w(px({w:.1}))");
    }
    if let Some(h) = node.style.height {
        let _ = writeln!(out, "{indent}    .h(px({h:.1}))");
    }
    if node.style.flex_grow.is_some_and(|g| g > 0.0) {
        let _ = writeln!(out, "{indent}    .flex_1()");
    }
    if let Some(p) = node.style.padding {
        let _ = writeln!(out, "{indent}    .p(px({p:.1}))");
    }
    if let Some(g) = node.style.gap {
        let _ = writeln!(out, "{indent}    .gap(px({g:.1}))");
    }
    let rad = node.resolved_radius();
    if rad > 0.0 {
        let _ = writeln!(out, "{indent}    .rounded(px({rad:.1}))");
    }
    if let Some(bg) = node.resolved_bg_hex(scheme) {
        let _ = writeln!(out, "{indent}    .bg(rgb({}))", hex_to_u32_literal(bg));
    }
    let fg = node.resolved_fg_hex(scheme);
    let _ = writeln!(
        out,
        "{indent}    .text_color(rgb({}))",
        hex_to_u32_literal(fg)
    );
    if let Some(br) = node
        .style
        .border_role
        .as_deref()
        .and_then(|r| scheme.role_hex(r))
    {
        let _ = writeln!(out, "{indent}    .border_1()");
        let _ = writeln!(
            out,
            "{indent}    .border_color(rgb({}))",
            hex_to_u32_literal(br)
        );
    }
    if let Some(txt) = &node.text {
        let _ = writeln!(out, "{indent}    .child(\"{}\")", escape_str(txt));
    }
    for child in &node.children {
        let _ = writeln!(out, "{indent}    .child(");
        write_gpui_node(out, child, scheme, depth + 2);
        let _ = writeln!(out, "{indent}    )");
    }
}

// ============================================================================
// 2. Rust Dioxus RSX
// ============================================================================

fn emit_rsx(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let fn_name = rust_ident(&doc.name);
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(out, "use dioxus::prelude::*;");
    let _ = writeln!(out);
    let _ = writeln!(out, "#[component]");
    let _ = writeln!(out, "pub fn {fn_name}() -> Element {{");
    let _ = writeln!(out, "    rsx! {{");
    if let Some(root) = doc.primary_root() {
        write_rsx_node(&mut out, root, &scheme, 2);
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
    out
}

fn css_var_or_hex(role: Option<&str>, fallback_hex: &str) -> String {
    match role {
        Some(r) => {
            let key = crate::material3::canonical_kotlin_color_key(r);
            format!(
                "var(--md-sys-color-{}, {fallback_hex})",
                pascal_to_kebab(&key)
            )
        }
        None => fallback_hex.to_owned(),
    }
}

fn node_inline_css(node: &DesignNode, scheme: &ResolvedColorScheme) -> String {
    let is_row = matches!(node.style.direction.as_deref(), Some("row"))
        || matches!(
            node.kind,
            DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
        );
    let mut css = format!(
        "display:{};flex-direction:{};",
        if node.kind == DesignNodeKind::Grid {
            "grid"
        } else {
            "flex"
        },
        if is_row { "row" } else { "column" }
    );
    if let Some(w) = node.style.width {
        let _ = write!(css, "width:{w}px;");
    }
    if let Some(h) = node.style.height {
        let _ = write!(css, "height:{h}px;");
    }
    if let Some(g) = node.style.flex_grow {
        let _ = write!(css, "flex-grow:{g};");
    }
    if let Some(p) = node.style.padding {
        let _ = write!(css, "padding:{p}px;");
    }
    if let Some(gap) = node.style.gap {
        let _ = write!(css, "gap:{gap}px;");
    }
    let rad = node.resolved_radius();
    if rad > 0.0 {
        let _ = write!(css, "border-radius:{rad}px;");
    }
    if let Some(bg_hex) = node.resolved_bg_hex(scheme) {
        let bg = css_var_or_hex(node.style.bg_role.as_deref(), bg_hex);
        let _ = write!(css, "background:{bg};");
    }
    let fg_hex = node.resolved_fg_hex(scheme);
    let fg = css_var_or_hex(node.style.fg_role.as_deref(), fg_hex);
    let _ = write!(css, "color:{fg};");
    if let Some(br_hex) = node
        .style
        .border_role
        .as_deref()
        .and_then(|r| scheme.role_hex(r))
    {
        let br = css_var_or_hex(node.style.border_role.as_deref(), br_hex);
        let _ = write!(css, "border:1px solid {br};");
    }
    css
}

fn html_tag(kind: &DesignNodeKind) -> &'static str {
    match kind {
        DesignNodeKind::Button => "button",
        DesignNodeKind::Input => "input",
        DesignNodeKind::Text | DesignNodeKind::Badge => "span",
        DesignNodeKind::Sidebar | DesignNodeKind::NavRail => "aside",
        DesignNodeKind::Toolbar | DesignNodeKind::TabBar => "nav",
        DesignNodeKind::Dialog => "dialog",
        DesignNodeKind::Separator => "hr",
        DesignNodeKind::Image => "img",
        DesignNodeKind::Video => "video",
        DesignNodeKind::ShaderCanvas => "canvas",
        _ => "div",
    }
}

fn write_rsx_node(out: &mut String, node: &DesignNode, scheme: &ResolvedColorScheme, depth: usize) {
    let indent = "    ".repeat(depth);
    let tag = html_tag(&node.kind);
    let css = node_inline_css(node, scheme);
    let _ = writeln!(out, "{indent}{tag} {{");
    let _ = writeln!(out, "{indent}    id: \"{}\",", escape_str(&node.id));
    if let Some(aria) = &node.a11y.aria_label {
        let _ = writeln!(out, "{indent}    aria_label: \"{}\",", escape_str(aria));
    }
    let _ = writeln!(out, "{indent}    style: \"{}\",", escape_str(&css));
    if let Some(txt) = &node.text {
        if tag == "input" {
            let _ = writeln!(out, "{indent}    placeholder: \"{}\",", escape_str(txt));
        } else {
            let _ = writeln!(out, "{indent}    \"{}\"", escape_str(txt));
        }
    }
    for child in &node.children {
        write_rsx_node(out, child, scheme, depth + 1);
    }
    let _ = writeln!(out, "{indent}}}");
}

// ============================================================================
// 3. React / NSH / Base UI TSX
// ============================================================================

fn emit_tsx(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let comp = rust_ident(&doc.name);
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(out, "import type {{ ReactNode }} from \"react\";");
    let _ = writeln!(out);
    let _ = writeln!(out, "export function {comp}(): ReactNode {{");
    let _ = writeln!(out, "  return (");
    if let Some(root) = doc.primary_root() {
        write_tsx_node(&mut out, root, &scheme, 2);
    }
    let _ = writeln!(out, "  );");
    let _ = writeln!(out, "}}");
    out
}

fn write_tsx_node(out: &mut String, node: &DesignNode, scheme: &ResolvedColorScheme, depth: usize) {
    let indent = "  ".repeat(depth);
    let tag = html_tag(&node.kind);
    let is_row = matches!(node.style.direction.as_deref(), Some("row"))
        || matches!(
            node.kind,
            DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
        );
    let mut style_entries = vec![
        format!(
            "display: \"{}\"",
            if node.kind == DesignNodeKind::Grid {
                "grid"
            } else {
                "flex"
            }
        ),
        format!(
            "flexDirection: \"{}\"",
            if is_row { "row" } else { "column" }
        ),
    ];
    if let Some(w) = node.style.width {
        style_entries.push(format!("width: {w}"));
    }
    if let Some(h) = node.style.height {
        style_entries.push(format!("height: {h}"));
    }
    if let Some(g) = node.style.flex_grow {
        style_entries.push(format!("flexGrow: {g}"));
    }
    if let Some(p) = node.style.padding {
        style_entries.push(format!("padding: {p}"));
    }
    if let Some(gap) = node.style.gap {
        style_entries.push(format!("gap: {gap}"));
    }
    let rad = node.resolved_radius();
    if rad > 0.0 {
        style_entries.push(format!("borderRadius: {rad}"));
    }
    if let Some(bg_hex) = node.resolved_bg_hex(scheme) {
        let bg = css_var_or_hex(node.style.bg_role.as_deref(), bg_hex);
        style_entries.push(format!("backgroundColor: \"{bg}\""));
    }
    let fg_hex = node.resolved_fg_hex(scheme);
    let fg = css_var_or_hex(node.style.fg_role.as_deref(), fg_hex);
    style_entries.push(format!("color: \"{fg}\""));

    let aria = node
        .a11y
        .aria_label
        .as_ref()
        .map(|l| format!(" aria-label=\"{}\"", escape_str(l)))
        .unwrap_or_default();
    let style_attr = format!("style={{{{ {} }}}}", style_entries.join(", "));
    if tag == "input" || tag == "hr" || tag == "img" {
        let ph = node
            .text
            .as_ref()
            .filter(|_| tag == "input")
            .map(|t| format!(" placeholder=\"{}\"", escape_str(t)))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{indent}<{tag} id=\"{}\"{aria}{ph} {style_attr} />",
            escape_str(&node.id)
        );
        return;
    }

    let _ = writeln!(
        out,
        "{indent}<{tag} id=\"{}\"{aria} {style_attr}>",
        escape_str(&node.id)
    );
    if let Some(txt) = &node.text {
        let _ = writeln!(out, "{indent}  {txt}");
    }
    for child in &node.children {
        write_tsx_node(out, child, scheme, depth + 1);
    }
    let _ = writeln!(out, "{indent}</{tag}>");
}

// ============================================================================
// 4. Native Bun (`bun:aphrody`) TypeScript
// ============================================================================

fn emit_ts_bun(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(
        out,
        "import {{ GpuiApplication, renderDualTarget, rgb, ui, wgpu }} from \"bun:aphrody\";"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "export function buildTree() {{");
    if let Some(root) = doc.primary_root() {
        let _ = write!(out, "  return ");
        write_bun_node(&mut out, root, &scheme, 1);
        let _ = writeln!(out, ";");
    } else {
        let _ = writeln!(out, "  return ui.vFlex(\"root\");");
    }
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(out, "if (import.meta.main) {{");
    let _ = writeln!(
        out,
        "  using device = wgpu.requestDevice({{ powerPreference: \"high-performance\" }});"
    );
    let _ = writeln!(out, "  using app = new GpuiApplication();");
    let _ = writeln!(
        out,
        "  const win = app.openWindow({{ width: {}, height: {} }});",
        doc.viewport_width, doc.viewport_height
    );
    let _ = writeln!(
        out,
        "  const dual = renderDualTarget({{ title: \"{}\", window: win, device, tree: buildTree() }});",
        escape_str(&doc.name)
    );
    let _ = writeln!(
        out,
        "  console.log({{ quads: dual.layout.quad_count, pngBytes: dual.gpuPng.byteLength, htmlBytes: dual.ssrHtml.length }});"
    );
    let _ = writeln!(out, "}}");
    out
}

fn write_bun_node(out: &mut String, node: &DesignNode, scheme: &ResolvedColorScheme, depth: usize) {
    let indent = "  ".repeat(depth);
    let id = escape_str(&node.id);
    let ctor = match node.kind {
        DesignNodeKind::Row => format!("ui.hFlex(\"{id}\")"),
        DesignNodeKind::Toolbar => format!("ui.toolbar(\"{id}\")"),
        DesignNodeKind::Sidebar | DesignNodeKind::NavRail => {
            format!(
                "ui.sidebar(\"{id}\", {})",
                node.style.width.unwrap_or(220.0)
            )
        }
        DesignNodeKind::Card => format!("ui.card(\"{id}\")"),
        DesignNodeKind::Button => format!(
            "ui.button(\"{id}\", \"{}\")",
            escape_str(node.text.as_deref().unwrap_or(&node.name))
        ),
        DesignNodeKind::Input => format!(
            "ui.input(\"{id}\", \"{}\")",
            escape_str(node.text.as_deref().unwrap_or(""))
        ),
        DesignNodeKind::Badge => format!(
            "ui.badge(\"{id}\", \"{}\")",
            escape_str(node.text.as_deref().unwrap_or(&node.name))
        ),
        DesignNodeKind::Text => format!(
            "ui.text(\"{id}\", \"{}\")",
            escape_str(node.text.as_deref().unwrap_or(""))
        ),
        DesignNodeKind::ShaderCanvas => format!(
            "ui.shaderView(\"{id}\", {}, {})",
            node.style.width.unwrap_or(320.0),
            node.style.height.unwrap_or(180.0)
        ),
        _ => format!("ui.vFlex(\"{id}\")"),
    };
    let _ = write!(out, "{ctor}");
    if let (Some(w), Some(h)) = (node.style.width, node.style.height) {
        let _ = write!(out, ".size({w}, {h})");
    } else if let Some(w) = node.style.width {
        let _ = write!(out, ".w({w})");
    } else if let Some(h) = node.style.height {
        let _ = write!(out, ".h({h})");
    }
    if let Some(g) = node.style.flex_grow {
        let _ = write!(out, ".flexGrow({g})");
    }
    if let Some(p) = node.style.padding {
        let _ = write!(out, ".p({p})");
    }
    if let Some(gap) = node.style.gap {
        let _ = write!(out, ".gap({gap})");
    }
    let rad = node.resolved_radius();
    if rad > 0.0 {
        let _ = write!(out, ".rounded({rad})");
    }
    if let Some(bg) = node.resolved_bg_hex(scheme) {
        let _ = write!(out, ".bg(rgb({}))", hex_to_u32_literal(bg));
    }
    if let Some(act) = &node.a11y.action {
        let _ = write!(out, ".action(\"{}\")", escape_str(act));
    }
    for child in &node.children {
        let _ = write!(out, "\n{indent}  .child(");
        write_bun_node(out, child, scheme, depth + 2);
        let _ = write!(out, ")");
    }
}

// ============================================================================
// 5. Rust Iced 0.14+ (verified via Context7 /iced-rs/iced)
// ============================================================================

fn emit_iced(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let fn_name = rust_ident(&doc.name);
    let mut actions = Vec::new();
    if let Some(root) = doc.primary_root() {
        collect_actions(root, &mut actions);
    }
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine for Iced 0.14+ from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(
        out,
        "use iced::widget::{{button, column, container, row, text, text_input}};"
    );
    let _ = writeln!(
        out,
        "use iced::{{Background, Border, Color, Element, Length}};"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "#[derive(Debug, Clone)]");
    let _ = writeln!(out, "pub enum {fn_name}Message {{");
    let _ = writeln!(out, "    Noop,");
    for act in &actions {
        let _ = writeln!(out, "    {act},");
    }
    let _ = writeln!(out, "    InputChanged(String),");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(out, "pub fn view<'a>() -> Element<'a, {fn_name}Message> {{");
    if let Some(root) = doc.primary_root() {
        write_iced_node(&mut out, root, &scheme, &fn_name, 1);
    } else {
        let _ = writeln!(out, "    column![].into()");
    }
    let _ = writeln!(out, "}}");
    out
}

fn collect_actions(node: &DesignNode, actions: &mut Vec<String>) {
    if node.kind == DesignNodeKind::Button {
        let variant = rust_ident(node.a11y.action.as_deref().unwrap_or(&node.id));
        if !actions.contains(&variant) {
            actions.push(variant);
        }
    }
    for c in &node.children {
        collect_actions(c, actions);
    }
}

fn write_iced_node(
    out: &mut String,
    node: &DesignNode,
    scheme: &ResolvedColorScheme,
    msg_prefix: &str,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    match node.kind {
        DesignNodeKind::Text | DesignNodeKind::Badge => {
            let (r, g, b) = hex_to_f32_tuple(node.resolved_fg_hex(scheme));
            let label = escape_str(node.text.as_deref().unwrap_or(&node.name));
            let _ = writeln!(
                out,
                "{indent}text(\"{label}\").color(Color::from_rgb({r:.3}, {g:.3}, {b:.3})).into()"
            );
        }
        DesignNodeKind::Button => {
            let label = escape_str(node.text.as_deref().unwrap_or(&node.name));
            let variant = rust_ident(node.a11y.action.as_deref().unwrap_or(&node.id));
            let pad = node.style.padding.unwrap_or(10.0) as u16;
            let _ = writeln!(
                out,
                "{indent}button(text(\"{label}\")).padding({pad}).on_press({msg_prefix}Message::{variant}).into()"
            );
        }
        DesignNodeKind::Input => {
            let ph = escape_str(node.text.as_deref().unwrap_or("Input"));
            let _ = writeln!(
                out,
                "{indent}text_input(\"{ph}\", \"\").on_input({msg_prefix}Message::InputChanged).into()"
            );
        }
        _ => {
            let is_row = matches!(node.style.direction.as_deref(), Some("row"))
                || matches!(
                    node.kind,
                    DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
                );
            let macro_name = if is_row { "row!" } else { "column!" };
            let gap = node.style.gap.unwrap_or(8.0);
            let pad = node.style.padding.unwrap_or(0.0);
            let rad = node.resolved_radius();
            let bg_hex = node
                .resolved_bg_hex(scheme)
                .or_else(|| scheme.role_hex("Surface"))
                .or_else(|| {
                    m3_bundle()
                        .color_dark_tokens
                        .get("Surface")
                        .map(String::as_str)
                })
                .unwrap_or_default();
            let (r, g, b) = hex_to_f32_tuple(bg_hex);

            let _ = writeln!(out, "{indent}container({macro_name}[");
            if let Some(txt) = &node.text {
                let (tr, tg, tb) = hex_to_f32_tuple(node.resolved_fg_hex(scheme));
                let _ = writeln!(
                    out,
                    "{indent}    text(\"{}\").color(Color::from_rgb({tr:.3}, {tg:.3}, {tb:.3})).into(),",
                    escape_str(txt)
                );
            }
            for child in &node.children {
                write_iced_node(out, child, scheme, msg_prefix, depth + 1);
                let _ = writeln!(out, "{indent}    ,");
            }
            let _ = writeln!(out, "{indent}].spacing({gap:.1}))");
            if pad > 0.0 {
                let _ = writeln!(out, "{indent}.padding({pad:.1})");
            }
            if let Some(w) = node.style.width {
                let _ = writeln!(out, "{indent}.width(Length::Fixed({w:.1}))");
            } else if node.style.flex_grow.is_some_and(|fg| fg > 0.0) {
                let _ = writeln!(out, "{indent}.width(Length::Fill)");
            }
            if let Some(h) = node.style.height {
                let _ = writeln!(out, "{indent}.height(Length::Fixed({h:.1}))");
            }
            let _ = writeln!(
                out,
                "{indent}.style(|_theme: &iced::Theme| container::Style {{\n\
                 {indent}    background: Some(Background::Color(Color::from_rgb({r:.3}, {g:.3}, {b:.3}))),\n\
                 {indent}    border: Border {{ radius: ({rad:.1}).into(), ..Border::default() }},\n\
                 {indent}    ..container::Style::default()\n\
                 {indent}}})\n\
                 {indent}.into()"
            );
        }
    }
}

// ============================================================================
// 6. Rust Bevy 0.20+ UI (verified via Context7 /bevyengine/bevy)
// ============================================================================

fn emit_bevy(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let fn_name = rust_ident(&doc.name).to_lowercase();
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine for Bevy 0.20+ from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(
        out,
        "use bevy::{{prelude::*, ui_widgets::{{observe, Activate, Button}}}};"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "pub fn spawn_{fn_name}_ui(mut commands: Commands) {{");
    let _ = writeln!(out, "    commands.spawn(Camera2d);");
    if let Some(root) = doc.primary_root() {
        let _ = writeln!(out, "    commands.spawn(");
        write_bevy_node(&mut out, root, &scheme, 2);
        let _ = writeln!(out, "    );");
    }
    let _ = writeln!(out, "}}");
    out
}

fn write_bevy_node(
    out: &mut String,
    node: &DesignNode,
    scheme: &ResolvedColorScheme,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    let is_row = matches!(node.style.direction.as_deref(), Some("row"))
        || matches!(
            node.kind,
            DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
        );
    let bg_hex = node
        .resolved_bg_hex(scheme)
        .or_else(|| scheme.role_hex("Surface"))
        .or_else(|| {
            m3_bundle()
                .color_dark_tokens
                .get("Surface")
                .map(String::as_str)
        })
        .unwrap_or_default();
    let (br, bg, bb) = hex_to_f32_tuple(bg_hex);
    let (fr, fg, fb) = hex_to_f32_tuple(node.resolved_fg_hex(scheme));
    let rad = node.resolved_radius();

    let _ = writeln!(out, "{indent}(");
    let _ = writeln!(out, "{indent}    Name::new(\"{}\"),", escape_str(&node.id));
    if node.kind == DesignNodeKind::Button {
        let _ = writeln!(out, "{indent}    Button,");
        let act = escape_str(node.a11y.action.as_deref().unwrap_or(&node.id));
        let _ = writeln!(
            out,
            "{indent}    observe(|_activate: On<Activate>| {{ info!(\"action: {act}\"); }}),"
        );
    }
    let _ = writeln!(out, "{indent}    Node {{");
    let _ = writeln!(
        out,
        "{indent}        flex_direction: {},",
        if is_row {
            "FlexDirection::Row"
        } else {
            "FlexDirection::Column"
        }
    );
    if let Some(w) = node.style.width {
        let _ = writeln!(out, "{indent}        width: Val::Px({w:.1}),");
    }
    if let Some(h) = node.style.height {
        let _ = writeln!(out, "{indent}        height: Val::Px({h:.1}),");
    }
    if let Some(g) = node.style.flex_grow {
        let _ = writeln!(out, "{indent}        flex_grow: {g:.1},");
    }
    if let Some(p) = node.style.padding {
        let _ = writeln!(
            out,
            "{indent}        padding: UiRect::all(Val::Px({p:.1})),"
        );
    }
    if let Some(gap) = node.style.gap {
        let _ = writeln!(out, "{indent}        row_gap: Val::Px({gap:.1}),");
        let _ = writeln!(out, "{indent}        column_gap: Val::Px({gap:.1}),");
    }
    if rad > 0.0 {
        let _ = writeln!(
            out,
            "{indent}        border_radius: BorderRadius::all(Val::Px({rad:.1})),"
        );
    }
    let _ = writeln!(out, "{indent}        ..default()");
    let _ = writeln!(out, "{indent}    }},");
    let _ = writeln!(
        out,
        "{indent}    BackgroundColor(Color::srgb({br:.3}, {bg:.3}, {bb:.3})),"
    );
    if node.text.is_some() || !node.children.is_empty() {
        let _ = writeln!(out, "{indent}    children![");
        if let Some(txt) = &node.text {
            let sz = node.style.font_size.unwrap_or(14.0);
            let _ = writeln!(
                out,
                "{indent}        (Text::new(\"{}\"), TextFont {{ font_size: FontSize::Px({sz:.1}), ..default() }}, TextColor(Color::srgb({fr:.3}, {fg:.3}, {fb:.3}))),",
                escape_str(txt)
            );
        }
        for child in &node.children {
            write_bevy_node(out, child, scheme, depth + 2);
            let _ = writeln!(out, "{indent}        ,");
        }
        let _ = writeln!(out, "{indent}    ],");
    }
    let _ = writeln!(out, "{indent})");
}

// ============================================================================
// 7. Rust Egui 0.34+ (verified via Context7 /emilk/egui)
// ============================================================================

fn emit_egui(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let fn_name = rust_ident(&doc.name).to_lowercase();
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine for Egui 0.34+ from pure Kotlin {} JSON ({} mode)",
        m3_bundle().tokens_namespace,
        doc.theme.as_str()
    );
    let _ = writeln!(
        out,
        "use egui::{{Color32, CornerRadius, Frame, RichText, Stroke, Ui}};"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "pub fn render_{fn_name}(ui: &mut Ui) {{");
    if let Some(root) = doc.primary_root() {
        write_egui_node(&mut out, root, &scheme, 1);
    }
    let _ = writeln!(out, "}}");
    out
}

fn write_egui_node(
    out: &mut String,
    node: &DesignNode,
    scheme: &ResolvedColorScheme,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    let fg_rgb = parse_hex_rgb(node.resolved_fg_hex(scheme)).unwrap_or([230, 224, 233]);
    match node.kind {
        DesignNodeKind::Text | DesignNodeKind::Badge => {
            let label = escape_str(node.text.as_deref().unwrap_or(&node.name));
            let _ = writeln!(
                out,
                "{indent}ui.label(RichText::new(\"{label}\").color(Color32::from_rgb({}, {}, {})));",
                fg_rgb[0], fg_rgb[1], fg_rgb[2]
            );
        }
        DesignNodeKind::Button => {
            let label = escape_str(node.text.as_deref().unwrap_or(&node.name));
            let _ = writeln!(
                out,
                "{indent}if ui.button(RichText::new(\"{label}\").color(Color32::from_rgb({}, {}, {}))).clicked() {{}}",
                fg_rgb[0], fg_rgb[1], fg_rgb[2]
            );
        }
        DesignNodeKind::Separator => {
            let _ = writeln!(out, "{indent}ui.separator();");
        }
        _ => {
            let bg_hex = node
                .resolved_bg_hex(scheme)
                .or_else(|| scheme.role_hex("Surface"))
                .or_else(|| {
                    m3_bundle()
                        .color_dark_tokens
                        .get("Surface")
                        .map(String::as_str)
                })
                .unwrap_or_default();
            let bg_rgb = parse_hex_rgb(bg_hex).unwrap_or([20, 18, 24]);
            let pad = node.style.padding.unwrap_or(8.0).clamp(0.0, 127.0) as i8;
            let rad = node.resolved_radius().clamp(0.0, 255.0) as u8;
            let is_row = matches!(node.style.direction.as_deref(), Some("row"))
                || matches!(
                    node.kind,
                    DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
                );
            let layout_fn = if is_row { "horizontal" } else { "vertical" };
            let _ = writeln!(
                out,
                "{indent}Frame::new()\n\
                 {indent}    .inner_margin({pad})\n\
                 {indent}    .corner_radius(CornerRadius::same({rad}))\n\
                 {indent}    .fill(Color32::from_rgb({}, {}, {}))\n\
                 {indent}    .stroke(Stroke::NONE)\n\
                 {indent}    .show(ui, |ui| {{",
                bg_rgb[0], bg_rgb[1], bg_rgb[2]
            );
            let _ = writeln!(out, "{indent}        ui.{layout_fn}(|ui| {{");
            if let Some(txt) = &node.text {
                let _ = writeln!(
                    out,
                    "{indent}            ui.label(RichText::new(\"{}\").color(Color32::from_rgb({}, {}, {})));",
                    escape_str(txt),
                    fg_rgb[0],
                    fg_rgb[1],
                    fg_rgb[2]
                );
            }
            for child in &node.children {
                write_egui_node(out, child, scheme, depth + 3);
            }
            let _ = writeln!(out, "{indent}        }});");
            let _ = writeln!(out, "{indent}    }});");
        }
    }
}

// ============================================================================
// 8. Rust Taffy + WGPU 64-byte Surface Uniform Bindings
// ============================================================================

fn emit_taffy_wgpu(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let fn_name = rust_ident(&doc.name).to_lowercase();
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine: Pure Kotlin {} JSON -> Taffy + WGPU (64-byte align(16))",
        m3_bundle().tokens_namespace
    );
    let _ = writeln!(
        out,
        "use design_engine::material3::WgpuMaterialSurfaceUniform;"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "pub fn {fn_name}_wgpu_uniforms() -> Vec<(&'static str, WgpuMaterialSurfaceUniform)> {{"
    );
    let _ = writeln!(out, "    vec![");
    if let Some(root) = doc.primary_root() {
        write_taffy_uniform_entries(&mut out, root, &scheme);
    }
    let _ = writeln!(out, "    ]");
    let _ = writeln!(out, "}}");
    out
}

fn write_taffy_uniform_entries(out: &mut String, node: &DesignNode, scheme: &ResolvedColorScheme) {
    let u = node.to_wgpu_uniform(scheme);
    let _ = writeln!(
        out,
        "        (\"{}\", WgpuMaterialSurfaceUniform {{\n\
         \x20           bg_rgba: [{:.4}, {:.4}, {:.4}, {:.4}],\n\
         \x20           fg_rgba: [{:.4}, {:.4}, {:.4}, {:.4}],\n\
         \x20           border_rgba: [{:.4}, {:.4}, {:.4}, {:.4}],\n\
         \x20           corner_radius_dp: {:.1},\n\
         \x20           elevation_dp: {:.1},\n\
         \x20           padding_h_dp: {:.1},\n\
         \x20           padding_v_dp: {:.1},\n\
         \x20       }}),",
        escape_str(&node.id),
        u.bg_rgba[0],
        u.bg_rgba[1],
        u.bg_rgba[2],
        u.bg_rgba[3],
        u.fg_rgba[0],
        u.fg_rgba[1],
        u.fg_rgba[2],
        u.fg_rgba[3],
        u.border_rgba[0],
        u.border_rgba[1],
        u.border_rgba[2],
        u.border_rgba[3],
        u.corner_radius_dp,
        u.elevation_dp,
        u.padding_h_dp,
        u.padding_v_dp
    );
    for child in &node.children {
        write_taffy_uniform_entries(out, child, scheme);
    }
}

// ============================================================================
// 9. DESIGN.md Specification
// ============================================================================

fn emit_design_md(doc: &DesignDocument) -> String {
    let bundle = m3_bundle();
    let scheme = doc.resolved_scheme();
    let report = audit_document(doc);
    let mut out = String::with_capacity(2048);
    let _ = writeln!(out, "# DESIGN — {}", doc.name);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- **Token Source**: `{}` (`configs/design/material3.json`)",
        bundle.tokens_namespace
    );
    let _ = writeln!(out, "- **Surface Mode**: `{:?}`", doc.surface_mode);
    let _ = writeln!(out, "- **Theme Mode**: `{}`", doc.theme.as_str());
    let _ = writeln!(
        out,
        "- **Viewport**: `{}x{}` (`{}`)",
        doc.viewport_width,
        doc.viewport_height,
        window_size_class(doc.viewport_width)
    );
    let _ = writeln!(
        out,
        "- **Craft & WCAG 2.2 AA Score**: `{}/100` (`passed={}`, errors={}, warnings={})",
        report.score, report.passed, report.error_count, report.warning_count
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "## Color Roles (Pure Kotlin JSON)");
    let _ = writeln!(out, "| Kotlin Token | CSS Variable | Hex |");
    let _ = writeln!(out, "|---|---|---|");
    for key in [
        "Primary",
        "OnPrimary",
        "PrimaryContainer",
        "OnPrimaryContainer",
        "Secondary",
        "SecondaryContainer",
        "Surface",
        "SurfaceContainerLow",
        "SurfaceContainer",
        "SurfaceContainerHigh",
        "OnSurface",
        "OnSurfaceVariant",
        "Outline",
        "OutlineVariant",
        "Error",
    ] {
        if let Some(hex) = scheme.role_hex(key) {
            let _ = writeln!(
                out,
                "| `{key}` | `--md-sys-color-{}` | `{hex}` |",
                pascal_to_kebab(key)
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## Pages & Routes");
    for page in &doc.pages {
        let _ = writeln!(
            out,
            "- `{}` — **{}** (`{}` nodes)",
            page.route,
            page.name,
            page.root.node_count()
        );
    }
    out
}

// ============================================================================
// 10. Apple SwiftUI + AppKit (Liquid Glass / NavigationSplitView / NSToolbarItemGroup)
// ============================================================================

fn emit_swiftui(doc: &DesignDocument) -> String {
    let scheme = doc.resolved_scheme();
    let struct_name = rust_ident(&doc.name);
    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "// Generated by design-engine for SwiftUI / AppKit ({} mode)",
        doc.theme.as_str()
    );
    let _ = writeln!(out, "import SwiftUI");
    let _ = writeln!(out, "#if canImport(AppKit)");
    let _ = writeln!(out, "import AppKit");
    let _ = writeln!(out, "#endif");
    let _ = writeln!(out);
    let _ = writeln!(out, "struct {struct_name}View: View {{");
    let _ = writeln!(
        out,
        "    @State private var glassTransparency: Double = 0.78"
    );
    let _ = writeln!(
        out,
        "    @State private var columnVisibility: NavigationSplitViewVisibility = .all"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "    var body: some View {{");
    if let Some(root) = doc.primary_root() {
        write_swiftui_node(&mut out, root, &scheme, 2);
    } else {
        let _ = writeln!(out, "        VStack {{ EmptyView() }}");
    }
    let _ = writeln!(out, "    }}");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(out, "#if canImport(AppKit)");
    let _ = writeln!(out, "@MainActor");
    let _ = writeln!(
        out,
        "func configure{struct_name}Window(_ window: NSWindow, toolbar: NSToolbar) {{"
    );
    let _ = writeln!(
        out,
        "    window.collectionBehavior.insert(.fullScreenAllowsTiling)"
    );
    let _ = writeln!(out, "    toolbar.allowsUserCustomization = true");
    let _ = writeln!(
        out,
        "    let group = NSToolbarItemGroup(itemIdentifier: NSToolbarItem.Identifier(\"aphrody.viewMode\"))"
    );
    let _ = writeln!(out, "    _ = group");
    let _ = writeln!(
        out,
        "    // Apply NSScrollEdgeEffectStyle.hard on scroll views beneath toolbar"
    );
    let _ = writeln!(out, "    _ = NSScrollEdgeEffectStyle.hard");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out, "#endif");
    out
}

fn swift_color_literal(hex: &str, alpha: f32) -> String {
    let (r, g, b) = hex_to_f32_tuple(hex);
    format!("Color(.sRGB, red: {r:.3}, green: {g:.3}, blue: {b:.3}, opacity: {alpha:.2})")
}

fn write_swiftui_node(
    out: &mut String,
    node: &DesignNode,
    scheme: &ResolvedColorScheme,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    let fg_hex = node.resolved_fg_hex(scheme);
    let bg_hex = node.resolved_bg_hex(scheme);
    let alpha = node.style.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
    let rad = node.resolved_radius();

    match node.kind {
        DesignNodeKind::Text | DesignNodeKind::Badge => {
            let txt = escape_str(node.text.as_deref().unwrap_or(&node.name));
            let _ = writeln!(
                out,
                "{indent}Text(\"{txt}\").foregroundStyle({})",
                swift_color_literal(fg_hex, 1.0)
            );
            return;
        }
        DesignNodeKind::Button => {
            let txt = escape_str(node.text.as_deref().unwrap_or(&node.name));
            let _ = writeln!(out, "{indent}Button(\"{txt}\") {{ }}");
            if let Some(bg) = bg_hex {
                let _ = writeln!(
                    out,
                    "{indent}    .background({}, in: RoundedRectangle(cornerRadius: {rad:.1}, style: .continuous))",
                    swift_color_literal(bg, alpha)
                );
            }
            return;
        }
        DesignNodeKind::Separator => {
            let _ = writeln!(
                out,
                "{indent}Divider().scrollEdgeEffectStyle(.hard, for: .top)"
            );
            return;
        }
        _ => {}
    }

    let is_split = node.id == "navigation-split-view";
    let is_row = matches!(node.style.direction.as_deref(), Some("row"))
        || matches!(
            node.kind,
            DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar
        );
    let gap = node.style.gap.unwrap_or(8.0);
    if is_split && node.children.len() >= 2 {
        let _ = writeln!(
            out,
            "{indent}NavigationSplitView(columnVisibility: $columnVisibility) {{"
        );
        write_swiftui_node(out, &node.children[0], scheme, depth + 1);
        let _ = writeln!(out, "{indent}}} detail: {{");
        for child in &node.children[1..] {
            write_swiftui_node(out, child, scheme, depth + 1);
        }
        let _ = writeln!(out, "{indent}}}");
        return;
    }

    let container = if is_row { "HStack" } else { "VStack" };
    let _ = writeln!(out, "{indent}{container}(spacing: {gap:.1}) {{");
    if let Some(txt) = &node.text {
        let _ = writeln!(
            out,
            "{indent}    Text(\"{}\").foregroundStyle({})",
            escape_str(txt),
            swift_color_literal(fg_hex, 1.0)
        );
    }
    for child in &node.children {
        write_swiftui_node(out, child, scheme, depth + 1);
    }
    let _ = writeln!(out, "{indent}}}");
    if let Some(pad) = node.style.padding {
        let _ = writeln!(out, "{indent}.padding({pad:.1})");
    }
    if node
        .style
        .material
        .as_deref()
        .is_some_and(|m| m.contains("liquid_glass"))
    {
        let _ = writeln!(
            out,
            "{indent}.background(.ultraThinMaterial.opacity(glassTransparency), in: RoundedRectangle(cornerRadius: {rad:.1}, style: .continuous))"
        );
    } else if let Some(bg) = bg_hex {
        let _ = writeln!(
            out,
            "{indent}.background({}, in: RoundedRectangle(cornerRadius: {rad:.1}, style: .continuous))",
            swift_color_literal(bg, alpha)
        );
    }
}
