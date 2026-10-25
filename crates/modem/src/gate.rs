//! The quiet gate: a reply waits until the room is quieter than a
//! threshold, then repeats a few times, each repeat also waiting for
//! quiet.
//!
//! [`QuietGate`] knows nothing about sound or sockets; it only tracks the
//! most recently reported room loudness and a FIFO queue of items waiting
//! to play. The caller feeds it room readings via [`QuietGate::observe`]
//! and polls [`QuietGate::due`] (for example once per demodulator
//! [`crate::DemodEvent::Level`]) to learn which item, if any, to play now.

use std::collections::VecDeque;

/// Approximate calibration offset (dBFS to a rough "room dB" figure) used
/// when no better measurement is available. See [`dbfs_to_room_db`].
pub const DEFAULT_CALIBRATION_OFFSET: f32 = 94.0;

/// Converts a dBFS level (0 dBFS is full scale) to an approximate room
/// loudness in dB, by adding `calibration_offset`.
///
/// This is a rough rule of thumb, not a calibrated sound-pressure-level
/// measurement: real microphones, gain stages and rooms vary, so the
/// result should be treated as approximate. [`DEFAULT_CALIBRATION_OFFSET`]
/// (94.0) is a reasonable default for a laptop microphone at typical
/// input gain.
pub fn dbfs_to_room_db(dbfs: f32, calibration_offset: f32) -> f32 {
    dbfs + calibration_offset
}

/// One item waiting in the gate's queue.
struct Entry {
    item: usize,
    /// How many times this item has played so far (0 before its first
    /// play).
    plays_done: u8,
    /// When it last played, if ever.
    last_played_ms: Option<u64>,
}

/// Holds reply items until the room is quiet enough to play them, then
/// repeats each a fixed number of times, spaced apart and also waiting for
/// quiet.
///
/// An item enqueued with [`QuietGate::enqueue`] first becomes due once the
/// most recently [`observe`](QuietGate::observe)d room level is below
/// `send_under_db`. After each play it becomes due again no earlier than
/// `repeat_gap_s` seconds later, and only once the room is quiet at (or
/// after) that time too. With `repeats = n` an item plays `1 + n` times in
/// total, then is removed from the queue. Several queued items play in
/// FIFO order; [`due`](QuietGate::due) returns at most one item per call.
/// Nothing is ever due before the first [`observe`](QuietGate::observe).
pub struct QuietGate {
    send_under_db: f32,
    repeats: u8,
    repeat_gap_ms: u64,
    last_room_db: Option<f32>,
    queue: VecDeque<Entry>,
}

impl QuietGate {
    pub fn new(send_under_db: f32, repeats: u8, repeat_gap_s: u32) -> Self {
        QuietGate {
            send_under_db,
            repeats,
            repeat_gap_ms: repeat_gap_s as u64 * 1000,
            last_room_db: None,
            queue: VecDeque::new(),
        }
    }

    /// Change the threshold: items play only while the room is quieter
    /// than `send_under_db`. Takes effect from the next
    /// [`due`](QuietGate::due).
    pub fn set_send_under(&mut self, send_under_db: f32) {
        self.send_under_db = send_under_db;
    }

    /// The threshold, in dB.
    pub fn send_under(&self) -> f32 {
        self.send_under_db
    }

    /// Queue `item` to play once the room is quiet.
    pub fn enqueue(&mut self, item: usize) {
        self.queue.push_back(Entry {
            item,
            plays_done: 0,
            last_played_ms: None,
        });
    }

    /// Record the room's current loudness in dB, as measured at `now_ms`.
    /// Only the most recent reading matters to [`due`](QuietGate::due).
    pub fn observe(&mut self, room_db: f32, now_ms: u64) {
        let _ = now_ms;
        self.last_room_db = Some(room_db);
    }

    /// Whether `item` is still waiting to play (its first play or a
    /// repeat). False once it has played `1 + repeats` times, so a caller
    /// keeping the item's samples elsewhere knows when to drop them.
    pub fn is_queued(&self, item: usize) -> bool {
        self.queue.iter().any(|e| e.item == item)
    }

