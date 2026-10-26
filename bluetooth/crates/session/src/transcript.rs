//! Pinned transcripts: fixed identities and fixed ephemeral keys make each
//! handshake deterministic (Ed25519 signatures are too), so every message,
//! the handshake hash and a first transport message in each direction are
//! pinned here, one transcript per kind of session. Any change to a
//! pattern, the prologue, the certificate or the order of things fails.
//!
//! The bytes were recorded from this implementation, so this is a guard
//! against drift, not an independent check. The independent checks are the
//! cacophony vectors in tests/vectors.rs, which run snow against the
//! published vectors for all three patterns.

use super::*;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn identity(fill: u8) -> Identity {
    let mut secret = [0u8; 64];
    for (i, b) in secret.iter_mut().enumerate() {
        *b = fill.wrapping_add(i as u8);
    }
    Identity::from_secret_bytes(&secret)
}

/// The contact hello's fixed time and target token.
const NOW_MS: u64 = 1_800_000_000_000;
const TOKEN: [u8; TOKEN_LEN] = [0x7b; TOKEN_LEN];

const INITIATOR_EPHEMERAL: [u8; 32] = [0x11; 32];
const RESPONDER_EPHEMERAL: [u8; 32] = [0x22; 32];

/// Run a whole handshake with fixed keys and return every message, the
/// handshake hash and one transport message each way, all as hex.
fn transcript(kind: Kind) -> Vec<String> {
    let alice = SessionKeys::new(&identity(0x40));
    let bob = SessionKeys::new(&identity(0x80));
    let (expected, responder_me) = match kind {
        Kind::Contact => (Some(bob.public().clone()), Some(&bob)),
        Kind::Request => (None, None),
        Kind::Bridge => (Some(bob.public().clone()), Some(&bob)),
    };
    let mut a = Handshake::new(
        kind,
        Role::Initiator,
        Some(&alice),
        expected,
        if kind == Kind::Contact {
            Freshness::Send {
                now_ms: NOW_MS,
                token: TOKEN,
            }
        } else {
            Freshness::None
        },
        Some(&INITIATOR_EPHEMERAL),
    );
    let mut b = Handshake::new(
        kind,
        Role::Responder,
        responder_me,
        None,
        if kind == Kind::Contact {
            Freshness::Check {
                now_ms: NOW_MS + 1_000,
                tokens: vec![TOKEN],
            }
        } else {
            Freshness::None
        },
        Some(&RESPONDER_EPHEMERAL),
    );

    let mut out = Vec::new();
    for step in 0..kind.messages() {
        let (writer, reader) = if step.is_multiple_of(2) {
            (&mut a, &mut b)
        } else {
            (&mut b, &mut a)
        };
        let m = writer.write_message().unwrap();
        reader.read_message(&m).unwrap();
        out.push(hex(&m));
    }
    assert!(a.is_finished() && b.is_finished());
    assert_eq!(b.remote_identity().as_ref(), Some(alice.public()));

    let hash_a = hex(a.noise.as_ref().unwrap().get_handshake_hash());
    let hash_b = hex(b.noise.as_ref().unwrap().get_handshake_hash());
    assert_eq!(hash_a, hash_b);
    out.push(hash_a);

    let mut ta = a.into_transport().unwrap();
    let mut tb = b.into_transport().unwrap();
    let there = ta.encrypt(0, b"hello").unwrap();
    assert_eq!(tb.decrypt(0, &there).unwrap(), b"hello");
    let back = tb.encrypt(0, b"hi").unwrap();
    assert_eq!(ta.decrypt(0, &back).unwrap(), b"hi");
    out.push(hex(&there));
    out.push(hex(&back));
    out
}

fn check(kind: Kind, pinned: &[&str]) {
    let got = transcript(kind);
    for (i, line) in got.iter().enumerate() {
        println!("{kind:?} {i} {line}");
    }
    assert_eq!(got.len(), pinned.len());
    for (i, (got, pinned)) in got.iter().zip(pinned).enumerate() {
        assert_eq!(got, pinned, "{kind:?} line {i}");
    }
}

#[test]
fn a_fixed_contact_handshake_produces_the_pinned_transcript() {
    check(Kind::Contact, &CONTACT);
}

#[test]
fn a_fixed_request_handshake_produces_the_pinned_transcript() {
    check(Kind::Request, &REQUEST);
}

