//! The renderer's ear — it doesn't just play; it feels the room.
//!
//! `FeelPulse` is the cross-pollination of two fleet concepts:
//!
//! * **LISTEN** — from `fleet-jepa-midi/vibe_matcher.py`: reduce the audio
//!   stream to an *energy frame* — energy, loudness (dB), and tempo as rolling
//!   moving averages. The vibe_matcher's acoustic ear, distilled to its
//!   energy essence.
//! * **FEEL** — from `elephant/pulse.py`: the perception-check math. One
//!   number is nothing; TWO numbers show DIRECTION; MORE THAN TWO show RATE OF
//!   CHANGE. Direction from the last two energy frames, rate (the second
//!   difference) from the last three+.
//!
//! The renderer's output is then *shaped* by what it feels: a rising pulse
//! pushes gain and velocity up, a falling pulse pulls them down, a flat pulse
//! holds. The renderer plays WITH the room, not just in it.
//!
//! # The chain
//!
//! ```text
//! audio frames → energy → FeelPulse (rolling energy/loudness/tempo)
//!              → perception check (direction / rate of change)
//!              → shape_output (rising ↑, falling ↓, flat →)
//! ```

use std::collections::VecDeque;

/// Per-pulse moves below this read as 0 — the number doesn't matter, only the
/// movement (mirrors `elephant/pulse.py::DEFAULT_NOISE_FLOOR`).
pub const DEFAULT_NOISE_FLOOR: f32 = 0.02;

/// Default number of energy frames held in the rolling history.
pub const DEFAULT_HISTORY: usize = 64;

/// Maximum push the feel applies to gain/velocity (a ±25% swing).
pub const MAX_PUSH: f32 = 0.25;

/// Weight of the direction (last two) in the felt push.
const DIR_WEIGHT: f32 = 1.0;

/// Weight of the rate of change (last three+) in the felt push.
const RATE_WEIGHT: f32 = 0.5;

/// How strongly the room's *mood* (warmth) tilts the felt energy.
const WARMTH_TILT: f32 = 0.2;

/// A beat is a rising energy edge this far above the rolling mean.
const ONSET_MARGIN: f32 = 0.05;

/// Exponential smoothing factor for the tempo estimate.
const TEMPO_ALPHA: f32 = 0.1;

/// Log-scale floor, so 0 energy reads as silence, not −inf dB.
const EPS: f32 = 1e-10;

/// Direction from the last TWO readings — the currency-pair insight.
///
/// One number is nothing; two numbers show DIRECTION. Returns the signed
/// movement between the last two entries of `series`, floored to 0 when below
/// `noise_floor`. The caller is expected to pass a NaN-sanitized series (see
/// [`FeelPulse::push`], which carries a glitch forward rather than letting it
/// fabricate a movement).
#[must_use]
pub fn direction(series: &[f32], noise_floor: f32) -> f32 {
    let n = series.len();
    if n < 2 {
        return 0.0;
    }
    let delta = series[n - 1] - series[n - 2];
    if delta.abs() < noise_floor {
        0.0
    } else {
        delta
    }
}

/// Rate of change from the last THREE+ readings — the second difference.
///
/// More than two numbers show RATE OF CHANGE. From the last three readings
/// this is the central second difference (the exact acceleration of the
/// quadratic interpolant through them — the elephant `three_reading_kinematics`
/// generalized to a scalar dial). Floored to 0 below `noise_floor`.
#[must_use]
pub fn rate_of_change(series: &[f32], noise_floor: f32) -> f32 {
    let n = series.len();
    if n < 3 {
        return 0.0;
    }
    let accel = series[n - 1] - 2.0 * series[n - 2] + series[n - 3];
    if accel.abs() < noise_floor {
        0.0
    } else {
        accel
    }
}

/// The felt direction of a pulse — what the ear reads from the movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeltDirection {
    /// Energy is rising — the room is warming.
    Rising,
    /// Energy is falling — the room is cooling.
    Falling,
    /// Energy is holding — no meaningful movement.
    Flat,
}

impl FeltDirection {
    /// Classify a signed direction signal against the noise floor.
    pub fn from_signal(direction: f32, noise_floor: f32) -> Self {
        if direction > noise_floor {
            FeltDirection::Rising
        } else if direction < -noise_floor {
            FeltDirection::Falling
        } else {
            FeltDirection::Flat
        }
    }
}

