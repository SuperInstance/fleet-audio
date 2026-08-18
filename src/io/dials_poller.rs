//! Dials endpoint poller — feeds the elephant's live field into `FeelPulse`.
//!
//! Polls a `--dials-endpoint <url>` (the elephant `roomd`'s `GET /field`, see
//! `PLAN.md` §1.4) every ~2s, parses `{"dials": {"volume", "mood"}, ...}`,
//! NaN/inf-guards the reading, and pushes it onto a bounded channel that the
//! render loop drains each chunk — the poll happens off the audio thread, and
//! the audio thread only ever does a non-allocating `try_recv`.

use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::warn;

use crate::feel::DialsReadings;

/// Default poll interval — "~2s" per PLAN.md §1.4.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Per-request timeout — guards against a hung or slow-loris field endpoint
/// wedging the poller forever.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The subset of the elephant's `/field` response fleet-audio cares about.
/// Unknown fields (`warmth`, `kappa`, `map_temperature`, ...) are ignored by
/// serde's default (non-`deny_unknown_fields`) behavior.
#[derive(Debug, Deserialize)]
struct FieldReading {
    dials: DialsJson,
}

#[derive(Debug, Deserialize)]
struct DialsJson {
    volume: f64,
    mood: f64,
}

/// Split an `http://host[:port][/path]` URL into its parts. Only plain HTTP
/// is supported — the field endpoint is a same-fleet, unauthenticated daemon.
fn parse_http_url(url: &str) -> anyhow::Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("only http:// dials endpoints are supported: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>()?),
        None => (authority.to_string(), 80),
    };
    Ok((host, port, path.to_string()))
}

/// Fetch the field endpoint once and extract a NaN/inf-guarded dials reading.
///
/// Returns `Ok(None)` when the endpoint answered but the reading was not
/// finite (a glitch upstream must not fabricate a movement downstream — the
/// same law `feel.rs`'s `push` already enforces). Returns `Err` when the
/// fetch or parse itself failed.
pub async fn fetch_dials_once(url: &str) -> anyhow::Result<Option<DialsReadings>> {
    let (host, port, path) = parse_http_url(url)?;

    let stream = tokio::time::timeout(
        FETCH_TIMEOUT,
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await??;

    let mut stream = stream;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\n\r\n");
    tokio::time::timeout(FETCH_TIMEOUT, stream.write_all(request.as_bytes())).await??;

    let mut reader = BufReader::new(stream);
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = tokio::time::timeout(FETCH_TIMEOUT, reader.read_line(&mut line)).await??;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            content_length = value.trim().parse().ok();
        }
    }

    let mut body = Vec::new();
    match content_length {
        Some(len) => {
            body.resize(len, 0);
            tokio::time::timeout(FETCH_TIMEOUT, reader.read_exact(&mut body)).await??;
        }
        None => {
            tokio::time::timeout(FETCH_TIMEOUT, reader.read_to_end(&mut body)).await??;
        }
    }

    let field: FieldReading = serde_json::from_slice(&body)?;
    Ok(guard_reading(field.dials.volume, field.dials.mood))
}

/// A glitchy upstream reading must not fabricate a movement downstream — the
/// same law `feel.rs`'s `push` enforces on the energy series. NaN/infinite
/// dials are dropped here rather than forwarded.
fn guard_reading(volume: f64, mood: f64) -> Option<DialsReadings> {
    if !volume.is_finite() || !mood.is_finite() {
        return None;
    }
    Some(DialsReadings::new(volume as f32, mood as f32))
}

/// Poll `url` every `interval`, pushing each guarded reading onto `tx`.
/// Runs forever; poll errors are logged and skipped, never fatal — a down
/// elephant must not take fleet-audio down with it.
pub async fn run_dials_poller(
    url: String,
    tx: crossbeam::channel::Sender<DialsReadings>,
    interval: Duration,
) {
    loop {
        match fetch_dials_once(&url).await {
            Ok(Some(readings)) => {
                if tx.try_send(readings).is_err() {
                    warn!("dials channel full or closed, dropping a reading from {url}");
                }
            }
            Ok(None) => {
                warn!("dials endpoint {url}: non-finite reading, skipping");
            }
            Err(e) => {
                warn!("dials endpoint {url}: poll failed: {e}");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serves `body` once as a well-formed HTTP/1.1 JSON response, then closes.
    fn spawn_mock_server_once(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        port
    }

    /// Serves `body` on every accepted connection — for testing the polling
    /// loop, which reconnects on every tick.
    fn spawn_mock_server_repeating(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        port
    }

    #[test]
    fn parse_http_url_splits_host_port_path() {
        let (host, port, path) = parse_http_url("http://127.0.0.1:4073/field").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 4073);
        assert_eq!(path, "/field");
    }

    #[test]
    fn parse_http_url_defaults_port_and_path() {
        let (host, port, path) = parse_http_url("http://elephant.local").unwrap();
        assert_eq!(host, "elephant.local");
        assert_eq!(port, 80);
        assert_eq!(path, "/");
    }

    #[test]
    fn parse_http_url_rejects_non_http() {
        assert!(parse_http_url("https://127.0.0.1:4073/field").is_err());
    }

    #[tokio::test]
    async fn fetch_dials_once_parses_field_json() {
        let port = spawn_mock_server_once(
            r#"{"dials":{"volume":0.8,"mood":0.5},"warmth":0.6,"kappa":0.1}"#,
        );
        let url = format!("http://127.0.0.1:{port}/field");
        let readings = fetch_dials_once(&url)
            .await
            .unwrap()
            .expect("well-formed field JSON should yield a reading");
        approx::assert_relative_eq!(readings.volume, 0.8, epsilon = 1e-6);
        approx::assert_relative_eq!(readings.mood, 0.5, epsilon = 1e-6);
    }

    #[test]
    fn guard_reading_rejects_nan_and_infinite() {
        assert!(guard_reading(f64::NAN, 0.0).is_none());
        assert!(guard_reading(0.0, f64::NAN).is_none());
        assert!(guard_reading(f64::INFINITY, 0.0).is_none());
        assert!(guard_reading(0.0, f64::NEG_INFINITY).is_none());
    }

    #[test]
    fn guard_reading_passes_finite_values() {
        let readings = guard_reading(0.8, 0.5).expect("finite values should pass the guard");
        approx::assert_relative_eq!(readings.volume, 0.8, epsilon = 1e-6);
        approx::assert_relative_eq!(readings.mood, 0.5, epsilon = 1e-6);
    }

    #[tokio::test]
    async fn fetch_dials_once_errors_on_malformed_json() {
        let port = spawn_mock_server_once("not json");
        let url = format!("http://127.0.0.1:{port}/field");
        assert!(fetch_dials_once(&url).await.is_err());
    }

    #[tokio::test]
    async fn run_dials_poller_feeds_channel_on_interval() {
        let port = spawn_mock_server_repeating(
            r#"{"dials":{"volume":0.6,"mood":0.2},"warmth":0.3}"#,
        );
        let url = format!("http://127.0.0.1:{port}/field");
        let (tx, rx) = crossbeam::channel::bounded(4);

        tokio::spawn(run_dials_poller(url, tx, Duration::from_millis(20)));

        let readings = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(r) = rx.try_recv() {
                    return r;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("poller should push a reading onto the channel within timeout");

        approx::assert_relative_eq!(readings.volume, 0.6, epsilon = 1e-6);
        approx::assert_relative_eq!(readings.mood, 0.2, epsilon = 1e-6);
    }
}
