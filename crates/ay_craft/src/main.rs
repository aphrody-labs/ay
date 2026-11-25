#![forbid(unsafe_code)]

use std::{env, path::Path, process::ExitCode};

use aphrody_craft::{
    APHRODITE_BRAND_SVG, brand_theme, browser_media_codecs, compiled_apps, dns_routes,
    media::{convert_media_file, inspect_media_bytes, optimize_svg},
    surface_theme_matrix,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("info");

    match cmd {
        "info" | "list" => {
            let payload = serde_json::json!({
                "binary": "craft",
                "version": env!("CARGO_PKG_VERSION"),
                "brand": brand_theme(),
                "apps": compiled_apps(),
                "codecs": browser_media_codecs(),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        "codecs" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&browser_media_codecs()).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        "brand" => {
            if args.iter().any(|a| a == "--svg") {
                println!("{APHRODITE_BRAND_SVG}");
            } else {
                let payload = serde_json::json!({
                    "theme": brand_theme(),
                    "surfaces": surface_theme_matrix(),
                    "svgBytes": APHRODITE_BRAND_SVG.len(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).unwrap_or_default()
                );
            }
            ExitCode::SUCCESS
        }
        "dns" => {
            let domain = args.get(1).map(String::as_str).unwrap_or("aphrody.app");
            println!(
                "{}",
                serde_json::to_string_pretty(&dns_routes(domain)).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        "inspect" => {
            let Some(path) = args.get(1) else {
                eprintln!("usage: craft inspect <file>");
                return ExitCode::from(2);
            };
            match std::fs::read(path) {
                Ok(bytes) => {
                    let info = inspect_media_bytes(&bytes);
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&info).unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("inspect error: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        "optimize-svg" => {
            let Some(input) = args.get(1) else {
                eprintln!("usage: craft optimize-svg <in.svg> [out.svg]");
                return ExitCode::from(2);
            };
            let raw = match std::fs::read_to_string(input) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("read svg error: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let opt = optimize_svg(&raw);
            if let Some(out) = args.get(2) {
                if let Err(err) = std::fs::write(out, opt.as_bytes()) {
                    eprintln!("write svg error: {err}");
                    return ExitCode::FAILURE;
                }
                let report = serde_json::json!({
                    "input": input,
                    "output": out,
                    "inputBytes": raw.len(),
                    "outputBytes": opt.len(),
                    "savedBytes": raw.len().saturating_sub(opt.len()),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            } else {
                println!("{opt}");
            }
            ExitCode::SUCCESS
        }
        "convert" => {
            let (Some(input), Some(output), Some(fmt)) = (args.get(1), args.get(2), args.get(3))
            else {
                eprintln!(
                    "usage: craft convert <in> <out> <svg|webp|avif|png-lossless|webm|av1> [--lossless]"
                );
                return ExitCode::from(2);
            };
            let lossless = args.iter().any(|a| a == "--lossless") || fmt == "png-lossless";
            match convert_media_file(Path::new(input), Path::new(output), fmt, lossless) {
                Ok(report) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("convert error: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        "mcp" => {
            let tools = serde_json::json!({
                "protocolVersion": "2026-07-28",
                "mode": "codemode",
                "tools": [
                    { "name": "craft_list_apps", "description": "List compiled Craft apps & formats" },
                    { "name": "craft_codecs", "description": "Inspect shared browser multimedia & document codecs" },
                    { "name": "craft_inspect_media", "description": "Sniff & parse SVG, PNG lossless, WebP, AVIF, GLB, WebM, AV1 headers" },
                    { "name": "craft_convert_media", "description": "Convert & optimize media cross-platform (WebP/AVIF/PNG-lossless/SVG/WebM/AV1)" },
                    { "name": "craft_dns_plan", "description": "Generate DNS/Wasm edge routes under *.aphrody.app" }
                ]
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&tools).unwrap_or_default()
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "usage: craft <info|list|codecs|brand|dns [domain]|inspect <file>|optimize-svg <in> [out]|convert <in> <out> <fmt> [--lossless]|mcp>"
            );
            ExitCode::from(2)
        }
    }
}
