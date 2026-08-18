//! fleet-audio — Streaming MIDI → audio renderer.
//!
//! Entry point: spawns MIDI input source(s), runs the synthesizer loop,
//! and writes audio to the streaming WAV output.
//!
//! ## Usage
//!
//! ```sh
//! # Render from JSONL spool to WAV
//! fleet-audio --spool /var/lib/fleet-audio/spool --output out.wav
//!
//! # Listen for CNS bus MIDI events via HTTP
//! fleet-audio --http-port 3900 --output out.wav
//!
//! # Both inputs
//! fleet-audio --spool /var/lib/fleet-audio/spool --http-port 3900 --output out.wav
//! ```

use std::sync::Arc;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use fleet_audio::{
    config::{Config, SAMPLE_RATE},
    feel::{DialsReadings, FeelPulse},
    midi::MidiEvent,
    ring::EventRing,
    synth::Synthesizer,
    wav::StreamingWavWriter,
    io::dials_poller,
    io::jsonl_spool::JsonlSpoolReader,
};

#[derive(Parser, Debug)]
#[command(name = "fleet-audio", version, about = "Streaming MIDI → audio renderer")]
struct Args {
    /// JSONL spool directory for MIDI input
    #[arg(long)]
    spool: Option<String>,

    /// HTTP port for MIDI input (CNS bus)
    #[arg(long)]
    http_port: Option<u16>,

    /// Output WAV file path
    #[arg(long)]
    output: Option<String>,

    /// Play live to the default output device (requires --features live)
    #[arg(long, default_value_t = false)]
    live: bool,

    /// Sample rate (default: 44100)
    #[arg(long, default_value = "44100")]
    sample_rate: u32,

    /// Chunk size (default: 1024)
    #[arg(long, default_value_t = 1024)]
    chunk_size: usize,

    /// Maximum polyphony (default: 64)
    #[arg(long, default_value_t = 64)]
    max_voices: usize,

    /// Master gain 0.0–1.0 (default: 0.7)
    #[arg(long, default_value_t = 0.7)]
    gain: f32,

    /// Elephant field endpoint to poll for dials readings, e.g.
    /// http://127.0.0.1:4073/field — feeds FeelPulse live from the room.
    #[arg(long)]
    dials_endpoint: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let args = Args::parse();

    tracing::info!("fleet-audio starting");
    tracing::info!("  sample_rate: {}", args.sample_rate);
    tracing::info!("  chunk_size: {} (~{:.1}ms)", args.chunk_size,
        args.chunk_size as f32 / args.sample_rate as f32 * 1000.0);
    tracing::info!("  max_voices: {}", args.max_voices);

    // Build configuration
    let config = Config {
        sample_rate: args.sample_rate,
        chunk_size: args.chunk_size,
        max_voices: args.max_voices,
        master_gain: args.gain,
        output_wav: args.output.clone(),
        jsonl_spool: args.spool.clone(),
        http_port: args.http_port,
        dials_endpoint: args.dials_endpoint.clone(),
        ..Default::default()
    };

    // Create the lock-free ring buffer — shared between input and synth
    let ring = Arc::new(EventRing::new(1024));

    // Spawn JSONL spool reader if configured
    if let Some(spool_dir) = &args.spool {
        // Ensure directory exists
        let _ = tokio::fs::create_dir_all(spool_dir).await;
        let reader = JsonlSpoolReader::new(spool_dir.clone(), ring.clone());
        tokio::spawn(async move {
            if let Err(e) = reader.run().await {
                tracing::error!("JSONL spool reader error: {e}");
            }
        });
        tracing::info!("JSONL spool reader started: {spool_dir}");
    }

    // Spawn HTTP server if configured
    if let Some(port) = args.http_port {
        let ring_clone = ring.clone();
        let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
        tokio::spawn(async move {
            if let Err(e) = fleet_audio::io::http_server::run_http_server(addr, ring_clone).await {
                tracing::error!("HTTP server error: {e}");
            }
        });
        tracing::info!("HTTP MIDI server started on port {port}");
    }

    // Spawn the dials endpoint poller if configured — the elephant's field
    // feeds the render loop's FeelPulse live via a bounded channel.
    let dials_rx = if let Some(url) = &args.dials_endpoint {
        let (tx, rx) = crossbeam::channel::bounded::<DialsReadings>(16);
        let url = url.clone();
        tokio::spawn(dials_poller::run_dials_poller(
            url.clone(),
            tx,
            dials_poller::DEFAULT_POLL_INTERVAL,
        ));
        tracing::info!("Dials endpoint poller started: {url}");
        Some(rx)
    } else {
        None
    };

    // Open WAV output if configured
    let wav_writer = if let Some(output_path) = &args.output {
        Some(StreamingWavWriter::create(output_path, args.sample_rate)?)
    } else {
        None
    };

    // Open the live sink if requested (feature-gated). A headless box
    // falls back to WAV-only with a warning — not fatal (plan §3.5).
    #[cfg(feature = "live")]
    let live_sink = if args.live {
        match fleet_audio::io::cpal_out::LiveSink::open(args.sample_rate) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("live output unavailable ({e}); continuing WAV-only");
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "live"))]
    if args.live {
        anyhow::bail!("--live requires building with --features live");
    }

    // Run the synthesis loop
    run_synth_loop(config, ring, wav_writer, dials_rx).await?;

    Ok(())
}

/// The main synthesis loop.
///
/// This runs the audio thread: for each chunk, drain events → synthesize → write.
/// Memory is O(chunk_size) — constant regardless of how long the loop runs.
async fn run_synth_loop(
    config: Config,
    ring: Arc<EventRing>,
    mut wav_writer: Option<StreamingWavWriter>,
    dials_rx: Option<crossbeam::channel::Receiver<DialsReadings>>,
) -> anyhow::Result<()> {
    let mut synth = Synthesizer::new(&config);
    let chunk_us = (config.chunk_size as f64 / config.sample_rate as f64 * 1_000_000.0) as u64;

    // Dials endpoint configured → the renderer starts listening to the room.
    if dials_rx.is_some() {
        synth.enable_feel(FeelPulse::new());
    }

    tracing::info!("Synthesis loop started (chunk_us={chunk_us})");

    // For production: this loop runs forever.
    // For testing / finite runs: we'd add a shutdown signal.
    loop {
        // Drain any dials readings that arrived since the last chunk — O(1),
        // no allocation. The poller runs off-thread; this loop only ever
        // does a non-blocking try_recv.
        if let Some(rx) = &dials_rx {
            while let Ok(readings) = rx.try_recv() {
                synth.feed_dials(readings);
            }
        }

        // Process one chunk
        let output = synth.process_chunk(&ring);

        // Write to WAV if configured
        if let Some(ref mut writer) = wav_writer {
            writer.write_chunk(output)?;
        }

        // Sleep for one chunk duration to maintain real-time.
        // In a real audio system, this would be driven by the audio interface
        // (e.g., cpal callback). For file rendering, we could go faster,
        // but sleeping keeps us real-time safe and prevents CPU spinning.
        tokio::time::sleep(std::time::Duration::from_micros(chunk_us)).await;

        // Periodic status logging
        if synth.current_time_us() % 10_000_000 == 0 && synth.current_time_us() > 0 {
            tracing::info!(
                "Status: {:.1}s elapsed, {} active voices",
                synth.current_time_us() as f64 / 1_000_000.0,
                synth.active_voice_count()
            );
        }
    }
}
