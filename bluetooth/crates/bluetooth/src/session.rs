//! One session on a link: what goes inside the encryption, and how it is
//! kept whole over writes that may be lost, repeated or reordered.
//!
//! Every transport message is sealed under an explicit nonce, a counter
//! of this side's sends that is never reused, carried in the clear beside
//! the ciphertext. The receiver keeps a replay window over those nonces,
//! so a message is accepted at most once, in any order.
//!
//! Inside, padded to a size bucket:
//!
//! ```text
//! type u8 | ack u32 | seq u32 | len u16 | body[len] | zeros
//! ```
//!
//! `type` is keepalive, envelope, ack or token. `ack` is the next envelope
//! sequence number this side is waiting for: every message acknowledges,
//! and acknowledgements are as authenticated as everything else. `seq`
//! numbers envelopes, which are delivered once each, in order, and sent
//! again until acknowledged. A token message is a keepalive that also
//! carries the sender's token schedule: its current advertising token and
//! the next ones it will rotate to, in order. Contact sessions send
//! one as soon as they are confirmed, at the sender's every rotation, and
//! in place of every keepalive, so a lost one is made good within
//! [`KEEPALIVE_MS`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use bombechat_core::PublicIdentity;
use bombechat_session::{Transport as Cipher, MAX_NONCE, TOKEN_LEN};

use crate::wire::{Kind, LinkMessage};

/// A confirmed contact session sends something at least this often...
pub const KEEPALIVE_MS: u64 = 20_000;
/// ...and is closed after this long without hearing anything back, or
/// without an acknowledgement for what it sent.
pub const SILENCE_MS: u64 = 60_000;
/// Unacknowledged envelopes go again this often.
pub const RESEND_MS: u64 = 1_500;
/// An acknowledgement on its own waits a random time up to this long
/// after the envelope it answers (unless something else carries it
/// sooner), so how soon a reply follows doesn't tell an envelope from a
/// keepalive.
pub const ACK_DELAY_MAX_MS: u64 = 400;
/// At most this many envelopes in flight on a session, and at most this
/// many held ahead of a gap...
pub const WINDOW: usize = 32;
/// ...and those held ahead at most this many bytes.
pub const AHEAD_BYTES: usize = 64 * 1024;
/// Nonces this far below the highest accepted one are refused outright.
const REPLAY_WINDOW: u64 = 1024;

const KEEPALIVE: u8 = 0;
const ENVELOPE: u8 = 1;
const ACK: u8 = 2;
const TOKEN: u8 = 3;
const INNER_HEADER: usize = 1 + 4 + 4 + 2;
/// A token schedule holds at most this many tokens: the current one and
/// the next three.
pub(crate) const SCHEDULE_MAX: usize = 4;

/// Keepalives, token messages and acknowledgements are padded to the size
/// of a short private message, the commonest envelope, so they pass for
/// one.
const CONTROL_PAD: usize = 512;

