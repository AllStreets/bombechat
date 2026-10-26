//! Shared test helpers: honest sessions of each kind, raw snow peers that
//! can break the rules, a resolver that lies about its static key, and a
//! seeded generator for the fuzz loops.

#![allow(dead_code)]

use bombechat_core::{Identity, PublicIdentity};
use std::sync::OnceLock;

use bombechat_session::{
    Handshake, Session, SessionError, SessionKeys, Transport, BRIDGE_PATTERN, CERTIFICATE_LEN,
    CONTACT_PATTERN, MAX_MESSAGE, PROLOGUE, REQUEST_PATTERN, TOKEN_LEN,
};

/// The clock every contact handshake in the tests reads, Unix ms.
pub const NOW_MS: u64 = 1_800_000_000_000;

/// The responder's current and previous advertising tokens.
pub const TOKEN: [u8; TOKEN_LEN] = [0xB0; TOKEN_LEN];
pub const PREVIOUS_TOKEN: [u8; TOKEN_LEN] = [0xB1; TOKEN_LEN];
pub const MY_TOKENS: [[u8; TOKEN_LEN]; 2] = [TOKEN, PREVIOUS_TOKEN];

/// A contact hello's tail after the certificate: sent-at and target token.
pub fn hello_tail(sent_at: u64, token: [u8; TOKEN_LEN]) -> Vec<u8> {
    let mut out = sent_at.to_be_bytes().to_vec();
    out.extend_from_slice(&token);
    out
}
use ed25519_dalek::{Signer, SigningKey};
use snow::params::{CipherChoice, DHChoice, HashChoice};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::types::{Cipher, Dh, Hash, Random};
use x25519_dalek::{PublicKey, StaticSecret};

/// The three kinds of session, mirrored here so tests can loop over them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Contact,
    Request,
    Bridge,
}

pub const KINDS: [Kind; 3] = [Kind::Contact, Kind::Request, Kind::Bridge];

