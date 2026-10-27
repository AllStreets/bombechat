use bombechat_core::{
    open_private, open_public, seal_private, seal_public, split_private_body, ChunkOpener, TEXT_AT,
    Envelope, Identity, Kind, OpenError, CHUNK, MAX_ENVELOPE,
};

fn alice() -> Identity {
    Identity::from_secret_bytes(&[0xA1; 64])
}

fn bob() -> Identity {
    Identity::from_secret_bytes(&[0xB0; 64])
}

/// A fixed sender and recipient, but the envelope itself is fresh each call
/// (random id, ephemeral keys and nonces).
fn fixture() -> Envelope {
    seal_private(
        &alice(),
        &[bob().public()],
        "Meet at the water tower at noon.",
        100,
        1440,
    )
}

fn e_len() -> usize {
    fixture().body.len()
}

#[test]
fn two_recipients_both_open() {
    let (a, b, c) = (
        Identity::generate(),
        Identity::generate(),
        Identity::generate(),
    );
    let e = seal_private(
        &a,
        &[b.public(), c.public()],
        "Water is at the church on Fifth.",
        100,
        1440,
    );
    assert_eq!(e.kind, Kind::Private);
    assert_eq!(e.expires_min, 100 + 1440);
    assert_eq!(
        open_private(&b, &e, 100).unwrap().text,
        "Water is at the church on Fifth."
    );
    let opened = open_private(&c, &e, 100).unwrap();
    assert_eq!(opened.sender, a.public());
    assert_eq!(opened.sent_min, 100);
}

#[test]
fn stranger_cannot_open() {
    let (a, b, x) = (
        Identity::generate(),
        Identity::generate(),
        Identity::generate(),
    );
    let e = seal_private(&a, &[b.public()], "hi", 100, 1440);
    assert_eq!(open_private(&x, &e, 100), Err(OpenError::NotForMe));
}

#[test]
fn any_flipped_byte_is_tampered() {
    let n = e_len();
    for i in 0..n {
        let mut e = fixture();
        assert_eq!(e.body.len(), n);
        e.body[i] ^= 1;
        let r = open_private(&bob(), &e, 100);
        assert!(
            matches!(
                r,
                Err(OpenError::Tampered | OpenError::NotForMe | OpenError::Malformed)
            ),
            "byte {i} flipped gave {r:?}"
        );
    }
}

#[test]
fn truncated_or_extended_body_fails() {
    let e = fixture();
    for cut in 1..e.body.len() {
        let mut t = e.clone();
        t.body.truncate(e.body.len() - cut);
        let r = open_private(&bob(), &t, 100);
        assert!(
            matches!(r, Err(OpenError::Tampered | OpenError::Malformed)),
            "cut {cut} gave {r:?}"
        );
    }
    let mut longer = e.clone();
    longer.body.push(0);
    assert!(matches!(
        open_private(&bob(), &longer, 100),
        Err(OpenError::Tampered | OpenError::Malformed)
    ));
}

#[test]
fn body_moved_to_another_envelope_fails() {
    let e = fixture();

    let mut other_id = e.clone();
    other_id.id[0] ^= 1;
    assert_eq!(
        open_private(&bob(), &other_id, 100),
        Err(OpenError::Tampered)
    );

    let mut other_expiry = e.clone();
    other_expiry.expires_min += 1;
    assert_eq!(
        open_private(&bob(), &other_expiry, 100),
        Err(OpenError::Tampered)
    );

    let mut other_kind = e.clone();
    other_kind.kind = Kind::Public;
    assert!(open_private(&bob(), &other_kind, 100).is_err());

    // A different, genuine envelope for the same recipient: grafting the
    // body of one onto the other's id and expiry fails.
    let second = fixture();
    let grafted = Envelope {
        body: e.body.clone(),
        ..second
    };
    assert_eq!(
        open_private(&bob(), &grafted, 100),
        Err(OpenError::Tampered)
    );

    // Hops sit outside the encryption on purpose: carriers change them.
    let mut hopped = e.clone();
    hopped.hops = 7;
    assert!(open_private(&bob(), &hopped, 100).is_ok());
}