/// The padded size of `n` bytes of plaintext: 256, 512, 1024, 2048, then
/// whole 4 KiB steps.
pub fn bucket(n: usize) -> usize {
    for b in [256, 512, 1024, 2048] {
        if n <= b {
            return b;
        }
    }
    n.div_ceil(4096) * 4096
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessKind {
    Contact,
    /// A request this device sent.
    RequestOut,
    /// A request this device received.
    RequestIn,
}

struct Unacked {
    seq: u32,
    body: Vec<u8>,
    tag: Option<u16>,
}

/// What one received message gave.
#[derive(Default)]
pub(crate) struct Delivery {
    /// Envelopes now in order, each for the first time.
    pub envelopes: Vec<Vec<u8>>,
    /// Tags of envelopes the other side now holds.
    pub acked: Vec<u16>,
    /// The other side's token schedule (current first, then the next
    /// ones in order), if this was a token message; empty otherwise.
    pub tokens: Vec<[u8; TOKEN_LEN]>,
}

pub(crate) struct LinkSession {
    pub kind: SessKind,
    cipher: Cipher,
    pub remote: Option<PublicIdentity>,
    flags: u8,
    xid: u32,
    pub started_ms: u64,
    pub confirmed_ms: Option<u64>,
    next_nonce: u64,
    send_next: u32,
    unacked: VecDeque<Unacked>,
    last_progress_ms: u64,
    last_resend_ms: u64,
    last_sent_ms: u64,
    last_heard_ms: u64,
    recv_next: u32,
    ahead: BTreeMap<u32, Vec<u8>>,
    ahead_bytes: usize,
    highest: Option<u64>,
    accepted: BTreeSet<u64>,
    ack_due: bool,
    /// When an acknowledgement on its own may go; `None` is at once.
    ack_at: Option<u64>,
    /// The token schedule this side last told the other, sent again in
    /// place of each keepalive.
    pub shared: Option<Vec<[u8; TOKEN_LEN]>>,
    /// For a request sent: the hello and which attempt this is.
    pub hello: Option<(Vec<u8>, u8)>,
    /// For a request received: when its hello arrived, and whether the
    /// person accepted it.
    pub heard_ms: Option<u64>,
    pub accepted_by_person: bool,
}

impl LinkSession {
    /// A session numbered `xid` on its link; `flags` marks what this side
    /// writes (who began the exchange).
    pub fn new(kind: SessKind, cipher: Cipher, flags: u8, xid: u32, now_ms: u64) -> LinkSession {
        let remote = cipher.remote_identity().cloned();
        let confirmed_ms = cipher.confirmed().then_some(now_ms);
        LinkSession {
            kind,
            cipher,
            remote,
            flags,
            xid,
            started_ms: now_ms,
            confirmed_ms,
            next_nonce: 0,
            send_next: 0,
            unacked: VecDeque::new(),
            last_progress_ms: now_ms,
            last_resend_ms: now_ms,
            last_sent_ms: now_ms,
            last_heard_ms: now_ms,
            recv_next: 0,
            ahead: BTreeMap::new(),
            ahead_bytes: 0,
            highest: None,
            accepted: BTreeSet::new(),
            ack_due: false,
            ack_at: None,
            shared: None,
            hello: None,
            heard_ms: None,
            accepted_by_person: false,
        }
    }

    pub fn confirmed(&self) -> bool {
        self.cipher.confirmed()
    }

    pub fn has_room(&self) -> bool {
        self.unacked.len() < WINDOW
    }

    pub fn all_acked(&self) -> bool {
        self.unacked.is_empty()
    }

    /// Tags of the envelopes in flight.
    pub fn in_flight(&self) -> impl Iterator<Item = u16> + '_ {
        self.unacked.iter().filter_map(|u| u.tag)
    }

    /// Bytes held ahead of a gap.
    pub fn held_ahead(&self) -> usize {
        self.ahead_bytes
    }

    fn seal(&mut self, ty: u8, seq: u32, body: &[u8], now_ms: u64) -> Option<Vec<u8>> {
        let nonce = self.next_nonce;
        if nonce > MAX_NONCE {
            return None;
        }
        let len = u16::try_from(body.len()).ok()?;
        let mut plain = Vec::with_capacity(bucket(INNER_HEADER + body.len()));
        plain.push(ty);
        plain.extend_from_slice(&self.recv_next.to_be_bytes());
        plain.extend_from_slice(&seq.to_be_bytes());
        plain.extend_from_slice(&len.to_be_bytes());
        plain.extend_from_slice(body);
        let pad = if ty == ENVELOPE {
            bucket(plain.len())
        } else {
            bucket(plain.len().max(CONTROL_PAD))
        };
        plain.resize(pad, 0);
        let ct = self.cipher.encrypt(nonce, &plain).ok()?;
        self.next_nonce += 1;
        self.last_sent_ms = now_ms;
        self.ack_due = false;
        self.ack_at = None;
        let mut wire = Vec::with_capacity(12 + ct.len());
        wire.extend_from_slice(&self.xid.to_be_bytes());
        wire.extend_from_slice(&nonce.to_be_bytes());
        wire.extend_from_slice(&ct);
        Some(
            LinkMessage {
                kind: Kind::Sealed,
                flags: self.flags,
                body: wire,
            }
            .encode(),
        )
    }

    /// Send one envelope, reliably. Its wire bytes, or `None` if the
    /// session can take no more.
    pub fn send_envelope(
        &mut self,
        envelope: &[u8],
        tag: Option<u16>,
        now_ms: u64,
    ) -> Option<Vec<u8>> {
        if self.send_next == u32::MAX || u16::try_from(envelope.len()).is_err() {
            return None;
        }
        let seq = self.send_next;
        let wire = self.seal(ENVELOPE, seq, envelope, now_ms)?;
        self.send_next += 1;
        if self.unacked.is_empty() {
            self.last_progress_ms = now_ms;
            self.last_resend_ms = now_ms;
        }
        self.unacked.push_back(Unacked {
            seq,
            body: envelope.to_vec(),
            tag,
        });
        Some(wire)
    }

    /// Hold the acknowledgement now due until `at_ms`, unless something
    /// else carries it sooner. Does nothing if none is due, or one is
    /// already held.
    pub fn hold_ack(&mut self, at_ms: u64) {
        if self.ack_due && self.ack_at.is_none() {
            self.ack_at = Some(at_ms);
        }
    }

    /// A keepalive, which also acknowledges: a token message, once a
    /// token has been shared.
    pub fn keepalive(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        match self.shared.clone() {
            Some(tokens) => self.seal(TOKEN, 0, &tokens.concat(), now_ms),
            None => self.seal(KEEPALIVE, 0, &[], now_ms),
        }
    }

    /// Tell the other side this side's token schedule (current first, at
    /// most [`SCHEDULE_MAX`]), now and in
    /// place of every keepalive from now on. Only for a confirmed contact
    /// session: the caller checks.
    pub fn send_token(&mut self, tokens: &[[u8; TOKEN_LEN]], now_ms: u64) -> Option<Vec<u8>> {
        self.shared = Some(tokens[..tokens.len().min(SCHEDULE_MAX)].to_vec());
        self.keepalive(now_ms)
    }

    /// Take one sealed message. `None` if it isn't authentic or was
    /// accepted before. With `listening` off, envelopes are neither taken
    /// nor acknowledged, so the sender keeps them and sends them again.
    pub fn receive(
        &mut self,
        nonce: u64,
        ct: &[u8],
        listening: bool,
        now_ms: u64,
    ) -> Option<Delivery> {
        if !self.fresh(nonce) {
            return None;
        }
        let plain = self.cipher.decrypt(nonce, ct).ok()?;
        self.mark(nonce);
        self.last_heard_ms = now_ms;
        self.confirmed_ms.get_or_insert(now_ms);
        if plain.len() < INNER_HEADER {
            return Some(Delivery::default());
        }
        let ty = plain[0];
        let ack = u32::from_be_bytes([plain[1], plain[2], plain[3], plain[4]]);
        let seq = u32::from_be_bytes([plain[5], plain[6], plain[7], plain[8]]);
        let len = u16::from_be_bytes([plain[9], plain[10]]) as usize;
        let Some(body) = plain.get(INNER_HEADER..INNER_HEADER + len) else {
            return Some(Delivery::default());
        };
        let mut d = Delivery {
            acked: self.acknowledge(ack, now_ms),
            envelopes: Vec::new(),
            tokens: Vec::new(),
        };
        // Envelopes, keepalives and token messages all draw an
        // acknowledgement, so whether a reply follows doesn't tell them
        // apart.
        if ty == ENVELOPE || ty == KEEPALIVE || ty == TOKEN {
            self.ack_due = true;
        }
        if ty == TOKEN
            && !body.is_empty()
            && body.len() % TOKEN_LEN == 0
            && body.len() <= SCHEDULE_MAX * TOKEN_LEN
        {
            d.tokens = body
                .chunks(TOKEN_LEN)
                .filter_map(|c| c.try_into().ok())
                .collect();
        }
        if ty == ENVELOPE && listening {
            if seq == self.recv_next {
                d.envelopes.push(body.to_vec());
                self.recv_next += 1;
                while let Some(next) = self.ahead.remove(&self.recv_next) {
                    self.ahead_bytes -= next.len();
                    d.envelopes.push(next);
                    self.recv_next += 1;
                }
            } else if seq > self.recv_next
                && (seq - self.recv_next) as usize <= WINDOW
                && self.ahead_bytes + body.len() <= AHEAD_BYTES
                && !self.ahead.contains_key(&seq)
            {
                self.ahead_bytes += body.len();
                self.ahead.insert(seq, body.to_vec());
            }
        }
        Some(d)
    }

    fn fresh(&self, nonce: u64) -> bool {
        match self.highest {
            None => true,
            Some(h) => nonce > h || (h - nonce < REPLAY_WINDOW && !self.accepted.contains(&nonce)),
        }
    }

    fn mark(&mut self, nonce: u64) {
        let h = self.highest.map_or(nonce, |h| h.max(nonce));
        self.highest = Some(h);
        self.accepted.insert(nonce);
        let floor = h.saturating_sub(REPLAY_WINDOW);
        self.accepted = self.accepted.split_off(&floor);
    }

    fn acknowledge(&mut self, next: u32, now_ms: u64) -> Vec<u16> {
        if next > self.send_next {
            return Vec::new();
        }
        let mut tags = Vec::new();
        while self.unacked.front().is_some_and(|u| u.seq < next) {
            if let Some(u) = self.unacked.pop_front() {
                tags.extend(u.tag);
            }
            self.last_progress_ms = now_ms;
            self.last_resend_ms = now_ms;
        }
        tags
    }

    /// What is due now: envelopes again, an acknowledgement, a keepalive.
    /// The writes, and whether the session has gone silent and should close.
    pub fn upkeep(&mut self, now_ms: u64) -> (Vec<Vec<u8>>, bool) {
        let silent = self.kind == SessKind::Contact
            && self.confirmed()
            && now_ms.saturating_sub(self.last_heard_ms) >= SILENCE_MS;
        let stalled =
            !self.unacked.is_empty() && now_ms.saturating_sub(self.last_progress_ms) >= SILENCE_MS;
        if silent || stalled {
            return (Vec::new(), true);
        }
        let mut writes = Vec::new();
        if !self.unacked.is_empty() && now_ms.saturating_sub(self.last_resend_ms) >= RESEND_MS {
            self.last_resend_ms = now_ms;
            let again: Vec<(u32, Vec<u8>)> = self
                .unacked
                .iter()
                .map(|u| (u.seq, u.body.clone()))
                .collect();
            for (seq, body) in again {
                writes.extend(self.seal(ENVELOPE, seq, &body, now_ms));
            }
        }
        if self.ack_due && self.ack_at.is_none_or(|at| now_ms >= at) {
            writes.extend(self.seal(ACK, 0, &[], now_ms));
        }
        if self.kind == SessKind::Contact
            && self.confirmed()
            && now_ms.saturating_sub(self.last_sent_ms) >= KEEPALIVE_MS
        {
            writes.extend(self.keepalive(now_ms));
        }
        (writes, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombechat_core::Identity;
    use bombechat_session::{Session, SessionKeys};

    fn pair() -> (LinkSession, LinkSession) {
        let a = SessionKeys::new(&Identity::generate());
        let mut i = Session::request_initiator(&a);
        let mut r = Session::request_responder();
        r.read_message(&i.write_message().unwrap()).unwrap();
        i.read_message(&r.write_message().unwrap()).unwrap();
        r.read_message(&i.write_message().unwrap()).unwrap();
        (
            LinkSession::new(SessKind::RequestOut, i.into_transport().unwrap(), 1, 7, 0),
            LinkSession::new(SessKind::RequestIn, r.into_transport().unwrap(), 0, 7, 0),
        )
    }

    /// The nonce and ciphertext of a sealed message's wire bytes.
    fn open(wire: &[u8]) -> (u64, Vec<u8>) {
        let m = LinkMessage::decode(wire).unwrap();
        let nonce = u64::from_be_bytes(m.body[4..12].try_into().unwrap());
        (nonce, m.body[12..].to_vec())
    }

    #[test]
    fn payloads_are_padded_to_buckets() {
        assert_eq!(bucket(1), 256);
        assert_eq!(bucket(257), 512);
        assert_eq!(bucket(2048), 2048);
        assert_eq!(bucket(2049), 4096);
        assert_eq!(bucket(4107), 8192);
        let (mut a, _) = pair();
        let small = a.send_envelope(&[1; 300], None, 0).unwrap();
        let other = a.send_envelope(&[1; 480], None, 0).unwrap();
        assert_eq!(small.len(), other.len(), "lengths hide inside the bucket");
        let ka = a.keepalive(0).unwrap();
        assert_eq!(
            ka.len(),
            small.len(),
            "a keepalive looks like a short message"
        );
    }

    #[test]
    fn a_repeated_message_is_taken_once_and_out_of_order_ones_wait() {
        let (mut a, mut b) = pair();
        let one = a.send_envelope(b"one", None, 0).unwrap();
        let two = a.send_envelope(b"two", None, 0).unwrap();
        let (n2, c2) = open(&two);
        let (n1, c1) = open(&one);
        assert!(b.receive(n2, &c2, true, 0).unwrap().envelopes.is_empty());
        assert!(b.receive(n2, &c2, true, 0).is_none(), "a replay is refused");
        let d = b.receive(n1, &c1, true, 0).unwrap();
        assert_eq!(d.envelopes, vec![b"one".to_vec(), b"two".to_vec()]);
        assert!(b.receive(n1, &c1, true, 0).is_none());
    }

    #[test]
    fn a_forged_ack_is_not_authentic() {
        let (mut a, mut b) = pair();
        let w = a.send_envelope(b"precious", Some(3), 0).unwrap();
        // someone on the link writes acknowledgement-shaped messages
        for nonce in [0, 1, 99] {
            let mut forged = vec![0u8; 256 + 16];
            forged[0] = ACK;
            forged[1..5].copy_from_slice(&5u32.to_be_bytes());
            assert!(a.receive(nonce, &forged, true, 0).is_none());
        }
        assert!(!a.all_acked(), "nothing was acknowledged");
        // b's genuine acknowledgement is
        let (n, c) = open(&w);
        b.receive(n, &c, true, 0).unwrap();
        let (n, c) = open(&b.keepalive(0).unwrap());
        assert_eq!(a.receive(n, &c, true, 0).unwrap().acked, vec![3]);
        assert!(a.all_acked());
    }

    #[test]
    fn a_replayed_old_message_below_the_window_is_refused() {
        let (mut a, mut b) = pair();
        let first = a.keepalive(0).unwrap();
        for _ in 0..(REPLAY_WINDOW + 5) {
            let (n, c) = open(&a.keepalive(0).unwrap());
            b.receive(n, &c, true, 0).unwrap();
        }
        let (n, c) = open(&first);
        assert!(b.receive(n, &c, true, 0).is_none());
    }

    #[test]
    fn listening_off_takes_and_acknowledges_nothing() {
        let (mut a, mut b) = pair();
        let w = a.send_envelope(b"later", Some(1), 0).unwrap();
        let (n, c) = open(&w);
        assert!(b.receive(n, &c, false, 0).unwrap().envelopes.is_empty());
        let (n, c) = open(&b.keepalive(0).unwrap());
        assert!(a.receive(n, &c, true, 0).unwrap().acked.is_empty());
        // sent again, now heard
        let (writes, _) = a.upkeep(RESEND_MS);
        let (n, c) = open(&writes[0]);
        assert_eq!(
            b.receive(n, &c, true, RESEND_MS).unwrap().envelopes,
            vec![b"later".to_vec()]
        );
    }

    #[test]
    fn a_held_acknowledgement_waits_unless_something_carries_it() {
        let (mut a, mut b) = pair();
        let (n, c) = open(&a.send_envelope(b"one", Some(1), 0).unwrap());
        b.receive(n, &c, true, 0).unwrap();
        b.hold_ack(300);
        b.hold_ack(5); // the first hold stands
        assert!(b.upkeep(299).0.is_empty(), "not yet");
        let (writes, _) = b.upkeep(300);
        assert_eq!(writes.len(), 1);
        let (n, c) = open(&writes[0]);
        assert_eq!(a.receive(n, &c, true, 300).unwrap().acked, vec![1]);
        // an envelope going the other way carries it sooner
        let (n, c) = open(&a.send_envelope(b"two", Some(2), 400).unwrap());
        b.receive(n, &c, true, 400).unwrap();
        b.hold_ack(800);
        let (n, c) = open(&b.send_envelope(b"back", None, 410).unwrap());
        assert_eq!(a.receive(n, &c, true, 410).unwrap().acked, vec![2]);
        assert!(b.upkeep(800).0.is_empty(), "nothing left to acknowledge");
        // with no hold, it goes at the next upkeep
        let (n, c) = open(&a.send_envelope(b"three", Some(3), 900).unwrap());
        b.receive(n, &c, true, 900).unwrap();
        assert_eq!(b.upkeep(900).0.len(), 1);
    }

    /// I1: a keepalive draws an acknowledgement just as an envelope
    /// does, at a held moment, and the acknowledgement draws nothing.
    #[test]
    fn a_keepalive_draws_an_acknowledgement_and_an_acknowledgement_none() {
        let (mut a, mut b) = pair();
        let (n, c) = open(&a.keepalive(1_000).unwrap());
        b.receive(n, &c, true, 1_000).unwrap();
        b.hold_ack(1_250);
        assert!(b.upkeep(1_249).0.is_empty());
        let (writes, _) = b.upkeep(1_250);
        assert_eq!(
            writes.len(),
            1,
            "an answer, though b's own keepalive isn't due"
        );
        let (n, c) = open(&writes[0]);
        a.receive(n, &c, true, 1_250).unwrap();
        a.hold_ack(1_250);
        assert!(
            a.upkeep(1_700).0.is_empty(),
            "an acknowledgement isn't answered"
        );
    }

    /// A token message is the size of a keepalive and of a short
    /// envelope on the wire, carries the schedule in order, draws an acknowledgement
    /// like them, and once sent takes the place of every keepalive.
    #[test]
    fn a_token_message_looks_like_a_keepalive_or_an_envelope() {
        let (mut a, mut b) = pair();
        let keepalive = a.keepalive(0).unwrap();
        let envelope = a.send_envelope(&[1; 300], None, 0).unwrap();
        let schedule = [
            [9; TOKEN_LEN],
            [10; TOKEN_LEN],
            [11; TOKEN_LEN],
            [12; TOKEN_LEN],
        ];
        let token = a.send_token(&schedule, 0).unwrap();
        assert_eq!(token.len(), keepalive.len());
        assert_eq!(token.len(), envelope.len());
        let (n, c) = open(&keepalive);
        assert!(b.receive(n, &c, true, 0).unwrap().tokens.is_empty());
        let (n, c) = open(&token);
        let d = b.receive(n, &c, true, 0).unwrap();
        assert_eq!(d.tokens, schedule.to_vec(), "the schedule, in order");
        assert!(d.envelopes.is_empty());
        b.hold_ack(10);
        assert_eq!(b.upkeep(10).0.len(), 1, "answered");
        // from now on a keepalive carries the token
        let (n, c) = open(&a.keepalive(20).unwrap());
        assert_eq!(
            b.receive(n, &c, true, 20).unwrap().tokens,
            schedule.to_vec()
        );
    }

    #[test]
    fn unordered_data_held_ahead_is_capped_by_bytes() {
        let (mut a, mut b) = pair();
        let big = vec![7u8; 20_000];
        a.send_envelope(&big, None, 0).unwrap(); // seq 0, never delivered
        for _ in 0..10 {
            let (n, c) = open(&a.send_envelope(&big, None, 0).unwrap());
            b.receive(n, &c, true, 0);
        }
        assert!(b.held_ahead() <= AHEAD_BYTES);
        assert_eq!(b.held_ahead(), 3 * 20_000);
    }
}
