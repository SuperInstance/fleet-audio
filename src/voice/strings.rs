//! String voice — sawtooth + vibrato + slow attack.
//!
//! The sawtooth provides a rich string-like tone.
//! Vibrato (LFO-modulated frequency) adds the "living" quality of strings.
//! The slow attack gives the bowing feel.

use super::{AdsrEnvelope, Voice};

/// String voice with vibrato.
pub struct StringVoice {
    note: Option<u8>,
    freq: f32,
    phase: f32,
    phase_inc: f32,
    velocity_amp: f32,
    envelope: AdsrEnvelope,
    /// Vibrato LFO phase.
    vibrato_phase: f32,
    /// Vibrato depth in semitones (cents * 0.01).
    vibrato_depth: f32,
    /// Vibrato rate in Hz.
    vibrato_rate: f32,
}

impl StringVoice {
    pub fn new() -> Self {
        Self {
            note: None,
            freq: 0.0,
            phase: 0.0,
            phase_inc: 0.0,
            velocity_amp: 0.0,
            envelope: AdsrEnvelope::strings(44100.0),
            vibrato_phase: 0.0,
            vibrato_depth: 0.15, // ~15 cents
            vibrato_rate: 5.0,   // 5 Hz vibrato
        }
    }

    #[inline]
    fn render_sample(&mut self, sample_rate: f32) -> f32 {
        let env_level = self.envelope.process();
        if env_level <= 0.0 {
            return 0.0;
        }

        // Vibrato: modulate frequency slightly
        let vibrato = fast_sin(self.vibrato_phase) * self.vibrato_depth;
        let modulated_freq = self.freq * 2.0_f32.powf(vibrato / 12.0);
        let inc = modulated_freq / sample_rate;

        // Sawtooth wave
        let saw = self.phase * 2.0 - 1.0;
        self.phase += inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        // Advance vibrato LFO
        self.vibrato_phase += self.vibrato_rate / sample_rate;
        if self.vibrato_phase >= 1.0 {
            self.vibrato_phase -= 1.0;
        }

        // Soften the sawtooth edges a bit for string warmth
        let softened = saw * 0.6 + (saw * saw * saw) * 0.4;

        softened * env_level * self.velocity_amp * 0.4
    }
}

impl Voice for StringVoice {
    fn note_on(&mut self, note: u8, velocity: u8) {
        self.note = Some(note);
        self.freq = crate::midi::MidiEvent::note_to_freq(note);
        self.velocity_amp = velocity as f32 / 127.0;
        self.phase = 0.0;
        self.vibrato_phase = 0.0;
        self.phase_inc = self.freq / 44100.0;
        self.envelope.trigger();
    }

    fn note_off(&mut self) {
        self.envelope.release();
    }

    fn render(&mut self, output: &mut [f32], sample_rate: u32) {
        let sr = sample_rate as f32;
        self.phase_inc = self.freq / sr;
        for sample in output.iter_mut() {
            *sample += self.render_sample(sr);
        }
    }

    fn is_active(&self) -> bool {
        !self.envelope.is_idle()
    }

    fn current_note(&self) -> Option<u8> {
        self.note
    }
}

/// Fast sine approximation.
#[inline]
fn fast_sin(phase: f32) -> f32 {
    let x = phase * 2.0 * std::f32::consts::PI;
    let x = x - 2.0 * std::f32::consts::PI * ((x / (2.0 * std::f32::consts::PI)).floor() + 0.5);
    let y = x * x;
    let s = 0.224 + 0.776 * (1.0 - y / 10.0);
    let result = x * s;
    result * (2.0 - result.abs() * 0.625)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_produces_sound() {
        let mut voice = StringVoice::new();
        voice.note_on(67, 90); // G4
        assert!(voice.is_active());

        let mut buffer = [0.0_f32; 1024];
        voice.render(&mut buffer, 44100);
        let peak = buffer.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(peak > 0.0, "String voice should produce sound");
    }

    #[test]
    fn string_slow_attack() {
        let mut voice = StringVoice::new();
        voice.note_on(67, 100);

        let mut early = [0.0_f32; 100];
        voice.render(&mut early, 44100);
        let early_peak = early.iter().cloned().fold(0.0_f32, f32::abs);

        let mut later = [0.0_f32; 10000];
        voice.render(&mut later, 44100);
        let later_peak = later.iter().cloned().fold(0.0_f32, f32::abs);

        // Slow attack: later should be louder than early
        assert!(
            later_peak > early_peak,
            "String voice should ramp up slowly (slow attack)"
        );
    }

    #[test]
    fn string_silences_after_release() {
        let mut voice = StringVoice::new();
        voice.note_on(67, 100);

        // Let it reach sustain
        let mut warmup = vec![0.0_f32; 22050]; // 0.5s
        voice.render(&mut warmup, 44100);

        voice.note_off();
        let mut release = vec![0.0_f32; 44100]; // 1s
        voice.render(&mut release, 44100);
        let tail = release[40000..].iter().cloned().fold(0.0_f32, f32::abs);
        assert!(tail < 0.01);
        assert!(!voice.is_active());
    }
}