#[test]
fn chunk_by_chunk_matches_whole() {
    let a = alice();
    let b = bob();
    let text = "The bridge on Halsted is out. Take Ashland north to the school, \
                where the clinic has water, blankets and a working radio until Sunday.";
    let e = seal_private(&a, &[b.public()], text, 4321, 600);

    let (header, chunks) = split_private_body(&e.body).unwrap();
    assert!(
        chunks.len() >= 4,
        "message should span several chunks, got {}",
        chunks.len()
    );
    for c in &chunks[..chunks.len() - 1] {
        assert_eq!(c.len(), CHUNK + 16);
    }

    let mut opener = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    let mut all_plain = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let last = i == chunks.len() - 1;
        let view = opener.next(c, last).unwrap();
        assert_eq!(view.heard, c[..c.len() - 16].to_vec());
        assert_eq!(view.plain.len(), view.heard.len());
        assert_eq!(view.key.len(), view.heard.len());
        let xored: Vec<u8> = view
            .heard
            .iter()
            .zip(&view.key)
            .map(|(h, k)| h ^ k)
            .collect();
        assert_eq!(xored, view.plain);
        all_plain.extend_from_slice(&view.plain);
    }

    // Inner plaintext: sender sign ‖ sender dh ‖ sent_min ‖ text ‖ signature.
    let mut expected_prefix = Vec::new();
    expected_prefix.extend_from_slice(&a.public().sign);
    expected_prefix.extend_from_slice(&a.public().dh);
    expected_prefix.extend_from_slice(&4321u32.to_be_bytes());
    expected_prefix.extend_from_slice(text.as_bytes());
    assert_eq!(all_plain.len(), expected_prefix.len() + 64);
    assert_eq!(&all_plain[..expected_prefix.len()], &expected_prefix[..]);

    let opened = opener.finish().unwrap();
    assert_eq!(opened, open_private(&b, &e, 4321).unwrap());
    assert_eq!(opened.text, text);
    assert_eq!(opened.sender, a.public());
}

#[test]
fn chunk_opener_rejects_wrong_last_flag_and_reordering() {
    let b = bob();
    let e = seal_private(&alice(), &[b.public()], &"x".repeat(200), 100, 60);
    let (header, chunks) = split_private_body(&e.body).unwrap();
    assert!(chunks.len() >= 3);

    // Marking a middle chunk as last (truncation) fails.
    let mut o = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    o.next(chunks[0], false).unwrap();
    assert!(matches!(o.next(chunks[1], true), Err(OpenError::Tampered)));

    // Not marking the real last chunk as last fails.
    let mut o = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    for c in &chunks[..chunks.len() - 1] {
        o.next(c, false).unwrap();
    }
    assert!(o.next(chunks[chunks.len() - 1], false).is_err());

    // Swapping two chunks fails.
    let mut o = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    assert!(matches!(o.next(chunks[1], false), Err(OpenError::Tampered)));

    // Finishing before the last chunk fails.
    let mut o = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    o.next(chunks[0], false).unwrap();
    assert_eq!(o.finish().unwrap_err(), OpenError::Malformed);

    // Once a chunk has failed, the opener stays failed.
    let mut o = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    assert!(o.next(chunks[1], false).is_err());
    assert!(o.next(chunks[0], false).is_err());
}

#[test]
fn chunk_opener_wrong_envelope_context_fails() {
    let b = bob();
    let e = fixture();
    let (header, _) = split_private_body(&e.body).unwrap();
    let mut id = e.id;
    id[15] ^= 0x80;
    assert_eq!(
        ChunkOpener::start(&b, id, e.expires_min, header).err(),
        Some(OpenError::Tampered)
    );
    assert_eq!(
        ChunkOpener::start(&Identity::generate(), e.id, e.expires_min, header).err(),
        Some(OpenError::NotForMe)
    );
}

#[test]
fn public_signature_checked() {
    let a = Identity::generate();
    let mut e = seal_public(&a, "Shelter open at Benito Juarez High.", 100, 1440);
    assert_eq!(e.kind, Kind::Public);
    let (sender, text, sent) = open_public(&e, 100).unwrap();
    assert_eq!(sender, a.public());
    assert_eq!(text, "Shelter open at Benito Juarez High.");
    assert_eq!(sent, 100);
    e.body[40] ^= 1;
    assert_eq!(open_public(&e, 100).unwrap_err(), OpenError::BadSignature);
}

