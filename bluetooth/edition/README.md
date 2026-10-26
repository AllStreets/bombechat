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
  the Bluetooth usage line in `Info.plist`, and the workspace's extra members
  `crates/session` and `crates/bluetooth` in `Cargo.toml` (with the curve
  and hash code optimised in dev builds, so the session tests stay quick).
- `files/`: files the edition adds or replaces. `app/ui/edition.css` names
  this build "standard build" on the machine's plates. `crates/session` is
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

Builds land in `bluetooth/target`, apart from the root's.
