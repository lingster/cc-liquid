//! Request routing (PRD §B.2, §B.8 `proxy::handler`): the market-source
//! toggle, the testnet-only write guard, and always-on capture logging.
//!
//! Routing rules:
//! * `/exchange` forwards **only** when `network = testnet` *and*
//!   `allow_trading` — otherwise it is rejected with a live-shaped
//!   `{"status":"err",...}` body (structurally impossible to trade mainnet).
//! * `/info` market reads (`allMids`/`meta`/`spotMeta`) are served from the
//!   playback session when `market_source = playback`; everything else
//!   forwards to the upstream exchange.
//! * Every round-trip — forwarded, played back, or rejected — is logged.

use std::str::FromStr;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::config::Network;
use crate::proxy::log::{redact_signature, ResponseSource, RpcLogEntry, RpcSink};
use crate::proxy::market::MarketDataProvider;
use crate::proxy::request::{classify, Endpoint, MethodTag};
use crate::proxy::upstream::Upstream;

/// Where `/info` market reads are answered from (PRD §B.2 the toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketSource {
    Forward,
    Playback,
}

impl FromStr for MarketSource {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "forward" => Ok(MarketSource::Forward),
            "playback" => Ok(MarketSource::Playback),
            other => Err(format!(
                "unknown market source `{other}` (use forward|playback)"
            )),
        }
    }
}

/// Resolved proxy behaviour (PRD §B.7).
#[derive(Debug, Clone, Copy)]
pub struct ProxyConfig {
    pub network: Network,
    pub market_source: MarketSource,
    pub allow_trading: bool,
    pub redact_signatures: bool,
}

impl ProxyConfig {
    /// The §B.2 write guard: `/exchange` may forward only on testnet with
    /// trading explicitly enabled.
    pub fn writes_allowed(&self) -> bool {
        self.network == Network::Testnet && self.allow_trading
    }
}

/// HTTP-level response the server returns to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyResponse {
    pub status: u16,
    pub body: String,
}

impl ProxyResponse {
    fn ok(body: String) -> Self {
        Self { status: 200, body }
    }
}

/// Routes intercepted requests; shared across connections (`&self` + interior
/// mutability — locks are never held across an upstream await).
pub struct ProxyHandler {
    cfg: ProxyConfig,
    upstream: Box<dyn Upstream>,
    market: Option<Mutex<Box<dyn MarketDataProvider>>>,
    log: Mutex<LogState>,
}

struct LogState {
    seq: u64,
    sink: Box<dyn RpcSink>,
}

