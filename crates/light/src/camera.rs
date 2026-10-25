//! Camera input: the [`CameraIo`] trait the light channel takes pictures
//! through, and [`AvCamera`], its implementation over the default camera
//! through AVFoundation (macOS only).
//!
//! Pictures are always grey ([`GrayImage`]), whatever the camera gives:
//! the luma plane of a YCbCr picture as it is, or the luma of an RGB one.
//! The reader thresholds each picture against its local mean, so the
//! video range of the luma (16 to 235) needs no stretching.

use std::fmt;

use image::GrayImage;

/// What can go wrong with the camera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraError {
    /// No camera is connected, or the one in use went away.
    NoDevice,
    /// The user (or a policy) has not allowed this app to use the camera.
    PermissionDenied,
    /// The camera cannot do what was asked, or some other failure that
    /// does not fit the other variants. The string is a detail for logs.
    Unsupported(String),
}

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CameraError::NoDevice => write!(f, "no camera available"),
            CameraError::PermissionDenied => write!(f, "camera access denied"),
            CameraError::Unsupported(detail) => write!(f, "camera unsupported: {detail}"),
        }
    }
}

impl std::error::Error for CameraError {}

/// A source of grey camera pictures.
pub trait CameraIo: Send {
    /// Open the camera and start delivering pictures. `sink` is called
    /// from a capture thread with each picture as it arrives; while it
    /// runs, later pictures are dropped rather than queued, so a slow
    /// sink sees the newest picture next. Starting a camera that is
    /// already running stops it first. The first start may ask the user
    /// for camera permission and wait for the answer.
    #[allow(clippy::type_complexity)]
    fn start(&mut self, sink: Box<dyn FnMut(GrayImage) + Send>) -> Result<(), CameraError>;

    /// Stop delivering pictures and release the camera, so the hardware
    /// and its light go off. Once this returns, `sink` is not called
    /// again (and has been dropped). Does nothing if the camera is not
    /// running.
    fn stop(&mut self);

    /// An error since the last call (the camera unplugged, or capture
    /// stopped on its own), if any; each one is returned once. `start`
    /// only reports failures to open the camera, so a caller polls this
    /// to learn that a running camera has died.
    fn take_error(&mut self) -> Option<CameraError>;
}

/// A grey picture from the first `width` bytes of each of `height` rows,
/// `stride` bytes apart, of an 8-bit luma plane. `None` if the plane is
/// shorter than that.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn from_luma(plane: &[u8], width: usize, height: usize, stride: usize) -> Option<GrayImage> {
    if width == 0 || height == 0 || stride < width || plane.len() < stride * (height - 1) + width {
        return None;
    }
    let mut out = Vec::with_capacity(width * height);
    for row in plane.chunks(stride).take(height) {
        out.extend_from_slice(&row[..width]);
    }
    GrayImage::from_raw(width as u32, height as u32, out)
}

/// A grey picture from `height` rows of `width` BGRA pixels, `stride`
/// bytes apart: the Rec. 709 luma of each, in whole numbers. `None` if
/// the buffer is shorter than that.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn from_bgra(pixels: &[u8], width: usize, height: usize, stride: usize) -> Option<GrayImage> {
    if width == 0
        || height == 0
        || stride < 4 * width
        || pixels.len() < stride * (height - 1) + 4 * width
    {
        return None;
    }
    let mut out = Vec::with_capacity(width * height);
    for row in pixels.chunks(stride).take(height) {
        out.extend(row[..4 * width].chunks_exact(4).map(|p| {
            // 0.0722 B + 0.7152 G + 0.2126 R, in 1/1024ths.
            let y = 74 * p[0] as u32 + 732 * p[1] as u32 + 218 * p[2] as u32;
            ((y + 512) >> 10).min(255) as u8
        }));
    }
    GrayImage::from_raw(width as u32, height as u32, out)
}

#[cfg(target_os = "macos")]
pub use avf::AvCamera;

#[cfg(target_os = "macos")]
mod avf {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{mpsc, Arc, Mutex, MutexGuard};

    use block2::RcBlock;
    use dispatch2::{DispatchQueue, DispatchRetained};
    use image::GrayImage;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
    use objc2_av_foundation::{
        AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput,
        AVCaptureOutput, AVCaptureSession, AVCaptureSessionPreset1280x720,
        AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType,
        AVMediaTypeVideo,
    };
    use objc2_core_media::CMSampleBuffer;
    use objc2_core_video::{
        kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVReturnSuccess, CVPixelBuffer,
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
        CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
        CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
        CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress,
    };
    use objc2_foundation::{NSDictionary, NSError, NSNumber, NSString};

