//! Best-effort operator notifications via a Discord webhook.
//!
//! Used for *unrecoverable* conditions only — the recorder exiting abnormally,
//! reconnection giving up — never for routine reconnects. The webhook URL is a
//! secret and comes from the `DISCORD_WEBHOOK_URL` environment variable (or a
//! nearby `.env`, see [`crate::crowdcent::apply_dotenv_fallback`]); it must
//! never be committed or logged.
//!
//! Every send is fallible-but-silent: a notification failure is logged and
//! swallowed, because alerting must never take down the recording it alerts on.

use tracing::{info, warn};

/// Environment variable holding the Discord webhook URL.
pub const WEBHOOK_ENV_VAR: &str = "DISCORD_WEBHOOK_URL";

/// Discord rejects `content` longer than 2000 characters.
const DISCORD_CONTENT_LIMIT: usize = 2000;

/// Truncate `msg` to Discord's content limit on a char boundary, marking cuts.
fn clamp_content(msg: &str) -> String {
    if msg.chars().count() <= DISCORD_CONTENT_LIMIT {
        return msg.to_string();
    }
    let cut: String = msg.chars().take(DISCORD_CONTENT_LIMIT - 1).collect();
    format!("{cut}…")
}

/// A Discord webhook notifier. Cheap to clone.
#[derive(Clone)]
pub struct DiscordNotifier {
    webhook_url: String,
    client: reqwest::Client,
}

impl DiscordNotifier {
    pub fn new(webhook_url: String) -> Self {
        Self {
            webhook_url,
            client: reqwest::Client::new(),
        }
    }

    /// Build from `DISCORD_WEBHOOK_URL`, or `None` when unset/empty.
    pub fn from_env() -> Option<Self> {
        match std::env::var(WEBHOOK_ENV_VAR) {
            Ok(url) if !url.trim().is_empty() => Some(Self::new(url.trim().to_string())),
            _ => None,
        }
    }

    /// Post `msg` to the webhook. Failures are logged, never propagated.
    pub async fn send(&self, msg: &str) {
        let body = match serde_json::to_string(&serde_json::json!({
            "content": clamp_content(msg),
        })) {
            Ok(b) => b,
            Err(e) => {
                warn!("discord notification not sent (encode failed): {e}");
                return;
            }
        };
        let result = self
            .client
            .post(&self.webhook_url)
            .header("Content-Type", "application/json")
            .body(body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        match result {
            Ok(resp) if resp.status().is_success() => {
                info!("discord notification sent");
            }
            Ok(resp) => warn!("discord notification rejected: HTTP {}", resp.status()),
            Err(e) => warn!("discord notification failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_content_is_unchanged() {
        assert_eq!(clamp_content("recorder down"), "recorder down");
    }

    #[test]
    fn long_content_is_clamped_to_discord_limit() {
        let long = "x".repeat(5000);
        let clamped = clamp_content(&long);
        assert_eq!(clamped.chars().count(), 2000);
        assert!(clamped.ends_with('…'));
    }

    #[test]
    fn from_env_is_none_when_unset() {
        // Serialize env mutation: this test owns the var for its duration.
        std::env::remove_var(WEBHOOK_ENV_VAR);
        assert!(DiscordNotifier::from_env().is_none());
    }
}
