//! Audio device I/O: the [`AudioIo`] trait the rest of the modem talks to,
//! and [`CpalAudio`], its implementation over the default system devices
//! via [cpal](https://docs.rs/cpal).
//!
//! Microphone and speaker samples are always `f32` and mono. A device
//! that captures or plays more than one channel is downmixed on input
//! (channels averaged) and duplicated on output (the same mono sample
//! written to every channel), so callers above this layer never see
//! channel counts.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, InputCallbackInfo, OutputCallbackInfo, StreamConfig};

/// What can go wrong talking to the audio hardware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// No input or output device is available on this system.
    NoDevice,
    /// The operating system or audio subsystem denied access (for
    /// example, microphone permission was not granted).
    PermissionDenied,
    /// The device cannot do what was asked (an unsupported sample rate,
    /// channel count, or sample format), or some other backend error that
    /// does not fit the other variants. The string is a human-readable
    /// detail for logs.
    Unsupported(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no audio device available"),
            AudioError::PermissionDenied => write!(f, "audio device access denied"),
            AudioError::Unsupported(detail) => write!(f, "unsupported audio device: {detail}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// A source of microphone samples and a sink for speaker samples, always
/// `f32`, always mono.
pub trait AudioIo: Send {
    /// Start capturing from the input device. `sink` is called from an
    /// audio callback thread with each chunk of mono samples as it
    /// arrives; it must not block.
    #[allow(clippy::type_complexity)]
    fn start_input(&mut self, sink: Box<dyn FnMut(&[f32]) + Send>) -> Result<(), AudioError>;

    /// Stop capturing from the input device and release it, so the
    /// hardware (and any OS recording indicator) goes quiet. Does nothing
    /// if input was never started, or was already stopped.
    /// [`start_input`](AudioIo::start_input) opens the device again. The
    /// default does nothing, for doubles with no device to release.
    fn stop_input(&mut self) {}

    /// Queue `samples` to be played on the output device. Returns once the
    /// samples are queued, without waiting for them to actually play.
    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError>;

    /// Queue `samples` as [`play`](AudioIo::play) does, marked as
    /// `owner`'s, so that [`cut`](AudioIo::cut) can take back whatever of
    /// them has not played yet. The default plays them unmarked, for
    /// devices that cannot take anything back.
    fn play_for(&mut self, owner: u16, samples: Vec<f32>) -> Result<(), AudioError> {
        let _ = owner;
        self.play(samples)
    }

    /// Drop every sample queued by [`play_for`](AudioIo::play_for) for
    /// `owner` that has not played yet, the one playing now included, so
    /// its sound stops within the output's own buffer (milliseconds).
    /// Everything else queued plays on as it would have. Returns how many
    /// samples were dropped. The default drops nothing: a device that
    /// cannot take audio back lets it play out.
    fn cut(&mut self, owner: u16) -> usize {
        let _ = owner;
        0
    }

    /// The sample rate, in Hz, that both the input and output streams run
    /// at.
    fn sample_rate(&self) -> u32;

    /// An error a stream reported asynchronously since the last call (a
    /// device unplugged, a backend failure), if any; each one is returned
    /// at most once. `start_input` and `play` only report synchronous
    /// failures, so a caller polls this to learn that a running stream
    /// has died. Devices with no asynchronous errors keep the default,
    /// which never reports one.
    fn take_error(&mut self) -> Option<AudioError> {
        None
    }
}

/// Maps a [`cpal::Error`] to an [`AudioError`], by its [`cpal::ErrorKind`].
fn map_cpal_error(err: cpal::Error) -> AudioError {
    match err.kind() {
        cpal::ErrorKind::PermissionDenied => AudioError::PermissionDenied,
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable => {
            AudioError::NoDevice
        }
        _ => AudioError::Unsupported(err.to_string()),
    }
}

/// Handles an error reported asynchronously by an input or output stream's
/// error callback, after the stream has already started: maps it the same
/// way a synchronous failure would be, logs one line, and stores it in
/// `slot` for [`CpalAudio::take_stream_error`] to pick up. A later error
/// overwrites an earlier one that was never taken.
///
/// Factored out of the stream-building closures so it can be unit-tested
/// directly, without real audio hardware.
fn record_stream_error(slot: &Mutex<Option<AudioError>>, err: cpal::Error) {
    let mapped = map_cpal_error(err);
    eprintln!("bombechat-modem: audio stream error: {mapped}");
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(mapped);
}

/// What the output stream plays from: chunks of mono samples in the order
/// they were queued, each marked with who queued it (if anyone), so one
/// owner's audio can be taken back without touching the rest.
#[derive(Default)]
struct Speaker {
    chunks: VecDeque<Chunk>,
}

struct Chunk {
    owner: Option<u16>,
    samples: Vec<f32>,
    /// How many of `samples` have played.
    at: usize,
}

impl Speaker {
    fn push(&mut self, owner: Option<u16>, samples: Vec<f32>) {
        if !samples.is_empty() {
            self.chunks.push_back(Chunk {
                owner,
                samples,
                at: 0,
            });
        }
    }

    /// The next sample to play, or silence when nothing is queued.
    fn next(&mut self) -> f32 {
        while let Some(chunk) = self.chunks.front_mut() {
            if let Some(&s) = chunk.samples.get(chunk.at) {
                chunk.at += 1;
                return s;
            }
            self.chunks.pop_front();
        }
        0.0
    }

    /// Drops what has not played of every chunk `owner` queued; returns
    /// how many samples that was.
    fn cut(&mut self, owner: u16) -> usize {
        let mut dropped = 0;
        self.chunks.retain(|c| {
            let keep = c.owner != Some(owner);
            if !keep {
                dropped += c.samples.len() - c.at;
            }
            keep
        });
        dropped
    }

    /// Samples queued and not yet played.
    #[cfg(test)]
    fn pending(&self) -> usize {
        self.chunks.iter().map(|c| c.samples.len() - c.at).sum()
    }
}

/// Default-device input and output over cpal.
///
/// Built once, at construction: the output stream is opened and started
/// immediately (playing silence until [`CpalAudio::play`] queues
/// something), so [`play`](AudioIo::play) is always ready to queue
/// without touching the device. The input stream is only opened once
/// [`start_input`](AudioIo::start_input) is called, since starting it may
/// be what triggers a microphone permission prompt.
pub struct CpalAudio {
    input_device: cpal::Device,
    sample_rate: u32,
    input_channels: u16,
    input_stream: Option<cpal::Stream>,
    // Kept alive for as long as `CpalAudio` is; dropping it stops output.
    _output_stream: cpal::Stream,
    output_queue: Arc<Mutex<Speaker>>,
    /// The most recent error reported by either stream's error callback
    /// (a device disconnect, an xrun, and so on) since it was last taken
    /// by [`CpalAudio::take_stream_error`]. `start_input`/`play` only
    /// report *synchronous* failures (device missing, config rejected);
    /// this is how a failure *after* a stream has started reaches anyone,
    /// since `AudioIo` has no asynchronous error channel of its own.
    stream_error: Arc<Mutex<Option<AudioError>>>,
}

impl CpalAudio {
    /// Opens the default input and output devices at the system's default
    /// sample rate (taken from the input device's default configuration)
    /// and starts the output stream.
    pub fn new() -> Result<Self, AudioError> {
        let host = cpal::default_host();

        let input_device = host.default_input_device().ok_or(AudioError::NoDevice)?;
        let output_device = host.default_output_device().ok_or(AudioError::NoDevice)?;

        let input_default = input_device
            .default_input_config()
            .map_err(map_cpal_error)?;
        let output_default = output_device
            .default_output_config()
            .map_err(map_cpal_error)?;

        // One sample rate for both streams, so a single `ModemConfig` can
        // describe both directions. Taken from the input device; if the
        // output device cannot run at it, opening the output stream below
        // reports `Unsupported`.
        let sample_rate = input_default.sample_rate();
        let input_channels = input_default.channels();
        let output_channels = output_default.channels();

        let output_queue: Arc<Mutex<Speaker>> = Arc::new(Mutex::new(Speaker::default()));
        let queue_for_callback = output_queue.clone();
        let out_channels = output_channels as usize;

        let stream_error: Arc<Mutex<Option<AudioError>>> = Arc::new(Mutex::new(None));
        let error_slot_for_output = stream_error.clone();

        let output_config = StreamConfig {
            channels: output_channels,
            sample_rate,
            buffer_size: BufferSize::Default,
        };
        let output_stream = output_device
            .build_output_stream::<f32, _, _>(
                output_config,
                move |data: &mut [f32], _: &OutputCallbackInfo| {
                    let mut queue = queue_for_callback
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    for frame in data.chunks_mut(out_channels.max(1)) {
                        let sample = queue.next();
                        for s in frame.iter_mut() {
                            *s = sample;
                        }
                    }
                },
                move |err: cpal::Error| record_stream_error(&error_slot_for_output, err),
                None,
            )
            .map_err(map_cpal_error)?;
        output_stream.play().map_err(map_cpal_error)?;

        Ok(CpalAudio {
            input_device,
            sample_rate,
            input_channels,
            input_stream: None,
            _output_stream: output_stream,
            output_queue,
            stream_error,
        })
    }

    /// The most recent stream error recorded by either the input or
    /// output audio callback since the last call, if any. Each error is
    /// returned at most once: calling this again immediately afterward
    /// returns `None` until another error is recorded. Task 9's transport
    /// is expected to poll this (for example alongside each
    /// [`crate::DemodEvent::Level`]) and turn a `Some` into a
    /// channel-unavailable event.
    pub fn take_stream_error(&self) -> Option<AudioError> {
        self.stream_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

impl CpalAudio {
    fn speaker(&self) -> std::sync::MutexGuard<'_, Speaker> {
        self.output_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl AudioIo for CpalAudio {
    fn start_input(&mut self, mut sink: Box<dyn FnMut(&[f32]) + Send>) -> Result<(), AudioError> {
        let in_channels = self.input_channels as usize;
        let input_config = StreamConfig {
            channels: self.input_channels,
            sample_rate: self.sample_rate,
            buffer_size: BufferSize::Default,
        };
        let error_slot_for_input = self.stream_error.clone();

        let stream = self
            .input_device
            .build_input_stream::<f32, _, _>(
                input_config,
                move |data: &[f32], _: &InputCallbackInfo| {
                    if in_channels <= 1 {
                        sink(data);
                    } else {
                        let mono: Vec<f32> = data
                            .chunks(in_channels)
                            .map(|frame| frame.iter().sum::<f32>() / in_channels as f32)
                            .collect();
                        sink(&mono);
                    }
                },
                move |err: cpal::Error| record_stream_error(&error_slot_for_input, err),
                None,
            )
            .map_err(map_cpal_error)?;
        stream.play().map_err(map_cpal_error)?;
        self.input_stream = Some(stream);
        Ok(())
    }

    /// Drops the input stream, which stops it (cpal stops a stream when
    /// it is dropped) and releases the microphone. A later
    /// [`start_input`](AudioIo::start_input) opens a fresh one.
    fn stop_input(&mut self) {
        self.input_stream = None;
    }

    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
        self.speaker().push(None, samples);
        Ok(())
    }

    fn play_for(&mut self, owner: u16, samples: Vec<f32>) -> Result<(), AudioError> {
        self.speaker().push(Some(owner), samples);
        Ok(())
    }

    /// Takes the samples out of the queue the output callback reads, so
    /// at most the device's current buffer of them is still heard.
    fn cut(&mut self, owner: u16) -> usize {
        self.speaker().cut(owner)
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn take_error(&mut self) -> Option<AudioError> {
        self.take_stream_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speaker_plays_its_chunks_in_order_then_silence() {
        let mut sp = Speaker::default();
        sp.push(Some(1), vec![0.1, 0.2]);
        sp.push(None, vec![]);
        sp.push(None, vec![0.3]);
        let heard: Vec<f32> = (0..5).map(|_| sp.next()).collect();
        assert_eq!(heard, vec![0.1, 0.2, 0.3, 0.0, 0.0]);
        assert_eq!(sp.pending(), 0);
    }

    #[test]
    fn a_cut_drops_only_its_owners_unplayed_samples_mid_chunk_included() {
        let mut sp = Speaker::default();
        sp.push(Some(7), vec![0.7; 4]);
        sp.push(Some(2), vec![0.2; 3]);
        sp.push(Some(7), vec![0.7; 5]);
        sp.push(None, vec![0.5; 2]);
        // one sample of the first chunk has played: it is cut part way
        assert_eq!(sp.next(), 0.7);
        assert_eq!(sp.cut(7), 3 + 5);
        assert_eq!(sp.pending(), 3 + 2);
        let heard: Vec<f32> = (0..6).map(|_| sp.next()).collect();
        assert_eq!(heard, vec![0.2, 0.2, 0.2, 0.5, 0.5, 0.0]);
        assert_eq!(sp.cut(7), 0, "nothing of it is left to cut");
    }

    #[test]
    fn stream_error_is_mapped_like_a_synchronous_one() {
        let slot: Mutex<Option<AudioError>> = Mutex::new(None);

        record_stream_error(&slot, cpal::ErrorKind::PermissionDenied.into());
        assert_eq!(*slot.lock().unwrap(), Some(AudioError::PermissionDenied));

        record_stream_error(&slot, cpal::ErrorKind::DeviceNotAvailable.into());
        assert_eq!(*slot.lock().unwrap(), Some(AudioError::NoDevice));

        record_stream_error(&slot, cpal::ErrorKind::HostUnavailable.into());
        assert_eq!(*slot.lock().unwrap(), Some(AudioError::NoDevice));

        record_stream_error(&slot, cpal::ErrorKind::Xrun.into());
        assert!(matches!(
            *slot.lock().unwrap(),
            Some(AudioError::Unsupported(_))
        ));
    }

    #[test]
    fn stream_error_slot_is_taken_once() {
        let slot: Arc<Mutex<Option<AudioError>>> = Arc::new(Mutex::new(None));

        record_stream_error(&slot, cpal::ErrorKind::PermissionDenied.into());
        let take = |slot: &Arc<Mutex<Option<AudioError>>>| slot.lock().unwrap().take();

        assert_eq!(take(&slot), Some(AudioError::PermissionDenied));
        // Already taken: nothing left until another error is recorded.
        assert_eq!(take(&slot), None);

        record_stream_error(&slot, cpal::ErrorKind::DeviceNotAvailable.into());
        assert_eq!(take(&slot), Some(AudioError::NoDevice));
        assert_eq!(take(&slot), None);
    }

    #[test]
    fn a_later_stream_error_overwrites_an_untaken_earlier_one() {
        let slot: Mutex<Option<AudioError>> = Mutex::new(None);

        record_stream_error(&slot, cpal::ErrorKind::PermissionDenied.into());
        record_stream_error(&slot, cpal::ErrorKind::DeviceNotAvailable.into());

        assert_eq!(slot.lock().unwrap().take(), Some(AudioError::NoDevice));
    }

    /// `stop_input` drops the real input stream, and `start_input` after
    /// it rebuilds a fresh one: the only way to check, without hardware
    /// mocks, that stopping actually releases the device rather than just
    /// muting the sink.
    ///
    /// Ignored by default, for the same reason as
    /// `plays_and_records_a_tone`; it needs a real microphone and (on
    /// macOS) a permission grant for the test binary. This is the
    /// evidence for Task 2's "the stream handle is gone" requirement that
    /// a built app and a glance at the recording indicator can't give on
    /// their own. Run it manually with:
    ///
    /// ```sh
    /// cargo test -p bombechat-modem --release audio::tests::stop_input_drops_the_stream_and_start_input_rebuilds_it -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn stop_input_drops_the_stream_and_start_input_rebuilds_it() {
        let mut audio = CpalAudio::new().expect("open default audio devices");
        assert!(audio.input_stream.is_none(), "not started yet");

        audio
            .start_input(Box::new(|_| {}))
            .expect("start microphone input");
        assert!(audio.input_stream.is_some(), "the stream is open");

        audio.stop_input();
        assert!(
            audio.input_stream.is_none(),
            "stop_input drops the stream, releasing the microphone"
        );

        audio
            .start_input(Box::new(|_| {}))
            .expect("start microphone input again");
        assert!(
            audio.input_stream.is_some(),
            "start_input after stop rebuilds a fresh stream"
        );
    }

    /// Opens the real default input and output devices, plays a 0.5 s 1
    /// kHz tone and records 1 s of whatever the microphone hears while it
    /// plays.
    ///
    /// Ignored by default: it needs real audio hardware (and, on macOS, a
    /// microphone permission grant for the test binary) and cannot run in
    /// CI. Run it manually with:
    ///
    /// ```sh
    /// cargo test -p bombechat-modem --release audio::tests::plays_and_records_a_tone -- --ignored --nocapture
    /// ```
    ///
    /// It only prints what it captured; it does not assert anything about
    /// the content, since room acoustics vary.
    #[test]
    #[ignore]
    fn plays_and_records_a_tone() {
        use std::f32::consts::TAU;
        use std::time::Duration;

        let mut audio = CpalAudio::new().expect("open default audio devices");
        let sample_rate = audio.sample_rate();

        let captured: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_sink = captured.clone();
        audio
            .start_input(Box::new(move |samples: &[f32]| {
                captured_for_sink
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend_from_slice(samples);
            }))
            .expect("start microphone input");

        let tone_len = (sample_rate as f32 * 0.5) as usize;
        let tone: Vec<f32> = (0..tone_len)
            .map(|i| 0.2 * (TAU * 1_000.0 * i as f32 / sample_rate as f32).sin())
            .collect();
        audio.play(tone).expect("queue the tone");

        std::thread::sleep(Duration::from_secs(1));

        let samples = captured.lock().unwrap().len();
        println!("captured {samples} samples at {sample_rate} Hz while playing the tone");
        assert!(
            samples > 0,
            "expected to capture something from the microphone"
        );
    }
}
