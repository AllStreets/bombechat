//! Requests to connect, and who is near: the node's side.
//!
//! A carrier that reaches people one by one (a transport implementing
//! [`Transport::request`] and reporting [`TransportEvent::Request`]) lets
//! a stranger nearby ask to connect, LinkedIn style: their request
//! carries their pairing hello, and nothing of this device's goes back
//! unless the person accepts. Accepting adds them as a contact, unchecked
//! until keys are compared in person, and sends this device's hello back
//! with proof of who sent it. Declining or ignoring keeps nothing.
//!
//! Requests are held in memory only, never in the store: at most
//! [`REQUESTS_WAITING`] (one more drops the oldest), and at most one from
//! the same device in [`REQUEST_GAP_MS`], the device being both the
//! requester's identity and the carrier's connection, so a stranger who
//! changes either is still held to it by the other.
//!
//! Strangers nearby are offered for requests by a short random label each
//! (never an identity: a stranger's is not known until they ask), for as
//! long as their carrier lists them.

use std::collections::VecDeque;

use bombechat_core::{safety_number, Envelope, PublicIdentity};
use bombechat_store::ContactId;
use rand_core::{OsRng, RngCore};

use crate::{control, low_order, now_min, random_id, Node, NodeEvent, TransportEvent, TTL_MIN};

/// At most this many requests wait for an answer; one more drops the
/// oldest. The one source of this number: a carrier that holds requests
/// itself caps them by it.
pub const REQUESTS_WAITING: usize = 20;

/// At most one request from the same device (its identity, or the
/// carrier's connection to it) is taken in this long: ten minutes. The
/// one source of this gap: a carrier that keeps its own per-connection
/// gap uses it.
pub const REQUEST_GAP_MS: u64 = 600_000;

/// Why a request is refused, or cannot be accepted, while new contacts
/// need a key check ([`Node::set_in_person_only`]).
pub const MEET_FIRST: &str =
    "Meet them and compare keys first: this machine only adds people checked in person.";

/// Why a request cannot go to a stranger who is no longer listed.
pub const NOT_IN_RANGE: &str = "They are no longer in range.";

/// Why a request cannot be accepted once it has gone.
pub const REQUEST_GONE: &str = "That request is no longer waiting.";

/// The letters a stranger's label is made of: no two that look alike.
const LABEL_LETTERS: &[u8] = b"ACDEFHJKLMNPRTUVWXY34679";
const LABEL_LEN: usize = 4;

/// Devices remembered for the gap, at most.
const RECENT_CAP: usize = 256;

/// How a request this node sent is getting on ([`NodeEvent::Requested`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestState {
    /// It reached them. Whether they accept is theirs to decide; a
    /// decline is never told, so it stays at this until it ends.
    Delivered,
    /// It reached them, then ended with no answer: its time ran out or
    /// the connection went. They may have declined; that is never told.
    Unanswered,
    /// It never reached them: it could not be sent, or the connection
    /// went before it was delivered.
    Failed,
    /// They accepted and proved who they are: now a contact, unchecked.
    Accepted(ContactId),
}

/// A request waiting for the person, as [`Node::requests`] lists it and
/// [`NodeEvent::Request`] first reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingRequest {
    pub id: u64,
    /// The name their hello gave: only a suggestion.
    pub name: String,
    /// The first bytes of their signing key.
    pub fingerprint: [u8; 8],
    /// The safety number shared with them, to compare in person.
    pub safety: String,
}

/// A request waiting for the person.
struct Waiting {
    id: u64,
    ti: usize,
    request: u64,
    from: PublicIdentity,
    name: String,
    safety: String,
}

/// A stranger a request can go to.
struct Stranger {
    ti: usize,
    key: u64,
    label: String,
}

/// What a transport last said of who is near.
#[derive(Default, Clone)]
struct Near {
    contacts: Vec<PublicIdentity>,
    seen: usize,
}

#[derive(Default)]
pub(crate) struct Requests {
    next_id: u64,
    waiting: VecDeque<Waiting>,
    /// When a request was last taken from each identity, and from each
    /// (transport, request number), by `now_ms`.
    recent_ids: VecDeque<(PublicIdentity, u64)>,
    recent_keys: VecDeque<((usize, u64), u64)>,
    strangers: Vec<Stranger>,
    /// Requests this node sent, waiting to be told how they went:
    /// (transport, stranger, label).
    asked: Vec<(usize, u64, String)>,
    near: Vec<Near>,
    /// Each contact's identity and id, as the store last had them: read
    /// only when contacts change ([`Node::share_contacts`]), never per
    /// tick.
    known: Vec<(PublicIdentity, ContactId)>,
    /// Contacts added by accepting a request whose proof never finished:
    /// not confirmed until a proven session with them, as the store keeps
    /// it.
    unconfirmed: Vec<ContactId>,
    /// Who is near may have changed since it was last reported.
    stale: bool,
    /// What [`NodeEvent::Peers`] last said.
    said: Option<(Vec<ContactId>, usize)>,
    /// The strangers' labels when it last said it: a change to who can
    /// be asked is announced too, even with the counts unchanged.
    said_listed: Vec<String>,
}

