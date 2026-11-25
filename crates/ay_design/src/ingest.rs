//! Reverse design synthesis from Code, Recon, Index, Sitemap, Image/Video captures,
//! `agent-desktop` accessibility trees, and `agent-browser` CDP snapshots.

#[cfg(feature = "png-ingest")]
use std::io::Cursor;

#[cfg(feature = "png-ingest")]
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ir::{
    DesignA11y, DesignDocument, DesignNode, DesignNodeKind, DesignPage, DesignSourceKind,
    DesignStyle, SurfaceMode,
};
#[cfg(not(feature = "png-ingest"))]
use crate::material3::ThemeMode;
#[cfg(feature = "png-ingest")]
use crate::material3::{ThemeMode, format_hex_rgb, m3_bundle, relative_luminance};

fn slug(raw: &str, fallback: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = s.trim_matches('-');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.chars().take(40).collect()
    }
}

fn default_shell_root(
    name: &str,
    nav_items: &[String],
    content_nodes: Vec<DesignNode>,
) -> DesignNode {
    let mut root = DesignNode::new("root", DesignNodeKind::Column).with_name(name);
    root.style.width = Some(1280.0);
    root.style.height = Some(800.0);
    root.style.bg_role = Some("Surface".to_owned());
    root.style.fg_role = Some("OnSurface".to_owned());
    root.style.padding = Some(16.0);
    root.style.gap = Some(12.0);

    let mut toolbar = DesignNode::new("top-toolbar", DesignNodeKind::Toolbar).with_name("Toolbar");
    toolbar.style.height = Some(48.0);
    toolbar.style.padding = Some(12.0);
    toolbar.style.gap = Some(12.0);
    toolbar.style.bg_role = Some("SurfaceContainer".to_owned());
    toolbar.style.fg_role = Some("OnSurface".to_owned());
    toolbar.style.shape_token = Some("CornerLarge".to_owned());
    let mut title = DesignNode::new("app-title", DesignNodeKind::Text).with_text(name);
    title.style.type_role = Some("TitleLarge".to_owned());
    title.style.font_size = Some(22.0);
    toolbar.children.push(title);
    root.children.push(toolbar);

    let mut body = DesignNode::new("workspace-body", DesignNodeKind::Row);
    body.style.flex_grow = Some(1.0);
    body.style.gap = Some(16.0);

    if !nav_items.is_empty() {
        let mut rail = DesignNode::new("nav-rail", DesignNodeKind::NavRail).with_name("Navigation");
        rail.style.width = Some(220.0);
        rail.style.padding = Some(12.0);
        rail.style.gap = Some(8.0);
        rail.style.bg_role = Some("SurfaceContainerLow".to_owned());
        rail.style.fg_role = Some("OnSurface".to_owned());
        rail.style.shape_token = Some("CornerLarge".to_owned());
        for (idx, item) in nav_items.iter().take(12).enumerate() {
            let mut btn = DesignNode::new(format!("nav-{idx}"), DesignNodeKind::Button)
                .with_name(item)
                .with_text(item);
            btn.style.height = Some(44.0);
            btn.style.padding = Some(12.0);
            btn.style.shape_token = Some("CornerFull".to_owned());
            btn.style.bg_role = Some(if idx == 0 {
                "SecondaryContainer".to_owned()
            } else {
                "SurfaceContainer".to_owned()
            });
            btn.a11y.aria_label = Some(format!("Navigate to {item}"));
            btn.a11y.focusable = true;
            btn.a11y.action = Some(format!("navigate_{}", slug(item, "home")));
            rail.children.push(btn);
        }
        body.children.push(rail);
    }

    let mut main = DesignNode::new("main-pane", DesignNodeKind::Card).with_name("Main");
    main.style.flex_grow = Some(1.0);
    main.style.padding = Some(16.0);
    main.style.gap = Some(12.0);
    main.style.bg_role = Some("SurfaceContainerLow".to_owned());
    main.style.fg_role = Some("OnSurface".to_owned());
    main.style.border_role = Some("OutlineVariant".to_owned());
    main.style.shape_token = Some("CornerLarge".to_owned());
    main.children = content_nodes;
    body.children.push(main);

    root.children.push(body);
    root
}

// ============================================================================
// 1. From Source Code (GPUI / RSX / TSX / HTML / Iced / Bevy / Egui / Kotlin)
// ============================================================================

