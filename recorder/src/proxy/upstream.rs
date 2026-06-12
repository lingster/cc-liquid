//! Upstream exchange transport: how forwarded requests reach the real
//! Hyperliquid API (PRD §B.8 `proxy::upstream`).
//!
//! The `Upstream` trait isolates the network so the handler's routing logic is
//! testable with a scripted double; `HttpUpstream` is the real HTTPS client.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

/// Status + verbatim body returned by the upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamResponse {
    pub status: u16,
    pub body: String,
}

/// Transport for forwarding a JSON POST to the upstream exchange.
#[async_trait]
pub trait Upstream: Send + Sync {
    /// POST `body` to `path` (e.g. `/info`) and return the raw response.
    async fn post_json(&self, path: &str, body: &str) -> anyhow::Result<UpstreamResponse>;
}

/// Real HTTPS upstream backed by `reqwest`.
pub struct HttpUpstream {
    base_url: String,
    client: reqwest::Client,
}

impl HttpUpstream {
    /// `base_url` without a trailing slash, e.g. `https://api.hyperliquid-testnet.xyz`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait]
impl Upstream for HttpUpstream {
    async fn post_json(&self, path: &str, body: &str) -> anyhow::Result<UpstreamResponse> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        let status = resp.status().as_u16();
        let body = resp.text().await?;
        Ok(UpstreamResponse { status, body })
    }
}

/// Scripted upstream for tests: canned responses per path, with a record of
/// every forwarded request.
#[derive(Default)]
pub struct ScriptedUpstream {
    responses: Mutex<HashMap<String, UpstreamResponse>>,
    requests: Mutex<Vec<(String, String)>>,
}

impl ScriptedUpstream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the response returned for every POST to `path`.
    pub fn respond(self, path: &str, status: u16, body: &str) -> Self {
        self.responses.lock().unwrap().insert(
            path.to_string(),
            UpstreamResponse {
                status,
                body: body.to_string(),
            },
        );
        self
    }

    /// All `(path, body)` pairs forwarded so far.
    pub fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Upstream for ScriptedUpstream {
    async fn post_json(&self, path: &str, body: &str) -> anyhow::Result<UpstreamResponse> {
        self.requests
            .lock()
            .unwrap()
            .push((path.to_string(), body.to_string()));
        self.responses
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("scripted upstream has no response for {path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_upstream_returns_canned_response_and_records_request() {
        let up = ScriptedUpstream::new().respond("/info", 200, r#"{"BTC":"95000.0"}"#);
        let resp = up
            .post_json("/info", r#"{"type":"allMids"}"#)
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, r#"{"BTC":"95000.0"}"#);
        assert_eq!(
            up.requests(),
            vec![("/info".to_string(), r#"{"type":"allMids"}"#.to_string())]
        );
    }

    #[tokio::test]
    async fn scripted_upstream_errors_on_unscripted_path() {
        let up = ScriptedUpstream::new();
        assert!(up.post_json("/exchange", "{}").await.is_err());
    }

    #[test]
    fn http_upstream_normalizes_trailing_slash() {
        let up = HttpUpstream::new("https://api.hyperliquid-testnet.xyz/");
        assert_eq!(up.base_url(), "https://api.hyperliquid-testnet.xyz");
    }
}
