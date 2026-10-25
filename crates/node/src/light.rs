//! The light transport: envelopes out as a paced sequence of code frames
//! for the page to draw, envelopes in from the camera through the reader.
//!
//! The node hands every transport the frames `bombechat_modem::split`
//! cuts an envelope into. This transport puts them back together and
//! fountain-codes the whole envelope ([`bombechat_light::encode`]); what
//! it collects from the camera it cuts up again the same way, so the node
//! reassembles and opens light envelopes exactly as it does sound ones.
//!
//! # Threads
//!
//! The camera and the reader live on a worker thread of their own, raised
//! to the highest scheduling class the system offers an ordinary thread
//! (user-interactive on macOS) so reading keeps up with the camera.
//! Starting the camera can wait up to 90 seconds on the permission prompt,
//! so the node's tick thread never touches the camera itself: it posts
//! "start" and "stop" to the worker and learns the outcome on a later
//! poll. The camera hands pictures to the worker through a queue that
//! keeps only the newest [`PICTURES_HELD`]: a reader that falls behind
//! skips old pictures rather than drifting further behind. What the
//! reader makes of each picture comes back through a channel, and is
//! collected into envelopes on the tick thread at the `now_ms` of the
//! poll that takes it, so collection runs on the node's clock.
//!
//! # Showing frames
//!
//! One envelope is shown at a time, its frames paced at the preset's
//! [`CodeSpec::frames_per_second`] by `now_ms`; others wait their turn,
//! up to [`MAX_QUEUED_ENVELOPES`]. Each envelope is shown for a sending
//! window of [`SEND_WINDOW`] times the frames a receiver needs at least,
//! but never less than [`HUMAN_FLOOR_MS`] of frames, so a person has time
//! to hold the screen up; [`Transport::delivered`] (everyone it was for
//! has it) cuts the window short. An envelope is coded in the preset
//! current when it goes on screen ([`Transport::set_preset`]).

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bombechat_core::MAX_ENVELOPE;
use bombechat_light::{
    encode, grid_bits, CameraError, CameraIo, CodeSpec, Collector, Encoder, LightEvent,
    ReadOutcome, Reader,
};
use bombechat_modem::{split, Event, Reassembler};
use bombechat_store::Channel;
use image::GrayImage;

use crate::sound::{MAX_QUEUED_ENVELOPES, QUEUE_FULL};
use crate::transport::{Transport, TransportEvent};

/// An envelope is shown for this many times the frames a receiver needs
/// at least, unless everyone it was for reports hearing it first, and for
/// no less than [`HUMAN_FLOOR_MS`].
pub const SEND_WINDOW: u32 = 3;

/// The shortest an envelope is shown, in ms, unless everyone it was for
/// reports hearing it first: time for a person to hold the screen up to
/// the other camera. A receipt (which nobody reports hearing) is shown
/// this long in full.
pub const HUMAN_FLOOR_MS: u32 = 4_000;

/// The notice for an envelope too long to show.
pub const TOO_LONG: &str = "message too long for light";

/// Frames to show an envelope of `symbols` source symbols for in `spec`:
/// [`SEND_WINDOW`] times the frames it needs, or [`HUMAN_FLOOR_MS`] of
/// frames, whichever is more.
fn window(spec: &CodeSpec, symbols: u32) -> u32 {
    let per_frame = spec.packets_per_frame().max(1) as u32;
    let needed = symbols.div_ceil(per_frame).max(1);
    let floor = (HUMAN_FLOOR_MS * spec.frames_per_second()).div_ceil(1000);
    (needed * SEND_WINDOW).max(floor)
}

/// Camera pictures waiting for the reader at most; when another arrives,
/// the oldest is let go.
pub const PICTURES_HELD: usize = 2;

/// How often the worker asks a running camera whether it has failed,
/// when nothing else wakes it.
const ERROR_CHECK: Duration = Duration::from_millis(100);

/// How long dropping the transport waits for the worker to close the
/// camera and end.
const WORKER_END_WAIT: Duration = Duration::from_secs(1);

/// An envelope part way through arriving is given up this long after its
/// last frame was read: the collector's own expiry.
const ARRIVAL_EXPIRY_MS: u64 = 600_000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the tick thread asks of the worker. A newer command replaces one
/// the worker has not taken yet.
enum Command {
    /// Open the camera; its pictures carry this start's number.
    Start(u64),
    /// Close the camera, and let go of pictures not yet read.
    Stop,
}

