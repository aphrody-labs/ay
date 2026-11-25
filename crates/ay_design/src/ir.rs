//! Canonical Design IR uniting FigCraft documents, pure Kotlin Material 3 JSON tokens,
//! Impeccable surface modes, W3C ARIA metadata, Taffy layout projection, and GPUI/WGPU export.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::material3::{ResolvedColorScheme, ThemeMode, WgpuMaterialSurfaceUniform, m3_bundle};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMode {
    #[default]
    Operate,
    Persuade,
    Read,
    Experience,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignSourceKind {
    #[default]
    Authored,
    Figcraft,
    Code,
    Recon,
    Index,
    Sitemap,
    ImageCapture,
    VideoCapture,
    AgentDesktop,
    AgentBrowser,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignNodeKind {
    #[default]
    Frame,
    Row,
    Column,
    Grid,
    Card,
    Toolbar,
    Sidebar,
    NavRail,
    TabBar,
    Dialog,
    Button,
    Input,
    Text,
    Badge,
    Progress,
    Switch,
    Checkbox,
    Avatar,
    Image,
    Video,
    ShaderCanvas,
    DataTable,
    Separator,
}

impl DesignNodeKind {
    #[must_use]
    pub const fn is_interactive(&self) -> bool {
        matches!(
            self,
            Self::Button | Self::Input | Self::Switch | Self::Checkbox | Self::TabBar
        )
    }

    #[must_use]
    pub const fn gpui_kind(&self) -> &'static str {
        match self {
            Self::Frame | Self::Column => "v_flex",
            Self::Row => "h_flex",
            Self::Grid => "grid",
            Self::Card => "card",
            Self::Toolbar => "toolbar",
            Self::Sidebar | Self::NavRail => "sidebar",
            Self::TabBar => "tab_bar",
            Self::Dialog => "dialog",
            Self::Button => "button",
            Self::Input => "input",
            Self::Text => "text",
            Self::Badge => "badge",
            Self::Progress => "progress",
            Self::Switch => "switch",
            Self::Checkbox => "checkbox",
            Self::Avatar => "avatar",
            Self::Image => "image",
            Self::Video => "video",
            Self::ShaderCanvas => "shader_view",
            Self::DataTable => "data_table",
            Self::Separator => "separator",
        }
    }

    #[must_use]
    pub const fn figcraft_type(&self) -> &'static str {
        match self {
            Self::Text => "TEXT",
            Self::Button
            | Self::Input
            | Self::Card
            | Self::Dialog
            | Self::NavRail
            | Self::Toolbar => "COMPONENT",
            Self::Separator => "LINE",
            Self::Avatar => "ELLIPSE",
            Self::Image | Self::Video | Self::ShaderCanvas => "RECTANGLE",
            _ => "FRAME",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignStyle {
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub justify: Option<String>,
    #[serde(default)]
    pub align: Option<String>,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub height: Option<f32>,
    #[serde(default)]
    pub flex_grow: Option<f32>,
    #[serde(default)]
    pub padding: Option<f32>,
    #[serde(default)]
    pub gap: Option<f32>,
    #[serde(default)]
    pub corner_radius: Option<f32>,
    #[serde(default)]
    pub shape_token: Option<String>,
    #[serde(default)]
    pub elevation_level: Option<String>,
    #[serde(default)]
    pub bg_role: Option<String>,
    #[serde(default)]
    pub fg_role: Option<String>,
    #[serde(default)]
    pub border_role: Option<String>,
    #[serde(default)]
    pub border_width: Option<f32>,
    #[serde(default)]
    pub bg_hex: Option<String>,
    #[serde(default)]
    pub fg_hex: Option<String>,
    #[serde(default)]
    pub type_role: Option<String>,
    #[serde(default)]
    pub font_size: Option<f32>,
    #[serde(default)]
    pub motion_spring: Option<String>,
    #[serde(default)]
    pub opacity: Option<f32>,
    #[serde(default)]
    pub material: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignA11y {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub aria_label: Option<String>,
    #[serde(default)]
    pub focusable: bool,
    #[serde(default)]
    pub action: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignNode {
    pub id: String,
    pub name: String,
    pub kind: DesignNodeKind,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub style: DesignStyle,
    #[serde(default)]
    pub a11y: DesignA11y,
    #[serde(default)]
    pub children: Vec<DesignNode>,
}

impl DesignNode {
    #[must_use]
    pub fn new(id: impl Into<String>, kind: DesignNodeKind) -> Self {
        let id = id.into();
        let name = id.clone();
        Self {
            id,
            name,
            kind,
            text: None,
            style: DesignStyle::default(),
            a11y: DesignA11y::default(),
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    #[must_use]
    pub fn with_child(mut self, child: DesignNode) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
    }

    #[must_use]
    pub fn resolved_radius(&self) -> f32 {
        if let Some(px) = self.style.corner_radius {
            return px;
        }
        if let Some(tok) = &self.style.shape_token
            && let Some(px) = m3_bundle().shape_dp(tok)
        {
            return px;
        }
        0.0
    }

    #[must_use]
    pub fn resolved_bg_hex<'a>(&'a self, scheme: &'a ResolvedColorScheme) -> Option<&'a str> {
        if let Some(hex) = &self.style.bg_hex {
            return Some(hex.as_str());
        }
        self.style
            .bg_role
            .as_deref()
            .and_then(|role| scheme.role_hex(role))
    }

    #[must_use]
    pub fn resolved_fg_hex<'a>(&'a self, scheme: &'a ResolvedColorScheme) -> &'a str {
        if let Some(hex) = &self.style.fg_hex {
            return hex.as_str();
        }
        if let Some(role) = self.style.fg_role.as_deref()
            && let Some(hex) = scheme.role_hex(role)
        {
            return hex;
        }
        if let Some(bg_role) = self.style.bg_role.as_deref() {
            let paired = ResolvedColorScheme::paired_on_role(bg_role);
            if let Some(hex) = scheme.role_hex(paired) {
                return hex;
            }
        }
        scheme
            .role_hex("OnSurface")
            .or_else(|| {
                m3_bundle()
                    .color_dark_tokens
                    .get("OnSurface")
                    .map(String::as_str)
            })
            .unwrap_or_default()
    }

    /// Pack this node's visual style into the 64-byte WGPU `WgpuMaterialSurfaceUniform`.
    #[must_use]
    pub fn to_wgpu_uniform(&self, scheme: &ResolvedColorScheme) -> WgpuMaterialSurfaceUniform {
        let bg = self.resolved_bg_hex(scheme).unwrap_or("Surface");
        let fg = self.resolved_fg_hex(scheme);
        let border = self
            .style
            .border_role
            .as_deref()
            .unwrap_or("OutlineVariant");
        let pad = self.style.padding.unwrap_or(0.0);
        let elev = self
            .style
            .elevation_level
            .as_deref()
            .map(|lvl| m3_bundle().elevation_dp(lvl))
            .unwrap_or(0.0);
        let mut bg_rgba = scheme.resolve_rgba_f32(bg);
        if let Some(alpha) = self.style.opacity {
            bg_rgba[3] *= alpha.clamp(0.0, 1.0);
        }
        WgpuMaterialSurfaceUniform {
            bg_rgba,
            fg_rgba: scheme.resolve_rgba_f32(fg),
            border_rgba: scheme.resolve_rgba_f32(border),
            corner_radius_dp: self.resolved_radius(),
            elevation_dp: elev,
            padding_h_dp: pad,
            padding_v_dp: pad,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignPage {
    pub id: String,
    pub name: String,
    pub route: String,
    pub root: DesignNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignDocument {
    pub schema: String,
    pub name: String,
    pub surface_mode: SurfaceMode,
    pub theme: ThemeMode,
    #[serde(default)]
    pub seed_hex: Option<String>,
    pub source: DesignSourceKind,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub pages: Vec<DesignPage>,
}

impl DesignDocument {
    #[must_use]
    pub fn new(name: impl Into<String>, root: DesignNode) -> Self {
        let name = name.into();
        Self {
            schema: "design.ir.v1".to_owned(),
            name: name.clone(),
            surface_mode: SurfaceMode::Operate,
            theme: ThemeMode::Dark,
            seed_hex: None,
            source: DesignSourceKind::Authored,
            viewport_width: 1280,
            viewport_height: 800,
            pages: vec![DesignPage {
                id: "page-main".to_owned(),
                name,
                route: "/".to_owned(),
                root,
            }],
        }
    }

    #[must_use]
    pub fn resolved_scheme(&self) -> ResolvedColorScheme {
        m3_bundle().color_scheme(self.theme, self.seed_hex.as_deref())
    }

    #[must_use]
    pub fn primary_root(&self) -> Option<&DesignNode> {
        self.pages.first().map(|p| &p.root)
    }

    #[must_use]
    pub fn total_nodes(&self) -> usize {
        self.pages.iter().map(|p| p.root.node_count()).sum()
    }

    /// Project the primary page to `GpuiElementSpec`-compatible Taffy+WGPU JSON.
    #[must_use]
    pub fn to_gpui_spec_json(&self) -> Value {
        let scheme = self.resolved_scheme();
        match self.primary_root() {
            Some(root) => node_to_gpui_json(root, &scheme),
            None => json!({ "id": "empty", "kind": "v_flex", "style": {}, "children": [] }),
        }
    }

    /// Export a FigCraft-compatible JSON document (`pages[].children[]`).
    #[must_use]
    pub fn to_figcraft_json(&self) -> Value {
        let scheme = self.resolved_scheme();
        let pages: Vec<Value> = self
            .pages
            .iter()
            .enumerate()
            .map(|(idx, page)| {
                json!({
                    "id": format!("0:{}", idx + 1),
                    "name": page.name,
                    "type": "PAGE",
                    "route": page.route,
                    "children": [node_to_figcraft_json(&page.root, &scheme, idx + 1, 1)],
                })
            })
            .collect();
        json!({
            "format": "figcraft",
            "schema": "figcraft.bridge.v1",
            "tokensNamespace": m3_bundle().tokens_namespace,
            "name": self.name,
            "theme": self.theme.as_str(),
            "surfaceMode": self.surface_mode,
            "seedHex": self.seed_hex,
            "viewport": { "width": self.viewport_width, "height": self.viewport_height },
            "pages": pages,
        })
    }

    pub fn from_figcraft_json(raw: &str) -> Result<Self, String> {
        let v: Value =
            serde_json::from_str(raw).map_err(|e| format!("invalid figcraft json: {e}"))?;
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("FigCraft Import")
            .to_owned();
        let theme = v
            .get("theme")
            .and_then(Value::as_str)
            .map(ThemeMode::parse)
            .unwrap_or(ThemeMode::Dark);
        let width = v
            .pointer("/viewport/width")
            .and_then(Value::as_u64)
            .unwrap_or(1280) as u32;
        let height = v
            .pointer("/viewport/height")
            .and_then(Value::as_u64)
            .unwrap_or(800) as u32;

        let mut pages = Vec::new();
        if let Some(arr) = v.get("pages").and_then(Value::as_array) {
            for (i, p) in arr.iter().enumerate() {
                let p_name = p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Page")
                    .to_owned();
                let route = p
                    .get("route")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| {
                        if i == 0 {
                            "/".to_owned()
                        } else {
                            format!("/page-{i}")
                        }
                    });
                let root = p
                    .get("children")
                    .and_then(Value::as_array)
                    .and_then(|c| c.first())
                    .map(figcraft_json_to_node)
                    .unwrap_or_else(|| DesignNode::new(format!("page-{i}"), DesignNodeKind::Frame));
                pages.push(DesignPage {
                    id: format!("page-{i}"),
                    name: p_name,
                    route,
                    root,
                });
            }
        }
        if pages.is_empty() {
            pages.push(DesignPage {
                id: "page-0".to_owned(),
                name: name.clone(),
                route: "/".to_owned(),
                root: DesignNode::new("root", DesignNodeKind::Frame),
            });
        }
        Ok(Self {
            schema: "design.ir.v1".to_owned(),
            name,
            surface_mode: SurfaceMode::Operate,
            theme,
            seed_hex: v
                .get("seedHex")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            source: DesignSourceKind::Figcraft,
            viewport_width: width,
            viewport_height: height,
            pages,
        })
    }
}

fn node_to_gpui_json(node: &DesignNode, scheme: &ResolvedColorScheme) -> Value {
    let dir = node.style.direction.as_deref().unwrap_or(match node.kind {
        DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar => "row",
        _ => "column",
    });
    let display = if matches!(node.kind, DesignNodeKind::Grid) {
        "grid"
    } else {
        "flex"
    };
    let bg = node.resolved_bg_hex(scheme).map(|h| {
        let mut rgba = scheme.resolve_rgba_f32(h);
        if let Some(alpha) = node.style.opacity {
            rgba[3] *= alpha.clamp(0.0, 1.0);
        }
        rgba
    });
    let border_color = node.style.border_role.as_deref().map(|r| {
        let mut rgba = scheme.resolve_rgba_f32(r);
        if let Some(alpha) = node.style.opacity {
            rgba[3] *= (0.4 + 0.6 * alpha.clamp(0.0, 1.0)).min(1.0);
        }
        rgba
    });

    let mut style = serde_json::Map::new();
    style.insert("display".to_owned(), json!(display));
    style.insert("flex_direction".to_owned(), json!(dir));
    if let Some(j) = &node.style.justify {
        style.insert("justify_content".to_owned(), json!(j));
    }
    if let Some(a) = &node.style.align {
        style.insert("align_items".to_owned(), json!(a));
    }
    if let Some(w) = node.style.width {
        style.insert("width".to_owned(), json!(w));
    }
    if let Some(h) = node.style.height {
        style.insert("height".to_owned(), json!(h));
    }
    if let Some(g) = node.style.flex_grow {
        style.insert("flex_grow".to_owned(), json!(g));
    }
    if let Some(p) = node.style.padding {
        style.insert("padding".to_owned(), json!(p));
    }
    if let Some(gap) = node.style.gap {
        style.insert("gap".to_owned(), json!(gap));
    }
    let rad = node.resolved_radius();
    if rad > 0.0 {
        style.insert("corner_radius".to_owned(), json!(rad));
    }
    if let Some(c) = bg {
        style.insert("bg".to_owned(), json!(c));
    }
    if let Some(bc) = border_color {
        style.insert("border_color".to_owned(), json!(bc));
        style.insert(
            "border_width".to_owned(),
            json!(node.style.border_width.unwrap_or(1.0)),
        );
    }

    let mut obj = serde_json::Map::new();
    obj.insert("id".to_owned(), json!(node.id));
    obj.insert("kind".to_owned(), json!(node.kind.gpui_kind()));
    if let Some(t) = &node.text {
        obj.insert("text".to_owned(), json!(t));
    }
    if let Some(act) = &node.a11y.action {
        obj.insert("action".to_owned(), json!(act));
    }
    obj.insert("style".to_owned(), Value::Object(style));
    obj.insert(
        "children".to_owned(),
        Value::Array(
            node.children
                .iter()
                .map(|c| node_to_gpui_json(c, scheme))
                .collect(),
        ),
    );
    Value::Object(obj)
}

fn node_to_figcraft_json(
    node: &DesignNode,
    scheme: &ResolvedColorScheme,
    page_idx: usize,
    seq: usize,
) -> Value {
    let bg_hex = node.resolved_bg_hex(scheme);
    let fg_hex = node.resolved_fg_hex(scheme);
    let children: Vec<Value> = node
        .children
        .iter()
        .enumerate()
        .map(|(i, c)| node_to_figcraft_json(c, scheme, page_idx, seq * 100 + i + 1))
        .collect();
    json!({
        "id": format!("{page_idx}:{seq}"),
        "nodeId": node.id,
        "name": node.name,
        "type": node.kind.figcraft_type(),
        "kind": node.kind,
        "characters": node.text,
        "layoutMode": if matches!(node.style.direction.as_deref(), Some("row"))
            || matches!(node.kind, DesignNodeKind::Row | DesignNodeKind::Toolbar | DesignNodeKind::TabBar)
        {
            "HORIZONTAL"
        } else {
            "VERTICAL"
        },
        "itemSpacing": node.style.gap.unwrap_or(0.0),
        "padding": node.style.padding.unwrap_or(0.0),
        "cornerRadius": node.resolved_radius(),
        "width": node.style.width,
        "height": node.style.height,
        "fills": bg_hex.map(|hex| vec![json!({ "type": "SOLID", "colorHex": hex, "role": node.style.bg_role })]).unwrap_or_default(),
        "foregroundHex": fg_hex,
        "wgpuUniformBytes": WgpuMaterialSurfaceUniform::SIZE_BYTES,
        "a11y": node.a11y,
        "children": children,
    })
}

fn figcraft_json_to_node(v: &Value) -> DesignNode {
    let id = v
        .get("nodeId")
        .or_else(|| v.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("node")
        .to_owned();
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_owned();
    let kind = v
        .get("kind")
        .and_then(|k| serde_json::from_value::<DesignNodeKind>(k.clone()).ok())
        .unwrap_or_else(|| match v.get("type").and_then(Value::as_str) {
            Some("TEXT") => DesignNodeKind::Text,
            Some("LINE") => DesignNodeKind::Separator,
            Some("ELLIPSE") => DesignNodeKind::Avatar,
            _ => match v.get("layoutMode").and_then(Value::as_str) {
                Some("HORIZONTAL") => DesignNodeKind::Row,
                _ => DesignNodeKind::Column,
            },
        });

    let mut style = DesignStyle {
        direction: match v.get("layoutMode").and_then(Value::as_str) {
            Some("HORIZONTAL") => Some("row".to_owned()),
            Some("VERTICAL") => Some("column".to_owned()),
            _ => None,
        },
        width: v.get("width").and_then(Value::as_f64).map(|x| x as f32),
        height: v.get("height").and_then(Value::as_f64).map(|x| x as f32),
        gap: v
            .get("itemSpacing")
            .and_then(Value::as_f64)
            .map(|x| x as f32),
        padding: v.get("padding").and_then(Value::as_f64).map(|x| x as f32),
        corner_radius: v
            .get("cornerRadius")
            .and_then(Value::as_f64)
            .map(|x| x as f32),
        ..Default::default()
    };

    if let Some(fill) = v
        .get("fills")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
    {
        style.bg_role = fill
            .get("role")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        style.bg_hex = fill
            .get("colorHex")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }

    let children = v
        .get("children")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(figcraft_json_to_node).collect())
        .unwrap_or_default();

    DesignNode {
        id,
        name,
        kind,
        text: v
            .get("characters")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        style,
        a11y: v
            .get("a11y")
            .and_then(|a| serde_json::from_value(a.clone()).ok())
            .unwrap_or_default(),
        children,
    }
}
