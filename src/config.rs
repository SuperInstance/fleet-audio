//! Configuration for the streaming audio renderer.

use serde::{Deserialize, Serialize};

/// Sample rate in Hz. Standard CD quality.
pub const SAMPLE_RATE: u32 = 44_100;

/// Chunk size in frames. 1024 samples ≈ 23ms at 44100Hz.
/// This is the unit of work for the audio thread — each chunk is synthesized,
/// mixed, written, and discarded. Memory is O(chunks) = O(1).
pub const CHUNK_SIZE: usize = 1024;

/// Maximum number of simultaneous voices (polyphony).
pub const MAX_VOICES: usize = 64;

/// Number of ring buffer slots for MIDI events.
/// Must be a power of two for mask-based indexing.
pub const RING_CAPACITY: usize = 1024;

/// Master output gain (linear).
pub const MASTER_GAIN: f32 = 0.7;

/// Derived: seconds per chunk.
pub const CHUNK_DURATION_SECS: f32 = CHUNK_SIZE as f32 / SAMPLE_RATE as f32;

/// Derived: samples per second.
pub const SAMPLES_PER_SEC: u32 = SAMPLE_RATE;

/// Application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Audio chunk size in frames.
    pub chunk_size: usize,
    /// Maximum polyphony.
    pub max_voices: usize,
    /// Ring buffer capacity (must be power of 2).
    pub ring_capacity: usize,
    /// Master gain (0.0 to 1.0).
    pub master_gain: f32,
    /// Output WAV file path. If None, no file output.
    pub output_wav: Option<String>,
    /// JSONL spool directory for MIDI input. If None, no spool input.
    pub jsonl_spool: Option<String>,
    /// HTTP port for MIDI input (CNS bus). If None, no HTTP input.
    pub http_port: Option<u16>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_rate: SAMPLE_RATE,
            chunk_size: CHUNK_SIZE,
            max_voices: MAX_VOICES,
            ring_capacity: RING_CAPACITY,
            master_gain: MASTER_GAIN,
            output_wav: None,
            jsonl_spool: None,
            http_port: None,
        }
    }
}