/// What the worker reports back, each under the number of the start it
/// belongs to, so the tick thread can let go of anything from a camera
/// it has since closed.
enum Report {
    Started(u64),
    Failed(u64, CameraError),
    Read(u64, ReadOutcome),
}

/// The worker's inbox.
#[derive(Default)]
struct Work {
    command: Option<Command>,
    /// The newest pictures, oldest first, each with its start's number.
    pictures: VecDeque<(u64, GrayImage)>,
    /// A command or picture has been taken and is not finished yet.
    busy: bool,
    /// The transport is gone: close the camera and end.
    closed: bool,
}

#[derive(Default)]
struct Shared {
    work: Mutex<Work>,
    wake: Condvar,
}

/// A handle on a [`LightTransport`]'s reader, which stays usable after
/// the transport has gone into a node.
#[derive(Clone)]
pub struct ReaderWatch(Arc<Shared>);

impl ReaderWatch {
    /// Whether the worker has nothing to do: no picture waiting or being
    /// read, and no start or stop in hand. Everything it did before is
    /// waiting for the transport's next poll.
    pub fn idle(&self) -> bool {
        let work = lock(&self.0.work);
        work.command.is_none() && work.pictures.is_empty() && !work.busy
    }
}

enum Job {
    Command(Command),
    Read(u64, GrayImage),
    /// Nothing came in for a while: just check the camera.
    Check,
    Close,
}