impl Requests {
    fn recently(&self, ti: usize, request: u64, from: &PublicIdentity, now: u64) -> bool {
        let fresh = |at: &u64| now.saturating_sub(*at) < REQUEST_GAP_MS;
        self.recent_ids
            .iter()
            .any(|(id, at)| id == from && fresh(at))
            || self
                .recent_keys
                .iter()
                .any(|(k, at)| *k == (ti, request) && fresh(at))
    }

    fn remember(&mut self, ti: usize, request: u64, from: PublicIdentity, now: u64) {
        self.recent_ids
            .retain(|(_, at)| now.saturating_sub(*at) < REQUEST_GAP_MS);
        self.recent_keys
            .retain(|(_, at)| now.saturating_sub(*at) < REQUEST_GAP_MS);
        self.recent_ids.push_back((from, now));
        self.recent_keys.push_back(((ti, request), now));
        while self.recent_ids.len() > RECENT_CAP {
            self.recent_ids.pop_front();
        }
        while self.recent_keys.len() > RECENT_CAP {
            self.recent_keys.pop_front();
        }
    }

    fn fresh_label(&self) -> String {
        loop {
            let label: String = (0..LABEL_LEN)
                .map(|_| {
                    let i = OsRng.next_u32() as usize % LABEL_LETTERS.len();
                    LABEL_LETTERS[i] as char
                })
                .collect();
            if !self.strangers.iter().any(|s| s.label == label) {
                return label;
            }
        }
    }
}

impl Node {
    /// Whether new contacts need a key check: while on (the default),
    /// requests to connect are neither sent nor accepted, so every contact
    /// is one checked in person. Requests still arrive and are shown, so
    /// the person can see who asked, and stay until they run out.
    pub fn set_in_person_only(&mut self, on: bool) {
        self.in_person_only = on;
    }

    /// The strangers nearby a request can go to, by a short random label
    /// each (never an identity), in a steady order. A label lasts as long
    /// as its carrier lists that stranger.
    pub fn strangers(&self) -> Vec<String> {
        self.requests
            .strangers
            .iter()
            .map(|s| s.label.clone())
            .collect()
    }

