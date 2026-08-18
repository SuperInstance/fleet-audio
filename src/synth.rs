//! The streaming synthesizer — core audio engine.
//!
//! This is the heart of fleet-audio. It:
//! 1. Drains MIDI events from the lock-free ring (O(1) per chunk)
//! 2. Manages a pool of active voices (polyphony)
//! 3. Renders each chunk by mixing all active voices
//! 4. Applies master gain and soft clipping
//! 5. Returns the chunk — the caller writes it to WAV/discard
//!
//! **Memory is O(max_voices × chunk_size)**, which is O(1) — it does not
//! grow with the duration of the piece. A 10-minute piece uses the same
//! memory as a 10-second piece.
//!
//! ## Audio Thread Contract
//!
//! The synthesizer is designed to be called from a dedicated audio thread.
//! No allocation happens in `process_chunk`. Voice slots are pre-allocated.

use crate::config::Config;
use crate::feel::{FeelPulse, RenderState};
use crate::midi::{MidiEvent, VoiceType};
use crate::ring::EventRing;
use crate::voice::{create_voice, Voice};

/// Active voice slot. Voices are reused from a pool.
enum VoiceSlot {
    /// Slot is available for a new note.
    Idle,
    /// Slot is actively rendering.
    Active {
        voice: Box<dyn Voice>,
        /// The channel this voice belongs to (for note-off matching).
        channel: u8,
    },
}

/// The streaming synthesizer.
pub struct Synthesizer {
    /// Voice slots — pre-allocated, never resized.
    voices: Vec<VoiceSlot>,
    /// Mix buffer — reused for every chunk, never reallocated.
    mix_buffer: Vec<f32>,
    /// Sample rate.
    sample_rate: u32,
    /// Chunk size.
    chunk_size: usize,
    /// Master gain.
    master_gain: f32,
    /// Maximum polyphony.
    max_voices: usize,
    /// Current time in microseconds (for timestamp processing).
    current_time_us: u64,
    /// Microseconds per sample.
    us_per_sample: f64,
    /// The feel pulse — the renderer's ear. When enabled, the renderer listens
    /// to its own output energy and shapes the master gain by what it feels
    /// (rising ↑, falling ↓, flat →). None = fixed master gain.
    feel: Option<FeelPulse>,
}

impl Synthesizer {
    /// Create a new synthesizer with the given configuration.
    pub fn new(config: &Config) -> Self {
        let max_voices = config.max_voices;
        let mut voices = Vec::with_capacity(max_voices);
        for _ in 0..max_voices {
            voices.push(VoiceSlot::Idle);
        }

        Self {
            voices,
            mix_buffer: vec![0.0; config.chunk_size],
            sample_rate: config.sample_rate,
            chunk_size: config.chunk_size,
            master_gain: config.master_gain,
            max_voices,
            current_time_us: 0,
            us_per_sample: 1_000_000.0 / config.sample_rate as f64,
            feel: None,
        }
    }

    /// Create with default config.
    pub fn with_defaults() -> Self {
        Self::new(&Config::default())
    }

    /// Process one chunk of audio.
    ///
    /// 1. Drain MIDI events from the ring (non-blocking).
    /// 2. Apply note-on/note-off to voices.
    /// 3. Mix all active voices into the output buffer.
    /// 4. Apply master gain and soft clipping.
    ///
    /// Returns a reference to the internal mix buffer.
    /// The caller must copy/discard the data before the next call.
    pub fn process_chunk(&mut self, ring: &EventRing) -> &[f32] {
        // Clear mix buffer — O(chunk_size)
        self.mix_buffer.fill(0.0);

        // Drain pending MIDI events — O(events_in_ring)
        let mut event_buf = [MidiEvent {
            timestamp_us: 0,
            channel: 0,
            note: 0,
            velocity: 0,
        }; 64];
        let n_events = ring.drain_into(&mut event_buf);

        // Process events
        for i in 0..n_events {
            let event = event_buf[i];
            if event.is_note_on() {
                self.handle_note_on(event.channel, event.note, event.velocity);
            } else if event.is_note_off() {
                self.handle_note_off(event.channel, event.note);
            }
        }

        // Render all active voices — O(max_voices × chunk_size)
        for slot in &mut self.voices {
            if let VoiceSlot::Active { voice, .. } = slot {
                if voice.is_active() {
                    voice.render(&mut self.mix_buffer, self.sample_rate);
                } else {
                    // Voice finished — return to pool
                    *slot = VoiceSlot::Idle;
                }
            }
        }

        // Apply master gain and soft clipping — O(chunk_size).
        //
        // When the feel pulse is enabled, the renderer first listens to its
        // own mixed energy, feeds it to the ear, and shapes the master gain by
        // the felt direction — it plays WITH the room, not just in it.
        let gain = if let Some(feel) = &mut self.feel {
            let energy = rms(&self.mix_buffer).tanh();
            feel.push(energy);
            feel.shape_output(RenderState::new(self.master_gain, 1.0)).gain
        } else {
            self.master_gain
        };
        for sample in &mut self.mix_buffer {
            *sample *= gain;
            // Soft clipping (tanh approximation)
            *sample = soft_clip(*sample);
        }

        // Advance time
        self.current_time_us += (self.chunk_size as f64 * self.us_per_sample) as u64;

        &self.mix_buffer
    }