/// One macro read of the pulse — the renderer's "trader's board".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Perception {
    /// Number of energy frames seen so far (bounded by history).
    pub n_readings: usize,
    /// Rolling mean energy.
    pub energy: f32,
    /// Rolling loudness in dB (log of the rolling energy).
    pub loudness_db: f32,
    /// Rolling tempo: EMA of the spacing between energy onsets (in frames).
    /// 0.0 until two onsets have been felt.
    pub tempo_frames: f32,
    /// Direction from the last TWO frames.
    pub direction: f32,
    /// Rate of change from the last THREE+ frames.
    pub rate: f32,
    /// The felt direction (rising / falling / flat).
    pub felt: FeltDirection,
}

/// The state of the renderer that the feel may shape.
///
/// `gain` and `velocity` are both linear 0.0–1.0. `shape_output` multiplies
/// them by the felt direction's push — rising pushes up, falling pulls down,
/// flat holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderState {
    /// Master gain (linear 0.0–1.0).
    pub gain: f32,
    /// Note velocity (linear 0.0–1.0).
    pub velocity: f32,
}

impl RenderState {
    pub fn new(gain: f32, velocity: f32) -> Self {
        Self { gain, velocity }
    }
}

/// The elephant's audio-adjacent dials — the room's loudness and warmth.
///
/// `volume` is `elephant/dials/volume.py` (how loud the room is talking,
/// 0 quiet .. 1 shouting); `mood` is `elephant/dials/mood.py` (the room's
/// valence, −1 cold .. +1 warm). Together they are the room's temperature,
/// and [`FeelPulse::from_dials`] turns them into the renderer's pulse input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DialsReadings {
    /// Room loudness, 0.0–1.0.
    pub volume: f32,
    /// Room warmth, −1.0 (cold) .. +1.0 (warm).
    pub mood: f32,
}

impl DialsReadings {
    pub fn new(volume: f32, mood: f32) -> Self {
        Self { volume, mood }
    }
}

/// The renderer's ear: a pulse over the audio stream that feels the room and
/// shapes the output by what it feels.
///
/// Feed it an energy frame per chunk (or an elephant dial reading) with
/// [`FeelPulse::push`]; read the macro sense with [`FeelPulse::perception_check`];
/// and shape the output with [`FeelPulse::shape_output`].
///
/// All state is a fixed-size ring — memory is O(history), never O(duration).
#[derive(Debug, Clone)]
pub struct FeelPulse {
    /// Rolling energy frames (bounded).
    history: VecDeque<f32>,
    /// Max frames held.
    history_cap: usize,
    /// Per-pulse noise floor (moves below this read as 0).
    noise_floor: f32,
    /// Running sum of the history (O(1) rolling mean).
    energy_sum: f32,
    /// Last valid energy (for NaN/inf carry-forward).
    last_valid: f32,
    /// EMA of the spacing between energy onsets, in frames (tempo).
    tempo_frames: f32,
    /// Number of onsets felt.
    onset_count: u64,
    /// Frames since the last onset.
    frames_since_onset: u64,
    /// Was the previous frame above the onset threshold?
    was_above: bool,
    /// Direction from the last two frames.
    direction: f32,
    /// Rate of change from the last three+ frames.
    rate: f32,
    /// The felt direction.
    felt: FeltDirection,
}

impl Default for FeelPulse {
    fn default() -> Self {
        Self::new()
    }
}

impl FeelPulse {
    /// A fresh ear, listening with the default noise floor and history.
    pub fn new() -> Self {
        Self::with_params(DEFAULT_NOISE_FLOOR, DEFAULT_HISTORY)
    }

    /// A fresh ear with an explicit noise floor and history length.
    pub fn with_params(noise_floor: f32, history_cap: usize) -> Self {
        debug_assert!(noise_floor >= 0.0, "noise_floor must be non-negative");
        Self {
            history: VecDeque::with_capacity(history_cap),
            history_cap: history_cap.max(3),
            noise_floor,
            energy_sum: 0.0,
            last_valid: 0.0,
            tempo_frames: 0.0,
            onset_count: 0,
            frames_since_onset: 0,
            was_above: false,
            direction: 0.0,
            rate: 0.0,
            felt: FeltDirection::Flat,
        }
    }

