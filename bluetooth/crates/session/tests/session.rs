//! Sessions end to end: handshakes, transport, tampering and replay, peers
//! that lie about who they are, snow against the published vector for the
//! pattern, and random bytes thrown at every reader.

use bombechat_core::{safety_number, Identity, PublicIdentity};
use bombechat_session::{
    Handshake, Session, SessionError, Transport, MAX_MESSAGE, MAX_PLAINTEXT, PATTERN, TAG,
};
use ed25519_dalek::{Signer, SigningKey};

// ---------------------------------------------------------------------------
// helpers

/// Run a whole handshake between `a` (initiator) and `b` (responder).
fn handshake(a: &mut Handshake, b: &mut Handshake) {
    let m1 = a.write_message().unwrap();
    b.read_message(&m1).unwrap();
    let m2 = b.write_message().unwrap();
    a.read_message(&m2).unwrap();
    let m3 = a.write_message().unwrap();
    b.read_message(&m3).unwrap();
}

fn connected(alice: &Identity, bob: &Identity) -> (Transport, Transport) {
    let mut a = Session::initiator(alice, None);
    let mut b = Session::responder(bob);
    handshake(&mut a, &mut b);
    (a.into_transport().unwrap(), b.into_transport().unwrap())
}

fn signing_key(id: &Identity) -> SigningKey {
    let secret = id.secret_bytes();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret[0..32]);
    SigningKey::from_bytes(&seed)
}

fn static_secret(id: &Identity) -> Vec<u8> {
    id.secret_bytes()[32..64].to_vec()
}

/// A raw snow side, for building peers that break the rules.
fn raw(static_key: &[u8], initiator: bool) -> snow::HandshakeState {
    let builder = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(static_key)
        .unwrap();
    if initiator {
        builder.build_initiator().unwrap()
    } else {
        builder.build_responder().unwrap()
    }
}

/// A proof as the crate lays it out, from parts the test chooses.
fn proof(signer: &SigningKey, claimed: &PublicIdentity, role: u8, hash: &[u8]) -> Vec<u8> {
    let mut signed = b"bombechat session v1".to_vec();
    signed.push(role);
    signed.extend_from_slice(hash);
    signed.extend_from_slice(&claimed.dh);
    let signature = signer.sign(&signed);
    let mut out = claimed.sign.to_vec();
    out.extend_from_slice(&claimed.dh);
    out.extend_from_slice(&signature.to_bytes());
    out
}

fn raw_write(state: &mut snow::HandshakeState, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; MAX_MESSAGE];
    let n = state.write_message(payload, &mut out).unwrap();
    out.truncate(n);
    out
}

fn raw_read(state: &mut snow::HandshakeState, message: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; MAX_MESSAGE];
    let n = state.read_message(message, &mut out).unwrap();
    out.truncate(n);
    out
}

/// Our initiator against a raw responder whose message-2 proof is built by
/// `make_proof(handshake hash before message 2)`. Returns what our
/// initiator says about message 2.
fn against_raw_responder(
    responder_static: &[u8],
    expected: Option<&PublicIdentity>,
    make_proof: impl Fn(&[u8]) -> Vec<u8>,
) -> Result<(), SessionError> {
    let alice = Identity::generate();
    let mut a = Session::initiator(&alice, expected);
    let mut b = raw(responder_static, false);
    raw_read(&mut b, &a.write_message().unwrap());
    let hash = b.get_handshake_hash().to_vec();
    let m2 = raw_write(&mut b, &make_proof(&hash));
    let result = a.read_message(&m2);
    if result.is_err() {
        assert!(a.remote_identity().is_none());
        assert_eq!(a.write_message(), Err(SessionError::Dead));
    }
    result
}

