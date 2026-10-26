//! bombechat-modem: sound and light modem encode/decode.

pub mod audio;
#[cfg(feature = "fake-audio")]
pub mod fake;
pub mod frames;
pub mod fsk;
pub mod gate;

pub use audio::*;
#[cfg(feature = "fake-audio")]
pub use fake::*;
pub use frames::*;
pub use fsk::*;
pub use gate::*;
