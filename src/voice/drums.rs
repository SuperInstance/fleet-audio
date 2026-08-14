//! Drum voice — filtered noise (snare/hat) + sine boom (kick).
//!
//! Maps specific MIDI note numbers to drum sounds:
//! - 35/36 = Bass Drum (kick) — sine boom with pitch drop
//! - 38/40 = Snare — filtered noise + tone
//! - 42/46 = Hi-Hat — high-passed noise, short
//! - 49/57 = Crash — filtered noise, long decay
//!
//! For simplicity, any unmapped note defaults to a kick-like boom.

use super::Voice;

/// Drum voice: produces percussive sounds.
pub struct DrumVoice {
    note: Option<u8>,
    /// Drum type (determined from note number).
    drum_type: DrumType,
    /// Noise state (LCG).
    noise_state: u32,
    /// Sine phase for kick drum.
    phase: f32,
    /// Sine phase increment.
    phase_inc: f32,
    /// Current amplitude envelope level.
    level: f32,
    /// Envelope decay rate per sample.
    decay_rate: f32,
    /// Filter state for filtered noise.
    filter_state: f32,
    /// Kick pitch envelope: starts high, drops to low.
    kick_freq_current: f32,
    kick_freq_target: f32,
    kick_freq_decay: f32,
    /// Active flag.
    active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DrumType {
    Kick,
    Snare,
    HiHat,
    Crash,
}

impl DrumVoice {
    pub fn new() -> Self {
        Self {
            note: None,
            drum_type: DrumType::Kick,
            noise_state: 67890,
            phase: 0.0,
            phase_inc: 0.0,
            level: 0.0,
            decay_rate: 0.0,
            filter_state: 0.0,
            kick_freq_current: 0.0,
            kick_freq_target: 0.0,
            kick_freq_decay: 0.0,
            active: false,
        }
    }

    /// Map MIDI note to drum type.
    fn note_to_drum(note: u8) -> DrumType {
        match note {
            35 | 36 => DrumType::Kick,
            38 | 40 | 39 | 41 => DrumType::Snare,
            42 | 44 | 46 => DrumType::HiHat,
            49 | 51 | 52 | 57 => DrumType::Crash,
            _ => DrumType::Kick, // Default
        }
    }

    /// Set parameters based on drum type.
    fn configure(&mut self, drum_type: DrumType, velocity: u8) {
        let vel = velocity as f32 / 127.0;
        self.drum_type = drum_type;
        self.level = vel;
        self.phase = 0.0;
        self.noise_state = 67890; // Reset noise for consistency

        match drum_type {
            DrumType::Kick => {
                // Pitch drop: 150Hz → 50Hz over ~50ms
                self.kick_freq_current = 150.0;
                self.kick_freq_target = 50.0;
                self.kick_freq_decay = 0.001; // per sample
                self.phase_inc = self.kick_freq_current / 44100.0;
                self.decay_rate = 0.0005; // ~200ms decay
            }
            DrumType::Snare => {
                // Medium decay noise + tone
                self.decay_rate = 0.002; // ~50ms
                self.filter_state = 0.0;
            }
            DrumType::HiHat => {
                // Very short, bright
                self.decay_rate = 0.01; // ~10ms
                self.filter_state = 0.0;
            }
            DrumType::Crash => {
                // Long decay, bright
                self.decay_rate = 0.0003; // ~300ms+
                self.filter_state = 0.0;
            }
        }
    }