/// Synthesize a structured M3 `DesignDocument` directly from source code in any supported
/// UI syntax (GPUI, Dioxus RSX, React/NSH TSX, HTML, Iced, Bevy, Egui, or Compose Kotlin).
#[must_use]
pub fn from_code(source: &str, name: &str) -> DesignDocument {
    let mut seed_hex: Option<String> = None;
    let mut nav_items: Vec<String> = Vec::new();
    let mut content: Vec<DesignNode> = Vec::new();

    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if seed_hex.is_none()
            && let Some(hex) = extract_first_hex(trimmed)
        {
            seed_hex = Some(hex);
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("<button")
            || lower.contains("button(")
            || lower.contains("ui.button(")
            || lower.contains("button {")
            || lower.contains("filledtonalbutton")
        {
            let label = extract_quoted_text(trimmed)
                .unwrap_or_else(|| format!("Action {}", content.len() + 1));
            let mut btn = DesignNode::new(format!("code-btn-{idx}"), DesignNodeKind::Button)
                .with_name(&label)
                .with_text(&label);
            btn.style.height = Some(44.0);
            btn.style.padding = Some(12.0);
            btn.style.shape_token = Some("CornerFull".to_owned());
            btn.style.bg_role = Some(if content.is_empty() {
                "Primary".to_owned()
            } else {
                "SecondaryContainer".to_owned()
            });
            btn.a11y.aria_label = Some(label.clone());
            btn.a11y.focusable = true;
            btn.a11y.action = Some(format!("click_{}", slug(&label, "action")));
            content.push(btn);
        } else if lower.contains("<input")
            || lower.contains("text_input(")
            || lower.contains("ui.input(")
            || lower.contains("input {")
        {
            let ph = extract_quoted_text(trimmed).unwrap_or_else(|| "Enter value".to_owned());
            let mut inp = DesignNode::new(format!("code-input-{idx}"), DesignNodeKind::Input)
                .with_name(&ph)
                .with_text(&ph);
            inp.style.height = Some(44.0);
            inp.style.padding = Some(12.0);
            inp.style.bg_role = Some("SurfaceContainerHighest".to_owned());
            inp.style.border_role = Some("Outline".to_owned());
            inp.style.shape_token = Some("CornerSmall".to_owned());
            inp.a11y.aria_label = Some(ph);
            inp.a11y.focusable = true;
            content.push(inp);
        } else if lower.contains("<nav")
            || lower.contains("sidebar")
            || lower.contains("nav_rail")
            || lower.contains("navigationrail")
        {
            if let Some(label) = extract_quoted_text(trimmed) {
                nav_items.push(label);
            }
        } else if (lower.contains("<h1")
            || lower.contains("<h2")
            || lower.contains("text::new(")
            || lower.contains("ui.text(")
            || lower.contains("text(\""))
            && let Some(label) = extract_quoted_text(trimmed)
        {
            let mut t = DesignNode::new(format!("code-text-{idx}"), DesignNodeKind::Text)
                .with_name(&label)
                .with_text(&label);
            t.style.type_role = Some(if lower.contains("<h1") {
                "HeadlineMedium".to_owned()
            } else {
                "BodyLarge".to_owned()
            });
            t.style.font_size = Some(if lower.contains("<h1") { 28.0 } else { 16.0 });
            content.push(t);
        }
    }

    if content.is_empty() {
        let mut fallback = DesignNode::new("code-summary", DesignNodeKind::Text)
            .with_text(format!("Synthesized from source: {name}"));
        fallback.style.type_role = Some("BodyLarge".to_owned());
        fallback.style.font_size = Some(16.0);
        content.push(fallback);
    }

    let mut doc = DesignDocument::new(name, default_shell_root(name, &nav_items, content));
    doc.source = DesignSourceKind::Code;
    doc.seed_hex = seed_hex;
    doc
}

fn extract_first_hex(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    for i in 0..bytes.len().saturating_sub(6) {
        if bytes[i] == b'#' && bytes[i + 1..i + 7].iter().all(|b| b.is_ascii_hexdigit()) {
            return Some(line[i..i + 7].to_ascii_uppercase());
        }
    }
    None
}

fn extract_quoted_text(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    let candidate = rest[..end].trim();
    if candidate.is_empty() || candidate.starts_with('#') || candidate.contains("::") {
        return None;
    }
    Some(candidate.to_owned())
}

