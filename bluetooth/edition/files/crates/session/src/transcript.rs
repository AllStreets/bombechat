//! A pinned transcript: fixed identities and fixed ephemeral keys make the
//! whole handshake deterministic (Ed25519 signatures are too), so its bytes
//! and handshake hash are pinned here. Any change to the pattern, the proof
//! layout, the signing context or the order of things fails this test.
//!
//! The bytes were recorded from this implementation, so this is a guard
//! against drift, not an independent check. The independent check is
//! `noise_xx_matches_the_cacophony_vector` in tests/session.rs, which runs
//! snow against the published vector for the pattern.

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

const ALICE_EPHEMERAL: [u8; 32] = [0x11; 32];
const BOB_EPHEMERAL: [u8; 32] = [0x22; 32];

const MESSAGE_1: &str = "7b4e909bbe7ffe44c465a220037d608ee35897d31ef972f07f74892cb0f73f13";
const MESSAGE_2: &str = concat!(
    "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20",
    "7a5f8df75ecfc882119296103867ce81c17788c1a17347e95ce91f1c7eae2d91",
    "f8ed34dc0fcb4802bd86a745706c1edd91f83f953703baaf8adaab248fe7be95",
    "0f9f23debdd7ff236ff9fa94f41a83672028a2c23f5e1171a7d65e580461febc",
    "461f06ad8c95afdd553a1e91988f3c45e6b0f283f424116aed63a55ed7a00ac8",
    "b86562668655e326604b725037d77ecd0302c81dec42bf4e11d3d01af0095201",
    "29c144ffb944d9fa4326da9beb928815b9f74a5e7a16ef1b9519e6d487a7b9cd",
);
const MESSAGE_3: &str = concat!(
    "39702b0092ec4bf8dd12645acbaca052d02fa2909fe20ead1a1d5a324ab29059",
    "10e450983c0dd678c2eb04ecea3c49afabc13ec0561b591d5c2da98b48564fc3",
    "49f403737820167cca3b842e80afa094bf845ccfaf2bf30587382de80b1eb1fd",
    "81a744d99c95743f48b5c8b011d8135ba5292d9827c390b289319211ada38197",
    "aa4b832ffce995c5681f1b9a1dd00b9295d30af8bf468300b7f619eb99eea5d5",
    "c6ee9acf2fd14cd85701f36eb8f9c3958cd11622cf87422b06ca271cb4fe5d28",
);
const HANDSHAKE_HASH: &str = "a4b488faacb451e50cdcaf9849f486f827c3d154af19dc310fa5531cdd1657f4";
const FIRST_TRANSPORT: &str = "1d60fc4f3449dfb73998bef75f3b7a2cf2eb10ab4a";

#[test]
fn a_fixed_handshake_produces_the_pinned_transcript() {
    let alice = identity(0x40);
    let bob = identity(0x80);
    let mut a = Handshake::new(
        &alice,
        Role::Initiator,
        Some(bob.public()),
        Some(&ALICE_EPHEMERAL),
    );
    let mut b = Handshake::new(&bob, Role::Responder, None, Some(&BOB_EPHEMERAL));

    let m1 = a.write_message().unwrap();
    b.read_message(&m1).unwrap();
    let m2 = b.write_message().unwrap();
    a.read_message(&m2).unwrap();
    let m3 = a.write_message().unwrap();
    b.read_message(&m3).unwrap();

    let hash_a = hex(a.noise.as_ref().unwrap().get_handshake_hash());
    let hash_b = hex(b.noise.as_ref().unwrap().get_handshake_hash());
    assert_eq!(hash_a, hash_b);

    let mut ta = a.into_transport().unwrap();
    let first = ta.encrypt(b"hello").unwrap();

    assert_eq!(hex(&m1), MESSAGE_1);
    assert_eq!(hex(&m2), MESSAGE_2);
    assert_eq!(hex(&m3), MESSAGE_3);
    assert_eq!(hash_a, HANDSHAKE_HASH);
    assert_eq!(hex(&first), FIRST_TRANSPORT);
}
