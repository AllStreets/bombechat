//! Fuzz-style loops, seeded so every run is the same: random and near-miss
//! bytes at every handshake reader of every kind, at transport decrypt,
//! and, through a raw snow peer, random payloads and certificates that
//! decrypt fine and so reach the certificate parser. Nothing may panic, and
//! every failure must be one of the errors that reader can give.

mod common;

use bombechat_core::{Identity, PublicIdentity};
use bombechat_session::{
    certificate, Handshake, SessionError, SessionKeys, CERTIFICATE_LEN, CONTACT_WINDOW_MS,
};
use common::*;

const ROUNDS: usize = 10_000;

/// Our side of `kind` playing `role`, advanced to just before it reads
/// message `step`, plus a genuine message `step` for it.
fn ready_to_read(
    kind: Kind,
    role: Role,
    step: u8,
    alice: &SessionKeys,
    bob: &SessionKeys,
) -> (Handshake, Vec<u8>) {
    let (mut a, mut b) = pair_keys(kind, alice, bob);
    let mut valid = Vec::new();
    for s in 0..=step {
        let (w, r) = if s.is_multiple_of(2) {
            (&mut a, &mut b)
        } else {
            (&mut b, &mut a)
        };
        let m = w.write_message().unwrap();
        if s == step {
            valid = m;
        } else {
            r.read_message(&m).unwrap();
        }
    }
    let reader = if role == Role::Initiator { a } else { b };
    (reader, valid)
}

/// Every handshake reader: (kind, the reader's role, the message it reads).
const READERS: [(Kind, Role, u8); 8] = [
    (Kind::Contact, Role::Responder, 0),
    (Kind::Contact, Role::Initiator, 1),
    (Kind::Request, Role::Responder, 0),
    (Kind::Request, Role::Initiator, 1),
    (Kind::Request, Role::Responder, 2),
    (Kind::Bridge, Role::Responder, 0),
    (Kind::Bridge, Role::Initiator, 1),
    (Kind::Bridge, Role::Responder, 2),
];

/// Throw random and near-miss bytes at reader `n` of [`READERS`].
fn fuzz_reader(n: usize) {
    let alice = SessionKeys::new(&Identity::generate());
    let bob = SessionKeys::new(&Identity::generate());
    let (kind, role, step) = READERS[n];
    {
        let mut rng = Rng(0x5E55_1000 + n as u64);
        // A genuine message from another run of the same handshake: the
        // near misses are shaped right but never decrypt.
        let (_, valid) = ready_to_read(kind, role, step, &alice, &bob);
        let mut accepted = 0;
        for _ in 0..ROUNDS {
            // A failed read ends a handshake, so each round needs a fresh one.
            // Message 1's reader needs nothing before it; later readers
            // need the handshake played up to their message.
            let mut reader = if step == 0 {
                ours(kind, Role::Responder, &bob, None)
            } else {
                ready_to_read(kind, role, step, &alice, &bob).0
            };
            let input = rng.input(&valid);
            if input == valid {
                // A replayed contact message 1 reads fine (IK can't tell a
                // replay at message 1); its answer is useless to whoever
                // replayed it, who lacks the initiator's ephemeral secret.
                continue;
            }
            match reader.read_message(&input) {
                // Any 32 bytes are an XX or XN message 1: Noise can't tell.
                Ok(()) => {
                    assert!(
                        step == 0 && kind != Kind::Contact && input.len() == 32,
                        "{kind:?} {role:?} accepted {} bytes",
                        input.len()
                    );
                    accepted += 1;
                }
                Err(e) => {
                    assert!(
                        matches!(
                            e,
                            SessionError::Malformed
                                | SessionError::Tampered
                                | SessionError::WeakKey
                        ),
                        "{kind:?} {role:?} message {step}: {e:?}"
                    );
                    assert!(reader.remote_identity().is_none());
                }
            }
        }
        if step == 0 && kind != Kind::Contact {
            assert!(accepted > 0, "{kind:?}");
        }
    }
}

// One test per reader, so they run in parallel.

#[test]
fn random_bytes_at_the_contact_responder_reading_message_1_never_panic() {
    fuzz_reader(0);
}

#[test]
fn random_bytes_at_the_contact_initiator_reading_message_2_never_panic() {
    fuzz_reader(1);
}

#[test]
fn random_bytes_at_the_request_recipient_reading_message_1_never_panic() {
    fuzz_reader(2);
}

#[test]
fn random_bytes_at_the_requester_reading_message_2_never_panic() {
    fuzz_reader(3);
}

#[test]
fn random_bytes_at_the_request_recipient_reading_message_3_never_panic() {
    fuzz_reader(4);
}

#[test]
fn random_bytes_at_the_bridge_responder_reading_message_1_never_panic() {
    fuzz_reader(5);
}

#[test]
fn random_bytes_at_the_bridge_initiator_reading_message_2_never_panic() {
    fuzz_reader(6);
}

#[test]
fn random_bytes_at_the_bridge_responder_reading_message_3_never_panic() {
    fuzz_reader(7);
}

