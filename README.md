# fleet-audio

> Streaming MIDI → audio renderer with O(1) memory. Replaces the numpy OOM killer.

**Phase 4** of the fleet infrastructure. Takes MIDI events from the CNS bus (JSONL spool or HTTP) and renders them to WAV in fixed-memory chunks. No allocation on the audio thread. No OOM. No O(duration) memory.

## Architecture

```text
  MIDI Input → Lock-free SPSC Ring → Audio Thread → WAV Writer
  (JSONL/HTTP)   (MidiEvent)         (Synthesizer)   (Streaming)
```

- **Chunk size:** 1024 samples (~23ms at 44100Hz)
- **Max buffered audio:** 1 second
- **No allocation in the audio thread**
- **Lock-free ring** between MIDI input and synthesis

## Voices

| Channel | Voice | Engine |
|---------|-------|--------|
| 0 | Piano | Additive synthesis with harmonic partials |
| 1 | Bass | Subtractive with lowpass filter |
| 2 | Strings | Slow-attack pad with vibrato |
| 3 | Guitar | Plucked-string model (Karplus-Strong derivative) |
| 9 | Drums | Noise + pitched oscillators (kick, snare, hat, crash) |

## Quick Start

```rust
use fleet_audio::{Config, EventRing, MidiEvent, Synthesizer};

let ring = EventRing::new(1024);
let mut synth = Synthesizer::with_defaults();

// Feed MIDI events
ring.push(MidiEvent::note_on(0, 60, 100, 0)); // middle C

// Render a chunk
let output: &[f32] = synth.process_chunk(&ring);
// output is CHUNK_SIZE samples, ready to write or play
```

## Configuration

```toml
[fleet-audio]
sample_rate = 44100
chunk_size = 1024
max_voices = 64
ring_capacity = 1024
master_gain = 0.7
output_wav = "/var/spool/fleet-audio/output.wav"
jsonl_spool = "/var/spool/fleet-audio/midi-in"
http_port = 3007
```

## Testing

```sh
cargo test
```

47 tests covering: ring buffer (push/pop/drain/wraparound), MIDI event handling, synthesizer polyphony, voice rendering (frequency correctness, envelope behavior, note-off silencing), WAV streaming, and memory boundedness.

## Design Decisions

- **Lock-free SPSC ring:** Audio thread has hard real-time deadlines (~23ms). Any lock would cause dropouts.
- **Power-of-two capacity:** Mask-based indexing (`idx & mask`) instead of modulo — faster on every architecture.
- **Fixed voice pool:** `MAX_VOICES` slots allocated at startup. No allocation on the hot path.
- **Additive synthesis:** Each voice sums harmonic partials. Rich tone, predictable cost.
- **Streaming WAV:** Samples written and discarded per chunk. Memory is O(chunk), never O(duration).

## Part of the Fleet

fleet-audio is the audio rendering backend for:
- **fleet-ensemble** — musical coordination and CNS bus MIDI streaming
- **fleet-cns** — Collective Nervous System event bus
- **tapscript-worker** — MIDI generation from musical theory
- **songforge** — AI-assisted songwriting pipeline

## License

MIT