// ============================================================================
// 2. From Replica Recon (`recon.md` / JSON)
// ============================================================================

/// Synthesize a multi-page `DesignDocument` from a Replica `recon.md` map (screens, flows, components).
#[must_use]
pub fn from_recon(markdown: &str, app_name: &str) -> DesignDocument {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_title = String::from("Overview");
    let mut current_items: Vec<String> = Vec::new();

    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(h) = trimmed
            .strip_prefix("## ")
            .or_else(|| trimmed.strip_prefix("### "))
        {
            if !current_items.is_empty() {
                sections.push((current_title.clone(), std::mem::take(&mut current_items)));
            }
            current_title = h.trim().to_owned();
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let clean = item.trim().trim_matches('`').to_owned();
            if !clean.is_empty() {
                current_items.push(clean);
            }
        }
    }
    if !current_items.is_empty() {
        sections.push((current_title, current_items));
    }
    if sections.is_empty() {
        sections.push((
            "Overview".to_owned(),
            vec![format!("{app_name} main workspace")],
        ));
    }

    let nav_items: Vec<String> = sections.iter().map(|(t, _)| t.clone()).collect();
    let pages: Vec<DesignPage> = sections
        .iter()
        .enumerate()
        .map(|(i, (title, items))| {
            let nodes: Vec<DesignNode> = items
                .iter()
                .enumerate()
                .map(|(j, label)| {
                    let mut card = DesignNode::new(format!("recon-{i}-{j}"), DesignNodeKind::Card)
                        .with_name(label)
                        .with_text(label);
                    card.style.padding = Some(12.0);
                    card.style.bg_role = Some("SurfaceContainer".to_owned());
                    card.style.shape_token = Some("CornerMedium".to_owned());
                    card
                })
                .collect();
            DesignPage {
                id: format!("recon-page-{i}"),
                name: title.clone(),
                route: if i == 0 {
                    "/".to_owned()
                } else {
                    format!("/{}", slug(title, "screen"))
                },
                root: default_shell_root(title, &nav_items, nodes),
            }
        })
        .collect();

    DesignDocument {
        schema: "design.ir.v1".to_owned(),
        name: app_name.to_owned(),
        surface_mode: SurfaceMode::Operate,
        theme: ThemeMode::Dark,
        seed_hex: None,
        source: DesignSourceKind::Recon,
        viewport_width: 1280,
        viewport_height: 800,
        pages,
    }
}

// ============================================================================
// 3. From XML Sitemap (`sitemap.xml`)
// ============================================================================

/// Synthesize a multi-page navigation architecture directly from a standard W3C XML `sitemap.xml`.
#[must_use]
pub fn from_sitemap_xml(xml: &str, site_name: &str) -> DesignDocument {
    let mut routes: Vec<String> = Vec::new();
    let mut cursor = xml;
    while let Some(start) = cursor.find("<loc>") {
        let after = &cursor[start + 5..];
        let Some(end) = after.find("</loc>") else {
            break;
        };
        let url = after[..end].trim();
        let path = url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.find('/').map(|idx| &rest[idx..]))
            .unwrap_or(url);
        let route = if path.is_empty() { "/" } else { path }.to_owned();
        if !routes.contains(&route) {
            routes.push(route);
        }
        cursor = &after[end + 6..];
    }
    if routes.is_empty() {
        routes.push("/".to_owned());
    }

    let nav_labels: Vec<String> = routes
        .iter()
        .map(|r| {
            let trimmed = r.trim_matches('/');
            if trimmed.is_empty() {
                "Home".to_owned()
            } else {
                trimmed.replace(['/', '-', '_'], " ")
            }
        })
        .collect();

    let pages: Vec<DesignPage> = routes
        .iter()
        .zip(nav_labels.iter())
        .enumerate()
        .map(|(i, (route, label))| {
            let mut heading = DesignNode::new(format!("route-heading-{i}"), DesignNodeKind::Text)
                .with_text(label);
            heading.style.type_role = Some("HeadlineLarge".to_owned());
            heading.style.font_size = Some(32.0);
            let mut cta = DesignNode::new(format!("route-cta-{i}"), DesignNodeKind::Button)
                .with_text(format!("Open {label}"));
            cta.style.height = Some(44.0);
            cta.style.padding = Some(12.0);
            cta.style.bg_role = Some("Primary".to_owned());
            cta.style.shape_token = Some("CornerFull".to_owned());
            cta.a11y.aria_label = Some(format!("Open {label}"));
            cta.a11y.focusable = true;
            DesignPage {
                id: format!("sitemap-page-{i}"),
                name: label.clone(),
                route: route.clone(),
                root: default_shell_root(site_name, &nav_labels, vec![heading, cta]),
            }
        })
        .collect();

    DesignDocument {
        schema: "design.ir.v1".to_owned(),
        name: site_name.to_owned(),
        surface_mode: SurfaceMode::Read,
        theme: ThemeMode::Dark,
        seed_hex: None,
        source: DesignSourceKind::Sitemap,
        viewport_width: 1280,
        viewport_height: 800,
        pages,
    }
}

