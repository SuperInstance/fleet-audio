//! JSONL spool reader — watches a directory for MIDI event files.
//!
//! Compatible with fleet-ensemble's CNS bus output format.
//! Each line is a JSON-serialized `MidiEvent`.
//!
//! Files are consumed (deleted) after reading.

use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::time::{sleep, Duration};
use tracing::{debug, warn};

use crate::midi::MidiEvent;
use crate::ring::EventRing;

/// Watches a directory for `.jsonl` files and feeds events into the ring.
pub struct JsonlSpoolReader {
    /// Directory to watch.
    spool_dir: PathBuf,
    /// Ring buffer to push events into.
    ring: std::sync::Arc<EventRing>,
}

impl JsonlSpoolReader {
    pub fn new(spool_dir: impl Into<PathBuf>, ring: std::sync::Arc<EventRing>) -> Self {
        Self {
            spool_dir: spool_dir.into(),
            ring,
        }
    }

    /// Run the spool reader loop. Polls the directory every 10ms.
    pub async fn run(&self) -> anyhow::Result<()> {
        tracing::info!("JSONL spool reader watching: {}", self.spool_dir.display());

        loop {
            if let Err(e) = self.poll_once().await {
                warn!("Spool poll error: {e}");
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    /// Poll the directory once, process all available files.
    async fn poll_once(&self) -> anyhow::Result<()> {
        let mut entries = fs::read_dir(&self.spool_dir).await?;
        let mut files = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }

        // Sort for deterministic ordering
        files.sort();

        for file_path in files {
            self.process_file(&file_path).await?;
        }

        Ok(())
    }

    /// Read and process one JSONL file.
    async fn process_file(&self, path: &Path) -> anyhow::Result<()> {
        let content = fs::read_to_string(path).await?;
        let mut count = 0;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<MidiEvent>(line) {
                Ok(event) => {
                    self.ring.push(event);
                    count += 1;
                }
                Err(e) => {
                    warn!("Failed to parse MIDI event from {line}: {e}");
                }
            }
        }

        if count > 0 {
            debug!("Processed {count} events from {}", path.display());
        }

        // Delete the file after processing
        if let Err(e) = fs::remove_file(path).await {
            warn!("Failed to delete spool file {}: {e}", path.display());
        }

        Ok(())
    }
}

/// CNS packet format from fleet-ensemble — we extract MIDI data from it.
/// This allows us to read CNS bus dumps directly.
#[derive(Debug, serde::Deserialize)]
pub struct CnsMidiMessage {
    /// Matches the CnsPacket::AgentPlayed variant.
    pub timestamp_us: u64,
    pub pitch: u8,
    pub velocity: u8,
    #[serde(default)]
    pub channel: u8,
}

/// Parse a CNS bus JSON line into a MidiEvent.
/// Handles both direct MidiEvent JSON and CnsPacket-style messages.
pub fn parse_midi_line(line: &str) -> Option<MidiEvent> {
    // Try direct MidiEvent first
    if let Ok(event) = serde_json::from_str::<MidiEvent>(line) {
        return Some(event);
    }

    // Try CNS AgentPlayed format
    if let Ok(msg) = serde_json::from_str::<CnsMidiMessage>(line) {
        return Some(MidiEvent {
            timestamp_us: msg.timestamp_us,
            channel: msg.channel,
            note: msg.pitch,
            velocity: msg.velocity,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_midi_event() {
        let json = r#"{"timestamp_us":1000,"channel":0,"note":60,"velocity":100}"#;
        let event = parse_midi_line(json).unwrap();
        assert_eq!(event.note, 60);
        assert_eq!(event.channel, 0);
        assert!(event.is_note_on());
    }

    #[test]
    fn parse_cns_agent_played() {
        let json = r#"{"timestamp_us":2000,"pitch":64,"velocity":80,"channel":2}"#;
        let event = parse_midi_line(json).unwrap();
        assert_eq!(event.note, 64);
        assert_eq!(event.velocity, 80);
        assert_eq!(event.channel, 2);
    }

    #[test]
    fn parse_invalid_line() {
        assert!(parse_midi_line("not json").is_none());
        assert!(parse_midi_line("").is_none());
    }

    #[tokio::test]
    async fn spool_reader_processes_files() {
        // Create temp directory with a test file
        let temp = tempfile_dir();
        let file_path = temp.join("test.jsonl");
        let events = vec![
            r#"{"timestamp_us":0,"channel":0,"note":60,"velocity":100}"#,
            r#"{"timestamp_us":1000,"channel":0,"note":60,"velocity":0}"#,
        ];
        std::fs::write(&file_path, events.join("\n")).unwrap();

        let ring = std::sync::Arc::new(EventRing::new(64));
        let reader = JsonlSpoolReader::new(&temp, ring.clone());

        // Poll once
        reader.poll_once().await.unwrap();

        // Should have pushed 2 events
        assert_eq!(ring.len(), 2);

        // File should be deleted
        assert!(!file_path.exists(), "Spool file should be deleted after processing");
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fleet-audio-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}