/// A raw initiator whose message-3 proof is built by `make_proof` against
/// our responder. Returns what our responder says about message 3.
fn against_raw_initiator(
    initiator_static: &[u8],
    make_proof: impl Fn(&[u8]) -> Vec<u8>,
) -> Result<(), SessionError> {
    let bob = Identity::generate();
    let mut b = Session::responder(&bob);
    let mut a = raw(initiator_static, true);
    b.read_message(&raw_write(&mut a, &[])).unwrap();
    raw_read(&mut a, &b.write_message().unwrap());
    let hash = a.get_handshake_hash().to_vec();
    let m3 = raw_write(&mut a, &make_proof(&hash));
    let result = b.read_message(&m3);
    if result.is_err() {
        assert!(b.remote_identity().is_none());
        assert!(!b.is_finished());
    }
    result
}

/// A small seeded generator (SplitMix64), so the fuzz loops are the same on
/// every run without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }

    /// Random bytes, or a valid message with a few bytes flipped, cut short
    /// or run long, so the readers see near misses too.
    fn input(&mut self, valid: &[u8]) -> Vec<u8> {
        match self.below(4) {
            0 => {
                // Mostly short, sometimes around the valid length, rarely huge.
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

// ---------------------------------------------------------------------------
// the happy path

#[test]
fn two_sides_complete_a_handshake_and_exchange_messages() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mut a = Session::initiator(&alice, Some(&bob.public()));
    let mut b = Session::responder(&bob);

    let m1 = a.write_message().unwrap();
    assert_eq!(m1.len(), 32, "message 1 is the bare ephemeral key");
    b.read_message(&m1).unwrap();
    assert!(b.remote_identity().is_none());

    let m2 = b.write_message().unwrap();
    a.read_message(&m2).unwrap();
    assert_eq!(a.remote_identity(), Some(bob.public()));
    assert!(!a.is_finished());

    let m3 = a.write_message().unwrap();
    assert!(a.is_finished());
    assert!(!b.is_finished());
    b.read_message(&m3).unwrap();
    assert!(b.is_finished());
    assert_eq!(b.remote_identity(), Some(alice.public()));

    let mut ta = a.into_transport().unwrap();
    let mut tb = b.into_transport().unwrap();
    assert_eq!(*ta.remote_identity(), bob.public());
    assert_eq!(*tb.remote_identity(), alice.public());
    assert_eq!(
        safety_number(&alice.public(), ta.remote_identity()),
        safety_number(tb.remote_identity(), &bob.public())
    );

    for i in 0..5u8 {
        let text = vec![i; 10 * i as usize];
        let sealed = ta.encrypt(&text).unwrap();
        assert_eq!(sealed.len(), text.len() + TAG);
        assert_eq!(tb.decrypt(&sealed).unwrap(), text);

        let back = tb.encrypt(b"and back").unwrap();
        assert_eq!(ta.decrypt(&back).unwrap(), b"and back");
    }
}

#[test]
fn a_message_of_the_largest_size_goes_through_and_one_byte_more_is_refused() {
    let (mut ta, mut tb) = connected(&Identity::generate(), &Identity::generate());
    assert_eq!(MAX_PLAINTEXT, 65519);
    let big = vec![0xA5; MAX_PLAINTEXT];
    let sealed = ta.encrypt(&big).unwrap();
    assert_eq!(sealed.len(), MAX_MESSAGE);
    assert_eq!(tb.decrypt(&sealed).unwrap(), big);
    assert_eq!(
        ta.encrypt(&vec![0; MAX_PLAINTEXT + 1]),
        Err(SessionError::TooLarge)
    );
    assert_eq!(
        tb.decrypt(&vec![0; MAX_MESSAGE + 1]),
        Err(SessionError::Malformed)
    );
    assert_eq!(tb.decrypt(&[0; TAG - 1]), Err(SessionError::Malformed));
    // An empty message is just a tag.
    let empty = ta.encrypt(&[]).unwrap();
    assert_eq!(empty.len(), TAG);
    assert_eq!(tb.decrypt(&empty).unwrap(), Vec::<u8>::new());
}

// ---------------------------------------------------------------------------
// tampering and replay

#[test]
fn a_tampered_handshake_message_fails_and_ends_the_handshake() {
    for which in 1..=3 {
        for byte in [0usize, 31, 32, 40, 100, 191] {
            let alice = Identity::generate();
            let bob = Identity::generate();
            let mut a = Session::initiator(&alice, None);
            let mut b = Session::responder(&bob);
            let mut m1 = a.write_message().unwrap();
            if which == 1 {
                if byte >= m1.len() {
                    continue;
                }
                m1[byte] ^= 0x01;
            }
            // A flipped ephemeral key in message 1 still reads; it only
            // shows when message 2 can't be decrypted by the initiator.
            b.read_message(&m1).unwrap();
            let mut m2 = b.write_message().unwrap();
            if which == 2 {
                m2[byte] ^= 0x01;
            }
            let r2 = a.read_message(&m2);
            if which <= 2 {
                assert_eq!(
                    r2,
                    Err(SessionError::Tampered),
                    "message {which} byte {byte}"
                );
                assert_eq!(a.write_message(), Err(SessionError::Dead));
                assert!(!a.is_finished());
                assert!(a.into_transport().is_err());
                continue;
            }
            r2.unwrap();
            let mut m3 = a.write_message().unwrap();
            m3[byte] ^= 0x01;
            assert_eq!(
                b.read_message(&m3),
                Err(SessionError::Tampered),
                "message 3 byte {byte}"
            );
            assert_eq!(b.read_message(&m3), Err(SessionError::Dead));
            assert!(b.remote_identity().is_none());
            assert_eq!(b.into_transport().err(), Some(SessionError::Dead));
        }
    }
}

#[test]
fn a_tampered_transport_message_fails_and_the_next_genuine_one_still_decrypts() {
    let (mut ta, mut tb) = connected(&Identity::generate(), &Identity::generate());
    let sealed = ta.encrypt(b"meet at the bridge").unwrap();
    for i in 0..sealed.len() {
        let mut bad = sealed.clone();
        bad[i] ^= 0x80;
        assert_eq!(tb.decrypt(&bad), Err(SessionError::Tampered), "byte {i}");
    }
    assert_eq!(
        tb.decrypt(&sealed[..sealed.len() - 1]),
        Err(SessionError::Tampered)
    );
    assert_eq!(tb.decrypt(&sealed).unwrap(), b"meet at the bridge");
}

#[test]
fn a_replayed_transport_message_fails() {
    let (mut ta, mut tb) = connected(&Identity::generate(), &Identity::generate());
    let first = ta.encrypt(b"one").unwrap();
    assert_eq!(tb.decrypt(&first).unwrap(), b"one");
    assert_eq!(tb.decrypt(&first), Err(SessionError::Tampered));
    let second = ta.encrypt(b"two").unwrap();
    assert_eq!(tb.decrypt(&second).unwrap(), b"two");
    assert_eq!(tb.decrypt(&first), Err(SessionError::Tampered));
}

#[test]
fn a_reordered_transport_message_fails() {
    let (mut ta, mut tb) = connected(&Identity::generate(), &Identity::generate());
    let first = ta.encrypt(b"one").unwrap();
    let second = ta.encrypt(b"two").unwrap();
    assert_eq!(tb.decrypt(&second), Err(SessionError::Tampered));
    assert_eq!(tb.decrypt(&first).unwrap(), b"one");
    assert_eq!(tb.decrypt(&second).unwrap(), b"two");
}

#[test]
fn a_message_from_another_session_fails() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let (mut ta, _) = connected(&alice, &bob);
    let (_, mut tb2) = connected(&alice, &bob);
    let sealed = ta.encrypt(b"for the first session").unwrap();
    assert_eq!(tb2.decrypt(&sealed), Err(SessionError::Tampered));
}

#[test]
fn a_side_cannot_decrypt_its_own_message() {
    let (mut ta, _) = connected(&Identity::generate(), &Identity::generate());
    let sealed = ta.encrypt(b"echo").unwrap();
    assert_eq!(ta.decrypt(&sealed), Err(SessionError::Tampered));
}

// ---------------------------------------------------------------------------
// peers that lie about who they are

#[test]
fn a_responder_whose_noise_key_does_not_match_its_signed_identity_is_rejected() {
    // Mallory's Noise static key, carrying Carol's identity and Carol's own
    // valid signature over this handshake: Noise and the proof disagree.
    let mallory = Identity::generate();
    let carol = Identity::generate();
    let carol_key = signing_key(&carol);
    let result = against_raw_responder(&static_secret(&mallory), None, |hash| {
        proof(&carol_key, &carol.public(), 1, hash)
    });
    assert_eq!(result, Err(SessionError::KeyMismatch));
}

#[test]
fn a_responder_claiming_an_ed25519_key_it_cannot_sign_for_is_rejected() {
    // Noise key and claimed dh agree (Mallory's), but the claimed signing
    // key is Carol's and the signature is Mallory's.
    let mallory = Identity::generate();
    let carol = Identity::generate();
    let claimed = PublicIdentity {
        sign: carol.public().sign,
        dh: mallory.public().dh,
    };
    let mallory_key = signing_key(&mallory);
    let result = against_raw_responder(&static_secret(&mallory), None, |hash| {
        proof(&mallory_key, &claimed, 1, hash)
    });
    assert_eq!(result, Err(SessionError::BadSignature));
}

#[test]
fn a_responder_proof_signed_over_another_handshake_is_rejected() {
    let bob = Identity::generate();
    let key = signing_key(&bob);
    let result = against_raw_responder(&static_secret(&bob), None, |_| {
        proof(&key, &bob.public(), 1, &[0u8; 32])
    });
    assert_eq!(result, Err(SessionError::BadSignature));
}

#[test]
fn a_responder_proof_with_the_initiator_role_byte_is_rejected() {
    let bob = Identity::generate();
    let key = signing_key(&bob);
    let result = against_raw_responder(&static_secret(&bob), None, |hash| {
        proof(&key, &bob.public(), 2, hash)
    });
    assert_eq!(result, Err(SessionError::BadSignature));
}

#[test]
fn a_correct_raw_responder_is_accepted() {
    // The helpers build exactly what the crate builds, so the rejections
    // above are about the one thing each changes.
    let bob = Identity::generate();
    let key = signing_key(&bob);
    let result = against_raw_responder(&static_secret(&bob), Some(&bob.public()), |hash| {
        proof(&key, &bob.public(), 1, hash)
    });
    assert_eq!(result, Ok(()));
    let result = against_raw_initiator(&static_secret(&bob), |hash| {
        proof(&key, &bob.public(), 2, hash)
    });
    assert_eq!(result, Ok(()));
}

#[test]
fn a_responder_proof_of_the_wrong_length_is_malformed() {
    let bob = Identity::generate();
    let key = signing_key(&bob);
    for cut in [0usize, 1, 64, 127] {
        let result = against_raw_responder(&static_secret(&bob), None, |hash| {
            proof(&key, &bob.public(), 1, hash)[..cut].to_vec()
        });
        assert_eq!(result, Err(SessionError::Malformed), "cut to {cut}");
    }
    let result = against_raw_responder(&static_secret(&bob), None, |hash| {
        let mut p = proof(&key, &bob.public(), 1, hash);
        p.push(0);
        p
    });
    assert_eq!(result, Err(SessionError::Malformed));
}

#[test]
fn an_initiator_whose_noise_key_does_not_match_its_signed_identity_is_rejected() {
    let mallory = Identity::generate();
    let carol = Identity::generate();
    let carol_key = signing_key(&carol);
    let result = against_raw_initiator(&static_secret(&mallory), |hash| {
        proof(&carol_key, &carol.public(), 2, hash)
    });
    assert_eq!(result, Err(SessionError::KeyMismatch));

    let mallory_key = signing_key(&mallory);
    let claimed = PublicIdentity {
        sign: carol.public().sign,
        dh: mallory.public().dh,
    };
    let result = against_raw_initiator(&static_secret(&mallory), |hash| {
        proof(&mallory_key, &claimed, 2, hash)
    });
    assert_eq!(result, Err(SessionError::BadSignature));

    // Reflecting a responder-role proof back is refused too.
    let result = against_raw_initiator(&static_secret(&mallory), |hash| {
        proof(&mallory_key, &mallory.public(), 1, hash)
    });
    assert_eq!(result, Err(SessionError::BadSignature));
}

#[test]
fn an_initiator_opened_to_one_contact_refuses_another() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let carol = Identity::generate();
    let mut a = Session::initiator(&alice, Some(&bob.public()));
    let mut c = Session::responder(&carol);
    c.read_message(&a.write_message().unwrap()).unwrap();
    let m2 = c.write_message().unwrap();
    assert_eq!(a.read_message(&m2), Err(SessionError::UnexpectedPeer));
    assert!(a.remote_identity().is_none());
    assert_eq!(a.write_message(), Err(SessionError::Dead));
}

