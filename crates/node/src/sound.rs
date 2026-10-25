//! The sound transport: frames out through the FSK modem and the speaker
//! once the room is quiet, frames in from the microphone through the
//! demodulator.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bombechat_modem::{
    dbfs_to_room_db, modulate, AudioError, AudioIo, DemodEvent, Demodulator, ModemConfig,
    QuietGate, DEFAULT_CALIBRATION_OFFSET,
};
use bombechat_store::Channel;

use crate::transport::{Transport, TransportEvent};

/// Most microphone audio held between polls: 60 s. If the node stops
/// polling for longer than that, the oldest audio is dropped rather than
/// growing without bound.
const MAX_BUFFERED_SECONDS: usize = 60;

/// Most envelopes (distinct frame tags) waiting in the gate at once. In a
/// room that never quiets the queue would otherwise grow without bound;
/// beyond this, new envelopes are refused with a notice.
pub const MAX_QUEUED_ENVELOPES: usize = 32;

/// The notice sent when an envelope is refused for a full queue.
pub const QUEUE_FULL: &str = "send queue full, message not sent";

/// Whether the microphone has been opened yet.
enum Input {
    NotStarted,
    Listening,
    Failed,
}

/// [`Transport`] over an [`AudioIo`] device and the FSK modem.
///
/// The microphone is opened on the first [`poll`](Transport::poll); if
/// that fails (for example, permission denied) the failure is reported
/// once as [`TransportEvent::Unavailable`] and the transport stays quiet
/// afterwards. Errors a running stream reports later (via
/// [`AudioIo::take_error`]) are reported the same way.
///
/// Every frame handed to [`send`](Transport::send) is queued in the
/// [`QuietGate`] (as bytes; it is modulated when it plays). Each poll
/// feeds the gate the room level from the demodulator and plays whatever
/// it says is due. At most [`MAX_QUEUED_ENVELOPES`] envelopes wait at
/// once; frames of a further envelope are refused and reported once as
/// [`TransportEvent::Notice`] with [`QUEUE_FULL`].
///
/// Each envelope's progress is reported by its frame tag:
/// [`TransportEvent::Playing`] when its first frame goes to the speaker,
/// and [`TransportEvent::Sent`] once its last frame's last repeat has
/// finished playing (the speaker plays what it is handed in order, so
/// that is when everything handed to it before has played too).
pub struct SoundTransport {
    io: Box<dyn AudioIo>,
    cfg: ModemConfig,
    gate: QuietGate,
    demod: Demodulator,
    input: Input,
    heard: Arc<Mutex<Vec<f32>>>,
    /// Each frame still in the gate, by gate item.
    queued: HashMap<usize, Vec<u8>>,
    /// The tag of the envelope last refused, so it is reported once.
    refused_tag: Option<Option<u16>>,
    /// Events raised outside a poll, reported on the next one.
    pending: Vec<TransportEvent>,
    next_item: usize,
    /// Whether the last play failed, so a dead speaker is reported once,
    /// not once per frame.
    play_failed: bool,
    /// Envelopes that have started playing, by tag, with when the last
    /// of their audio handed to the speaker so far finishes, in `now_ms`.
    playing: HashMap<u16, u64>,
    /// When everything handed to the speaker so far finishes, in `now_ms`.
    speaker_free_ms: u64,
}

impl SoundTransport {
    pub fn new(io: Box<dyn AudioIo>, cfg: ModemConfig, gate: QuietGate) -> SoundTransport {
        SoundTransport {
            io,
            demod: Demodulator::new(cfg),
            cfg,
            gate,
            input: Input::NotStarted,
            heard: Arc::new(Mutex::new(Vec::new())),
            queued: HashMap::new(),
            next_item: 0,
            refused_tag: None,
            pending: Vec::new(),
            play_failed: false,
            playing: HashMap::new(),
            speaker_free_ms: 0,
        }
    }

    fn start_input(&mut self) -> Option<TransportEvent> {
        let heard = self.heard.clone();
        let cap = MAX_BUFFERED_SECONDS * self.io.sample_rate() as usize;
        let sink = Box::new(move |samples: &[f32]| {
            let mut buf = heard.lock().unwrap_or_else(|p| p.into_inner());
            buf.extend_from_slice(samples);
            if buf.len() > cap {
                let excess = buf.len() - cap;
                buf.drain(..excess);
            }
        });
        match self.io.start_input(sink) {
            Ok(()) => {
                self.input = Input::Listening;
                None
            }
            Err(e) => {
                self.input = Input::Failed;
                Some(TransportEvent::Unavailable(reason(&e)))
            }
        }
    }
}

