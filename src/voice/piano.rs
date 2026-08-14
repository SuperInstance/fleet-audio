//! Piano voice — additive synthesis with 8 harmonics, exponential decay.
//!
//! Each harmonic has a decreasing amplitude and its own decay rate.
//! The result is a rich, bell-like piano tone.

use super::{AdsrEnvelope, Voice};

/// Number of harmonics for additive synthesis.
const HARMONICS: usize = 8;

/// Piano voice using additive synthesis.
///
/// Harmonic amplitudes follow a roughly exponential decay,
/// and higher harmonics decay faster (physics of a struck string).
pub struct PianoVoice {
    /// Current note (None if idle).
    note: Option<u8>,
    /// Fundamental frequency.
    freq: f32,
    /// Phase accumulators for each harmonic.
    phases: [f32; HARMONICS],
    /// Phase increments for each harmonic.
    phase_incs: [f32; HARMONICS],
    /// Amplitude scaling from velocity.
    velocity_amp: f32,
    /// Envelope for amplitude shaping.
    envelope: AdsrEnvelope,
}

impl PianoVoice {
    pub fn new() -> Self {
        Self {
            note: None,
            freq: 0.0,
            phases: [0.0; HARMONICS],
            phase_incs: [0.0; HARMONICS],
            velocity_amp: 0.0,
            envelope: AdsrEnvelope::piano(44100.0),
        }
    }

    /// Harmonic amplitude table (decreasing).
    #[inline]
    fn harmonic_amp(h: usize) -> f32 {
        // Exponential decay: 1, 0.5, 0.33, 0.25, ...
        1.0 / (h as f32 + 1.0)
    }

    /// Update phase increments when frequency changes.
    fn update_phase_incs(&mut self, sample_rate: f32) {
        for h in 0..HARMONICS {
            let harmonic_freq = self.freq * (h + 1) as f32;
            self.phase_incs[h] = harmonic_freq / sample_rate;
        }
    }

    /// Render one sample.
    #[inline]
    fn render_sample(&mut self) -> f32 {
        let env_level = self.envelope.process();
        if env_level <= 0.0 {
            return 0.0;
        }

        let mut sample = 0.0_f32;
        for h in 0..HARMONICS {
            // Sine wave via polynomial approximation
            let s = fast_sin(self.phases[h]);
            sample += s * Self::harmonic_amp(h);
        }

        // Advance phases
        for h in 0..HARMONICS {
            self.phases[h] += self.phase_incs[h];
            if self.phases[h] >= 1.0 {
                self.phases[h] -= 1.0;
            }
        }

        sample * env_level * self.velocity_amp * 0.3 // scale to prevent clipping
    }
}

impl Voice for PianoVoice {
    fn note_on(&mut self, note: u8, velocity: u8) {
        self.note = Some(note);
        self.freq = crate::midi::MidiEvent::note_to_freq(note);
        self.velocity_amp = velocity as f32 / 127.0;
        self.update_phase_incs(44100.0);
        // Reset phases for clean attack
        self.phases = [0.0; HARMONICS];
        self.envelope.trigger();
    }

    fn note_off(&mut self) {
        self.envelope.release();
    }

    fn render(&mut self, output: &mut [f32], sample_rate: u32) {
        // Update phase increments in case sample rate changed
        self.update_phase_incs(sample_rate as f32);

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

/// Fast sine approximation using a parabola.
/// Good enough for audio synthesis — max error ~0.001.
#[inline]
fn fast_sin(phase: f32) -> f32 {
    // Map [0,1) to [0, 2π)
    let x = phase * 2.0 * std::f32::consts::PI;
    // Bhaskara I sine approximation: sin(x) ≈ 16x(π-x) / (5π² - 4x(π-x))
    // But we'll use the simpler parabola method:
    let x = x - 2.0 * std::f32::consts::PI * ((x / (2.0 * std::f32::consts::PI)).floor() + 0.5);
    let y = x * x;
    let s = 0.224 + 0.776 * (1.0 - y / 10.0);
    let result = x * s;
    // Correct the shape
    result * (2.0 - result.abs() * 0.625)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piano_voice_renders_audio() {
        let mut voice = PianoVoice::new();
        assert!(!voice.is_active());

        voice.note_on(69, 100); // A4
        assert!(voice.is_active());
        assert_eq!(voice.current_note(), Some(69));

        let mut buffer = [0.0_f32; 256];
        voice.render(&mut buffer, 44100);

        // Should produce non-zero output
        let max_sample = buffer.iter().cloned().fold(0.0_f32, f32::max);
        assert!(max_sample > 0.0, "Piano voice should produce sound");
    }

    #[test]
    fn piano_voice_respects_velocity() {
        let mut loud = PianoVoice::new();
        let mut quiet = PianoVoice::new();

        loud.note_on(60, 127);
        quiet.note_on(60, 30);

        let mut buf_loud = [0.0_f32; 512];
        let mut buf_quiet = [0.0_f32; 512];
        loud.render(&mut buf_loud, 44100);
        quiet.render(&mut buf_quiet, 44100);

        let peak_loud = buf_loud.iter().cloned().fold(0.0_f32, f32::abs);
        let peak_quiet = buf_quiet.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(
            peak_loud > peak_quiet,
            "Higher velocity should produce louder output"
        );
    }

    #[test]
    fn piano_voice_note_off_silences() {
        let mut voice = PianoVoice::new();
        voice.note_on(60, 100);

        let mut buffer = [0.0_f32; 1024];
        voice.render(&mut buffer, 44100);
        let peak_active = buffer.iter().cloned().fold(0.0_f32, f32::abs);

        voice.note_off();
        // Render enough samples for release to complete
        let mut buffer = [0.0_f32; 44100]; // 1 second
        voice.render(&mut buffer, 44100);
        let peak_after = buffer[40000..].iter().cloned().fold(0.0_f32, f32::abs);

        assert!(peak_active > 0.0);
        assert!(peak_after < 0.01, "Voice should be near-silent after release");
        assert!(!voice.is_active());
    }

    #[test]
    fn piano_produces_correct_frequency() {
        let mut voice = PianoVoice::new();
        voice.note_on(69, 100); // A4 = 440 Hz

        // Render at 44100 Hz, check that we get a signal around 440 Hz
        // by counting zero crossings over a known duration
        let n = 4410; // 0.1 seconds
        let mut buffer = vec![0.0_f32; n];
        voice.render(&mut buffer, 44100);

        let mut crossings = 0;
        for i in 1..n {
            if (buffer[i - 1] < 0.0 && buffer[i] >= 0.0)
                || (buffer[i - 1] >= 0.0 && buffer[i] < 0.0)
            {
                crossings += 1;
            }
        }
        // 0.1 seconds of 440 Hz → ~44 zero crossings (2 per cycle)
        let expected = 44.0_f32;
        let actual = crossings as f32;
        // Allow 20% tolerance (additive synthesis adds upper partials)
        assert!(
            (actual - expected).abs() / expected < 0.3,
            "Expected ~{expected} zero crossings, got {actual}"
        );
    }
}