#[test]
fn message_one_with_a_payload_is_malformed() {
    let alice = Identity::generate();
    let mut a = raw(&static_secret(&alice), true);
    let m1 = raw_write(&mut a, b"hello");
    let mut b = Session::responder(&Identity::generate());
    assert_eq!(b.read_message(&m1), Err(SessionError::Malformed));
    assert_eq!(b.write_message(), Err(SessionError::Dead));
}

// ---------------------------------------------------------------------------
// the state machine

#[test]
fn steps_out_of_turn_are_refused_without_ending_the_handshake() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mut a = Session::initiator(&alice, None);
    let mut b = Session::responder(&bob);

    assert_eq!(b.write_message(), Err(SessionError::OutOfTurn));
    assert_eq!(a.read_message(&[0; 32]), Err(SessionError::OutOfTurn));
    assert_eq!(a.into_transport().err(), Some(SessionError::NotFinished));

    let mut a = Session::initiator(&alice, None);
    let m1 = a.write_message().unwrap();
    assert_eq!(a.write_message(), Err(SessionError::OutOfTurn));
    b.read_message(&m1).unwrap();
    assert_eq!(b.read_message(&m1), Err(SessionError::OutOfTurn));
    let m2 = b.write_message().unwrap();
    a.read_message(&m2).unwrap();
    let m3 = a.write_message().unwrap();
    b.read_message(&m3).unwrap();

    assert_eq!(a.write_message(), Err(SessionError::OutOfTurn));
    assert_eq!(a.read_message(&m2), Err(SessionError::OutOfTurn));
    assert_eq!(b.write_message(), Err(SessionError::OutOfTurn));
    assert_eq!(b.read_message(&m3), Err(SessionError::OutOfTurn));
    assert!(a.is_finished() && b.is_finished());
    let (mut ta, mut tb) = (a.into_transport().unwrap(), b.into_transport().unwrap());
    let sealed = ta.encrypt(b"still fine").unwrap();
    assert_eq!(tb.decrypt(&sealed).unwrap(), b"still fine");
}

