# Bombechat Plan 3: the Bluetooth edition

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a second buildable project, the Bluetooth (standard) edition, that is a full copy of the airgapped project plus Bluetooth: discovery, requests to connect, forward-secret sessions and message exchange over BLE, with the airgapped project untouched and provably Bluetooth-free.

**Architecture:** The repository root stays the airgapped project. A new top-level folder `bluetooth/` holds a complete, independently buildable copy (its own Cargo workspace, Tauri app, UI, tools and package.json). The copy is generated: `tools/sync-bluetooth.sh` copies the shared tree from the root into `bluetooth/`, applies the edition's patches, and lays the edition's own files over it. CI fails if the committed `bluetooth/` differs from what the script produces, so a fix made at the root always reaches both editions and the editions can only differ where the edition says so. Bluetooth itself is a transport behind the existing `Transport` trait, so the envelope, sealing, sound, light and storage are unchanged and a later Bitchat bridge stays possible.

**Tech stack:** Rust, objc2 + objc2-core-bluetooth (CoreBluetooth, both central and peripheral roles, as the camera already uses objc2 AVFoundation), `snow` for Noise, Tauri 2, the existing web UI.

**Spec:** `docs/superpowers/specs/2026-10-24-bombechat-design.md` (Channels: Bluetooth; Keys and contacts: requests from strangers; Cryptography: Noise sessions; Bitchat; airgapped means no Bluetooth and no networking).

**Commissioner's rulings for this plan (2026-10-25):**
- The Bluetooth build lives in a duplicate folder that holds a copy of everything built so far and is a second type of project that can be built, so the original stays airgapped.
- Requests to connect work LinkedIn style: nothing is exchanged until the person accepts.
- Bitchat interop is not in this plan, but nothing here may make it harder.

## Global Constraints

