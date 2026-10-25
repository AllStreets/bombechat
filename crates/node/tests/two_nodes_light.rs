//! Nodes talking by light alone: each node's Display frames go up on its
//! "screen", and a synthetic camera (the light crate's test model: a
//! perspective view of the screen in a cluttered room, lens blur, sensor
//! noise, vignetting) photographs whatever screens are in its view and
//! feeds the pictures to the other node's `FakeCamera`.
//!
//! Time is explicit `now_ms` in 20 ms ticks, as the app ticks. The reader
//! runs on its own thread, so after each picture the test waits until
//! that node's reader is idle; what it read is then collected on the
//! node's next tick, at that tick's `now_ms`.

#[path = "../../light/tests/camera/mod.rs"]
mod camera;

use std::thread;
use std::time::{Duration, Instant};

use bombechat_core::{Identity, PublicIdentity};
use bombechat_light::{CodeSpec, FakeCamera, Reader};
use bombechat_node::{
    LightSwitch, LightTransport, Node, NodeEvent, OutboxState, ReaderWatch, SoundSwitch,
};
use bombechat_store::{Channel, ContactId, Store};
use camera::{shoot, Camera, Code, Pose};
use tempfile::TempDir;

const TEXT: &str = "Water is at the church on Fifth.";
const TICK_MS: u64 = 20;

/// What one node's screen shows.
#[derive(Clone)]
struct Shown {
    spec: CodeSpec,
    bits: Vec<u8>,
}

impl Shown {
    /// The grid as the camera model takes it: row-major, `true` dark.
    fn grid(&self) -> Vec<bool> {
        let n = self.spec.cells as usize;
        (0..n * n)
            .map(|i| self.bits[i / 8] & (0x80 >> (i % 8)) != 0)
            .collect()
    }
}

struct Peer {
    node: Node,
    cam: FakeCamera,
    watch: ReaderWatch,
    me: PublicIdentity,
    screen: Option<Shown>,
    /// Every event this node reported.
    log: Vec<NodeEvent>,
}

impl Peer {
    fn new(dir: &TempDir, name: &str, cam: FakeCamera) -> Peer {
        let id = Identity::generate();
        let store =
            Store::create(&dir.path().join(format!("{name}.db")), "pass", &id, name).unwrap();
        // A patient reader: the tests run in parallel on a busy machine,
        // and what they check is the transport, not the reader's budget.
        let light = LightTransport::with_reader(
            Box::new(cam.clone()),
            CodeSpec::normal(),
            Reader::with_budget(Duration::from_millis(200)),
        );
        let watch = light.watch();
        let mut node = Node::new(Some(store), vec![Box::new(light)]);
        node.set_sound(SoundSwitch::Off);
        node.set_light(LightSwitch::ReadAndFlash);
        Peer {
            node,
            cam,
            watch,
            me: id.public(),
            screen: None,
            log: Vec::new(),
        }
    }

    fn add(&mut self, other: &Peer) -> ContactId {
        self.node.confirm_pairing(&other.me, "them")
    }