impl Kind {
    pub fn pattern(self) -> &'static str {
        match self {
            Kind::Contact => CONTACT_PATTERN,
            Kind::Request => REQUEST_PATTERN,
            Kind::Bridge => BRIDGE_PATTERN,
        }
    }

    pub fn messages(self) -> u8 {
        match self {
            Kind::Contact => 2,
            Kind::Request | Kind::Bridge => 3,
        }
    }

    pub fn sends_static(self, step: u8) -> bool {
        match self {
            Kind::Contact => step == 0,
            Kind::Request => step == 2,
            Kind::Bridge => step == 1 || step == 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

impl Role {
    pub fn other(self) -> Role {
        match self {
            Role::Initiator => Role::Responder,
            Role::Responder => Role::Initiator,
        }
    }

    /// Whether this role writes message `step`.
    pub fn writes(self, step: u8) -> bool {
        step.is_multiple_of(2) == (self == Role::Initiator)
    }
}

/// Our side of a handshake, through the public API. `me` is ignored by the
/// request responder; `them` is the contact a contact initiator dials or
/// the identity a bridge initiator insists on.
pub fn ours(kind: Kind, role: Role, me: &SessionKeys, them: Option<&PublicIdentity>) -> Handshake {
    match (kind, role) {
        (Kind::Contact, Role::Initiator) => {
            let them = them.expect("a contact initiator dials someone");
            Session::contact_initiator(me, them, NOW_MS, TOKEN)
        }
        (Kind::Contact, Role::Responder) => Session::contact_responder(me, NOW_MS, &MY_TOKENS),
        (Kind::Request, Role::Initiator) => Session::request_initiator(me),
        (Kind::Request, Role::Responder) => Session::request_responder(),
        (Kind::Bridge, Role::Initiator) => Session::bridge_initiator(me, them),
        (Kind::Bridge, Role::Responder) => Session::bridge_responder(me),
    }
}

/// Run a whole handshake between two of our sides and return every
/// message sent, in order.
pub fn run(kind: Kind, a: &mut Handshake, b: &mut Handshake) -> Vec<Vec<u8>> {
    let mut sent = Vec::new();
    for step in 0..kind.messages() {
        let (w, r) = if step.is_multiple_of(2) {
            (&mut *a, &mut *b)
        } else {
            (&mut *b, &mut *a)
        };
        let m = w.write_message().unwrap();
        r.read_message(&m).unwrap();
        sent.push(m);
    }
    sent
}

/// Honest handshakes of `kind` between alice (initiator) and bob
/// (responder), ready to read and write.
pub fn pair(kind: Kind, alice: &Identity, bob: &Identity) -> (Handshake, Handshake) {
    pair_keys(kind, &SessionKeys::new(alice), &SessionKeys::new(bob))
}

pub fn pair_keys(kind: Kind, alice: &SessionKeys, bob: &SessionKeys) -> (Handshake, Handshake) {
    (
        ours(kind, Role::Initiator, alice, Some(bob.public())),
        ours(kind, Role::Responder, bob, None),
    )
}

pub fn connected(kind: Kind, alice: &Identity, bob: &Identity) -> (Transport, Transport) {
    let (mut a, mut b) = pair(kind, alice, bob);
    run(kind, &mut a, &mut b);
    (a.into_transport().unwrap(), b.into_transport().unwrap())
}

/// An identity with its session keys made once, for loops that run many
/// handshakes.
pub struct Peer {
    pub id: Identity,
    pub keys: SessionKeys,
}

impl Peer {
    pub fn new() -> Peer {
        Peer::from(Identity::generate())
    }

    pub fn public(&self) -> PublicIdentity {
        self.id.public()
    }
}

impl From<Identity> for Peer {
    fn from(id: Identity) -> Peer {
        let keys = SessionKeys::new(&id);
        Peer { id, keys }
    }
}

/// Our side's keys in duels: one identity for the whole test binary.
fn our_keys() -> &'static SessionKeys {
    static KEYS: OnceLock<SessionKeys> = OnceLock::new();
    KEYS.get_or_init(|| SessionKeys::new(&Identity::generate()))
}

pub fn static_secret(id: &Identity) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&id.secret_bytes()[32..64]);
    out
}

/// A certificate naming `claimed`, signed by `signer`'s Ed25519 key over
/// `claimed.dh`, in the crate's layout.
pub fn forged_certificate(signer: &Identity, claimed: &PublicIdentity) -> Vec<u8> {
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&signer.secret_bytes()[0..32]);
    let key = SigningKey::from_bytes(&seed);
    let mut signed = b"bombechat static key v1".to_vec();
    signed.extend_from_slice(&claimed.dh);
    let mut out = claimed.sign.to_vec();
    out.extend_from_slice(&claimed.dh);
    out.extend_from_slice(&key.sign(&signed).to_bytes());
    out
}

// ---------------------------------------------------------------------------
// raw snow peers

/// How a raw peer's static key misbehaves.
#[derive(Clone, Copy)]
pub enum Lie {
    /// An honest static key.
    None,
    /// Presents this public key as its static, without its secret: its own
    /// secret is used for the Diffie-Hellman steps.
    Impostor([u8; 32]),
    /// Presents this low-order point as its static, and computes every
    /// static Diffie-Hellman as the all-zero result the other side gets.
    LowOrder([u8; 32]),
}

/// Low-order X25519 points: 0, 1, a point of order 8, and p - 1.
pub const LOW_ORDER: [[u8; 32]; 4] = [
    [0; 32],
    [
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ],
    [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4,
        0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49,
        0xb8, 0x00,
    ],
    [
        0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x7f,
    ],
];

struct LyingDh {
    secret: [u8; 32],
    public: [u8; 32],
    lie: Lie,
    /// Set through `set`, which snow uses for the static key only here.
    is_static: bool,
}