    /// Bridge from the elephant's dials — the room's temperature steering the
    /// renderer.
    ///
    /// `volume` (loudness) is the raw energy; `mood` (warmth) tilts it — a
    /// warm room reads louder than its raw volume, a cold room quieter. The
    /// result is a pulse seeded with that single felt-energy frame.
    pub fn from_dials(readings: DialsReadings) -> Self {
        let mut feel = Self::new();
        feel.push_dials(readings);
        feel
    }

    /// Push one elephant dial reading into the pulse (see [`Self::from_dials`]).
    pub fn push_dials(&mut self, readings: DialsReadings) {
        let volume = clamp01(readings.volume);
        let mood = readings.mood.clamp(-1.0, 1.0);
        // Warmth is energy: a warm room feels louder, a cold room quieter.
        let energy = clamp01(volume + WARMTH_TILT * mood);
        self.push(energy);
    }

    /// Push one energy frame (per chunk / per pulse).
    ///
    /// `energy_frame` is the loudness of that chunk, conventionally normalized
    /// to 0.0–1.0. NaN or infinite frames are carried forward from the last
    /// valid frame — a glitch is NOT a movement, and the number doesn't matter;
    /// only the movement does. This keeps `perception_check` and `shape_output`
    /// always finite.
    pub fn push(&mut self, energy_frame: f32) {
        let energy = if energy_frame.is_finite() {
            energy_frame
        } else {
            self.last_valid
        };
        self.last_valid = energy;

        self.history.push_back(energy);
        self.energy_sum += energy;
        if self.history.len() > self.history_cap {
            if let Some(evicted) = self.history.pop_front() {
                self.energy_sum -= evicted;
            }
        }

        // Tempo: a beat is a rising energy edge above the rolling mean + a
        // margin. Track the EMA of the spacing between beats (in frames).
        self.frames_since_onset += 1;
        let above = energy > self.energy_mean() + ONSET_MARGIN;
        if above && !self.was_above {
            self.onset_count += 1;
            if self.onset_count > 1 {
                let spacing = self.frames_since_onset as f32;
                if spacing >= 1.0 {
                    self.tempo_frames = if self.tempo_frames <= 0.0 {
                        spacing
                    } else {
                        self.tempo_frames + TEMPO_ALPHA * (spacing - self.tempo_frames)
                    };
                }
            }
            self.frames_since_onset = 0;
        }
        self.was_above = above;

        self.refresh();
    }

    /// The macro read of the pulse: direction from the last two frames, rate
    /// from the last three+, plus the rolling energy/loudness/tempo.
    #[must_use]
    pub fn perception_check(&self) -> Perception {
        Perception {
            n_readings: self.history.len(),
            energy: self.energy_mean(),
            loudness_db: self.loudness_db(),
            tempo_frames: self.tempo_frames,
            direction: self.direction,
            rate: self.rate,
            felt: self.felt,
        }
    }

    /// Shape a render state by the felt direction.
    ///
    /// Rising pushes gain and velocity up, falling pulls them down, flat
    /// holds. The push is bounded to ±[`MAX_PUSH`] and always finite.
    #[must_use]
    pub fn shape_output(&self, state: RenderState) -> RenderState {
        let mult = self.gain_multiplier();
        RenderState {
            gain: clamp01(state.gain * mult),
            velocity: clamp01(state.velocity * mult),
        }
    }

    /// The felt gain multiplier: 1.0 is a hold, >1.0 is a push up, <1.0 a
    /// pull down. Bounded to `[1.0 − MAX_PUSH, 1.0 + MAX_PUSH]`.
    #[must_use]
    pub fn gain_multiplier(&self) -> f32 {
        (1.0 + self.felt_push()).clamp(0.0, 1.0 + MAX_PUSH)
    }

    /// The felt push (signed, in `[−MAX_PUSH, +MAX_PUSH]`): direction weighted
    /// against rate of change, clamped.
    fn felt_push(&self) -> f32 {
        let dir = self.direction.clamp(-1.0, 1.0);
        let rate = self.rate.clamp(-1.0, 1.0);
        (dir * DIR_WEIGHT + rate * RATE_WEIGHT).clamp(-MAX_PUSH, MAX_PUSH)
    }

