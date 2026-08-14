//! Guitar voice — Karplus-Strong string synthesis.
//!
//! The Karplus-Strong algorithm models a plucked string:
//! 1. Fill a delay line with noise (the "pluck" — broad spectrum)
//! 2. On each sample, read from the delay line
//! 3. Feed back: average of current and next sample → delay line
//! 4. The averaging is a lowpass filter that progressively damps high frequencies
//!
//! The delay line length = sample_rate / frequency, which sets the pitch.
//! This naturally produces the correct harmonic structure and decay.

use super::{AdsrEnvelope, Voice};

/// Maximum delay line length (for low notes at 44100 Hz).
/// C0 (MIDI 0) ≈ 16.35 Hz → 2699 samples. Round up.
const MAX_DELAY: usize = 4096;

/// Guitar voice using Karplus-Strong synthesis.
pub struct GuitarVoice {
    note: Option<u8>,
    /// Delay line buffer (pre-allocated, never resized).
    delay_line: [f32; MAX_DELAY],
    /// Read index into the delay line.
    read_idx: usize,
    /// Write index into the delay line.
    write_idx: usize,
    /// Effective delay length for the current note.
    delay_length: usize,
    /// Decay factor (0.99 = long sustain, 0.5 = short pluck).
    decay: f32,
    /// Amplitude from velocity.
    velocity_amp: f32,
    /// Envelope for amplitude shaping.
    envelope: AdsrEnvelope,
    /// Simple deterministic noise generator state (for pluck initialization).
    noise_state: u32,
}

impl GuitarVoice {
    pub fn new() -> Self {
        Self {
            note: None,
            delay_line: [0.0; MAX_DELAY],
            read_idx: 0,
            write_idx: 0,
            delay_length: 0,
            decay: 0.996,
            velocity_amp: 0.0,
            envelope: AdsrEnvelope::guitar(44100.0),
            noise_state: 12345,
        }
    }

    /// Initialize the delay line with "noise" (the pluck).
    /// Uses a simple LCG for deterministic, allocation-free noise.
    fn excite(&mut self) {
        let n = self.delay_length;
        for i in 0..n {
            // Simple LCG noise: xorshift variant
            self.noise_state ^= self.noise_state << 13;
            self.noise_state ^= self.noise_state >> 17;
            self.noise_state ^= self.noise_state << 5;
            let noise = (self.noise_state as f32 / u32::MAX as f32) * 2.0 - 1.0;
            self.delay_line[i] = noise * self.velocity_amp;
        }
        self.read_idx = 0;
        self.write_idx = 0;
    }

    #[inline]
    fn render_sample(&mut self) -> f32 {
        let env_level = self.envelope.process();
        if env_level <= 0.0 || self.delay_length == 0 {
            return 0.0;
        }

        let current = self.delay_line[self.read_idx];
        let next = self.delay_line[(self.read_idx + 1) % self.delay_length];

        // Karplus-Strong: average current and next, apply decay
        let output = current;
        let feedback = (current + next) * 0.5 * self.decay;

        // Write feedback into the delay line
        self.delay_line[self.write_idx] = feedback;

        // Advance indices
        self.read_idx = (self.read_idx + 1) % self.delay_length;
        self.write_idx = (self.write_idx + 1) % self.delay_length;

        output * env_level
    }
}

impl Voice for GuitarVoice {
    fn note_on(&mut self, note: u8, velocity: u8) {
        self.note = Some(note);
        let freq = crate::midi::MidiEvent::note_to_freq(note);
        self.delay_length = (44100.0 / freq) as usize;
        if self.delay_length > MAX_DELAY {
            self.delay_length = MAX_DELAY;
        }
        if self.delay_length < 2 {
            self.delay_length = 2;
        }
        self.velocity_amp = velocity as f32 / 127.0;
        self.excite();
        self.envelope.trigger();
    }

    fn note_off(&mut self) {
        self.envelope.release();
    }

    fn render(&mut self, output: &mut [f32], _sample_rate: u32) {
        for sample in output.iter_mut() {
            *sample += self.render_sample();
        }
    }

    fn is_active(&self) -> bool {
        !self.envelope.is_idle()
    }

    fn current_note(&self) -> Option<u8> {
        self.note
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guitar_produces_sound() {
        let mut voice = GuitarVoice::new();
        voice.note_on(52, 100); // E3
        assert!(voice.is_active());

        let mut buffer = [0.0_f32; 512];
        voice.render(&mut buffer, 44100);
        let peak = buffer.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert!(peak > 0.0, "Guitar voice should produce sound");
    }

    #[test]
    fn guitar_pluck_decays() {
        let mut voice = GuitarVoice::new();
        voice.note_on(52, 100);

        // Early samples should be louder (pluck)
        let mut early = [0.0_f32; 1000];
        voice.render(&mut early, 44100);
        let early_peak = early.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));

        // Later samples should be quieter (decay)
        let mut late = vec![0.0_f32; 44100]; // 1 second
        voice.render(&mut late, 44100);
        let late_peak = late[40000..].iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));

        assert!(
            early_peak > late_peak,
            "Guitar should decay: early={early_peak:.4} late={late_peak:.4}"
        );
    }

    #[test]
    fn guitar_respects_velocity() {
        let mut loud = GuitarVoice::new();
        let mut quiet = GuitarVoice::new();
        loud.note_on(52, 127);
        quiet.note_on(52, 20);

        let mut bl = [0.0_f32; 256];
        let mut bq = [0.0_f32; 256];
        loud.render(&mut bl, 44100);
        quiet.render(&mut bq, 44100);

        let pl = bl.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        let pq = bq.iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert!(pl > pq, "Higher velocity = louder pluck");
    }

    #[test]
    fn guitar_silences_after_release() {
        let mut voice = GuitarVoice::new();
        voice.note_on(52, 100);
        voice.note_off();

        let mut buffer = vec![0.0_f32; 44100];
        voice.render(&mut buffer, 44100);
        let tail = buffer[40000..].iter().cloned().fold(0.0_f32, |acc, x| acc.max(x.abs()));
        assert!(tail < 0.01, "Should be silent after release");
        assert!(!voice.is_active());
    }
}