impl LyingDh {
    fn derive(&mut self) {
        self.public = PublicKey::from(&StaticSecret::from(self.secret)).to_bytes();
    }
}

impl Dh for LyingDh {
    fn name(&self) -> &'static str {
        "25519"
    }
    fn pub_len(&self) -> usize {
        32
    }
    fn priv_len(&self) -> usize {
        32
    }
    fn set(&mut self, privkey: &[u8]) {
        self.secret.copy_from_slice(&privkey[..32]);
        self.is_static = true;
        self.derive();
    }
    fn generate(&mut self, rng: &mut dyn Random) -> Result<(), snow::Error> {
        rng.try_fill_bytes(&mut self.secret)?;
        self.derive();
        Ok(())
    }
    fn pubkey(&self) -> &[u8] {
        match self.lie {
            Lie::Impostor(ref k) | Lie::LowOrder(ref k) if self.is_static => k,
            _ => &self.public,
        }
    }
    fn privkey(&self) -> &[u8] {
        &self.secret
    }
    fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
        if self.is_static && matches!(self.lie, Lie::LowOrder(_)) {
            out[..32].fill(0);
            return Ok(());
        }
        let mut their = [0u8; 32];
        their.copy_from_slice(&pubkey[..32]);
        let shared = StaticSecret::from(self.secret).diffie_hellman(&PublicKey::from(their));
        out[..32].copy_from_slice(shared.as_bytes());
        Ok(())
    }
}

struct LyingResolver(Lie);

impl CryptoResolver for LyingResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        DefaultResolver.resolve_rng()
    }
    fn resolve_dh(&self, _choice: &DHChoice) -> Option<Box<dyn Dh>> {
        Some(Box::new(LyingDh {
            secret: [0; 32],
            public: [0; 32],
            lie: self.0,
            is_static: false,
        }))
    }
    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        DefaultResolver.resolve_hash(choice)
    }
    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        DefaultResolver.resolve_cipher(choice)
    }
}

/// A raw snow side of `kind`, with this crate's prologue rule. `local` is
/// its static secret (none for a request responder); `remote` is the
/// static it dials (contact initiator only).
pub fn raw(
    kind: Kind,
    role: Role,
    local: Option<&[u8; 32]>,
    remote: Option<&[u8; 32]>,
    lie: Lie,
) -> snow::HandshakeState {
    let params = kind.pattern().parse().unwrap();
    let mut b = snow::Builder::with_resolver(params, Box::new(LyingResolver(lie)));
    if let Some(local) = local {
        b = b.local_private_key(local).unwrap();
    }
    if let Some(remote) = remote {
        b = b.remote_public_key(remote).unwrap();
    }
    if kind != Kind::Bridge {
        b = b.prologue(PROLOGUE).unwrap();
    }
    match role {
        Role::Initiator => b.build_initiator().unwrap(),
        Role::Responder => b.build_responder().unwrap(),
    }
}

pub fn raw_write(state: &mut snow::HandshakeState, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; MAX_MESSAGE];
    let n = state.write_message(payload, &mut out).unwrap();
    out.truncate(n);
    out
}

