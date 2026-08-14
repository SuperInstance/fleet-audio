//! Bass voice — sawtooth oscillator + one-pole lowpass filter.
//!
//! The sawtooth gives it a rich, buzzy character.
//! The lowpass filter tames the high frequencies for warmth.

use super::{AdsrEnvelope, Voice};

/// Simple one-pole lowpass filter.
struct OnePoleLowpass {
    a0: f32,
    b1: f32,
    z1: f32,
}

impl OnePoleLowpass {
    fn new() -> Self {
        Self {
            a0: 1.0,
            b1: 0.0,
            z1: 0.0,
        }
    }

    /// Set cutoff frequency.
    fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: f32) {
        let x = std::f32::consts::PI * cutoff_hz / sample_rate;
        self.a0 = x;
        self.b1 = 1.0 - x;
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.z1 = input * self.a0 + self.z1 * self.b1;
        self.z1
    }
}

/// Bass voice: sawtooth → lowpass filter → envelope.
pub struct BassVoice {
    note: Option<u8>,
    freq: f32,
    phase: f32,
    phase_inc: f32,
    velocity_amp: f32,
    filter: OnePoleLowpass,
    envelope: AdsrEnvelope,
}

impl BassVoice {
    pub fn new() -> Self {
        let mut filter = OnePoleLowpass::new();
        filter.set_cutoff(800.0, 44100.0); // Warm bass cutoff
        Self {
            note: None,
            freq: 0.0,
            phase: 0.0,
            phase_inc: 0.0,
            velocity_amp: 0.0,
            filter,
            envelope: AdsrEnvelope::bass(44100.0),
        }
    }

    #[inline]
    fn update_phase_inc(&mut self, sample_rate: f32) {
        self.phase_inc = self.freq / sample_rate;
    }

    /// Render one sample: sawtooth → filter → envelope.
    #[inline]
    fn render_sample(&mut self) -> f32 {
        let env_level = self.envelope.process();
        if env_level <= 0.0 {
            return 0.0;
        }

        // Sawtooth: ramp from -1 to 1
        let saw = self.phase * 2.0 - 1.0;
        self.phase += self.phase_inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        let filtered = self.filter.process(saw);
        filtered * env_level * self.velocity_amp
    }
}

impl Voice for BassVoice {
    fn note_on(&mut self, note: u8, velocity: u8) {
        self.note = Some(note);
        self.freq = crate::midi::MidiEvent::note_to_freq(note);
        self.velocity_amp = velocity as f32 / 127.0;
        self.update_phase_inc(44100.0);
        self.phase = 0.0;
        self.envelope.trigger();
    }

    fn note_off(&mut self) {
        self.envelope.release();
    }

    fn render(&mut self, output: &mut [f32], sample_rate: u32) {
        self.update_phase_inc(sample_rate as f32);
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
    fn bass_produces_sound() {
        let mut voice = BassVoice::new();
        voice.note_on(36, 100); // C2
        assert!(voice.is_active());

        let mut buffer = [0.0_f32; 512];
        voice.render(&mut buffer, 44100);
        let peak = buffer.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(peak > 0.0, "Bass voice should produce sound");
    }

    #[test]
    fn bass_respects_velocity() {
        let mut loud = BassVoice::new();
        let mut quiet = BassVoice::new();
        loud.note_on(36, 127);
        quiet.note_on(36, 20);

        let mut bl = [0.0_f32; 512];
        let mut bq = [0.0_f32; 512];
        loud.render(&mut bl, 44100);
        quiet.render(&mut bq, 44100);

        let pl = bl.iter().cloned().fold(0.0_f32, f32::abs);
        let pq = bq.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(pl > pq, "Higher velocity = louder");
    }

    #[test]
    fn bass_silences_after_release() {
        let mut voice = BassVoice::new();
        voice.note_on(40, 100);
        voice.note_off();

        let mut buffer = vec![0.0_f32; 44100];
        voice.render(&mut buffer, 44100);
        let tail = buffer[40000..].iter().cloned().fold(0.0_f32, f32::abs);
        assert!(tail < 0.01, "Should be silent after release");
        assert!(!voice.is_active());
    }
}
