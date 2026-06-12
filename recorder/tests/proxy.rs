//! End-to-end proxy tests over real sockets: a recorded Parquet session served
//! through the loopback HTTP server, plus live forwarding to a (mock) upstream
//! via the real `HttpUpstream`. PRD Appendix B acceptance.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use hl_recorder::config::Network;
use hl_recorder::events::{AllMids, MarketEvent, RecordedEvent};
use hl_recorder::proxy::handler::{MarketSource, ProxyConfig, ProxyHandler};
use hl_recorder::proxy::log::{read_jsonl, JsonlSink, ResponseSource, RPC_LOG_FILE};
use hl_recorder::proxy::market::PlaybackMarket;
use hl_recorder::proxy::server::bind_and_serve;
use hl_recorder::proxy::upstream::{HttpUpstream, ScriptedUpstream};
use hl_recorder::sink::EventSink;
use hl_recorder::storage::ParquetSink;

fn mids_event(seq: u64, prices: &[(&str, f64)]) -> RecordedEvent {
    RecordedEvent {
        seq,
        ts_event_ms: 1_000 + seq as i64,
        ts_recv_ms: 1_000 + seq as i64,
        payload: MarketEvent::AllMids(AllMids {
            mids: prices.iter().map(|(c, p)| (c.to_string(), *p)).collect(),
        }),
    }
}

/// Write a minimal mids-only recorded session (Parquet + manifest).
fn write_demo_session(dir: &Path) {
    let mut sink = ParquetSink::create(dir).unwrap();
    sink.write(&mids_event(0, &[("BTC", 95000.0), ("ETH", 3200.5)]))
        .unwrap();
    sink.write(&mids_event(1, &[("BTC", 95100.0), ("ETH", 3201.0)]))
        .unwrap();
    sink.finalize().unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({
            "schema_version": 1,
            "network": "mainnet",
            "endpoint": "wss://api.hyperliquid.xyz/ws",
            "coins": ["BTC", "ETH"],
            "streams": ["allMids"],
            "started_at": "2026-06-09T00:00:00Z",
            "ended_at": "2026-06-09T00:05:00Z",
            "duration_secs": 300,
            "counts": {"recorded":2,"ignored":0,"parse_errors":0,"all_mids":2,"l2_book":0,"trades":0},
            "recorder_version": "0.1.0"
        })
        .to_string(),
    )
    .unwrap();
}