/// Raise the calling thread to the user-interactive class, so the reader
/// gets a performance core.
#[cfg(target_os = "macos")]
fn raise_priority() {
    extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    // SAFETY: sets a scheduling hint on the calling thread only; takes no
    // pointers and cannot fail in a way that matters (a refusal leaves
    // the thread at its default class).
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

/// No portable equivalent without more dependencies: the thread keeps
/// the default class.
#[cfg(not(target_os = "macos"))]
fn raise_priority() {}

/// The worker: owns the camera and the reader.
fn work(
    mut camera: Box<dyn CameraIo>,
    mut reader: Reader,
    shared: Arc<Shared>,
    out: Sender<Report>,
) {
    raise_priority();
    // The reader's own budget, before any picture raised it.
    let base = reader.budget();
    let mut running: Option<u64> = None;
    loop {
        let job = {
            let mut work = lock(&shared.work);
            loop {
                if work.closed {
                    break Job::Close;
                }
                if let Some(command) = work.command.take() {
                    work.busy = true;
                    break Job::Command(command);
                }
                if let Some((start, picture)) = work.pictures.pop_front() {
                    work.busy = true;
                    break Job::Read(start, picture);
                }
                let (guard, waited) = shared
                    .wake
                    .wait_timeout(work, ERROR_CHECK)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                work = guard;
                if waited.timed_out() {
                    break Job::Check;
                }
            }
        };
        match job {
            Job::Close => {
                if running.is_some() {
                    camera.stop();
                }
                return;
            }
            Job::Command(Command::Start(start)) => {
                // A fresh start: a fresh reader at its own budget, and no
                // picture from before.
                reader = Reader::with_budget(base);
                lock(&shared.work).pictures.clear();
                let into = shared.clone();
                // Never blocks for long: the worker holds this lock only to
                // move a picture or a command, never while it reads.
                let sink = Box::new(move |picture: GrayImage| {
                    let mut work = lock(&into.work);
                    work.pictures.push_back((start, picture));
                    while work.pictures.len() > PICTURES_HELD {
                        work.pictures.pop_front();
                    }
                    drop(work);
                    into.wake.notify_one();
                });
                let report = match camera.start(sink) {
                    Ok(()) => {
                        running = Some(start);
                        Report::Started(start)
                    }
                    Err(e) => {
                        running = None;
                        Report::Failed(start, e)
                    }
                };
                let _ = out.send(report);
            }
            Job::Command(Command::Stop) => {
                if running.take().is_some() {
                    camera.stop();
                }
                lock(&shared.work).pictures.clear();
            }
            Job::Read(start, picture) => {
                let outcome = reader.read(&picture);
                let _ = out.send(Report::Read(start, outcome));
            }
            Job::Check => {}
        }
        if let Some(start) = running {
            if let Some(e) = camera.take_error() {
                camera.stop();
                running = None;
                lock(&shared.work).pictures.clear();
                let _ = out.send(Report::Failed(start, e));
            }
        }
        lock(&shared.work).busy = false;
    }
}

/// Where the camera is, as far as the tick thread knows.
enum Camera {
    /// Closed, and to be opened on the next poll (unless reading is off,
    /// in which case it goes straight to [`Camera::Stopped`]).
    NotStarted,
    /// Asked to open; the worker has not said how it went.
    Starting,
    Running,
    /// Closed because reading is off. Goes back to
    /// [`Camera::NotStarted`] when reading turns on again.
    Stopped,
    /// Would not open, or failed while open. Turning reading off and on
    /// again tries afresh.
    Failed,
}

/// One envelope on screen, or paused part way through.
struct Outgoing {
    tag: u16,
    /// The preset it is shown in, fixed once it goes on screen.
    spec: CodeSpec,
    encoder: Encoder,
    /// Frames to show in all.
    window: u32,
    /// Frames shown so far.
    shown: u32,
    /// Whether [`TransportEvent::Playing`] has been reported since it last
    /// went on screen.
    announced: bool,
}

/// The reason shown after "unavailable: " for a camera failure.
fn reason(e: &CameraError) -> String {
    match e {
        CameraError::PermissionDenied => "camera permission denied".to_string(),
        CameraError::NoDevice => "no camera".to_string(),
        CameraError::NotAnswered => "camera permission not answered".to_string(),
        CameraError::Interrupted => "camera in use by another app".to_string(),
        CameraError::Unsupported(detail) => format!("camera unsupported ({detail})"),
    }
}

/// [`Transport`] over a [`CameraIo`] camera and a screen the page draws.
///
/// The camera is opened by the first [`poll`](Transport::poll) (on the
/// worker thread; see the module notes) and closed, so its light goes
/// off, when [`set_listening`](Transport::set_listening) turns off. A
/// camera that will not open or that fails while open is reported once
/// as [`TransportEvent::Unavailable`]; turning listening off and on again
/// tries it afresh.
///
/// With [`set_sending`](Transport::set_sending) off nothing is shown and
/// envelopes wait. Turned off part way through an envelope, the screen
/// clears at once ([`TransportEvent::DisplayStopped`]) and the envelope
/// is reported [`TransportEvent::Waiting`]; turned on again, it carries
/// on where it stopped.
pub struct LightTransport {
    worker: Option<JoinHandle<()>>,
    /// Disconnects when the worker has ended.
    ended: Receiver<()>,
    spec: CodeSpec,
    shared: Arc<Shared>,
    reports: Receiver<Report>,
    camera: Camera,
    /// The number of the latest camera start.
    start: u64,
    collector: Collector,
    /// Tags part way through arriving, with when a frame of each was last
    /// read.
    arriving: HashMap<u16, u64>,
    /// The node's frames, put back together into envelopes.
    outgoing: Reassembler,
    /// Envelopes waiting their turn, by tag; each is coded in the preset
    /// current when it goes on screen.
    queue: VecDeque<(u16, Vec<u8>)>,
    /// The envelope on screen, or paused part way through.
    showing: Option<Outgoing>,
    /// When the current run of frames started, the index of the next
    /// frame slot after the last one shown, counted from there, and the
    /// frames a second of the run.
    pace: Option<(u64, u64, u64)>,
    /// The tag of the envelope whose frame is on screen now, if any.
    lit: Option<u16>,
    pending: Vec<TransportEvent>,
    listening: bool,
    sending: bool,
    now_ms: u64,
}

impl LightTransport {
    /// A light transport reading through `camera` and showing `spec`
    /// codes. Nothing is opened until the first poll.
    ///
    /// # Panics
    ///
    /// Panics if `spec` is not a preset, or if the worker thread cannot
    /// be started.
    pub fn new(camera: Box<dyn CameraIo>, spec: CodeSpec) -> LightTransport {
        LightTransport::with_reader(camera, spec, Reader::new())
    }

    /// As [`LightTransport::new`], reading with `reader`: for a test that
    /// wants a reader with more time than the default budget.
    pub fn with_reader(
        camera: Box<dyn CameraIo>,
        spec: CodeSpec,
        reader: Reader,
    ) -> LightTransport {
        assert!(spec.is_valid(), "{spec:?} is not a light code preset");
        let shared = Arc::new(Shared::default());
        let (out, reports) = mpsc::channel();
        let theirs = shared.clone();
        // Dropped when the worker ends, however it ends.
        let (alive, ended) = mpsc::channel::<()>();
        let worker = thread::Builder::new()
            .name("light reader".to_string())
            .spawn(move || {
                let _alive = alive;
                work(camera, reader, theirs, out)
            })
            .expect("start the light reader thread");
        LightTransport {
            worker: Some(worker),
            ended,
            spec,
            shared,
            reports,
            camera: Camera::NotStarted,
            start: 0,
            collector: Collector::new(),
            arriving: HashMap::new(),
            outgoing: Reassembler::new(),
            queue: VecDeque::new(),
            showing: None,
            pace: None,
            lit: None,
            pending: Vec::new(),
            listening: true,
            sending: true,
            now_ms: 0,
        }
    }

    /// A handle on the reader, for waiting until it has caught up.
    pub fn watch(&self) -> ReaderWatch {
        ReaderWatch(self.shared.clone())
    }

    fn command(&self, command: Command) {
        lock(&self.shared.work).command = Some(command);
        self.shared.wake.notify_one();
    }

    /// Queue one whole envelope to show.
    fn enqueue(&mut self, tag: u16, envelope: Vec<u8>) {
        let refused = if envelope.len() > MAX_ENVELOPE {
            Some(TOO_LONG)
        } else if self.queue.len() + usize::from(self.showing.is_some()) >= MAX_QUEUED_ENVELOPES {
            Some(QUEUE_FULL)
        } else {
            None
        };
        if let Some(why) = refused {
            self.pending.push(TransportEvent::Notice(why.to_string()));
            self.pending.push(TransportEvent::Dropped(tag));
            return;
        }
        self.queue.push_back((tag, envelope));
    }

    /// Put the next waiting envelope on screen, coded in the current
    /// preset.
    fn next_up(&mut self) -> Option<Outgoing> {
        let (tag, envelope) = self.queue.pop_front()?;
        let spec = self.spec;
        let encoder = encode(&spec, &envelope, tag);
        Some(Outgoing {
            tag,
            spec,
            window: window(&spec, encoder.source_symbols()),
            encoder,
            shown: 0,
            announced: false,
        })
    }

    /// Take what the worker reported. Whether a code was read, or seen
    /// and not read, is said once for the whole poll.
    fn take_reports(&mut self, now_ms: u64, events: &mut Vec<TransportEvent>) {
        let (mut read_any, mut bad_any) = (false, false);
        while let Ok(report) = self.reports.try_recv() {
            match report {
                Report::Started(start) if start == self.start => {
                    if matches!(self.camera, Camera::Starting) {
                        self.camera = Camera::Running;
                    }
                }
                Report::Failed(start, e) if start == self.start => {
                    if matches!(self.camera, Camera::Starting | Camera::Running) {
                        self.camera = Camera::Failed;
                        events.push(TransportEvent::Unavailable(reason(&e)));
                    }
                }
                Report::Read(start, outcome) if start == self.start && self.listening => {
                    let read = self.collector.push_read(&outcome, now_ms);
                    bad_any |= read.contains(&LightEvent::FrameBad);
                    read_any |= read.iter().any(|e| matches!(e, LightEvent::FrameOk { .. }));
                    self.collected(read, now_ms, events);
                }
                _ => {}
            }
        }
        if read_any {
            events.push(TransportEvent::CodeRead);
        } else if bad_any {
            events.push(TransportEvent::CodeUnreadable);
        }
    }

    /// Report what the collector made of a picture.
    fn collected(&mut self, read: Vec<LightEvent>, now_ms: u64, events: &mut Vec<TransportEvent>) {
        let complete: Vec<u16> = read
            .iter()
            .filter_map(|e| match e {
                LightEvent::Complete { tag, .. } => Some(*tag),
                _ => None,
            })
            .collect();
        for event in read {
            match event {
                LightEvent::FrameOk { tag, .. } if !complete.contains(&tag) => {
                    if self.arriving.insert(tag, now_ms).is_none() {
                        events.push(TransportEvent::Arriving(tag));
                    }
                }
                LightEvent::Complete { tag, bytes } => {
                    self.arriving.remove(&tag);
                    events.extend(split(&bytes, tag).into_iter().map(TransportEvent::Frame));
                }
                _ => {}
            }
        }
    }

    /// Show the next frame if one is due.
    fn show(&mut self, now_ms: u64, events: &mut Vec<TransportEvent>) {
        if self.showing.is_none() {
            self.showing = self.next_up();
        }
        if self.showing.is_none() && self.lit.is_none() {
            self.pace = None;
            return;
        }
        // A run keeps its rate; an envelope in another preset starts a new
        // run at its own.
        if let Some(out) = &self.showing {
            let fps = u64::from(out.spec.frames_per_second());
            if self.pace.is_some_and(|(_, _, f)| f != fps) {
                self.pace = None;
            }
            self.pace.get_or_insert((now_ms, 0, fps));
        }
        let Some((from, next, fps)) = self.pace else {
            return;
        };
        let slot = now_ms.saturating_sub(from) * fps / 1000;
        if slot < next {
            return;
        }
        self.pace = Some((from, slot + 1, fps));
        let Some(out) = self.showing.as_mut() else {
            if let Some(tag) = self.lit.take() {
                self.pace = None;
                events.push(TransportEvent::DisplayStopped { tag });
            }
            return;
        };
        if !out.announced {
            out.announced = true;
            events.push(TransportEvent::Playing(out.tag));
        }
        let frame = out.encoder.next_frame();
        events.push(TransportEvent::Display {
            tag: out.tag,
            index: out.shown,
            spec: out.spec.preset().expect("only presets are kept"),
            bits: grid_bits(&out.spec, &frame),
        });
        self.lit = Some(out.tag);
        out.shown += 1;
        if out.shown >= out.window {
            events.push(TransportEvent::Sent(out.tag));
            self.showing = None;
        }
    }
}

impl Drop for LightTransport {
    /// The worker closes the camera and ends. This waits for it up to
    /// [`WORKER_END_WAIT`], so a transport built next does not race this
    /// one for the camera; a worker still waiting on the permission
    /// prompt is left to end by itself once it is answered.
    fn drop(&mut self) {
        lock(&self.shared.work).closed = true;
        self.shared.wake.notify_one();
        if let Err(RecvTimeoutError::Disconnected) = self.ended.recv_timeout(WORKER_END_WAIT) {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl Transport for LightTransport {
    fn name(&self) -> Channel {
        Channel::Light
    }

    /// The symbol bytes a second the preset shows: what a camera that
    /// reads every frame takes in.
    fn rate(&self) -> Option<f32> {
        let per_frame = self.spec.packets_per_frame() * self.spec.symbol_size as usize;
        Some((per_frame as u32 * self.spec.frames_per_second()) as f32)
    }

    /// Show the envelopes that have not gone on screen yet in `spec`, from
    /// the next one on; the one on screen finishes as it started. Not a
    /// preset: ignored.
    fn set_preset(&mut self, spec: CodeSpec) {
        if spec.is_valid() {
            self.spec = spec;
        }
    }

    /// Off, the camera is closed (the worker stops it), not just ignored,
    /// so its light goes off; what it read and has not been collected yet
    /// is let go. On again, the next poll opens it afresh.
    fn set_listening(&mut self, on: bool) {
        self.listening = on;
        match (on, &self.camera) {
            (false, Camera::Starting | Camera::Running) => {
                self.command(Command::Stop);
                self.camera = Camera::Stopped;
            }
            (false, Camera::NotStarted | Camera::Failed) => self.camera = Camera::Stopped,
            (true, Camera::Stopped) => self.camera = Camera::NotStarted,
            _ => {}
        }
    }

    /// Off, the screen clears at once and the envelope on it is reported
    /// waiting; on, it carries on from the frame it stopped at.
    fn set_sending(&mut self, on: bool) {
        if self.sending == on {
            return;
        }
        self.sending = on;
        if on {
            return;
        }
        if let Some(tag) = self.lit.take() {
            self.pending.push(TransportEvent::DisplayStopped { tag });
        }
        self.pace = None;
        if let Some(out) = self.showing.as_mut() {
            if out.announced {
                out.announced = false;
                self.pending.push(TransportEvent::Waiting(out.tag));
            }
        }
    }

    /// Stop showing the envelope under `tag`, or drop it from the queue,
    /// and report it sent: everyone it was for has it. If nothing else is
    /// waiting, the screen clears at once.
    fn delivered(&mut self, tag: u16) {
        if self.showing.as_ref().is_some_and(|o| o.tag == tag) {
            self.showing = None;
            if self.queue.is_empty() {
                // Nothing follows: clear the screen now rather than at the
                // end of the frame on it.
                if let Some(lit) = self.lit.take() {
                    self.pace = None;
                    self.pending
                        .push(TransportEvent::DisplayStopped { tag: lit });
                }
            }
        } else if let Some(i) = self.queue.iter().position(|(t, _)| *t == tag) {
            self.queue.remove(i);
        } else {
            return;
        }
        self.pending.push(TransportEvent::Sent(tag));
    }

    /// Forget that `tag` was collected, so its frames still being shown
    /// are collected afresh: a damaged collection gets another try.
    fn rejected(&mut self, tag: u16) -> bool {
        self.collector.reject(tag);
        true
    }

    fn send(&mut self, frame: Vec<u8>) {
        for event in self.outgoing.push(&frame, self.now_ms) {
            if let Event::Complete { tag, bytes } = event {
                self.enqueue(tag, bytes);
            }
        }
    }

    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent> {
        self.now_ms = now_ms;
        let mut events = std::mem::take(&mut self.pending);

        if matches!(self.camera, Camera::NotStarted) {
            self.start += 1;
            self.command(Command::Start(self.start));
            self.camera = Camera::Starting;
        }
        self.take_reports(now_ms, &mut events);

        self.collector.expire(now_ms);
        let mut gone: Vec<u16> = self
            .arriving
            .iter()
            .filter(|(_, at)| now_ms.saturating_sub(**at) > ARRIVAL_EXPIRY_MS)
            .map(|(tag, _)| *tag)
            .collect();
        gone.sort_unstable();
        for tag in gone {
            self.arriving.remove(&tag);
            events.push(TransportEvent::Abandoned(tag));
        }
        self.outgoing.expire(now_ms);

        if self.sending {
            self.show(now_ms, &mut events);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    use bombechat_light::FakeCamera;

    fn transport(cam: &FakeCamera) -> LightTransport {
        LightTransport::new(Box::new(cam.clone()), CodeSpec::normal())
    }

    fn settle(t: &LightTransport) {
        let until = Instant::now() + Duration::from_secs(10);
        while !t.watch().idle() {
            assert!(Instant::now() < until, "the worker never caught up");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Hand `t` an envelope of `len` bytes under `tag`, as the node does.
    fn send(t: &mut LightTransport, tag: u16, len: usize) {
        for frame in split(&vec![0x5a; len], tag) {
            t.send(frame);
        }
    }

    /// What is shown, as (tag, index), and the rest, over polls every
    /// 20 ms from `from` to `to`.
    fn run(t: &mut LightTransport, from: u64, to: u64) -> (Vec<(u16, u32)>, Vec<TransportEvent>) {
        let (mut shown, mut rest) = (Vec::new(), Vec::new());
        for now in (from..to).step_by(20) {
            for e in t.poll(now) {
                match e {
                    TransportEvent::Display { tag, index, .. } => shown.push((tag, index)),
                    other => rest.push(other),
                }
            }
        }
        (shown, rest)
    }

    #[test]
    fn frames_are_paced_by_the_presets_rate_and_never_shown_under_the_human_floor() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        // 1000 bytes is 5 symbols of 229 at one symbol a frame, so 3x the
        // need is 15 frames; the floor, 4 s at 10 a second, is 40.
        send(&mut t, 7, 1000);
        let (shown, rest) = run(&mut t, 0, 1_000);
        assert_eq!(
            shown,
            (0..10).map(|i| (7, i)).collect::<Vec<_>>(),
            "10 a second"
        );
        assert_eq!(rest, vec![TransportEvent::Playing(7)]);

        let (shown, rest) = run(&mut t, 1_000, 3_980);
        assert_eq!(shown, (10..40).map(|i| (7, i)).collect::<Vec<_>>());
        assert_eq!(rest, vec![TransportEvent::Sent(7)], "sent with frame 40");
        let (shown, rest) = run(&mut t, 3_980, 5_000);
        assert!(shown.is_empty());
        assert_eq!(
            rest,
            vec![TransportEvent::DisplayStopped { tag: 7 }],
            "cleared when the last frame's time is up"
        );
    }

    #[test]
    fn the_floor_is_four_seconds_at_every_preset_and_a_big_envelope_gets_three_times_its_need() {
        for spec in CodeSpec::presets() {
            let fps = spec.frames_per_second();
            assert_eq!(window(&spec, 1), 4 * fps, "{spec:?}");
            let symbols = 4 * fps; // needs 4 s of frames: 3x is 12 s
            assert_eq!(window(&spec, symbols), 3 * symbols, "{spec:?}");
        }
    }

    #[test]
    fn a_preset_change_applies_from_the_next_envelope() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        send(&mut t, 7, 100);
        send(&mut t, 8, 100);
        let specs = |events: Vec<TransportEvent>| -> Vec<(u16, u8)> {
            events
                .into_iter()
                .filter_map(|e| match e {
                    TransportEvent::Display { tag, spec, .. } => Some((tag, spec)),
                    _ => None,
                })
                .collect()
        };
        let first = specs(t.poll(0));
        t.set_preset(CodeSpec::fast());
        t.set_preset(CodeSpec {
            cells: 40,
            ..CodeSpec::fast()
        }); // not a preset: ignored
        let mut seen = first;
        for now in (20..9_000).step_by(20) {
            seen.extend(specs(t.poll(now)));
        }
        assert!(
            seen.iter().filter(|s| s.0 == 7).all(|s| s.1 == 1),
            "7 stays normal"
        );
        let fast: Vec<_> = seen.iter().filter(|s| s.0 == 8).collect();
        assert_eq!(fast.len(), 60, "4 s at 15 a second");
        assert!(fast.iter().all(|s| s.1 == 2), "8 goes fast");
    }

    #[test]
    fn an_envelope_too_long_for_light_says_so() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        send(&mut t, 9, MAX_ENVELOPE + 1);
        assert_eq!(
            t.poll(0),
            vec![
                TransportEvent::Notice(TOO_LONG.to_string()),
                TransportEvent::Dropped(9)
            ]
        );
    }

    #[test]
    fn a_late_tick_skips_frame_slots_rather_than_catching_up() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        send(&mut t, 7, 1000);
        t.poll(0);
        let shown = |events: Vec<TransportEvent>| {
            events
                .iter()
                .filter(|e| matches!(e, TransportEvent::Display { .. }))
                .count()
        };
        assert_eq!(shown(t.poll(550)), 1, "one frame, not five");
        assert_eq!(shown(t.poll(560)), 0);
        assert_eq!(shown(t.poll(600)), 1);
    }

    #[test]
    fn a_delivered_envelope_stops_at_once_and_the_next_one_follows() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        send(&mut t, 7, 1000);
        send(&mut t, 8, 100);
        let (shown, _) = run(&mut t, 0, 300);
        assert_eq!(shown, vec![(7, 0), (7, 1), (7, 2)]);
        t.delivered(7);
        let (shown, rest) = run(&mut t, 300, 5_000);
        assert_eq!(
            shown,
            (0..40).map(|i| (8, i)).collect::<Vec<_>>(),
            "one symbol, but the 4 s floor"
        );
        assert_eq!(
            rest,
            vec![
                TransportEvent::Sent(7),
                TransportEvent::Playing(8),
                TransportEvent::Sent(8),
                TransportEvent::DisplayStopped { tag: 8 }
            ]
        );
        t.delivered(8);
        assert!(
            t.poll(6_000).is_empty(),
            "already sent: nothing more to say"
        );
    }

    #[test]
    fn with_sending_off_nothing_is_shown_and_envelopes_wait() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        t.set_sending(false);
        send(&mut t, 7, 100);
        let (shown, rest) = run(&mut t, 0, 2_000);
        assert!(shown.is_empty());
        assert!(rest.is_empty(), "not playing, not dropped: waiting");
        t.set_sending(true);
        let (shown, rest) = run(&mut t, 2_000, 2_100);
        assert_eq!(shown, vec![(7, 0)]);
        assert_eq!(rest, vec![TransportEvent::Playing(7)]);
    }

    #[test]
    fn a_full_queue_refuses_the_next_envelope() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        for tag in 0..MAX_QUEUED_ENVELOPES as u16 {
            send(&mut t, tag, 100);
        }
        send(&mut t, 999, 100);
        let events = t.poll(0);
        assert!(events.starts_with(&[
            TransportEvent::Notice(QUEUE_FULL.to_string()),
            TransportEvent::Dropped(999)
        ]));
    }

    #[test]
    fn a_camera_that_fails_while_open_is_unavailable_and_retried_after_off_and_on() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        t.poll(0);
        settle(&t);
        assert!(cam.is_running());
        cam.fail(CameraError::NoDevice);
        // The worker checks the camera at least every ERROR_CHECK.
        let until = Instant::now() + Duration::from_secs(10);
        while cam.is_running() {
            assert!(Instant::now() < until, "the failure was never noticed");
            thread::sleep(Duration::from_millis(5));
        }
        settle(&t);
        assert_eq!(
            t.poll(20),
            vec![TransportEvent::Unavailable("no camera".to_string())]
        );
        t.poll(40);
        settle(&t);
        assert_eq!(cam.starts(), 1, "failed stays failed");

        t.set_listening(false);
        t.set_listening(true);
        t.poll(60);
        settle(&t);
        assert_eq!(cam.starts(), 2, "off and on tries afresh");
        assert!(cam.is_running());
    }

    /// A picture of the one frame envelope `b"hi"` under tag 3, on a
    /// mid-grey surround.
    fn hi() -> GrayImage {
        let spec = CodeSpec::normal();
        let frame = encode(&spec, b"hi", 3).next_frame();
        let code = bombechat_light::render(&spec, &frame, 480);
        let mut picture = GrayImage::from_pixel(800, 600, image::Luma([128]));
        image::imageops::overlay(&mut picture, &code, 160, 60);
        picture
    }

    #[test]
    fn what_the_camera_reads_comes_out_as_the_nodes_frames() {
        let cam = FakeCamera::new(vec![hi()]);
        let mut t = transport(&cam);
        t.poll(0);
        settle(&t);
        assert!(cam.show_next());
        settle(&t);
        let mut frames: Vec<TransportEvent> = split(b"hi", 3)
            .into_iter()
            .map(TransportEvent::Frame)
            .collect();
        frames.push(TransportEvent::CodeRead);
        assert_eq!(t.poll(20), frames);
    }

    #[test]
    fn a_rejected_envelope_is_collected_again_from_the_next_picture() {
        let cam = FakeCamera::new(vec![hi(), hi()]);
        let mut t = transport(&cam);
        t.poll(0);
        settle(&t);
        assert!(cam.show_next());
        settle(&t);
        assert!(t.poll(20).contains(&TransportEvent::CodeRead));
        assert!(t.rejected(3), "light collects afresh");
        assert!(cam.show_next());
        settle(&t);
        let again = t.poll(40);
        assert!(
            again.iter().any(|e| matches!(e, TransportEvent::Frame(_))),
            "handed over again: {again:?}"
        );
    }

    #[test]
    fn a_code_that_cannot_be_read_is_said_once_a_poll() {
        // A finger over the middle of the code: its finders in view, a
        // third of its data hidden.
        let mut picture = hi();
        for (x, y, p) in picture.enumerate_pixels_mut() {
            if (300..500).contains(&x) && (200..400).contains(&y) {
                *p = image::Luma([90]);
            }
        }
        let cam = FakeCamera::new(vec![picture.clone(), picture]);
        let mut t = transport(&cam);
        t.poll(0);
        settle(&t);
        assert!(cam.show_next());
        settle(&t);
        assert!(cam.show_next());
        settle(&t);
        let events = t.poll(20);
        assert_eq!(
            events
                .iter()
                .filter(|e| **e == TransportEvent::CodeUnreadable)
                .count(),
            1,
            "{events:?}"
        );
    }

    #[test]
    fn off_closes_the_camera_and_what_it_read_is_let_go() {
        let cam = FakeCamera::new(vec![hi(), hi()]);
        let mut t = transport(&cam);
        t.poll(0);
        settle(&t);
        assert!(cam.show_next());
        settle(&t);
        // Read, but not yet collected; Off lets it go.
        t.set_listening(false);
        assert!(t.poll(20).is_empty());
        settle(&t);
        assert!(!cam.is_running());
        assert_eq!(cam.stops(), 1);
        assert!(!cam.show_next(), "nothing more reaches the reader");
        t.poll(40);
        settle(&t);
        assert_eq!(cam.starts(), 1, "stays closed while off");

        // On again: a fresh start, and nothing from before comes back.
        t.set_listening(true);
        t.poll(60);
        settle(&t);
        assert_eq!(cam.starts(), 2);
        assert!(t.poll(80).is_empty());
    }

    #[test]
    fn dropping_it_has_closed_the_camera_by_the_time_it_returns() {
        let cam = FakeCamera::new(Vec::new());
        let mut t = transport(&cam);
        t.poll(0);
        settle(&t);
        assert!(cam.is_running());
        drop(t);
        assert!(!cam.is_running(), "closed before drop returned");
        assert_eq!(cam.stops(), 1);
        // So a transport built next has the camera to itself.
        let mut again = transport(&cam);
        again.poll(0);
        settle(&again);
        assert!(cam.is_running());
        assert_eq!((cam.starts(), cam.stops()), (2, 1));
    }

    #[test]
    fn its_rate_is_the_symbol_bytes_a_second_it_shows() {
        let cam = FakeCamera::new(Vec::new());
        let t = transport(&cam);
        assert_eq!(t.rate(), Some(2290.0));
    }
}