    /// Recompute direction / rate / felt from the rolling history.
    fn refresh(&mut self) {
        let n = self.history.len();
        self.direction = if n >= 2 {
            let delta = self.history[n - 1] - self.history[n - 2];
            if delta.abs() < self.noise_floor {
                0.0
            } else {
                delta
            }
        } else {
            0.0
        };
        self.rate = if n >= 3 {
            let accel =
                self.history[n - 1] - 2.0 * self.history[n - 2] + self.history[n - 3];
            if accel.abs() < self.noise_floor {
                0.0
            } else {
                accel
            }
        } else {
            0.0
        };
        self.felt = FeltDirection::from_signal(self.direction, self.noise_floor);
    }

    /// Rolling mean energy (0.0 when nothing has been heard yet).
    pub fn energy_mean(&self) -> f32 {
        let n = self.history.len();
        if n == 0 {
            0.0
        } else {
            self.energy_sum / n as f32
        }
    }

    /// Rolling loudness in dB (log of the rolling energy).
    pub fn loudness_db(&self) -> f32 {
        20.0 * (self.energy_mean() + EPS).log10()
    }

    /// Rolling tempo in beats-per-minute, given how many energy frames arrive
    /// per second. Returns 0.0 until two onsets have been felt.
    pub fn tempo_bpm(&self, frames_per_second: f32) -> f32 {
        if self.tempo_frames <= 0.0 || !frames_per_second.is_finite() || frames_per_second <= 0.0
        {
            return 0.0;
        }
        frames_per_second * 60.0 / self.tempo_frames
    }

    /// Number of energy frames seen so far.
    pub fn n_readings(&self) -> usize {
        self.history.len()
    }

    /// The most recent energy frame (0.0 if none yet).
    pub fn last_energy(&self) -> f32 {
        self.history.back().copied().unwrap_or(0.0)
    }

    /// The felt direction (rising / falling / flat).
    pub fn felt(&self) -> FeltDirection {
        self.felt
    }
}

