//! Who an envelope is for, read from what any carrier can see of it.

use bombechat_core::{split_private_body, Envelope, Kind};

/// Bytes of one recipient's wrap in a private body's header.
const WRAP_LEN: usize = 112;
/// Bytes before the first wrap: version and count.
const WRAPS_AT: usize = 2;
/// A control envelope's header: version and type.
const CONTROL_HEADER: usize = 2;
const CONTROL_HEARD: u8 = 2;

/// Where an envelope goes over Bluetooth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Targets {
    /// To the contacts with these fingerprints, and no one else.
    Only(Vec<[u8; 8]>),
    /// To every confirmed contact session (public broadcasts, and pairing
    /// hellos, which are public too).
    Everyone,
}

/// Where `envelope` goes, or `None` if it isn't a well-formed envelope
/// this build knows how to address.
pub(crate) fn targets(envelope: &[u8]) -> Option<Targets> {
    let env = Envelope::decode(envelope).ok()?;
    match env.kind {
        Kind::Private => recipients(&env.body).map(Targets::Only),
        Kind::Public => Some(Targets::Everyone),
        Kind::Control => {
            if env.body.len() < CONTROL_HEADER {
                return None;
            }
            if env.body[1] == CONTROL_HEARD {
                // a receipt: a private envelope inside, for the original sender
                let inner = Envelope::decode(&env.body[CONTROL_HEADER..]).ok()?;
                if inner.kind != Kind::Private {
                    return None;
                }
                recipients(&inner.body).map(Targets::Only)
            } else {
                Some(Targets::Everyone)
            }
        }
    }
}

/// The recipient fingerprints of a private body.
fn recipients(body: &[u8]) -> Option<Vec<[u8; 8]>> {
    let (header, _) = split_private_body(body).ok()?;
    let count = header[1] as usize;
    (0..count)
        .map(|i| {
            let at = WRAPS_AT + i * WRAP_LEN;
            header.get(at..at + 8)?.try_into().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombechat_core::{seal_private, seal_public, Identity};

    #[test]
    fn private_envelopes_go_to_their_recipients_and_public_ones_to_everyone() {
        let (a, b, c) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let env = seal_private(&a, &[b.public(), c.public()], "hi", 1000, 60);
        assert_eq!(
            targets(&env.encode()),
            Some(Targets::Only(vec![
                b.public().fingerprint(),
                c.public().fingerprint()
            ]))
        );
        let env = seal_public(&a, "all", 1000, 60);
        assert_eq!(targets(&env.encode()), Some(Targets::Everyone));
        assert_eq!(targets(&[1, 2, 3]), None);
    }

    #[test]
    fn a_receipt_goes_only_to_the_original_sender() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let inner = seal_private(&b, &[a.public()], "bombechat heard v1\n00", 1000, 60);
        let mut body = vec![1, CONTROL_HEARD];
        body.extend_from_slice(&inner.encode());
        let receipt = Envelope {
            kind: Kind::Control,
            id: [9; 16],
            expires_min: 1060,
            hops: 0,
            body,
        };
        assert_eq!(
            targets(&receipt.encode()),
            Some(Targets::Only(vec![a.public().fingerprint()]))
        );
    }
}