/// Spawn a canned upstream that answers every POST with `200 {body}`.
async fn spawn_canned_upstream(body: &'static str) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let body = body.to_string();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

async fn post(client: &reqwest::Client, url: &str, body: Value) -> (u16, Value) {
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
    (status, body)
}

#[tokio::test]
async fn playback_proxy_serves_recorded_market_data_and_guards_writes() {
    let session = tempfile::tempdir().unwrap();
    write_demo_session(session.path());
    let out = tempfile::tempdir().unwrap();

    let handler = ProxyHandler::new(
        ProxyConfig {
            network: Network::Mainnet,
            market_source: MarketSource::Playback,
            allow_trading: false,
            redact_signatures: false,
        },
        Box::new(ScriptedUpstream::new().respond(
            "/info",
            200,
            r#"{"marginSummary":{"accountValue":"1000.0"}}"#,
        )),
        Some(Box::new(PlaybackMarket::load(session.path()).unwrap())),
        Box::new(JsonlSink::create(out.path()).unwrap()),
    )
    .unwrap();

    let (addr, _task) = bind_and_serve("127.0.0.1:0", Arc::new(handler))
        .await
        .unwrap();
    let url = format!("http://{addr}/");
    let client = reqwest::Client::new();

    // Market reads come from the recorded session, advancing per poll.
    let (status, mids) = post(
        &client,
        &format!("{url}info"),
        json!({"type":"allMids","dex":""}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(mids["BTC"], "95000.0");
    assert_eq!(mids["ETH"], "3200.5");
    let (_, mids2) = post(
        &client,
        &format!("{url}info"),
        json!({"type":"allMids","dex":""}),
    )
    .await;
    assert_eq!(mids2["BTC"], "95100.0");

    // Universe synthesized from the session manifest; spot is empty-but-valid.
    let (_, meta) = post(
        &client,
        &format!("{url}info"),
        json!({"type":"meta","dex":""}),
    )
    .await;
    let names: Vec<&str> = meta["universe"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["BTC", "ETH"]);
    let (_, spot) = post(&client, &format!("{url}info"), json!({"type":"spotMeta"})).await;
    assert_eq!(spot["universe"], json!([]));

    // Account reads forward to the upstream.
    let (_, state) = post(
        &client,
        &format!("{url}info"),
        json!({"type":"clearinghouseState","user":"0xabc","dex":""}),
    )
    .await;
    assert_eq!(state["marginSummary"]["accountValue"], "1000.0");

    // Mainnet writes are structurally rejected.
    let (status, reject) = post(
        &client,
        &format!("{url}exchange"),
        json!({"action":{"type":"order","orders":[]},"nonce":1,"signature":{"r":"0x1","s":"0x2","v":27}}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(reject["status"], "err");

    // The capture log recorded every round-trip, gap-free and self-classified.
    let entries = read_jsonl(out.path().join(RPC_LOG_FILE)).unwrap();
    assert_eq!(entries.len(), 6);
    let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5]);
    let tags: Vec<&str> = entries.iter().map(|e| e.method_tag.as_str()).collect();
    assert_eq!(
        tags,
        vec![
            "all_mids",
            "all_mids",
            "meta",
            "spot_meta",
            "user_state",
            "bulk_orders"
        ]
    );
    assert_eq!(entries[0].source, ResponseSource::Playback);
    assert_eq!(entries[4].source, ResponseSource::Forward);
    assert_eq!(entries[5].source, ResponseSource::Rejected);
}

#[tokio::test]
async fn forward_proxy_relays_through_real_http_upstream() {
    let upstream_addr =
        spawn_canned_upstream(r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"0.01","avgPx":"95000.0","fee":"0.5"}}]}}}"#)
            .await;
    let out = tempfile::tempdir().unwrap();

    let handler = ProxyHandler::new(
        ProxyConfig {
            network: Network::Testnet,
            market_source: MarketSource::Forward,
            allow_trading: true,
            redact_signatures: true,
        },
        Box::new(HttpUpstream::new(format!("http://{upstream_addr}"))),
        None,
        Box::new(JsonlSink::create(out.path()).unwrap()),
    )
    .unwrap();
    let (addr, _task) = bind_and_serve("127.0.0.1:0", Arc::new(handler))
        .await
        .unwrap();
    let client = reqwest::Client::new();

    // Testnet + allow-trading: the signed order forwards and the real
    // response comes back verbatim.
    let (status, body) = post(
        &client,
        &format!("http://{addr}/exchange"),
        json!({"action":{"type":"order","orders":[]},"nonce":7,"signature":{"r":"0xa","s":"0xb","v":27}}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert_eq!(
        body["response"]["data"]["statuses"][0]["filled"]["avgPx"],
        "95000.0"
    );

    let entries = read_jsonl(out.path().join(RPC_LOG_FILE)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, ResponseSource::Forward);
    assert_eq!(entries[0].network, "testnet");
    assert!(entries[0].latency_ms >= 0);
    // --redact-signatures replaced the raw signature in the log.
    let sig = entries[0].request["signature"].as_str().unwrap();
    assert!(sig.starts_with("redacted:"), "got {sig}");
}

#[tokio::test]
async fn unsupported_paths_and_methods_get_clean_errors() {
    let out = tempfile::tempdir().unwrap();
    let handler = ProxyHandler::new(
        ProxyConfig {
            network: Network::Mainnet,
            market_source: MarketSource::Forward,
            allow_trading: false,
            redact_signatures: false,
        },
        Box::new(ScriptedUpstream::new()),
        None,
        Box::new(JsonlSink::create(out.path()).unwrap()),
    )
    .unwrap();
    let (addr, _task) = bind_and_serve("127.0.0.1:0", Arc::new(handler))
        .await
        .unwrap();
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{addr}/unknown"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);

    let resp = client
        .get(format!("http://{addr}/info"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 405);
}