// ============================================================================
// 4. From Project Index / Code Graph JSON (`HYPER_INDEX.json` / routes index)
// ============================================================================

/// Synthesize a multi-view workspace `DesignDocument` from an index JSON (`HYPER_INDEX.json`,
/// `CODE_GRAPH_INGEST.json`, or a custom `{ "routes": [...] }` / `{ "modules": [...] }` index).
pub fn from_index_json(raw: &str, project_name: &str) -> Result<DesignDocument, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("invalid index json: {e}"))?;
    let mut entries: Vec<String> = Vec::new();
    for key in [
        "routes",
        "pages",
        "modules",
        "packages",
        "crates",
        "workspaces",
        "files",
    ] {
        if let Some(arr) = v.get(key).and_then(Value::as_array) {
            for item in arr.iter().take(16) {
                if let Some(s) = item.as_str() {
                    entries.push(s.to_owned());
                } else if let Some(s) = item
                    .get("name")
                    .or_else(|| item.get("path"))
                    .or_else(|| item.get("route"))
                    .and_then(Value::as_str)
                {
                    entries.push(s.to_owned());
                }
            }
        }
    }
    if entries.is_empty()
        && let Some(obj) = v.as_object()
    {
        for k in obj.keys().take(12) {
            entries.push(k.clone());
        }
    }
    if entries.is_empty() {
        entries.push("Overview".to_owned());
    }

    let cards: Vec<DesignNode> = entries
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut card = DesignNode::new(format!("idx-card-{i}"), DesignNodeKind::Card)
                .with_name(item)
                .with_text(item);
            card.style.padding = Some(12.0);
            card.style.bg_role = Some("SurfaceContainer".to_owned());
            card.style.shape_token = Some("CornerMedium".to_owned());
            card
        })
        .collect();

    let mut doc = DesignDocument::new(
        project_name,
        default_shell_root(project_name, &entries, cards),
    );
    doc.source = DesignSourceKind::Index;
    Ok(doc)
}

// ============================================================================
// 5. From Real PNG Image Capture & Video Storyboard Keyframes
// ============================================================================

#[cfg(feature = "png-ingest")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageAnalysis {
    pub width: u32,
    pub height: u32,
    pub avg_luminance: f64,
    pub detected_theme: ThemeMode,
    pub dominant_seed_hex: String,
    pub has_top_bar: bool,
    pub has_left_sidebar: bool,
}

