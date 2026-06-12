//! Capture log: every request/response crossing the proxy — forwarded, served
//! from playback, or rejected — is appended here (PRD §B.5).
//!
//! The log is JSONL (one entry per line) because the RPC bodies have variable
//! schemas; market-data WS frames continue to use the Parquet session format.
//! `RpcSink` mirrors the crate's `EventSink` pattern: a trait with a durable
//! (JSONL file) impl and an in-memory impl for tests.

use std::collections::hash_map::DefaultHasher;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// File name of the capture log inside a session/output directory.
pub const RPC_LOG_FILE: &str = "rpc_log.jsonl";

/// Where a response came from (PRD §B.5 `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseSource {
    /// Forwarded to the real upstream exchange and answered by it.
    Forward,
    /// Served locally from a recorded playback session.
    Playback,
    /// Refused by the proxy (e.g. `/exchange` without `--allow-trading`).
    Rejected,
}

/// One captured request/response round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcLogEntry {
    /// Monotonic, gap-free per-session counter.
    pub seq: u64,
    /// When the proxy received the client request (ms since epoch).
    pub ts_recv_ms: i64,
    /// When the proxy returned the response (ms since epoch).
    pub ts_resp_ms: i64,
    /// Upstream round-trip latency (0 for playback/rejected).
    pub latency_ms: i64,
    /// `http` (WS capture is future work).
    pub transport: String,
    /// `/info` or `/exchange`.
    pub endpoint: String,
    /// Resolved Appendix A method tag (self-classifying log).
    pub method_tag: String,
    /// Verbatim request body (signatures optionally redacted).
    pub request: Value,
    /// Verbatim response body.
    pub response: Value,
    pub status_code: u16,
    pub source: ResponseSource,
    /// `mainnet` or `testnet`.
    pub network: String,
}

/// Destination for captured round-trips.
pub trait RpcSink: Send {
    fn append(&mut self, entry: &RpcLogEntry) -> anyhow::Result<()>;
}

/// Durable JSONL sink: one entry per line, flushed per append so a crash loses
/// at most the in-flight entry.
pub struct JsonlSink {
    writer: BufWriter<File>,
}

impl JsonlSink {
    /// Create (or append to) `rpc_log.jsonl` inside `dir`, creating the
    /// directory if needed.
    pub fn create(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(RPC_LOG_FILE))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }
}

impl RpcSink for JsonlSink {
    fn append(&mut self, entry: &RpcLogEntry) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.writer, entry)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

/// In-memory sink for tests and inspection.
#[derive(Default)]
pub struct MemorySink {
    entries: Vec<RpcLogEntry>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[RpcLogEntry] {
        &self.entries
    }
}

impl RpcSink for MemorySink {
    fn append(&mut self, entry: &RpcLogEntry) -> anyhow::Result<()> {
        self.entries.push(entry.clone());
        Ok(())
    }
}

/// Replace a `/exchange` body's `signature` field with a deterministic hash
/// placeholder so traces can be shared without leaking signatures (PRD §B.5).
/// Bodies without a `signature` field are returned unchanged.
pub fn redact_signature(mut body: Value) -> Value {
    if let Some(obj) = body.as_object_mut() {
        if let Some(sig) = obj.get("signature") {
            let mut hasher = DefaultHasher::new();
            sig.to_string().hash(&mut hasher);
            let placeholder = format!("redacted:{:016x}", hasher.finish());
            obj.insert("signature".to_string(), Value::String(placeholder));
        }
    }
    body
}

/// Read a JSONL capture log back into entries (for tests/tools).
pub fn read_jsonl(path: impl AsRef<Path>) -> anyhow::Result<Vec<RpcLogEntry>> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(seq: u64) -> RpcLogEntry {
        RpcLogEntry {
            seq,
            ts_recv_ms: 1_733_836_800_123,
            ts_resp_ms: 1_733_836_800_187,
            latency_ms: 64,
            transport: "http".into(),
            endpoint: "/exchange".into(),
            method_tag: "bulk_orders".into(),
            request: json!({"action":{"type":"order"},"nonce":1}),
            response: json!({"status":"ok"}),
            status_code: 200,
            source: ResponseSource::Forward,
            network: "testnet".into(),
        }
    }

    #[test]
    fn memory_sink_collects_entries_in_order() {
        let mut sink = MemorySink::new();
        sink.append(&entry(0)).unwrap();
        sink.append(&entry(1)).unwrap();
        assert_eq!(sink.entries().len(), 2);
        assert_eq!(sink.entries()[1].seq, 1);
    }

    #[test]
    fn jsonl_sink_round_trips_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = JsonlSink::create(dir.path()).unwrap();
        sink.append(&entry(0)).unwrap();
        sink.append(&entry(1)).unwrap();
        drop(sink);

        let loaded = read_jsonl(dir.path().join(RPC_LOG_FILE)).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], entry(0));
        assert_eq!(loaded[1].seq, 1);
    }

    #[test]
    fn jsonl_sink_appends_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut sink = JsonlSink::create(dir.path()).unwrap();
            sink.append(&entry(0)).unwrap();
        }
        {
            let mut sink = JsonlSink::create(dir.path()).unwrap();
            sink.append(&entry(1)).unwrap();
        }
        let loaded = read_jsonl(dir.path().join(RPC_LOG_FILE)).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn source_serializes_lowercase() {
        let json = serde_json::to_string(&ResponseSource::Playback).unwrap();
        assert_eq!(json, "\"playback\"");
        let json = serde_json::to_string(&ResponseSource::Rejected).unwrap();
        assert_eq!(json, "\"rejected\"");
    }

    #[test]
    fn redaction_replaces_signature_with_stable_placeholder() {
        let body = json!({
            "action": {"type":"order"},
            "nonce": 42,
            "signature": {"r":"0xdead","s":"0xbeef","v":27}
        });
        let redacted = redact_signature(body.clone());
        let sig = redacted.get("signature").unwrap().as_str().unwrap();
        assert!(sig.starts_with("redacted:"), "got {sig}");
        // Deterministic: same input, same placeholder.
        assert_eq!(redact_signature(body.clone()), redacted);
        // Other fields untouched.
        assert_eq!(redacted["nonce"], json!(42));
        assert_eq!(redacted["action"], body["action"]);
    }

    #[test]
    fn redaction_leaves_bodies_without_signature_unchanged() {
        let body = json!({"type":"allMids"});
        assert_eq!(redact_signature(body.clone()), body);
    }
}