#[test]
fn public_any_flipped_byte_or_context_fails() {
    let a = Identity::generate();
    let e = seal_public(&a, "Road closed at 26th.", 100, 1440);
    for i in 0..e.body.len() {
        let mut t = e.clone();
        t.body[i] ^= 1;
        let r = open_public(&t, 100);
        assert!(
            matches!(r, Err(OpenError::BadSignature | OpenError::Malformed)),
            "byte {i} flipped gave {r:?}"
        );
    }
    let mut other_id = e.clone();
    other_id.id[3] ^= 1;
    assert_eq!(
        open_public(&other_id, 100).unwrap_err(),
        OpenError::BadSignature
    );
    let mut other_expiry = e.clone();
    other_expiry.expires_min += 60;
    assert_eq!(
        open_public(&other_expiry, 100).unwrap_err(),
        OpenError::BadSignature
    );
    let mut short = e.clone();
    short.body.truncate(100);
    assert_eq!(open_public(&short, 100).unwrap_err(), OpenError::Malformed);
}

#[test]
fn kinds_are_not_interchangeable() {
    let a = alice();
    let p = seal_public(&a, "hello", 100, 60);
    assert_eq!(open_private(&bob(), &p, 100), Err(OpenError::Malformed));
    let s = fixture();
    assert_eq!(open_public(&s, 100).unwrap_err(), OpenError::Malformed);
}

#[test]
fn skew_tolerated_but_expiry_enforced() {
    let a = alice();
    let b = bob();

    // Sealed at minute 0 with a 1000-minute lifetime: expires at 1000.
    let e = seal_private(&a, &[b.public()], "Generator fuel at the depot.", 0, 1000);
    assert_eq!(e.expires_min, 1000);
    assert!(open_private(&b, &e, 0).is_ok());
    // Past expiry but inside the 24-hour skew window: still opens.
    assert!(open_private(&b, &e, 2000).is_ok());
    assert!(open_private(&b, &e, 2440).is_ok());
    // Beyond the window: refused.
    assert_eq!(open_private(&b, &e, 2441), Err(OpenError::Expired));
    assert_eq!(open_private(&b, &e, 2500), Err(OpenError::Expired));

    let p = seal_public(&a, "Generator fuel at the depot.", 0, 1000);
    assert_eq!(p.expires_min, 1000);
    assert!(open_public(&p, 2000).is_ok());
    assert_eq!(open_public(&p, 2500).unwrap_err(), OpenError::Expired);
}

#[test]
fn ids_are_random_and_bodies_fit() {
    let a = alice();
    let b = bob();
    let e1 = seal_private(&a, &[b.public()], "same", 100, 60);
    let e2 = seal_private(&a, &[b.public()], "same", 100, 60);
    assert_ne!(e1.id, e2.id);
    assert_ne!(e1.body, e2.body);
    assert!(e1.encode().len() <= MAX_ENVELOPE);

    let text = "y".repeat(2000);
    let e = seal_private(
        &a,
        &[b.public(), Identity::generate().public()],
        &text,
        100,
        60,
    );
    assert!(e.encode().len() <= MAX_ENVELOPE);
    assert_eq!(open_private(&b, &e, 100).unwrap().text, text);
}

#[test]
#[should_panic]
fn oversized_private_text_panics() {
    seal_private(&alice(), &[bob().public()], &"z".repeat(4000), 100, 60);
}

#[test]
#[should_panic]
fn no_recipients_panics() {
    seal_private(&alice(), &[], "to nobody", 100, 60);
}

#[test]
fn low_order_ephemeral_key_is_rejected() {
    // Layout: version, count, then the first wrap: fingerprint [8], ephemeral
    // public key [32], ...
    let mut e = fixture();
    e.body[10..42].fill(0);
    assert_eq!(open_private(&bob(), &e, 100), Err(OpenError::Tampered));
}

#[test]
#[should_panic]
fn low_order_recipient_key_panics() {
    let mut bogus = bob().public();
    bogus.dh = [0u8; 32];
    seal_private(&alice(), &[bogus], "never sealed", 100, 60);
}

/// The text starts TEXT_AT bytes into the plaintext the chunks carry, every
/// chunk but the last is CHUNK bytes of it, and the page lays the plain row
/// from both as the machine reports them.
#[test]
fn text_starts_at_text_at_in_whole_chunks() {
    let b = bob();
    let text = "Water is at the church on Fifth.";
    let e = seal_private(&alice(), &[b.public()], text, 4321, 600);
    let (header, chunks) = split_private_body(&e.body).unwrap();
    let mut o = ChunkOpener::start(&b, e.id, e.expires_min, header).unwrap();
    let mut plain = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let last = i == chunks.len() - 1;
        let view = o.next(c, last).unwrap();
        if !last {
            assert_eq!(view.plain.len(), CHUNK);
        }
        plain.extend(view.plain);
    }
    assert_eq!(TEXT_AT, 68);
    assert_eq!(&plain[TEXT_AT..TEXT_AT + text.len()], text.as_bytes());
}
