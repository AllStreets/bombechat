//! Test doubles for [`crate::AudioIo`], gated behind the `fake-audio`
//! feature so they never ship in a build that does not ask for them.
//!
//! [`FakeAudio`] is a controllable single device. [`LoopbackAudio::pair`]
//! cross-wires two of them so that playing on one delivers (optionally
//! with noise mixed in) straight to the other's input sink, for tests
//! that exercise both ends of the acoustic link without real hardware.

use std::sync::{Arc, Mutex};

use crate::audio::{AudioError, AudioIo};

type Sink = Box<dyn FnMut(&[f32]) + Send>;
type NoiseFn = Box<dyn FnMut() -> f32 + Send>;

enum Mode {
    /// `start_input` always fails with `PermissionDenied`, as if the user
    /// declined a microphone permission prompt.
    Denied,
    Normal,
}

/// A controllable [`AudioIo`] test double.
///
/// By default it just records whatever is played (see
/// [`FakeAudio::played`]) and can be fed simulated microphone samples
/// directly (see [`FakeAudio::feed`]). Use [`LoopbackAudio::pair`] to get
/// two instances that feed each other automatically.
pub struct FakeAudio {
    mode: Mode,
    sample_rate: u32,
    played: Arc<Mutex<Vec<f32>>>,
    sink: Arc<Mutex<Option<Sink>>>,
    /// Where this device's `play()` output is delivered, if connected to
    /// a partner; see [`LoopbackAudio::pair`].
    forward_to: Option<Arc<Mutex<Option<Sink>>>>,
    /// Ambient noise mixed into samples forwarded to the partner.
    noise: Option<Arc<Mutex<NoiseFn>>>,
}

impl FakeAudio {
    /// A device that denies microphone access: [`AudioIo::start_input`]
    /// always returns [`AudioError::PermissionDenied`].
    pub fn denied() -> Self {
        FakeAudio {
            mode: Mode::Denied,
            sample_rate: 48_000,
            played: Arc::new(Mutex::new(Vec::new())),
            sink: Arc::new(Mutex::new(None)),
            forward_to: None,
            noise: None,
        }
    }

    /// A working fake device running at `sample_rate`, with nothing
    /// connected to it yet.
    pub fn new(sample_rate: u32) -> Self {
        FakeAudio {
            mode: Mode::Normal,
            sample_rate,
            played: Arc::new(Mutex::new(Vec::new())),
            sink: Arc::new(Mutex::new(None)),
            forward_to: None,
            noise: None,
        }
    }

    /// Mix ambient noise into every sample this device forwards to a
    /// [`LoopbackAudio::pair`] partner (it has no effect without one). One
    /// `noise()` call per forwarded sample.
    pub fn with_noise(mut self, noise: impl FnMut() -> f32 + Send + 'static) -> Self {
        self.noise = Some(Arc::new(Mutex::new(Box::new(noise))));
        self
    }

    /// Deliver `samples` to this device's registered input sink directly,
    /// as if a microphone had just heard them. Does nothing if
    /// [`AudioIo::start_input`] has not been called (or was denied).
    pub fn feed(&self, samples: &[f32]) {
        if let Some(sink) = self.sink.lock().unwrap().as_mut() {
            sink(samples);
        }
    }

    /// All samples handed to [`AudioIo::play`] so far, in order.
    pub fn played(&self) -> Vec<f32> {
        self.played.lock().unwrap().clone()
    }
}

impl AudioIo for FakeAudio {
    fn start_input(&mut self, sink: Sink) -> Result<(), AudioError> {
        match self.mode {
            Mode::Denied => Err(AudioError::PermissionDenied),
            Mode::Normal => {
                *self.sink.lock().unwrap() = Some(sink);
                Ok(())
            }
        }
    }

    fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError> {
        self.played.lock().unwrap().extend(&samples);

        if let Some(target) = &self.forward_to {
            let delivered: Vec<f32> = match &self.noise {
                Some(noise) => {
                    let mut noise = noise.lock().unwrap();
                    samples.iter().map(|&s| s + noise()).collect()
                }
                None => samples,
            };
            if let Some(sink) = target.lock().unwrap().as_mut() {
                sink(&delivered);
            }
        }
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Builds connected pairs of [`FakeAudio`].
pub struct LoopbackAudio;

impl LoopbackAudio {
    /// Two [`FakeAudio`] devices, both at `sample_rate`, wired so that
    /// playing on one immediately delivers those samples to the other's
    /// input sink (once it has one registered via
    /// [`AudioIo::start_input`]), as if they shared a room. Add ambient
    /// noise to either side with [`FakeAudio::with_noise`] before using
    /// it.
    pub fn pair(sample_rate: u32) -> (FakeAudio, FakeAudio) {
        let mut a = FakeAudio::new(sample_rate);
        let mut b = FakeAudio::new(sample_rate);
        a.forward_to = Some(b.sink.clone());
        b.forward_to = Some(a.sink.clone());
        (a, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_mic_is_reported_not_panicked() {
        let mut a = FakeAudio::denied();
        assert_eq!(
            a.start_input(Box::new(|_| {})),
            Err(AudioError::PermissionDenied)
        );
    }

    #[test]
    fn loopback_pair_delivers_played_samples_to_the_partner() {
        let (mut a, mut b) = LoopbackAudio::pair(8_000);

        let heard: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let heard_for_sink = heard.clone();
        b.start_input(Box::new(move |s: &[f32]| {
            heard_for_sink.lock().unwrap().extend_from_slice(s);
        }))
        .unwrap();

        a.play(vec![0.1, 0.2, 0.3]).unwrap();

        assert_eq!(a.played(), vec![0.1, 0.2, 0.3]);
        assert_eq!(*heard.lock().unwrap(), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn loopback_pair_mixes_in_noise() {
        let (a, mut b) = LoopbackAudio::pair(8_000);
        let mut a = a.with_noise(|| 1.0);

        let heard: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let heard_for_sink = heard.clone();
        b.start_input(Box::new(move |s: &[f32]| {
            heard_for_sink.lock().unwrap().extend_from_slice(s);
        }))
        .unwrap();

        a.play(vec![0.0, 0.0]).unwrap();

        assert_eq!(*heard.lock().unwrap(), vec![1.0, 1.0]);
    }

    #[test]
    fn feed_reaches_a_registered_sink() {
        let mut a = FakeAudio::new(8_000);
        let heard: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let heard_for_sink = heard.clone();
        a.start_input(Box::new(move |s: &[f32]| {
            heard_for_sink.lock().unwrap().extend_from_slice(s);
        }))
        .unwrap();

        a.feed(&[9.0, 8.0]);
        assert_eq!(*heard.lock().unwrap(), vec![9.0, 8.0]);
    }
}
