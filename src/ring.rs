//! Lock-free single-producer single-consumer ring buffer for MIDI events.
//!
//! This is the bridge between the MIDI input thread (producer) and the
//! audio synthesis thread (consumer). It must be lock-free because:
//!
//! 1. The audio thread has hard real-time deadlines (~23ms per chunk).
//! 2. Any lock/blocking would cause audio dropouts.
//! 3. MIDI input can burst (many events in one message).
//!
//! Implementation: bounded SPSC queue using atomic head/tail indices
//! with power-of-two masking. No allocation on the hot path.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::cell::UnsafeCell;
use crate::midi::MidiEvent;

/// A slot in the ring buffer can hold one MIDI event.
/// Using `Option<MidiEvent>` so empty slots are distinguishable.
const EMPTY: usize = usize::MAX;

/// Lock-free SPSC ring buffer for MIDI events.
///
/// Capacity must be a power of two. Uses atomic indices for head and tail.
/// The producer writes to `tail`, the consumer reads from `head`.
///
/// Memory layout:
/// ```text
/// [slot_0][slot_1][slot_2]...[slot_N-1]
///    ↑ head (consumer reads here)
///                      ↑ tail (producer writes here)
/// ```
pub struct EventRing {
    /// Storage buffer — initialized once, never resized.
    buffer: Box<[UnsafeCell<Option<MidiEvent>>]>,
    /// Mask = capacity - 1 (for fast modulo via bitwise AND).
    mask: usize,
    /// Consumer index (reads). Only modified by consumer thread.
    head: AtomicUsize,
    /// Producer index (writes). Only modified by producer thread.
    tail: AtomicUsize,
}

// SAFETY: The ring buffer is safe to share between one producer and one
// consumer thread. Each slot is only accessed by one thread at a time:
// the producer writes to slots[tail..tail+n], the consumer reads from
// slots[head..head+n]. The atomic head/tail ensure ordering.
unsafe impl Send for EventRing {}
unsafe impl Sync for EventRing {}

impl EventRing {
    /// Create a new ring buffer with the given capacity.
    /// Capacity will be rounded up to the next power of two.
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two();
        let mut buf = Vec::with_capacity(cap);
        for _ in 0..cap {
            buf.push(UnsafeCell::new(None));
        }
        Self {
            buffer: buf.into_boxed_slice(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Push a MIDI event into the ring. Called by the producer thread.
    ///
    /// Returns `true` if successful, `false` if the ring is full
    /// (event dropped — this is acceptable for real-time audio).
    #[inline]
    pub fn push(&self, event: MidiEvent) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let next_tail = tail.wrapping_add(1);

        // Check if full: tail would catch up to head
        if (next_tail & self.mask) == (head & self.mask) {
            return false; // Ring full — drop event
        }

        // Write the event
        let idx = tail & self.mask;
        unsafe {
            *self.buffer[idx].get() = Some(event);
        }

        // Publish
        self.tail.store(next_tail, Ordering::Release);
        true
    }

    /// Pop a MIDI event from the ring. Called by the consumer thread.
    ///
    /// Returns `Some(event)` if available, `None` if empty.
    #[inline]
    pub fn pop(&self) -> Option<MidiEvent> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        // Check if empty
        if (head & self.mask) == (tail & self.mask) {
            return None; // Ring empty
        }

        // Read the event
        let idx = head & self.mask;
        let event = unsafe { (*self.buffer[idx].get()).take() };

        // Advance head
        self.head.store(head.wrapping_add(1), Ordering::Release);

        event
    }

    /// Drain up to `max` events into the provided slice.
    /// Returns the number of events drained.
    /// This is the batch-consume path used by the audio thread.
    #[inline]
    pub fn drain_into(&self, out: &mut [MidiEvent]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.pop() {
                Some(event) => {
                    *slot = event;
                    count += 1;
                }
                None => break,
            }
        }
        count
    }

    /// Number of events currently in the ring (approximate).
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        tail.wrapping_sub(head) & self.mask
    }

    /// Is the ring empty? (approximate)
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        (head & self.mask) == (tail & self.mask)
    }

    /// Total capacity of the ring.
    pub fn capacity(&self) -> usize {
        self.mask + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(note: u8) -> MidiEvent {
        MidiEvent::note_on(0, note, 100, 0)
    }

    #[test]
    fn push_pop_basic() {
        let ring = EventRing::new(16);
        assert!(ring.is_empty());

        let event = make_event(60);
        assert!(ring.push(event));

        assert!(!ring.is_empty());
        let popped = ring.pop();
        assert!(popped.is_some());
        assert_eq!(popped.unwrap().note, 60);
        assert!(ring.is_empty());
    }

    #[test]
    fn fifo_ordering() {
        let ring = EventRing::new(64);
        for i in 0..10 {
            ring.push(make_event(i));
        }
        for i in 0..10 {
            let e = ring.pop().unwrap();
            assert_eq!(e.note, i);
        }
    }

    #[test]
    fn ring_full_returns_false() {
        let ring = EventRing::new(8);
        // Capacity is 7 (one slot always empty to distinguish full from empty)
        for i in 0..7 {
            assert!(ring.push(make_event(i)));
        }
        // Next push should fail
        assert!(!ring.push(make_event(99)));
    }

    #[test]
    fn drain_into_batch() {
        let ring = EventRing::new(64);
        for i in 0..20 {
            ring.push(make_event(i));
        }

        let mut buf = [MidiEvent::note_off(0, 0, 0); 32];
        let count = ring.drain_into(&mut buf);
        assert_eq!(count, 20);
        for i in 0..20 {
            assert_eq!(buf[i].note, i as u8);
        }
    }

    #[test]
    fn capacity_rounds_to_power_of_two() {
        let ring = EventRing::new(100);
        assert_eq!(ring.capacity(), 128); // next power of 2
    }

    #[test]
    fn wraparound() {
        let ring = EventRing::new(4); // real capacity = 3
        // Fill, drain, refill — exercises wraparound
        for cycle in 0..10 {
            for i in 0..3 {
                let note = (cycle * 3 + i) as u8;
                assert!(ring.push(make_event(note)));
            }
            let mut buf = [MidiEvent::note_off(0, 0, 0); 4];
            let count = ring.drain_into(&mut buf);
            assert_eq!(count, 3);
        }
    }

    #[test]
    fn len_tracks_count() {
        let ring = EventRing::new(16);
        assert_eq!(ring.len(), 0);
        ring.push(make_event(1));
        ring.push(make_event(2));
        assert_eq!(ring.len(), 2);
        ring.pop();
        assert_eq!(ring.len(), 1);
        ring.pop();
        assert_eq!(ring.len(), 0);
    }
}