#[test]
fn every_error_reads_as_a_sentence() {
    let all = [
        SessionError::OutOfTurn,
        SessionError::NotFinished,
        SessionError::Dead,
        SessionError::Malformed,
        SessionError::Tampered,
        SessionError::BadSignature,
        SessionError::KeyMismatch,
        SessionError::UnexpectedPeer,
        SessionError::TooLarge,
        SessionError::Exhausted,
        SessionError::Random,
        SessionError::Noise,
    ];
    for e in all {
        let text = e.to_string();
        assert!(text.starts_with(|c: char| c.is_ascii_lowercase()), "{text}");
        assert!(!text.ends_with('.'), "{text}");
        assert!(!text.contains('\u{2014}') && !text.contains("--"), "{text}");
    }
    assert!(SessionError::TooLarge.to_string().contains("65519"));
}

// ---------------------------------------------------------------------------
// known answer

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// The Noise_XX_25519_ChaChaPoly_SHA256 vector from the cacophony test
/// vectors, as snow 0.10.0 ships them in tests/vectors/cacophony.txt. It
/// proves the snow build linked here computes the standard pattern, byte
/// for byte, with the same features this crate turns on.
#[test]
fn noise_xx_matches_the_cacophony_vector() {
    const PROLOGUE: &str = "4a6f686e2047616c74";
    const INIT_STATIC: &str = "e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1";
    const INIT_EPHEMERAL: &str = "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a";
    const RESP_STATIC: &str = "4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893";
    const RESP_EPHEMERAL: &str = "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b";
    const HANDSHAKE_HASH: &str = "c8e5f64e846193be2a834104c2a009868d6c9f3bd3c186299888b488b2f1f58e";
    const MESSAGES: [(&str, &str); 6] = [
        (
            "4c756477696720766f6e204d69736573",
            "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c79444c756477696720766f6e204d69736573",
        ),
        (
            "4d757272617920526f746862617264",
            concat!(
                "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843",
                "81cbad1f276e038c48378ffce2b65285e08d6b68aaa3629a5a8639392490e5b9",
                "bd5269c2f1e4f488ed8831161f19b7815528f8982ffe09be9b5c412f8a0db50f",
                "8814c7194e83f23dbd8d162c9326ad",
            ),
        ),
        (
            "462e20412e20486179656b",
            concat!(
                "c7195ffacac1307ff99046f219750fc47693e23c3cb08b89c2af808b444850a8",
                "0ae475b9df0f169ae80a89be0865b57f58c9fea0d4ec82a286427402f113e4b6",
                "ae769a1d95941d49b25030",
            ),
        ),
        (
            "4361726c204d656e676572",
            "96763ed773f8e47bb3712f0e29b3060ffc956ffc146cee53d5e1df",
        ),
        (
            "4a65616e2d426170746973746520536179",
            "3e40f15f6f3a46ae446b253bf8b1d9ffb6ed9b174d272328ff91a7e2e5c79c07f5",
        ),
        (
            "457567656e2042f6686d20766f6e2042617765726b",
            "eb3f3515110702e047a6c9da4478b6ead94873c11c0f2d710ddb3f09fce024b3a58502ae3f",
        ),
    ];
    assert_eq!(PATTERN, "Noise_XX_25519_ChaChaPoly_SHA256");

    let prologue = unhex(PROLOGUE);
    let (is, ie, rs, re) = (
        unhex(INIT_STATIC),
        unhex(INIT_EPHEMERAL),
        unhex(RESP_STATIC),
        unhex(RESP_EPHEMERAL),
    );
    let side = |s: &[u8], e: &[u8], initiator: bool| {
        let b = snow::Builder::new(PATTERN.parse().unwrap())
            .local_private_key(s)
            .unwrap()
            .fixed_ephemeral_key_for_testing_only(e)
            .prologue(&prologue)
            .unwrap();
        if initiator {
            b.build_initiator().unwrap()
        } else {
            b.build_responder().unwrap()
        }
    };
    let mut init = side(&is, &ie, true);
    let mut resp = side(&rs, &re, false);

    // Messages 0..3 are the handshake, alternating from the initiator.
    for (i, (payload, ciphertext)) in MESSAGES[..3].iter().enumerate() {
        let (w, r) = if i % 2 == 0 {
            (&mut init, &mut resp)
        } else {
            (&mut resp, &mut init)
        };
        let sent = raw_write(w, &unhex(payload));
        assert_eq!(sent, unhex(ciphertext), "handshake message {i}");
        assert_eq!(raw_read(r, &sent), unhex(payload));
    }
    assert_eq!(init.get_handshake_hash(), unhex(HANDSHAKE_HASH).as_slice());
    assert_eq!(resp.get_handshake_hash(), unhex(HANDSHAKE_HASH).as_slice());

    let mut init = init.into_transport_mode().unwrap();
    let mut resp = resp.into_transport_mode().unwrap();
    for (i, (payload, ciphertext)) in MESSAGES[3..].iter().enumerate() {
        let plain = unhex(payload);
        let mut out = vec![0u8; MAX_MESSAGE];
        let mut back = vec![0u8; MAX_MESSAGE];
        // Transport messages keep alternating: responder, initiator, responder.
        let (n, m) = if i % 2 == 0 {
            let n = resp.write_message(&plain, &mut out).unwrap();
            (n, init.read_message(&out[..n], &mut back).unwrap())
        } else {
            let n = init.write_message(&plain, &mut out).unwrap();
            (n, resp.read_message(&out[..n], &mut back).unwrap())
        };
        assert_eq!(
            &out[..n],
            unhex(ciphertext).as_slice(),
            "transport message {i}"
        );
        assert_eq!(&back[..m], plain.as_slice());
    }
}

