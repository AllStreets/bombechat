//! bombechat-light: sending sealed envelopes screen to camera.
//!
//! [`code`] defines the grid one frame is drawn as and the bytes it
//! carries, [`fountain`] streams an envelope as RaptorQ-coded frames and
//! collects them back, [`mod@render`] draws a frame as a grey image, and
//! [`decode`] reads frames back out of camera pictures, which [`camera`]
//! takes.

pub mod camera;
pub mod code;
pub mod decode;
mod detect;
#[cfg(feature = "fake-camera")]
pub mod fake;
pub mod fountain;
pub mod render;

#[cfg(target_os = "macos")]
pub use camera::AvCamera;
pub use camera::{CameraAccess, CameraError, CameraIo};

pub use code::{orientation, Cell, CodeSpec, Layout, Orientation};
pub use decode::{read, read_within, ReadOutcome, Reader, DEFAULT_BUDGET};
#[cfg(feature = "fake-camera")]
pub use fake::FakeCamera;
pub use fountain::{encode, Collector, Encoder, LightEvent};
pub use render::{geometry, read_clean, render, Geometry};