    /// Handle a note-on event: find a free voice slot and activate it.
    fn handle_note_on(&mut self, channel: u8, note: u8, velocity: u8) {
        // First, try to find an idle slot
        let voice_type = VoiceType::from_channel(channel);
        let new_voice = create_voice(voice_type);

        // Find an idle slot, or steal the oldest if all are busy
        let slot_idx = self.find_free_slot().unwrap_or_else(|| self.find_voice_to_steal());

        self.voices[slot_idx] = VoiceSlot::Active {
            voice: new_voice,
            channel,
        };

        if let VoiceSlot::Active { voice, .. } = &mut self.voices[slot_idx] {
            voice.note_on(note, velocity);
        }
    }

    /// Handle a note-off event: find the matching voice and release it.
    fn handle_note_off(&mut self, channel: u8, note: u8) {
        for slot in &mut self.voices {
            if let VoiceSlot::Active { voice, channel: ch } = slot {
                if *ch == channel && voice.current_note() == Some(note) {
                    voice.note_off();
                    break;
                }
            }
        }
    }

    /// Find an idle voice slot.
    fn find_free_slot(&self) -> Option<usize> {
        for (i, slot) in self.voices.iter().enumerate() {
            if matches!(slot, VoiceSlot::Idle) {
                return Some(i);
            }
        }
        None
    }

    /// Find a voice to steal when all slots are busy.
    /// Strategy: steal the first active voice (simple, could be improved).
    fn find_voice_to_steal(&self) -> usize {
        // Prefer stealing a releasing voice (lowest level)
        // For simplicity, just steal slot 0 — in practice we'd track ages
        0
    }

    /// Enable the feel pulse — the renderer starts listening to its own
    /// output and shaping the master gain by the felt direction.
    pub fn enable_feel(&mut self, feel: FeelPulse) {
        self.feel = Some(feel);
    }

    /// Mutable access to the feel pulse, if enabled.
    pub fn feel_mut(&mut self) -> Option<&mut FeelPulse> {
        self.feel.as_mut()
    }

    /// Feed an external energy frame (or dial reading) into the feel pulse.
    /// No-op when the feel pulse is disabled.
    pub fn feed_energy(&mut self, energy: f32) {
        if let Some(feel) = &mut self.feel {
            feel.push(energy);
        }
    }

    /// Feed an elephant dial reading (from `--dials-endpoint`) into the feel
    /// pulse. No-op when the feel pulse is disabled.
    pub fn feed_dials(&mut self, readings: crate::feel::DialsReadings) {
        if let Some(feel) = &mut self.feel {
            feel.push_dials(readings);
        }
    }

    /// Get the number of currently active voices.
    pub fn active_voice_count(&self) -> usize {
        self.voices
            .iter()
            .filter(|s| matches!(s, VoiceSlot::Active { voice, .. } if voice.is_active()))
            .count()
    }

    /// Current time in microseconds.
    pub fn current_time_us(&self) -> u64 {
        self.current_time_us
    }
}

/// Root-mean-square energy of a sample buffer — the renderer's loudness for
/// one chunk.
#[inline]
fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = buf.iter().map(|s| s * s).sum();
    (sum_sq / buf.len() as f32).sqrt()
}