pub fn raw_read(state: &mut snow::HandshakeState, message: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0u8; MAX_MESSAGE];
    let n = state.read_message(message, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

/// One of our sides against a raw peer playing `raw_role`.
pub struct Duel {
    pub kind: Kind,
    pub raw_role: Role,
    pub ours: Handshake,
    pub raw: snow::HandshakeState,
    /// The raw peer's identity and certificate, as an honest one sends it.
    pub raw_public: PublicIdentity,
    pub raw_certificate: [u8; CERTIFICATE_LEN],
}

impl Duel {
    /// Our side uses the binary's shared keys (none for the request
    /// responder). The raw peer's static key is `peer`'s, presented as
    /// `lie` says.
    pub fn new(kind: Kind, raw_role: Role, peer: &Peer, lie: Lie) -> Duel {
        let me = our_keys();
        let our_role = raw_role.other();
        let raw_public = peer.public();
        // Only the contact initiator needs to know whom it dials.
        let them = (kind == Kind::Contact).then_some(&raw_public);
        let ours = ours(kind, our_role, me, them);
        let raw_static = static_secret(&peer.id);
        let local = match (kind, raw_role) {
            (Kind::Request, Role::Responder) => None,
            _ => Some(&raw_static),
        };
        let remote = match (kind, raw_role) {
            (Kind::Contact, Role::Initiator) => Some(&me.public().dh),
            _ => None,
        };
        Duel {
            kind,
            raw_role,
            ours,
            raw: raw(kind, raw_role, local, remote, lie),
            raw_public,
            raw_certificate: *peer.keys.certificate(),
        }
    }

    /// The payload an honest raw peer sends at `step`.
    pub fn honest_payload(&self, step: u8) -> Vec<u8> {
        if self.kind.sends_static(step) {
            self.wrap(step, &self.raw_certificate)
        } else {
            Vec::new()
        }
    }

    /// `certificate` as the payload of message `step`: as it is, or for a
    /// contact message 1, with an honest hello tail after it.
    pub fn wrap(&self, step: u8, certificate: &[u8]) -> Vec<u8> {
        let mut out = certificate.to_vec();
        if self.kind == Kind::Contact && step == 0 {
            out.extend(hello_tail(NOW_MS, TOKEN));
        }
        out
    }

    /// Play the handshake, the raw peer sending `payload(step)` in its
    /// messages. Returns our side's first error, if any.
    pub fn play(&mut self, payload: impl Fn(u8) -> Vec<u8>) -> Result<(), SessionError> {
        for step in 0..self.kind.messages() {
            if self.raw_role.writes(step) {
                let m = raw_write(&mut self.raw, &payload(step));
                self.ours.read_message(&m)?;
            } else {
                let m = self.ours.write_message()?;
                raw_read(&mut self.raw, &m).ok_or(SessionError::Noise)?;
            }
        }
        Ok(())
    }
}

/// Where a raw peer sends its static key and certificate: (kind, the raw
/// peer's role, the step).
pub const CERTIFICATE_SENDERS: [(Kind, Role, u8); 4] = [
    (Kind::Contact, Role::Initiator, 0),
    (Kind::Request, Role::Initiator, 2),
    (Kind::Bridge, Role::Responder, 1),
    (Kind::Bridge, Role::Initiator, 2),
];

/// Where a raw peer sends a message whose payload must be empty.
pub const EMPTY_SENDERS: [(Kind, Role, u8); 4] = [
    (Kind::Contact, Role::Responder, 1),
    (Kind::Request, Role::Initiator, 0),
    (Kind::Request, Role::Responder, 1),
    (Kind::Bridge, Role::Initiator, 0),
];

// ---------------------------------------------------------------------------
// seeded generator

/// SplitMix64, so the fuzz loops are the same on every run without a
/// dependency.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// Random bytes, or `valid` with a few bits flipped, cut short or run
    /// long, so readers see near misses too.
    pub fn input(&mut self, valid: &[u8]) -> Vec<u8> {
        match self.below(4) {
            0 => {
                let len = match self.below(10) {
                    0 => self.below(MAX_MESSAGE + 64),
                    1..=4 => self.below(valid.len() + 32),
                    _ => self.below(64),
                };
                self.bytes(len)
            }
            1 => {
                let mut m = valid.to_vec();
                for _ in 0..=self.below(3) {
                    if !m.is_empty() {
                        let i = self.below(m.len());
                        m[i] ^= 1 << self.below(8);
                    }
                }
                m
            }
            2 => {
                let mut m = valid.to_vec();
                m.truncate(self.below(valid.len() + 1));
                m
            }
            _ => {
                let mut m = valid.to_vec();
                let extra = self.below(32) + 1;
                m.extend(self.bytes(extra));
                m
            }
        }
    }
}
