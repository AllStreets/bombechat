//! Bombechat's desktop shell, airgapped build: the web interface in
//! `app/ui/` in a Tauri window, wired to a [`Node`] listening and playing
//! through the sound card.
//!
//! The node lives behind a mutex ([`commands::Shared`]). A pump thread
//! ticks it every 20 ms and re-emits each [`bombechat_node::NodeEvent`] as
//! the bridge event `bridge.js` listens for; each command locks it only
//! for its own call. The store lives in the app data folder: the first
//! passphrase the page gives creates it, later ones open it.

pub mod commands;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use bombechat_core::{Identity, PublicIdentity};
use bombechat_modem::{CpalAudio, ModemConfig, QuietGate};
use bombechat_node::{Node, NodeEvent, SoundTransport, Transport};
use bombechat_store::{Channel, Contact, ContactId, Store, StoreError};
use tauri::{AppHandle, Emitter, Manager, State};

use commands::{bridge_event, Lock, Machine, Shared, MACHINE_NAME};

/// How often the pump ticks the node.
const TICK: Duration = Duration::from_millis(20);

/// The quiet gate's defaults, as the settings page shows them: send under
/// 40 dB, two repeats a minute and a half apart.
const SEND_UNDER_DB: f32 = 40.0;
const REPEATS: u8 = 2;
const REPEAT_GAP_S: u32 = 90;

/// The store's file name in the app data folder.
const STORE_FILE: &str = "bombechat.db";

/// The real machine: a node over the sound card and the store on disk.
struct Device {
    node: Node,
    store_path: PathBuf,
    unlocked: bool,
    /// Settings as the page last set them. Plan 1's node has nothing to
    /// retune yet (band, gate, volume), so they are only kept.
    settings: HashMap<String, usize>,
    /// Reported on the first tick: why there is no sound transport.
    pending: Vec<NodeEvent>,
}

impl Device {
    fn new(store_path: PathBuf) -> Device {
        let mut pending = Vec::new();
        let transports: Vec<Box<dyn Transport>> = match CpalAudio::new() {
            Ok(audio) => {
                let cfg = ModemConfig {
                    sample_rate: bombechat_modem::AudioIo::sample_rate(&audio),
                    ..ModemConfig::default()
                };
                let gate = QuietGate::new(SEND_UNDER_DB, REPEATS, REPEAT_GAP_S);
                vec![Box::new(SoundTransport::new(Box::new(audio), cfg, gate))]
            }
            Err(e) => {
                pending.push(NodeEvent::ChannelState {
                    channel: Channel::Sound,
                    state: format!("unavailable: {e}"),
                });
                Vec::new()
            }
        };
        Device {
            node: Node::new(None, transports),
            store_path,
            unlocked: false,
            settings: HashMap::new(),
            pending,
        }
    }
}

impl Machine for Device {
    fn send(&mut self, to: &[ContactId], text: &str, channel: Channel) -> [u8; 16] {
        self.node.send(to, text, channel)
    }

    fn broadcast(&mut self, text: &str) -> [u8; 16] {
        self.node.broadcast(text)
    }

    fn start_pairing(&mut self, my_name: &str) {
        self.node.start_pairing(my_name)
    }

    fn confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId {
        self.node.confirm_pairing(peer, name)
    }

    fn contacts(&self) -> Vec<Contact> {
        self.node.contacts()
    }

    fn set_setting(&mut self, id: &str, value_index: usize) {
        self.settings.insert(id.to_string(), value_index);
    }

    fn lock(&self) -> Lock {
        if self.unlocked {
            Lock::Unlocked
        } else if self.store_path.exists() {
            Lock::Locked
        } else {
            Lock::NoStore
        }
    }

    fn unlock(&mut self, passphrase: &str) -> Result<(), String> {
        let store = match self.lock() {
            Lock::Unlocked => return Err("already unlocked".to_string()),
            Lock::Locked => Store::open(&self.store_path, passphrase),
            Lock::NoStore if passphrase.is_empty() => return Err("choose a passphrase".to_string()),
            Lock::NoStore => Store::create(
                &self.store_path,
                passphrase,
                &Identity::generate(),
                MACHINE_NAME,
            ),
        };
        let store = store.map_err(|e| match e {
            StoreError::WrongPassphrase => "wrong passphrase".to_string(),
            other => other.to_string(),
        })?;
        self.node.unlock(store);
        self.unlocked = true;
        Ok(())
    }

    fn tick(&mut self, now_ms: u64) -> Vec<NodeEvent> {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(self.node.tick(now_ms));
        events
    }
}

#[tauri::command]
fn send(
    state: State<'_, Shared>,
    contact_ids: Vec<i64>,
    text: String,
    channel: String,
) -> Result<String, String> {
    commands::send(&state, contact_ids, text, channel)
}

#[tauri::command]
fn broadcast(state: State<'_, Shared>, text: String) -> Result<String, String> {
    commands::broadcast(&state, text)
}

#[tauri::command]
fn pair(state: State<'_, Shared>) -> Result<(), String> {
    commands::pair(&state)
}

#[tauri::command]
fn confirm_pair(state: State<'_, Shared>, peer_hex: String, name: String) -> Result<i64, String> {
    commands::confirm_pair(&state, peer_hex, name)
}

#[tauri::command]
fn set_setting(state: State<'_, Shared>, id: String, value_index: usize) -> Result<(), String> {
    commands::set_setting(&state, id, value_index)
}

/// Async, so the key derivation (Argon2id, most of a second) runs off the
/// window's thread.
#[tauri::command]
async fn unlock(state: State<'_, Shared>, passphrase: String) -> Result<(), String> {
    commands::unlock(&state, passphrase)
}

#[tauri::command]
fn contacts(state: State<'_, Shared>) -> Result<Vec<commands::ContactView>, String> {
    commands::contacts(&state)
}

/// Tick the node every [`TICK`] and emit what it reports.
fn pump(app: AppHandle) {
    let start = Instant::now();
    loop {
        thread::sleep(TICK);
        let now_ms = start.elapsed().as_millis() as u64;
        let events = {
            let state = app.state::<Shared>();
            let mut machine = state.lock().unwrap_or_else(|p| p.into_inner());
            machine.tick(now_ms)
        };
        for e in &events {
            let (name, payload) = bridge_event(e);
            // A page between loads misses the event, as a closed one would.
            let _ = app.emit(name, payload);
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let machine: Box<dyn Machine> = Box::new(Device::new(dir.join(STORE_FILE)));
            app.manage::<Shared>(Mutex::new(machine));
            let handle = app.handle().clone();
            thread::Builder::new()
                .name("node-pump".to_string())
                .spawn(move || pump(handle))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            send,
            broadcast,
            pair,
            confirm_pair,
            set_setting,
            unlock,
            contacts
        ])
        .run(tauri::generate_context!())
        .expect("Bombechat could not start");
}
