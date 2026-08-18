//! Live audio output — the score stops being a file and becomes a sound
//! in the room (plan §3.5). Behind the `live` feature gate; headless CI
//! stays green without it.

#![cfg(feature = "live")]

use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const MAX_BUFFERED: usize = 48_000; // ~1s at 48k — bounded by law

static LIVE_BUFFER: OnceLock<Mutex<VecDeque<f32>>> = OnceLock::new();

fn live_buffer() -> &'static Mutex<VecDeque<f32>> {
    LIVE_BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_BUFFERED)))
}

pub struct LiveSink {
    _stream: cpal::Stream,
}

impl LiveSink {
    /// Open the default output device at `sample_rate`, mono.
    ///
    /// Returns Err on headless boxes (no device) — callers treat that as
    /// "fall back to WAV," not fatal.
    pub fn open(sample_rate: u32) -> Result<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no audio output device (headless?)")?;
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = device
            .build_output_stream(
                &config,
                {
                    let buf = live_buffer();
                    move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let mut q = buf.lock().unwrap();
                        for frame in out.iter_mut() {
                            *frame = q.pop_front().unwrap_or(0.0);
                        }
                    }
                },
                |err| tracing::warn!("live sink error: {err}"),
                None,
            )
            .context("build_output_stream failed")?;
        stream.play().context("stream.play failed")?;
        Ok(Self { _stream: stream })
    }

    /// Push one chunk into the device buffer (drops the oldest if the
    /// device falls behind — a live room does not catch up, it moves on).
    pub fn write_chunk(&self, chunk: &[f32]) {
        let mut q = live_buffer().lock().unwrap();
        for &s in chunk {
            if q.len() >= MAX_BUFFERED {
                q.pop_front();
            }
            q.push_back(s);
        }
    }

    /// True when a device buffer has underrun to empty (useful in tests
    /// with a null sink to assert events flowed).
    pub fn is_idle(&self) -> bool {
        live_buffer().lock().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sink_opens_or_reports_headless() {
        // On a headless CI box this returns Err (no device) — both paths
        // are success for this test; we only assert no panic and that a
        // device, if present, starts idle.
        match super::LiveSink::open(44_100) {
            Ok(sink) => assert!(sink.is_idle()),
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("device") || msg.contains("audio") || msg.contains("ALSA"),
                        "unexpected error: {msg}");
            }
        }
    }
}
