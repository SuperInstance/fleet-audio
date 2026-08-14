//! Synthesis voices — each voice type produces audio samples from MIDI notes.
//!
//! All voices implement the [`Voice`] trait. Each voice maintains its own
//! internal state (phase, envelope, delay lines) and renders one sample at
//! a time. Voices are designed for real-time: no allocation in the render
//! path, all state is pre-allocated.

pub mod piano;
pub mod bass;
pub mod strings;
pub mod guitar;
pub mod drums;

use crate::midi::VoiceType;

pub use piano::PianoVoice;
pub use bass::BassVoice;
pub use strings::StringVoice;
pub use guitar::GuitarVoice;
pub use drums::DrumVoice;

/// A synthesis voice that can render audio samples.
///
/// Contract:
/// - `note_on` / `note_off` control the voice.
/// - `render` fills the output buffer with samples.
/// - `is_active` returns false when the voice has finished ringing out.
/// - No allocation in any of these methods.
pub trait Voice: Send {
    /// Trigger a note on.
    fn note_on(&mut self, note: u8, velocity: u8);

    /// Trigger a note off (begin release phase).
    fn note_off(&mut self);

    /// Render `n` samples into the provided buffer.
    /// Mixes (adds) into the buffer — does not overwrite.
    fn render(&mut self, output: &mut [f32], sample_rate: u32);

    /// Is this voice currently producing sound?
    fn is_active(&self) -> bool;

    /// What note is this voice playing? (None if idle)
    fn current_note(&self) -> Option<u8>;
}

/// Create a new voice of the given type.
pub fn create_voice(voice_type: VoiceType) -> Box<dyn Voice> {
    match voice_type {
        VoiceType::Piano => Box::new(PianoVoice::new()),
        VoiceType::Bass => Box::new(BassVoice::new()),
        VoiceType::Strings => Box::new(StringVoice::new()),
        VoiceType::Guitar => Box::new(GuitarVoice::new()),
        VoiceType::Drums => Box::new(DrumVoice::new()),
    }
}

/// Simple ADSR envelope generator.
/// Used by multiple voices for amplitude shaping.
#[derive(Debug, Clone)]
pub struct AdsrEnvelope {
    /// Attack time in seconds.
    pub attack: f32,
    /// Decay time in seconds.
    pub decay: f32,
    /// Sustain level (0.0–1.0).
    pub sustain: f32,
    /// Release time in seconds.
    pub release: f32,

    // Internal state
    sample_rate: f32,
    phase: EnvelopePhase,
    level: f32,
    release_start_level: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EnvelopePhase {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

impl AdsrEnvelope {
    pub fn new(attack: f32, decay: f32, sustain: f32, release: f32, sample_rate: f32) -> Self {
        Self {
            attack,
            decay,
            sustain,
            release,
            sample_rate,
            phase: EnvelopePhase::Idle,
            level: 0.0,
            release_start_level: 0.0,
        }
    }

    /// Default piano-like envelope: fast attack, long decay.
    pub fn piano(sample_rate: f32) -> Self {
        Self::new(0.005, 0.3, 0.001, 0.4, sample_rate)
    }

    /// Default bass envelope: medium attack, medium decay.
    pub fn bass(sample_rate: f32) -> Self {
        Self::new(0.01, 0.15, 0.6, 0.2, sample_rate)
    }

    /// Default string envelope: slow attack, sustained.
    pub fn strings(sample_rate: f32) -> Self {
        Self::new(0.15, 0.1, 0.85, 0.5, sample_rate)
    }

    /// Default guitar envelope: very fast attack, exponential decay.
    pub fn guitar(sample_rate: f32) -> Self {
        Self::new(0.002, 0.4, 0.0, 0.3, sample_rate)
    }

    /// Trigger note on — start attack phase.
    pub fn trigger(&mut self) {
        self.phase = EnvelopePhase::Attack;
        self.level = 0.0;
    }

    /// Trigger note off — start release phase.
    pub fn release(&mut self) {
        if self.phase != EnvelopePhase::Idle {
            self.release_start_level = self.level;
            self.phase = EnvelopePhase::Release;
        }
    }

    /// Is the envelope idle (finished)?
    #[inline]
    pub fn is_idle(&self) -> bool {
        self.phase == EnvelopePhase::Idle
    }

    /// Advance the envelope by one sample and return the current level.
    #[inline]
    pub fn process(&mut self) -> f32 {
        let dt = 1.0 / self.sample_rate;
        match self.phase {
            EnvelopePhase::Idle => {
                self.level = 0.0;
            }
            EnvelopePhase::Attack => {
                if self.attack > 0.0 {
                    self.level += dt / self.attack;
                } else {
                    self.level = 1.0;
                }
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.phase = EnvelopePhase::Decay;
                }
            }
            EnvelopePhase::Decay => {
                if self.decay > 0.0 {
                    self.level -= dt / self.decay * (1.0 - self.sustain);
                } else {
                    self.level = self.sustain;
                }
                if self.level <= self.sustain {
                    self.level = self.sustain;
                    self.phase = EnvelopePhase::Sustain;
                }
            }
            EnvelopePhase::Sustain => {
                self.level = self.sustain;
            }
            EnvelopePhase::Release => {
                if self.release > 0.0 {
                    self.level -= dt / self.release * self.release_start_level;
                } else {
                    self.level = 0.0;
                }
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.phase = EnvelopePhase::Idle;
                }
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_attack_decay_sustain() {
        let mut env = AdsrEnvelope::new(0.01, 0.01, 0.5, 0.1, 44100.0);
        env.trigger();
        // After enough samples, should reach sustain
        for _ in 0..2000 {
            env.process();
        }
        approx::assert_relative_eq!(env.level, 0.5, epsilon = 0.05);
    }

    #[test]
    fn envelope_release_to_idle() {
        let mut env = AdsrEnvelope::new(0.001, 0.001, 0.8, 0.01, 44100.0);
        env.trigger();
        for _ in 0..1000 {
            env.process();
        }
        env.release();
        for _ in 0..1000 {
            env.process();
        }
        assert!(env.is_idle());
    }

    #[test]
    fn envelope_idle_stays_zero() {
        let mut env = AdsrEnvelope::piano(44100.0);
        for _ in 0..100 {
            let level = env.process();
            assert_eq!(level, 0.0);
        }
    }
}