#[inline]
fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: a fresh feel fed a series of energy frames.
    fn feel_from(series: &[f32]) -> FeelPulse {
        let mut f = FeelPulse::new();
        for &e in series {
            f.push(e);
        }
        f
    }

    #[test]
    fn rising_energy_shapes_output_up() {
        let f = feel_from(&[0.1, 0.2, 0.3]);
        assert_eq!(f.felt(), FeltDirection::Rising);
        let shaped = f.shape_output(RenderState::new(0.7, 0.5));
        assert!(shaped.gain > 0.7, "rising should push gain up");
        assert!(shaped.velocity > 0.5, "rising should push velocity up");
        assert!(f.gain_multiplier() > 1.0);
    }

    #[test]
    fn falling_energy_shapes_output_down() {
        let f = feel_from(&[0.3, 0.2, 0.1]);
        assert_eq!(f.felt(), FeltDirection::Falling);
        let shaped = f.shape_output(RenderState::new(0.7, 0.5));
        assert!(shaped.gain < 0.7, "falling should pull gain down");
        assert!(shaped.velocity < 0.5, "falling should pull velocity down");
        assert!(f.gain_multiplier() < 1.0);
    }

    #[test]
    fn flat_energy_holds_output() {
        let f = feel_from(&[0.2, 0.2, 0.2]);
        assert_eq!(f.felt(), FeltDirection::Flat);
        let shaped = f.shape_output(RenderState::new(0.7, 0.5));
        approx::assert_relative_eq!(shaped.gain, 0.7, epsilon = 1e-6);
        approx::assert_relative_eq!(shaped.velocity, 0.5, epsilon = 1e-6);
        approx::assert_relative_eq!(f.gain_multiplier(), 1.0, epsilon = 1e-6);
    }

    #[test]
    fn acceleration_adds_to_push() {
        // Constant-speed rise vs accelerating rise: the accelerating one
        // pushes harder (rate of change adds to the felt push).
        let steady = feel_from(&[0.10, 0.20, 0.30]);
        let accel = feel_from(&[0.10, 0.20, 0.40]);
        assert!(accel.gain_multiplier() > steady.gain_multiplier());
        assert_eq!(steady.rate, 0.0, "steady rise has zero rate of change");
        assert!(accel.rate > 0.0, "accelerating rise has positive rate");
    }

    #[test]
    fn nan_is_carried_forward_not_a_movement() {
        let f = feel_from(&[0.2, 0.3, f32::NAN, 0.3]);
        // The NaN glitch is carried forward: 0.3 → 0.3 → 0.3, so flat.
        assert_eq!(f.felt(), FeltDirection::Flat);
        let shaped = f.shape_output(RenderState::new(0.7, 0.5));
        assert!(shaped.gain.is_finite());
        assert!(shaped.velocity.is_finite());
        approx::assert_relative_eq!(f.gain_multiplier(), 1.0, epsilon = 1e-6);
    }

    #[test]
    fn inf_is_carried_forward_not_a_movement() {
        let f = feel_from(&[0.2, f32::INFINITY, 0.2]);
        // inf carried forward → 0.2 → 0.2 → 0.2, flat.
        assert_eq!(f.felt(), FeltDirection::Flat);
        assert!(f.gain_multiplier().is_finite());
    }

    #[test]
    fn direction_needs_two_readings() {
        assert_eq!(direction(&[0.5], DEFAULT_NOISE_FLOOR), 0.0);
        approx::assert_relative_eq!(direction(&[0.5, 0.7], DEFAULT_NOISE_FLOOR), 0.2);
        // Below the noise floor → 0.
        assert_eq!(direction(&[0.5, 0.51], DEFAULT_NOISE_FLOOR), 0.0);
    }

    #[test]
    fn rate_needs_three_readings() {
        assert_eq!(rate_of_change(&[0.5, 0.7], DEFAULT_NOISE_FLOOR), 0.0);
        // Constant speed → zero acceleration.
        assert_eq!(rate_of_change(&[0.1, 0.2, 0.3], DEFAULT_NOISE_FLOOR), 0.0);
        // Accelerating → positive second difference.
        approx::assert_relative_eq!(
            rate_of_change(&[0.1, 0.2, 0.4], DEFAULT_NOISE_FLOOR),
            0.1,
            epsilon = 1e-6
        );
    }

    #[test]
    fn history_is_bounded() {
        let mut f = FeelPulse::with_params(DEFAULT_NOISE_FLOOR, 8);
        for i in 0..1000 {
            f.push((i % 10) as f32 / 10.0);
        }
        assert!(f.n_readings() <= 8, "history must stay bounded");
    }

    #[test]
    fn dials_bridge_works() {
        // A warm room reads louder than its raw volume.
        let warm = FeelPulse::from_dials(DialsReadings::new(0.8, 0.5));
        // A cold room reads quieter than its raw volume.
        let cold = FeelPulse::from_dials(DialsReadings::new(0.8, -1.0));
        assert!(warm.last_energy() > 0.8, "warmth tilts energy up");
        assert!(cold.last_energy() < 0.8, "cold tilts energy down");
        assert!(warm.last_energy() > cold.last_energy());
        // The dials seed a valid, finite pulse.
        assert!(warm.last_energy().is_finite());
        assert!(cold.last_energy().is_finite());
    }

    #[test]
    fn dials_feed_rises_shapes_up() {
        // Feeding rising room loudness through the dials bridge shapes up.
        let mut f = FeelPulse::from_dials(DialsReadings::new(0.3, 0.0));
        f.push_dials(DialsReadings::new(0.6, 0.0));
        f.push_dials(DialsReadings::new(0.9, 0.0));
        assert_eq!(f.felt(), FeltDirection::Rising);
        assert!(f.gain_multiplier() > 1.0);
    }

    #[test]
    fn loudness_db_is_finite_for_silence() {
        let f = feel_from(&[0.0, 0.0, 0.0]);
        assert!(f.loudness_db().is_finite());
        assert!(f.loudness_db() < 0.0);
    }

    #[test]
    fn tempo_is_zero_for_flat_stream() {
        // A flat stream has no rising onsets → no tempo yet.
        let f = feel_from(&[0.2; 32]);
        assert_eq!(f.tempo_bpm(1.0), 0.0);
    }

    #[test]
    fn tempo_detects_onsets() {
        // A stream with clear rising onsets should feel a tempo.
        let mut f = FeelPulse::new();
        for i in 0..32 {
            let phase = i % 4;
            let e = if phase == 0 { 0.9 } else { 0.05 };
            f.push(e);
        }
        assert!(f.onset_count > 1, "should detect multiple onsets");
        assert!(f.tempo_bpm(1.0) > 0.0);
    }
}