// ---------------------------------------------------------------------------
// fuzz-style: random and near-miss bytes at every reader, seeded

const FUZZ_ROUNDS: usize = 10_000;

#[test]
fn random_bytes_at_the_responder_reading_message_one_never_panic() {
    let bob = Identity::generate();
    let valid = Session::initiator(&Identity::generate(), None)
        .write_message()
        .unwrap();
    let mut rng = Rng(0x5E55_1011);
    let mut accepted = 0;
    for _ in 0..FUZZ_ROUNDS {
        let mut b = Session::responder(&bob);
        let input = rng.input(&valid);
        match b.read_message(&input) {
            // Any 32 bytes are an ephemeral key; Noise can't tell yet.
            Ok(()) => {
                assert_eq!(input.len(), 32);
                accepted += 1;
            }
            Err(e) => assert!(matches!(e, SessionError::Malformed), "{e:?}"),
        }
    }
    assert!(accepted > 0);
}

#[test]
fn random_bytes_at_the_initiator_reading_message_two_never_panic() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mut a = Session::initiator(&alice, None);
    let mut b = Session::responder(&bob);
    b.read_message(&a.write_message().unwrap()).unwrap();
    let valid = b.write_message().unwrap();

    let mut rng = Rng(0x5E55_1012);
    for _ in 0..FUZZ_ROUNDS {
        // A failed read ends a handshake, so each round needs a fresh one.
        let mut a = Session::initiator(&alice, None);
        a.write_message().unwrap();
        let input = rng.input(&valid);
        assert!(a.read_message(&input).is_err());
        assert!(a.remote_identity().is_none());
    }
}