/// The envelope tag a frame from `bombechat_modem::split` carries.
fn frame_tag(frame: &[u8]) -> Option<u16> {
    frame.get(..2).map(|t| u16::from_be_bytes([t[0], t[1]]))
}

/// The reason shown after "unavailable: " for an audio failure.
fn reason(e: &AudioError) -> String {
    match e {
        AudioError::PermissionDenied => "microphone permission denied".to_string(),
        AudioError::NoDevice => "no audio device".to_string(),
        AudioError::Unsupported(detail) => format!("audio device unsupported ({detail})"),
    }
}

impl Transport for SoundTransport {
    fn name(&self) -> Channel {
        Channel::Sound
    }

    fn send(&mut self, frame: Vec<u8>) {
        let tag = frame_tag(&frame);
        let tags: HashSet<Option<u16>> = self.queued.values().map(|f| frame_tag(f)).collect();
        if !tags.contains(&tag) && tags.len() >= MAX_QUEUED_ENVELOPES {
            if self.refused_tag != Some(tag) {
                self.refused_tag = Some(tag);
                self.pending
                    .push(TransportEvent::Notice(QUEUE_FULL.to_string()));
            }
            return;
        }
        let item = self.next_item;
        self.next_item = self.next_item.wrapping_add(1);
        self.queued.insert(item, frame);
        self.gate.enqueue(item);
    }

    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent> {
        let mut events = std::mem::take(&mut self.pending);

        if matches!(self.input, Input::NotStarted) {
            events.extend(self.start_input());
        }
        if let Some(e) = self.io.take_error() {
            events.push(TransportEvent::Unavailable(reason(&e)));
        }

        let samples = std::mem::take(&mut *self.heard.lock().unwrap_or_else(|p| p.into_inner()));
        for event in self.demod.push(&samples) {
            match event {
                DemodEvent::Frame(bytes) => events.push(TransportEvent::Frame(bytes)),
                DemodEvent::Level { dbfs } => {
                    let room = dbfs_to_room_db(dbfs, DEFAULT_CALIBRATION_OFFSET);
                    self.gate.observe(room, now_ms);
                    events.push(TransportEvent::RoomDb(room));
                }
            }
        }

        while let Some(item) = self.gate.due(now_ms) {
            let Some(frame) = self.queued.get(&item) else {
                continue;
            };
            let samples = modulate(&self.cfg, frame);
            let tag = frame_tag(frame);
            if !self.gate.is_queued(item) {
                self.queued.remove(&item);
            }
            let rate = u64::from(self.io.sample_rate().max(1));
            let ends =
                self.speaker_free_ms.max(now_ms) + (samples.len() as u64 * 1000).div_ceil(rate);
            self.speaker_free_ms = ends;
            if let Some(tag) = tag {
                if self.playing.insert(tag, ends).is_none() {
                    events.push(TransportEvent::Playing(tag));
                }
            }
            match self.io.play(samples) {
                Ok(()) => self.play_failed = false,
                Err(e) => {
                    if !self.play_failed {
                        events.push(TransportEvent::Unavailable(reason(&e)));
                    }
                    self.play_failed = true;
                }
            }
        }

        let still_queued: HashSet<Option<u16>> =
            self.queued.values().map(|f| frame_tag(f)).collect();
        let mut done: Vec<u16> = self
            .playing
            .iter()
            .filter(|(tag, ends)| now_ms >= **ends && !still_queued.contains(&Some(**tag)))
            .map(|(tag, _)| *tag)
            .collect();
        done.sort_unstable();
        for tag in done {
            self.playing.remove(&tag);
            events.push(TransportEvent::Sent(tag));
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Node, NodeEvent, OutboxState};
    use bombechat_core::Identity;
    use bombechat_modem::split;
    use bombechat_store::Store;

    type Sink = Box<dyn FnMut(&[f32]) + Send>;

    /// An audio device the test drives by hand.
    #[derive(Clone, Default)]
    struct Scripted {
        sink: Arc<Mutex<Option<Sink>>>,
        error: Arc<Mutex<Option<AudioError>>>,
        played: Arc<Mutex<Vec<Vec<f32>>>>,
    }

    impl Scripted {
        /// 200 ms of near silence, enough for two quiet level readings.
        fn quiet(&self) {
            if let Some(sink) = self.sink.lock().unwrap().as_mut() {
                sink(&vec![0.0; 9_600]);
            }
        }
    }

    impl AudioIo for Scripted {
        fn start_input(&mut self, sink: Sink) -> Result<(), AudioError> {
            *self.sink.lock().unwrap() = Some(sink);
            Ok(())
        }
        fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
            self.played.lock().unwrap().push(samples);
            Ok(())
        }
        fn sample_rate(&self) -> u32 {
            48_000
        }
        fn take_error(&mut self) -> Option<AudioError> {
            self.error.lock().unwrap().take()
        }
    }