    /// Wait until the reader has read every picture it was given and
    /// opened or closed the camera as asked.
    fn settle(&self) {
        let until = Instant::now() + Duration::from_secs(30);
        while !self.watch.idle() {
            assert!(Instant::now() < until, "the reader never caught up");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn tick(&mut self, now_ms: u64) {
        let events = self.node.tick(now_ms);
        for e in &events {
            match e {
                NodeEvent::Display { bits, spec_id, .. } => {
                    self.screen = Some(Shown {
                        spec: CodeSpec::from_preset(*spec_id).unwrap(),
                        bits: bits.clone(),
                    })
                }
                NodeEvent::DisplayStopped { .. } => self.screen = None,
                _ => {}
            }
        }
        self.log.extend(events);
    }
}

/// Who sees whose screen, and where it sits in the picture.
struct View {
    viewer: usize,
    shown: usize,
    pose: Pose,
}

fn facing(viewer: usize, shown: usize) -> View {
    View {
        viewer,
        shown,
        pose: Pose {
            scale: 0.6,
            centre: (0.5, 0.5),
            roll: 6.0,
            tilt: 15.0,
            tilt_axis: 30.0,
        },
    }
}

/// The room the nodes are in.
struct Room {
    peers: Vec<Peer>,
    views: Vec<View>,
    now_ms: u64,
    pictures: u64,
    /// What each viewer last photographed, to take a picture only when a
    /// screen in its view changed.
    last: Vec<Vec<Option<Vec<u8>>>>,
}

impl Room {
    fn new(peers: Vec<Peer>, views: Vec<View>) -> Room {
        let n = peers.len();
        let mut room = Room {
            peers,
            views,
            now_ms: 0,
            pictures: 0,
            last: vec![vec![None; n]; n],
        };
        // The first tick opens the cameras; let them open.
        room.step();
        for p in &room.peers {
            p.settle();
        }
        room
    }

    fn camera(&self) -> Camera {
        Camera {
            width: 960,
            height: 540,
            ..Camera::default()
        }
    }

    /// One tick of every node, then a picture for every viewer whose view
    /// changed.
    fn step(&mut self) {
        self.now_ms += TICK_MS;
        for p in &mut self.peers {
            p.tick(self.now_ms);
        }
        for viewer in 0..self.peers.len() {
            let mut changed = false;
            let mut codes = Vec::new();
            for v in self.views.iter().filter(|v| v.viewer == viewer) {
                let shown = self.peers[v.shown].screen.clone();
                let bits = shown.as_ref().map(|s| s.bits.clone());
                if self.last[viewer][v.shown] != bits {
                    changed = true;
                    self.last[viewer][v.shown] = bits;
                }
                if let Some(s) = shown {
                    codes.push(Code {
                        n: s.spec.cells as usize,
                        grid: s.grid(),
                        pose: v.pose.clone(),
                        occlude: None,
                    });
                }
            }
            if !changed || codes.is_empty() || !self.peers[viewer].cam.is_running() {
                continue;
            }
            self.pictures += 1;
            let picture = shoot(&codes, &self.camera(), self.pictures);
            let peer = &self.peers[viewer];
            peer.cam.add(picture);
            peer.cam.show_next();
            peer.settle();
        }
    }

    /// Step until `done` holds for the room, or `ms` pass.
    fn run_until(&mut self, ms: u64, done: impl Fn(&Room) -> bool) -> bool {
        let end = self.now_ms + ms;
        while self.now_ms < end {
            self.step();
            if done(self) {
                return true;
            }
        }
        false
    }

    fn run(&mut self, ms: u64) {
        self.run_until(ms, |_| false);
    }
}

fn messages(log: &[NodeEvent]) -> Vec<(String, Channel)> {
    log.iter()
        .filter_map(|e| match e {
            NodeEvent::Message(m) => Some((m.text.clone(), m.channel)),
            _ => None,
        })
        .collect()
}

fn outbox(log: &[NodeEvent], id: [u8; 16]) -> Vec<OutboxState> {
    log.iter()
        .filter_map(|e| match e {
            NodeEvent::Outbox { id: i, state } if *i == id => Some(*state),
            _ => None,
        })
        .collect()
}

fn light_states(log: &[NodeEvent]) -> Vec<String> {
    log.iter()
        .filter_map(|e| match e {
            NodeEvent::ChannelState {
                channel: Channel::Light,
                state,
            } => Some(state.clone()),
            _ => None,
        })
        .collect()
}

fn heard(log: &[NodeEvent], id: [u8; 16]) -> bool {
    log.iter()
        .any(|e| matches!(e, NodeEvent::Heard { id: i, .. } if *i == id))
}

fn displays(log: &[NodeEvent]) -> Vec<(u16, u32)> {
    log.iter()
        .filter_map(|e| match e {
            NodeEvent::Display { tag, index, .. } => Some((*tag, *index)),
            _ => None,
        })
        .collect()
}

fn offer(log: &[NodeEvent]) -> Option<(PublicIdentity, String)> {
    log.iter().find_map(|e| match e {
        NodeEvent::PairOffer { peer, name, .. } => Some((peer.clone(), name.clone())),
        _ => None,
    })
}

#[test]
fn pair_message_and_receipt_by_light_alone() {
    let dir = TempDir::new().unwrap();
    let a = Peer::new(&dir, "ann", FakeCamera::new(Vec::new()));
    let b = Peer::new(&dir, "ben", FakeCamera::new(Vec::new()));
    let mut room = Room::new(vec![a, b], vec![facing(0, 1), facing(1, 0)]);

    // Both hold up their hellos at once; each camera reads the other's,
    // and each hello keeps showing until its person confirms.
    room.peers[0].node.start_pairing("Ann").unwrap();
    room.peers[1].node.start_pairing("Ben").unwrap();
    assert!(room.run_until(10_000, |r| offer(&r.peers[0].log).is_some()
        && offer(&r.peers[1].log).is_some()));
    let (ann, name) = offer(&room.peers[1].log).unwrap();
    assert_eq!(
        (ann.clone(), name.as_str()),
        (room.peers[0].me.clone(), "Ann")
    );
    let (ben, name) = offer(&room.peers[0].log).unwrap();
    assert_eq!(
        (ben.clone(), name.as_str()),
        (room.peers[1].me.clone(), "Ben")
    );
    assert!(room.peers.iter().all(|p| p.node.pairing()));
    let ann_id = room.peers[1].node.confirm_pairing(&ann, "Ann");
    let ben_id = room.peers[0].node.confirm_pairing(&ben, "Ben");
    assert!(room.peers.iter().all(|p| !p.node.pairing()));
    room.step();
    room.step();
    assert!(
        room.peers.iter().all(|p| p.screen.is_none()),
        "the hellos stop showing once confirmed"
    );

    // A message, and its receipt, by light.
    let id = room.peers[0].node.send(&[ben_id], TEXT, Channel::Light);
    assert!(room.run_until(10_000, |r| heard(&r.peers[0].log, id)));
    assert_eq!(
        messages(&room.peers[1].log),
        vec![(TEXT.to_string(), Channel::Light)]
    );
    // its chunks reach the drums as a sound message's do, saying they came by light
    let chunks: Vec<Channel> = room.peers[1]
        .log
        .iter()
        .filter_map(|e| match e {
            NodeEvent::ChunkLanded { channel, .. } => Some(*channel),
            _ => None,
        })
        .collect();
    assert!(!chunks.is_empty() && chunks.iter().all(|c| *c == Channel::Light));
    assert_eq!(
        room.peers[1].node.conversation(ann_id).last().unwrap().text,
        TEXT
    );
    // The receipt ends the sending window: reported on the next tick.
    room.step();
    let states = outbox(&room.peers[0].log, id);
    assert_eq!(
        states,
        vec![
            OutboxState::Waiting,
            OutboxState::Playing,
            OutboxState::Sent
        ]
    );
    assert!(
        room.peers[0]
            .node
            .conversation(ben_id)
            .last()
            .unwrap()
            .heard
    );

    // Both screens are clear once the windows end (the receipt runs its
    // full 4 s floor), and the message is never delivered twice.
    room.run(5_000);
    assert!(room.peers.iter().all(|p| p.screen.is_none()));
    assert_eq!(messages(&room.peers[1].log).len(), 1);
}

/// Contacts already paired, each seeing the other.
fn paired(dir: &TempDir) -> (Room, ContactId, ContactId) {
    let mut a = Peer::new(dir, "ann", FakeCamera::new(Vec::new()));
    let mut b = Peer::new(dir, "ben", FakeCamera::new(Vec::new()));
    let ben = a.add(&b);
    let ann = b.add(&a);
    (
        Room::new(vec![a, b], vec![facing(0, 1), facing(1, 0)]),
        ben,
        ann,
    )
}

#[test]
fn a_receiver_that_joins_mid_sequence_still_gets_the_message() {
    let dir = TempDir::new().unwrap();
    let (mut room, ben, _) = paired(&dir);
    // Ben looks away while Ann starts showing a long message.
    room.views.retain(|v| v.viewer != 1);
    let long = "Bridge out at mile nine, take the ridge road. ".repeat(20);
    let id = room.peers[0].node.send(&[ben], &long, Channel::Light);
    room.run(1_200);
    let shown_before = displays(&room.peers[0].log).len();
    assert!(
        shown_before >= 10,
        "only {shown_before} frames went by unseen"
    );
    assert!(outbox(&room.peers[0].log, id).ends_with(&[OutboxState::Playing]));

    // He looks now: the source frames are long gone, repair frames do it.
    room.views.push(facing(1, 0));
    assert!(room.run_until(10_000, |r| !messages(&r.peers[1].log).is_empty()));
    assert_eq!(
        messages(&room.peers[1].log),
        vec![(long.clone(), Channel::Light)]
    );
    assert!(light_states(&room.peers[1].log).contains(&"receiving".to_string()));
}

#[test]
fn two_senders_in_view_both_arrive() {
    let dir = TempDir::new().unwrap();
    let mut a = Peer::new(&dir, "ann", FakeCamera::new(Vec::new()));
    let mut b = Peer::new(&dir, "ben", FakeCamera::new(Vec::new()));
    let mut c = Peer::new(&dir, "cy", FakeCamera::new(Vec::new()));
    let (ben_for_a, ben_for_c) = (a.add(&b), c.add(&b));
    b.add(&a);
    b.add(&c);
    let side = |shown, x| View {
        viewer: 1,
        shown,
        pose: Pose {
            scale: 0.5,
            centre: (x, 0.5),
            roll: -4.0,
            tilt: 10.0,
            tilt_axis: 90.0,
        },
    };
    let mut room = Room::new(vec![a, b, c], vec![side(0, 0.27), side(2, 0.73)]);
    room.peers[0].node.send(
        &[ben_for_a],
        "From Ann, by the left screen.",
        Channel::Light,
    );
    room.peers[2].node.send(
        &[ben_for_c],
        "From Cy, by the right screen.",
        Channel::Light,
    );
    assert!(room.run_until(10_000, |r| messages(&r.peers[1].log).len() == 2));
    let mut got: Vec<String> = messages(&room.peers[1].log)
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            "From Ann, by the left screen.".to_string(),
            "From Cy, by the right screen.".to_string()
        ]
    );
}

