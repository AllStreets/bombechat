# The Bluetooth edition

`bluetooth/` is the standard build of Bombechat: the airgapped project at the
root of this repository, plus Bluetooth. It is a whole project of its own,
with its own Cargo workspace, Tauri app and interface, built and tested from
inside `bluetooth/`.

Most of it is generated. Don't edit files in `bluetooth/` outside this
folder: make the change at the root, or here, then run

    bash tools/sync-bluetooth.sh

from the root. The script copies the root's shared tree (every tracked file
except `bluetooth/`, `.github/`, `docs/`, `mockups/`, the root README and
the sync script and its test),
applies `patches/*.patch` in order, and lays `files/` over the result. CI runs
it with `--check` and fails if the committed `bluetooth/` differs.

What lives here:

- `PATCHED`: the shared files this edition changes, each with a patch.
- `patches/`: those changes, kept small. The identifier
  (`app.bombechat.standard`, so the store and Keychain item are its own),
  the Bluetooth usage line in `Info.plist`, the workspace's extra members
  `crates/session` and `crates/bluetooth` in `Cargo.toml` (with the curve
  and hash code optimised in dev builds, so the session tests stay quick),
  the app's dependency on `crates/bluetooth`, and `tools/check-bluetooth.cjs`
  in `npm run check`.
- `files/`: files the edition adds or replaces. `app/ui/edition.css` names
  this build "standard build" on the machine's plates ("bluetooth fitted" on
  the maker's plate) and styles the front's Bluetooth face: three switches of
  one size, the tray of request and stranger slips, and the tape under the
  switch.
  `app/src-tauri/src/edition.rs` and `app/ui/edition.js` replace the root's
  empty ones: the app's Bluetooth (the transport over the CoreBluetooth
  radio, the Bluetooth switch setting, its channel's words, and New
  contacts need a key check as a real choice), the settings card's
  Bluetooth row and its note on the Mac's name, the simulator's Bluetooth (a
  contact and strangers in range, requests to connect, the locked and
  unavailable radio), and the front's Bluetooth face: the Bluetooth switch
  (fitted through the page's knob, as Sound and Light are), who is in range
  on the bank's foot, request slips with Accept and Decline, strangers to ask,
  the not confirmed mark on a station, and the number to compare on the key
  check card.
  `app/src-tauri/tests/device_bluetooth.rs` and `tools/check-bluetooth.cjs`
  test them. `crates/session` is
  the live session crate: Noise handshakes (IK for contacts, XN for
  requests to connect, XX kept for a future Bitchat bridge) that prove each
  identity that is sent, then transport encryption. `crates/bluetooth` is the
  link protocol over any radio: adverts, fragments, contact and request
  handshakes, ordered and resent transport messages, and the node's
  Bluetooth transport, with an in-memory radio for tests, and the real
  radio: CoreBluetooth on macOS (both roles; its logic is platform-free in
  `adapter.rs` and tested without hardware), and on other systems a radio
  that says Bluetooth isn't supported yet.

Prefer adding a file here over patching a shared one.

`bluetooth/Cargo.lock` is the edition's own: the sync never writes it and
`--check` ignores it, because the edition's dependencies will differ from
the root's. When a manifest changes (at the root or here), run cargo inside
`bluetooth/` to update the lock and commit it. CI builds the edition with
`--locked`, so a stale lock fails there.

Building and checking:

    cd bluetooth
    cargo test --workspace
    npm run check

The app:

    cd bluetooth/app/src-tauri
    cargo tauri build

Builds land in `bluetooth/target`, apart from the root's, so the app is
`bluetooth/target/release/bundle/macos/Bombechat.app`. It has the same name
as the airgapped app, so don't copy both into `/Applications`. Its store is
in `~/Library/Application Support/app.bombechat.standard/`.

Part 4 of the walkthrough at the root, `docs/testing/two-laptops.md`, tries
it on two Macs: the hardware tests in `crates/bluetooth/src/macos.rs` with
their six measurements, then the app step by step.

The Mac's name: anyone in Bluetooth range can read it, Bombechat or not.
People are told: "Anyone in Bluetooth range can read this Mac's name. Give
it a plain name that doesn't say who you are, in System Settings, General,
About." The Bluetooth group's note on the wiring schedule carries the same
sentence.
