//! Pure-Rust OpenAuth (`anomalyco/openauth` MIT) OAuth2 / OIDC Discovery & PKCE S256 core
//! fused directly into `ay_net` (zero external crypto bloat, `#![forbid(unsafe_code)]`).

use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};

/// RFC 8414 OAuth 2.0 Authorization Server Metadata served at `/.well-known/oauth-authorization-server`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAuthMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    pub response_types_supported: Vec<String>,
    pub grant_types_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
}

impl OpenAuthMetadata {
    #[must_use]
    pub fn for_issuer(issuer: &str) -> Self {
        let base = issuer.trim_end_matches('/');
        Self {
            issuer: base.to_owned(),
            authorization_endpoint: format!("{base}/authorize"),
            token_endpoint: format!("{base}/token"),
            jwks_uri: format!("{base}/.well-known/jwks.json"),
            response_types_supported: vec!["code".to_owned()],
            grant_types_supported: vec![
                "authorization_code".to_owned(),
                "refresh_token".to_owned(),
                "client_credentials".to_owned(),
            ],
            code_challenge_methods_supported: vec!["S256".to_owned()],
        }
    }
}

/// Validates RFC 7636 PKCE verifier format (43..=128 unreserved URL-safe ASCII chars).
#[must_use]
pub fn is_valid_pkce_verifier(verifier: &str) -> bool {
    let len = verifier.len();
    (43..=128).contains(&len)
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// Builds an Axum router exposing OpenAuth RFC 8414 discovery metadata.
pub fn openauth_router(issuer: impl Into<String>) -> Router {
    let meta = OpenAuthMetadata::for_issuer(&issuer.into());
    Router::new().route(
        "/.well-known/oauth-authorization-server",
        get(move || {
            let m = meta.clone();
            async move { Json(m) }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_rfc8414_metadata_and_validates_pkce_verifier() {
        let meta = OpenAuthMetadata::for_issuer("https://auth.aphrody.com/");
        assert_eq!(meta.issuer, "https://auth.aphrody.com");
        assert_eq!(meta.token_endpoint, "https://auth.aphrody.com/token");
        assert_eq!(meta.code_challenge_methods_supported, vec!["S256"]);
        assert!(is_valid_pkce_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert!(!is_valid_pkce_verifier("too-short"));
        assert!(!is_valid_pkce_verifier("invalid spaces in verifier string 12345678901234567890"));
    }
}
