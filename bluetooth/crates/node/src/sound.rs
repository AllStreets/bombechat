//! The sound transport: frames out through the FSK modem and the speaker
//! once the room is quiet, frames in from the microphone through the
//! demodulator.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use bombechat_modem::{
    dbfs_to_room_db, measured_rate, modulate, AudioError, AudioIo, Band, DemodEvent, Demodulator,
    ModemConfig, QuietGate, DEFAULT_CALIBRATION_OFFSET,
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
    /// Never started, and will be as soon as [`SoundTransport::poll`] next
    /// runs (unless listening is off, in which case it goes straight to
    /// [`Input::Stopped`] instead).
    NotStarted,
    Listening,
    /// Stopped by [`Transport::set_listening`] turning off, with the
    /// stream (and the microphone) released. Goes back to
    /// [`Input::NotStarted`] when listening turns on again.
    Stopped,
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
/// With [`set_listening`](Transport::set_listening) off, the microphone
/// is closed (no frames and no room level) until it turns on again; with
/// [`set_sending`](Transport::set_sending) off the queue waits and nothing
/// plays.
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
/// that is when everything handed to it before has played too). An
/// envelope refused for a full queue, or with a frame the speaker would
/// not play, is reported [`TransportEvent::Dropped`] instead.
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
    /// What has been handed to the speaker and has not finished, in the
    /// order it plays: each frame's tag, and when it starts and ends.
    handed: VecDeque<(Option<u16>, u64, u64)>,
    /// The last poll's `now_ms`.
    now_ms: u64,
    /// Envelopes with a frame the speaker refused to play, by tag.
    unplayed: HashSet<u16>,
    /// Off: what the microphone hears is let go undemodulated.
    listening: bool,
    /// Off: nothing plays; the gate's queue waits.
    sending: bool,
    /// The gain on every sample played, 0 to 1.
    gain: f32,
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
            handed: VecDeque::new(),
            now_ms: 0,
            unplayed: HashSet::new(),
            listening: true,
            sending: true,
            gain: 1.0,
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

    /// The modem's [`measured_rate`] at the configuration it runs at.
    fn rate(&self) -> Option<f32> {
        Some(measured_rate(&self.cfg))
    }

    /// Off, the microphone is actually closed (the input stream is
    /// dropped), not just ignored: so the OS recording indicator goes
    /// out too. On again, [`SoundTransport::poll`] opens a fresh stream
    /// on its next run.
    fn set_listening(&mut self, on: bool) {
        self.listening = on;
        match (on, &self.input) {
            (false, Input::Listening) => {
                self.io.stop_input();
                self.input = Input::Stopped;
            }
            (false, Input::NotStarted) => self.input = Input::Stopped,
            (true, Input::Stopped) => self.input = Input::NotStarted,
            _ => {}
        }
    }

    fn set_sending(&mut self, on: bool) {
        self.sending = on;
    }

    /// Frames of `tag` still in the gate (first plays or repeats) are
    /// let go, and what of it was already handed to the speaker and has
    /// not played yet is cut ([`AudioIo::cut`]), so it falls silent at
    /// once rather than playing out. Only `tag`'s own audio stops: any
    /// other envelope handed to the speaker plays on, just sooner. The
    /// envelope is reported sent on the next poll, if any of it played.
    fn withdraw(&mut self, tag: u16) {
        let gate = &mut self.gate;
        self.queued.retain(|item, frame| {
            let keep = frame_tag(frame) != Some(tag);
            if !keep {
                gate.forget(*item);
            }
            keep
        });
        if self.io.cut(tag) == 0 {
            return;
        }
        // its frames leave the speaker's timeline, and what was queued
        // behind each moves up by what of it was cut
        let now = self.now_ms;
        let mut shift = 0;
        for (t, start, end) in self.handed.iter_mut() {
            if *t == Some(tag) {
                shift += end.saturating_sub((*start).max(now));
            } else {
                *start -= shift;
                *end -= shift;
            }
        }
        self.handed.retain(|(t, _, _)| *t != Some(tag));
        self.speaker_free_ms = self.handed.back().map_or(now, |c| c.2);
        let handed = &self.handed;
        for (t, ends) in self.playing.iter_mut() {
            *ends = if *t == tag {
                0
            } else {
                handed
                    .iter()
                    .filter(|c| c.0 == Some(*t))
                    .map(|c| c.2)
                    .max()
                    .unwrap_or(*ends)
            };
        }
    }

    fn set_send_under(&mut self, db: f32) {
        self.gate.set_send_under(db);
    }

    /// Takes effect from the next frame handed to the speaker; one
    /// already playing finishes as it was.
    fn set_volume(&mut self, gain: f32) {
        self.gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            1.0
        };
    }

    /// Frames already queued follow the new count; one that has already
    /// played as often as it now should is let go (and its envelope
    /// reported sent once it finishes).
    fn set_repeats(&mut self, repeats: u8) {
        self.gate.set_repeats(repeats);
        let gate = &self.gate;
        self.queued.retain(|item, _| gate.is_queued(*item));
    }

    /// A sample rate too low for the near-silent band keeps the audible
    /// one. Queued frames are modulated when they play, so they go out on
    /// the new band; the demodulator starts afresh, dropping whatever
    /// was part way through arriving.
    fn set_band(&mut self, band: Band) {
        let wanted = ModemConfig { band, ..self.cfg };
        let fallback = ModemConfig {
            band: Band::Audible,
            ..self.cfg
        };
        let Some(cfg) = [wanted, fallback]
            .into_iter()
            .find(|c| c.validate().is_ok())
        else {
            return;
        };
        if cfg.band != self.cfg.band {
            self.cfg = cfg;
            self.demod = Demodulator::new(cfg);
        }
    }

    fn band(&self) -> Option<Band> {
        Some(self.cfg.band)
    }

    fn send(&mut self, frame: Vec<u8>) {
        let tag = frame_tag(&frame);
        let tags: HashSet<Option<u16>> = self.queued.values().map(|f| frame_tag(f)).collect();
        if !tags.contains(&tag) && tags.len() >= MAX_QUEUED_ENVELOPES {
            if self.refused_tag != Some(tag) {
                self.refused_tag = Some(tag);
                self.pending
                    .push(TransportEvent::Notice(QUEUE_FULL.to_string()));
                if let Some(tag) = tag {
                    self.pending.push(TransportEvent::Dropped(tag));
                }
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
        self.now_ms = now_ms;
        while self.handed.front().is_some_and(|c| c.2 <= now_ms) {
            self.handed.pop_front();
        }

        if matches!(self.input, Input::NotStarted) {
            events.extend(self.start_input());
        }
        if let Some(e) = self.io.take_error() {
            events.push(TransportEvent::Unavailable(reason(&e)));
        }

        let samples = std::mem::take(&mut *self.heard.lock().unwrap_or_else(|p| p.into_inner()));
        let samples = if self.listening { samples } else { Vec::new() };
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

        while let Some(item) = self.sending.then(|| self.gate.due(now_ms)).flatten() {
            let Some(frame) = self.queued.get(&item) else {
                continue;
            };
            let mut samples = modulate(&self.cfg, frame);
            if self.gain != 1.0 {
                samples.iter_mut().for_each(|x| *x *= self.gain);
            }
            let tag = frame_tag(frame);
            if !self.gate.is_queued(item) {
                self.queued.remove(&item);
            }
            let rate = u64::from(self.io.sample_rate().max(1));
            let starts = self.speaker_free_ms.max(now_ms);
            let ends = starts + (samples.len() as u64 * 1000).div_ceil(rate);
            self.speaker_free_ms = ends;
            self.handed.push_back((tag, starts, ends));
            if let Some(tag) = tag {
                if self.playing.insert(tag, ends).is_none() {
                    events.push(TransportEvent::Playing(tag));
                }
            }
            let played = match tag {
                Some(tag) => self.io.play_for(tag, samples),
                None => self.io.play(samples),
            };
            match played {
                Ok(()) => self.play_failed = false,
                Err(e) => {
                    if let Some(tag) = tag {
                        self.unplayed.insert(tag);
                    }
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
            events.push(if self.unplayed.remove(&tag) {
                TransportEvent::Dropped(tag)
            } else {
                TransportEvent::Sent(tag)
            });
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
        fail_play: Arc<Mutex<bool>>,
        starts: Arc<Mutex<u32>>,
        stops: Arc<Mutex<u32>>,
    }

    impl Scripted {
        /// 200 ms of near silence, enough for two quiet level readings.
        fn quiet(&self) {
            if let Some(sink) = self.sink.lock().unwrap().as_mut() {
                sink(&vec![0.0; 9_600]);
            }
        }

        fn starts(&self) -> u32 {
            *self.starts.lock().unwrap()
        }

        fn stops(&self) -> u32 {
            *self.stops.lock().unwrap()
        }

        /// Whether the stream is open: a sink is registered.
        fn is_open(&self) -> bool {
            self.sink.lock().unwrap().is_some()
        }
    }

    impl AudioIo for Scripted {
        fn start_input(&mut self, sink: Sink) -> Result<(), AudioError> {
            *self.starts.lock().unwrap() += 1;
            *self.sink.lock().unwrap() = Some(sink);
            Ok(())
        }
        fn stop_input(&mut self) {
            *self.stops.lock().unwrap() += 1;
            *self.sink.lock().unwrap() = None;
        }
        fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
            if *self.fail_play.lock().unwrap() {
                return Err(AudioError::NoDevice);
            }
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
    fn the_rate_it_reports_is_the_modems_measured_rate() {
        let io = Scripted::default();
        let rate = bombechat_modem::measured_rate(&ModemConfig::default());
        assert_eq!(transport(&io).rate(), Some(rate));
        let node = Node::new(None, vec![Box::new(transport(&io))]);
        assert_eq!(node.rates(), vec![(Channel::Sound, rate)]);
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
            .filter(|e| {
                matches!(
                    e,
                    TransportEvent::Playing(_)
                        | TransportEvent::Sent(_)
                        | TransportEvent::Dropped(_)
                )
            })
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
    fn an_envelope_the_speaker_never_played_is_dropped_not_sent() {
        let io = Scripted::default();
        *io.fail_play.lock().unwrap() = true;
        let mut t = transport(&io);
        t.poll(0);
        for f in split(&[1; 100], 4) {
            t.send(f);
        }
        io.quiet();
        let events = t.poll(100);
        assert!(events.contains(&TransportEvent::Unavailable("no audio device".to_string())));
        assert!(progress(t.poll(100_000)).contains(&TransportEvent::Dropped(4)));
    }

    #[test]
    fn a_send_the_queue_refuses_leaves_the_outbox_as_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, them) = (Identity::generate(), Identity::generate());
        let store = Store::create(&dir.path().join("m.db"), "pass", &me, "me").unwrap();
        let contact = store.add_contact("them", &them.public(), true).unwrap();
        let io = Scripted::default();
        let mut node = Node::new(Some(store), vec![Box::new(transport(&io))]);
        node.tick(0);
        let ids: Vec<_> = (0..=MAX_QUEUED_ENVELOPES)
            .map(|i| node.send(&[contact], &format!("m{i}"), Channel::Sound))
            .collect();
        let report = outbox(node.tick(10));
        let last = *ids.last().unwrap();
        assert!(report.contains(&(last, OutboxState::Failed)));
        assert!(!report.contains(&(ids[0], OutboxState::Failed)));
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
            vec![
                TransportEvent::Notice(QUEUE_FULL.to_string()),
                TransportEvent::Dropped(999)
            ]
        );

        // Frames of an envelope already queued are still accepted.
        t.send(split(&[2; 10], 0).remove(0));
        assert_eq!(t.queued.len(), before + 1);
    }

    /// 200 ms at a steady -60 dBFS: about 34 dB in the room.
    fn murmur(io: &Scripted) {
        if let Some(sink) = io.sink.lock().unwrap().as_mut() {
            sink(&vec![0.001; 9_600]);
        }
    }

    #[test]
    fn with_listening_off_what_the_microphone_hears_is_let_go() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        let frame = split(b"hello", 7).remove(0);
        let heard = |events: &[TransportEvent]| {
            events
                .iter()
                .filter(|e| matches!(e, TransportEvent::Frame(_) | TransportEvent::RoomDb(_)))
                .count()
        };
        let say = |io: &Scripted| {
            let mut x = modulate(&ModemConfig::default(), &frame);
            x.extend(vec![0.0; 9_600]);
            if let Some(sink) = io.sink.lock().unwrap().as_mut() {
                sink(&x);
            }
        };

        assert_eq!(io.starts(), 1, "poll(0) opened the microphone");
        t.set_listening(false);
        assert_eq!(io.stops(), 1, "off closes the stream, not just the ear");
        assert!(!io.is_open());
        say(&io);
        assert_eq!(heard(&t.poll(100)), 0, "nothing demodulated while off");
        t.set_listening(true);
        assert_eq!(heard(&t.poll(200)), 0, "nor kept for later");
        assert_eq!(io.starts(), 2, "listen reopens the microphone");
        say(&io);
        assert!(t.poll(300).contains(&TransportEvent::Frame(frame.clone())));
    }

    #[test]
    fn off_closes_the_microphone_and_the_stream_is_gone() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        assert_eq!((io.starts(), io.stops()), (1, 0));
        assert!(io.is_open(), "the stream is open after poll starts it");

        t.set_listening(false);
        assert_eq!((io.starts(), io.stops()), (1, 1), "stop_input was called");
        assert!(!io.is_open(), "the stream is gone");

        // Polling again while off does not reopen it.
        t.poll(100);
        assert_eq!(io.starts(), 1, "still off, nothing to restart");
    }

    #[test]
    fn listen_starts_the_microphone_again() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        t.set_listening(false);
        assert_eq!((io.starts(), io.stops()), (1, 1));

        t.set_listening(true);
        assert!(!io.is_open(), "not reopened until the next poll");
        t.poll(100);
        assert_eq!((io.starts(), io.stops()), (2, 1), "start_input ran again");
        assert!(io.is_open());
    }

    #[test]
    fn rapid_toggles_end_in_the_right_state() {
        let io = Scripted::default();
        let mut t = transport(&io);

        // Off, on, off again before the microphone was ever opened: it
        // should never open at all.
        t.set_listening(false);
        t.set_listening(true);
        t.set_listening(false);
        t.poll(0);
        assert_eq!((io.starts(), io.stops()), (0, 0), "stayed off throughout");
        assert!(!io.is_open());

        // On, and several redundant flips to the same state, settle on
        // exactly one real start and one real stop.
        t.set_listening(true);
        t.set_listening(true);
        t.poll(10);
        assert_eq!((io.starts(), io.stops()), (1, 0));
        t.set_listening(false);
        t.set_listening(false);
        assert_eq!((io.starts(), io.stops()), (1, 1));
        assert!(!io.is_open());

        // And back on settles the microphone open again.
        t.set_listening(true);
        t.poll(20);
        assert_eq!((io.starts(), io.stops()), (2, 1));
        assert!(io.is_open());
    }

    /// A speaker the test plays by hand: `FakeAudio` holds what it was
    /// handed until `speak` says the time it takes has passed.
    fn fake_transport(io: &bombechat_modem::FakeAudio) -> SoundTransport {
        let mut t = SoundTransport::new(
            Box::new(io.clone()),
            ModemConfig::default(),
            QuietGate::new(40.0, 0, 90),
        );
        t.poll(0);
        t
    }

    #[test]
    fn a_withdrawn_envelope_is_cut_from_the_speaker_and_falls_silent() {
        let io = bombechat_modem::FakeAudio::new(48_000);
        let mut t = fake_transport(&io);
        for f in split(&[3; 300], 9) {
            t.send(f);
        }
        io.feed(&[0.0; 9_600]);
        assert_eq!(progress(t.poll(100)), vec![TransportEvent::Playing(9)]);
        let handed = io.played().len();
        assert!(
            io.pending(Some(9)) > 48_000,
            "seconds of it wait at the speaker"
        );

        // 50 ms of it plays, then it is stopped
        io.speak(2_400);
        t.withdraw(9);
        assert_eq!(io.pending(None), 0, "nothing of it is left to play");
        // it is over at once, not when it would have finished playing
        assert_eq!(progress(t.poll(150)), vec![TransportEvent::Sent(9)]);
        for now in [200, 5_000, 60_000, 200_000] {
            io.feed(&[0.0; 9_600]);
            assert!(progress(t.poll(now)).is_empty());
        }
        assert_eq!(io.played().len(), handed, "no more of it was handed over");
    }

    #[test]
    fn withdrawing_one_envelope_leaves_another_playing_on() {
        let io = bombechat_modem::FakeAudio::new(48_000);
        let mut t = fake_transport(&io);
        // a message to a contact goes out, then the test is queued behind it
        for f in split(&[1; 200], 2) {
            t.send(f);
        }
        io.feed(&[0.0; 9_600]);
        assert_eq!(progress(t.poll(100)), vec![TransportEvent::Playing(2)]);
        for f in split(&[3; 200], 9) {
            t.send(f);
        }
        assert_eq!(progress(t.poll(200)), vec![TransportEvent::Playing(9)]);
        let contact = io.pending(Some(2));
        assert!(contact > 0 && io.pending(Some(9)) > 0);

        io.speak(4_800);
        t.withdraw(9);
        assert_eq!(io.pending(Some(9)), 0, "the test is cut");
        assert_eq!(
            io.pending(Some(2)),
            contact - 4_800,
            "the contact's message is not"
        );
        assert_eq!(progress(t.poll(300)), vec![TransportEvent::Sent(9)]);

        // the contact's message is sent once it has played, and no sooner
        let ms = (contact as u64 * 1000).div_ceil(48_000);
        assert!(progress(t.poll(100 + ms - 1)).is_empty(), "still playing");
        assert_eq!(
            progress(t.poll(100 + ms + 1)),
            vec![TransportEvent::Sent(2)]
        );
    }

    #[test]
    fn a_contacts_message_queued_behind_the_test_moves_up_when_it_is_cut() {
        let io = bombechat_modem::FakeAudio::new(48_000);
        let mut t = fake_transport(&io);
        for f in split(&[3; 200], 9) {
            t.send(f);
        }
        io.feed(&[0.0; 9_600]);
        assert_eq!(progress(t.poll(100)), vec![TransportEvent::Playing(9)]);
        for f in split(&[1; 200], 2) {
            t.send(f);
        }
        assert_eq!(progress(t.poll(200)), vec![TransportEvent::Playing(2)]);
        let contact = io.pending(Some(2));
        t.withdraw(9);
        assert_eq!(
            io.pending(None),
            contact,
            "only the contact's message is left"
        );
        assert_eq!(progress(t.poll(300)), vec![TransportEvent::Sent(9)]);
        // it plays from now, not after the test's cut audio
        let ms = (contact as u64 * 1000).div_ceil(48_000);
        assert!(progress(t.poll(300 + ms - 200)).is_empty());
        assert_eq!(
            progress(t.poll(300 + ms + 1)),
            vec![TransportEvent::Sent(2)]
        );
    }

    #[test]
    fn a_frame_already_playing_is_allowed_to_finish_and_the_outbox_stays_truthful() {
        let io = Scripted::default();
        let mut t = SoundTransport::new(
            Box::new(io.clone()),
            ModemConfig::default(),
            QuietGate::new(40.0, 0, 90),
        );
        t.poll(0);
        let frame = split(b"hello", 7).remove(0);
        t.send(frame);
        io.quiet();
        assert_eq!(progress(t.poll(100)), vec![TransportEvent::Playing(7)]);

        // The Sound switch goes to Off mid-playback: the microphone
        // closes, but what is already on its way to the speaker is not
        // torn up, and finishing it is reported truthfully rather than
        // as a drop.
        t.set_listening(false);
        t.set_sending(false);
        assert!(!io.is_open(), "the microphone closed");
        assert_eq!(
            progress(t.poll(200_000)),
            vec![TransportEvent::Sent(7)],
            "finished playing truthfully, not dropped for being off"
        );
    }

    #[test]
    fn with_sending_off_the_queue_waits() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        t.set_sending(false);
        t.send(split(b"hello", 7).remove(0));
        io.quiet();
        assert!(progress(t.poll(100)).is_empty());
        assert!(io.played.lock().unwrap().is_empty(), "nothing played");
        t.set_sending(true);
        io.quiet();
        assert_eq!(progress(t.poll(200)), vec![TransportEvent::Playing(7)]);
    }

    #[test]
    fn send_under_moves_the_quiet_gate() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        t.set_send_under(30.0);
        t.send(split(b"hello", 7).remove(0));
        murmur(&io);
        assert!(progress(t.poll(100)).is_empty(), "34 dB is not under 30");
        t.set_send_under(40.0);
        murmur(&io);
        assert_eq!(progress(t.poll(200)), vec![TransportEvent::Playing(7)]);
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, x| m.max(x.abs()))
    }

    #[test]
    fn volume_is_a_gain_on_what_plays() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        let frame = split(b"hello", 7).remove(0);
        let full = peak(&modulate(&ModemConfig::default(), &frame));
        t.set_volume(0.2);
        t.send(frame.clone());
        io.quiet();
        t.poll(100);
        let played = io.played.lock().unwrap().clone();
        assert_eq!(played.len(), 1);
        assert!(
            (peak(&played[0]) - 0.2 * full).abs() < 1e-4,
            "a fifth as loud"
        );

        t.set_volume(7.0);
        t.send(split(b"again", 8).remove(0));
        io.quiet();
        t.poll(200_000);
        let loudest = peak(io.played.lock().unwrap().last().unwrap());
        assert!(loudest <= full + 1e-4, "never past full scale");
    }

    #[test]
    fn fewer_repeats_let_a_frame_go_and_its_envelope_is_sent() {
        let io = Scripted::default();
        let mut t = SoundTransport::new(
            Box::new(io.clone()),
            ModemConfig::default(),
            QuietGate::new(40.0, 3, 90),
        );
        t.poll(0);
        t.send(split(b"hello", 7).remove(0));
        io.quiet();
        assert_eq!(progress(t.poll(100)), vec![TransportEvent::Playing(7)]);
        t.set_repeats(0);
        assert!(
            t.queued.is_empty(),
            "played once, which is all it now plays"
        );
        assert_eq!(progress(t.poll(100_000)), vec![TransportEvent::Sent(7)]);
        assert_eq!(io.played.lock().unwrap().len(), 1);
    }

    #[test]
    fn more_repeats_play_a_queued_frame_more_often() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        t.set_repeats(2);
        t.send(split(b"hello", 7).remove(0));
        for now in [100, 90_200, 180_300] {
            io.quiet();
            t.poll(now);
        }
        assert_eq!(io.played.lock().unwrap().len(), 3);
    }

    #[test]
    fn the_band_moves_what_plays_and_what_is_heard() {
        let io = Scripted::default();
        let mut t = transport(&io);
        t.poll(0);
        assert_eq!(t.band(), Some(Band::NearSilent));
        t.set_band(Band::Audible);
        assert_eq!(t.band(), Some(Band::Audible));
        let audible = ModemConfig {
            band: Band::Audible,
            ..ModemConfig::default()
        };
        let frame = split(b"hello", 7).remove(0);
        t.send(frame.clone());
        io.quiet();
        t.poll(100);
        assert_eq!(
            io.played.lock().unwrap().clone(),
            vec![modulate(&audible, &frame)],
            "played on the audible band"
        );

        let say = |cfg: &ModemConfig| {
            let mut x = modulate(cfg, &frame);
            x.extend(vec![0.0; 9_600]);
            if let Some(sink) = io.sink.lock().unwrap().as_mut() {
                sink(&x);
            }
        };
        say(&audible);
        assert!(t.poll(200).contains(&TransportEvent::Frame(frame.clone())));
        say(&ModemConfig::default());
        assert!(
            !t.poll(300).contains(&TransportEvent::Frame(frame.clone())),
            "the near-silent band is no longer heard"
        );
    }

    /// A card at 32 kHz cannot carry the near-silent band.
    #[derive(Clone, Default)]
    struct Slow(Scripted);

    impl AudioIo for Slow {
        fn start_input(&mut self, sink: Sink) -> Result<(), AudioError> {
            self.0.start_input(sink)
        }
        fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
            self.0.play(samples)
        }
        fn sample_rate(&self) -> u32 {
            32_000
        }
    }

    #[test]
    fn a_card_too_slow_for_near_silent_stays_audible() {
        let audible = ModemConfig {
            sample_rate: 32_000,
            band: Band::Audible,
            ..ModemConfig::default()
        };
        let mut t = SoundTransport::new(
            Box::new(Slow::default()),
            audible,
            QuietGate::new(40.0, 0, 90),
        );
        t.set_band(Band::NearSilent);
        assert_eq!(t.band(), Some(Band::Audible));
    }
}
