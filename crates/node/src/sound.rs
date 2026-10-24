//! The sound transport: frames out through the FSK modem and the speaker
//! once the room is quiet, frames in from the microphone through the
//! demodulator.

use std::collections::HashMap;
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
/// Every frame handed to [`send`](Transport::send) is modulated and queued
/// in the [`QuietGate`]; each poll feeds the gate the room level from the
/// demodulator and plays whatever it says is due.
pub struct SoundTransport {
    io: Box<dyn AudioIo>,
    cfg: ModemConfig,
    gate: QuietGate,
    demod: Demodulator,
    input: Input,
    heard: Arc<Mutex<Vec<f32>>>,
    /// Modulated samples for each frame still in the gate, by gate item.
    queued: HashMap<usize, Vec<f32>>,
    next_item: usize,
    /// Whether the last play failed, so a dead speaker is reported once,
    /// not once per frame.
    play_failed: bool,
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
            play_failed: false,
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
        let item = self.next_item;
        self.next_item = self.next_item.wrapping_add(1);
        self.queued.insert(item, modulate(&self.cfg, &frame));
        self.gate.enqueue(item);
    }

    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent> {
        let mut events = Vec::new();

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
            let Some(samples) = self.queued.get(&item) else {
                continue;
            };
            let samples = if self.gate.is_queued(item) {
                samples.clone()
            } else {
                self.queued.remove(&item).expect("present above")
            };
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

        events
    }
}
