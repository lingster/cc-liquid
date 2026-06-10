//! Thin loopback HTTP server for the proxy (PRD §B.8 `proxy::server`).
//!
//! Hand-rolled HTTP/1.1 over tokio — the surface is tiny (two JSON POST
//! endpoints, loopback only, no TLS per §B.4) so no server framework is
//! needed. The request/response codec is pure and unit-tested; the async
//! accept loop just plumbs bytes between the socket and [`ProxyHandler`].

use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, warn};

use crate::proxy::handler::ProxyHandler;

/// Upper bounds keeping a malformed client from exhausting memory.
const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Parsed request head (everything before the blank line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    /// Path with any query string stripped.
    pub path: String,
    pub content_length: usize,
    /// Whether the client asked to keep the connection open.
    pub keep_alive: bool,
}

/// Parse an HTTP/1.x request head. Returns `None` on malformed input.
pub fn parse_head(head: &str) -> Option<RequestHead> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let version = parts.next()?;
    let path = target.split('?').next()?.to_string();

    let mut content_length = 0usize;
    // HTTP/1.1 defaults to keep-alive; 1.0 defaults to close.
    let mut keep_alive = version == "HTTP/1.1";
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.parse().ok()?,
            "connection" => keep_alive = !value.eq_ignore_ascii_case("close"),
            _ => {}
        }
    }
    Some(RequestHead {
        method,
        path,
        content_length,
        keep_alive,
    })
}

/// Render a JSON response with the proper framing headers.
pub fn format_response(status: u16, body: &str, keep_alive: bool) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        502 => "Bad Gateway",
        _ => "Internal Server Error",
    };
    let connection = if keep_alive { "keep-alive" } else { "close" };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
        body.len()
    )
}

/// Serve connections on `listener` until the task is dropped/aborted.
pub async fn serve(listener: TcpListener, handler: Arc<ProxyHandler>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await.context("accepting connection")?;
        debug!("connection from {peer}");
        let handler = handler.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, handler).await {
                debug!("connection from {peer} ended: {e}");
            }
        });
    }
}

/// Bind + serve, returning the bound address (port 0 supported for tests).
pub async fn bind_and_serve(
    addr: &str,
    handler: Arc<ProxyHandler>,
) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let local = listener.local_addr()?;
    let task = tokio::spawn(async move {
        if let Err(e) = serve(listener, handler).await {
            warn!("proxy server stopped: {e}");
        }
    });
    Ok((local, task))
}

async fn handle_connection(
    mut stream: TcpStream,
    handler: Arc<ProxyHandler>,
) -> anyhow::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    loop {
        // Read until the blank line terminating the head.
        let head_end = loop {
            if let Some(pos) = find_head_end(&buf) {
                break pos;
            }
            anyhow::ensure!(buf.len() <= MAX_HEAD_BYTES, "request head too large");
            let mut chunk = [0u8; 8 * 1024];
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(()); // client closed between requests
            }
            buf.extend_from_slice(&chunk[..n]);
        };

        let head_text = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let Some(head) = parse_head(&head_text) else {
            stream
                .write_all(
                    format_response(400, r#"{"error":"malformed request"}"#, false).as_bytes(),
                )
                .await?;
            return Ok(());
        };
        anyhow::ensure!(
            head.content_length <= MAX_BODY_BYTES,
            "request body too large"
        );

        // Read the body (some of it may already be buffered).
        let body_start = head_end + 4;
        while buf.len() < body_start + head.content_length {
            let mut chunk = [0u8; 8 * 1024];
            let n = stream.read(&mut chunk).await?;
            anyhow::ensure!(n > 0, "connection closed mid-body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf[body_start..body_start + head.content_length])
            .into_owned();
        buf.drain(..body_start + head.content_length);

        let resp = if head.method == "POST" {
            let r = handler.handle(&head.path, &body).await;
            format_response(r.status, &r.body, head.keep_alive)
        } else {
            format_response(
                405,
                r#"{"error":"only POST /info and POST /exchange are supported"}"#,
                head.keep_alive,
            )
        };
        stream.write_all(resp.as_bytes()).await?;
        if !head.keep_alive {
            return Ok(());
        }
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_requests_post() {
        let head = "POST /info HTTP/1.1\r\nHost: 127.0.0.1:8088\r\nUser-Agent: python-requests/2.32\r\nContent-Type: application/json\r\nContent-Length: 19\r\nAccept-Encoding: gzip";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/info");
        assert_eq!(parsed.content_length, 19);
        assert!(parsed.keep_alive, "HTTP/1.1 defaults to keep-alive");
    }

    #[test]
    fn header_names_are_case_insensitive_and_query_is_stripped() {
        let head = "POST /exchange?x=1 HTTP/1.1\r\ncontent-length: 5\r\nCONNECTION: close";
        let parsed = parse_head(head).unwrap();
        assert_eq!(parsed.path, "/exchange");
        assert_eq!(parsed.content_length, 5);
        assert!(!parsed.keep_alive);
    }

    #[test]
    fn http_1_0_defaults_to_close() {
        let parsed = parse_head("POST /info HTTP/1.0\r\nContent-Length: 0").unwrap();
        assert!(!parsed.keep_alive);
    }

    #[test]
    fn missing_content_length_means_empty_body() {
        let parsed = parse_head("GET /ws HTTP/1.1\r\nHost: x").unwrap();
        assert_eq!(parsed.content_length, 0);
    }

    #[test]
    fn malformed_request_line_is_rejected() {
        assert!(parse_head("").is_none());
        assert!(parse_head("POST").is_none());
        assert!(parse_head("POST /info HTTP/1.1\r\nContent-Length: nan").is_none());
    }

    #[test]
    fn response_framing_includes_length_and_connection() {
        let resp = format_response(200, r#"{"ok":true}"#, true);
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(resp.contains("Content-Length: 11\r\n"));
        assert!(resp.contains("Connection: keep-alive\r\n"));
        assert!(resp.ends_with("\r\n\r\n{\"ok\":true}"));

        let resp = format_response(502, "{}", false);
        assert!(resp.starts_with("HTTP/1.1 502 Bad Gateway\r\n"));
        assert!(resp.contains("Connection: close\r\n"));
    }

    #[test]
    fn finds_head_terminator() {
        assert_eq!(find_head_end(b"POST / HTTP/1.1\r\n\r\nbody"), Some(15));
        assert_eq!(find_head_end(b"partial\r\n"), None);
    }
}
