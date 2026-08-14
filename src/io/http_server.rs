//! HTTP server for receiving MIDI events from fleet-ensemble's CNS bus.
//!
//! POST /midi — accepts a JSON array (or single) of MIDI events.
//! POST /cns   — accepts CNS AgentPlayed packets (auto-converts).
//! GET  /health — health check.
//! GET  /status — synthesizer status (active voices, etc.).

use std::net::SocketAddr;
use std::sync::Arc;
use axum::{Router, routing::post, routing::get, extract::State, Json, http::StatusCode};
use serde::{Deserialize, Serialize};
use tracing::{info, debug, warn};
use crate::midi::MidiEvent;
use crate::ring::EventRing;

/// HTTP server state shared across handlers.
#[derive(Clone)]
struct HttpState {
    ring: Arc<EventRing>,
}

/// MIDI event(s) submitted via HTTP.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MidiPayload {
    Single(MidiEvent),
    Multiple(Vec<MidiEvent>),
}

/// CNS AgentPlayed packet.
#[derive(Debug, Deserialize)]
struct CnsAgentPlayed {
    timestamp_us: u64,
    pitch: u8,
    velocity: u8,
    #[serde(default)]
    channel: u8,
}

/// Status response.
#[derive(Debug, Serialize)]
struct StatusResponse {
    status: &'static str,
    ring_len: usize,
    ring_capacity: usize,
}

/// Run the HTTP server.
pub async fn run_http_server(
    addr: SocketAddr,
    ring: Arc<EventRing>,
) -> anyhow::Result<()> {
    let state = HttpState { ring };

    let app = Router::new()
        .route("/midi", post(handle_midi))
        .route("/cns", post(handle_cns))
        .route("/health", get(handle_health))
        .route("/status", get(handle_status))
        .with_state(state);

    info!("HTTP MIDI server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn handle_midi(
    State(state): State<HttpState>,
    Json(payload): Json<MidiPayload>,
) -> StatusCode {
    let events = match payload {
        MidiPayload::Single(e) => vec![e],
        MidiPayload::Multiple(v) => v,
    };

    let total = events.len();
    let mut pushed = 0;
    for event in events {
        if state.ring.push(event) {
            pushed += 1;
        } else {
            warn!("Ring buffer full, dropping MIDI event");
        }
    }

    debug!("HTTP /midi: pushed {pushed}/{total} events");
    if pushed > 0 {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn handle_cns(
    State(state): State<HttpState>,
    Json(payload): Json<CnsAgentPlayed>,
) -> StatusCode {
    let event = MidiEvent {
        timestamp_us: payload.timestamp_us,
        channel: payload.channel,
        note: payload.pitch,
        velocity: payload.velocity,
    };

    if state.ring.push(event) {
        debug!("HTTP /cns: note={} vel={}", payload.pitch, payload.velocity);
        StatusCode::OK
    } else {
        warn!("Ring buffer full, dropping CNS event");
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn handle_health() -> &'static str {
    "ok"
}

async fn handle_status(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "running",
        "ring_len": state.ring.len(),
        "ring_capacity": state.ring.capacity(),
    }))
}
