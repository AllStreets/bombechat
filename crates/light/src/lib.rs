//! bombechat-light: sending sealed envelopes screen to camera.
//!
//! [`code`] defines the grid one frame is drawn as and the bytes it
//! carries, [`fountain`] streams an envelope as RaptorQ-coded frames and
//! collects them back, and [`render`] draws a frame as a grey image.

pub mod code;
pub mod fountain;
pub mod render;

pub use code::{orientation, Cell, CodeSpec, Layout, Orientation};
pub use fountain::{encode, Collector, Encoder, LightEvent};
pub use render::{geometry, read_clean, render, Geometry};