    /// Generate filtered noise sample.
    #[inline]
    fn noise_sample(&mut self) -> f32 {
        // LCG noise
        self.noise_state ^= self.noise_state << 13;
        self.noise_state ^= self.noise_state >> 17;
        self.noise_state ^= self.noise_state << 5;
        (self.noise_state as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Render one sample based on drum type.
    #[inline]
    fn render_sample(&mut self) -> f32 {
        if self.level <= 0.001 {
            self.active = false;
            return 0.0;
        }

        let sample = match self.drum_type {
            DrumType::Kick => {
                // Sine with pitch envelope
                let s = fast_sin(self.phase);
                self.phase += self.phase_inc;
                if self.phase >= 1.0 {
                    self.phase -= 1.0;
                }

                // Pitch drop
                if self.kick_freq_current > self.kick_freq_target {
                    self.kick_freq_current -= self.kick_freq_decay * 44100.0;
                    if self.kick_freq_current < self.kick_freq_target {
                        self.kick_freq_current = self.kick_freq_target;
                    }
                    self.phase_inc = self.kick_freq_current / 44100.0;
                }

                s * 0.8
            }
            DrumType::Snare => {
                // Mix of filtered noise (body) and tone (200 Hz sine)
                let noise = self.noise_sample();
                // Lowpass the noise for snare body
                self.filter_state = self.filter_state * 0.7 + noise * 0.3;
                let tone = fast_sin(self.phase) * 0.3;
                self.phase += 200.0 / 44100.0;
                if self.phase >= 1.0 {
                    self.phase -= 1.0;
                }
                self.filter_state * 0.6 + tone
            }
            DrumType::HiHat => {
                // High-passed noise (bright, metallic)
                let noise = self.noise_sample();
                let hp = noise - self.filter_state * 0.9;
                self.filter_state = self.filter_state * 0.9 + noise * 0.1;
                hp * 0.5
            }
            DrumType::Crash => {
                // Bright noise, long decay
                let noise = self.noise_sample();
                let hp = noise - self.filter_state * 0.95;
                self.filter_state = self.filter_state * 0.95 + noise * 0.05;
                hp * 0.6
            }
        };

        // Apply amplitude decay
        let output = sample * self.level;
        self.level -= self.decay_rate;
        if self.level < 0.0 {
            self.level = 0.0;
        }

        output
    }
}

impl Voice for DrumVoice {
    fn note_on(&mut self, note: u8, velocity: u8) {
        self.note = Some(note);
        let drum = Self::note_to_drum(note);
        self.configure(drum, velocity);
        self.active = true;
    }

    fn note_off(&mut self) {
        // Drums don't really have note-off — they decay naturally.
        // But we can speed up the decay.
        self.decay_rate *= 4.0;
    }

    fn render(&mut self, output: &mut [f32], _sample_rate: u32) {
        for sample in output.iter_mut() {
            *sample += self.render_sample();
        }
    }

    fn is_active(&self) -> bool {
        self.active
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
    fn kick_drum_produces_boom() {
        let mut voice = DrumVoice::new();
        voice.note_on(36, 100); // Bass drum
        assert!(voice.is_active());

        let mut buffer = [0.0_f32; 512];
        voice.render(&mut buffer, 44100);
        let peak = buffer.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(peak > 0.0, "Kick drum should produce sound");
    }

    #[test]
    fn snare_produces_noise() {
        let mut voice = DrumVoice::new();
        voice.note_on(38, 100); // Snare
        assert!(voice.is_active());

        let mut buffer = [0.0_f32; 512];
        voice.render(&mut buffer, 44100);
        let peak = buffer.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(peak > 0.0, "Snare should produce sound");
    }

    #[test]
    fn hihat_is_short() {
        let mut voice = DrumVoice::new();
        voice.note_on(42, 100); // Hi-hat

        let mut early = [0.0_f32; 500]; // ~11ms
        voice.render(&mut early, 44100);
        let early_peak = early.iter().cloned().fold(0.0_f32, f32::abs);

        // After 200ms, should be silent
        let mut late = vec![0.0_f32; 8820]; // 200ms
        voice.render(&mut late, 44100);
        let late_peak = late.iter().cloned().fold(0.0_f32, f32::abs);

        assert!(early_peak > 0.0, "Hi-hat should produce initial sound");
        assert!(late_peak < 0.01, "Hi-hat should decay quickly");
        assert!(!voice.is_active(), "Hi-hat should be inactive after decay");
    }

    #[test]
    fn drum_velocity_affects_volume() {
        let mut loud = DrumVoice::new();
        let mut quiet = DrumVoice::new();
        loud.note_on(36, 127);
        quiet.note_on(36, 30);

        let mut bl = [0.0_f32; 256];
        let mut bq = [0.0_f32; 256];
        loud.render(&mut bl, 44100);
        quiet.render(&mut bq, 44100);

        let pl = bl.iter().cloned().fold(0.0_f32, f32::abs);
        let pq = bq.iter().cloned().fold(0.0_f32, f32::abs);
        assert!(pl > pq, "Higher velocity = louder drum");
    }

    #[test]
    fn crash_is_longer_than_hihat() {
        // Crash should decay slower than hi-hat
        let mut crash = DrumVoice::new();
        let mut hat = DrumVoice::new();
        crash.note_on(49, 100);
        hat.note_on(42, 100);

        // After ~100ms (4410 samples)
        let mut buf = vec![0.0_f32; 4410];
        crash.render(&mut buf, 44100);
        let crash_level = buf.iter().cloned().fold(0.0_f32, f32::abs);

        let mut buf = vec![0.0_f32; 4410];
        hat.render(&mut buf, 44100);
        let hat_level = buf.iter().cloned().fold(0.0_f32, f32::abs);

        // By this point, hi-hat should be near zero but crash should still have energy
        // (or at least crash should have more energy than hat)
        assert!(
            crash_level >= hat_level,
            "Crash should sustain longer than hi-hat"
        );
    }
}