impl ProxyHandler {
    /// `market` is required when `market_source = playback`.
    pub fn new(
        cfg: ProxyConfig,
        upstream: Box<dyn Upstream>,
        market: Option<Box<dyn MarketDataProvider>>,
        sink: Box<dyn RpcSink>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !(cfg.market_source == MarketSource::Playback && market.is_none()),
            "market_source=playback requires a playback session"
        );
        Ok(Self {
            cfg,
            upstream,
            market: market.map(Mutex::new),
            log: Mutex::new(LogState { seq: 0, sink }),
        })
    }

    /// Handle one POST: classify, route, log, respond.
    pub async fn handle(&self, path: &str, raw_body: &str) -> ProxyResponse {
        let ts_recv_ms = now_ms();

        let Some(endpoint) = Endpoint::from_path(path) else {
            return ProxyResponse {
                status: 404,
                body: json!({"error": format!("unsupported path `{path}`")}).to_string(),
            };
        };

        let Ok(request) = serde_json::from_str::<Value>(raw_body) else {
            let resp = ProxyResponse {
                status: 400,
                body: json!({"error": "invalid JSON body"}).to_string(),
            };
            self.log_round_trip(
                endpoint,
                &MethodTag::Unknown,
                Value::String(raw_body.to_string()),
                &resp,
                ResponseSource::Rejected,
                ts_recv_ms,
                0,
            );
            return resp;
        };

        let tag = classify(endpoint, &request);
        let (resp, source, latency_ms) = self.route(endpoint, &tag, raw_body).await;
        self.log_round_trip(
            endpoint, &tag, request, &resp, source, ts_recv_ms, latency_ms,
        );
        resp
    }

    async fn route(
        &self,
        endpoint: Endpoint,
        tag: &MethodTag,
        raw_body: &str,
    ) -> (ProxyResponse, ResponseSource, i64) {
        match endpoint {
            Endpoint::Exchange if !self.cfg.writes_allowed() => {
                (self.reject_write(), ResponseSource::Rejected, 0)
            }
            Endpoint::Info
                if tag.is_market_read() && self.cfg.market_source == MarketSource::Playback =>
            {
                (self.serve_playback(tag), ResponseSource::Playback, 0)
            }
            _ => {
                let started = now_ms();
                let resp = self.forward(endpoint, raw_body).await;
                (resp, ResponseSource::Forward, now_ms() - started)
            }
        }
    }

    fn reject_write(&self) -> ProxyResponse {
        let reason = format!(
            "hl-proxy: /exchange forwarding disabled (network={}, allow_trading={}); \
             writes require --network testnet --allow-trading",
            self.cfg.network.as_str(),
            self.cfg.allow_trading
        );
        ProxyResponse::ok(json!({"status": "err", "response": reason}).to_string())
    }

    fn serve_playback(&self, tag: &MethodTag) -> ProxyResponse {
        let market = self
            .market
            .as_ref()
            .expect("playback mode requires a market provider");
        let mut market = market.lock().unwrap();
        let body = match tag {
            MethodTag::AllMids => market.all_mids(),
            MethodTag::Meta => market.meta(),
            MethodTag::SpotMeta => market.spot_meta(),
            other => unreachable!("{other:?} is not a playback-servable read"),
        };
        ProxyResponse::ok(body.to_string())
    }

    async fn forward(&self, endpoint: Endpoint, raw_body: &str) -> ProxyResponse {
        match self.upstream.post_json(endpoint.as_str(), raw_body).await {
            Ok(up) => ProxyResponse {
                status: up.status,
                body: up.body,
            },
            Err(e) => ProxyResponse {
                status: 502,
                body: json!({"status": "err", "response": format!("hl-proxy upstream error: {e}")})
                    .to_string(),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn log_round_trip(
        &self,
        endpoint: Endpoint,
        tag: &MethodTag,
        mut request: Value,
        resp: &ProxyResponse,
        source: ResponseSource,
        ts_recv_ms: i64,
        latency_ms: i64,
    ) {
        if self.cfg.redact_signatures && endpoint == Endpoint::Exchange {
            request = redact_signature(request);
        }
        // Non-JSON upstream bodies are preserved verbatim as a JSON string.
        let response =
            serde_json::from_str(&resp.body).unwrap_or_else(|_| Value::String(resp.body.clone()));

        let mut log = self.log.lock().unwrap();
        let entry = RpcLogEntry {
            seq: log.seq,
            ts_recv_ms,
            ts_resp_ms: now_ms(),
            latency_ms,
            transport: "http".to_string(),
            endpoint: endpoint.as_str().to_string(),
            method_tag: tag.as_str().to_string(),
            request,
            response,
            status_code: resp.status,
            source,
            network: self.cfg.network.as_str().to_string(),
        };
        if let Err(e) = log.sink.append(&entry) {
            tracing::error!("failed to append rpc log entry seq={}: {e}", entry.seq);
        }
        log.seq += 1;
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AllMids, MarketEvent, RecordedEvent};
    use crate::proxy::log::{MemorySink, RpcLogEntry};
    use crate::proxy::market::PlaybackMarket;
    use crate::proxy::upstream::ScriptedUpstream;
    use std::sync::Arc;

    /// MemorySink handle the test can inspect after moving a clone-side into
    /// the handler.
    #[derive(Clone, Default)]
    struct SharedSink(Arc<Mutex<MemorySink>>);

    impl RpcSink for SharedSink {
        fn append(&mut self, entry: &RpcLogEntry) -> anyhow::Result<()> {
            self.0.lock().unwrap().append(entry)
        }
    }

    impl SharedSink {
        fn entries(&self) -> Vec<RpcLogEntry> {
            self.0.lock().unwrap().entries().to_vec()
        }
    }

    fn cfg(network: Network, market_source: MarketSource, allow_trading: bool) -> ProxyConfig {
        ProxyConfig {
            network,
            market_source,
            allow_trading,
            redact_signatures: false,
        }
    }

    fn playback_market() -> Box<dyn MarketDataProvider> {
        let mids = |seq: u64, px: f64| RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 1000 + seq as i64,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), px)],
            }),
        };
        Box::new(PlaybackMarket::from_events(
            vec![mids(0, 95000.0), mids(1, 95100.0)],
            vec!["BTC".into()],
            json!({"universe":[{"name":"BTC","szDecimals":5}]}),
        ))
    }

    fn order_body() -> String {
        json!({
            "action": {"type":"order","orders":[{"a":0,"b":true,"p":"95000","s":"0.01","r":false}]},
            "nonce": 1, "signature": {"r":"0x1","s":"0x2","v":27}
        })
        .to_string()
    }

    #[tokio::test]
    async fn mainnet_exchange_writes_are_rejected_even_with_allow_trading() {
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Forward, true),
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(sink.clone()),
        )
        .unwrap();

        let resp = h.handle("/exchange", &order_body()).await;
        assert_eq!(
            resp.status, 200,
            "reject is a structured err, not an HTTP error"
        );
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["status"], "err");

        let entries = sink.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, ResponseSource::Rejected);
        assert_eq!(entries[0].method_tag, "bulk_orders");
    }

    #[tokio::test]
    async fn testnet_exchange_writes_require_allow_trading() {
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Testnet, MarketSource::Forward, false),
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(sink.clone()),
        )
        .unwrap();
        let resp = h.handle("/exchange", &order_body()).await;
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["status"], "err");
        assert_eq!(sink.entries()[0].source, ResponseSource::Rejected);
    }

    #[tokio::test]
    async fn testnet_exchange_writes_forward_when_allowed() {
        let up_body = r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":77}}]}}}"#;
        let upstream = Arc::new(ScriptedUpstream::new().respond("/exchange", 200, up_body));
        struct Fwd(Arc<ScriptedUpstream>);
        #[async_trait::async_trait]
        impl Upstream for Fwd {
            async fn post_json(
                &self,
                path: &str,
                body: &str,
            ) -> anyhow::Result<crate::proxy::upstream::UpstreamResponse> {
                self.0.post_json(path, body).await
            }
        }

        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Testnet, MarketSource::Forward, true),
            Box::new(Fwd(upstream.clone())),
            None,
            Box::new(sink.clone()),
        )
        .unwrap();

        let body = order_body();
        let resp = h.handle("/exchange", &body).await;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, up_body, "upstream response returned verbatim");
        // Forwarded body is the verbatim signed request.
        assert_eq!(upstream.requests(), vec![("/exchange".to_string(), body)]);
        let entry = &sink.entries()[0];
        assert_eq!(entry.source, ResponseSource::Forward);
        assert_eq!(
            entry.response["response"]["data"]["statuses"][0]["resting"]["oid"],
            77
        );
        assert!(entry.latency_ms >= 0);
    }

    #[tokio::test]
    async fn playback_serves_market_reads_without_touching_upstream() {
        let upstream = Arc::new(ScriptedUpstream::new());
        struct Fwd(Arc<ScriptedUpstream>);
        #[async_trait::async_trait]
        impl Upstream for Fwd {
            async fn post_json(
                &self,
                path: &str,
                body: &str,
            ) -> anyhow::Result<crate::proxy::upstream::UpstreamResponse> {
                self.0.post_json(path, body).await
            }
        }
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Playback, false),
            Box::new(Fwd(upstream.clone())),
            Some(playback_market()),
            Box::new(sink.clone()),
        )
        .unwrap();

        let mids = h.handle("/info", r#"{"type":"allMids","dex":""}"#).await;
        assert_eq!(mids.status, 200);
        let mids: Value = serde_json::from_str(&mids.body).unwrap();
        assert_eq!(mids["BTC"], "95000.0");

        let meta = h.handle("/info", r#"{"type":"meta","dex":""}"#).await;
        let meta: Value = serde_json::from_str(&meta.body).unwrap();
        assert_eq!(meta["universe"][0]["name"], "BTC");

        let spot = h.handle("/info", r#"{"type":"spotMeta"}"#).await;
        let spot: Value = serde_json::from_str(&spot.body).unwrap();
        assert_eq!(spot["universe"], json!([]));

        // Successive polls advance the playback cursor.
        let mids2 = h.handle("/info", r#"{"type":"allMids","dex":""}"#).await;
        let mids2: Value = serde_json::from_str(&mids2.body).unwrap();
        assert_eq!(mids2["BTC"], "95100.0");

        assert!(upstream.requests().is_empty(), "nothing must forward");
        let sources: Vec<_> = sink.entries().iter().map(|e| e.source).collect();
        assert!(sources.iter().all(|s| *s == ResponseSource::Playback));
    }

    #[tokio::test]
    async fn playback_mode_still_forwards_account_reads() {
        let up_body = r#"{"marginSummary":{"accountValue":"1000.0"}}"#;
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Playback, false),
            Box::new(ScriptedUpstream::new().respond("/info", 200, up_body)),
            Some(playback_market()),
            Box::new(sink.clone()),
        )
        .unwrap();

        let resp = h
            .handle("/info", r#"{"type":"clearinghouseState","user":"0xabc"}"#)
            .await;
        assert_eq!(resp.body, up_body);
        let entry = &sink.entries()[0];
        assert_eq!(entry.source, ResponseSource::Forward);
        assert_eq!(entry.method_tag, "user_state");
    }

    #[tokio::test]
    async fn forward_mode_forwards_market_reads() {
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Forward, false),
            Box::new(ScriptedUpstream::new().respond("/info", 200, r#"{"BTC":"1.0"}"#)),
            None,
            Box::new(sink.clone()),
        )
        .unwrap();
        let resp = h.handle("/info", r#"{"type":"allMids"}"#).await;
        assert_eq!(resp.body, r#"{"BTC":"1.0"}"#);
        assert_eq!(sink.entries()[0].source, ResponseSource::Forward);
    }

    #[tokio::test]
    async fn upstream_failure_maps_to_502_structured_error() {
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Forward, false),
            Box::new(ScriptedUpstream::new()), // nothing scripted -> error
            None,
            Box::new(sink.clone()),
        )
        .unwrap();
        let resp = h.handle("/info", r#"{"type":"allMids"}"#).await;
        assert_eq!(resp.status, 502);
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["status"], "err");
        assert_eq!(sink.entries()[0].status_code, 502);
    }

    #[tokio::test]
    async fn unsupported_path_is_404() {
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Forward, false),
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(MemorySink::new()),
        )
        .unwrap();
        let resp = h.handle("/ws", "{}").await;
        assert_eq!(resp.status, 404);
    }

    #[tokio::test]
    async fn invalid_json_is_400_and_logged() {
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Forward, false),
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(sink.clone()),
        )
        .unwrap();
        let resp = h.handle("/info", "not json").await;
        assert_eq!(resp.status, 400);
        let entry = &sink.entries()[0];
        assert_eq!(entry.method_tag, "unknown");
        assert_eq!(entry.source, ResponseSource::Rejected);
    }

    #[tokio::test]
    async fn redaction_applies_to_logged_exchange_requests_only() {
        let sink = SharedSink::default();
        let mut config = cfg(Network::Mainnet, MarketSource::Forward, false);
        config.redact_signatures = true;
        let h = ProxyHandler::new(
            config,
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(sink.clone()),
        )
        .unwrap();
        h.handle("/exchange", &order_body()).await;
        let logged_sig = sink.entries()[0].request["signature"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(logged_sig.starts_with("redacted:"));
    }

    #[tokio::test]
    async fn seq_is_monotonic_and_gap_free_across_mixed_requests() {
        let sink = SharedSink::default();
        let h = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Playback, false),
            Box::new(ScriptedUpstream::new().respond("/info", 200, "{}")),
            Some(playback_market()),
            Box::new(sink.clone()),
        )
        .unwrap();
        h.handle("/info", r#"{"type":"allMids"}"#).await;
        h.handle("/exchange", &order_body()).await;
        h.handle("/info", r#"{"type":"userFees","user":"0xabc"}"#)
            .await;
        let seqs: Vec<u64> = sink.entries().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn playback_mode_without_session_is_a_config_error() {
        let err = ProxyHandler::new(
            cfg(Network::Mainnet, MarketSource::Playback, false),
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(MemorySink::new()),
        );
        assert!(err.is_err());
    }

    #[test]
    fn market_source_parses_case_insensitively() {
        assert_eq!(
            MarketSource::from_str("Forward").unwrap(),
            MarketSource::Forward
        );
        assert_eq!(
            MarketSource::from_str("PLAYBACK").unwrap(),
            MarketSource::Playback
        );
        assert!(MarketSource::from_str("replay").is_err());
    }
}
