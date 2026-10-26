//! What this build fits to the machine beyond sound and light: its own
//! carriers, the settings they bring, and how their channels say what
//! they are doing. The airgapped build fits nothing more, so everything
//! here is empty. The standard build (`bluetooth/`) lays its own copy of
//! this file over this one, so the two builds share every other file of
//! the app, as they share `app/ui/edition.css`.

use bombechat_node::{NodeEvent, Transport};
use bombechat_store::Channel;

/// The settings this build adds to the shared ones, as
/// [`crate::commands::SETTINGS`] lists them: id, how many values, and the
/// value it has until the person changes it.
pub const SETTINGS: &[(&str, usize, usize)] = &[];

/// Whether every contact is added by checking keys in person: this build
/// has no other way to add one, so New contacts need a key check is held
/// On ([`crate::commands::KEY_CHECK_ALWAYS`]).
pub const ONLY_IN_PERSON: bool = true;

/// The parts this build fits, as the machine holds them.
pub struct Fittings;

impl Fittings {
    /// None of the parts: for a machine over the carriers it is given.
    pub fn none() -> Fittings {
        Fittings
    }

    /// The parts on this computer's own hardware: the fittings, their
    /// carriers for the node, and what to report on the first tick.
    pub fn hardware() -> (Fittings, Vec<Box<dyn Transport>>, Vec<NodeEvent>) {
        (Fittings, Vec::new(), Vec::new())
    }

    /// A setting this build added ([`SETTINGS`]) was set, or is applied
    /// at start.
    pub fn apply(&mut self, _id: &str, _value_index: usize) {}

    /// The store was opened: these parts may now work.
    pub fn unlocked(&mut self) {}

    /// The state of each channel these parts carry, as its switch alone
    /// says it: what `channels` shows before the channel has reported.
    pub fn states(&self) -> Vec<(Channel, String)> {
        Vec::new()
    }

    /// After each tick of the node: put each of these channels' states in
    /// this build's words, and report a switch turned since the last tick.
    pub fn tick(&mut self, _events: &mut Vec<NodeEvent>) {}
}
