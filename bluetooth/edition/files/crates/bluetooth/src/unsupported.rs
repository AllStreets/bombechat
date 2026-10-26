//! The radio on a system Bombechat has no Bluetooth code for yet: it
//! never touches any hardware and says so at the first start.

use bombechat_session::TOKEN_LEN;

use crate::radio::{LinkId, Mode, Radio, RadioEvent};

/// What the page shows on a system with no Bluetooth radio here yet.
pub const BLUETOOTH_NOT_SUPPORTED: &str = "Bluetooth is not supported on this system yet";

/// A radio that only reports [`BLUETOOTH_NOT_SUPPORTED`]. On systems
/// other than macOS it is `CoreBluetoothRadio`.
#[derive(Debug, Default)]
pub struct UnsupportedRadio {
    said: bool,
    pending: Vec<RadioEvent>,
}

impl UnsupportedRadio {
    pub fn new() -> UnsupportedRadio {
        UnsupportedRadio::default()
    }
}

impl Radio for UnsupportedRadio {
    fn start(&mut self, _mode: Mode) {
        if !self.said {
            self.said = true;
            self.pending
                .push(RadioEvent::Unavailable(BLUETOOTH_NOT_SUPPORTED.to_string()));
        }
    }

    fn stop(&mut self) {
        self.said = false;
        self.pending.clear();
    }

    fn advertise(&mut self, _token: [u8; TOKEN_LEN]) {}

    fn disconnect(&mut self, _link: LinkId) {}

    fn write(&mut self, _link: LinkId, _bytes: &[u8]) {}

    fn poll(&mut self) -> Vec<RadioEvent> {
        std::mem::take(&mut self.pending)
    }

    fn max_write(&self, _link: LinkId) -> usize {
        20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_says_bluetooth_is_not_supported_once_per_start() {
        let mut r = UnsupportedRadio::new();
        assert!(r.poll().is_empty());
        r.start(Mode::Contacts);
        r.start(Mode::Open);
        r.advertise([1; TOKEN_LEN]);
        assert_eq!(
            r.poll(),
            vec![RadioEvent::Unavailable(
                "Bluetooth is not supported on this system yet".into()
            )]
        );
        assert!(r.poll().is_empty());
        r.stop();
        r.start(Mode::Contacts);
        assert_eq!(r.poll().len(), 1);
    }
}
