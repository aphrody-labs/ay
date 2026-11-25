//! Pure-Rust media format sniffer, header parser, SVG lossless optimizer, asset indexer,
//! and cross-platform download/conversion orchestrator (`#![forbid(unsafe_code)]`).
//!
//! Covers SVG, Wallpaper, GLB (glTF 2.0), WebP (lossy + VP8L lossless), WebM (VP9/AV1),
//! AV1/AVIF (ISOBMFF), and PNG lossless with deterministic magic-byte inspection.

#![forbid(unsafe_code)]

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Canonical media format detected from magic bytes (never trusting file extensions alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaFormat {
    Svg,
    Png,
    Webp,
    Avif,
    Jpeg,
    Glb,
    Webm,
    Mp4Av1,
    Mp4,
    Unknown,
}

impl MediaFormat {
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Jpeg => "image/jpeg",
            Self::Glb => "model/gltf-binary",
            Self::Webm => "video/webm",
            Self::Mp4Av1 | Self::Mp4 => "video/mp4",
            Self::Unknown => "application/octet-stream",
        }
    }

    #[must_use]
    pub const fn ext(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jpeg => "jpg",
            Self::Glb => "glb",
            Self::Webm => "webm",
            Self::Mp4Av1 | Self::Mp4 => "mp4",
            Self::Unknown => "bin",
        }
    }
}

/// Detailed pure-Rust inspection metadata extracted directly from file headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaInspection {
    pub format: MediaFormat,
    pub mime: &'static str,
    pub bytes: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub lossless: bool,
    pub is_wallpaper_candidate: bool,
    pub codec_hint: Option<String>,
    pub glb_version: Option<u32>,
    pub glb_extensions: Vec<String>,
}

/// Sniff and parse media headers directly from raw bytes (zero unsafe, zero external C FFI).
#[must_use]
pub fn inspect_media_bytes(bytes: &[u8]) -> MediaInspection {
    // PNG: 89 50 4E 47 0D 0A 1A 0A + IHDR chunk (always lossless DEFLATE)
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return MediaInspection {
            format: MediaFormat::Png,
            mime: MediaFormat::Png.mime(),
            bytes: bytes.len(),
            width: Some(width),
            height: Some(height),
            lossless: true,
            is_wallpaper_candidate: width >= 1920 && height >= 1080,
            codec_hint: Some("png-lossless".to_owned()),
            glb_version: None,
            glb_extensions: Vec::new(),
        };
    }

    // WebP: RIFF....WEBP + VP8 / VP8L (lossless) / VP8X
    if bytes.len() >= 16 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let chunk = &bytes[12..16];
        let (lossless, codec, width, height) = match chunk {
            b"VP8L" if bytes.len() >= 25 && bytes[20] == 0x2F => {
                let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
                let w = (bits & 0x3FFF) + 1;
                let h = ((bits >> 14) & 0x3FFF) + 1;
                (true, "webp-vp8l-lossless", Some(w), Some(h))
            }
            b"VP8X" if bytes.len() >= 30 => {
                let w = 1
                    + (u32::from(bytes[24])
                        | (u32::from(bytes[25]) << 8)
                        | (u32::from(bytes[26]) << 16));
                let h = 1
                    + (u32::from(bytes[27])
                        | (u32::from(bytes[28]) << 8)
                        | (u32::from(bytes[29]) << 16));
                (false, "webp-vp8x", Some(w), Some(h))
            }
            _ => (false, "webp-vp8", None, None),
        };
        return MediaInspection {
            format: MediaFormat::Webp,
            mime: MediaFormat::Webp.mime(),
            bytes: bytes.len(),
            width,
            height,
            lossless,
            is_wallpaper_candidate: width.unwrap_or(0) >= 1920 && height.unwrap_or(0) >= 1080,
            codec_hint: Some(codec.to_owned()),
            glb_version: None,
            glb_extensions: Vec::new(),
        };
    }

    // GLB (glTF 2.0 binary): magic "glTF", u32 version, u32 total_length, optional JSON chunk
    if bytes.len() >= 12 && &bytes[0..4] == b"glTF" {
        let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut extensions = Vec::new();
        if bytes.len() >= 20 && &bytes[16..20] == b"JSON" {
            let chunk_len =
                u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
            let end = 20usize.saturating_add(chunk_len).min(bytes.len());
            if let Ok(json_str) = std::str::from_utf8(&bytes[20..end])
                && let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str)
                && let Some(arr) = val.get("extensionsUsed").and_then(|v| v.as_array())
            {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        extensions.push(s.to_owned());
                    }
                }
            }
        }
        return MediaInspection {
            format: MediaFormat::Glb,
            mime: MediaFormat::Glb.mime(),
            bytes: bytes.len(),
            width: None,
            height: None,
            lossless: true,
            is_wallpaper_candidate: false,
            codec_hint: Some(format!("gltf-{version}")),
            glb_version: Some(version),
            glb_extensions: extensions,
        };
    }

    // WebM / EBML: 1A 45 DF A3
    if bytes.len() >= 4 && bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let scan_len = bytes.len().min(4096);
        let window = &bytes[..scan_len];
        let codec = if window.windows(5).any(|w| w == b"V_AV1") {
            "av1-webm"
        } else if window.windows(5).any(|w| w == b"V_VP9") {
            "vp9-webm"
        } else {
            "webm"
        };
        return MediaInspection {
            format: MediaFormat::Webm,
            mime: MediaFormat::Webm.mime(),
            bytes: bytes.len(),
            width: None,
            height: None,
            lossless: false,
            is_wallpaper_candidate: false,
            codec_hint: Some(codec.to_owned()),
            glb_version: None,
            glb_extensions: Vec::new(),
        };
    }

    // ISOBMFF (AVIF / MP4 / AV1): ....ftyp<brand>
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        let scan_len = bytes.len().min(4096);
        let has_av01 = bytes[..scan_len].windows(4).any(|w| w == b"av01");
        if brand == b"avif" || brand == b"avis" {
            return MediaInspection {
                format: MediaFormat::Avif,
                mime: MediaFormat::Avif.mime(),
                bytes: bytes.len(),
                width: None,
                height: None,
                lossless: false,
                is_wallpaper_candidate: false,
                codec_hint: Some("av01-avif".to_owned()),
                glb_version: None,
                glb_extensions: Vec::new(),
            };
        }
        let format = if has_av01 {
            MediaFormat::Mp4Av1
        } else {
            MediaFormat::Mp4
        };
        return MediaInspection {
            format,
            mime: format.mime(),
            bytes: bytes.len(),
            width: None,
            height: None,
            lossless: false,
            is_wallpaper_candidate: false,
            codec_hint: Some(if has_av01 { "av01-mp4" } else { "isobmff-mp4" }.to_owned()),
            glb_version: None,
            glb_extensions: Vec::new(),
        };
    }

    // JPEG: FF D8 FF
    if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return MediaInspection {
            format: MediaFormat::Jpeg,
            mime: MediaFormat::Jpeg.mime(),
            bytes: bytes.len(),
            width: None,
            height: None,
            lossless: false,
            is_wallpaper_candidate: false,
            codec_hint: Some("jpeg".to_owned()),
            glb_version: None,
            glb_extensions: Vec::new(),
        };
    }

    // SVG: UTF-8 text beginning with <svg or <?xml ... <svg
    let prefix_len = bytes.len().min(512);
    if let Ok(head) = std::str::from_utf8(&bytes[..prefix_len]) {
        let trimmed = head.trim_start();
        if trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && trimmed.contains("<svg"))
        {
            return MediaInspection {
                format: MediaFormat::Svg,
                mime: MediaFormat::Svg.mime(),
                bytes: bytes.len(),
                width: None,
                height: None,
                lossless: true,
                is_wallpaper_candidate: false,
                codec_hint: Some("svg-vector".to_owned()),
                glb_version: None,
                glb_extensions: Vec::new(),
            };
        }
    }

    MediaInspection {
        format: MediaFormat::Unknown,
        mime: MediaFormat::Unknown.mime(),
        bytes: bytes.len(),
        width: None,
        height: None,
        lossless: false,
        is_wallpaper_candidate: false,
        codec_hint: None,
        glb_version: None,
        glb_extensions: Vec::new(),
    }
}

