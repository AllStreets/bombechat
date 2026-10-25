//! A test double for [`crate::CameraIo`], behind the `fake-camera`
//! feature so it never ships in a build that does not ask for it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use image::GrayImage;

use crate::camera::{CameraError, CameraIo};

type Sink = Box<dyn FnMut(GrayImage) + Send>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A camera that plays a scripted sequence of pictures, one at a time when
/// told to, and counts how often it was started and stopped.
///
/// [`Clone`] shares its state, so a test can keep a handle to play
/// pictures and check the counts after a clone has been moved into
/// whatever owns it as a `Box<dyn CameraIo>`.
#[derive(Clone)]
pub struct FakeCamera {
    denied: bool,
    pictures: Arc<Mutex<VecDeque<GrayImage>>>,
    sink: Arc<Mutex<Option<Sink>>>,
    error: Arc<Mutex<Option<CameraError>>>,
    starts: Arc<Mutex<u32>>,
    stops: Arc<Mutex<u32>>,
    /// Starts still to refuse, and with what.
    refusing: Arc<Mutex<(u32, Option<CameraError>)>>,
}

impl FakeCamera {
    /// A camera that will show `pictures`, in order.
    pub fn new(pictures: Vec<GrayImage>) -> Self {
        FakeCamera {
            denied: false,
            pictures: Arc::new(Mutex::new(pictures.into())),
            sink: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
            starts: Arc::new(Mutex::new(0)),
            stops: Arc::new(Mutex::new(0)),
            refusing: Arc::new(Mutex::new((0, None))),
        }
    }

    /// A camera the user has not allowed: [`CameraIo::start`] always
    /// returns [`CameraError::PermissionDenied`].
    pub fn denied() -> Self {
        FakeCamera {
            denied: true,
            ..FakeCamera::new(Vec::new())
        }
    }

    /// Add `picture` to the end of the script, for a test that makes
    /// pictures as it goes (a screen another node is drawing on).
    pub fn add(&self, picture: GrayImage) {
        lock(&self.pictures).push_back(picture);
    }

    /// Show the next picture to the running sink. False, showing nothing,
    /// if the camera is not running or the script has run out. A picture
    /// shown while stopped is not used up.
    pub fn show_next(&self) -> bool {
        let mut sink = lock(&self.sink);
        let Some(sink) = sink.as_mut() else {
            return false;
        };
        let Some(picture) = lock(&self.pictures).pop_front() else {
            return false;
        };
        sink(picture);
        true
    }

    /// Show every picture left, while the camera runs; how many were
    /// shown.
    pub fn show_all(&self) -> usize {
        let mut shown = 0;
        while self.show_next() {
            shown += 1;
        }
        shown
    }

    /// Pictures left to show.
    pub fn remaining(&self) -> usize {
        lock(&self.pictures).len()
    }

    /// Have [`CameraIo::take_error`] report `error` next, as a camera
    /// that failed while running would.
    pub fn fail(&self, error: CameraError) {
        *lock(&self.error) = Some(error);
    }

    /// Have the next `n` calls to [`CameraIo::start`] return `error`, as
    /// a camera still in use by another app, or not plugged back in yet,
    /// would.
    pub fn refuse_starts(&self, n: u32, error: CameraError) {
        *lock(&self.refusing) = (n, Some(error));
    }

    /// Times [`CameraIo::start`] has been called, successful or not.
    pub fn starts(&self) -> u32 {
        *lock(&self.starts)
    }

    /// Times [`CameraIo::stop`] has been called.
    pub fn stops(&self) -> u32 {
        *lock(&self.stops)
    }

    /// Whether a sink is registered: true once `start` succeeds, false
    /// again once `stop` releases it.
    pub fn is_running(&self) -> bool {
        lock(&self.sink).is_some()
    }
}

impl CameraIo for FakeCamera {
    fn start(&mut self, sink: Sink) -> Result<(), CameraError> {
        *lock(&self.starts) += 1;
        if self.denied {
            return Err(CameraError::PermissionDenied);
        }
        let mut refusing = lock(&self.refusing);
        if refusing.0 > 0 {
            refusing.0 -= 1;
            return Err(refusing.1.clone().expect("set with the count"));
        }
        drop(refusing);
        *lock(&self.sink) = Some(sink);
        Ok(())
    }

    fn stop(&mut self) {
        *lock(&self.stops) += 1;
        *lock(&self.sink) = None;
    }

    fn take_error(&mut self) -> Option<CameraError> {
        lock(&self.error).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(grey: u8) -> GrayImage {
        GrayImage::from_pixel(4, 3, image::Luma([grey]))
    }

    fn collecting() -> (Arc<Mutex<Vec<u8>>>, Sink) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = seen.clone();
        let sink: Sink =
            Box::new(move |img: GrayImage| into.lock().unwrap().push(img[(0, 0)].0[0]));
        (seen, sink)
    }

    #[test]
    fn start_and_stop_are_counted_and_stop_releases_the_sink() {
        let mut camera = FakeCamera::new(vec![picture(1), picture(2), picture(3)]);
        let handle = camera.clone();
        assert_eq!((handle.starts(), handle.stops()), (0, 0));
        assert!(!handle.show_next(), "nothing is shown before start");

        let (seen, sink) = collecting();
        camera.start(sink).unwrap();
        assert_eq!((handle.starts(), handle.stops()), (1, 0));
        assert!(handle.is_running());
        assert!(handle.show_next());

        camera.stop();
        assert_eq!((handle.starts(), handle.stops()), (1, 1));
        assert!(!handle.is_running(), "stop releases the sink");
        assert!(!handle.show_next(), "nothing is shown after stop");
        assert_eq!(handle.remaining(), 2, "a picture not shown is kept");

        let (again, sink) = collecting();
        camera.start(sink).unwrap();
        assert_eq!(handle.show_all(), 2);
        assert_eq!((handle.starts(), handle.stops()), (2, 1));
        assert_eq!(*seen.lock().unwrap(), vec![1]);
        assert_eq!(*again.lock().unwrap(), vec![2, 3]);
        assert!(!handle.show_next(), "the script has run out");
    }

    #[test]
    fn a_denied_camera_gives_permission_denied_and_is_counted() {
        let mut camera = FakeCamera::denied();
        let (_, sink) = collecting();
        assert_eq!(camera.start(sink), Err(CameraError::PermissionDenied));
        assert_eq!(camera.starts(), 1);
        assert!(!camera.is_running());
    }

    #[test]
    fn a_failure_is_taken_once() {
        let mut camera = FakeCamera::new(Vec::new());
        assert_eq!(camera.take_error(), None);
        camera.fail(CameraError::NoDevice);
        assert_eq!(camera.take_error(), Some(CameraError::NoDevice));
        assert_eq!(camera.take_error(), None);
    }
}