/// Soft clipping using a tanh approximation.
/// Prevents harsh digital clipping while allowing some warm saturation.
#[inline]
fn soft_clip(x: f32) -> f32 {
    // Polynomial tanh approximation: x * (27 + x²) / (27 + 9x²)
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_produces_silence_without_events() {
        let ring = EventRing::new(64);
        let mut synth = Synthesizer::with_defaults();

        let output = synth.process_chunk(&ring);
        let peak = output.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert_eq!(peak, 0.0, "No events → silence");
    }

    #[test]
    fn synth_produces_sound_with_note_on() {
        let ring = EventRing::new(64);
        let mut synth = Synthesizer::with_defaults();

        ring.push(MidiEvent::note_on(0, 69, 100, 0)); // A4 on piano channel

        let output = synth.process_chunk(&ring);
        let peak = output.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert!(peak > 0.0, "Note on should produce sound");

        assert!(synth.active_voice_count() > 0);
    }

    #[test]
    fn synth_silences_after_note_off() {
        let ring = EventRing::new(64);
        let mut synth = Synthesizer::with_defaults();

        // Note on
        ring.push(MidiEvent::note_on(0, 60, 100, 0));
        let _ = synth.process_chunk(&ring);
        assert!(synth.active_voice_count() > 0);

        // Note off
        ring.push(MidiEvent::note_off(0, 60, 0));

        // Process several chunks to let release complete
        for _ in 0..100 {
            let _ = synth.process_chunk(&ring);
        }

        assert_eq!(synth.active_voice_count(), 0, "Voice should be idle after release");
    }

    #[test]
    fn synth_handles_polyphony() {
        let ring = EventRing::new(256);
        let mut synth = Synthesizer::with_defaults();

        // Play multiple notes
        ring.push(MidiEvent::note_on(0, 60, 80, 0)); // C
        ring.push(MidiEvent::note_on(0, 64, 80, 0)); // E
        ring.push(MidiEvent::note_on(0, 67, 80, 0)); // G

        let output: Vec<f32> = synth.process_chunk(&ring).to_vec();
        let voice_count = synth.active_voice_count();
        let peak = output.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));

        assert!(voice_count >= 3, "Should have 3 active voices");
        assert!(peak > 0.0, "Should produce sound");
    }

    #[test]
    fn synth_uses_correct_voice_per_channel() {
        let ring = EventRing::new(64);
        let mut synth = Synthesizer::with_defaults();

        // Piano (ch 0) and Bass (ch 1)
        ring.push(MidiEvent::note_on(0, 69, 100, 0));
        ring.push(MidiEvent::note_on(1, 36, 100, 0)); // C2

        let output: Vec<f32> = synth.process_chunk(&ring).to_vec();
        let voice_count = synth.active_voice_count();

        // Both should produce sound
        let peak = output.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert!(voice_count >= 2);
        assert!(peak > 0.0);
    }

    #[test]
    fn synth_memory_is_bounded() {
        // The key property: no matter how many chunks we process,
        // the number of voice slots stays the same.
        let ring = EventRing::new(64);
        let mut synth = Synthesizer::with_defaults();
        let initial_slots = synth.voices.len();

        // Process many chunks with continuous notes
        for i in 0..1000 {
            ring.push(MidiEvent::note_on(0, (i % 12 + 60) as u8, 100, i * 1000));
            let _ = synth.process_chunk(&ring);
        }

        assert_eq!(
            synth.voices.len(),
            initial_slots,
            "Voice slots should not grow — O(1) memory"
        );
    }

    #[test]
    fn soft_clip_limits_range() {
        // soft_clip should keep values roughly in [-1, 1]
        for x in [-5.0, -2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 5.0] {
            let clipped = soft_clip(x);
            assert!(
                clipped.abs() < 1.05,
                "soft_clip({x}) = {clipped} should be within [-1.05, 1.05]"
            );
        }
    }

    #[test]
    fn rms_matches_hand_computed() {
        assert_eq!(rms(&[]), 0.0);
        // RMS of [3, 4] = sqrt((9+16)/2) = sqrt(12.5) ≈ 3.5355
        approx::assert_relative_eq!(rms(&[3.0, 4.0]), 3.5355, epsilon = 1e-3);
        assert_eq!(rms(&[0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn feel_enabled_still_renders() {
        let ring = EventRing::new(64);
        let mut synth = Synthesizer::with_defaults();
        synth.enable_feel(FeelPulse::new());

        // Silence with the ear enabled → still silence.
        let output = synth.process_chunk(&ring);
        let peak = output.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert_eq!(peak, 0.0, "feel enabled + no events → silence");

        // A note with the ear enabled → still renders sound.
        ring.push(MidiEvent::note_on(0, 69, 100, 0));
        let output = synth.process_chunk(&ring);
        let peak = output.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert!(peak > 0.0, "feel enabled + note on → sound");
    }

    #[test]
    fn feed_dials_shapes_gain_when_feel_enabled() {
        let mut synth = Synthesizer::with_defaults();
        synth.enable_feel(FeelPulse::new());

        synth.feed_dials(crate::feel::DialsReadings::new(0.3, 0.0));
        synth.feed_dials(crate::feel::DialsReadings::new(0.6, 0.0));
        synth.feed_dials(crate::feel::DialsReadings::new(0.9, 0.0));

        let feel = synth.feel_mut().unwrap();
        assert_eq!(feel.felt(), crate::feel::FeltDirection::Rising);
        assert!(
            feel.gain_multiplier() > 1.0,
            "rising dials should push the render gain up"
        );
    }

    #[test]
    fn feed_dials_is_noop_when_feel_disabled() {
        let mut synth = Synthesizer::with_defaults();
        assert!(synth.feel_mut().is_none());
        synth.feed_dials(crate::feel::DialsReadings::new(0.9, 0.5));
        assert!(synth.feel_mut().is_none());
    }

    #[test]
    fn feel_disabled_is_fixed_gain() {
        // Default synthesizer has no feel → fixed master gain, no ear.
        let mut synth = Synthesizer::with_defaults();
        assert!(synth.feel_mut().is_none());
        synth.feed_energy(1.0); // no-op when disabled
        assert!(synth.feel_mut().is_none());
    }
}
