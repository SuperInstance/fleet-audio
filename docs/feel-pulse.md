# The Feel Pulse — the renderer plays with the room, not just in it

fleet-audio renders sound. The fleet's JEPA ear (`fleet-jepa-midi/vibe_matcher.py`
and `elephant/pulse.py`) *feels* sound. The maturation that this module ships is
the marriage of the two: the renderer gains a **FEEL module** — a pulse over the
audio stream that listens to its own energy, reads the direction and rate of
change the way a trader reads a currency pair, and shapes its own output by what
it feels.

A rising pulse pushes the gain and velocity up. A falling pulse pulls them down.
A flat pulse holds. The renderer doesn't just play — it feels the room's pulse
and plays *with* it.

## The chain

```
 audio frames → energy → FeelPulse (rolling energy / loudness / tempo)
              → perception check (direction / rate of change)
              → shape_output (rising ↑, falling ↓, flat →)
```

Three concepts, cross-pollinated from two older fleet repos:

1. **LISTEN** — from `vibe_matcher.py`: the acoustic ear reduces the audio
   stream to an *energy frame*. Here it is distilled to its essence — energy,
   loudness (dB of the rolling energy), and tempo (the EMA of the spacing
   between energy onsets) as bounded rolling moving averages.
2. **FEEL** — from `elephant/pulse.py`: the perception-check math. One number
   is nothing; **two numbers show direction**; **more than two show rate of
   change**. Direction is the signed movement between the last two energy
   frames; rate is the second difference over the last three (the exact
   acceleration of the quadratic interpolant through them).
3. **SHAPE** — the output modifier: the felt direction is turned into a bounded
   push (a ±25% swing) applied to gain and velocity.

## The `FeelPulse` API

```rust
use fleet_audio::{FeelPulse, RenderState, DialsReadings};

let mut ear = FeelPulse::new();

// LISTEN: feed an energy frame per chunk (0.0–1.0 normalized loudness).
ear.push(0.1);
ear.push(0.2);
ear.push(0.3);

// FEEL: the macro read — direction from the last two, rate from the last three.
let p = ear.perception_check();
assert_eq!(p.felt, FeltDirection::Rising);

// SHAPE: a rising pulse pushes gain/velocity up.
let shaped = ear.shape_output(RenderState::new(0.7, 0.5));
assert!(shaped.gain > 0.7);
```

- `push(energy_frame)` — rolling energy / loudness / tempo, NaN- and inf-guarded
  (a glitch is carried forward; it is *not* a movement, so a bad sample never
  fabricates direction or rate).
- `perception_check()` — returns a `Perception`: `energy`, `loudness_db`,
  `tempo_frames`, `direction` (last two), `rate` (last three+), and the
  classified `felt` direction (`Rising` / `Falling` / `Flat`).
- `shape_output(RenderState)` — multiplies `gain` and `velocity` by the felt
  push, bounded to `[1 − MAX_PUSH, 1 + MAX_PUSH]` (a ±25% swing), always finite.
- `gain_multiplier()` — the felt multiplier alone: `>1.0` push up, `<1.0` pull
  down, `1.0` hold.
- `from_dials(DialsReadings)` / `push_dials(...)` — the elephant bridge (below).

Memory is O(history) — a fixed 64-frame ring, never O(duration).

## The elephant bridge — the room's temperature steers the renderer

`elephant/dials/volume.py` (the room's loudness, 0 quiet .. 1 shouting) and
`elephant/dials/mood.py` (the room's warmth, −1 cold .. +1 warm) are the room's
temperature. `from_dials` turns them into the renderer's pulse input:

```rust
// The room is loud (0.8) and warm (+0.5) → it *feels* louder than it is.
let ear = FeelPulse::from_dials(DialsReadings::new(0.8, 0.5));
// volume + WARMTH_TILT * mood = 0.8 + 0.2 * 0.5 = 0.9 felt energy.
assert!(ear.last_energy() > 0.8);

// The same loudness, but a cold room (−1.0) → it *feels* quieter.
let ear = FeelPulse::from_dials(DialsReadings::new(0.8, -1.0));
assert!(ear.last_energy() < 0.8);
```

Warmth is energy: a warm room reads louder than its raw volume, a cold room
quieter. Feed the dials every pulse and the renderer's ear tracks the room's
temperature, shaping the output as the room warms and cools.

## Wiring into the synthesizer

The `Synthesizer` carries an optional `FeelPulse`. When enabled
(`enable_feel`), `process_chunk` listens to its own mixed energy (the RMS of
the chunk, soft-normalized), feeds it to the ear, and shapes the master gain by
the felt direction before soft clipping. Disabled (the default), the renderer
behaves exactly as before — a fixed master gain, no ear.

```rust
let mut synth = Synthesizer::with_defaults();
synth.enable_feel(FeelPulse::new());
// … now each process_chunk(&ring) feels its own energy and shapes the gain.
```

There is also `feed_energy(f32)` to push an *external* energy frame (or a dial
reading) into the ear when you want the renderer to feel something other than
its own output.

## Why this matters

The old renderer was a transducer — MIDI in, audio out, no memory of itself. The
feel pulse gives it a sensorimotor loop: it perceives (energy → direction →
rate) and acts (shape gain/velocity). The number alone is nothing; the movement
is the perception. That is the fleet's JEPA ear, matured from a hand-crafted
analysis script into a live, bounded, NaN-guarded renderer module — and the
elephant's pulse is now the renderer's heartbeat.
