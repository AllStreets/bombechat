//! bombechat-light: sending sealed envelopes screen to camera.
//!
//! [`code`] defines the grid one frame is drawn as and the bytes it
//! carries, [`fountain`] streams an envelope as RaptorQ-coded frames and
//! collects them back, [`mod@render`] draws a frame as a grey image, and
//! [`decode`] reads frames back out of camera pictures.

pub mod code;
pub mod decode;
mod detect;
pub mod fountain;
pub mod render;

pub use code::{orientation, Cell, CodeSpec, Layout, Orientation};
pub use decode::{read, ReadOutcome};
pub use fountain::{encode, Collector, Encoder, LightEvent};
pub use render::{geometry, read_clean, render, Geometry};
