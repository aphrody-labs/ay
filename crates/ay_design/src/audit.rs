//! Impeccable + UI/UX Pro Max + W3C WCAG 2.2 AA deterministic craft auditor and auto-optimizer.

use serde::{Deserialize, Serialize};

use crate::{
    ir::{DesignDocument, DesignNode, DesignNodeKind},
    material3::{
        POINTER_TARGET_MIN_DP, ResolvedColorScheme, ThemeMode, WCAG_AA_LARGE, WCAG_AA_NORMAL,
        m3_bundle, nested_corner_radius, parse_hex_rgb, wcag_contrast_ratio,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditFinding {
    pub rule: String,
    pub severity: AuditSeverity,
    pub node_id: String,
    pub message: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignAuditReport {
    pub score: u8,
    pub passed: bool,
    pub error_count: usize,
    pub warning_count: usize,
    pub node_count: usize,
    pub findings: Vec<AuditFinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignOptimizeResult {
    pub before_score: u8,
    pub after_score: u8,
    pub fixes_applied: Vec<String>,
    pub report: DesignAuditReport,
}

const SLOP_TERMS: &[&str] = &["lorem ipsum", "click here", "todo:", "placeholder text"];

/// Generic marketing buzzwords and AI prose tics banned by Impeccable and Aphrody copy standards.
const BANNED_COPY_TERMS: &[&str] = &[
    "unified",
    "stack",
    "local",
    "local",
    "kinetic glass",
    "constitution",
    "seamless",
    "delve",
    "tapestry",
    "pivotal",
];

struct AuditContext<'a> {
    scheme: &'a ResolvedColorScheme,
    opposite_scheme: &'a ResolvedColorScheme,
    inherited_bg: &'a str,
    opposite_inherited_bg: &'a str,
    parent_radius: Option<f32>,
    parent_padding: f32,
    inside_card: bool,
}

/// Audit every page and node against WCAG 2.2 AA contrast (active + opposite scheme),
/// M3 touch targets, nested corner radii, W3C ARIA accessible names, typography minimums,
/// 4dp spacing rhythm, Impeccable anti-patterns (eyebrow kickers, nested cards, ghost cards,
/// competing primary CTAs), and copy hygiene.
#[must_use]
pub fn audit_document(doc: &DesignDocument) -> DesignAuditReport {
    let scheme = doc.resolved_scheme();
    let opposite_mode = if doc.theme.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    let opposite_scheme = m3_bundle().color_scheme(opposite_mode, doc.seed_hex.as_deref());
    let default_bg = scheme
        .role_hex("Surface")
        .or_else(|| {
            m3_bundle()
                .color_dark_tokens
                .get("Surface")
                .map(String::as_str)
        })
        .unwrap_or_default()
        .to_owned();
    let opposite_bg = opposite_scheme
        .role_hex("Surface")
        .unwrap_or_default()
        .to_owned();
    let mut findings = Vec::new();

    let root_ctx = AuditContext {
        scheme: &scheme,
        opposite_scheme: &opposite_scheme,
        inherited_bg: &default_bg,
        opposite_inherited_bg: &opposite_bg,
        parent_radius: None,
        parent_padding: 0.0,
        inside_card: false,
    };

    for page in &doc.pages {
        audit_node(&page.root, &root_ctx, &mut findings);
    }

    let error_count = findings
        .iter()
        .filter(|f| f.severity == AuditSeverity::Error)
        .count();
    let warning_count = findings
        .iter()
        .filter(|f| f.severity == AuditSeverity::Warning)
        .count();

    let penalty = (error_count * 12 + warning_count * 4).min(100);
    let score = (100 - penalty) as u8;
    DesignAuditReport {
        score,
        passed: error_count == 0,
        error_count,
        warning_count,
        node_count: doc.total_nodes(),
        findings,
    }
}

fn is_heading_node(node: &DesignNode) -> bool {
    if node.kind != DesignNodeKind::Text {
        return false;
    }
    if node.style.font_size.is_some_and(|sz| sz >= 20.0) {
        return true;
    }
    node.style.type_role.as_deref().is_some_and(|role| {
        let lower = role.to_ascii_lowercase();
        lower.contains("display") || lower.contains("headline") || lower.contains("title")
    })
}

fn is_eyebrow_kicker_node(node: &DesignNode) -> bool {
    if !matches!(node.kind, DesignNodeKind::Text | DesignNodeKind::Badge) {
        return false;
    }
    let id_low = node.id.to_ascii_lowercase();
    let name_low = node.name.to_ascii_lowercase();
    for marker in ["eyebrow", "kicker", "overline"] {
        if id_low.contains(marker) || name_low.contains(marker) {
            return true;
        }
    }
    let Some(text) = node.text.as_deref().map(str::trim) else {
        return false;
    };
    if text.is_empty() || text.len() > 48 {
        return false;
    }
    let letters: usize = text.chars().filter(|c| c.is_alphabetic()).count();
    let all_caps = letters >= 3 && text.chars().all(|c| !c.is_lowercase());
    let has_dot_sep = text.contains(" · ") || text.contains(" • ");
    (all_caps && node.style.font_size.is_none_or(|sz| sz <= 13.0)) || has_dot_sep
}

fn is_primary_cta(node: &DesignNode) -> bool {
    node.kind == DesignNodeKind::Button
        && node
            .style
            .bg_role
            .as_deref()
            .is_some_and(|r| r.eq_ignore_ascii_case("primary") || r.eq_ignore_ascii_case("accent"))
}

fn check_contrast(
    node: &DesignNode,
    ctx: &AuditContext<'_>,
    has_text: bool,
    findings: &mut Vec<AuditFinding>,
) {
    let own_bg = node.resolved_bg_hex(ctx.scheme).unwrap_or(ctx.inherited_bg);
    let fg = node.resolved_fg_hex(ctx.scheme);
    let font_sz = node.style.font_size.unwrap_or(14.0);
    let min_ratio = if font_sz >= 24.0 {
        WCAG_AA_LARGE
    } else {
        WCAG_AA_NORMAL
    };

    if (has_text || node.kind.is_interactive())
        && let (Some(fg_rgb), Some(bg_rgb)) = (parse_hex_rgb(fg), parse_hex_rgb(own_bg))
    {
        let ratio = wcag_contrast_ratio(fg_rgb, bg_rgb);
        if ratio < min_ratio {
            findings.push(AuditFinding {
                rule: "wcag-contrast-aa".to_owned(),
                severity: AuditSeverity::Error,
                node_id: node.id.clone(),
                message: format!(
                    "Contrast ratio {ratio:.2}:1 between fg {fg} and bg {own_bg} is below WCAG 2.2 AA ({min_ratio:.1}:1)"
                ),
                suggestion: "Pair container fill strictly with its Kotlin M3 On* foreground token"
                    .to_owned(),
            });
        }
    }

    // Check cross-scheme (Light/Dark) safety whenever raw hex colors override semantic roles.
    let uses_raw_hex = node.style.fg_hex.is_some() || node.style.bg_hex.is_some();
    if uses_raw_hex && (has_text || node.kind.is_interactive()) {
        let opp_bg = node
            .resolved_bg_hex(ctx.opposite_scheme)
            .unwrap_or(ctx.opposite_inherited_bg);
        let opp_fg = node.resolved_fg_hex(ctx.opposite_scheme);
        if let (Some(fg_rgb), Some(bg_rgb)) = (parse_hex_rgb(opp_fg), parse_hex_rgb(opp_bg)) {
            let opp_ratio = wcag_contrast_ratio(fg_rgb, bg_rgb);
            if opp_ratio < min_ratio {
                findings.push(AuditFinding {
                    rule: "wcag-cross-scheme-contrast".to_owned(),
                    severity: AuditSeverity::Warning,
                    node_id: node.id.clone(),
                    message: format!(
                        "Raw hex override drops contrast to {opp_ratio:.2}:1 in {} mode (below {min_ratio:.1}:1)",
                        ctx.opposite_scheme.mode.as_str()
                    ),
                    suggestion: "Replace raw fg_hex/bg_hex literals with paired Material 3 semantic roles (Surface/OnSurface, Primary/OnPrimary)"
                        .to_owned(),
                });
            }
        }
    }
}

fn check_card_discipline(node: &DesignNode, inside_card: bool, findings: &mut Vec<AuditFinding>) {
    if node.kind != DesignNodeKind::Card {
        return;
    }
    if inside_card {
        findings.push(AuditFinding {
            rule: "impeccable-no-nested-cards".to_owned(),
            severity: AuditSeverity::Warning,
            node_id: node.id.clone(),
            message: "Card is nested inside another Card container".to_owned(),
            suggestion: "Flatten inner Card into a borderless Column or tonal section".to_owned(),
        });
    }
    let has_border =
        node.style.border_role.is_some() || node.style.border_width.is_some_and(|w| w > 0.0);
    let has_elevation = node
        .style
        .elevation_level
        .as_deref()
        .is_some_and(|lvl| !lvl.eq_ignore_ascii_case("level0"));
    if has_border && has_elevation {
        findings.push(AuditFinding {
            rule: "impeccable-no-ghost-card".to_owned(),
            severity: AuditSeverity::Warning,
            node_id: node.id.clone(),
            message: "Card combines a border with shadow/tonal elevation (ghost card)".to_owned(),
            suggestion:
                "Declare elevation once: use either border_role or elevation_level, never both"
                    .to_owned(),
        });
    }
}

fn check_copy_hygiene(node: &DesignNode, findings: &mut Vec<AuditFinding>) {
    let Some(txt) = &node.text else {
        return;
    };
    let low = txt.to_ascii_lowercase();
    for term in SLOP_TERMS {
        if low.contains(term) {
            findings.push(AuditFinding {
                rule: "zero-slop-copy".to_owned(),
                severity: AuditSeverity::Warning,
                node_id: node.id.clone(),
                message: format!("Placeholder copy detected: '{term}'"),
                suggestion: "Replace with concise, action-oriented domain copy".to_owned(),
            });
        }
    }
    for term in BANNED_COPY_TERMS {
        if contains_word(&low, term) {
            findings.push(AuditFinding {
                rule: "zero-banned-buzzwords".to_owned(),
                severity: AuditSeverity::Warning,
                node_id: node.id.clone(),
                message: format!("Banned buzzword or AI slop term detected: '{term}'"),
                suggestion: "Use concrete domain names and plain active verbs".to_owned(),
            });
        }
    }
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let idx = start + pos;
        let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
        let after_idx = idx + nlen;
        let after_ok = after_idx == bytes.len() || !bytes[after_idx].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        start = idx + 1;
    }
    false
}

fn check_children_patterns(node: &DesignNode, findings: &mut Vec<AuditFinding>) {
    let primary_buttons = node.children.iter().filter(|c| is_primary_cta(c)).count();
    if primary_buttons > 1 {
        findings.push(AuditFinding {
            rule: "craft-single-primary-cta".to_owned(),
            severity: AuditSeverity::Warning,
            node_id: node.id.clone(),
            message: format!(
                "Container has {primary_buttons} primary-filled CTAs competing for focus"
            ),
            suggestion: "Keep one primary CTA and demote secondary actions to SecondaryContainer"
                .to_owned(),
        });
    }

    for pair in node.children.windows(2) {
        if is_eyebrow_kicker_node(&pair[0]) && is_heading_node(&pair[1]) {
            findings.push(AuditFinding {
                rule: "impeccable-no-eyebrow-kicker".to_owned(),
                severity: AuditSeverity::Warning,
                node_id: pair[0].id.clone(),
                message: format!(
                    "Eyebrow or kicker '{}' sits directly above heading '{}'",
                    pair[0].id, pair[1].id
                ),
                suggestion: "Delete the eyebrow/kicker label and let the heading speak directly"
                    .to_owned(),
            });
        }
    }
}

fn audit_node(node: &DesignNode, ctx: &AuditContext<'_>, findings: &mut Vec<AuditFinding>) {
    let own_bg = node.resolved_bg_hex(ctx.scheme).unwrap_or(ctx.inherited_bg);
    let opp_bg = node
        .resolved_bg_hex(ctx.opposite_scheme)
        .unwrap_or(ctx.opposite_inherited_bg);
    let has_text = node.text.as_deref().is_some_and(|t| !t.trim().is_empty());

    check_contrast(node, ctx, has_text, findings);
    check_card_discipline(node, ctx.inside_card, findings);

    if matches!(node.kind, DesignNodeKind::Button | DesignNodeKind::Input)
        && let Some(h) = node.style.height
        && h < POINTER_TARGET_MIN_DP
    {
        findings.push(AuditFinding {
            rule: "touch-target-min".to_owned(),
            severity: AuditSeverity::Error,
            node_id: node.id.clone(),
            message: format!(
                "Interactive control height {h}dp is below M3 pointer minimum ({POINTER_TARGET_MIN_DP}dp)"
            ),
            suggestion: format!("Raise height to at least {POINTER_TARGET_MIN_DP}dp"),
        });
    }

    if node.kind.is_interactive()
        && !has_text
        && node
            .a11y
            .aria_label
            .as_deref()
            .is_none_or(|l| l.trim().is_empty())
    {
        findings.push(AuditFinding {
            rule: "aria-accessible-name".to_owned(),
            severity: AuditSeverity::Error,
            node_id: node.id.clone(),
            message: "Interactive control has neither visible text nor aria_label".to_owned(),
            suggestion: "Set `a11y.aria_label` or visible label text".to_owned(),
        });
    }

    let rad = node.resolved_radius();
    if let Some(outer_rad) = ctx.parent_radius
        && outer_rad > 0.0
        && outer_rad < 999.0
        && rad > 0.0
        && rad < 999.0
    {
        let expected_max = nested_corner_radius(outer_rad, ctx.parent_padding);
        if rad > expected_max + 0.5 {
            findings.push(AuditFinding {
                rule: "nested-corner-radius".to_owned(),
                severity: AuditSeverity::Warning,
                node_id: node.id.clone(),
                message: format!(
                    "Nested corner radius {rad}dp exceeds M3 outer-padding bound ({expected_max}dp)"
                ),
                suggestion: format!("Set corner radius <= {expected_max}dp"),
            });
        }
    }

    if let Some(sz) = node.style.font_size
        && sz < 11.0
    {
        findings.push(AuditFinding {
            rule: "typography-floor".to_owned(),
            severity: AuditSeverity::Warning,
            node_id: node.id.clone(),
            message: format!("Font size {sz}sp is below M3 LabelSmall floor (11sp)"),
            suggestion: "Use M3 BodyMedium (14sp) or LabelMedium (12sp)".to_owned(),
        });
    }

    if let Some(pad) = node.style.padding
        && pad > 0.0
        && (pad % 4.0).abs() > 0.01
    {
        findings.push(AuditFinding {
            rule: "spacing-rhythm-4dp".to_owned(),
            severity: AuditSeverity::Warning,
            node_id: node.id.clone(),
            message: format!("Padding {pad}dp is off the 4dp Material 3 spacing grid"),
            suggestion: format!("Snap padding to {}dp", (pad / 4.0).round() * 4.0),
        });
    }

    if node.kind == DesignNodeKind::Progress
        && !has_text
        && node
            .a11y
            .aria_label
            .as_deref()
            .is_none_or(|l| l.trim().is_empty())
    {
        findings.push(AuditFinding {
            rule: "impeccable-no-bare-spinner".to_owned(),
            severity: AuditSeverity::Warning,
            node_id: node.id.clone(),
            message: "Progress/spinner node has no descriptive status text or aria_label".to_owned(),
            suggestion: "Replace bare spinners with informative status text or perception motion plus aria_label".to_owned(),
        });
    }

    check_copy_hygiene(node, findings);
    check_children_patterns(node, findings);

    let next_ctx = AuditContext {
        scheme: ctx.scheme,
        opposite_scheme: ctx.opposite_scheme,
        inherited_bg: own_bg,
        opposite_inherited_bg: opp_bg,
        parent_radius: if rad > 0.0 {
            Some(rad)
        } else {
            ctx.parent_radius
        },
        parent_padding: node.style.padding.unwrap_or(0.0),
        inside_card: ctx.inside_card || node.kind == DesignNodeKind::Card,
    };
    for child in &node.children {
        audit_node(child, &next_ctx, findings);
    }
}

/// Auto-optimize a `DesignDocument` in place to fix WCAG AA contrast violations (both schemes),
/// undersized touch targets, missing ARIA names, nested corner radii, 4dp spacing rhythm,
/// nested/ghost cards, eyebrow kickers, and competing primary CTAs.
pub fn optimize_document(doc: &mut DesignDocument) -> DesignOptimizeResult {
    let before = audit_document(doc);
    let scheme = doc.resolved_scheme();
    let opposite_mode = if doc.theme.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    let opposite_scheme = m3_bundle().color_scheme(opposite_mode, doc.seed_hex.as_deref());
    let default_bg = scheme
        .role_hex("Surface")
        .or_else(|| {
            m3_bundle()
                .color_dark_tokens
                .get("Surface")
                .map(String::as_str)
        })
        .unwrap_or_default()
        .to_owned();
    let opposite_bg = opposite_scheme
        .role_hex("Surface")
        .unwrap_or_default()
        .to_owned();
    let mut fixes = Vec::new();

    let root_ctx = AuditContext {
        scheme: &scheme,
        opposite_scheme: &opposite_scheme,
        inherited_bg: &default_bg,
        opposite_inherited_bg: &opposite_bg,
        parent_radius: None,
        parent_padding: 0.0,
        inside_card: false,
    };

    for page in &mut doc.pages {
        optimize_node(&mut page.root, &root_ctx, &mut fixes);
    }

    let after = audit_document(doc);
    DesignOptimizeResult {
        before_score: before.score,
        after_score: after.score,
        fixes_applied: fixes,
        report: after,
    }
}

fn fix_spacing_and_targets(node: &mut DesignNode, fixes: &mut Vec<String>) {
    if let Some(pad) = node.style.padding
        && pad > 0.0
        && (pad % 4.0).abs() > 0.01
    {
        let snapped = ((pad / 4.0).round() * 4.0).max(4.0);
        node.style.padding = Some(snapped);
        fixes.push(format!("{}: snapped padding {pad} -> {snapped}dp", node.id));
    }
    if let Some(gap) = node.style.gap
        && gap > 0.0
        && (gap % 4.0).abs() > 0.01
    {
        let snapped = ((gap / 4.0).round() * 4.0).max(4.0);
        node.style.gap = Some(snapped);
        fixes.push(format!("{}: snapped gap {gap} -> {snapped}dp", node.id));
    }
    if matches!(node.kind, DesignNodeKind::Button | DesignNodeKind::Input)
        && let Some(h) = node.style.height
        && h < POINTER_TARGET_MIN_DP
    {
        node.style.height = Some(POINTER_TARGET_MIN_DP);
        fixes.push(format!(
            "{}: raised target height {h} -> {POINTER_TARGET_MIN_DP}dp",
            node.id
        ));
    }
    if let Some(sz) = node.style.font_size
        && sz < 11.0
    {
        node.style.font_size = Some(12.0);
        fixes.push(format!("{}: raised font size {sz} -> 12sp", node.id));
    }
}

fn fix_card_and_a11y(node: &mut DesignNode, inside_card: bool, fixes: &mut Vec<String>) {
    if node.kind == DesignNodeKind::Card && inside_card {
        node.kind = DesignNodeKind::Column;
        node.style.border_role = None;
        node.style.border_width = None;
        node.style.elevation_level = None;
        fixes.push(format!("{}: flattened nested Card into Column", node.id));
    } else if node.kind == DesignNodeKind::Card {
        let has_border =
            node.style.border_role.is_some() || node.style.border_width.is_some_and(|w| w > 0.0);
        let has_elevation = node
            .style
            .elevation_level
            .as_deref()
            .is_some_and(|lvl| !lvl.eq_ignore_ascii_case("level0"));
        if has_border && has_elevation {
            node.style.border_role = None;
            node.style.border_width = None;
            fixes.push(format!(
                "{}: cleared redundant border on elevated Card (ghost card)",
                node.id
            ));
        }
    }

    if node.kind == DesignNodeKind::Progress {
        let has_text = node.text.as_deref().is_some_and(|t| !t.trim().is_empty());
        let has_aria = node
            .a11y
            .aria_label
            .as_deref()
            .is_some_and(|l| !l.trim().is_empty());
        if !has_text && !has_aria {
            let label = format!("Loading {}", node.name.replace(['-', '_'], " "));
            node.a11y.aria_label = Some(label.clone());
            fixes.push(format!(
                "{}: added status aria_label '{label}' on Progress",
                node.id
            ));
        }
    }

    if node.kind.is_interactive() {
        node.a11y.focusable = true;
        let has_text = node.text.as_deref().is_some_and(|t| !t.trim().is_empty());
        let has_aria = node
            .a11y
            .aria_label
            .as_deref()
            .is_some_and(|l| !l.trim().is_empty());
        if !has_text && !has_aria {
            let label = node.name.replace(['-', '_'], " ");
            node.a11y.aria_label = Some(label.clone());
            fixes.push(format!("{}: added aria_label '{label}'", node.id));
        }
    }
}

fn fix_contrast(node: &mut DesignNode, ctx: &AuditContext<'_>, fixes: &mut Vec<String>) {
    let min_ratio = if node.style.font_size.unwrap_or(14.0) >= 24.0 {
        WCAG_AA_LARGE
    } else {
        WCAG_AA_NORMAL
    };
    let own_bg = node.resolved_bg_hex(ctx.scheme).unwrap_or(ctx.inherited_bg);
    let fg = node.resolved_fg_hex(ctx.scheme);
    let fails_active = matches!(
        (parse_hex_rgb(fg), parse_hex_rgb(own_bg)),
        (Some(f), Some(b)) if wcag_contrast_ratio(f, b) < min_ratio
    );
    let opp_bg = node
        .resolved_bg_hex(ctx.opposite_scheme)
        .unwrap_or(ctx.opposite_inherited_bg);
    let opp_fg = node.resolved_fg_hex(ctx.opposite_scheme);
    let fails_opposite = (node.style.fg_hex.is_some() || node.style.bg_hex.is_some())
        && matches!(
            (parse_hex_rgb(opp_fg), parse_hex_rgb(opp_bg)),
            (Some(f), Some(b)) if wcag_contrast_ratio(f, b) < min_ratio
        );

    if fails_active || fails_opposite {
        if node.style.bg_hex.is_some() && node.style.bg_role.is_none() {
            node.style.bg_role = Some("SurfaceContainerLow".to_owned());
        }
        node.style.bg_hex = None;
        let paired = node
            .style
            .bg_role
            .as_deref()
            .map(ResolvedColorScheme::paired_on_role)
            .unwrap_or("OnSurface");
        node.style.fg_hex = None;
        node.style.fg_role = Some(paired.to_owned());
        fixes.push(format!(
            "{}: paired fg_role -> {paired} for WCAG AA",
            node.id
        ));
    }
}

fn fix_children_hierarchy(node: &mut DesignNode, fixes: &mut Vec<String>) {
    // Strip eyebrow/kicker nodes sitting directly above headings.
    let mut keep = vec![true; node.children.len()];
    for idx in 0..node.children.len().saturating_sub(1) {
        if is_eyebrow_kicker_node(&node.children[idx]) && is_heading_node(&node.children[idx + 1]) {
            keep[idx] = false;
            fixes.push(format!(
                "{}: removed eyebrow/kicker above heading '{}'",
                node.children[idx].id,
                node.children[idx + 1].id
            ));
        }
    }
    let mut iter = keep.into_iter();
    node.children.retain(|_| iter.next().unwrap_or(true));

    // Demote competing primary CTAs beyond the first to SecondaryContainer (tonal).
    let mut primary_seen = 0usize;
    for child in &mut node.children {
        if is_primary_cta(child) {
            primary_seen += 1;
            if primary_seen > 1 {
                child.style.bg_role = Some("SecondaryContainer".to_owned());
                child.style.fg_role = Some("OnSecondaryContainer".to_owned());
                fixes.push(format!(
                    "{}: demoted competing primary CTA to SecondaryContainer",
                    child.id
                ));
            }
        }
    }
}

fn optimize_node(node: &mut DesignNode, ctx: &AuditContext<'_>, fixes: &mut Vec<String>) {
    fix_spacing_and_targets(node, fixes);
    fix_card_and_a11y(node, ctx.inside_card, fixes);

    let rad = node.resolved_radius();
    if let Some(outer) = ctx.parent_radius
        && outer > 0.0
        && outer < 999.0
        && rad > 0.0
        && rad < 999.0
    {
        let bound = nested_corner_radius(outer, ctx.parent_padding);
        if rad > bound + 0.5 {
            node.style.corner_radius = Some(bound);
            node.style.shape_token = None;
            fixes.push(format!(
                "{}: adjusted nested corner radius {rad} -> {bound}dp",
                node.id
            ));
        }
    }

    fix_contrast(node, ctx, fixes);
    fix_children_hierarchy(node, fixes);

    let own_bg = node
        .resolved_bg_hex(ctx.scheme)
        .unwrap_or(ctx.inherited_bg)
        .to_owned();
    let opp_bg = node
        .resolved_bg_hex(ctx.opposite_scheme)
        .unwrap_or(ctx.opposite_inherited_bg)
        .to_owned();
    let next_rad = {
        let r = node.resolved_radius();
        if r > 0.0 { Some(r) } else { ctx.parent_radius }
    };
    let next_ctx = AuditContext {
        scheme: ctx.scheme,
        opposite_scheme: ctx.opposite_scheme,
        inherited_bg: &own_bg,
        opposite_inherited_bg: &opp_bg,
        parent_radius: next_rad,
        parent_padding: node.style.padding.unwrap_or(0.0),
        inside_card: ctx.inside_card || node.kind == DesignNodeKind::Card,
    };
    for child in &mut node.children {
        optimize_node(child, &next_ctx, fixes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_rule(report: &DesignAuditReport, rule: &str) -> bool {
        report.findings.iter().any(|f| f.rule == rule)
    }

    #[test]
    fn catches_and_fixes_eyebrow_kicker_abuse() {
        let mut eyebrow =
            DesignNode::new("section-eyebrow", DesignNodeKind::Text).with_text("CATALOGUE · APPS");
        eyebrow.style.font_size = Some(12.0);
        let mut heading =
            DesignNode::new("section-title", DesignNodeKind::Text).with_text("Applications");
        heading.style.font_size = Some(28.0);
        let root = DesignNode::new("root", DesignNodeKind::Column)
            .with_child(eyebrow)
            .with_child(heading);

        let mut doc = DesignDocument::new("Eyebrow Test", root);
        let before = audit_document(&doc);
        assert!(has_rule(&before, "impeccable-no-eyebrow-kicker"));

        let opt = optimize_document(&mut doc);
        assert!(opt.after_score > opt.before_score);
        assert!(!has_rule(&opt.report, "impeccable-no-eyebrow-kicker"));
    }

    #[test]
    fn catches_and_fixes_nested_cards_and_ghost_cards() {
        let mut inner_card = DesignNode::new("inner-card", DesignNodeKind::Card);
        inner_card.style.bg_role = Some("SurfaceContainer".to_owned());
        inner_card.style.border_role = Some("OutlineVariant".to_owned());

        let mut outer_card = DesignNode::new("outer-card", DesignNodeKind::Card);
        outer_card.style.bg_role = Some("SurfaceContainerLow".to_owned());
        outer_card.style.border_role = Some("OutlineVariant".to_owned());
        outer_card.style.elevation_level = Some("Level2".to_owned());
        outer_card.children.push(inner_card);

        let root = DesignNode::new("root", DesignNodeKind::Column).with_child(outer_card);
        let mut doc = DesignDocument::new("Card Craft", root);

        let before = audit_document(&doc);
        assert!(has_rule(&before, "impeccable-no-nested-cards"));
        assert!(has_rule(&before, "impeccable-no-ghost-card"));

        let opt = optimize_document(&mut doc);
        assert!(opt.after_score > opt.before_score);
        assert!(!has_rule(&opt.report, "impeccable-no-nested-cards"));
        assert!(!has_rule(&opt.report, "impeccable-no-ghost-card"));
    }

    #[test]
    fn catches_and_fixes_competing_primary_ctas() {
        let mut b1 = DesignNode::new("cta-launch", DesignNodeKind::Button).with_text("Launch");
        b1.style.bg_role = Some("Primary".to_owned());
        b1.style.height = Some(48.0);
        let mut b2 = DesignNode::new("cta-export", DesignNodeKind::Button).with_text("Export");
        b2.style.bg_role = Some("Primary".to_owned());
        b2.style.height = Some(48.0);

        let root = DesignNode::new("toolbar", DesignNodeKind::Row)
            .with_child(b1)
            .with_child(b2);
        let mut doc = DesignDocument::new("CTA Test", root);

        let before = audit_document(&doc);
        assert!(has_rule(&before, "craft-single-primary-cta"));

        let opt = optimize_document(&mut doc);
        assert!(opt.after_score > opt.before_score);
        assert!(!has_rule(&opt.report, "craft-single-primary-cta"));
    }

    #[test]
    fn catches_and_fixes_missing_accessible_names() {
        let mut icon_btn = DesignNode::new("close-panel", DesignNodeKind::Button);
        icon_btn.style.height = Some(48.0);
        icon_btn.style.bg_role = Some("SecondaryContainer".to_owned());

        let root = DesignNode::new("bar", DesignNodeKind::Row).with_child(icon_btn);
        let mut doc = DesignDocument::new("A11y Test", root);

        let before = audit_document(&doc);
        assert!(!before.passed);
        assert!(has_rule(&before, "aria-accessible-name"));

        let opt = optimize_document(&mut doc);
        assert!(opt.report.passed);
        assert!(opt.after_score > opt.before_score);
        assert!(!has_rule(&opt.report, "aria-accessible-name"));
    }

    #[test]
    fn catches_and_fixes_dark_and_light_contrast_pairs() {
        // Hardcoded dark-mode light ink passes on dark Surface, but breaks on light Surface.
        let mut raw_text =
            DesignNode::new("raw-label", DesignNodeKind::Text).with_text("Session Wayland");
        raw_text.style.fg_hex = m3_bundle().color_dark_tokens.get("OnSurface").cloned();

        let mut root = DesignNode::new("root", DesignNodeKind::Column).with_child(raw_text);
        root.style.bg_role = Some("Surface".to_owned());

        let mut doc = DesignDocument::new("Cross Scheme", root);
        let before = audit_document(&doc);
        assert!(has_rule(&before, "wcag-cross-scheme-contrast"));

        let opt = optimize_document(&mut doc);
        assert!(opt.after_score > opt.before_score);
        assert!(!has_rule(&opt.report, "wcag-cross-scheme-contrast"));
        assert!(!has_rule(&opt.report, "wcag-contrast-aa"));
    }

    #[test]
    fn catches_banned_buzzwords_and_slop_copy() {
        let buzz = DesignNode::new("hero-copy", DesignNodeKind::Text)
            .with_text("A seamless platform for your workflow");
        let slop = DesignNode::new("cta-copy", DesignNodeKind::Text).with_text("Click here");
        let root = DesignNode::new("root", DesignNodeKind::Column)
            .with_child(buzz)
            .with_child(slop);
        let doc = DesignDocument::new("Copy Test", root);

        let report = audit_document(&doc);
        assert!(has_rule(&report, "zero-banned-buzzwords"));
        assert!(has_rule(&report, "zero-slop-copy"));
    }
}
