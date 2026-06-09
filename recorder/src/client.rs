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

        Ok(Self { write, read })
    }
}

#[async_trait]
impl EventSource for WsSource {
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        loop {
            match self.read.next().await {
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
            }
        }
    }
}
