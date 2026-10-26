//! Sessions end to end: the three kinds of handshake, transport, tampering
//! and replay, certificates, weak keys, the contact responder's choice to
//! stay silent and the request recipient's anonymity.

mod common;

use bombechat_core::{safety_number, Identity, PublicIdentity};
use bombechat_session::{
    certificate, Session, SessionError, SessionKeys, CERTIFICATE_LEN, CONTACT_HELLO_LEN,
    CONTACT_WINDOW_MS, MAX_MESSAGE, MAX_NONCE, MAX_PLAINTEXT, TAG, TOKEN_LEN,
};
use common::*;

// ---------------------------------------------------------------------------
// the happy paths

#[test]
fn a_contact_session_completes_and_the_responder_sees_who_it_is_before_answering() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mut a = Session::contact_initiator(&SessionKeys::new(&alice), &bob.public(), NOW_MS, TOKEN);
    let mut b = Session::contact_responder(&SessionKeys::new(&bob), NOW_MS, &MY_TOKENS);
    assert!(a.remote_identity().is_none());

    let m1 = a.write_message().unwrap();
    b.read_message(&m1).unwrap();
    // Bob knows it is Alice before he has sent a single byte.
    assert_eq!(b.remote_identity(), Some(alice.public()));
    assert!(!b.is_finished());

    let m2 = b.write_message().unwrap();
    assert!(b.is_finished());
    a.read_message(&m2).unwrap();
    assert!(a.is_finished());
    assert_eq!(a.remote_identity(), Some(bob.public()));

    let mut ta = a.into_transport().unwrap();
    let mut tb = b.into_transport().unwrap();
    assert_eq!(ta.remote_identity(), Some(&bob.public()));
    assert_eq!(tb.remote_identity(), Some(&alice.public()));
    assert_eq!(
        safety_number(&alice.public(), &bob.public()),
        safety_number(tb.remote_identity().unwrap(), ta.remote_identity().unwrap())
    );
    for i in 0..5u8 {
        let text = vec![i; 10 * i as usize];
        let sealed = ta.encrypt(0, &text).unwrap();
        assert_eq!(sealed.len(), text.len() + TAG);
        assert_eq!(tb.decrypt(0, &sealed).unwrap(), text);
        let back = tb.encrypt(0, b"and back").unwrap();
        assert_eq!(ta.decrypt(0, &back).unwrap(), b"and back");
    }
}

#[test]
fn a_request_session_completes_with_the_recipient_anonymous_and_works_both_ways() {
    let alice = Identity::generate();
    let mut a = Session::request_initiator(&SessionKeys::new(&alice));
    let mut b = Session::request_responder();
    let sent = run(Kind::Request, &mut a, &mut b);
    assert_eq!(sent.len(), 3);
    assert!(a.is_finished() && b.is_finished());
    assert_eq!(a.remote_identity(), None);
    assert_eq!(b.remote_identity(), Some(alice.public()));

    let mut ta = a.into_transport().unwrap();
    let mut tb = b.into_transport().unwrap();
    assert_eq!(ta.remote_identity(), None);
    assert_eq!(tb.remote_identity(), Some(&alice.public()));
    // The recipient's hello, after it accepts, goes this way.
    let hello = tb.encrypt(0, b"hello, accepted").unwrap();
    assert_eq!(ta.decrypt(0, &hello).unwrap(), b"hello, accepted");
    let reply = ta.encrypt(0, b"thanks").unwrap();
    assert_eq!(tb.decrypt(0, &reply).unwrap(), b"thanks");
}

#[test]
fn a_bridge_session_completes_and_each_side_learns_the_other() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let mut a = Session::bridge_initiator(&SessionKeys::new(&alice), Some(&bob.public()));
    let mut b = Session::bridge_responder(&SessionKeys::new(&bob));
    let m1 = a.write_message().unwrap();
    assert_eq!(m1.len(), 32, "message 1 is the bare ephemeral key");
    b.read_message(&m1).unwrap();
    assert!(b.remote_identity().is_none());
    a.read_message(&b.write_message().unwrap()).unwrap();
    assert_eq!(a.remote_identity(), Some(bob.public()));
    assert!(!a.is_finished());
    b.read_message(&a.write_message().unwrap()).unwrap();
    assert!(a.is_finished() && b.is_finished());
    assert_eq!(b.remote_identity(), Some(alice.public()));
    let (ta, mut tb) = (a.into_transport().unwrap(), b.into_transport().unwrap());
    let sealed = ta.encrypt(0, b"over the bridge").unwrap();
    assert_eq!(tb.decrypt(0, &sealed).unwrap(), b"over the bridge");
}

