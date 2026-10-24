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

    /// Queue `samples` to be played on the output device. Returns once the
    /// samples are queued, without waiting for them to actually play.
    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError>;

    /// The sample rate, in Hz, that both the input and output streams run
    /// at.
    fn sample_rate(&self) -> u32;
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
    output_queue: Arc<Mutex<VecDeque<f32>>>,
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

        let output_queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let queue_for_callback = output_queue.clone();
        let out_channels = output_channels as usize;

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
                        let sample = queue.pop_front().unwrap_or(0.0);
                        for s in frame.iter_mut() {
                            *s = sample;
                        }
                    }
                },
                |_err: cpal::Error| {},
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
        })
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
                |_err: cpal::Error| {},
                None,
            )
            .map_err(map_cpal_error)?;
        stream.play().map_err(map_cpal_error)?;
        self.input_stream = Some(stream);
        Ok(())
    }

    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
        let mut queue = self
            .output_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.extend(samples);
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
