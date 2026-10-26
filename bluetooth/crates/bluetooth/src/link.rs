//! A link and the exchanges on it: handshakes part way, and sessions.

use std::collections::BTreeMap;

use bombechat_core::PublicIdentity;
use bombechat_session::{Handshake, TOKEN_LEN};
use rand_core::RngCore;

use crate::radio::{LinkId, Radio, TAG_LEN};
use crate::session::{LinkSession, SessKind};
use crate::wire::{fragment, Reassembly, FROM_INITIATOR};

/// Who began an exchange: this device, or the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Side {
    Mine,
    Theirs,
}

impl Side {
    pub fn flags(self) -> u8 {
        match self {
            Side::Mine => FROM_INITIATOR,
            Side::Theirs => 0,
        }
    }

    /// The side a received message belongs to, by its flags.
    pub fn of_received(flags: u8) -> Side {
        if flags & FROM_INITIATOR != 0 {
            Side::Theirs
        } else {
            Side::Mine
        }
    }
}

/// An exchange on a link: who began it, and its number.
pub(crate) type ExKey = (Side, u32);

pub(crate) enum Exchange {
    /// A contact hello this device sent: waiting for message 2. A decoy
    /// has no contact and is never answered.
    Trial {
        contact: Option<PublicIdentity>,
        hs: Handshake,
        sent_ms: u64,
        attempt: u8,
        /// A redial to a contact whose session ended, outside the rounds.
        redial: bool,
    },
    /// A request this device began: message 1 sent, waiting for message 2.
    Requesting {
        hs: Handshake,
        hello: Vec<u8>,
        sent_ms: u64,
        attempt: u8,
    },
    /// A request the other side began: message 2 sent, waiting for
    /// message 3.
    Answering {
        hs: Handshake,
        since_ms: u64,
    },
    Session(Box<LinkSession>),
}

pub(crate) struct Link {
    /// This device connected to the other's advert (so it may send
    /// contact hellos here); otherwise the other device connected to it.
    pub opened: bool,
    /// On a link this device opened, the advert's tag.
    pub tag: Option<[u8; TAG_LEN]>,
    /// The token contact hellos on this link name: one a contact gave
    /// inside a session whose tag the advert shows, if any; else one this
    /// device was given earlier for that advert, on a link it lost; else
    /// the one this link was given. `None` if none of those (the other
    /// device withheld it), and for a link the other device opened.
    pub token: Option<[u8; TOKEN_LEN]>,
    /// What this link's one round of hellos is remembered by: its token,
    /// or for a link without one, something standing for the advert.
    pub round: Option<[u8; TOKEN_LEN]>,
    pub up_ms: u64,
    /// The last time the link had a session or a handshake on it.
    pub busy_ms: u64,
    pub reassembly: Reassembly,
    next_fragment: u16,
    pub exchanges: BTreeMap<ExKey, Exchange>,
}

impl Link {
    pub fn new(
        opened: bool,
        tag: Option<[u8; TAG_LEN]>,
        token: Option<[u8; TOKEN_LEN]>,
        round: Option<[u8; TOKEN_LEN]>,
        now_ms: u64,
    ) -> Link {
        Link {
            opened,
            tag,
            token,
            round,
            up_ms: now_ms,
            busy_ms: now_ms,
            reassembly: Reassembly::new(),
            next_fragment: 0,
            exchanges: BTreeMap::new(),
        }
    }

    pub fn session(&mut self, k: &ExKey) -> Option<&mut LinkSession> {
        match self.exchanges.get_mut(k) {
            Some(Exchange::Session(s)) => Some(s),
            _ => None,
        }
    }

    pub fn has_confirmed_contact(&self) -> bool {
        self.exchanges.values().any(
            |e| matches!(e, Exchange::Session(s) if s.kind == SessKind::Contact && s.confirmed()),
        )
    }

    /// Whether this device has its own contact session here.
    pub fn has_own_session(&self) -> bool {
        self.exchanges.iter().any(|(k, e)| {
            k.0 == Side::Mine && matches!(e, Exchange::Session(s) if s.kind == SessKind::Contact)
        })
    }

    /// A fresh exchange number for this device's side.
    pub fn fresh_id(&self, rng: &mut dyn RngCore) -> u32 {
        loop {
            let id = rng.next_u32();
            if !self.exchanges.contains_key(&(Side::Mine, id)) {
                return id;
            }
        }
    }

    /// Write one link message, in fragments the link takes.
    pub fn put<R: Radio>(&mut self, radio: &mut R, id: LinkId, message: &[u8]) {
        let n = self.next_fragment;
        self.next_fragment = n.wrapping_add(1);
        for f in fragment(message, n, radio.max_write(id)) {
            radio.write(id, &f);
        }
    }
}

pub(crate) fn body_with_id(xid: u32, rest: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + rest.len());
    b.extend_from_slice(&xid.to_be_bytes());
    b.extend_from_slice(rest);
    b
}