    use super::{from_bgra, from_luma, CameraError, CameraIo};

    type Sink = Box<dyn FnMut(GrayImage) + Send>;

    /// The pixel formats pictures can come in: 4:2:0 YCbCr, full and video
    /// range, whose first plane is the luma, and BGRA.
    const LUMA_FULL: u32 = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
    const LUMA_VIDEO: u32 = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    const BGRA: u32 = kCVPixelFormatType_32BGRA;

    /// `AVErrorApplicationIsNotAuthorizedToUseDevice`.
    const NOT_AUTHORIZED: isize = -11852;
    /// `AVErrorDeviceNotConnected`.
    const NOT_CONNECTED: isize = -11814;

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What the capture thread shares with the camera.
    pub(super) struct Shared {
        sink: Mutex<Option<Sink>>,
        error: Arc<Mutex<Option<CameraError>>>,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and `Frames`
        // does not implement Drop.
        #[unsafe(super(NSObject))]
        #[name = "BombechatCameraFrames"]
        #[ivars = Shared]
        pub(super) struct Frames;

        unsafe impl NSObjectProtocol for Frames {}

        unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Frames {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            fn frame(
                &self,
                _output: &AVCaptureOutput,
                buffer: &CMSampleBuffer,
                _connection: &AVCaptureConnection,
            ) {
                self.deliver(buffer);
            }
        }
    );

    impl Frames {
        fn new(shared: Shared) -> Retained<Self> {
            let this = Self::alloc().set_ivars(shared);
            // SAFETY: NSObject's init on a freshly allocated object.
            unsafe { msg_send![super(this), init] }
        }

        /// Hand one captured picture to the sink, grey, or record why it
        /// could not be. Never unwinds into AVFoundation: a sink that
        /// panics is dropped and the panic recorded as an error.
        fn deliver(&self, buffer: &CMSampleBuffer) {
            let shared = self.ivars();
            let mut slot = lock(&shared.sink);
            let Some(sink) = slot.as_mut() else {
                return;
            };
            // SAFETY: a sample buffer from a video data output holds an
            // image buffer, or nothing.
            let picture = match unsafe { buffer.image_buffer() } {
                Some(pixels) => grey(&pixels),
                None => Err(CameraError::Unsupported("a frame with no picture".into())),
            };
            match picture {
                Ok(picture) => {
                    if catch_unwind(AssertUnwindSafe(|| sink(picture))).is_err() {
                        *slot = None;
                        *lock(&shared.error) = Some(CameraError::Unsupported(
                            "the picture handler panicked".into(),
                        ));
                    }
                }
                Err(error) => *lock(&shared.error) = Some(error),
            }
        }
    }

    /// `pixels` as a grey picture: the luma plane of a 4:2:0 picture as it
    /// is, or the luma of a BGRA one.
    fn grey(pixels: &CVPixelBuffer) -> Result<GrayImage, CameraError> {
        let flags = CVPixelBufferLockFlags::ReadOnly;
        // SAFETY: a valid pixel buffer, unlocked below with the same flags.
        if unsafe { CVPixelBufferLockBaseAddress(pixels, flags) } != kCVReturnSuccess {
            return Err(CameraError::Unsupported(
                "a picture that cannot be read".into(),
            ));
        }
        let format = CVPixelBufferGetPixelFormatType(pixels);
        // SAFETY: while the buffer is locked, each base address points to
        // at least bytes-per-row times height bytes.
        let picture = unsafe {
            match format {
                LUMA_FULL | LUMA_VIDEO => plane(
                    CVPixelBufferGetBaseAddressOfPlane(pixels, 0),
                    CVPixelBufferGetHeightOfPlane(pixels, 0),
                    CVPixelBufferGetBytesPerRowOfPlane(pixels, 0),
                )
                .and_then(|p| {
                    from_luma(
                        p,
                        CVPixelBufferGetWidthOfPlane(pixels, 0),
                        CVPixelBufferGetHeightOfPlane(pixels, 0),
                        CVPixelBufferGetBytesPerRowOfPlane(pixels, 0),
                    )
                }),
                BGRA => plane(
                    CVPixelBufferGetBaseAddress(pixels),
                    CVPixelBufferGetHeight(pixels),
                    CVPixelBufferGetBytesPerRow(pixels),
                )
                .and_then(|p| {
                    from_bgra(
                        p,
                        CVPixelBufferGetWidth(pixels),
                        CVPixelBufferGetHeight(pixels),
                        CVPixelBufferGetBytesPerRow(pixels),
                    )
                }),
                _ => None,
            }
        };
        // SAFETY: locked above with these flags.
        unsafe { CVPixelBufferUnlockBaseAddress(pixels, flags) };
        picture.ok_or_else(|| CameraError::Unsupported(format!("pixel format {format:#010x}")))
    }