    /// The item to play now, if any. Marks that item as played at
    /// `now_ms` and removes it from the queue once it has played
    /// `1 + repeats` times.
    pub fn due(&mut self, now_ms: u64) -> Option<usize> {
        let quiet = self.last_room_db.is_some_and(|db| db < self.send_under_db);
        if !quiet {
            return None;
        }

        let ready = |e: &Entry| match e.last_played_ms {
            None => true,
            Some(last) => now_ms >= last + self.repeat_gap_ms,
        };
        let pos = self.queue.iter().position(ready)?;

        let entry = &mut self.queue[pos];
        entry.plays_done += 1;
        entry.last_played_ms = Some(now_ms);
        let item = entry.item;
        let exhausted = entry.plays_done as u32 > self.repeats as u32;

        if exhausted {
            self.queue.remove(pos);
        }
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbfs_to_room_db_adds_the_calibration_offset() {
        assert_eq!(dbfs_to_room_db(-40.0, DEFAULT_CALIBRATION_OFFSET), 54.0);
        assert_eq!(dbfs_to_room_db(-40.0, 94.0), 54.0);
        assert_eq!(dbfs_to_room_db(0.0, 94.0), 94.0);
    }

    #[test]
    fn is_queued_until_the_last_repeat_has_played() {
        let mut g = QuietGate::new(40.0, 1, 90);
        assert!(!g.is_queued(3));
        g.enqueue(3);
        assert!(g.is_queued(3));
        g.observe(10.0, 0);
        assert_eq!(g.due(0), Some(3));
        assert!(g.is_queued(3), "one repeat still to play");
        assert_eq!(g.due(90_000), Some(3));
        assert!(!g.is_queued(3));
    }

    #[test]
    fn waits_for_quiet() {
        let mut g = QuietGate::new(40.0, 2, 90);
        g.enqueue(0);
        g.observe(47.0, 0);
        assert_eq!(g.due(0), None);
        g.observe(38.0, 1000);
        assert_eq!(g.due(1000), Some(0));
    }

    #[test]
    fn repeats_twice_ninety_seconds_apart() {
        let mut g = QuietGate::new(40.0, 2, 90);
        g.enqueue(7);
        g.observe(10.0, 0);

        // First play, as soon as the room is quiet.
        assert_eq!(g.due(0), Some(7));

        // The first repeat is not due before the 90 s gap, even though
        // the room stays quiet the whole time.
        assert_eq!(g.due(1), None);
        assert_eq!(g.due(89_999), None);

        // Due again exactly 90 s after the first play.
        assert_eq!(g.due(90_000), Some(7));

        // The second repeat follows the same rule, timed from the first
        // repeat.
        assert_eq!(g.due(90_001), None);
        assert_eq!(g.due(179_999), None);
        assert_eq!(g.due(180_000), Some(7));

        // Three plays total (1 + repeats(2)); never due again.
        assert_eq!(g.due(180_001), None);
        assert_eq!(g.due(270_000), None);
        assert_eq!(g.due(10_000_000), None);
    }

    #[test]
    fn nothing_due_before_any_observation() {
        let mut g = QuietGate::new(40.0, 0, 90);
        g.enqueue(1);
        assert_eq!(g.due(0), None);
        assert_eq!(g.due(1_000_000), None);

        // Still nothing once the room is loud.
        g.observe(90.0, 1_000_001);
        assert_eq!(g.due(1_000_001), None);
    }

    #[test]
    fn fifo_order_of_two_items() {
        let mut g = QuietGate::new(40.0, 0, 90);
        g.enqueue(1);
        g.enqueue(2);
        g.observe(10.0, 0);

        // Both are due for their one and only play, in enqueue order, one
        // per call.
        assert_eq!(g.due(0), Some(1));
        assert_eq!(g.due(0), Some(2));
        assert_eq!(g.due(0), None);
    }

    #[test]
    fn repeat_waits_for_quiet_after_the_gap() {
        let mut g = QuietGate::new(40.0, 1, 90);
        g.enqueue(5);
        g.observe(10.0, 0);
        assert_eq!(g.due(0), Some(5));

        // The gap has elapsed, but the room is loud: the repeat still
        // waits.
        g.observe(55.0, 90_000);
        assert_eq!(g.due(90_000), None);
        g.observe(50.0, 95_000);
        assert_eq!(g.due(95_000), None);

        // Quiet again, after the gap: now it plays.
        g.observe(30.0, 96_000);
        assert_eq!(g.due(96_000), Some(5));

        // One repeat used up; never due again.
        g.observe(10.0, 500_000);
        assert_eq!(g.due(500_000), None);
    }

    #[test]
    fn a_later_item_can_become_due_while_an_earlier_one_waits_on_its_gap() {
        // Even though item 1 is ahead in the queue, it is mid-repeat-cycle
        // and not ready; item 2, enqueued later but never played, should
        // not be blocked by it.
        let mut g = QuietGate::new(40.0, 1, 90);
        g.enqueue(1);
        g.observe(10.0, 0);
        assert_eq!(g.due(0), Some(1));

        g.enqueue(2);
        // Item 1's repeat is not due yet (gap not elapsed); item 2 is
        // ready for its first play.
        assert_eq!(g.due(1000), Some(2));
    }
}
