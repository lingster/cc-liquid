//! Concurrent fan-in of multiple [`EventSource`]s into one.
//!
//! Used for **connection sharding**: the coin universe is split across several
//! WebSocket connections (each its own `EventSource`), and [`MergeSource`] runs
//! one reader task per shard, funnelling all frames into a single bounded
//! channel. This parallelizes network I/O and TLS across cores while presenting
//! the recorder with one ordinary `EventSource`.
//!
//! Note: frames from different shards interleave by arrival, not by exchange
//! time. The recorder assigns the authoritative monotonic `seq` centrally, so
//! ordering remains well-defined and deterministic *per shard*.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::source::EventSource;

/// Split a coin list into shards of at most `shard_size` coins each.
pub fn shard_coins(coins: &[String], shard_size: usize) -> Vec<Vec<String>> {
    let shard_size = shard_size.max(1);
    coins.chunks(shard_size).map(|c| c.to_vec()).collect()
}

/// A single `EventSource` that merges many underlying sources concurrently.
pub struct MergeSource {
    rx: mpsc::Receiver<anyhow::Result<String>>,
}

impl MergeSource {
    /// Spawn one reader task per source. The channel closes (yielding `None`)
    /// once every source is exhausted.
    ///
    /// A shard that ends while its siblings are still live is a *partial*
    /// failure: without intervention the merged stream would keep flowing,
    /// silently missing that shard's coins. Each reader therefore emits an
    /// error frame when its source ends, so the reconnect layer tears down and
    /// re-establishes the whole sharded connection set.
    pub fn spawn<S>(sources: Vec<S>, buffer: usize) -> Self
    where
        S: EventSource + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(buffer.max(1));
        for (i, mut source) in sources.into_iter().enumerate() {
            let tx = tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = source.next_message().await {
                    if tx.send(msg).await.is_err() {
                        return; // receiver dropped
                    }
                }
                // Signal the partial failure; ignore send failure (shutdown).
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "shard {i} ended while merged stream is live"
                    )))
                    .await;
            });
        }
        // Drop the original handle so the channel ends when all tasks finish.
        drop(tx);
        Self { rx }
    }
}

#[async_trait]
impl EventSource for MergeSource {
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ScriptedSource;
    use std::collections::HashSet;

    #[test]
    fn shard_coins_chunks_evenly() {
        let coins: Vec<String> = ["BTC", "ETH", "SOL", "DOGE", "WIF"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let shards = shard_coins(&coins, 2);
        assert_eq!(shards.len(), 3); // 2 + 2 + 1
        assert_eq!(shards[0], vec!["BTC", "ETH"]);
        assert_eq!(shards[2], vec!["WIF"]);
    }

    #[tokio::test]
    async fn merges_all_messages_from_every_shard() {
        let s1 = ScriptedSource::new(vec!["a".into(), "b".into()]);
        let s2 = ScriptedSource::new(vec!["c".into(), "d".into(), "e".into()]);
        let mut merged = MergeSource::spawn(vec![s1, s2], 16);

        let mut seen = HashSet::new();
        let mut shard_end_errors = 0;
        while let Some(msg) = merged.next_message().await {
            match msg {
                Ok(text) => {
                    seen.insert(text);
                }
                Err(_) => shard_end_errors += 1,
            }
        }
        // All five messages arrive (interleaving order is not guaranteed).
        assert_eq!(
            seen,
            ["a", "b", "c", "d", "e"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // Each shard's end is surfaced so a live consumer can reconnect.
        assert_eq!(shard_end_errors, 2);
    }

    #[tokio::test]
    async fn a_dead_shard_surfaces_as_an_error() {
        let merged = MergeSource::spawn(vec![ScriptedSource::new(vec![])], 4);
        let mut merged = merged;
        let err = merged.next_message().await.unwrap().unwrap_err();
        assert!(err.to_string().contains("shard 0 ended"), "{err}");
        assert!(merged.next_message().await.is_none());
    }
}