    /// The `height` rows of `stride` bytes at `base`, or `None` if null.
    ///
    /// # Safety
    ///
    /// `base` must point to that many readable bytes for as long as the
    /// slice is used.
    unsafe fn plane<'a>(
        base: *mut std::ffi::c_void,
        height: usize,
        stride: usize,
    ) -> Option<&'a [u8]> {
        (!base.is_null()).then(|| std::slice::from_raw_parts(base as *const u8, height * stride))
    }

    fn video() -> Result<&'static AVMediaType, CameraError> {
        // SAFETY: a constant AVFoundation exports.
        unsafe { AVMediaTypeVideo }
            .ok_or_else(|| CameraError::Unsupported("no video media type".into()))
    }

    /// Whether this app may use the camera, asking the user if it has not
    /// been decided (and waiting for the answer).
    fn authorize() -> Result<(), CameraError> {
        let media = video()?;
        // SAFETY: the video media type is one this accepts.
        let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
        decide(status, || {
            let (tx, rx) = mpsc::channel();
            let answer = RcBlock::new(move |granted: Bool| {
                let _ = tx.send(granted.as_bool());
            });
            // SAFETY: the video media type, and a block AVFoundation
            // copies and calls once, on a queue of its own.
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &answer) };
            rx.recv().unwrap_or(false)
        })
    }

    /// What an authorization status means for starting the camera; `ask`
    /// asks the user, if no one has decided yet.
    pub(super) fn decide(
        status: AVAuthorizationStatus,
        ask: impl FnOnce() -> bool,
    ) -> Result<(), CameraError> {
        match status {
            AVAuthorizationStatus::Authorized => Ok(()),
            AVAuthorizationStatus::NotDetermined if ask() => Ok(()),
            _ => Err(CameraError::PermissionDenied),
        }
    }

    /// The camera error an AVFoundation error stands for.
    fn from_ns_error(error: &NSError) -> CameraError {
        match error.code() {
            NOT_AUTHORIZED => CameraError::PermissionDenied,
            NOT_CONNECTED => CameraError::NoDevice,
            _ => CameraError::Unsupported(error.localizedDescription().to_string()),
        }
    }

    /// An open capture session and everything it holds.
    pub(super) struct Running {
        pub(super) session: Retained<AVCaptureSession>,
        device: Retained<AVCaptureDevice>,
        input: Retained<AVCaptureDeviceInput>,
        output: Retained<AVCaptureVideoDataOutput>,
        pub(super) frames: Retained<Frames>,
        queue: DispatchRetained<DispatchQueue>,
        /// The session's death has been reported by `take_error`.
        reported: bool,
    }

    impl Running {
        /// Stop the session, take the sink back and let go of the camera:
        /// the session stops, loses its input and output, and is dropped
        /// with them, so nothing holds the device. Once this returns the
        /// sink has been dropped and is never called again.
        fn close(self) {
            // SAFETY: plain AVFoundation calls on objects this owns;
            // stopRunning returns once capture has stopped, and a nil
            // delegate takes a nil queue.
            unsafe {
                self.session.stopRunning();
                self.output.setSampleBufferDelegate_queue(None, None);
                self.session.beginConfiguration();
                self.session.removeInput(&self.input);
                self.session.removeOutput(&self.output);
                self.session.commitConfiguration();
            }
            // A picture being delivered holds this lock; once it is ours,
            // no picture is being delivered and none will be.
            let sink = lock(&self.frames.ivars().sink).take();
            drop(sink);
        }
    }

    /// The default camera through AVFoundation: 1280x720 where the camera
    /// offers it (else its default size), at the camera's default rate (30
    /// pictures a second on Mac cameras at that size), delivered grey.
    ///
    /// Nothing is opened until [`CameraIo::start`], and [`CameraIo::stop`]
    /// (or dropping it) releases the camera entirely: the session is
    /// stopped, emptied and dropped, so the camera light goes off.
    pub struct AvCamera {
        pub(super) running: Option<Running>,
        error: Arc<Mutex<Option<CameraError>>>,
    }

    // SAFETY: the AVFoundation objects in `running` are only touched from
    // `&mut self` methods, so never from two threads at once, and
    // AVCaptureSession may be configured, started and stopped from any
    // thread (Apple's own samples use a background queue). The delegate
    // shares only `Shared`, whose fields are Mutexes of Send data.
    unsafe impl Send for AvCamera {}

    impl Default for AvCamera {
        fn default() -> Self {
            AvCamera::new()
        }
    }

    impl AvCamera {
        /// A camera, not yet started.
        pub fn new() -> Self {
            AvCamera {
                running: None,
                error: Arc::new(Mutex::new(None)),
            }
        }

        fn open(&mut self, sink: Sink) -> Result<Running, CameraError> {
            authorize()?;
            let media = video()?;
            // SAFETY: the video media type.
            let device = unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media) }
                .ok_or(CameraError::NoDevice)?;
            // SAFETY: a device of the video type.
            let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
                .map_err(|e| from_ns_error(&e))?;
            let frames = Frames::new(Shared {
                sink: Mutex::new(Some(sink)),
                error: self.error.clone(),
            });
            let queue = DispatchQueue::new("app.bombechat.camera", None);
            // SAFETY: new objects with no requirements.
            let (session, output) =
                unsafe { (AVCaptureSession::new(), AVCaptureVideoDataOutput::new()) };
            let running = Running {
                session,
                device,
                input,
                output,
                frames,
                queue,
                reported: false,
            };
            match Self::configure(&running) {
                Ok(()) => Ok(running),
                Err(e) => {
                    running.close();
                    Err(e)
                }
            }
        }

        /// Wire `r`'s input and output into its session and start it.
        fn configure(r: &Running) -> Result<(), CameraError> {
            let format = NSNumber::new_u32(LUMA_FULL);
            // SAFETY: kCVPixelBufferPixelFormatTypeKey is a CFString,
            // toll-free bridged to NSString.
            let key: &NSString =
                unsafe { &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString) };
            let value: &AnyObject = &format;
            let settings = NSDictionary::from_slices(&[key], &[value]);
            // SAFETY: AVFoundation calls on objects `r` owns, between
            // begin and commit; the delegate and a serial queue are
            // retained by `r` for as long as the output is.
            unsafe {
                let session = &r.session;
                session.beginConfiguration();
                let added = session.canAddInput(&r.input) && {
                    session.addInput(&r.input);
                    // Which presets a session can take depends on its
                    // camera, so this comes after the input.
                    let preset = AVCaptureSessionPreset1280x720;
                    if session.canSetSessionPreset(preset) {
                        session.setSessionPreset(preset);
                    }
                    r.output.setVideoSettings(Some(&settings));
                    r.output.setAlwaysDiscardsLateVideoFrames(true);
                    r.output.setSampleBufferDelegate_queue(
                        Some(ProtocolObject::from_ref(&*r.frames)),
                        Some(&r.queue),
                    );
                    session.canAddOutput(&r.output)
                };
                if added {
                    session.addOutput(&r.output);
                }
                session.commitConfiguration();
                if !added {
                    return Err(CameraError::Unsupported(
                        "the camera cannot be captured from".into(),
                    ));
                }
                session.startRunning();
                if !session.isRunning() {
                    return Err(CameraError::Unsupported(
                        "the camera did not start (another app may be using it)".into(),
                    ));
                }
            }
            Ok(())
        }
    }

    impl CameraIo for AvCamera {
        fn start(&mut self, sink: Sink) -> Result<(), CameraError> {
            self.stop();
            *lock(&self.error) = None;
            self.running = Some(self.open(sink)?);
            Ok(())
        }

        fn stop(&mut self) {
            if let Some(running) = self.running.take() {
                running.close();
            }
        }

        fn take_error(&mut self) -> Option<CameraError> {
            if let Some(error) = lock(&self.error).take() {
                return Some(error);
            }
            let running = self.running.as_mut()?;
            if running.reported {
                return None;
            }
            // SAFETY: property reads on objects `running` owns.
            let error = unsafe {
                if !running.device.isConnected() {
                    Some(CameraError::NoDevice)
                } else if !running.session.isRunning() {
                    Some(CameraError::Unsupported("the camera stopped".into()))
                } else {
                    None
                }
            };
            running.reported = error.is_some();
            error
        }
    }

    impl Drop for AvCamera {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_luma_plane_with_row_padding_reads_as_its_pixels() {
        // Two rows of three pixels, each row padded to five bytes.
        let plane = [1, 2, 3, 99, 99, 4, 5, 6, 99, 99];
        let img = from_luma(&plane, 3, 2, 5).unwrap();
        assert_eq!(img.dimensions(), (3, 2));
        assert_eq!(img.as_raw(), &[1, 2, 3, 4, 5, 6]);
        // The last row need not be padded.
        assert!(from_luma(&plane[..8], 3, 2, 5).is_some());
        assert!(from_luma(&plane[..7], 3, 2, 5).is_none());
        assert!(from_luma(&plane, 6, 2, 5).is_none(), "stride under width");
        assert!(from_luma(&[], 0, 0, 0).is_none());
    }

    #[test]
    fn bgra_reads_as_its_luma() {
        // White, black, pure green, pure blue; one row, padded.
        let px = [
            255, 255, 255, 255, 0, 0, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255, 7, 7,
        ];
        let img = from_bgra(&px, 4, 1, 18).unwrap();
        assert_eq!(img.as_raw(), &[255, 0, 182, 18]);
        assert!(from_bgra(&px[..15], 4, 1, 18).is_none());
    }

    #[cfg(target_os = "macos")]
    mod mac {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        use objc2::rc::Weak;
        use objc2_av_foundation::AVAuthorizationStatus;

        use super::super::avf::decide;
        use super::super::*;

        /// A camera the user has refused, or that a policy keeps from
        /// this app, gives PermissionDenied without asking again; one not
        /// yet decided asks, and a no is PermissionDenied too. None of it
        /// panics.
        #[test]
        fn a_denied_camera_gives_permission_denied() {
            let never = || -> bool { panic!("asked although already decided") };
            assert_eq!(
                decide(AVAuthorizationStatus::Denied, never),
                Err(CameraError::PermissionDenied)
            );
            assert_eq!(
                decide(AVAuthorizationStatus::Restricted, never),
                Err(CameraError::PermissionDenied)
            );
            assert_eq!(
                decide(AVAuthorizationStatus::NotDetermined, || false),
                Err(CameraError::PermissionDenied)
            );
            assert_eq!(
                decide(AVAuthorizationStatus::NotDetermined, || true),
                Ok(())
            );
            assert_eq!(decide(AVAuthorizationStatus::Authorized, never), Ok(()));
        }

        /// Whether any process has a camera running, as CoreMediaIO sees
        /// it: what turns the camera light on.
        fn a_camera_is_running() -> bool {
            use std::ffi::c_void;
            #[repr(C)]
            struct Address {
                selector: u32,
                scope: u32,
                element: u32,
            }
            #[link(name = "CoreMediaIO", kind = "framework")]
            extern "C" {
                fn CMIOObjectGetPropertyDataSize(
                    object: u32,
                    address: *const Address,
                    qualifier_size: u32,
                    qualifier: *const c_void,
                    size: *mut u32,
                ) -> i32;
                fn CMIOObjectGetPropertyData(
                    object: u32,
                    address: *const Address,
                    qualifier_size: u32,
                    qualifier: *const c_void,
                    size: u32,
                    used: *mut u32,
                    data: *mut c_void,
                ) -> i32;
            }
            let code = |s: &[u8; 4]| u32::from_be_bytes(*s);
            let address = |selector| Address {
                selector,
                scope: code(b"glob"),
                element: 0,
            };
            // kCMIOObjectSystemObject, kCMIOHardwarePropertyDevices.
            let devices = address(code(b"dev#"));
            let mut size = 0u32;
            // SAFETY: plain CoreMediaIO property reads into buffers of
            // the size it reports.
            unsafe {
                assert_eq!(
                    CMIOObjectGetPropertyDataSize(1, &devices, 0, std::ptr::null(), &mut size),
                    0
                );
                let mut ids = vec![0u32; size as usize / 4];
                let mut used = 0u32;
                assert_eq!(
                    CMIOObjectGetPropertyData(
                        1,
                        &devices,
                        0,
                        std::ptr::null(),
                        size,
                        &mut used,
                        ids.as_mut_ptr().cast()
                    ),
                    0
                );
                // kCMIODevicePropertyDeviceIsRunningSomewhere.
                let running = address(code(b"gone"));
                ids.iter().any(|&id| {
                    let mut on = 0u32;
                    let mut used = 0u32;
                    CMIOObjectGetPropertyData(
                        id,
                        &running,
                        0,
                        std::ptr::null(),
                        4,
                        &mut used,
                        (&mut on as *mut u32).cast(),
                    ) == 0
                        && on != 0
                })
            }
        }

        /// Opens the default camera, takes 10 pictures, stops, and checks
        /// that stopping let go of the camera: the session stopped and was
        /// freed, and CoreMediaIO sees no camera running (the light is
        /// off). Then starts it again.
        ///
        /// Ignored by default: it needs a camera, nothing else using it,
        /// and camera permission for the process running the tests (the
        /// terminal). If that has not been decided, macOS asks, and the
        /// test waits for the answer. Run it by hand:
        ///
        /// ```sh
        /// cargo test -p bombechat-light --lib camera -- --ignored --nocapture
        /// ```
        #[test]
        #[ignore = "needs a camera and camera permission; run by hand"]
        fn the_default_camera_gives_pictures_and_stopping_lets_it_go() {
            assert!(!a_camera_is_running(), "close other apps using the camera");
            let mut camera = AvCamera::new();
            let (tx, rx) = mpsc::channel();
            let opened = Instant::now();
            camera
                .start(Box::new(move |img| {
                    let _ = tx.send((Instant::now(), img));
                }))
                .expect("start the default camera");
            println!("started in {:.0} ms", opened.elapsed().as_secs_f64() * 1e3);
            let pictures: Vec<_> = (0..10)
                .map(|i| {
                    rx.recv_timeout(Duration::from_secs(5))
                        .unwrap_or_else(|_| panic!("picture {i} within 5 s"))
                })
                .collect();
            let (first, last) = (pictures[0].0, pictures[9].0);
            let rate = 9.0 / (last - first).as_secs_f64();
            let img = &pictures[9].1;
            let mean =
                img.as_raw().iter().map(|&p| p as f64).sum::<f64>() / img.as_raw().len() as f64;
            println!(
                "10 pictures, {}x{}, {rate:.1} a second, mean grey {mean:.0}",
                img.width(),
                img.height()
            );
            assert!(img.width() > 0 && img.height() > 0);
            assert!(a_camera_is_running(), "the camera is on while started");
            assert_eq!(camera.take_error(), None);

            let running = camera.running.as_ref().expect("running");
            let session = Weak::from_retained(&running.session);
            let frames = Weak::from_retained(&running.frames);
            camera.stop();
            assert!(camera.running.is_none());
            let session_running = session.load().map(|s| unsafe { s.isRunning() });
            println!("after stop: session isRunning {session_running:?}");
            assert_ne!(session_running, Some(true));
            let stopped = Instant::now();
            while (session.load().is_some() || frames.load().is_some() || a_camera_is_running())
                && stopped.elapsed() < Duration::from_secs(3)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            println!(
                "after {:.0} ms: session freed {}, delegate freed {}, a camera running {}",
                stopped.elapsed().as_secs_f64() * 1e3,
                session.load().is_none(),
                frames.load().is_none(),
                a_camera_is_running()
            );
            assert!(session.load().is_none(), "the session is freed");
            assert!(frames.load().is_none(), "the delegate is freed");
            assert!(
                !a_camera_is_running(),
                "no camera running: the light is off"
            );
            // Pictures sent before stop may be waiting; after them the
            // channel is closed, because stop dropped the sink.
            while rx.try_recv().is_ok() {}
            assert_eq!(
                rx.try_recv(),
                Err(mpsc::TryRecvError::Disconnected),
                "stop dropped the sink"
            );

            let (tx, rx) = mpsc::channel();
            camera
                .start(Box::new(move |img| {
                    let _ = tx.send(img);
                }))
                .expect("start again");
            rx.recv_timeout(Duration::from_secs(5))
                .expect("a picture after starting again");
            drop(camera);
            let dropped = Instant::now();
            while a_camera_is_running() && dropped.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!a_camera_is_running(), "dropping the camera lets it go");
        }
    }
}
