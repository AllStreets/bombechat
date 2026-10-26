//! Bombechat's desktop shell, airgapped build: the web interface in
//! `app/ui/` in a Tauri window, wired to a [`bombechat_node::Node`]
//! listening and playing through the sound card, and reading through the
//! camera and flashing codes on the page.
//!
//! The node lives behind a mutex in [`commands::Shared`]. A pump thread
//! ticks it every 20 ms and re-emits each [`bombechat_node::NodeEvent`] as
//! the bridge event `bridge.js` listens for; each command locks it only
//! for its own call. The store lives in the app data folder: the first
//! passphrase the page gives creates it, later ones open it.

pub mod commands;
pub mod device;
pub mod edition;
pub mod keychain;

use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};

pub use device::Device;

use commands::{bridge_event, Shared};

/// How often the pump ticks the node.
pub(crate) const TICK: Duration = Duration::from_millis(20);

/// The store's file name in the app data folder.
const STORE_FILE: &str = "bombechat.db";

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
fn broadcast(state: State<'_, Shared>, text: String, channel: String) -> Result<String, String> {
    commands::broadcast(&state, text, channel)
}

#[tauri::command]
fn pair(state: State<'_, Shared>, channel: String) -> Result<(), String> {
    commands::pair(&state, channel)
}

#[tauri::command]
fn stop_pairing(state: State<'_, Shared>) {
    commands::stop_pairing(&state)
}

#[tauri::command]
fn finish_pairing(state: State<'_, Shared>) {
    commands::finish_pairing(&state)
}

#[tauri::command]
fn self_test(state: State<'_, Shared>, channel: String) -> Result<(), String> {
    commands::self_test(&state, channel)
}

#[tauri::command]
fn stop_self_test(state: State<'_, Shared>) {
    commands::stop_self_test(&state)
}

#[tauri::command]
fn eyepiece(state: State<'_, Shared>, on: bool) {
    commands::eyepiece(&state, on)
}

#[tauri::command]
fn confirm_pair(state: State<'_, Shared>, peer_hex: String, name: String) -> Result<i64, String> {
    commands::confirm_pair(&state, peer_hex, name)
}

#[tauri::command]
fn set_setting(state: State<'_, Shared>, id: String, value_index: usize) -> Result<(), String> {
    commands::set_setting(&state, id, value_index)
}

#[tauri::command]
fn settings(state: State<'_, Shared>) -> std::collections::BTreeMap<&'static str, usize> {
    commands::settings(&state)
}

#[tauri::command]
fn setting_notes(
    state: State<'_, Shared>,
) -> std::collections::BTreeMap<&'static str, commands::SettingNote> {
    commands::setting_notes(&state)
}

/// Async, so the key derivation (Argon2id, most of a second) runs off the
/// window's thread as well as off the machine's lock.
#[tauri::command]
async fn unlock(
    state: State<'_, Shared>,
    passphrase: String,
    name: Option<String>,
) -> Result<(), String> {
    commands::unlock(&state, passphrase, name)
}

#[tauri::command]
fn contacts(state: State<'_, Shared>) -> Result<Vec<commands::ContactView>, String> {
    commands::contacts(&state)
}

#[tauri::command]
fn conversation(
    state: State<'_, Shared>,
    contact_id: i64,
) -> Result<Vec<serde_json::Value>, String> {
    commands::conversation(&state, contact_id)
}

#[tauri::command]
fn broadcasts(state: State<'_, Shared>) -> Result<Vec<serde_json::Value>, String> {
    commands::broadcasts(&state)
}

#[tauri::command]
fn machine(state: State<'_, Shared>) -> Result<commands::MachineView, String> {
    commands::machine(&state)
}

#[tauri::command]
fn channels(state: State<'_, Shared>) -> Vec<commands::ChannelView> {
    commands::channels(&state)
}

#[tauri::command]
fn requests(state: State<'_, Shared>) -> Result<Vec<commands::RequestView>, String> {
    commands::requests(&state)
}

#[tauri::command]
fn accept_request(state: State<'_, Shared>, id: u64, name: String) -> Result<i64, String> {
    commands::accept_request(&state, id, name)
}

#[tauri::command]
fn decline_request(state: State<'_, Shared>, id: u64) -> Result<(), String> {
    commands::decline_request(&state, id)
}

#[tauri::command]
fn send_request(state: State<'_, Shared>, label: String) -> Result<(), String> {
    commands::send_request(&state, label)
}

#[tauri::command]
fn strangers(state: State<'_, Shared>) -> Result<Vec<String>, String> {
    commands::strangers(&state)
}

#[tauri::command]
fn peers(state: State<'_, Shared>) -> Result<commands::PeersView, String> {
    commands::peers(&state)
}

#[tauri::command]
fn unconfirmed(state: State<'_, Shared>) -> Result<Vec<i64>, String> {
    commands::unconfirmed(&state)
}

/// Tick the node every [`TICK`] and emit what it reports.
fn pump(app: AppHandle) {
    let start = Instant::now();
    loop {
        thread::sleep(TICK);
        let now_ms = start.elapsed().as_millis() as u64;
        let events = commands::tick(&app.state::<Shared>(), now_ms);
        for e in &events {
            let (name, payload) = bridge_event(e);
            // A page between loads misses the event; `channels` keeps the
            // state that matters for when it is back.
            let _ = app.emit(name, payload);
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = dir.join(STORE_FILE);
            let device = Device::with_hardware(store.clone()).with_keeper(keychain::system(&store));
            app.manage(Shared::new(Box::new(device)));
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
            stop_pairing,
            finish_pairing,
            self_test,
            stop_self_test,
            confirm_pair,
            set_setting,
            settings,
            setting_notes,
            unlock,
            contacts,
            conversation,
            broadcasts,
            machine,
            channels,
            eyepiece,
            requests,
            accept_request,
            decline_request,
            send_request,
            strangers,
            peers,
            unconfirmed
        ])
        .run(tauri::generate_context!())
        .expect("Bombechat could not start");
}
