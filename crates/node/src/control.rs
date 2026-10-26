//! Control envelopes: pairing hellos and delivery receipts.
//!
//! A control envelope is an ordinary [`Envelope`] of kind
//! [`Kind::Control`]. Its body wraps a second, complete envelope built
//! with the core's sealing functions, so control traffic is signed and
//! (for receipts) encrypted by exactly the same code as messages, with no
//! new construction:
//!
//! ```text
//! version u8 = 1 | type u8 | inner envelope (Envelope::encode)
//! ```
//!
//! * type 1, `PairHello`: the inner envelope is a signed public envelope
//!   ([`seal_public`]) from the identity being offered, whose text is
//!   `"bombechat pair hello v1\n" ‖ name`. The signature proves the
//!   sender holds the signing key for the identity it offers, and covers
//!   the X25519 key too.
//! * type 2, `Heard`: the inner envelope is a private envelope
//!   ([`seal_private`]) sealed to the original sender, whose text is
//!   `"bombechat heard v1\n" ‖ 32 lowercase hex digits` of the heard
//!   message's envelope id. The seal's signature names who heard it.
//!
//! The text prefixes keep the two apart from anything a person typed: a
//! broadcast whose text starts with the hello prefix is not shown as a
//! broadcast, and a hello's inner envelope lifted out and replayed as a
//! broadcast is refused for the same reason.

use bombechat_core::{
    open_private, open_public, seal_private, seal_public, Envelope, Identity, Kind, OpenError,
    PublicIdentity,
};

const VERSION: u8 = 1;
const TYPE_PAIR_HELLO: u8 = 1;
const TYPE_HEARD: u8 = 2;

pub(crate) const HELLO_PREFIX: &str = "bombechat pair hello v1\n";
const HEARD_PREFIX: &str = "bombechat heard v1\n";

/// Longest display name carried in a hello, in bytes. Longer names are cut
/// at the last character boundary that fits.
pub(crate) const MAX_NAME: usize = 64;

/// A control message, once opened and checked.
pub(crate) enum Control {
    PairHello {
        identity: PublicIdentity,
        name: String,
    },
    /// `by` is the identity whose signature sealed the receipt.
    Heard { id: [u8; 16], by: PublicIdentity },
}

fn wrap(kind_byte: u8, inner: &Envelope, id: [u8; 16], expires_min: u32) -> Envelope {
    let mut body = vec![VERSION, kind_byte];
    body.extend_from_slice(&inner.encode());
    Envelope {
        kind: Kind::Control,
        id,
        expires_min,
        hops: 0,
        body,
    }
}

/// A signed hello offering `me` under `name`.
pub(crate) fn pair_hello(
    me: &Identity,
    name: &str,
    id: [u8; 16],
    now_min: u32,
    ttl_min: u32,
) -> Envelope {
    let name = truncate(name, MAX_NAME);
    let inner = seal_public(me, &format!("{HELLO_PREFIX}{name}"), now_min, ttl_min);
    wrap(TYPE_PAIR_HELLO, &inner, id, now_min.saturating_add(ttl_min))
}

/// A receipt for message `heard`, sealed from `me` to `to`. The caller
/// must have checked `to` is a usable (not low-order) key.
pub(crate) fn heard(
    me: &Identity,
    to: &PublicIdentity,
    heard: &[u8; 16],
    id: [u8; 16],
    now_min: u32,
    ttl_min: u32,
) -> Envelope {
    let text = format!("{HEARD_PREFIX}{}", hex(heard));
    let inner = seal_private(me, std::slice::from_ref(to), &text, now_min, ttl_min);
    wrap(TYPE_HEARD, &inner, id, now_min.saturating_add(ttl_min))
}

/// Why a control envelope did not open.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// A receipt sealed to someone else: nothing wrong with it.
    NotForMe,
    /// Malformed, unverifiable or expired.
    Bad,
}