- The root project must not change in any way that adds Bluetooth or networking code. `tools/check-airgapped.sh` at the root stays clean; the only root changes allowed are `Cargo.toml` `exclude = ["bluetooth"]`, the sync script, CI, docs and README.
- Shared code is identical in both editions except files listed in `bluetooth/edition/PATCHED` (each with a patch in `bluetooth/edition/patches/`) and files added from `bluetooth/edition/files/`. Patches stay small; prefer adding a file over patching one.
- Identifier `app.bombechat.standard`, product name `Bombechat`, the machine's top plate reads "standard build" where the airgapped one reads "airgapped build". Its store lives in `~/Library/Application Support/app.bombechat.standard/`, never shared with the airgapped app.
- Same proven primitives only: X25519, Ed25519, ChaCha20-Poly1305, SHA-256/BLAKE2 as `snow` provides, HKDF. Noise pattern `Noise_XX_25519_ChaChaPoly_SHA256` (the pattern Bitchat uses for live sessions). No home-made constructions.
- Our own BLE service and characteristic UUIDs (generated once, recorded in the plan's Task 2 and the spec), never Bitchat's, so the two never confuse each other's devices. Frame and fragment sizes are not fixed in a way that clashes with Bitchat's (~469-byte fragments, 256/512/1024/2048 padding buckets).
- Advertising never carries the identity key, the name or anything stable: a random token rotated at least every 15 minutes.
- Nothing is sent to a stranger until the person accepts their request. Declining or ignoring keeps nothing.
- At Bluetooth Off the radio is idle: not advertising, not scanning, no connections, no CoreBluetooth managers alive. The app asks for Bluetooth permission only when the switch is first turned on.
- UI uniformity is never sacrificed; nothing AI-looking (no glows, italic captions, tracked caps eyebrows, card kits, generic modals). Sentence case. No em dashes, no emojis.
- Every change is built, tested (screenshots at full size plus 2x crops at 1024/1440/1728, function checks of every control), iterated to a professional design-engineer bar. No flaky tests: no wall-clock budgets in tests.
- Push to github.com/AllStreets/bombechat after each task once iterated.

## Review Focus

1. Bluetooth Off really stops the radio: a person who turns it off expects the Mac's Bluetooth menu to show the app not using it. Test: the CoreBluetooth radio's managers are dropped at Off (unit test on the radio adapter's state, hardware test ignored for the human).
2. A flood of requests from strangers (one device spamming, or many devices) must not fill the store, the screen or memory. Test: inbox cap and per-device rate limit in node tests.
3. A peer that disconnects mid-message, or sends fragments out of order, duplicated or forever: reassembly is bounded in count, size and time. Test: fragment fuzz-style tests.
4. The same contact in range by Bluetooth and by sound at once: one message, one receipt, no duplicates. Test: node test with both transports.
5. Editions drifting: a fix at the root that the sync script forgets. Test: CI sync check, and a test that the shared file list covers every root file except an explicit exclusion list.

---

### Task 0: the edition scaffold

**Files:** `tools/sync-bluetooth.sh`, `bluetooth/edition/{PATCHED,patches/,files/,README.md}`, generated `bluetooth/**`, root `Cargo.toml` (exclude), `.github/workflows/ci.yml`, `README.md` (a short "Two editions" section).

- [ ] `tools/sync-bluetooth.sh [--check]`: copies the root's shared tree (every tracked file except `bluetooth/`, `.github/`, `docs/`, `mockups/` and the root README) into `bluetooth/`, deletes files there that the root no longer has (never touching `bluetooth/edition/`), applies `bluetooth/edition/patches/*.patch` in order, then copies `bluetooth/edition/files/` over. `--check` does the same into a temporary directory and fails with a diff if `bluetooth/` differs. Shell only, macOS and Linux.
- [ ] The edition's first patches: identifier `app.bombechat.standard`; the top plate's "airgapped build" becomes "standard build" (find where the page gets it; prefer the machine telling the page over patching HTML); `NSBluetoothAlwaysUsageDescription` "Bombechat uses Bluetooth to reach people near you. Nothing is sent to a stranger until you accept them." in the bundle's Info.plist.
- [ ] Root `Cargo.toml`: `exclude = ["bluetooth"]`. `bluetooth/Cargo.toml` is its own workspace.
- [ ] CI: a `sync` job runs `tools/sync-bluetooth.sh --check`; the macOS job also runs `cd bluetooth && cargo test --workspace`; the root airgapped checks are unchanged.
- [ ] Test: a script test that a file added at the root appears in `bluetooth/` after sync and a file listed in neither place fails `--check`.
- [ ] The edition builds and its UI checks pass (`cd bluetooth && npm run check`), identical to the root apart from the build label. Commit `feat: the Bluetooth edition, a synced copy`.

### Task 1: Noise sessions (`bluetooth/edition/files/crates/session`)

**Interfaces produced:** `Session::initiator(&Identity, Option<&PublicIdentity>) -> Handshake`, `Session::responder(&Identity) -> Handshake`, `Handshake::{write_message, read_message, is_finished, into_transport}`, `Transport::{encrypt, decrypt}`, `Handshake::remote_identity() -> Option<PublicIdentity>`. Errors as an enum, never panics.

- [ ] `Noise_XX_25519_ChaChaPoly_SHA256` via `snow`, static key = the identity's X25519 key. The handshake payloads carry the Ed25519 public key and a signature binding it to the Noise static key, so a session's remote is a full `PublicIdentity` and can be matched against contacts and the safety number.
- [ ] Tests: two sides complete a handshake and exchange messages; a tampered message fails; a replayed transport message fails (nonce); a responder that presents a key not matching its signature is rejected; known-answer test against snow's own vectors for the pattern; a parser fuzz-style loop over random bytes never panics.
- [ ] Commit `feat(bt): Noise sessions`.

### Task 2: the link protocol, platform-free (`crates/bluetooth`, in the edition)

**Interfaces produced:** `trait Radio { fn start(&mut self, mode: Mode); fn stop(&mut self); fn write(&mut self, peer: LinkId, bytes: &[u8]); fn poll(&mut self) -> Vec<RadioEvent>; fn max_write(&self, peer: LinkId) -> usize }`, `RadioEvent::{Up(LinkId), Down(LinkId), Bytes(LinkId, Vec<u8>), Unavailable(String)}`, `FakeRadio` (in-memory, several radios in one "room", with drop/reorder/duplicate knobs), `BleTransport<R: Radio>` implementing the node's `Transport` (name `Channel::Bluetooth`).

- [x] Our service UUID and characteristic UUID, generated once, recorded in code and the spec: service `8AAC3E33-A919-4002-96A8-14E1A873404F`, characteristic `CC26A52C-3F93-439B-83DC-2301C13CA327`.
- [ ] Framing: link messages `{kind, flags, len, body}`, fragmented to the link's `max_write`, reassembled with caps (at most 32 assemblies per link, 64 KiB each, 30 s) and dropped on overflow.
- [ ] Per link: a Noise XX handshake first; envelopes travel inside the session. A link that fails the handshake is closed.
- [ ] Peers: a link's remote identity is a contact, a stranger, or self (dropped). Envelopes go only to links whose remote is the recipient contact, or, for public broadcasts, to every session.
- [ ] Rotating advert token (random, 15 min). Nothing in the advert is stable.
- [ ] Tests with `FakeRadio`: two transports exchange an envelope; a third device in range that is a stranger gets nothing private; out-of-order, duplicated and endless fragments are bounded; Down mid-message drops the partial; Review Focus 3.
- [ ] Commit `feat(bt): the link protocol over a fake radio`.

### Task 3: the CoreBluetooth radio

**Interfaces produced:** `CoreBluetoothRadio: Radio` (macOS), behind `#[cfg(target_os = "macos")]`; on other platforms `Radio::start` reports `Unavailable("Bluetooth is not supported on this system yet")`.

- [ ] One `CBPeripheralManager` (advertise our service with the rotating token; a GATT service with one write-without-response + notify characteristic) and one `CBCentralManager` (scan for our service only, connect, discover, subscribe), both driven on a dispatch queue, events forwarded through a channel, as the camera does.
- [ ] `stop()` stops advertising and scanning, cancels every connection and drops both managers; Review Focus 1.
- [ ] Permission: asked on first `start`; denied or powered off becomes `Unavailable` with a sentence the page shows ("Bluetooth is off in System Settings.", "Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.").
- [ ] Hardware test, `#[ignore]`, for the human on two Macs: discover, connect, exchange 4 KB both ways, stop and confirm the managers are gone.
- [ ] Commit `feat(bt): the CoreBluetooth radio`.

### Task 4: requests to connect and peers in range (node, in the edition)

**Interfaces produced:** `NodeEvent::Request{id, name, fingerprint, safety}`, `NodeEvent::RequestGone{id}`, `NodeEvent::Peers{contacts_in_range, strangers_in_range}`, `Node::request(contact_name_hint)` (send your hello to a stranger in range: only from an explicit press), `Node::accept_request(id, name) -> ContactId`, `Node::decline_request(id)`.

- [ ] Bluetooth switch: `Off`, `Contacts` (sessions only with contacts; requests neither sent nor shown), `Open` (requests can arrive and be sent). Default Off.
- [ ] A request is a stranger's hello (the same hello pairing uses) sent over a session. Nothing of ours goes back until accept; accept sends our hello and adds them unchecked (spec); decline keeps nothing.
- [ ] The in-person setting: when "new contacts need a key check" is on, accept is refused in sentence case and the request says to meet and pair in person.
- [ ] Caps: at most 20 requests waiting, one per device per 10 minutes, oldest dropped; requests are kept in memory only.
- [ ] Review Focus 2 and 4 tests. Commit `feat(bt): requests to connect`.

### Task 5: app wiring (edition)

- [ ] Tauri commands `requests`, `accept_request`, `decline_request`, `send_request`, events `request`, `requestGone`, `peers`, the Bluetooth channel in `channels()` with its state strings and rate. Settings: `bluetooth` (Off/Contacts/Open) on the back panel as a row of jacks; the root's taped Bluetooth row becomes live in the edition.
- [ ] Simulator: a stranger in range who sends a request, a contact in range, so the page works in a plain browser.
- [ ] Device tests as for sound and light. Commit `feat(app): Bluetooth wired`.

### Task 6: the Bluetooth face of the machine (mockup first, then wired)

- [ ] Mockup first, in the machine's language, shown to the commissioner as crops: the Bluetooth tile on the front (the "Not fitted" blanking plate becomes a rotary switch Off / Contacts / Open with its jewel, like Sound and Light), a peers count, and a request inbox (requests arrive as telegram slips in a tray beside the sheet: name as given, short fingerprint, "Accept" and "Decline" stamps; a typed note when the key check setting forbids accepting).
- [ ] Wire it; Escape and keyboard as elsewhere; crops at 1024/1440/1728; iterate to the bar; uniformity checked side by side with the Sound and Light tiles and the pairing card.
- [ ] Commit `feat(ui): the Bluetooth switch and requests`.

### Task 7: walkthrough and docs

- [ ] `bluetooth/edition/files/docs/testing/bluetooth.md` (or a section in the walkthrough): building the edition, permission, the hardware test, two Macs: see each other, request, accept, message, Off stops the radio (Bluetooth menu), results table, what to send back.
- [ ] README "Two editions" section with how to build each and why the airgapped one exists. Commit, push.
