//! Known answers: snow, built with this crate's features, against the
//! cacophony test vectors for all three patterns, as snow 0.10.0 ships
//! them in tests/vectors/cacophony.txt. They prove the library linked here
//! computes the standard patterns byte for byte. (The vectors use their own
//! prologue, "John Galt"; ours, "bombechat v1", is pinned by the
//! transcript tests in src/transcript.rs.)

mod common;

use bombechat_session::{BRIDGE_PATTERN, CONTACT_PATTERN, MAX_MESSAGE, REQUEST_PATTERN};
use common::{raw_read, raw_write};

struct Vector {
    protocol: &'static str,
    prologue: &'static str,
    init_static: Option<&'static str>,
    init_ephemeral: &'static str,
    init_remote_static: Option<&'static str>,
    resp_static: Option<&'static str>,
    resp_ephemeral: &'static str,
    handshake_hash: &'static str,
    /// (payload, ciphertext), alternating from the initiator: the handshake
    /// messages, then transport messages.
    messages: &'static [(&'static str, &'static str)],
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn check(v: &Vector, handshake_messages: usize) {
    let prologue = unhex(v.prologue);
    let side = |s: Option<&str>, e: &str, rs: Option<&str>, initiator: bool| {
        let (s, e, rs) = (s.map(unhex), unhex(e), rs.map(unhex));
        let mut b = snow::Builder::new(v.protocol.parse().unwrap())
            .fixed_ephemeral_key_for_testing_only(&e)
            .prologue(&prologue)
            .unwrap();
        if let Some(s) = &s {
            b = b.local_private_key(s).unwrap();
        }
        if let Some(rs) = &rs {
            b = b.remote_public_key(rs).unwrap();
        }
        if initiator {
            b.build_initiator().unwrap()
        } else {
            b.build_responder().unwrap()
        }
    };
    let mut init = side(v.init_static, v.init_ephemeral, v.init_remote_static, true);
    let mut resp = side(v.resp_static, v.resp_ephemeral, None, false);

    for (i, (payload, ciphertext)) in v.messages[..handshake_messages].iter().enumerate() {
        let (w, r) = if i.is_multiple_of(2) {
            (&mut init, &mut resp)
        } else {
            (&mut resp, &mut init)
        };
        let sent = raw_write(w, &unhex(payload));
        assert_eq!(
            sent,
            unhex(ciphertext),
            "{} handshake message {i}",
            v.protocol
        );
        assert_eq!(raw_read(r, &sent).unwrap(), unhex(payload));
    }
    let hash = unhex(v.handshake_hash);
    assert_eq!(init.get_handshake_hash(), hash.as_slice(), "{}", v.protocol);
    assert_eq!(resp.get_handshake_hash(), hash.as_slice(), "{}", v.protocol);

    let mut init = init.into_transport_mode().unwrap();
    let mut resp = resp.into_transport_mode().unwrap();
    for (i, (payload, ciphertext)) in v.messages.iter().enumerate().skip(handshake_messages) {
        let plain = unhex(payload);
        let mut out = vec![0u8; MAX_MESSAGE];
        let mut back = vec![0u8; MAX_MESSAGE];
        let (n, m) = if i.is_multiple_of(2) {
            let n = init.write_message(&plain, &mut out).unwrap();
            (n, resp.read_message(&out[..n], &mut back).unwrap())
        } else {
            let n = resp.write_message(&plain, &mut out).unwrap();
            (n, init.read_message(&out[..n], &mut back).unwrap())
        };
        assert_eq!(
            &out[..n],
            unhex(ciphertext).as_slice(),
            "{} message {i}",
            v.protocol
        );
        assert_eq!(&back[..m], plain.as_slice());
    }
}

#[test]
fn noise_ik_matches_the_cacophony_vector() {
    assert_eq!(CONTACT_VECTOR.protocol, CONTACT_PATTERN);
    check(&CONTACT_VECTOR, 2);
}

#[test]
fn noise_xn_matches_the_cacophony_vector() {
    assert_eq!(REQUEST_VECTOR.protocol, REQUEST_PATTERN);
    check(&REQUEST_VECTOR, 3);
}

#[test]
fn noise_xx_matches_the_cacophony_vector() {
    assert_eq!(BRIDGE_VECTOR.protocol, BRIDGE_PATTERN);
    check(&BRIDGE_VECTOR, 3);
}

const BRIDGE_VECTOR: Vector = Vector {
    protocol: "Noise_XX_25519_ChaChaPoly_SHA256",
    prologue: "4a6f686e2047616c74",
    init_static: Some("e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1"),
    init_ephemeral: "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a",
    init_remote_static: None,
    resp_static: Some("4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893"),
    resp_ephemeral: "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b",
    handshake_hash: "c8e5f64e846193be2a834104c2a009868d6c9f3bd3c186299888b488b2f1f58e",
    messages: &[
        (
            "4c756477696720766f6e204d69736573",
            concat!(
                "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944",
                "4c756477696720766f6e204d69736573",
            ),
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
            concat!(
                "3e40f15f6f3a46ae446b253bf8b1d9ffb6ed9b174d272328ff91a7e2e5c79c07",
                "f5",
            ),
        ),
        (
            "457567656e2042f6686d20766f6e2042617765726b",
            concat!(
                "eb3f3515110702e047a6c9da4478b6ead94873c11c0f2d710ddb3f09fce024b3",
                "a58502ae3f",
            ),
        ),
    ],
};

const CONTACT_VECTOR: Vector = Vector {
    protocol: "Noise_IK_25519_ChaChaPoly_SHA256",
    prologue: "4a6f686e2047616c74",
    init_static: Some("e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1"),
    init_ephemeral: "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a",
    init_remote_static: Some("31e0303fd6418d2f8c0e78b91f22e8caed0fbe48656dcf4767e4834f701b8f62"),
    resp_static: Some("4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893"),
    resp_ephemeral: "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b",
    handshake_hash: "0b0f68fb0c27e03ce9b97565995ed4838cc0581b762ef72b062f6a546419fad7",
    messages: &[
        (
            "4c756477696720766f6e204d69736573",
            concat!(
                "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944",
                "718da798efbcd91528520204f904b9bd6c7413dccdc214d951e15253e39987f1",
                "8146e8cd0873654207148333479d4d16c289f0294b29960a72f48e0b7bba2e89",
                "083169825e59642148d492020664ccf7",
            ),
        ),
        (
            "4d757272617920526f746862617264",
            concat!(
                "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843",
                "5361e70b2ed446e6c9ec387d1d6b3b840f194e373979d241b203c4acafccf5",
            ),
        ),
        (
            "462e20412e20486179656b",
            "050e9f3c8fac16b68dbce8f8c4bfbf6617c897f9ada4aa29aa19c8",
        ),
        (
            "4361726c204d656e676572",
            "344233a6cabb7141d80f3da2fedc311d9646bbb0f505afe403a667",
        ),
        (
            "4a65616e2d426170746973746520536179",
            concat!(
                "62cdeeb172ad7ade7aa7d9e069da5790f12331bfa00177787a1d0810c67dc3b2",
                "b4",
            ),
        ),
        (
            "457567656e2042f6686d20766f6e2042617765726b",
            concat!(
                "029bead1b40992327044d409d9a1f3ad8f36c3c452775d557e18bbeb2e8dfcea",
                "d32d514024",
            ),
        ),
    ],
};

const REQUEST_VECTOR: Vector = Vector {
    protocol: "Noise_XN_25519_ChaChaPoly_SHA256",
    prologue: "4a6f686e2047616c74",
    init_static: Some("e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1"),
    init_ephemeral: "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a",
    init_remote_static: None,
    resp_static: None,
    resp_ephemeral: "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b",
    handshake_hash: "3e9a5237b8680385267a50da8ecaa453d59509e21cc4f392988514d182a63691",
    messages: &[
        (
            "4c756477696720766f6e204d69736573",
            concat!(
                "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944",
                "4c756477696720766f6e204d69736573",
            ),
        ),
        (
            "4d757272617920526f746862617264",
            concat!(
                "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843",
                "cede969108db1d801a3c5550fcd4a68b48f7e29e56d7806723fcb465f91e89",
            ),
        ),
        (
            "462e20412e20486179656b",
            concat!(
                "f8332c0aa6726115565aea0afc6d28890e24fadd512e60c9d8ea2c22e87f276f",
                "56a236002bbb58d0a1ead5ad40c262ab2bd138391cef42ef97b500cd5c745cce",
                "1e25f2420809dead4e6f28",
            ),
        ),
        (
            "4361726c204d656e676572",
            "05173034244d88ec53f37457e682743786d461c1f40ebeba92503f",
        ),
        (
            "4a65616e2d426170746973746520536179",
            concat!(
                "e3f9c0732abc45f4c544246545d68248db15f3810a155901076e16ca135dadff",
                "df",
            ),
        ),
        (
            "457567656e2042f6686d20766f6e2042617765726b",
            concat!(
                "f5ee4ab80ee7539f4c4b168c70ca31f1113f53e38cddc59ed93d4c2152e682af",
                "d177f39a91",
            ),
        ),
    ],
};