/// Analyze real PNG bytes in pure Rust: detects Light/Dark/OLED theme polarity, extracts the
/// highest-chroma accent seed color, and segments top-bar / left-sidebar visual bands.
#[cfg(feature = "png-ingest")]
pub fn analyze_png_bytes(bytes: &[u8]) -> Result<ImageAnalysis, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("failed to decode PNG header: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("failed to decode PNG frame: {e}"))?;
    let w = info.width;
    let h = info.height;
    let bpp = match info.color_type {
        png::ColorType::Rgba => 4usize,
        png::ColorType::Rgb => 3usize,
        png::ColorType::GrayscaleAlpha => 2usize,
        png::ColorType::Grayscale => 1usize,
        png::ColorType::Indexed => 1usize,
    };

    let stride = (w as usize) * bpp;
    let step_x = ((w as usize) / 64).max(1);
    let step_y = ((h as usize) / 64).max(1);

    let mut lum_sum = 0.0f64;
    let mut lum_count = 0u64;
    let mut top_lum_sum = 0.0f64;
    let mut top_count = 0u64;
    let mut left_lum_sum = 0.0f64;
    let mut left_count = 0u64;
    let mut body_lum_sum = 0.0f64;
    let mut body_count = 0u64;

    let mut best_chroma = 0u16;
    let mut best_rgb = [0x67u8, 0x50u8, 0xA4u8];

    let pixel_rgb = |x: usize, y: usize| -> [u8; 3] {
        let off = y * stride + x * bpp;
        if off + bpp > buf.len() {
            return [0, 0, 0];
        }
        match bpp {
            4 | 3 => [buf[off], buf[off + 1], buf[off + 2]],
            _ => [buf[off], buf[off], buf[off]],
        }
    };

    let mut y = 0usize;
    while y < h as usize {
        let mut x = 0usize;
        while x < w as usize {
            let rgb = pixel_rgb(x, y);
            let lum = relative_luminance(rgb);
            lum_sum += lum;
            lum_count += 1;

            let is_top = y * 10 < h as usize;
            let is_left = x * 5 < w as usize;
            if is_top {
                top_lum_sum += lum;
                top_count += 1;
            } else if is_left {
                left_lum_sum += lum;
                left_count += 1;
            } else {
                body_lum_sum += lum;
                body_count += 1;
            }

            let max_c = rgb[0].max(rgb[1]).max(rgb[2]);
            let min_c = rgb[0].min(rgb[1]).min(rgb[2]);
            let chroma = u16::from(max_c - min_c);
            if chroma > best_chroma && max_c > 40 {
                best_chroma = chroma;
                best_rgb = rgb;
            }
            x += step_x;
        }
        y += step_y;
    }

    let avg_lum = if lum_count > 0 {
        lum_sum / lum_count as f64
    } else {
        0.1
    };
    let top_lum = if top_count > 0 {
        top_lum_sum / top_count as f64
    } else {
        avg_lum
    };
    let left_lum = if left_count > 0 {
        left_lum_sum / left_count as f64
    } else {
        avg_lum
    };
    let body_lum = if body_count > 0 {
        body_lum_sum / body_count as f64
    } else {
        avg_lum
    };

    // M3 dark Surface sits near 0.007, so only near-true-black frames count as OLED.
    let detected_theme = if avg_lum > 0.45 {
        ThemeMode::Light
    } else if avg_lum < 0.0025 {
        ThemeMode::OledPitch
    } else {
        ThemeMode::Dark
    };

    let dominant_seed_hex = if best_chroma >= 24 {
        format_hex_rgb(best_rgb)
    } else {
        m3_bundle()
            .palette_tokens
            .get("Primary40")
            .cloned()
            .unwrap_or_else(|| format_hex_rgb(best_rgb))
    };

    Ok(ImageAnalysis {
        width: w,
        height: h,
        avg_luminance: avg_lum,
        detected_theme,
        dominant_seed_hex,
        has_top_bar: (top_lum - body_lum).abs() > 0.025,
        has_left_sidebar: (left_lum - body_lum).abs() > 0.025,
    })
}

/// Synthesize a complete M3 Expressive `DesignDocument` directly from a captured PNG screenshot.
#[cfg(feature = "png-ingest")]
pub fn from_png_bytes(bytes: &[u8], name: &str) -> Result<(DesignDocument, ImageAnalysis), String> {
    let analysis = analyze_png_bytes(bytes)?;
    let nav = if analysis.has_left_sidebar {
        vec![
            "Overview".to_owned(),
            "Inspector".to_owned(),
            "Assets".to_owned(),
        ]
    } else {
        Vec::new()
    };

    let mut hero = DesignNode::new("captured-canvas", DesignNodeKind::ShaderCanvas)
        .with_name(name)
        .with_text(format!(
            "Captured {}x{} ({}, seed {})",
            analysis.width,
            analysis.height,
            analysis.detected_theme.as_str(),
            analysis.dominant_seed_hex
        ));
    hero.style.flex_grow = Some(1.0);
    hero.style.padding = Some(16.0);
    hero.style.bg_role = Some("SurfaceContainer".to_owned());
    hero.style.shape_token = Some("CornerLarge".to_owned());

    let mut cta =
        DesignNode::new("captured-primary-cta", DesignNodeKind::Button).with_text("Inspect Tokens");
    cta.style.height = Some(44.0);
    cta.style.padding = Some(12.0);
    cta.style.bg_role = Some("Primary".to_owned());
    cta.style.shape_token = Some("CornerFull".to_owned());
    cta.a11y.aria_label = Some("Inspect Tokens".to_owned());
    cta.a11y.focusable = true;

    let mut doc = DesignDocument::new(name, default_shell_root(name, &nav, vec![hero, cta]));
    doc.source = DesignSourceKind::ImageCapture;
    doc.theme = analysis.detected_theme;
    doc.seed_hex = Some(analysis.dominant_seed_hex.clone());
    doc.viewport_width = analysis.width;
    doc.viewport_height = analysis.height;
    Ok((doc, analysis))
}

