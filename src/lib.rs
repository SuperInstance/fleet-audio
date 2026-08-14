//! fleet-audio — Streaming MIDI → audio renderer.
//!
//! **Phase 4** of the fleet infrastructure: replaces the numpy OOM killer
//! with a fixed-memory streaming pipeline. Memory is O(chunk), never O(duration).
//!
//! ## Architecture
//!
//! ```text
//!  MIDI Input → Lock-free SPSC Ring → Audio Thread → WAV Writer
//!  (JSONL/HTTP)   (MidiEvent)         (Synthesizer)   (Streaming)
//! ```
//!
//! - Chunk size: 1024 samples (~23ms at 44100Hz)
//! - Max buffered audio: 1 second
//! - No allocation in the audio thread
//! - Lock-free ring between MIDI input and synthesis

pub mod config;
pub mod midi;
pub mod ring;
pub mod synth;
pub mod voice;
pub mod io;
pub mod wav;

pub use config::Config;
pub use midi::MidiEvent;
pub use ring::EventRing;
pub use synth::Synthesizer;
