//! The [`EventSource`] abstraction: where raw messages come from.
//!
//! The recorder pulls raw text frames from an `EventSource`. The live
//! implementation is a WebSocket client; tests use a scripted in-memory source.
//! This inverts the dependency so orchestration never touches sockets directly.

use async_trait::async_trait;

/// An async source of raw WebSocket text frames.
#[async_trait]
pub trait EventSource {
    /// Return the next raw message, or `None` when the source is exhausted/closed.
    async fn next_message(&mut self) -> Option<anyhow::Result<String>>;
}

/// A scripted source that yields a fixed list of messages, for tests.
pub struct ScriptedSource {
    messages: std::collections::VecDeque<String>,
}

impl ScriptedSource {
    pub fn new(messages: Vec<String>) -> Self {
        Self {
            messages: messages.into(),
        }
    }
}

#[async_trait]
impl EventSource for ScriptedSource {
    async fn next_message(&mut self) -> Option<anyhow::Result<String>> {
        self.messages.pop_front().map(Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_source_drains_in_order_then_ends() {
        let mut src = ScriptedSource::new(vec!["a".into(), "b".into()]);
        assert_eq!(src.next_message().await.unwrap().unwrap(), "a");
        assert_eq!(src.next_message().await.unwrap().unwrap(), "b");
        assert!(src.next_message().await.is_none());
    }
}
