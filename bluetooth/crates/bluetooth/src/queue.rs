//! Envelopes waiting for the people they are for.

use std::collections::BTreeSet;

use bombechat_node::TransportEvent;

use crate::route::Targets;

/// An envelope waits this long for the people it is for to come in range.
pub const HOLD_MS: u64 = 10 * 60_000;
/// At most this many envelopes wait; one more drops the oldest.
pub const QUEUE_CAP: usize = 64;

/// A public identity as a map key: sign key then dh key.
pub(crate) type Key = [u8; 64];

pub(crate) struct Outgoing {
    pub tag: u16,
    pub envelope: Vec<u8>,
    pub targets: Targets,
    pub done: BTreeSet<Key>,
    done_fps: BTreeSet<[u8; 8]>,
    since_ms: u64,
    pub playing: bool,
    reported: bool,
}

#[derive(Default)]
pub(crate) struct Queue {
    pub items: Vec<Outgoing>,
}

impl Queue {
    /// Queue an envelope. The tag of one dropped to make room, if any.
    pub fn add(
        &mut self,
        tag: u16,
        envelope: Vec<u8>,
        targets: Targets,
        now_ms: u64,
    ) -> Option<u16> {
        self.items.retain(|o| o.tag != tag);
        let mut dropped = None;
        if self.items.len() >= QUEUE_CAP {
            let old = self.items.remove(0);
            if !old.reported {
                dropped = Some(old.tag);
            }
        }
        self.items.push(Outgoing {
            tag,
            envelope,
            targets,
            done: BTreeSet::new(),
            done_fps: BTreeSet::new(),
            since_ms: now_ms,
            playing: false,
            reported: false,
        });
        dropped
    }

    pub fn remove(&mut self, tag: u16) {
        self.items.retain(|o| o.tag != tag);
    }

    /// The contact `who` (with fingerprint `fp`) holds the envelope `tag`.
    pub fn acked(&mut self, tag: u16, who: Key, fp: [u8; 8]) {
        if let Some(o) = self.items.iter_mut().find(|o| o.tag == tag) {
            o.done.insert(who);
            o.done_fps.insert(fp);
        }
    }

    /// Report what everyone it was for now holds, and retire what has
    /// waited its time. `everyone` is every confirmed contact present.
    pub fn retire(&mut self, now_ms: u64, everyone: &BTreeSet<Key>) -> Vec<TransportEvent> {
        let mut events = Vec::new();
        self.items.retain_mut(|o| {
            if !o.reported {
                let complete = match &o.targets {
                    Targets::Only(fps) => fps.iter().all(|fp| o.done_fps.contains(fp)),
                    Targets::Everyone => !o.done.is_empty() && everyone.is_subset(&o.done),
                };
                if complete {
                    o.reported = true;
                    events.push(TransportEvent::Sent(o.tag));
                }
            }
            if now_ms.saturating_sub(o.since_ms) >= HOLD_MS {
                if !o.reported {
                    events.push(if o.done.is_empty() {
                        TransportEvent::Dropped(o.tag)
                    } else {
                        TransportEvent::Sent(o.tag)
                    });
                }
                return false;
            }
            // a broadcast stays for anyone who comes later in its time
            !(o.reported && matches!(o.targets, Targets::Only(_)))
        });
        events
    }

    /// Empty the queue: the tags never reported.
    pub fn drain_unreported(&mut self) -> Vec<u16> {
        self.items
            .drain(..)
            .filter(|o| !o.reported)
            .map(|o| o.tag)
            .collect()
    }
}
