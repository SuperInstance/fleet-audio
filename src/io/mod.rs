//! I/O modules: MIDI input (JSONL spool, HTTP), streaming WAV output.

pub mod jsonl_spool;
pub mod http_server;
pub mod dials_poller;
#[cfg(feature = "live")]
pub mod cpal_out;