#[cfg(feature = "png-ingest")]
#[derive(Debug, Clone)]
pub struct VideoKeyframeInput {
    pub timestamp_sec: f64,
    pub label: String,
    pub png_bytes: Vec<u8>,
}

/// Synthesize a multi-page storyboard `DesignDocument` across captured video keyframes.
#[cfg(feature = "png-ingest")]
pub fn from_video_keyframes(
    frames: &[VideoKeyframeInput],
    name: &str,
) -> Result<DesignDocument, String> {
    if frames.is_empty() {
        return Err("video storyboard requires at least one keyframe".to_owned());
    }
    let mut pages = Vec::with_capacity(frames.len());
    let mut primary_theme = ThemeMode::Dark;
    let mut primary_seed = None;
    let mut vw = 1280u32;
    let mut vh = 720u32;

    for (idx, frame) in frames.iter().enumerate() {
        let (sub_doc, analysis) = from_png_bytes(&frame.png_bytes, &frame.label)?;
        if idx == 0 {
            primary_theme = analysis.detected_theme;
            primary_seed = Some(analysis.dominant_seed_hex);
            vw = analysis.width;
            vh = analysis.height;
        }
        if let Some(mut page) = sub_doc.pages.into_iter().next() {
            page.id = format!("frame-{idx}");
            page.name = format!("{} ({:.2}s)", frame.label, frame.timestamp_sec);
            page.route = format!("/frame/{idx}");
            pages.push(page);
        }
    }

    Ok(DesignDocument {
        schema: "design.ir.v1".to_owned(),
        name: name.to_owned(),
        surface_mode: SurfaceMode::Experience,
        theme: primary_theme,
        seed_hex: primary_seed,
        source: DesignSourceKind::VideoCapture,
        viewport_width: vw,
        viewport_height: vh,
        pages,
    })
}

// ============================================================================
// 6. From `agent-desktop` Native Accessibility Trees (UIA / AT-SPI / AX)
// ============================================================================

