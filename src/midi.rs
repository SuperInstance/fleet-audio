//! MIDI event types — compatible with fleet-ensemble's CNS bus.
//!
//! These mirror the `MidiEvent` struct from `fleet-ensemble::midi_io::stream`
//! so the two systems interoperate via JSONL serialization.

use serde::{Deserialize, Serialize};

/// A MIDI event with timestamp.
///
/// This is the universal currency of the fleet-audio pipeline.
/// Produced by JSONL spool readers, HTTP endpoints, or SMF parsers.
/// Consumed by the synthesizer via the lock-free ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiEvent {
    /// Timestamp in microseconds since renderer start.
    pub timestamp_us: u64,
    /// MIDI channel (0–15).
    pub channel: u8,
    /// Note number (0–127, 60 = middle C).
    pub note: u8,
    /// Velocity (0–127, 0 = note off).
    pub velocity: u8,
}

impl MidiEvent {
    /// Is this a note-on event?
    #[inline]
    pub fn is_note_on(&self) -> bool {
        self.velocity > 0
    }

    /// Is this a note-off event?
    #[inline]
    pub fn is_note_off(&self) -> bool {
        self.velocity == 0
    }

    /// Create a note-on event.
    pub fn note_on(channel: u8, note: u8, velocity: u8, timestamp_us: u64) -> Self {
        Self { timestamp_us, channel, note, velocity }
    }

    /// Create a note-off event.
    pub fn note_off(channel: u8, note: u8, timestamp_us: u64) -> Self {
        Self { timestamp_us, channel, note, velocity: 0 }
    }

    /// Convert MIDI note number to frequency in Hz.
    /// Uses equal temperament: f = 440 * 2^((n - 69) / 12)
    #[inline]
    pub fn note_to_freq(note: u8) -> f32 {
        440.0_f32 * 2.0_f32.powf((note as f32 - 69.0) / 12.0)
    }
}

/// Voice type selection based on MIDI channel.
/// Maps to the synthesis engines in `voice/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceType {
    Piano,
    Bass,
    Strings,
    Guitar,
    Drums,
}

impl VoiceType {
    /// Map a MIDI channel to a voice type.
    /// General MIDI convention:
    /// - 0 = Piano (acoustic grand)
    /// - 1 = Bass (electric bass)
    /// - 2 = Strings (string ensemble)
    /// - 3 = Guitar (acoustic guitar)
    /// - 9 = Drums (channel 10, 0-indexed)
    pub fn from_channel(ch: u8) -> Self {
        match ch {
            0 => VoiceType::Piano,
            1 => VoiceType::Bass,
            2 => VoiceType::Strings,
            3 => VoiceType::Guitar,
            9 => VoiceType::Drums,
            _ => VoiceType::Piano, // default to piano
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_on_off_detection() {
        let on = MidiEvent::note_on(0, 60, 100, 0);
        assert!(on.is_note_on());
        assert!(!on.is_note_off());

        let off = MidiEvent::note_off(0, 60, 0);
        assert!(off.is_note_off());
        assert!(!off.is_note_on());
    }

    #[test]
    fn note_to_freq_middle_c() {
        // MIDI 60 = C5 in MIDI convention (C4 scientific), ~261.63 Hz
        let freq = MidiEvent::note_to_freq(60);
        approx::assert_relative_eq!(freq, 261.6256, epsilon = 0.01);
    }

    #[test]
    fn note_to_freq_a4() {
        // MIDI 69 = A4 = 440 Hz exactly
        let freq = MidiEvent::note_to_freq(69);
        approx::assert_relative_eq!(freq, 440.0, epsilon = 0.01);
    }

    #[test]
    fn voice_type_mapping() {
        assert_eq!(VoiceType::from_channel(0), VoiceType::Piano);
        assert_eq!(VoiceType::from_channel(1), VoiceType::Bass);
        assert_eq!(VoiceType::from_channel(2), VoiceType::Strings);
        assert_eq!(VoiceType::from_channel(3), VoiceType::Guitar);
        assert_eq!(VoiceType::from_channel(9), VoiceType::Drums);
        // Default fallback
        assert_eq!(VoiceType::from_channel(15), VoiceType::Piano);
    }
}
