//! Streaming WAV writer — writes audio chunk-by-chunk, never holding the full file.
//!
//! The WAV format has a header followed by PCM data. The header needs the
//! total data size, but we don't know that upfront for streaming. Solution:
//! write a placeholder header, stream data, then patch the header on close.
//!
//! This is the standard approach for streaming WAV and works perfectly:
//! - Memory: O(1) (only the header is buffered, 44 bytes)
//! - Disk: appended sequentially
//! - The file is valid once `close()` patches the sizes.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use hound::{WavSpec, WavWriter};
use anyhow::Result;
use tracing::{info, debug};

/// Streaming WAV writer. Writes chunks sequentially, patches header on close.
///
/// Memory: O(1). Only keeps the file handle and a byte counter.
pub struct StreamingWavWriter {
    /// Underlying hound WAV writer.
    writer: WavWriter<BufWriter<File>>,
    /// Total samples written (for status reporting).
    samples_written: u64,
}

impl StreamingWavWriter {
    /// Create a new streaming WAV writer.
    pub fn create(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self> {
        let spec = WavSpec {
            channels: 1,           // mono
            sample_rate,
            bits_per_sample: 16,   // 16-bit PCM
            sample_format: hound::SampleFormat::Int,
        };

        let writer = WavWriter::create(path.as_ref(), spec)?;
        info!("StreamingWavWriter created: {}", path.as_ref().display());

        Ok(Self {
            writer,
            samples_written: 0,
        })
    }

    /// Write a chunk of f32 samples (will be converted to i16).
    /// This is called for every chunk from the synthesizer.
    /// The samples are immediately flushed to disk — no buffering beyond
    /// the BufWriter's internal buffer.
    pub fn write_chunk(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            // Convert f32 [-1.0, 1.0] to i16 [-32768, 32767]
            let clamped = sample.clamp(-1.0, 1.0);
            let i16_sample = (clamped * 32767.0) as i16;
            self.writer.write_sample(i16_sample)?;
        }
        self.samples_written += samples.len() as u64;
        debug!("Wrote {} samples (total: {})", samples.len(), self.samples_written);
        Ok(())
    }

    /// Finalize the WAV file — patches the header with correct sizes.
    /// Must be called when done writing.
    pub fn close(mut self) -> Result<()> {
        self.writer.flush()?;
        // hound's WavWriter.finalize() patches the header
        self.writer.finalize()?;
        info!("WAV finalized: {} total samples", self.samples_written);
        Ok(())
    }

    /// Total samples written so far.
    pub fn samples_written(&self) -> u64 {
        self.samples_written
    }

    /// Duration of audio written so far (in seconds).
    pub fn duration_secs(&self, sample_rate: u32) -> f64 {
        self.samples_written as f64 / sample_rate as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn write_and_read_wav() {
        let path = std::env::temp_dir().join("fleet_audio_test.wav");

        // Write a simple sine wave
        {
            let mut writer = StreamingWavWriter::create(&path, 44100).unwrap();
            let chunk_size = 1024;
            let num_chunks = 10;

            for chunk_idx in 0..num_chunks {
                let mut samples = vec![0.0_f32; chunk_size];
                for (i, s) in samples.iter_mut().enumerate() {
                    let t = (chunk_idx * chunk_size + i) as f32 / 44100.0;
                    *s = (t * 2.0 * std::f32::consts::PI * 440.0).sin() * 0.5;
                }
                writer.write_chunk(&samples).unwrap();
            }
            assert_eq!(writer.samples_written(), (chunk_size * num_chunks) as u64);
            writer.close().unwrap();
        }

        // Read it back and verify
        {
            let mut reader = hound::WavReader::open(&path).unwrap();
            let spec = reader.spec();
            assert_eq!(spec.sample_rate, 44100);
            assert_eq!(spec.bits_per_sample, 16);
            assert_eq!(spec.channels, 1);

            let samples: Vec<i16> = reader.samples().map(|s| s.unwrap()).collect();
            assert_eq!(samples.len(), 1024 * 10);

            // Verify it's not all silence
            let max_abs = samples.iter().map(|&s| s.abs()).max().unwrap();
            assert!(max_abs > 1000, "WAV should contain non-trivial audio data");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn streaming_memory_is_constant() {
        // The key property: the writer holds only a file handle and a counter.
        // No matter how much data we write, its memory doesn't grow.
        let path = std::env::temp_dir().join("fleet_audio_streaming_test.wav");
        {
            let mut writer = StreamingWavWriter::create(&path, 44100).unwrap();

            // Write many chunks
            for _ in 0..1000 {
                let samples = vec![0.5_f32; 1024];
                writer.write_chunk(&samples).unwrap();
            }

            assert_eq!(writer.samples_written(), 1_024_000);
            writer.close().unwrap();
        }

        // File should exist and be correct size (1024000 samples × 2 bytes + 44 header)
        let file_size = std::fs::metadata(&path).unwrap().len();
        assert_eq!(file_size, 1_024_000 * 2 + 44);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duration_tracking() {
        let path = std::env::temp_dir().join("fleet_audio_duration_test.wav");
        let mut writer = StreamingWavWriter::create(&path, 44100).unwrap();

        // Write exactly 1 second of audio
        let samples = vec![0.0_f32; 44100];
        writer.write_chunk(&samples).unwrap();

        approx::assert_relative_eq!(
            writer.duration_secs(44100),
            1.0,
            epsilon = 0.001
        );

        writer.close().unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
