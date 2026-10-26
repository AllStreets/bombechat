use bombechat_core::{Envelope, Kind, WireError, MAX_ENVELOPE};
use proptest::prelude::*;

fn sample() -> Envelope {
    Envelope {
        kind: Kind::Private,
        id: [7u8; 16],
        expires_min: 1_000_000,
        hops: 3,
        body: vec![1, 2, 3, 4],
    }
}

proptest! {
    #[test]
    fn roundtrip(kind in 1u8..=3, id: [u8;16], exp: u32, hops: u8, body in proptest::collection::vec(any::<u8>(), 0..4000)) {
        let e = Envelope { kind: Kind::try_from(kind).unwrap(), id, expires_min: exp, hops, body };
        prop_assert_eq!(Envelope::decode(&e.encode()).unwrap(), e);
    }
}

proptest! {
    #[test]
    fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..5000)) {
        let _ = Envelope::decode(&bytes);
    }
}

#[test]
fn rejects_truncated() {
    let e = sample();
    let b = e.encode();
    assert_eq!(
        Envelope::decode(&b[..b.len() - 1]),
        Err(WireError::Truncated)
    );
}

#[test]
fn rejects_version_2() {
    let mut b = sample().encode();
    b[0] = 2;
    assert_eq!(Envelope::decode(&b), Err(WireError::UnknownVersion(2)));
}

#[test]
fn rejects_oversize() {
    let mut e = sample();
    e.body = vec![0; MAX_ENVELOPE];
    assert!(matches!(
        Envelope::decode(&e.encode()),
        Err(WireError::TooLarge)
    ));
}

#[test]
fn rejects_trailing_bytes() {
    let mut b = sample().encode();
    b.push(0);
    assert_eq!(Envelope::decode(&b), Err(WireError::TrailingBytes));
}
