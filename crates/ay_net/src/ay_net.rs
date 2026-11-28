//! Pure high-performance HTTP/edge networking core (`axum` + `lol_html` + `bun_hpack` + `bun_wyhash` + `pingora_ketama` + `tinyufo`).

use axum::{
    Json, Router,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use lol_html::{HtmlRewriter, Settings, element};
use serde::{Deserialize, Serialize};

pub mod openauth;

pub use bun_hpack;
pub use pingora_ketama;
pub use tinyufo;

/// Structured fail-fast HTTP error response (never silent fallback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
    pub hint: &'static str,
    #[serde(skip)]
    pub status: StatusCode,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}

/// Cross-Origin isolation headers required for WebAssembly threads, SharedArrayBuffer, JSPI, OPFS, and WebGPU.
pub fn wasm_webgpu_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-embedder-policy"),
        HeaderValue::from_static("credentialless"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    headers
}

fn escape_html_attr(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Zero-copy streaming HTML rewriter backed by Cloudflare `lol_html` 3.x.
pub fn inject_seo_meta(html: &str, canonical_url: &str) -> Result<String, ApiError> {
    let trimmed = canonical_url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://"))
        || trimmed.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return Err(ApiError {
            code: "INVALID_CANONICAL_URL",
            message: format!("rejected unsafe canonical URL `{trimmed}`"),
            hint: "Use an absolute http:// or https:// URL without control characters",
            status: StatusCode::BAD_REQUEST,
        });
    }
    let mut output = Vec::with_capacity(html.len() + 128);
    let url_escaped = escape_html_attr(trimmed);
    let mut rewriter = HtmlRewriter::new(
        Settings::new().append_element_content_handler(element!("head", move |el| {
            el.append(
                &format!(r#"<link rel="canonical" href="{url_escaped}">"#),
                lol_html::html_content::ContentType::Html,
            );
            Ok(())
        })),
        |chunk: &[u8]| output.extend_from_slice(chunk),
    );
    rewriter.write(html.as_bytes()).map_err(|e| ApiError {
        code: "HTML_REWRITE_ERROR",
        message: e.to_string(),
        hint: "Verify valid UTF-8 HTML input",
        status: StatusCode::BAD_REQUEST,
    })?;
    rewriter.end().map_err(|e| ApiError {
        code: "HTML_REWRITE_END_ERROR",
        message: e.to_string(),
        hint: "Verify HTML stream closure",
        status: StatusCode::BAD_REQUEST,
    })?;
    String::from_utf8(output).map_err(|e| ApiError {
        code: "HTML_UTF8_ERROR",
        message: e.to_string(),
        hint: "Input must be valid UTF-8",
        status: StatusCode::BAD_REQUEST,
    })
}

async fn health() -> (HeaderMap, Json<serde_json::Value>) {
    (
        wasm_webgpu_headers(),
        Json(serde_json::json!({
            "ok": true,
            "runtime": "ay",
            "webkit": false,
            "jsc": false
        })),
    )
}

async fn robots_txt() -> &'static str {
    "User-agent: *\nAllow: /\nSitemap: /sitemap.xml\n"
}

async fn llms_txt() -> &'static str {
    "# ay\n\n> Pure-Rust zero-WebKit runtime & edge networking core.\n\n- GET /health\n- GET /v1/models\n- GET /.well-known/agent.json\n"
}

async fn agent_card() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "ay",
        "protocol": "a2a/1.0",
        "capabilities": ["http", "hpack", "ketama", "tinyufo", "wasm-webgpu-headers"],
        "endpoints": {
            "health": "/health",
            "llms": "/llms.txt",
            "models": "/v1/models"
        }
    }))
}

async fn openai_models() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "object": "list",
        "data": [
            { "id": "ay-local-ggml", "object": "model", "owned_by": "ay" },
            { "id": "ay-router-auto", "object": "model", "owned_by": "ay" }
        ]
    }))
}

/// Lean `axum 0.8` router shared across desktop/VPS and edge.
pub fn build_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/robots.txt", get(robots_txt))
        .route("/llms.txt", get(llms_txt))
        .route("/.well-known/agent.json", get(agent_card))
        .route("/v1/models", get(openai_models))
}

/// Fast consistent-hash + TinyUFO cache key using Bun's Wyhash.
pub fn cache_key_wyhash(payload: &[u8]) -> u64 {
    bun_wyhash::hash(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::Request;
    use tower::ServiceExt;

    #[test]
    fn lol_html_injects_canonical_link() {
        let out = inject_seo_meta(
            "<html><head><title>Hi</title></head><body></body></html>",
            "https://ay.dev/",
        )
        .expect("seo meta injection");
        assert!(out.contains(r#"<link rel="canonical" href="https://ay.dev/">"#));
    }

    #[tokio::test]
    async fn router_serves_health_with_coop_coep_headers() {
        let app = build_router();
        let req = Request::builder()
            .method("GET")
            .uri("/health")
            .body(Body::empty())
            .expect("health request");
        let resp = app.oneshot(req).await.expect("health response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("cross-origin-opener-policy")
                .expect("coop header"),
            "same-origin"
        );
    }
}