    fn transport(io: &Scripted) -> SoundTransport {
        SoundTransport::new(
            Box::new(io.clone()),
            ModemConfig::default(),
            QuietGate::new(40.0, 0, 90),
        )
    }

    #[test]
    fn a_stream_error_reaches_the_node_as_channel_state() {
        let io = Scripted::default();
        let mut node = Node::new(None, vec![Box::new(transport(&io))]);
        assert!(!node
            .tick(0)
            .iter()
            .any(|e| matches!(e, NodeEvent::ChannelState { .. })));

        *io.error.lock().unwrap() = Some(AudioError::NoDevice);
        let states: Vec<_> = node
            .tick(20)
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::ChannelState { channel, state } => Some((channel, state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![(Channel::Sound, "unavailable: no audio device".to_string())]
        );
    }

    #[test]
    fn frames_are_modulated_when_they_play() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        let frame = split(b"hello", 7).remove(0);
        t.send(frame.clone());
        assert!(io.played.lock().unwrap().is_empty(), "nothing before quiet");

        io.quiet();
        t.poll(100);
        let played = io.played.lock().unwrap().clone();
        assert_eq!(played, vec![modulate(&ModemConfig::default(), &frame)]);
        assert!(t.queued.is_empty(), "played frame dropped");
    }

    fn progress(events: Vec<TransportEvent>) -> Vec<TransportEvent> {
        events
            .into_iter()
            .filter(|e| matches!(e, TransportEvent::Playing(_) | TransportEvent::Sent(_)))
            .collect()
    }

    #[test]
    fn an_envelope_is_playing_from_its_first_frame_and_sent_after_its_last_repeat() {
        let io = Scripted::default();
        let mut t = SoundTransport::new(
            Box::new(io.clone()),
            ModemConfig::default(),
            QuietGate::new(40.0, 1, 90),
        );
        t.poll(0);
        let frames = split(&[1; 100], 7);
        assert!(frames.len() > 1);
        for f in frames {
            t.send(f);
        }
        assert!(progress(t.poll(10)).is_empty(), "waiting for quiet");

        io.quiet();
        assert_eq!(progress(t.poll(100)), vec![TransportEvent::Playing(7)]);
        assert!(progress(t.poll(60_000)).is_empty(), "one repeat to go");

        // The repeat is handed to the speaker at 90.1 s, but is only sent
        // once the speaker has finished playing it.
        assert!(progress(t.poll(90_100)).is_empty());
        assert!(progress(t.poll(90_101)).is_empty());
        assert_eq!(progress(t.poll(200_000)), vec![TransportEvent::Sent(7)]);
        assert!(progress(t.poll(300_000)).is_empty(), "reported once");
    }

    fn outbox(events: Vec<NodeEvent>) -> Vec<([u8; 16], OutboxState)> {
        events
            .into_iter()
            .filter_map(|e| match e {
                NodeEvent::Outbox { id, state } => Some((id, state)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_send_is_waiting_then_playing_then_sent() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, them) = (Identity::generate(), Identity::generate());
        let store = Store::create(&dir.path().join("m.db"), "pass", &me, "me").unwrap();
        let contact = store.add_contact("them", &them.public(), true).unwrap();
        let io = Scripted::default();
        let mut node = Node::new(Some(store), vec![Box::new(transport(&io))]);
        node.tick(0);

        let id = node.send(&[contact], "hi", Channel::Sound);
        assert_eq!(outbox(node.tick(10)), vec![(id, OutboxState::Waiting)]);
        io.quiet();
        assert_eq!(outbox(node.tick(100)), vec![(id, OutboxState::Playing)]);
        assert_eq!(outbox(node.tick(100_000)), vec![(id, OutboxState::Sent)]);
        assert!(outbox(node.tick(200_000)).is_empty());
    }

    #[test]
    fn a_full_queue_refuses_new_envelopes_once() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        for tag in 0..MAX_QUEUED_ENVELOPES as u16 {
            for f in split(&[1; 100], tag) {
                t.send(f);
            }
        }
        assert!(t.poll(10).is_empty(), "the room never quieted; all queued");

        let before = t.queued.len();
        for f in split(&[1; 100], 999) {
            t.send(f);
        }
        assert_eq!(t.queued.len(), before, "the extra envelope is refused");
        assert_eq!(
            t.poll(20),
            vec![TransportEvent::Notice(QUEUE_FULL.to_string())]
        );

        // Frames of an envelope already queued are still accepted.
        t.send(split(&[2; 10], 0).remove(0));
        assert_eq!(t.queued.len(), before + 1);
    }
}
