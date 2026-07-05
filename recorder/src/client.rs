//! Live Hyperliquid WebSocket [`EventSource`].
//!
//! Thin transport layer: connects, sends subscription frames, and yields raw
//! text messages. All decoding/sequencing lives in pure modules, so this file
//! deliberately contains no business logic and is exercised by the (network-
//! gated) integration test.

use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::debug;

use crate::merge_source::{shard_coins, MergeSource};
use crate::source::EventSource;
use crate::subscription::{build_subscriptions, StreamSelection};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Channel depth for the sharded fan-in (bounded for back-pressure).
const SHARD_CHANNEL_DEPTH: usize = 4096;

/// Hyperliquid closes WebSockets with no inbound traffic for 60s; the docs ask
/// clients to send `{"method":"ping"}` on a shorter period. The resulting
/// `{"channel":"pong"}` also guarantees a healthy connection always has
/// traffic, so the idle watchdog (see [`crate::watchdog`]) only trips on
/// genuinely dead links.
const APP_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(45);

/// Application-level keepalive frame per the Hyperliquid WebSocket docs.
const APP_PING_FRAME: &str = r#"{"method":"ping"}"#;

/// Connect a **sharded** live source: split `coins` into groups of at most
/// `shard_size`, open one WebSocket per group, and merge them concurrently.
///
/// `allMids` is global, so it is subscribed only on the first shard to avoid
/// duplicate universe broadcasts; every shard subscribes its own `l2Book` /
/// `trades`. This parallelizes network I/O for full-universe L2 capture.
pub async fn connect_sharded(
    endpoint: &str,
    coins: &[String],
    streams: &StreamSelection,
    shard_size: usize,
) -> anyhow::Result<MergeSource> {
    let shards = shard_coins(coins, shard_size);
    let mut sources = Vec::with_capacity(shards.len());
    for (i, shard) in shards.iter().enumerate() {
        // Only the first shard carries the global allMids subscription.
        let sel = StreamSelection {
            all_mids: streams.all_mids && i == 0,
            l2_book: streams.l2_book,
            trades: streams.trades,
        };
        let subs = build_subscriptions(shard, &sel);
        sources.push(WsSource::connect(endpoint, &subs).await?);
    }
    Ok(MergeSource::spawn(sources, SHARD_CHANNEL_DEPTH))
}

/// A connected Hyperliquid WebSocket, split into independent read/write halves.
pub struct WsSource {
    write: SplitSink<WsStream, Message>,
    read: SplitStream<WsStream>,
    ping_timer: tokio::time::Interval,
}

impl WsSource {
    /// Connect to `endpoint` and send the given subscription messages.
    pub async fn connect(endpoint: &str, subscriptions: &[Value]) -> anyhow::Result<Self> {
        let (stream, _resp) = connect_async(endpoint).await?;
        let (mut write, read) = stream.split();

        for sub in subscriptions {
            write.send(Message::text(sub.to_string())).await?;
            debug!("sent subscription: {sub}");
        }

        let mut ping_timer = tokio::time::interval(APP_PING_INTERVAL);
        ping_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the immediate first tick; the first ping is one period in.
        ping_timer.tick().await;

        Ok(Self {
            write,
            read,
            ping_timer,
        })
    }
}

#[async_trait]
impl EventSource for WsSource {
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        loop {
            tokio::select! {
                // Prefer draining frames; the keepalive only needs its period.
                biased;
                frame = self.read.next() => match frame {
                    Some(Ok(Message::Text(text))) => return Some(Ok(text.as_str().to_string())),
                    Some(Ok(Message::Ping(payload))) => {
                        // Keep the connection alive; surface send failures.
                        if let Err(e) = self.write.send(Message::Pong(payload)).await {
                            return Some(Err(e.into()));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return None,
                    // Pong/Binary/raw frames carry no recordable data.
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Some(Err(e.into())),
                },
                _ = self.ping_timer.tick() => {
                    if let Err(e) = self.write.send(Message::text(APP_PING_FRAME)).await {
                        return Some(Err(e.into()));
                    }
                    debug!("sent application ping");
                }
            }
        }
    }
}
