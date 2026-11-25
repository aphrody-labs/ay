//! `design-engine`: Pure-Rust consolidated design system & multi-target generator.
//!
//! Unites:
//! - Pure Kotlin (`androidx.compose.material3.tokens`) JSON tokens (`configs/design/material3.json`)
//! - FigCraft document IR & lossless JSON bridge
//! - Impeccable + UI/UX Pro Max + W3C WCAG 2.2 AA auditor & auto-optimizer
//! - 12 codegen/export targets: `gpui`, `rsx` (Dioxus), `tsx` (React/NSH), `ts` (Bun),
//!   `iced`, `bevy`, `egui`, `taffy` (WGPU 64-byte uniforms), `wgsl`, `css`, `design_md`, `figcraft`
//! - Reverse design synthesis from Code, Recon, Index, Sitemap, PNG Image / Video keyframes,
//!   `agent-desktop` UIA/AT-SPI/AX trees, and `agent-browser` CDP snapshots.

#![forbid(unsafe_code)]

pub mod audit;
pub mod codegen;
pub mod ingest;
pub mod ir;
pub mod material3;
pub mod system;
pub mod themes;

pub use audit::{
    AuditFinding, AuditSeverity, DesignAuditReport, DesignOptimizeResult, audit_document,
    optimize_document,
};
pub use codegen::{CodeTarget, GeneratedArtifact, generate_code, generate_code_with_system};
#[cfg(feature = "png-ingest")]
pub use ingest::{
    ImageAnalysis, VideoKeyframeInput, analyze_png_bytes, from_png_bytes, from_video_keyframes,
};
pub use ingest::{
    from_agent_browser_snapshot, from_agent_desktop_tree, from_code, from_index_json, from_recon,
    from_sitemap_xml,
};
pub use ir::{
    DesignA11y, DesignDocument, DesignNode, DesignNodeKind, DesignPage, DesignSourceKind,
    DesignStyle, SurfaceMode,
};
pub use material3::{
    MATERIAL3_KOTLIN_JSON, Material3KotlinBundle, ResolvedColorScheme, SurfaceUniformSpec,
    TaffyMaterialLayout, ThemeMode, TypeScaleEntry, WgpuMaterialSurfaceUniform, m3_bundle,
    wcag_contrast_ratio,
};
pub use system::{
    DensityScale, DesignSystemConfig, DesignSystemPreset, MotionConfig, TypographyConfig,
};
pub use themes::{
    AndroidDesktopSpec, DESKTOP_SYSTEMS_JSON, DesktopColorMode, DesktopPresetsCatalog,
    DesktopThemeKind, OmarchyThemePalette, TiledWindowRect, TilingLayoutResult, TilingOptions,
    WindowTilingEngine, compute_tiling_layout, desktop_material_fragment_wgsl,
    desktop_presets_catalog, list_omarchy_themes, load_omarchy_theme, select_theme_preset,
    select_theme_preset_with_tiling, wgsl_desktop_material_helpers,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "png-ingest")]
    fn encode_test_png(width: u32, height: u32, fill_top: [u8; 4], fill_body: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, width, height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("png header");
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            for y in 0..height {
                for x in 0..width {
                    let off = ((y * width + x) * 4) as usize;
                    let c = if y < height / 10 { fill_top } else { fill_body };
                    pixels[off..off + 4].copy_from_slice(&c);
                }
            }
            writer.write_image_data(&pixels).expect("png pixels");
        }
        out
    }

    #[test]
    fn loads_pure_kotlin_m3_json_and_packs_64_byte_wgpu_uniform() {
        let m3 = m3_bundle();
        assert_eq!(m3.namespace, "androidx.compose.material3");
        assert_eq!(m3.tokens_namespace, "androidx.compose.material3.tokens");
        assert_eq!(m3.color_light_tokens.len(), 47);
        assert_eq!(m3.color_dark_tokens.len(), 47);
        let raw_json: serde_json::Value =
            serde_json::from_str(MATERIAL3_KOTLIN_JSON).expect("parse material3.json");
        assert_eq!(
            m3.color_light_tokens.get("Primary").map(String::as_str),
            raw_json["ColorLightTokens"]["Primary"].as_str()
        );
        assert_eq!(
            m3.color_dark_tokens.get("Primary").map(String::as_str),
            raw_json["ColorDarkTokens"]["Primary"].as_str()
        );
        assert_eq!(m3.shape_dp("CornerLarge"), Some(16.0));

        let u = m3.surface_uniform(SurfaceUniformSpec {
            mode: ThemeMode::Dark,
            seed_override: None,
            bg_role: "SurfaceContainerLow",
            fg_role: None,
            border_role: None,
            shape_token: "CornerLarge",
            elevation_level: "Level1",
            padding_h_dp: 16.0,
            padding_v_dp: 16.0,
        });
        assert_eq!(std::mem::size_of::<WgpuMaterialSurfaceUniform>(), 64);
        let bytes = u.to_le_bytes();
        assert_eq!(bytes.len(), 64);
        assert_eq!(f32::from_le_bytes(bytes[48..52].try_into().unwrap()), 16.0);
        assert_eq!(f32::from_le_bytes(bytes[52..56].try_into().unwrap()), 1.0);
    }

    #[test]
    fn audits_and_auto_optimizes_wcag_touch_target_aria_and_nested_radius() {
        let mut root = DesignNode::new("root", DesignNodeKind::Column);
        root.style.bg_role = Some("Surface".to_owned());
        root.style.padding = Some(10.0); // off 4dp grid!
        root.style.corner_radius = Some(16.0);

        let mut bad_btn = DesignNode::new("tiny-btn", DesignNodeKind::Button);
        bad_btn.style.height = Some(24.0); // below 44dp pointer target!
        bad_btn.style.bg_role = Some("Primary".to_owned());
        bad_btn.style.fg_hex = m3_bundle().color_dark_tokens.get("Primary").cloned(); // 1:1 contrast violation!
        bad_btn.style.corner_radius = Some(14.0); // violates nested 16 - 10 = 6dp!
        // also missing text and aria_label!
        root.children.push(bad_btn);

        let mut doc = DesignDocument::new("Audit Demo", root);
        let before = audit_document(&doc);
        assert!(!before.passed);
        assert!(before.error_count >= 3);

        let opt = optimize_document(&mut doc);
        assert!(opt.after_score > opt.before_score);
        assert!(opt.report.passed);
        assert_eq!(opt.report.error_count, 0);
        assert!(opt.fixes_applied.len() >= 4);
    }

    #[test]
    fn emits_all_12_targets_including_gpui_rsx_tsx_ts_iced_bevy_egui_taffy_and_figcraft() {
        let code_src = r#"
            <h1>Workspace</h1>
            <nav>"Overview"</nav>
            <input placeholder="Search symbols" />
            <button>Launch Build</button>
        "#;
        let mut doc = from_code(code_src, "StudioShell");
        let _ = optimize_document(&mut doc);

        for (target, needle) in [
            (CodeTarget::Gpui, "impl Render for StudioShell"),
            (CodeTarget::Rsx, "rsx! {"),
            (CodeTarget::Tsx, "export function StudioShell()"),
            (CodeTarget::TsBun, "from \"bun:aphrody\""),
            (CodeTarget::Iced, "use iced::widget::"),
            (CodeTarget::Bevy, "use bevy::"),
            (CodeTarget::Egui, "Frame::new()"),
            (CodeTarget::Taffy, "WgpuMaterialSurfaceUniform"),
            (CodeTarget::Wgsl, "struct MaterialSurfaceUniform"),
            (CodeTarget::Css, "--md-sys-color-primary:"),
            (CodeTarget::DesignMd, "# DESIGN — StudioShell"),
            (CodeTarget::Figcraft, "\"format\": \"figcraft\""),
        ] {
            let artifact = generate_code(&doc, target);
            assert!(
                artifact.code.contains(needle),
                "target {target:?} missing '{needle}':\n{}",
                artifact.code
            );
        }

        // Lossless FigCraft JSON round-trip
        let fig_json = generate_code(&doc, CodeTarget::Figcraft).code;
        let imported = DesignDocument::from_figcraft_json(&fig_json).expect("figcraft roundtrip");
        assert_eq!(imported.source, DesignSourceKind::Figcraft);
        assert!(imported.total_nodes() >= 3);
    }

    #[test]
    fn synthesizes_designs_from_recon_sitemap_index_png_video_desktop_and_browser() {
        let recon = from_recon(
            "## Dashboard\n- Metrics Grid\n## Settings\n- Theme Picker\n",
            "Ops",
        );
        assert_eq!(recon.source, DesignSourceKind::Recon);
        assert_eq!(recon.pages.len(), 2);

        let sitemap = from_sitemap_xml(
            "<urlset><url><loc>https://example.com/</loc></url><url><loc>https://example.com/docs/gpu</loc></url></urlset>",
            "DocsSite",
        );
        assert_eq!(sitemap.source, DesignSourceKind::Sitemap);
        assert_eq!(sitemap.pages.len(), 2);

        let idx =
            from_index_json(r#"{"routes":["/","/gpu","/design"]}"#, "IndexApp").expect("index");
        assert_eq!(idx.source, DesignSourceKind::Index);

        #[cfg(feature = "png-ingest")]
        {
            let png_dark = encode_test_png(64, 48, [36, 30, 56, 255], [20, 18, 24, 255]);
            let (img_doc, analysis) =
                from_png_bytes(&png_dark, "CaptureDark").expect("png capture");
            assert_eq!(img_doc.source, DesignSourceKind::ImageCapture);
            assert_eq!(analysis.detected_theme, ThemeMode::Dark);
            assert_eq!(analysis.width, 64);

            let png_light = encode_test_png(64, 48, [103, 80, 164, 255], [248, 244, 255, 255]);
            let video = from_video_keyframes(
                &[
                    VideoKeyframeInput {
                        timestamp_sec: 0.0,
                        label: "Intro".to_owned(),
                        png_bytes: png_dark,
                    },
                    VideoKeyframeInput {
                        timestamp_sec: 2.5,
                        label: "Reveal".to_owned(),
                        png_bytes: png_light,
                    },
                ],
                "LaunchReel",
            )
            .expect("video storyboard");
            assert_eq!(video.source, DesignSourceKind::VideoCapture);
            assert_eq!(video.pages.len(), 2);
        }

        let desktop = from_agent_desktop_tree(
            r#"{"role":"Window","name":"IDE","children":[{"role":"TabItem","name":"Explorer"},{"role":"Button","name":"Run"}]}"#,
            "DesktopIDE",
        );
        assert_eq!(desktop.source, DesignSourceKind::AgentDesktop);
        assert!(desktop.total_nodes() >= 4);

        let browser = from_agent_browser_snapshot(
            "- heading \"Aphrody Studio\" [level=1]\n- link \"Docs\" [ref=@e1]\n- textbox \"Search\" [ref=@e2]\n- button \"Generate\" [ref=@e3]\n",
            "StudioWeb",
        );
        assert_eq!(browser.source, DesignSourceKind::AgentBrowser);
        assert!(browser.total_nodes() >= 5);
    }

    #[test]
    fn emits_all_targets_and_swiftui_plus_material_wgsl_for_desktop_presets() {
        let apple = select_theme_preset_with_tiling(
            DesktopThemeKind::AppleLiquidGlass,
            DesktopColorMode::Dark,
            Some(WindowTilingEngine::HyprMacBsp),
            None,
            0.78,
        )
        .expect("apple preset");
        let swift = generate_code(&apple, CodeTarget::SwiftUi);
        assert_eq!(swift.extension, "swift");
        assert!(swift.code.contains("NavigationSplitView"));
        assert!(swift.code.contains("NSToolbarItemGroup"));
        assert!(swift.code.contains("NSScrollEdgeEffectStyle.hard"));
        assert!(swift.code.contains("fullScreenAllowsTiling"));

        let wgsl = generate_code(&apple, CodeTarget::Wgsl);
        assert!(wgsl.code.contains("liquid_glass_refraction_edge"));
        assert!(wgsl.code.contains("fluent_mica_acrylic_noise"));
        assert!(wgsl.code.contains("googlebook_gemini_edge_glow"));

        let frag = desktop_material_fragment_wgsl(
            DesktopThemeKind::FluentMica,
            DesktopColorMode::HighContrast,
            0.85,
        );
        assert!(frag.contains("fn fs_main"));
    }
}