#[test]
fn a_bridge_initiator_opened_to_one_identity_refuses_another() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let carol = Identity::generate();
    let mut a = Session::bridge_initiator(&SessionKeys::new(&alice), Some(&bob.public()));
    let mut c = Session::bridge_responder(&SessionKeys::new(&carol));
    c.read_message(&a.write_message().unwrap()).unwrap();
    let m2 = c.write_message().unwrap();
    assert_eq!(a.read_message(&m2), Err(SessionError::UnexpectedPeer));
    assert!(a.remote_identity().is_none());
    assert_eq!(a.write_message(), Err(SessionError::Dead));
}

#[test]
fn a_message_of_the_largest_size_goes_through_and_one_byte_more_is_refused() {
    for kind in KINDS {
        let (ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        assert_eq!(MAX_PLAINTEXT, 65519);
        let big = vec![0xA5; MAX_PLAINTEXT];
        let sealed = ta.encrypt(0, &big).unwrap();
        assert_eq!(sealed.len(), MAX_MESSAGE);
        assert_eq!(tb.decrypt(0, &sealed).unwrap(), big);
        assert_eq!(
            ta.encrypt(0, &vec![0; MAX_PLAINTEXT + 1]),
            Err(SessionError::TooLarge)
        );
        assert_eq!(
            tb.decrypt(0, &vec![0; MAX_MESSAGE + 1]),
            Err(SessionError::Malformed)
        );
        assert_eq!(tb.decrypt(0, &[0; TAG - 1]), Err(SessionError::Malformed));
        let empty = ta.encrypt(1, &[]).unwrap();
        assert_eq!(empty.len(), TAG);
        assert_eq!(tb.decrypt(1, &empty).unwrap(), Vec::<u8>::new());
    }
}

// ---------------------------------------------------------------------------
// staying silent, staying anonymous

#[test]
fn a_contact_responder_that_drops_a_stranger_sends_nothing() {
    let stranger = Identity::generate();
    let bob = Identity::generate();
    let contacts = [Identity::generate().public(), Identity::generate().public()];

    // What Bob's link layer would put on the air.
    let mut outbox: Vec<Vec<u8>> = Vec::new();
    let mut a =
        Session::contact_initiator(&SessionKeys::new(&stranger), &bob.public(), NOW_MS, TOKEN);
    let mut b = Session::contact_responder(&SessionKeys::new(&bob), NOW_MS, &MY_TOKENS);
    b.read_message(&a.write_message().unwrap()).unwrap();
    let them = b.remote_identity().unwrap();
    assert_eq!(them, stranger.public());
    if contacts.contains(&them) {
        outbox.push(b.write_message().unwrap());
    } else {
        drop(b);
    }
    assert_eq!(outbox.iter().map(Vec::len).sum::<usize>(), 0);
    assert!(!a.is_finished());
    assert!(a.remote_identity().is_none());

    // And a contact gets an answer the same way.
    let mut a =
        Session::contact_initiator(&SessionKeys::new(&stranger), &bob.public(), NOW_MS, TOKEN);
    let mut b = Session::contact_responder(&SessionKeys::new(&bob), NOW_MS, &MY_TOKENS);
    b.read_message(&a.write_message().unwrap()).unwrap();
    let known = [stranger.public()];
    if known.contains(&b.remote_identity().unwrap()) {
        outbox.push(b.write_message().unwrap());
    }
    assert_eq!(outbox.len(), 1);
    a.read_message(&outbox[0]).unwrap();
    assert!(a.is_finished());
}

#[test]
fn a_contact_message_one_sent_to_the_wrong_device_fails_there() {
    // Alice trial-sends to Bob's key; Carol hears it. Carol can't read who
    // it is from, and stays silent.
    let alice = Identity::generate();
    let bob = Identity::generate();
    let carol = Identity::generate();
    let mut a = Session::contact_initiator(&SessionKeys::new(&alice), &bob.public(), NOW_MS, TOKEN);
    let mut c = Session::contact_responder(&SessionKeys::new(&carol), NOW_MS, &MY_TOKENS);
    assert_eq!(
        c.read_message(&a.write_message().unwrap()),
        Err(SessionError::Tampered)
    );
    assert!(c.remote_identity().is_none());
    assert_eq!(c.write_message(), Err(SessionError::Dead));
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_request_recipient_never_reveals_its_static_key_or_certificate() {
    let alice = Identity::generate();
    // The recipient's identity exists on its device but plays no part.
    let bob = Identity::generate();
    let mut a = Session::request_initiator(&SessionKeys::new(&alice));
    let mut b = Session::request_responder();
    let mut wire = run(Kind::Request, &mut a, &mut b);
    let (mut ta, mut tb) = (a.into_transport().unwrap(), b.into_transport().unwrap());
    for i in 0..4u8 {
        let n = u64::from(i);
        let there = ta.encrypt(n, &[i; 40]).unwrap();
        tb.decrypt(n, &there).unwrap();
        let back = tb.encrypt(n, &[i; 40]).unwrap();
        ta.decrypt(n, &back).unwrap();
        wire.push(there);
        wire.push(back);
    }
    let bob_public = bob.public();
    let bob_certificate = certificate(&bob);
    let alice_public = alice.public();
    for (i, m) in wire.iter().enumerate() {
        for secret in [
            &bob_public.sign[..],
            &bob_public.dh[..],
            &bob_certificate[..],
            &bob_certificate[64..],
        ] {
            assert!(!contains(m, secret), "message {i} carries the recipient");
        }
        // The requester's identity goes encrypted, never in the clear.
        assert!(!contains(m, &alice_public.sign), "message {i}");
        assert!(!contains(m, &alice_public.dh), "message {i}");
    }
}

// ---------------------------------------------------------------------------
// tampering and replay

#[test]
fn a_tampered_handshake_message_fails_and_ends_the_handshake() {
    for kind in KINDS {
        for which in 0..kind.messages() {
            for byte in [0usize, 31, 32, 40, 100, 191] {
                let (mut a, mut b) = pair(kind, &Identity::generate(), &Identity::generate());
                let mut first_error = None;
                let mut skipped = false;
                for step in 0..kind.messages() {
                    let (w, r) = if step.is_multiple_of(2) {
                        (&mut a, &mut b)
                    } else {
                        (&mut b, &mut a)
                    };
                    let mut m = w.write_message().unwrap();
                    if step == which {
                        if byte >= m.len() {
                            skipped = true;
                            break;
                        }
                        m[byte] ^= 0x01;
                    }
                    if let Err(e) = r.read_message(&m) {
                        first_error = Some(e);
                        assert_eq!(r.write_message(), Err(SessionError::Dead));
                        assert!(r.remote_identity().is_none());
                        break;
                    }
                }
                if skipped {
                    continue;
                }
                // A flipped ephemeral key in an XX or XN message 1 still
                // reads; it shows when message 2 can't be decrypted.
                let case = format!("{kind:?} message {which} byte {byte}");
                assert_eq!(first_error, Some(SessionError::Tampered), "{case}");
                assert!(!(a.is_finished() && b.is_finished()), "{case}");
            }
        }
    }
}

#[test]
fn a_tampered_transport_message_fails_and_the_next_genuine_one_still_decrypts() {
    for kind in KINDS {
        let (ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        let sealed = ta.encrypt(0, b"meet at the bridge").unwrap();
        for i in 0..sealed.len() {
            let mut bad = sealed.clone();
            bad[i] ^= 0x80;
            assert_eq!(tb.decrypt(0, &bad), Err(SessionError::Tampered), "byte {i}");
        }
        assert_eq!(
            tb.decrypt(0, &sealed[..sealed.len() - 1]),
            Err(SessionError::Tampered)
        );
        assert_eq!(tb.decrypt(0, &sealed).unwrap(), b"meet at the bridge");
    }
}

#[test]
fn a_message_decrypts_only_under_its_own_nonce_in_any_order() {
    for kind in KINDS {
        let (ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        let first = ta.encrypt(0, b"one").unwrap();
        let second = ta.encrypt(1, b"two").unwrap();
        // out of order is fine: each carries its own nonce
        assert_eq!(tb.decrypt(1, &second).unwrap(), b"two");
        assert_eq!(tb.decrypt(0, &first).unwrap(), b"one");
        // moved to another nonce, it fails
        assert_eq!(tb.decrypt(1, &first), Err(SessionError::Tampered));
        assert_eq!(tb.decrypt(0, &second), Err(SessionError::Tampered));
        assert_eq!(tb.decrypt(7, &first), Err(SessionError::Tampered));
        // a repeat under its own nonce decrypts again: refusing it is the
        // caller's job (the link layer's replay window)
        assert_eq!(tb.decrypt(0, &first).unwrap(), b"one");
    }
}

#[test]
fn the_largest_nonce_is_refused_and_the_one_below_it_works() {
    for kind in KINDS {
        let (ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        assert_eq!(ta.encrypt(u64::MAX, b"x"), Err(SessionError::Exhausted));
        let last = ta.encrypt(MAX_NONCE, b"last").unwrap();
        assert_eq!(tb.decrypt(MAX_NONCE, &last).unwrap(), b"last");
        assert_eq!(tb.decrypt(u64::MAX, &last), Err(SessionError::Malformed));
    }
}

#[test]
fn each_direction_has_its_own_nonces() {
    for kind in KINDS {
        let (mut ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        // the same nonce both ways gives unrelated messages, neither of
        // which reads as the other direction's
        let there = ta.encrypt(5, b"there").unwrap();
        let back = tb.encrypt(5, b"back").unwrap();
        assert_eq!(tb.decrypt(5, &there).unwrap(), b"there");
        assert_eq!(ta.decrypt(5, &back).unwrap(), b"back");
        assert_eq!(ta.decrypt(5, &there), Err(SessionError::Tampered));
        assert_eq!(tb.decrypt(5, &back), Err(SessionError::Tampered));
    }
}

#[test]
fn a_message_from_another_session_or_from_itself_fails() {
    let alice = Identity::generate();
    let bob = Identity::generate();
    let (mut ta, _) = connected(Kind::Contact, &alice, &bob);
    let (_, mut tb) = connected(Kind::Contact, &alice, &bob);
    let sealed = ta.encrypt(0, b"for the first session").unwrap();
    assert_eq!(tb.decrypt(0, &sealed), Err(SessionError::Tampered));
    assert_eq!(ta.decrypt(0, &sealed), Err(SessionError::Tampered));
}

// ---------------------------------------------------------------------------
// certificates

#[test]
fn a_certificate_is_the_same_bytes_every_time_and_names_its_identity() {
    let alice = Identity::generate();
    let c = certificate(&alice);
    assert_eq!(c.len(), CERTIFICATE_LEN);
    assert_eq!(c, certificate(&alice));
    assert_eq!(c[0..32], alice.public().sign);
    assert_eq!(c[32..64], alice.public().dh);
}

#[test]
fn honest_raw_peers_are_accepted() {
    // The raw peers build exactly what the crate builds, so every rejection
    // below is about the one thing it changes.
    for (kind, raw_role, _) in CERTIFICATE_SENDERS.into_iter().chain(EMPTY_SENDERS) {
        let mut duel = Duel::new(kind, raw_role, &Peer::new(), Lie::None);
        let payloads: Vec<Vec<u8>> = (0..3).map(|s| duel.honest_payload(s)).collect();
        assert_eq!(
            duel.play(|s| payloads[s as usize].clone()),
            Ok(()),
            "{kind:?} raw {raw_role:?}"
        );
        assert!(duel.ours.is_finished());
        // Everyone learns the raw peer's identity except a requester, whose
        // recipient stays anonymous.
        let expected = match (kind, raw_role) {
            (Kind::Request, Role::Responder) => None,
            _ => Some(duel.raw_public.clone()),
        };
        assert_eq!(
            duel.ours.remote_identity(),
            expected,
            "{kind:?} raw {raw_role:?}"
        );
    }
}

#[test]
fn a_certificate_presented_with_another_static_key_fails() {
    // Mallory's real static key, carrying Alice's genuine certificate.
    for (kind, raw_role, step) in CERTIFICATE_SENDERS {
        let alice = Identity::generate();
        let mut duel = Duel::new(kind, raw_role, &Peer::new(), Lie::None);
        let alice_certificate = duel.wrap(step, &certificate(&alice));
        let honest: Vec<Vec<u8>> = (0..3).map(|s| duel.honest_payload(s)).collect();
        let result = duel.play(|s| {
            if s == step {
                alice_certificate.clone()
            } else {
                honest[s as usize].clone()
            }
        });
        assert_eq!(
            result,
            Err(SessionError::KeyMismatch),
            "{kind:?} raw {raw_role:?}"
        );
        assert!(duel.ours.remote_identity().is_none());
        assert_eq!(duel.ours.write_message(), Err(SessionError::Dead));
    }
}

#[test]
fn a_valid_certificate_without_its_static_secret_cannot_complete_a_handshake() {
    // Mallory presents Alice's static public key and Alice's certificate,
    // both genuine, but holds only her own secret.
    for (kind, raw_role, step) in CERTIFICATE_SENDERS {
        let alice = Identity::generate();
        let mut duel = Duel::new(
            kind,
            raw_role,
            &Peer::new(),
            Lie::Impostor(alice.public().dh),
        );
        let alice_certificate = duel.wrap(step, &certificate(&alice));
        let result = duel.play(|s| {
            if s == step {
                alice_certificate.clone()
            } else {
                Vec::new()
            }
        });
        assert_eq!(
            result,
            Err(SessionError::Tampered),
            "{kind:?} raw {raw_role:?}"
        );
        assert!(duel.ours.remote_identity().is_none());
        assert!(!duel.ours.is_finished());
    }
}

#[test]
fn a_certificate_claiming_an_ed25519_key_it_cannot_sign_for_fails() {
    for (kind, raw_role, step) in CERTIFICATE_SENDERS {
        let mallory = Identity::generate();
        let carol = Identity::generate();
        let claimed = PublicIdentity {
            sign: carol.public().sign,
            dh: mallory.public().dh,
        };
        let forged = forged_certificate(&mallory, &claimed);
        let mut duel = Duel::new(kind, raw_role, &Peer::from(mallory), Lie::None);
        let forged = duel.wrap(step, &forged);
        let result = duel.play(|s| {
            if s == step {
                forged.clone()
            } else {
                Vec::new()
            }
        });
        assert_eq!(
            result,
            Err(SessionError::BadSignature),
            "{kind:?} raw {raw_role:?}"
        );
    }
}

#[test]
fn a_certificate_signed_over_another_key_fails() {
    // Mallory signs her own Ed25519 key over a different X25519 key, then
    // relabels the certificate with her real static.
    for (kind, raw_role, step) in CERTIFICATE_SENDERS {
        let mallory = Identity::generate();
        let other = Identity::generate();
        let mut forged = forged_certificate(
            &mallory,
            &PublicIdentity {
                sign: mallory.public().sign,
                dh: other.public().dh,
            },
        );
        forged[32..64].copy_from_slice(&mallory.public().dh);
        let mut duel = Duel::new(kind, raw_role, &Peer::from(mallory), Lie::None);
        let forged = duel.wrap(step, &forged);
        let result = duel.play(|s| {
            if s == step {
                forged.clone()
            } else {
                Vec::new()
            }
        });
        assert_eq!(result, Err(SessionError::BadSignature), "{kind:?}");
    }
}

#[test]
fn a_certificate_of_the_wrong_length_is_malformed() {
    for (kind, raw_role, step) in CERTIFICATE_SENDERS {
        for len in [0usize, 1, 64, 127, 129, 300] {
            let mut duel = Duel::new(kind, raw_role, &Peer::new(), Lie::None);
            let mut c = duel.raw_certificate.to_vec();
            c.resize(len, 0);
            let c = duel.wrap(step, &c);
            let result = duel.play(|s| if s == step { c.clone() } else { Vec::new() });
            assert_eq!(result, Err(SessionError::Malformed), "{kind:?} len {len}");
        }
    }
}

#[test]
fn a_payload_where_none_belongs_is_malformed() {
    for (kind, raw_role, step) in EMPTY_SENDERS {
        let mut duel = Duel::new(kind, raw_role, &Peer::new(), Lie::None);
        let honest: Vec<Vec<u8>> = (0..3).map(|s| duel.honest_payload(s)).collect();
        let result = duel.play(|s| {
            if s == step {
                b"hello".to_vec()
            } else {
                honest[s as usize].clone()
            }
        });
        assert_eq!(
            result,
            Err(SessionError::Malformed),
            "{kind:?} raw {raw_role:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// weak keys

#[test]
fn a_weak_ephemeral_key_is_refused() {
    for kind in KINDS {
        for point in LOW_ORDER {
            // As message 1, to the responder.
            let (_, mut b) = pair(kind, &Identity::generate(), &Identity::generate());
            // Shaped like a real message 1: the bare key, or for a contact
            // the key, the encrypted static and the encrypted hello.
            let mut m1 = point.to_vec();
            m1.resize(if kind == Kind::Contact { 248 } else { 32 }, 0);
            assert_eq!(b.read_message(&m1), Err(SessionError::WeakKey), "{kind:?}");
            assert_eq!(b.write_message(), Err(SessionError::Dead));

            // As message 2, to the initiator.
            let (mut a, mut b) = pair(kind, &Identity::generate(), &Identity::generate());
            b.read_message(&a.write_message().unwrap()).unwrap();
            let mut m2 = b.write_message().unwrap();
            m2[..32].copy_from_slice(&point);
            assert_eq!(a.read_message(&m2), Err(SessionError::WeakKey), "{kind:?}");
            assert!(a.remote_identity().is_none());
        }
    }
}

#[test]
fn a_weak_static_key_is_refused() {
    for (kind, raw_role, _) in CERTIFICATE_SENDERS {
        for point in LOW_ORDER {
            let mut duel = Duel::new(kind, raw_role, &Peer::new(), Lie::LowOrder(point));
            let result = duel.play(|s| {
                if kind.sends_static(s) {
                    vec![0; CERTIFICATE_LEN]
                } else {
                    Vec::new()
                }
            });
            assert_eq!(
                result,
                Err(SessionError::WeakKey),
                "{kind:?} raw {raw_role:?}"
            );
            assert!(duel.ours.remote_identity().is_none());
        }
    }
}

#[test]
fn a_contact_with_a_weak_key_is_never_dialled() {
    for point in LOW_ORDER {
        let them = PublicIdentity {
            sign: Identity::generate().public().sign,
            dh: point,
        };
        let mut a = Session::contact_initiator(
            &SessionKeys::new(&Identity::generate()),
            &them,
            NOW_MS,
            TOKEN,
        );
        assert_eq!(a.write_message(), Err(SessionError::Dead));
        assert!(a.into_transport().is_err());
    }
}

// ---------------------------------------------------------------------------
// the state machine

#[test]
fn steps_out_of_turn_are_refused_without_ending_the_handshake() {
    for kind in KINDS {
        let (mut a, mut b) = pair(kind, &Identity::generate(), &Identity::generate());
        assert_eq!(b.write_message(), Err(SessionError::OutOfTurn));
        assert_eq!(a.read_message(&[0; 32]), Err(SessionError::OutOfTurn));
        assert_eq!(a.into_transport().err(), Some(SessionError::NotFinished));

        let (mut a, mut b) = pair(kind, &Identity::generate(), &Identity::generate());
        let m1 = a.write_message().unwrap();
        assert_eq!(a.write_message(), Err(SessionError::OutOfTurn));
        b.read_message(&m1).unwrap();
        assert_eq!(b.read_message(&m1), Err(SessionError::OutOfTurn));
        let m2 = b.write_message().unwrap();
        a.read_message(&m2).unwrap();
        if kind.messages() == 3 {
            let m3 = a.write_message().unwrap();
            b.read_message(&m3).unwrap();
        }
        assert!(a.is_finished() && b.is_finished(), "{kind:?}");
        assert_eq!(a.write_message(), Err(SessionError::OutOfTurn));
        assert_eq!(b.write_message(), Err(SessionError::OutOfTurn));
        assert_eq!(a.read_message(&m2), Err(SessionError::OutOfTurn));
        assert_eq!(b.read_message(&m1), Err(SessionError::OutOfTurn));
        let (ta, mut tb) = (a.into_transport().unwrap(), b.into_transport().unwrap());
        let sealed = ta.encrypt(0, b"still fine").unwrap();
        assert_eq!(tb.decrypt(0, &sealed).unwrap(), b"still fine");
    }
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
        SessionError::WeakKey,
        SessionError::Stale,
        SessionError::WrongTarget,
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
// the contact hello: time, target, replay cache, confirmation

/// Alice's contact message 1 to Bob, sent at `sent_at` to `token`, read by
/// Bob at `now` with `tokens`.
fn hello(
    sent_at: u64,
    token: [u8; TOKEN_LEN],
    now: u64,
    tokens: &[[u8; TOKEN_LEN]],
) -> Result<(), SessionError> {
    let alice = SessionKeys::new(&Identity::generate());
    let bob = SessionKeys::new(&Identity::generate());
    let mut a = Session::contact_initiator(&alice, bob.public(), sent_at, token);
    let mut b = Session::contact_responder(&bob, now, tokens);
    let m1 = a.write_message().unwrap();
    assert_eq!(m1.len(), 32 + 48 + CONTACT_HELLO_LEN + TAG);
    let result = b.read_message(&m1);
    match result {
        Ok(()) => {
            assert_eq!(b.remote_identity().as_ref(), Some(alice.public()));
            assert_eq!(
                b.remote_ephemeral().as_ref(),
                Some(&m1[..32].try_into().unwrap())
            );
        }
        Err(_) => {
            // Refused silently: nothing to send, nothing learned.
            assert!(b.remote_identity().is_none());
            assert!(b.remote_ephemeral().is_none());
            assert_eq!(b.write_message(), Err(SessionError::Dead));
        }
    }
    result
}

#[test]
fn a_contact_hello_inside_the_window_is_accepted_up_to_its_exact_edges() {
    for offset in [0i64, 1, -1, 119_999, -119_999, 120_000, -120_000] {
        let sent_at = NOW_MS.checked_add_signed(offset).unwrap();
        assert_eq!(
            hello(sent_at, TOKEN, NOW_MS, &MY_TOKENS),
            Ok(()),
            "{offset}"
        );
    }
    assert_eq!(CONTACT_WINDOW_MS, 120_000);
}

#[test]
fn a_stale_contact_hello_is_refused() {
    for offset in [120_001u64, 600_000, NOW_MS] {
        assert_eq!(
            hello(NOW_MS - offset, TOKEN, NOW_MS, &MY_TOKENS),
            Err(SessionError::Stale),
            "{offset} ms old"
        );
    }
}

#[test]
fn a_contact_hello_from_the_future_is_refused() {
    for offset in [120_001u64, 600_000, u64::MAX - NOW_MS] {
        assert_eq!(
            hello(NOW_MS + offset, TOKEN, NOW_MS, &MY_TOKENS),
            Err(SessionError::Stale),
            "{offset} ms ahead"
        );
    }
}

#[test]
fn a_contact_hello_for_another_token_is_refused() {
    // Alice meant it for the device advertising `elsewhere`; a tracker
    // relays it to Bob, whose tokens differ.
    let elsewhere = [0xEE; TOKEN_LEN];
    assert_eq!(
        hello(NOW_MS, elsewhere, NOW_MS, &MY_TOKENS),
        Err(SessionError::WrongTarget)
    );
    // Bob's previous token still counts, so a rotation mid-dial is fine.
    assert_eq!(hello(NOW_MS, PREVIOUS_TOKEN, NOW_MS, &MY_TOKENS), Ok(()));
    // With no tokens, nothing is meant for this device.
    assert_eq!(
        hello(NOW_MS, TOKEN, NOW_MS, &[]),
        Err(SessionError::WrongTarget)
    );
}

#[test]
fn a_hello_with_a_bent_time_or_token_fails_authentication() {
    // The time and token sit at the end of the encrypted hello, just before
    // the tag: flipping any of their bytes in transit breaks the AEAD.
    let alice = SessionKeys::new(&Identity::generate());
    let bob = SessionKeys::new(&Identity::generate());
    let m1 = Session::contact_initiator(&alice, bob.public(), NOW_MS, TOKEN)
        .write_message()
        .unwrap();
    let tail = m1.len() - TAG - 24..m1.len() - TAG;
    for i in tail {
        let mut bent = m1.clone();
        bent[i] ^= 0x01;
        let mut b = Session::contact_responder(&bob, NOW_MS, &MY_TOKENS);
        assert_eq!(
            b.read_message(&bent),
            Err(SessionError::Tampered),
            "byte {i}"
        );
    }
}

#[test]
fn the_remote_ephemeral_is_the_one_the_message_carried() {
    let (mut a, mut b) = pair(Kind::Contact, &Identity::generate(), &Identity::generate());
    let m1 = a.write_message().unwrap();
    assert_eq!(b.remote_ephemeral(), None);
    b.read_message(&m1).unwrap();
    assert_eq!(b.remote_ephemeral(), Some(m1[..32].try_into().unwrap()));
    let m2 = b.write_message().unwrap();
    a.read_message(&m2).unwrap();
    assert_eq!(a.remote_ephemeral(), Some(m2[..32].try_into().unwrap()));
    // A replay inside the window reads again, with the same ephemeral: the
    // link layer's cache is what refuses it.
    let bob_keys = SessionKeys::new(&Identity::generate());
    let alice_keys = SessionKeys::new(&Identity::generate());
    let m1 = Session::contact_initiator(&alice_keys, bob_keys.public(), NOW_MS, TOKEN)
        .write_message()
        .unwrap();
    let mut first = Session::contact_responder(&bob_keys, NOW_MS, &MY_TOKENS);
    let mut again = Session::contact_responder(&bob_keys, NOW_MS + 5_000, &MY_TOKENS);
    first.read_message(&m1).unwrap();
    again.read_message(&m1).unwrap();
    assert_eq!(first.remote_ephemeral(), again.remote_ephemeral());
}

#[test]
fn only_the_contact_responder_starts_unconfirmed() {
    for kind in KINDS {
        let (ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        assert!(ta.confirmed(), "{kind:?} initiator");
        assert_eq!(tb.confirmed(), kind != Kind::Contact, "{kind:?} responder");
        // A message that fails to decrypt confirms nothing.
        assert!(tb.decrypt(0, &[0x5a; TAG + 4]).is_err());
        assert_eq!(tb.confirmed(), kind != Kind::Contact);
        // The first genuine message from the initiator does.
        let sealed = ta.encrypt(0, b"here").unwrap();
        tb.decrypt(0, &sealed).unwrap();
        assert!(tb.confirmed());
        // Sending confirms nothing on the sender's side; it stays as it was.
        ta.encrypt(0, b"more").unwrap();
        assert!(ta.confirmed());
    }
}

#[test]
fn a_replayed_contact_hello_never_confirms() {
    // A replayer gets Bob to answer, but can't produce a transport message,
    // so Bob's session never confirms Alice.
    let alice = SessionKeys::new(&Identity::generate());
    let bob = SessionKeys::new(&Identity::generate());
    let m1 = Session::contact_initiator(&alice, bob.public(), NOW_MS, TOKEN)
        .write_message()
        .unwrap();
    let mut b = Session::contact_responder(&bob, NOW_MS, &MY_TOKENS);
    b.read_message(&m1).unwrap();
    b.write_message().unwrap();
    let mut tb = b.into_transport().unwrap();
    assert_eq!(tb.remote_identity(), Some(alice.public()));
    assert!(!tb.confirmed());
    let mut rng = Rng(0xC0F1);
    for _ in 0..200 {
        let len = rng.below(64) + TAG;
        assert!(tb.decrypt(0, &rng.bytes(len)).is_err());
    }
    assert!(!tb.confirmed());
}

// ---------------------------------------------------------------------------
// accepting a request: the recipient proves itself with a contact handshake

#[test]
fn an_accepted_request_is_answered_with_a_contact_handshake() {
    let alice = SessionKeys::new(&Identity::generate());
    let bob = SessionKeys::new(&Identity::generate());

    // Alice asks; Bob, anonymous, learns who she is.
    let mut a = Session::request_initiator(&alice);
    let mut b = Session::request_responder();
    run(Kind::Request, &mut a, &mut b);
    let asker = b.remote_identity().unwrap();
    assert_eq!(&asker, alice.public());

    // Bob accepts by dialling her as a contact; she learns it is Bob.
    let alice_tokens = [[0xA1; TOKEN_LEN]];
    let mut bob_dials = Session::contact_initiator(&bob, &asker, NOW_MS, alice_tokens[0]);
    let mut alice_answers = Session::contact_responder(&alice, NOW_MS, &alice_tokens);
    alice_answers
        .read_message(&bob_dials.write_message().unwrap())
        .unwrap();
    assert_eq!(alice_answers.remote_identity().as_ref(), Some(bob.public()));
    bob_dials
        .read_message(&alice_answers.write_message().unwrap())
        .unwrap();
    assert!(bob_dials.is_finished() && alice_answers.is_finished());
}

#[test]
fn holding_only_bobs_certificate_is_not_enough_to_be_bob() {
    let bob = Identity::generate();
    let bob_certificate = certificate(&bob);

    // As Bob dialling Alice: Mallory presents Bob's static key and his
    // genuine certificate in a well-formed hello, but holds only her own
    // secret.
    let mut duel = Duel::new(
        Kind::Contact,
        Role::Initiator,
        &Peer::new(),
        Lie::Impostor(bob.public().dh),
    );
    let hello = duel.wrap(0, &bob_certificate);
    assert_eq!(duel.play(|_| hello.clone()), Err(SessionError::Tampered));
    assert!(duel.ours.remote_identity().is_none());

    // As Bob answering Alice's dial: Mallory's own keys can't read a
    // message 1 encrypted to Bob, so she has nothing to answer.
    let alice = SessionKeys::new(&Identity::generate());
    let mallory = SessionKeys::new(&Identity::generate());
    let mut a = Session::contact_initiator(&alice, &bob.public(), NOW_MS, TOKEN);
    let mut m = Session::contact_responder(&mallory, NOW_MS, &MY_TOKENS);
    assert_eq!(
        m.read_message(&a.write_message().unwrap()),
        Err(SessionError::Tampered)
    );
    assert!(!a.is_finished());
}
