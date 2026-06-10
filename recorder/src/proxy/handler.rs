//! Request routing (PRD §B.2, §B.6, §B.8 `proxy::handler`): the market-source
//! toggle, the testnet-only write guard, the engine-backed sim, and always-on
//! capture logging.
//!
//! Routing rules:
//! * With the **sim** enabled (playback + `--sim`), `/exchange` orders are
//!   matched by the [`SimEngine`] against the replayed market and account
//!   reads are answered from the [`VirtualAccount`] — the full offline
//!   simulator over the wire (PRD §B.6 follow-up). No real writes can occur.
//! * Without the sim, `/exchange` forwards **only** when `network = testnet`
//!   *and* `allow_trading` — otherwise it is rejected with a live-shaped
//!   `{"status":"err",...}` body (structurally impossible to trade mainnet).
//! * `/info` market reads (`allMids`/`meta`/`spotMeta`) are served from the
//!   playback session when `market_source = playback`; everything else
//!   forwards to the upstream exchange.
//! * Every round-trip — forwarded, played back, or rejected — is logged.
//!
//! [`VirtualAccount`]: crate::sim::VirtualAccount

use std::str::FromStr;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::config::Network;
use crate::proxy::log::{redact_signature, ResponseSource, RpcLogEntry, RpcSink};
use crate::proxy::market::MarketDataProvider;
use crate::proxy::request::{classify, Endpoint, MethodTag};
use crate::proxy::upstream::Upstream;
use crate::sim::SimEngine;

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
    /// Engine-backed simulation (PRD §B.6 follow-up): when present, account
    /// reads and `/exchange` writes are served offline. Requires a playback
    /// market (the sim matches against its folded state).
    sim: Option<Mutex<SimEngine>>,
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
            sim: None,
            log: Mutex::new(LogState { seq: 0, sink }),
        })
    }

    /// Attach the simulation engine (requires a playback market whose folded
    /// state the matcher uses).
    pub fn with_sim(mut self, sim: SimEngine) -> anyhow::Result<Self> {
        let has_state = self
            .market
            .as_ref()
            .is_some_and(|m| m.lock().unwrap().state().is_some());
        anyhow::ensure!(
            has_state,
            "--sim requires market_source=playback (the sim matches against replayed state)"
        );
        self.sim = Some(Mutex::new(sim));
        Ok(self)
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
        let (resp, source, latency_ms) = self.route(endpoint, &tag, &request, raw_body).await;
        self.log_round_trip(
            endpoint, &tag, request, &resp, source, ts_recv_ms, latency_ms,
        );
        resp
    }

    async fn route(
        &self,
        endpoint: Endpoint,
        tag: &MethodTag,
        request: &Value,
        raw_body: &str,
    ) -> (ProxyResponse, ResponseSource, i64) {
        match endpoint {
            Endpoint::Exchange if self.sim.is_some() => (
                self.serve_sim_exchange(request),
                ResponseSource::Playback,
                0,
            ),
            Endpoint::Exchange if !self.cfg.writes_allowed() => {
                (self.reject_write(), ResponseSource::Rejected, 0)
            }
            Endpoint::Info if self.sim.is_some() && is_sim_read(tag) => (
                self.serve_sim_info(tag, request),
                ResponseSource::Playback,
                0,
            ),
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

    /// Route an `/exchange` action to the matching engine, matched against
    /// the playback state at the cursor. Simulated time is the replay cursor
    /// timestamp, keeping runs deterministic.
    fn serve_sim_exchange(&self, request: &Value) -> ProxyResponse {
        let market = self.sim_market().lock().unwrap();
        let mut sim = self.sim.as_ref().unwrap().lock().unwrap();
        let state = market.state().expect("sim requires playback state");
        sim.set_accepting(!market.is_stopped());
        let now = state.cursor_ts_ms().unwrap_or_else(now_ms);
        let action = request.get("action").cloned().unwrap_or(Value::Null);
        let body = sim.handle_action(&action, state, now);
        ProxyResponse::ok(body.to_string())
    }

    /// Serve an account read from the virtual account.
    fn serve_sim_info(&self, tag: &MethodTag, request: &Value) -> ProxyResponse {
        let market = self.sim_market().lock().unwrap();
        let sim = self.sim.as_ref().unwrap().lock().unwrap();
        let state = market.state().expect("sim requires playback state");
        let now = state.cursor_ts_ms().unwrap_or_else(now_ms);
        let body = match tag {
            MethodTag::UserState => sim.user_state_json(state, now),
            MethodTag::FrontendOpenOrders => sim.open_orders_json(),
            MethodTag::UserFills => sim.user_fills_json(),
            MethodTag::UserFillsByTime => {
                let start = request.get("startTime").and_then(Value::as_i64);
                let end = request.get("endTime").and_then(Value::as_i64);
                filter_fills_by_time(sim.user_fills_json(), start, end)
            }
            MethodTag::UserFees => sim.user_fees_json(),
            other => unreachable!("{other:?} is not a sim-servable read"),
        };
        ProxyResponse::ok(body.to_string())
    }

    fn sim_market(&self) -> &Mutex<Box<dyn MarketDataProvider>> {
        self.market
            .as_ref()
            .expect("sim mode requires a playback market")
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
            MethodTag::AllMids => {
                let mids = market.all_mids();
                // The cursor advanced: let the sim react to the new tick
                // (trigger activation, resting fills, end-of-window stop).
                if let Some(sim) = &self.sim {
                    let mut sim = sim.lock().unwrap();
                    let trades = market.take_trades();
                    if let Some(state) = market.state() {
                        let now = state.cursor_ts_ms().unwrap_or_else(now_ms);
                        sim.on_tick(state, &trades, now);
                    }
                    sim.set_accepting(!market.is_stopped());
                }
                mids
            }
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

/// Account/order reads the sim can answer (PRD §B.6 follow-up).
fn is_sim_read(tag: &MethodTag) -> bool {
    matches!(
        tag,
        MethodTag::UserState
            | MethodTag::FrontendOpenOrders
            | MethodTag::UserFills
            | MethodTag::UserFillsByTime
            | MethodTag::UserFees
    )
}

/// Keep only fills whose `time` falls inside `[start, end]`.
fn filter_fills_by_time(fills: Value, start: Option<i64>, end: Option<i64>) -> Value {
    let Value::Array(entries) = fills else {
        return fills;
    };
    Value::Array(
        entries
            .into_iter()
            .filter(|f| {
                let t = f.get("time").and_then(Value::as_i64).unwrap_or(0);
                start.is_none_or(|s| t >= s) && end.is_none_or(|e| t <= e)
            })
            .collect(),
    )
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

#[cfg(test)]
mod sim_tests {
    use super::*;
    use crate::events::{AllMids, MarketEvent, RecordedEvent};
    use crate::proxy::log::MemorySink;
    use crate::proxy::market::{EndOfWindow, PlaybackMarket};
    use crate::proxy::upstream::ScriptedUpstream;
    use crate::sim::order::Universe;
    use crate::sim::{SimConfig, SimEngine};

    fn mids_ev(seq: u64, px: f64) -> RecordedEvent {
        RecordedEvent {
            seq,
            ts_event_ms: 1000 + seq as i64,
            ts_recv_ms: 1000 + seq as i64,
            payload: MarketEvent::AllMids(AllMids {
                mids: vec![("BTC".into(), px)],
            }),
        }
    }

    fn meta_body() -> Value {
        json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 50}]})
    }

    fn sim_handler(events: Vec<RecordedEvent>, eow: EndOfWindow) -> ProxyHandler {
        let market = PlaybackMarket::from_events(events, vec!["BTC".into()], meta_body())
            .with_end_of_window(eow);
        let sim = SimEngine::new(
            SimConfig::default(),
            Universe::from_meta(&meta_body()).unwrap(),
        );
        ProxyHandler::new(
            ProxyConfig {
                network: Network::Mainnet,
                market_source: MarketSource::Playback,
                allow_trading: false,
                redact_signatures: false,
            },
            Box::new(ScriptedUpstream::new()),
            Some(Box::new(market)),
            Box::new(MemorySink::new()),
        )
        .unwrap()
        .with_sim(sim)
        .unwrap()
    }

    fn order_body(is_buy: bool, sz: &str, px: &str) -> String {
        json!({
            "action": {"type": "order", "orders": [
                {"a": 0, "b": is_buy, "p": px, "s": sz, "r": false,
                 "t": {"limit": {"tif": "Ioc"}}}
            ], "grouping": "na"},
            "nonce": 1, "signature": {"r": "0x1", "s": "0x2", "v": 27}
        })
        .to_string()
    }

    #[tokio::test]
    async fn full_offline_trading_loop_works_even_on_mainnet_config() {
        let h = sim_handler(
            vec![mids_ev(0, 95000.0), mids_ev(1, 95100.0)],
            EndOfWindow::Hold,
        );

        // Prime the market cursor.
        h.handle("/info", r#"{"type":"allMids"}"#).await;

        // Place an order: matched by the sim, never forwarded or rejected.
        let resp = h
            .handle("/exchange", &order_body(true, "0.01", "96000"))
            .await;
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["status"], "ok");
        let filled = &body["response"]["data"]["statuses"][0]["filled"];
        assert_eq!(filled["totalSz"], "0.01");
        assert_eq!(filled["avgPx"], "95000.0");

        // Account state reflects the simulated fill at the next mark.
        h.handle("/info", r#"{"type":"allMids"}"#).await; // cursor -> 95100
        let resp = h
            .handle("/info", r#"{"type":"clearinghouseState","user":"0xabc"}"#)
            .await;
        let state: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(state["assetPositions"][0]["position"]["szi"], "0.01");
        assert_eq!(
            state["assetPositions"][0]["position"]["unrealizedPnl"],
            "1.0"
        );

        // Fills and fees come from the virtual account too.
        let fills: Value = serde_json::from_str(
            &h.handle("/info", r#"{"type":"userFills","user":"0xabc"}"#)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(fills[0]["coin"], "BTC");
        let fees: Value = serde_json::from_str(
            &h.handle("/info", r#"{"type":"userFees","user":"0xabc"}"#)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(fees["userCrossRate"], "0.00045");
    }

    #[tokio::test]
    async fn user_fills_by_time_filters_the_window() {
        let h = sim_handler(
            vec![mids_ev(0, 100.0), mids_ev(1, 100.0)],
            EndOfWindow::Hold,
        );
        h.handle("/info", r#"{"type":"allMids"}"#).await; // cursor ts=1000
        h.handle("/exchange", &order_body(true, "1", "101")).await; // fill at t=1000
        let resp = h
            .handle(
                "/info",
                r#"{"type":"userFillsByTime","user":"0xabc","startTime":0,"endTime":900}"#,
            )
            .await;
        let fills: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(
            fills.as_array().unwrap().len(),
            0,
            "fill at t=1000 is outside [0,900]"
        );
        let resp = h
            .handle(
                "/info",
                r#"{"type":"userFillsByTime","user":"0xabc","startTime":0,"endTime":2000}"#,
            )
            .await;
        let fills: Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(fills.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn end_of_window_stop_rejects_orders_past_the_window() {
        let h = sim_handler(vec![mids_ev(0, 100.0)], EndOfWindow::Stop);
        // Drain the window: tick 0, then one poll past the end flags stop.
        h.handle("/info", r#"{"type":"allMids"}"#).await;
        h.handle("/info", r#"{"type":"allMids"}"#).await;
        let resp = h.handle("/exchange", &order_body(true, "1", "101")).await;
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        let err = body["response"]["data"]["statuses"][0]["error"]
            .as_str()
            .unwrap();
        assert!(err.contains("window ended"), "got: {err}");
    }

    #[tokio::test]
    async fn resting_order_fills_as_replay_advances_through_polls() {
        let h = sim_handler(
            vec![mids_ev(0, 100.0), mids_ev(1, 99.0), mids_ev(2, 94.0)],
            EndOfWindow::Hold,
        );
        h.handle("/info", r#"{"type":"allMids"}"#).await; // cursor: 100
                                                          // Resting Gtc buy at 95 (not crossing at 100).
        let gtc = json!({
            "action": {"type": "order", "orders": [
                {"a": 0, "b": true, "p": "95", "s": "1", "r": false,
                 "t": {"limit": {"tif": "Gtc"}}}
            ]},
            "nonce": 2, "signature": {}
        })
        .to_string();
        let resp = h.handle("/exchange", &gtc).await;
        let body: Value = serde_json::from_str(&resp.body).unwrap();
        assert!(body["response"]["data"]["statuses"][0]["resting"]["oid"].is_u64());

        // Open order is visible.
        let orders: Value = serde_json::from_str(
            &h.handle("/info", r#"{"type":"frontendOpenOrders","user":"0xabc"}"#)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(orders.as_array().unwrap().len(), 1);

        // Replay advances: 99 (no fill), then 94 — strictly through 95.
        h.handle("/info", r#"{"type":"allMids"}"#).await;
        h.handle("/info", r#"{"type":"allMids"}"#).await;

        let orders: Value = serde_json::from_str(
            &h.handle("/info", r#"{"type":"frontendOpenOrders","user":"0xabc"}"#)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(orders.as_array().unwrap().len(), 0, "filled and removed");
        let state: Value = serde_json::from_str(
            &h.handle("/info", r#"{"type":"clearinghouseState","user":"0xabc"}"#)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(state["assetPositions"][0]["position"]["szi"], "1.0");
        assert_eq!(state["assetPositions"][0]["position"]["entryPx"], "95.0");
    }

    #[tokio::test]
    async fn sim_cancel_round_trips_over_the_wire() {
        let h = sim_handler(
            vec![mids_ev(0, 100.0), mids_ev(1, 100.0)],
            EndOfWindow::Hold,
        );
        h.handle("/info", r#"{"type":"allMids"}"#).await;
        let gtc = json!({
            "action": {"type": "order", "orders": [
                {"a": 0, "b": true, "p": "95", "s": "1", "r": false,
                 "t": {"limit": {"tif": "Gtc"}}}
            ]},
            "nonce": 2, "signature": {}
        })
        .to_string();
        let body: Value = serde_json::from_str(&h.handle("/exchange", &gtc).await.body).unwrap();
        let oid = body["response"]["data"]["statuses"][0]["resting"]["oid"]
            .as_u64()
            .unwrap();

        let cancel = json!({
            "action": {"type": "cancel", "cancels": [{"a": 0, "o": oid}]},
            "nonce": 3, "signature": {}
        })
        .to_string();
        let body: Value = serde_json::from_str(&h.handle("/exchange", &cancel).await.body).unwrap();
        assert_eq!(body["response"]["data"]["statuses"][0], "success");
    }

    #[test]
    fn sim_without_playback_market_is_rejected() {
        let sim = SimEngine::new(
            SimConfig::default(),
            Universe::from_meta(&meta_body()).unwrap(),
        );
        let res = ProxyHandler::new(
            ProxyConfig {
                network: Network::Mainnet,
                market_source: MarketSource::Forward,
                allow_trading: false,
                redact_signatures: false,
            },
            Box::new(ScriptedUpstream::new()),
            None,
            Box::new(MemorySink::new()),
        )
        .unwrap()
        .with_sim(sim);
        assert!(res.is_err());
    }
}