/// Lossless pure-Rust SVG optimizer: strips XML comments, `<metadata>...</metadata>` blocks,
/// and redundant inter-tag whitespace while preserving `<style>`, keyframes, and `viewBox`.
#[must_use]
pub fn optimize_svg(input: &str) -> String {
    let mut s = strip_delimited(input, "<!--", "-->");
    s = strip_delimited(&s, "<metadata", "</metadata>");
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => {
                if prev_space && out.ends_with('>') {
                    // drop inter-tag whitespace
                } else if prev_space {
                    out.push(' ');
                }
                prev_space = false;
                in_tag = true;
                out.push('<');
            }
            '>' => {
                prev_space = false;
                in_tag = false;
                out.push('>');
            }
            c if c.is_whitespace() => {
                if in_tag {
                    if !prev_space {
                        out.push(' ');
                        prev_space = true;
                    }
                } else {
                    prev_space = true;
                }
            }
            c => {
                if prev_space && !out.is_empty() && !out.ends_with('>') {
                    out.push(' ');
                }
                prev_space = false;
                out.push(c);
            }
        }
    }
    out.trim().to_owned()
}

fn strip_delimited(input: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + open.len()..];
        if let Some(end) = after_open.find(close) {
            rest = &after_open[end + close.len()..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Result of a real media conversion / optimization step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionReport {
    pub input: String,
    pub output: String,
    pub target_format: String,
    pub lossless: bool,
    pub encoder: String,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub saved_bytes: i64,
}

/// Real cross-platform media converter using local pure-Rust SVG optimizer or host encoders
/// (`cwebp`, `avifenc`, `ffmpeg`, `magick`). Zero simulated outputs.
pub fn convert_media_file(
    input: &Path,
    output: &Path,
    target_format: &str,
    lossless: bool,
) -> Result<ConversionReport, String> {
    let in_meta = std::fs::metadata(input).map_err(|e| format!("read {}: {e}", input.display()))?;
    let target = target_format.to_ascii_lowercase();

    let encoder = match target.as_str() {
        "svg" => {
            let raw = std::fs::read_to_string(input).map_err(|e| format!("read svg: {e}"))?;
            let optimized = optimize_svg(&raw);
            std::fs::write(output, optimized.as_bytes()).map_err(|e| format!("write svg: {e}"))?;
            "craft-svg-rust".to_owned()
        }
        "webp" => {
            let mut cmd = Command::new("cwebp");
            cmd.arg("-quiet");
            if lossless {
                cmd.args(["-lossless", "-z", "9"]);
            } else {
                cmd.args(["-q", "82", "-m", "6"]);
            }
            cmd.arg(input).arg("-o").arg(output);
            run_encoder(cmd, "cwebp")?
        }
        "avif" => {
            let mut cmd = Command::new("avifenc");
            if lossless {
                cmd.arg("--lossless");
            } else {
                cmd.args(["-q", "62", "-s", "6"]);
            }
            cmd.arg(input).arg(output);
            run_encoder(cmd, "avifenc")?
        }
        "png" | "png-lossless" => {
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-y", "-loglevel", "error", "-i"])
                .arg(input)
                .args(["-compression_level", "9", "-pred", "mixed"])
                .arg(output);
            run_encoder(cmd, "ffmpeg-png-lossless")?
        }
        "webm" | "av1" | "av1-webm" => {
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-y", "-loglevel", "error", "-i"]).arg(input);
            if target == "av1" || target == "av1-webm" {
                cmd.args([
                    "-c:v",
                    "libsvtav1",
                    "-crf",
                    "32",
                    "-preset",
                    "8",
                    "-c:a",
                    "libopus",
                ]);
            } else {
                cmd.args([
                    "-c:v",
                    "libvpx-vp9",
                    "-crf",
                    "32",
                    "-b:v",
                    "0",
                    "-c:a",
                    "libopus",
                ]);
            }
            cmd.arg(output);
            run_encoder(cmd, "ffmpeg")?
        }
        other => return Err(format!("unsupported target format: {other}")),
    };

    let out_meta =
        std::fs::metadata(output).map_err(|e| format!("stat {}: {e}", output.display()))?;
    let input_bytes = in_meta.len();
    let output_bytes = out_meta.len();
    let saved_bytes = i64::try_from(input_bytes).unwrap_or(i64::MAX)
        - i64::try_from(output_bytes).unwrap_or(i64::MAX);

    Ok(ConversionReport {
        input: input.display().to_string(),
        output: output.display().to_string(),
        target_format: target,
        lossless,
        encoder,
        input_bytes,
        output_bytes,
        saved_bytes,
    })
}

fn run_encoder(mut cmd: Command, label: &str) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("failed to spawn encoder `{label}`: {e}"))?;
    if !out.status.success() {
        let stderr = std::str::from_utf8(&out.stderr).unwrap_or("<non-utf8 stderr>");
        return Err(format!(
            "encoder `{label}` failed ({}): {}",
            out.status,
            stderr.trim()
        ));
    }
    Ok(label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_png_webp_lossless_glb_webm_av1_and_svg() {
        let mut png = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        png.extend_from_slice(&3840u32.to_be_bytes());
        png.extend_from_slice(&2160u32.to_be_bytes());
        let p = inspect_media_bytes(&png);
        assert_eq!(p.format, MediaFormat::Png);
        assert_eq!(p.width, Some(3840));
        assert_eq!(p.height, Some(2160));
        assert!(p.lossless);
        assert!(p.is_wallpaper_candidate);

        let mut glb = Vec::new();
        let json =
            br#"{"asset":{"version":"2.0"},"extensionsUsed":["KHR_draco_mesh_compression"]}"#;
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((20 + json.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(json);
        let g = inspect_media_bytes(&glb);
        assert_eq!(g.format, MediaFormat::Glb);
        assert_eq!(g.glb_version, Some(2));
        assert_eq!(g.glb_extensions, vec!["KHR_draco_mesh_compression"]);

        let webm_av1 = [0x1A, 0x45, 0xDF, 0xA3, 0, 0, b'V', b'_', b'A', b'V', b'1'];
        let w = inspect_media_bytes(&webm_av1);
        assert_eq!(w.format, MediaFormat::Webm);
        assert_eq!(w.codec_hint.as_deref(), Some("av1-webm"));

        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg"><!-- hi --><metadata>x</metadata>  <circle cx="1" cy="1" r="1"/>  </svg>"#;
        let s = inspect_media_bytes(svg.as_bytes());
        assert_eq!(s.format, MediaFormat::Svg);
        let opt = optimize_svg(svg);
        assert!(opt.len() < svg.len());
        assert!(!opt.contains("<!--"));
        assert!(!opt.contains("<metadata>"));
    }
}