#[test]
fn random_bytes_at_transport_decrypt_never_panic() {
    for (n, kind) in KINDS.into_iter().enumerate() {
        let (ta, mut tb) = connected(kind, &Identity::generate(), &Identity::generate());
        let valid = ta.encrypt(0, b"the one real message").unwrap();
        let mut rng = Rng(0x5E55_2000 + n as u64);
        for _ in 0..ROUNDS {
            let input = rng.input(&valid);
            if input == valid {
                continue;
            }
            let e = tb.decrypt(0, &input).unwrap_err();
            assert!(
                matches!(e, SessionError::Malformed | SessionError::Tampered),
                "{e:?}"
            );
        }
        // None of that moved the receive counter.
        assert_eq!(tb.decrypt(0, &valid).unwrap(), b"the one real message");
    }
}

#[test]
fn random_payloads_and_certificates_from_a_raw_peer_never_panic() {
    let mut rng = Rng(0x5E55_3000);
    let senders: Vec<(Kind, Role, u8, bool)> = CERTIFICATE_SENDERS
        .iter()
        .map(|&(k, r, s)| (k, r, s, true))
        .chain(EMPTY_SENDERS.iter().map(|&(k, r, s)| (k, r, s, false)))
        .collect();
    let others: Vec<[u8; CERTIFICATE_LEN]> =
        (0..4).map(|_| certificate(&Identity::generate())).collect();
    let peers: Vec<Peer> = (0..8).map(|_| Peer::new()).collect();
    let mut accepted = 0;
    for round in 0..ROUNDS {
        let (kind, raw_role, step, carries) = senders[round % senders.len()];
        let peer = &peers[rng.below(peers.len())];
        let mut duel = Duel::new(kind, raw_role, peer, Lie::None);
        let mine = duel.raw_certificate;
        let mut alternative: Option<PublicIdentity> = None;
        let payload = match rng.below(6) {
            // Random bytes of any length a handshake payload might have.
            0 => {
                let len = rng.below(300);
                rng.bytes(len)
            }
            // The right certificate, bent.
            1 => duel.wrap(step, &rng.input(&mine)),
            // Someone else's certificate, whole or bent.
            2 => {
                let other = others[rng.below(others.len())];
                if rng.below(2) == 0 {
                    duel.wrap(step, &other)
                } else {
                    duel.wrap(step, &rng.input(&other))
                }
            }
            // A certificate naming this static, signed by another key. If
            // it names that key too, it is a valid certificate for a
            // different identity that shares this static, and the peer,
            // who holds the static, may present it.
            3 => {
                let signer = Identity::generate();
                let own_key = rng.below(2) == 0;
                let claimed = PublicIdentity {
                    sign: if own_key {
                        signer.public().sign
                    } else {
                        rng.bytes(32).try_into().unwrap()
                    },
                    dh: duel.raw_public.dh,
                };
                if own_key {
                    alternative = Some(claimed.clone());
                }
                duel.wrap(step, &forged_certificate(&signer, &claimed))
            }
            // The genuine payload with its hello tail (if any) bent or
            // replaced: other times, other tokens.
            4 => {
                let mut p = duel.honest_payload(step);
                if p.len() > CERTIFICATE_LEN {
                    let tail = p.split_off(CERTIFICATE_LEN);
                    p.extend(if rng.below(2) == 0 {
                        rng.input(&tail)
                    } else {
                        rng.bytes(tail.len())
                    });
                }
                p
            }
            // The genuine payload, now and then, so the loop knows the
            // path to acceptance is still open.
            _ => duel.honest_payload(step),
        };
        let honest: Vec<Vec<u8>> = (0..3).map(|s| duel.honest_payload(s)).collect();
        let result = duel.play(|s| {
            if s == step {
                payload.clone()
            } else {
                honest[s as usize].clone()
            }
        });
        match result {
            Ok(()) => {
                let proved = duel.ours.remote_identity();
                // A contact hello's tail is acceptable if it is the right
                // length, inside the window and for one of the tokens.
                let tail_ok = kind != Kind::Contact || {
                    let tail = payload.get(CERTIFICATE_LEN..).unwrap_or(&[]);
                    tail.len() == 24
                        && u64::from_be_bytes(tail[..8].try_into().unwrap()).abs_diff(NOW_MS)
                            <= CONTACT_WINDOW_MS
                        && MY_TOKENS.iter().any(|t| t[..] == tail[8..])
                };
                let cert = payload.get(..CERTIFICATE_LEN).unwrap_or(&[]);
                let fine = if !carries {
                    payload.is_empty()
                } else if cert == mine {
                    tail_ok && proved == Some(duel.raw_public.clone())
                } else {
                    tail_ok && alternative.is_some() && proved == alternative
                };
                assert!(
                    fine,
                    "{kind:?} raw {raw_role:?} step {step} accepted {} bytes",
                    payload.len()
                );
                accepted += 1;
            }
            Err(e) => {
                assert!(
                    matches!(
                        e,
                        SessionError::Malformed
                            | SessionError::KeyMismatch
                            | SessionError::BadSignature
                            | SessionError::Stale
                            | SessionError::WrongTarget
                    ),
                    "{kind:?} raw {raw_role:?}: {e:?}"
                );
                assert!(duel.ours.remote_identity().is_none());
            }
        }
    }
    assert!(accepted > 0);
}