/// Synthesize a `DesignDocument` directly from `agent-desktop` JSON or indented UIA/AT-SPI/AX tree text.
#[must_use]
pub fn from_agent_desktop_tree(raw: &str, app_name: &str) -> DesignDocument {
    let mut nodes: Vec<DesignNode> = Vec::new();
    let mut nav_items: Vec<String> = Vec::new();

    if let Ok(json_val) = serde_json::from_str::<Value>(raw) {
        walk_desktop_json(&json_val, &mut nav_items, &mut nodes);
    } else {
        for (idx, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            let label = extract_quoted_text(trimmed).unwrap_or_else(|| {
                trimmed
                    .split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            let label = if label.is_empty() {
                format!("Control {idx}")
            } else {
                label
            };
            if lower.contains("tabitem")
                || lower.contains("menuitem")
                || lower.contains("navigation")
            {
                nav_items.push(label);
            } else if let Some(node) = desktop_role_to_node(idx, &lower, &label) {
                nodes.push(node);
            }
        }
    }

    let mut doc = DesignDocument::new(app_name, default_shell_root(app_name, &nav_items, nodes));
    doc.source = DesignSourceKind::AgentDesktop;
    doc
}

fn walk_desktop_json(v: &Value, nav: &mut Vec<String>, out: &mut Vec<DesignNode>) {
    let role = v
        .get("role")
        .or_else(|| v.get("control_type"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let name = v
        .get("name")
        .or_else(|| v.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();

    if !name.is_empty() {
        if role.contains("tab") || role.contains("menu") {
            nav.push(name.clone());
        } else if let Some(node) = desktop_role_to_node(out.len(), &role, &name) {
            out.push(node);
        }
    }
    if let Some(children) = v.get("children").and_then(Value::as_array) {
        for child in children {
            walk_desktop_json(child, nav, out);
        }
    }
}

fn desktop_role_to_node(idx: usize, role: &str, label: &str) -> Option<DesignNode> {
    let (kind, bg_role) = if role.contains("button") {
        (DesignNodeKind::Button, "Primary")
    } else if role.contains("edit") || role.contains("textbox") || role.contains("input") {
        (DesignNodeKind::Input, "SurfaceContainerHighest")
    } else if role.contains("checkbox") {
        (DesignNodeKind::Checkbox, "SecondaryContainer")
    } else if role.contains("switch") || role.contains("toggle") {
        (DesignNodeKind::Switch, "SecondaryContainer")
    } else if role.contains("text") || role.contains("heading") || role.contains("label") {
        (DesignNodeKind::Text, "SurfaceContainerLow")
    } else {
        return None;
    };
    let mut node = DesignNode::new(format!("desktop-{idx}"), kind)
        .with_name(label)
        .with_text(label);
    node.style = DesignStyle {
        height: if node.kind.is_interactive() {
            Some(44.0)
        } else {
            None
        },
        padding: Some(12.0),
        bg_role: Some(bg_role.to_owned()),
        shape_token: Some(if node.kind == DesignNodeKind::Button {
            "CornerFull".to_owned()
        } else {
            "CornerSmall".to_owned()
        }),
        ..Default::default()
    };
    node.a11y = DesignA11y {
        role: Some(role.to_owned()),
        aria_label: Some(label.to_owned()),
        focusable: node.kind.is_interactive(),
        action: if node.kind == DesignNodeKind::Button {
            Some(format!("desktop_{}", slug(label, "click")))
        } else {
            None
        },
    };
    Some(node)
}

// ============================================================================
// 7. From `agent-browser` CDP Accessibility Snapshots (`@eN` refs)
// ============================================================================

/// Synthesize a `DesignDocument` directly from an `agent-browser` accessibility snapshot
/// (lines like `- button "Deploy" [ref=@e4]` or `- heading "Dashboard" [level=1]`).
#[must_use]
pub fn from_agent_browser_snapshot(snapshot: &str, page_title: &str) -> DesignDocument {
    let mut nav_items: Vec<String> = Vec::new();
    let mut nodes: Vec<DesignNode> = Vec::new();

    for (idx, line) in snapshot.lines().enumerate() {
        let trimmed = line.trim().trim_start_matches('-').trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let label = extract_quoted_text(trimmed).unwrap_or_else(|| format!("Element {idx}"));
        if lower.starts_with("link ") || lower.starts_with("tab ") {
            nav_items.push(label);
        } else if lower.starts_with("button") {
            let mut btn = DesignNode::new(format!("browser-btn-{idx}"), DesignNodeKind::Button)
                .with_name(&label)
                .with_text(&label);
            btn.style.height = Some(44.0);
            btn.style.padding = Some(12.0);
            btn.style.bg_role = Some(if nodes.is_empty() {
                "Primary".to_owned()
            } else {
                "SecondaryContainer".to_owned()
            });
            btn.style.shape_token = Some("CornerFull".to_owned());
            btn.a11y.aria_label = Some(label.clone());
            btn.a11y.focusable = true;
            btn.a11y.action = Some(format!("browser_{}", slug(&label, "click")));
            nodes.push(btn);
        } else if lower.starts_with("textbox") || lower.starts_with("searchbox") {
            let mut inp = DesignNode::new(format!("browser-input-{idx}"), DesignNodeKind::Input)
                .with_name(&label)
                .with_text(&label);
            inp.style.height = Some(44.0);
            inp.style.padding = Some(12.0);
            inp.style.bg_role = Some("SurfaceContainerHighest".to_owned());
            inp.style.border_role = Some("Outline".to_owned());
            inp.style.shape_token = Some("CornerSmall".to_owned());
            inp.a11y.aria_label = Some(label);
            inp.a11y.focusable = true;
            nodes.push(inp);
        } else if lower.starts_with("heading") {
            let mut txt = DesignNode::new(format!("browser-heading-{idx}"), DesignNodeKind::Text)
                .with_name(&label)
                .with_text(&label);
            txt.style.type_role = Some("HeadlineMedium".to_owned());
            txt.style.font_size = Some(28.0);
            nodes.push(txt);
        }
    }

    let mut doc = DesignDocument::new(
        page_title,
        default_shell_root(page_title, &nav_items, nodes),
    );
    doc.source = DesignSourceKind::AgentBrowser;
    doc
}