#[test]
fn random_bytes_at_the_responder_reading_message_three_never_panic() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mut a = Session::initiator(&alice, None);
    let mut b = Session::responder(&bob);
    b.read_message(&a.write_message().unwrap()).unwrap();
    a.read_message(&b.write_message().unwrap()).unwrap();
    let valid = a.write_message().unwrap();

    let mut rng = Rng(0x5E55_1013);
    let m1 = Session::initiator(&alice, None).write_message().unwrap();
    for _ in 0..FUZZ_ROUNDS {
        let mut b = Session::responder(&bob);
        b.read_message(&m1).unwrap();
        b.write_message().unwrap();
        let input = rng.input(&valid);
        assert!(b.read_message(&input).is_err());
        assert!(b.remote_identity().is_none());
    }
}

#[test]
fn random_bytes_at_transport_decrypt_never_panic() {
    let (mut ta, mut tb) = connected(&Identity::generate(), &Identity::generate());
    let valid = ta.encrypt(b"the one real message").unwrap();
    let mut rng = Rng(0x5E55_1014);
    for _ in 0..FUZZ_ROUNDS {
        let input = rng.input(&valid);
        if input == valid {
            continue;
        }
        let e = tb.decrypt(&input).unwrap_err();
        assert!(
            matches!(e, SessionError::Malformed | SessionError::Tampered),
            "{e:?}"
        );
    }
    // None of that moved the receive counter.
    assert_eq!(tb.decrypt(&valid).unwrap(), b"the one real message");
}