#[test]
fn a_fixed_bridge_handshake_produces_the_pinned_transcript() {
    check(Kind::Bridge, &BRIDGE);
}

const CONTACT: [&str; 5] = [
    concat!(
        "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13",
        "8e753ea12ef63311734247f8e3e5a9320321b16a033f3b716e2dd24759402c70",
        "20f1e9ad17de75b908fe2d1cefa1c220413d9dfb80805d2fbad978cb3e957702",
        "12406644f2ed9198f3ed2733ca739c09bf007707495feabe284d4d3cd10a51a5",
        "48294bf28f8b5cd0e59329d8f7cef8b0186269a4021887be03647d84d5350e0f",
        "d37e7980dd9499d3626f96ab02b0c453c83ba3fee3a1d4fbccf0ca7d5d6888ea",
        "db86502da3ee52808431ea09763ef80e76e2e1c2a5e0e21f272770c2617d7ec5",
        "6759f0f5c1785389d0f64e5d807f42439d2fe81967ea8881",
    ),
    concat!(
        "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20",
        "55f72752313b77091cc0fab360c1f661",
    ),
    "004adff48b16bd9a2c12ae676127a3ef0628598f40205721a125dc6a2b5e7356",
    "1d5f4e25eb6fc02f43200b2fa39f5d5ec8d066acec",
    "6678233759750379819cdd737bb6eaa98945",
];

const REQUEST: [&str; 6] = [
    "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13",
    concat!(
        "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20",
        "84646f7e2220ca4ea42252090d246508",
    ),
    concat!(
        "549a4a315f4ddf6fc0316080cdda9dd2dd3f7d3eb90acf7301bd001c3985ac38",
        "7bcf6206cefc59a9989a577cbd68215ecdcea021f6a4c5ea3896e5bd55b4851f",
        "6a34ad20b475f0d230aa6a043ad2c79ee19dd984d4ffba7565fb8e128be542c8",
        "1e1444b03e9b4fc6200093ee7e690a9cc235c78b33e62185d4a2de12610fa523",
        "f575aa18d279eb6c7a1a9f4b8047616a3b932f9f1552102e6c601a5e8f859c14",
        "8cc87346f89d9dab54adc3b7c4701152e84589f24b5411119d66ed7398b60475",
    ),
    "a393e9d3f4938c4c1eefda715829a7c309c0d3b6d9687c4ab9a70124e20b012c",
    "785d9e08b09c108f69cff4c910d4091ab32048d1a6",
    "987cc9cc0ff147ee472e56506ac54fcf50a6",
];

const BRIDGE: [&str; 6] = [
    "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13",
    concat!(
        "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20",
        "7a5f8df75ecfc882119296103867ce81c17788c1a17347e95ce91f1c7eae2d91",
        "f8ed34dc0fcb4802bd86a745706c1edd91f83f953703baaf8adaab248fe7be95",
        "0f9f23debdd7ff236ff9fa94f41a83672028a2c23f5e1171a7d65e580461febc",
        "461f06ad8c95afdd553a1e91988f3c454a4ea90819893cf08e3f111be9665190",
        "ea9ded481b0d32f369564056d5d6e16f2b0462777a126e6581442c55ce0d5cac",
        "7137c76f3df8f7714accb726613f2810c7b3161d57f944f429863b7ea57f0cbd",
    ),
    concat!(
        "39702b0092ec4bf8dd12645acbaca052d02fa2909fe20ead1a1d5a324ab29059",
        "c90c2df9c9c14c96041cbb9ff8b86bc9abc13ec0561b591d5c2da98b48564fc3",
        "49f403737820167cca3b842e80afa094bf845ccfaf2bf30587382de80b1eb1fd",
        "81a744d99c95743f48b5c8b011d8135bead2c0a7d34c67b7863cb8fb63aa3a6f",
        "cd387f0730d6f2e1811071c79ba01cc0483755460af9ab08a8e8b06ea308b403",
        "0c9225d74c235ff27e4164259def4495f361ba7cb500ce108790e21d168f9dcc",
    ),
    "8b7367bd17309f83b8b47bd4598b51a93fc27aa4b37dd1ee506d2dfc5a578ff0",
    "1d60fc4f3449dfb73998bef75f3b7a2cf2eb10ab4a",
    "eb26488ecc7fb658a9a47fd22227faf5ef42",
];