/// Open a control envelope's body. [`Refused::NotForMe`] for a receipt
/// addressed to someone else; [`Refused::Bad`] for anything malformed,
/// unverifiable or expired.
///
/// Also returns the inner envelope's id. Unlike the outer id, it is
/// covered by the inner signature (and, for receipts, the seal), so it is
/// the id to dedup control traffic on: an inner envelope re-wrapped under
/// a fresh outer id is still recognised as the same message.
pub(crate) fn open(
    me: &Identity,
    body: &[u8],
    now_min: u32,
) -> Result<(Control, [u8; 16]), Refused> {
    let [version, kind, rest @ ..] = body else {
        return Err(Refused::Bad);
    };
    if *version != VERSION {
        return Err(Refused::Bad);
    }
    let inner = Envelope::decode(rest).map_err(|_| Refused::Bad)?;
    match *kind {
        TYPE_PAIR_HELLO => {
            let (identity, text, _) = open_public(&inner, now_min).map_err(|_| Refused::Bad)?;
            let name = text.strip_prefix(HELLO_PREFIX).ok_or(Refused::Bad)?;
            Ok((
                Control::PairHello {
                    identity,
                    name: truncate(name, MAX_NAME).to_string(),
                },
                inner.id,
            ))
        }
        TYPE_HEARD => {
            let opened = open_private(me, &inner, now_min).map_err(|e| match e {
                OpenError::NotForMe => Refused::NotForMe,
                _ => Refused::Bad,
            })?;
            let digits = opened.text.strip_prefix(HEARD_PREFIX).ok_or(Refused::Bad)?;
            Ok((
                Control::Heard {
                    id: unhex(digits).ok_or(Refused::Bad)?,
                    by: opened.sender,
                },
                inner.id,
            ))
        }
        _ => Err(Refused::Bad),
    }
}

/// Who a pairing hello envelope is from and the name it gives, if it is
/// one, its signature verifies and it has not expired by `now_min`. Only
/// the signer is proved: a hello is public, so anyone may pass one on.
pub(crate) fn read_hello(env: &Envelope, now_min: u32) -> Option<(PublicIdentity, String)> {
    if env.kind != Kind::Control {
        return None;
    }
    let [VERSION, TYPE_PAIR_HELLO, rest @ ..] = env.body.as_slice() else {
        return None;
    };
    let inner = Envelope::decode(rest).ok()?;
    let (identity, text, _) = open_public(&inner, now_min).ok()?;
    let name = text.strip_prefix(HELLO_PREFIX)?;
    Some((identity, truncate(name, MAX_NAME).to_string()))
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32
        || !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips_and_names_the_signer() {
        let a = Identity::generate();
        let env = pair_hello(&a, "Ana", [7; 16], 100, 60);
        assert_eq!(env.kind, Kind::Control);
        match open(&Identity::generate(), &env.body, 100) {
            Ok((Control::PairHello { identity, name }, _)) => {
                assert_eq!(identity, a.public());
                assert_eq!(name, "Ana");
            }
            _ => panic!("hello did not open"),
        }
    }

    #[test]
    fn heard_opens_only_for_its_recipient() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let env = heard(&b, &a.public(), &[0xab; 16], [1; 16], 100, 60);
        match open(&a, &env.body, 100) {
            Ok((Control::Heard { id, by }, _)) => {
                assert_eq!(id, [0xab; 16]);
                assert_eq!(by, b.public());
            }
            _ => panic!("receipt did not open"),
        }
        assert!(open(&Identity::generate(), &env.body, 100).is_err());
    }

    #[test]
    fn a_broadcast_is_not_a_hello() {
        let a = Identity::generate();
        let inner = seal_public(&a, "Ana", 100, 60);
        let env = wrap(TYPE_PAIR_HELLO, &inner, [2; 16], 160);
        assert!(open(&a, &env.body, 100).is_err());
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary() {
        let name = "é".repeat(40); // 80 bytes
        let cut = truncate(&name, MAX_NAME);
        assert_eq!(cut.len(), 64);
        assert!(cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_hello_reads_back_and_nothing_else_does() {
        let a = Identity::generate();
        let env = pair_hello(&a, "Ana", [7; 16], 100, 60);
        assert_eq!(read_hello(&env, 100), Some((a.public(), "Ana".into())));
        assert_eq!(read_hello(&env, 100 + 60 + 1441), None, "expired");
        let receipt = heard(&a, &a.public(), &[1; 16], [2; 16], 100, 60);
        assert_eq!(read_hello(&receipt, 100), None);
        let mut forged = env.clone();
        let last = forged.body.len() - 1;
        forged.body[last] ^= 1;
        assert_eq!(read_hello(&forged, 100), None);
    }

    #[test]
    fn garbage_never_panics() {
        let me = Identity::generate();
        for body in [
            &[][..],
            &[1][..],
            &[1, 1][..],
            &[2, 1, 0][..],
            &[1, 9, 1, 2, 3][..],
        ] {
            assert!(open(&me, body, 100).is_err());
        }
    }
}