    /// Ask the stranger under `label` ([`Node::strangers`]) to connect: our
    /// pairing hello (our identity and name) goes to them alone. Only from
    /// an explicit press. How it goes is reported as
    /// [`NodeEvent::Requested`]; if they accept, they are added as a
    /// contact, unchecked.
    ///
    /// Refused with "locked" before [`Node::unlock`]; with [`MEET_FIRST`]
    /// while new contacts need a key check; with [`NOT_IN_RANGE`] for a
    /// label no longer listed; and with the carrier's own sentence when it
    /// cannot send requests now.
    pub fn request(&mut self, label: &str) -> Result<(), &'static str> {
        let (Some(me), Some(store)) = (&self.me, &self.store) else {
            return Err("locked");
        };
        if self.in_person_only {
            return Err(MEET_FIRST);
        }
        let Some((ti, key)) = self
            .requests
            .strangers
            .iter()
            .find(|s| s.label == label)
            .map(|s| (s.ti, s.key))
        else {
            return Err(NOT_IN_RANGE);
        };
        let hello = control::pair_hello(me, &store.my_name(), random_id(), now_min(), TTL_MIN);
        self.transports[ti].request(key, hello.encode())?;
        let asked = &mut self.requests.asked;
        if !asked.iter().any(|(t, k, _)| (*t, *k) == (ti, key)) {
            asked.push((ti, key, label.to_string()));
        }
        Ok(())
    }

    /// Accept the request `id` ([`NodeEvent::Request`]): store the
    /// requester as a contact named `name`, unchecked, and send them our
    /// hello with proof of who we are. Returns the contact's id (the
    /// existing one if they are already a contact).
    ///
    /// Refused with "locked" before [`Node::unlock`]; with
    /// [`REQUEST_GONE`] if the request has gone; and with [`MEET_FIRST`]
    /// while new contacts need a key check, when the request stays until
    /// it runs out, so the person can still see it.
    pub fn accept_request(&mut self, id: u64, name: &str) -> Result<ContactId, &'static str> {
        let (Some(me), Some(store)) = (&self.me, &self.store) else {
            return Err("locked");
        };
        let Some(at) = self.requests.waiting.iter().position(|w| w.id == id) else {
            return Err(REQUEST_GONE);
        };
        if self.in_person_only {
            return Err(MEET_FIRST);
        }
        let w = &self.requests.waiting[at];
        let existing = store.contacts().into_iter().find(|c| c.who == w.from);
        let contact = match existing {
            Some(c) => c.id,
            // not confirmed until they prove they hold us too; kept only
            // with that mark, or not at all
            None => store
                .add_unconfirmed_contact(name, &w.from)
                .map_err(|_| "not stored")?,
        };
        let hello = control::pair_hello(me, &store.my_name(), random_id(), now_min(), TTL_MIN);
        let w = self.requests.waiting.remove(at).expect("found above");
        self.share_contacts();
        self.transports[w.ti].answer_request(w.request, Some(hello.encode()));
        Ok(contact)
    }

    /// The requests waiting for the person, oldest first: what the
    /// [`NodeEvent::Request`]s not yet followed by a
    /// [`NodeEvent::RequestGone`] said, for a page that opens after them.
    pub fn requests(&self) -> Vec<WaitingRequest> {
        self.requests
            .waiting
            .iter()
            .map(|w| WaitingRequest {
                id: w.id,
                name: w.name.clone(),
                fingerprint: w.from.fingerprint(),
                safety: w.safety.clone(),
            })
            .collect()
    }

    /// Who is near, as the last [`NodeEvent::Peers`] said: the contacts
    /// in range, and how many strangers were seen lately. None in range
    /// until a carrier has said.
    pub fn peers(&self) -> (Vec<ContactId>, usize) {
        self.requests.said.clone().unwrap_or_default()
    }

    /// Contacts added by accepting a request whose requester hasn't yet
    /// been met in a proven session: they may not hold this device as a
    /// contact (the proof never finished: the link went, or time ran out),
    /// so the page shows them as not confirmed. Each is confirmed
    /// ([`NodeEvent::ContactConfirmed`]) at its first proven session; kept
    /// in the store across restarts.
    pub fn unconfirmed(&self) -> Vec<ContactId> {
        self.requests.unconfirmed.clone()
    }

    /// Contact `c` was paired in person: each holds the other, so if an
    /// accept left it unconfirmed it is confirmed now, and the page told.
    pub(crate) fn paired(&mut self, c: ContactId) {
        let Some(store) = &self.store else { return };
        if self.requests.unconfirmed.contains(&c) && store.set_confirmed(c, true).is_ok() {
            self.requests.unconfirmed.retain(|u| *u != c);
            self.pending
                .push(NodeEvent::ContactConfirmed { contact: c });
        }
    }

    /// Decline the request `id`: it is forgotten, and nothing at all goes
    /// back to the requester.
    pub fn decline_request(&mut self, id: u64) {
        if let Some(at) = self.requests.waiting.iter().position(|w| w.id == id) {
            let w = self.requests.waiting.remove(at).expect("found above");
            self.transports[w.ti].answer_request(w.request, None);
        }
    }

    /// Tell every transport who this device's contacts are.
    pub(crate) fn share_contacts(&mut self) {
        let Some(store) = &self.store else { return };
        let known: Vec<(PublicIdentity, ContactId)> = store
            .contacts()
            .into_iter()
            .map(|c| (c.who, c.id))
            .collect();
        let who: Vec<PublicIdentity> = known.iter().map(|(w, _)| w.clone()).collect();
        for t in &mut self.transports {
            t.set_contacts(&who);
        }
        self.requests.known = known;
        self.requests.unconfirmed = store.unconfirmed();
        self.requests.stale = true;
    }

    /// One of the request events a transport reported.
    pub(crate) fn on_request_event(
        &mut self,
        ti: usize,
        event: TransportEvent,
        events: &mut Vec<NodeEvent>,
    ) {
        match event {
            TransportEvent::Request {
                request,
                from,
                hello,
            } => self.on_request(ti, request, from, &hello, events),
            TransportEvent::RequestGone { request } => {
                if let Some(at) = self
                    .requests
                    .waiting
                    .iter()
                    .position(|w| (w.ti, w.request) == (ti, request))
                {
                    let w = self.requests.waiting.remove(at).expect("found above");
                    events.push(NodeEvent::RequestGone { id: w.id });
                }
            }
            TransportEvent::RequestDelivered { stranger } => {
                if let Some(label) = self.asked(ti, stranger, false) {
                    events.push(NodeEvent::Requested {
                        label,
                        state: RequestState::Delivered,
                    });
                }
            }
            TransportEvent::RequestFailed { stranger } => {
                if let Some(label) = self.asked(ti, stranger, true) {
                    events.push(NodeEvent::Requested {
                        label,
                        state: RequestState::Failed,
                    });
                }
            }
            TransportEvent::RequestUnanswered { stranger } => {
                if let Some(label) = self.asked(ti, stranger, true) {
                    events.push(NodeEvent::Requested {
                        label,
                        state: RequestState::Unanswered,
                    });
                }
            }
            TransportEvent::RequestAccepted {
                stranger,
                from,
                hello,
            } => self.on_accepted(ti, stranger, from, &hello, events),
            TransportEvent::Peers {
                contacts,
                strangers,
                strangers_seen,
            } => self.on_peers(ti, contacts, strangers, strangers_seen),
            _ => {}
        }
    }

    /// The label a request sent to `stranger` went under, forgotten if
    /// `done`.
    fn asked(&mut self, ti: usize, stranger: u64, done: bool) -> Option<String> {
        let asked = &mut self.requests.asked;
        let at = asked
            .iter()
            .position(|(t, k, _)| (*t, *k) == (ti, stranger))?;
        Some(if done {
            asked.remove(at).2
        } else {
            asked[at].2.clone()
        })
    }

    /// The hello's signer and name, if it is a hello from `from` that is
    /// neither this device nor a key that cannot be sealed to.
    fn hello_from(&self, from: &PublicIdentity, hello: &[u8]) -> Option<String> {
        let me = self.me.as_ref()?.public();
        let env = Envelope::decode(hello).ok()?;
        let (who, name) = control::read_hello(&env, now_min())?;
        (who == *from && who != me && !low_order(&who.dh)).then_some(name)
    }

    fn on_request(
        &mut self,
        ti: usize,
        request: u64,
        from: PublicIdentity,
        hello: &[u8],
        events: &mut Vec<NodeEvent>,
    ) {
        let now = self.now_ms;
        let name = self.hello_from(&from, hello);
        let contact = self.contact_for(&from).is_some();
        let (Some(name), false, Some(me)) = (name, contact, self.me.as_ref()) else {
            self.transports[ti].answer_request(request, None);
            return;
        };
        let safety = safety_number(&me.public(), &from);
        if self.requests.recently(ti, request, &from, now) {
            self.transports[ti].answer_request(request, None);
            return;
        }
        self.requests.remember(ti, request, from.clone(), now);
        if self.requests.waiting.len() >= REQUESTS_WAITING {
            let old = self.requests.waiting.pop_front().expect("full");
            self.transports[old.ti].answer_request(old.request, None);
            events.push(NodeEvent::RequestGone { id: old.id });
        }
        self.requests.next_id += 1;
        let id = self.requests.next_id;
        events.push(NodeEvent::Request {
            id,
            name: name.clone(),
            fingerprint: from.fingerprint(),
            safety: safety.clone(),
        });
        self.requests.waiting.push_back(Waiting {
            id,
            ti,
            request,
            from,
            name,
            safety,
        });
    }

    fn on_accepted(
        &mut self,
        ti: usize,
        stranger: u64,
        from: PublicIdentity,
        hello: &[u8],
        events: &mut Vec<NodeEvent>,
    ) {
        let label = self.asked(ti, stranger, true);
        let name = self.hello_from(&from, hello);
        let added = match (&label, name, &self.store) {
            (Some(_), Some(name), Some(store)) if !self.in_person_only => {
                match store.contacts().into_iter().find(|c| c.who == from) {
                    Some(c) => Some(c.id),
                    None => store.add_contact(&name, &from, false).ok(),
                }
            }
            _ => None,
        };
        // the carrier holds them as a contact only if they are one now
        self.share_contacts();
        if let Some(label) = label {
            events.push(NodeEvent::Requested {
                label,
                state: added.map_or(RequestState::Failed, RequestState::Accepted),
            });
        }
    }

    fn on_peers(&mut self, ti: usize, contacts: Vec<PublicIdentity>, keys: Vec<u64>, seen: usize) {
        let r = &mut self.requests;
        if r.near.len() <= ti {
            r.near.resize(ti + 1, Near::default());
        }
        r.near[ti] = Near { contacts, seen };
        r.stale = true;
        r.strangers.retain(|s| s.ti != ti || keys.contains(&s.key));
        for key in keys {
            if !r.strangers.iter().any(|s| (s.ti, s.key) == (ti, key)) {
                let label = r.fresh_label();
                r.strangers.push(Stranger { ti, key, label });
            }
        }
        r.strangers.sort_by_key(|s| (s.ti, s.key));
    }

    /// Report who is near if it changed: contacts present by any carrier,
    /// and strangers seen lately, summed over carriers.
    pub(crate) fn report_peers(&mut self, events: &mut Vec<NodeEvent>) {
        // no carrier has said who is near: nothing to report (sound and
        // light never do)
        if !self.requests.stale || self.requests.near.is_empty() {
            return;
        }
        self.requests.stale = false;
        let r = &self.requests;
        let mut contacts: Vec<ContactId> = r
            .near
            .iter()
            .flat_map(|n| n.contacts.iter())
            .filter_map(|who| r.known.iter().find(|(w, _)| w == who).map(|(_, id)| *id))
            .collect();
        contacts.sort_by_key(|c| c.0);
        contacts.dedup();
        // a proven session confirms a contact an accept left unconfirmed
        let met: Vec<ContactId> = contacts
            .iter()
            .copied()
            .filter(|c| self.requests.unconfirmed.contains(c))
            .collect();
        for c in met {
            if let Some(store) = &self.store {
                let _ = store.set_confirmed(c, true);
            }
            self.requests.unconfirmed.retain(|u| *u != c);
            events.push(NodeEvent::ContactConfirmed { contact: c });
        }
        let strangers = self.requests.near.iter().map(|n| n.seen).sum();
        let now = (contacts, strangers);
        let listed = self.strangers();
        if self.requests.said.as_ref() != Some(&now) || self.requests.said_listed != listed {
            self.requests.said = Some(now.clone());
            self.requests.said_listed = listed;
            events.push(NodeEvent::Peers {
                contacts_in_range: now.0,
                strangers_in_range: now.1,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pairing_hello, read_hello, OutboxState, Transport, NOT_SENT};
    use bombechat_core::Identity;
    use bombechat_store::{Channel, Store};
    use std::sync::{Arc, Mutex};

    /// Shared with the test: what the carrier was told.
    type Log<T> = Arc<Mutex<Vec<T>>>;

    /// A carrier that reaches people one by one, scripted by hand.
    #[derive(Clone, Default)]
    struct Reach {
        news: Log<TransportEvent>,
        /// Requests answered: accepted with a hello, or declined.
        answers: Log<(u64, Option<Vec<u8>>)>,
        /// Requests sent: (stranger, hello).
        asked: Log<(u64, Vec<u8>)>,
        contacts: Arc<Mutex<Vec<PublicIdentity>>>,
        me: Arc<Mutex<Option<PublicIdentity>>>,
        refuse: Arc<Mutex<Option<&'static str>>>,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl Transport for Reach {
        fn name(&self) -> Channel {
            Channel::Bluetooth
        }
        fn send(&mut self, frame: Vec<u8>) {
            self.sent.lock().unwrap().push(frame);
        }
        fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
            std::mem::take(&mut *self.news.lock().unwrap())
        }
        fn set_identity(&mut self, me: Option<&Identity>) {
            *self.me.lock().unwrap() = me.map(Identity::public);
        }
        fn set_contacts(&mut self, contacts: &[PublicIdentity]) {
            *self.contacts.lock().unwrap() = contacts.to_vec();
        }
        fn refusal(&self) -> Option<&'static str> {
            *self.refuse.lock().unwrap()
        }
        fn request(&mut self, stranger: u64, hello: Vec<u8>) -> Result<(), &'static str> {
            if let Some(why) = *self.refuse.lock().unwrap() {
                return Err(why);
            }
            self.asked.lock().unwrap().push((stranger, hello));
            Ok(())
        }
        fn answer_request(&mut self, request: u64, hello: Option<Vec<u8>>) {
            self.answers.lock().unwrap().push((request, hello));
        }
    }

    impl Reach {
        fn tell(&self, e: TransportEvent) {
            self.news.lock().unwrap().push(e);
        }
        /// A stranger's request, with their real hello.
        fn ask(&self, request: u64, who: &Identity, name: &str) {
            self.tell(TransportEvent::Request {
                request,
                from: who.public(),
                hello: pairing_hello(who, name, now_min()).encode(),
            });
        }
        fn answered(&self) -> Vec<(u64, Option<Vec<u8>>)> {
            std::mem::take(&mut *self.answers.lock().unwrap())
        }
    }

    fn node(dir: &tempfile::TempDir, me: &Identity) -> (Node, Reach) {
        let store = Store::create(&dir.path().join("me.db"), "pass", me, "Ann").unwrap();
        let reach = Reach::default();
        let mut node = Node::new(Some(store), vec![Box::new(reach.clone())]);
        node.set_in_person_only(false);
        (node, reach)
    }

    fn asked(events: &[NodeEvent]) -> Vec<(u64, String)> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Request { id, name, .. } => Some((*id, name.clone())),
                _ => None,
            })
            .collect()
    }

    fn gone(events: &[NodeEvent]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                NodeEvent::RequestGone { id } => Some(*id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_carrier_is_given_the_identity_and_contacts_on_unlock() {
        let dir = tempfile::TempDir::new().unwrap();
        let me = Identity::generate();
        let (node, reach) = node(&dir, &me);
        assert_eq!(*reach.me.lock().unwrap(), Some(me.public()));
        assert!(reach.contacts.lock().unwrap().is_empty());
        drop(node);
    }

    #[test]
    fn a_request_shows_who_asks_and_a_decline_keeps_and_sends_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        reach.ask(7, &ben, "Ben");
        let events = node.tick(0);
        let [NodeEvent::Request {
            id,
            name,
            fingerprint,
            safety,
        }] = &events[..]
        else {
            panic!("one request: {events:?}");
        };
        assert_eq!(name, "Ben");
        assert_eq!(*fingerprint, ben.public().fingerprint());
        assert_eq!(*safety, safety_number(&me.public(), &ben.public()));
        node.decline_request(*id);
        assert_eq!(reach.answered(), vec![(7, None)], "declined, nothing sent");
        assert!(node.contacts().is_empty(), "nothing kept");
        assert_eq!(node.accept_request(*id, "Ben"), Err(REQUEST_GONE));
    }

    #[test]
    fn accepting_adds_them_unchecked_and_answers_with_our_hello() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        reach.ask(7, &ben, "Ben");
        let id = asked(&node.tick(0))[0].0;
        let contact = node.accept_request(id, "Ben from the bus").unwrap();
        let c = &node.contacts()[0];
        assert_eq!(
            (c.id, c.name.as_str(), c.checked),
            (contact, "Ben from the bus", false)
        );
        assert_eq!(*reach.contacts.lock().unwrap(), vec![ben.public()]);
        let answers = reach.answered();
        let [(7, Some(hello))] = &answers[..] else {
            panic!("one answer: {answers:?}");
        };
        assert_eq!(
            read_hello(hello, now_min()),
            Some((me.public(), "Ann".to_string()))
        );
        // answered: no longer waiting, and its end is not news
        reach.tell(TransportEvent::RequestGone { request: 7 });
        assert!(gone(&node.tick(10)).is_empty());
    }

    #[test]
    fn a_request_from_someone_already_a_contact_is_declined_unseen() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        let store = node.store.as_ref().unwrap();
        store.add_contact("Ben", &ben.public(), true).unwrap();
        reach.ask(7, &ben, "Ben");
        assert!(asked(&node.tick(0)).is_empty(), "not shown");
        assert_eq!(reach.answered(), vec![(7, None)]);
    }

    #[test]
    fn an_accept_stays_unconfirmed_until_a_proven_session_and_across_restarts() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        reach.ask(7, &ben, "Ben");
        let id = asked(&node.tick(0))[0].0;
        let contact = node.accept_request(id, "Ben").unwrap();
        assert_eq!(node.unconfirmed(), vec![contact]);
        // the proof never finished: the request just ends
        reach.tell(TransportEvent::RequestGone { request: 7 });
        node.tick(10);
        assert_eq!(node.unconfirmed(), vec![contact]);
        drop(node);
        let store = Store::open(&dir.path().join("me.db"), "pass").unwrap();
        let reach = Reach::default();
        let mut node = Node::new(Some(store), vec![Box::new(reach.clone())]);
        assert_eq!(node.unconfirmed(), vec![contact], "kept across restarts");
        // a contact paired in person is never unconfirmed
        let cat = node.confirm_pairing(&Identity::generate().public(), "Cat");
        assert!(!node.unconfirmed().contains(&cat));
        // met in a proven session: confirmed, once
        reach.tell(TransportEvent::Peers {
            contacts: vec![ben.public()],
            strangers: vec![],
            strangers_seen: 0,
        });
        let events = node.tick(20);
        assert!(events
            .iter()
            .any(|e| matches!(e, NodeEvent::ContactConfirmed { contact: c } if *c == contact)));
        assert!(node.unconfirmed().is_empty());
        reach.tell(TransportEvent::Peers {
            contacts: vec![],
            strangers: vec![],
            strangers_seen: 0,
        });
        reach.tell(TransportEvent::Peers {
            contacts: vec![ben.public()],
            strangers: vec![],
            strangers_seen: 0,
        });
        assert!(!node
            .tick(30)
            .iter()
            .any(|e| matches!(e, NodeEvent::ContactConfirmed { .. })));
    }

    #[test]
    fn pairing_in_person_confirms_a_contact_an_accept_left_unconfirmed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        reach.ask(7, &ben, "Ben");
        let id = asked(&node.tick(0))[0].0;
        let contact = node.accept_request(id, "Ben").unwrap();
        assert_eq!(node.unconfirmed(), vec![contact]);
        assert_eq!(node.confirm_pairing(&ben.public(), "Ben"), contact);
        assert!(node.unconfirmed().is_empty(), "each holds the other now");
        assert!(
            node.store.as_ref().unwrap().unconfirmed().is_empty(),
            "and kept so"
        );
        let events = node.tick(10);
        assert_eq!(
            events
                .iter()
                .filter(
                    |e| matches!(e, NodeEvent::ContactConfirmed { contact: c } if *c == contact)
                )
                .count(),
            1,
            "the page is told"
        );
        // pairing again changes nothing more
        node.confirm_pairing(&ben.public(), "Ben");
        assert!(!node
            .tick(20)
            .iter()
            .any(|e| matches!(e, NodeEvent::ContactConfirmed { .. })));
    }

    #[test]
    fn waiting_requests_and_who_is_near_are_there_for_a_page_that_opens_late() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben, cat) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let (mut node, reach) = node(&dir, &me);
        assert!(node.requests().is_empty());
        assert_eq!(node.peers(), (vec![], 0), "none near until a carrier says");
        reach.ask(7, &ben, "Ben");
        reach.ask(8, &cat, "Cat");
        let shown = asked(&node.tick(0));
        let listed = node.requests();
        assert_eq!(
            listed
                .iter()
                .map(|w| (w.id, w.name.clone()))
                .collect::<Vec<_>>(),
            shown,
            "as the events said, oldest first"
        );
        assert_eq!(listed[0].fingerprint, ben.public().fingerprint());
        assert_eq!(listed[0].safety, safety_number(&me.public(), &ben.public()));
        node.decline_request(listed[0].id);
        assert_eq!(node.requests().len(), 1);
        let dan = node.confirm_pairing(&Identity::generate().public(), "Dan");
        let dan_key = node.contacts()[0].who.clone();
        reach.tell(TransportEvent::Peers {
            contacts: vec![dan_key],
            strangers: vec![3],
            strangers_seen: 2,
        });
        node.tick(10);
        assert_eq!(node.peers(), (vec![dan], 2));
    }

    #[test]
    fn with_key_checks_required_accept_is_refused_and_the_request_stays() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        node.set_in_person_only(true);
        reach.ask(7, &ben, "Ben");
        let id = asked(&node.tick(0))[0].0;
        assert_eq!(
            node.accept_request(id, "Ben"),
            Err("Meet them and compare keys first: this machine only adds people checked in person.")
        );
        assert!(node.contacts().is_empty());
        assert!(
            reach.answered().is_empty(),
            "nothing sent, nothing declined"
        );
        // it stays until it runs out
        reach.tell(TransportEvent::RequestGone { request: 7 });
        assert_eq!(gone(&node.tick(10)), vec![id]);
    }

    #[test]
    fn at_most_twenty_wait_and_the_oldest_goes() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        let people: Vec<Identity> = (0..25).map(|_| Identity::generate()).collect();
        for (i, p) in people.iter().enumerate() {
            reach.ask(i as u64, p, "someone");
        }
        let events = node.tick(0);
        let ids: Vec<u64> = asked(&events).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids.len(), 25);
        assert_eq!(gone(&events), ids[..5].to_vec(), "the five oldest went");
        let declined: Vec<u64> = reach.answered().into_iter().map(|(r, _)| r).collect();
        assert_eq!(declined, vec![0, 1, 2, 3, 4]);
        assert_eq!(node.requests.waiting.len(), REQUESTS_WAITING);
        // an accepted one leaves the twenty, so it is never pushed out
        node.accept_request(ids[5], "kept").unwrap();
        assert_eq!(node.requests.waiting.len(), REQUESTS_WAITING - 1);
    }

    #[test]
    fn one_request_per_device_in_ten_minutes_by_identity_or_connection() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        let (ben, cat, dan) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        reach.ask(1, &ben, "Ben");
        let first = asked(&node.tick(0))[0].0;
        node.decline_request(first);
        reach.answered();
        // the same identity on another connection, and another identity on
        // the same connection: both too soon
        reach.ask(2, &ben, "Ben again");
        reach.ask(1, &cat, "Cat");
        assert!(asked(&node.tick(1_000)).is_empty());
        let declined: Vec<u64> = reach.answered().into_iter().map(|(r, _)| r).collect();
        assert_eq!(declined, vec![2, 1]);
        // a new device is taken
        reach.ask(3, &dan, "Dan");
        assert_eq!(asked(&node.tick(2_000)).len(), 1);
        // and ten minutes on, the first may ask again
        reach.ask(1, &ben, "Ben");
        assert!(asked(&node.tick(REQUEST_GAP_MS - 1)).is_empty());
        reach.ask(1, &ben, "Ben");
        assert_eq!(asked(&node.tick(REQUEST_GAP_MS)).len(), 1);
    }

    #[test]
    fn a_hello_that_is_not_the_askers_own_is_declined_unseen() {
        let dir = tempfile::TempDir::new().unwrap();
        let me = Identity::generate();
        let (mut node, reach) = node(&dir, &me);
        let (ben, cat) = (Identity::generate(), Identity::generate());
        // Ben passes on Cat's hello
        reach.tell(TransportEvent::Request {
            request: 1,
            from: ben.public(),
            hello: pairing_hello(&cat, "Cat", now_min()).encode(),
        });
        // a broken hello, and our own
        reach.tell(TransportEvent::Request {
            request: 2,
            from: ben.public(),
            hello: vec![1, 2, 3],
        });
        reach.ask(3, &me, "me");
        assert!(node.tick(0).is_empty());
        let declined: Vec<u64> = reach.answered().into_iter().map(|(r, _)| r).collect();
        assert_eq!(declined, vec![1, 2, 3]);
    }

    #[test]
    fn requests_never_touch_the_store() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        let ben = Identity::generate();
        reach.ask(1, &ben, "Ben");
        node.tick(0);
        let store = node.store.as_ref().unwrap();
        assert_eq!(store.counts(), (0, 0));
        assert!(store.contacts().is_empty());
    }

    #[test]
    fn a_request_goes_to_one_stranger_by_label_and_an_accept_adds_them() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, ben) = (Identity::generate(), Identity::generate());
        let (mut node, reach) = node(&dir, &me);
        assert!(node.strangers().is_empty());
        reach.tell(TransportEvent::Peers {
            contacts: vec![],
            strangers: vec![40, 41],
            strangers_seen: 3,
        });
        let events = node.tick(0);
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::Peers { contacts_in_range, strangers_in_range: 3 } if contacts_in_range.is_empty()
        )));
        let labels = node.strangers();
        assert_eq!(labels.len(), 2);
        for l in &labels {
            assert_eq!(l.len(), LABEL_LEN);
            assert!(l.bytes().all(|b| LABEL_LETTERS.contains(&b)));
        }
        assert_ne!(labels[0], labels[1]);
        assert_eq!(node.request("ZZZZ"), Err(NOT_IN_RANGE));
        node.request(&labels[1]).unwrap();
        let sent = std::mem::take(&mut *reach.asked.lock().unwrap());
        let [(41, hello)] = &sent[..] else {
            panic!("one request, to the stranger chosen: {sent:?}");
        };
        assert_eq!(
            read_hello(hello, now_min()),
            Some((me.public(), "Ann".to_string()))
        );
        assert!(node.contacts().is_empty(), "nothing stored by asking");

        // labels hold while the stranger is listed
        reach.tell(TransportEvent::Peers {
            contacts: vec![],
            strangers: vec![41],
            strangers_seen: 3,
        });
        reach.tell(TransportEvent::RequestDelivered { stranger: 41 });
        let events = node.tick(10);
        assert_eq!(node.strangers(), vec![labels[1].clone()]);
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::Requested { label, state: RequestState::Delivered } if *label == labels[1]
        )));

        reach.tell(TransportEvent::RequestAccepted {
            stranger: 41,
            from: ben.public(),
            hello: pairing_hello(&ben, "Ben", now_min()).encode(),
        });
        reach.tell(TransportEvent::Peers {
            contacts: vec![ben.public()],
            strangers: vec![],
            strangers_seen: 3,
        });
        let events = node.tick(20);
        let c = node.contacts()[0].clone();
        assert_eq!(
            (c.name.as_str(), c.checked, &c.who),
            ("Ben", false, &ben.public())
        );
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::Requested { state: RequestState::Accepted(id), .. } if *id == c.id
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::Peers { contacts_in_range, .. } if *contacts_in_range == vec![c.id]
        )));
        assert_eq!(*reach.contacts.lock().unwrap(), vec![ben.public()]);
    }

    #[test]
    fn asking_is_refused_while_locked_with_key_checks_required_or_by_the_carrier() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        reach.tell(TransportEvent::Peers {
            contacts: vec![],
            strangers: vec![1],
            strangers_seen: 1,
        });
        node.tick(0);
        let label = node.strangers()[0].clone();
        node.set_in_person_only(true);
        assert_eq!(node.request(&label), Err(MEET_FIRST));
        node.set_in_person_only(false);
        *reach.refuse.lock().unwrap() = Some("closed");
        assert_eq!(node.request(&label), Err("closed"));
        assert!(reach.asked.lock().unwrap().is_empty());

        let mut locked = Node::new(None, vec![Box::new(Reach::default())]);
        assert_eq!(locked.request(&label), Err("locked"));
        assert_eq!(locked.accept_request(1, "x"), Err("locked"));
    }

    #[test]
    fn an_accept_after_key_checks_were_required_adds_nobody() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        let ben = Identity::generate();
        reach.tell(TransportEvent::Peers {
            contacts: vec![],
            strangers: vec![1],
            strangers_seen: 1,
        });
        node.tick(0);
        let label = node.strangers()[0].clone();
        node.request(&label).unwrap();
        node.set_in_person_only(true);
        reach.tell(TransportEvent::RequestAccepted {
            stranger: 1,
            from: ben.public(),
            hello: pairing_hello(&ben, "Ben", now_min()).encode(),
        });
        let events = node.tick(10);
        assert!(node.contacts().is_empty());
        assert!(reach.contacts.lock().unwrap().is_empty());
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::Requested {
                state: RequestState::Failed,
                ..
            }
        )));
    }

    #[test]
    fn a_change_in_who_can_be_asked_is_announced_even_with_the_same_counts() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        let peers = |events: Vec<NodeEvent>| {
            events
                .into_iter()
                .filter(|e| matches!(e, NodeEvent::Peers { .. }))
                .count()
        };
        let near = |strangers: Vec<u64>| TransportEvent::Peers {
            contacts: vec![],
            strangers,
            strangers_seen: 2,
        };
        reach.tell(near(vec![1, 2]));
        assert_eq!(peers(node.tick(0)), 1);
        let before = node.strangers();
        // the same again says nothing
        reach.tell(near(vec![1, 2]));
        assert_eq!(peers(node.tick(10)), 0);
        // one stranger's link closed, another's opened: the counts hold,
        // the list does not
        reach.tell(near(vec![2, 3]));
        assert_eq!(peers(node.tick(20)), 1, "announced");
        let after = node.strangers();
        assert_eq!(after[0], before[1], "a stranger keeps its label");
        assert!(!before.contains(&after[1]), "a new one gets a new label");
        // one goes, with the seen count unchanged
        reach.tell(near(vec![3]));
        assert_eq!(peers(node.tick(30)), 1, "announced");
        assert_eq!(node.strangers(), vec![after[1].clone()]);
    }

    #[test]
    fn a_delivered_request_that_ends_unanswered_is_told_apart_from_one_never_delivered() {
        let dir = tempfile::TempDir::new().unwrap();
        let (mut node, reach) = node(&dir, &Identity::generate());
        reach.tell(TransportEvent::Peers {
            contacts: vec![],
            strangers: vec![1, 2],
            strangers_seen: 2,
        });
        node.tick(0);
        let labels = node.strangers();
        node.request(&labels[0]).unwrap();
        node.request(&labels[1]).unwrap();
        reach.tell(TransportEvent::RequestDelivered { stranger: 1 });
        reach.tell(TransportEvent::RequestUnanswered { stranger: 1 });
        reach.tell(TransportEvent::RequestFailed { stranger: 2 });
        // told once: a late word about an ended request says nothing
        reach.tell(TransportEvent::RequestFailed { stranger: 1 });
        let states: Vec<(String, RequestState)> = node
            .tick(10)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::Requested { label, state } => Some((label, state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![
                (labels[0].clone(), RequestState::Delivered),
                (labels[0].clone(), RequestState::Unanswered),
                (labels[1].clone(), RequestState::Failed),
            ]
        );
    }

    #[test]
    fn a_carrier_with_no_switch_here_refuses_sends_in_its_own_words() {
        let dir = tempfile::TempDir::new().unwrap();
        let ben = Identity::generate();
        let (mut node, reach) = node(&dir, &Identity::generate());
        let store = node.store.as_ref().unwrap();
        let b = store.add_contact("Ben", &ben.public(), true).unwrap();
        *reach.refuse.lock().unwrap() = Some("not sent: off");
        let id = node.send(&[b], "hi", Channel::Bluetooth);
        assert_ne!(id, NOT_SENT);
        let events = node.tick(0);
        assert!(events.iter().any(|e| matches!(
            e,
            NodeEvent::ChannelState { channel: Channel::Bluetooth, state } if state == "not sent: off"
        )));
        assert!(events.iter().any(
            |e| matches!(e, NodeEvent::Outbox { id: i, state: OutboxState::Failed } if *i == id)
        ));
        assert!(reach.sent.lock().unwrap().is_empty());
        *reach.refuse.lock().unwrap() = None;
        node.send(&[b], "hi again", Channel::Bluetooth);
        assert!(!reach.sent.lock().unwrap().is_empty(), "it goes out");
    }
}