#[test]
fn off_mid_read_closes_the_camera() {
    let dir = TempDir::new().unwrap();
    let (mut room, ben, _) = paired(&dir);
    let long = "Bridge out at mile nine, take the ridge road. ".repeat(20);
    room.peers[0].node.send(&[ben], &long, Channel::Light);
    assert!(room.run_until(5_000, |r| light_states(&r.peers[1].log)
        .contains(&"receiving".to_string())));
    assert!(room.peers[1].cam.is_running());

    room.peers[1].node.set_light(LightSwitch::Off);
    room.step();
    room.peers[1].settle();
    assert!(!room.peers[1].cam.is_running(), "the camera is closed");
    assert_eq!(room.peers[1].cam.stops(), 1);
    assert_eq!(light_states(&room.peers[1].log).last().unwrap(), "off");

    // Ann keeps showing, but nothing reaches Ben, and his camera stays shut.
    room.run(5_000);
    assert!(messages(&room.peers[1].log).is_empty());
    assert!(!room.peers[1].cam.is_running());
    assert_eq!(room.peers[1].cam.starts(), 1);
}

#[test]
fn off_mid_flash_clears_the_screen_and_the_outbox_says_waiting() {
    let dir = TempDir::new().unwrap();
    let (mut room, ben, _) = paired(&dir);
    room.views.clear();
    let long = "Bridge out at mile nine, take the ridge road. ".repeat(20);
    let id = room.peers[0].node.send(&[ben], &long, Channel::Light);
    room.run(500);
    let before = displays(&room.peers[0].log);
    assert!(!before.is_empty());
    assert!(room.peers[0].screen.is_some());

    // Read only: the screen clears on the next tick, and stays clear.
    room.peers[0].node.set_light(LightSwitch::Read);
    room.step();
    assert!(room.peers[0].screen.is_none(), "cleared at once");
    assert_eq!(
        outbox(&room.peers[0].log, id).last(),
        Some(&OutboxState::Waiting)
    );
    room.run(2_000);
    assert_eq!(displays(&room.peers[0].log), before, "nothing more shown");
    assert!(room.peers[0].cam.is_running(), "Read still reads");

    // Off: the camera closes too, and the message still waits.
    room.peers[0].node.set_light(LightSwitch::Off);
    room.step();
    room.peers[0].settle();
    assert!(!room.peers[0].cam.is_running());
    room.run(2_000);
    assert_eq!(displays(&room.peers[0].log), before);
    assert_eq!(
        outbox(&room.peers[0].log, id).last(),
        Some(&OutboxState::Waiting)
    );
    let kept = room.peers[0].node.conversation(ben);
    assert_eq!(kept.last().unwrap().delivery.as_str(), "waiting");

    // Back at Read and flash it carries on from the frame it stopped at,
    // and finishes its window.
    room.peers[0].node.set_light(LightSwitch::ReadAndFlash);
    assert!(
        room.run_until(20_000, |r| outbox(&r.peers[0].log, id).last()
            == Some(&OutboxState::Sent))
    );
    let shown = displays(&room.peers[0].log);
    let indices: Vec<u32> = shown.iter().map(|(_, i)| *i).collect();
    assert_eq!(indices, (0..indices.len() as u32).collect::<Vec<_>>());
    assert_eq!(
        outbox(&room.peers[0].log, id),
        vec![
            OutboxState::Waiting,
            OutboxState::Playing,
            OutboxState::Waiting,
            OutboxState::Playing,
            OutboxState::Sent
        ]
    );
}

#[test]
fn a_denied_camera_reports_light_unavailable() {
    let dir = TempDir::new().unwrap();
    let mut a = Peer::new(&dir, "ann", FakeCamera::denied());
    a.tick(20);
    a.settle();
    a.tick(40);
    assert!(light_states(&a.log).contains(&"unavailable: camera permission denied".to_string()));
    assert_eq!(a.cam.starts(), 1);
    a.tick(60);
    a.settle();
    assert_eq!(a.cam.starts(), 1, "not asked again until the switch moves");
}
