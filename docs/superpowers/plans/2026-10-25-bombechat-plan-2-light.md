# Bombechat Plan 2: the light path

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two airgapped laptops exchange sealed messages, receipts, broadcasts and pairing hellos by light. One laptop's screen shows an animated sequence of codes, and the other laptop's camera reads them. This runs through the same node, store and Bombe interface as sound.

**Architecture:**
- **New crate `bombechat-light`**, the light equivalent of the modem:
  - a visual code format: cell grid, finder marks, frame header
  - fountain coding over envelope bytes, so a receiver can join mid-sequence and miss frames
  - a renderer that turns frames into grayscale bitmaps
  - a detector and decoder that turns camera bitmaps back into frames
- **`bombechat-node`** gains a `LightTransport` behind the existing `Transport` trait.
- **Camera capture** sits behind a `CameraIo` trait, the light counterpart of `AudioIo`. The real backend is AVFoundation, through the `nokhwa` crate. If nokhwa pulls in anything networked, use a minimal objc2 AVFoundation binding instead. Tests use a fake.
- **Display:** the node emits frames, and the page draws them on a canvas in a flashing view. The page never sees key material, only the public code bitmaps.

**Tech Stack:**
- `raptorq` for the fountain code (RFC 6330)
- `image` for test images
- `imageproc` (or hand-written code) for thresholding and perspective
- `nokhwa`, with only the `input-avfoundation` feature
- everything from Plan 1

**Spec:** `docs/superpowers/specs/2026-10-24-bombechat-design.md`. The light channel lines are 101 and 94, the Listen/Send semantics line 129, and "left to the builder" line 142 covers the code format and frame rate.

## Global Constraints

- Every Plan 1 and 1b constraint still applies:
  - airgapped: no Bluetooth, no networking, CSP `connect-src 'none'`
  - nothing plaintext on disk
  - no key material in the webview
  - the device's settings are the only source of truth
  - interface text: sentence case, no em dashes, no emojis, nothing under 12px, no AI look
  - visual work iterated with 2x crops at 1024, 1440 and 1728 until it reads as the same machine
- **Light switch** positions are Off, Read, and Read and flash. Flash implies Read.
  - At Off the camera is closed and the macOS camera light is off.
  - At Read nothing is ever shown.
  - Refusals are sentence case, the same as for sound.
- **Flash speed setting:** Slow, Normal, Fast map to code frame rates (suggested 6, 12 and 20 frames per second) and cell density. The interface shows the measured rate.
- **Photosensitivity:** the flashing view must never flash full-field black and white. Codes are drawn as a fixed-position grid with a stable mid-grey surround, so per-frame luminance change across the view stays small, under WCAG 2.3.1's general flash threshold. A check asserts it. The view also shows a still "hold your screen up to their camera" state until sending starts, and Escape always stops flashing.
- **Taped settings:** the light rows taped in Plan 1b ("comes with light") become live rows. The relay rows stay taped.
- **Transport:** light uses the same sealed envelopes and the `Transport` trait. Delivery state, receipts, dedup and retention behave exactly as they do for sound.

## Review Focus

1. **A receiver that joins mid-sequence or misses frames** must still decode once it has enough symbols (that's the fountain property), and never panics on partial or garbage frames. Owner: Tasks 1 and 2.
2. **Real camera conditions:** rotation up to ±20°, perspective up to 30°, blur, screen moiré, uneven lighting, low light, a hand-held tremble. The decoder must succeed on synthetic versions of each and fail cleanly on unreadable ones. Owner: Task 2.
3. **Two codes in view, or a code from a screen behind** the intended sender: frames are tagged, and each message reassembles separately. Owner: Tasks 1 and 4.
4. **Camera permission denied or no camera:** the Light channel shows unavailable with the reason, and nothing crashes. Owner: Task 3.
5. **Turning Light Off mid-flash or mid-read:** the camera closes and flashing stops at once, and the outbox status stays truthful. Owner: Tasks 4 and 5.

---

### Task 0: the leftover from Plan 1b

**Files:** `crates/store/src/lib.rs`

- [ ] **VACUUM failure during the v3 to v4 migration:** if VACUUM fails (for example on a full disk), don't report "store is corrupt". Open the store at v3 and retry the migration on the next open, or map the failure to Io with a sentence-case reason. Test with a forced VACUUM failure.
- [ ] **Broadcast cap:** the cap now gives back its pages.
- [ ] Commit: `fix: a store that can't reclaim space still opens`.

### Task 1: the light code format and fountain framing

**Files:**
- Create: `crates/light/Cargo.toml`, `crates/light/src/{lib.rs,code.rs,fountain.rs,render.rs}`, `crates/light/tests/code.rs`
- Modify: `Cargo.toml` (workspace member), `tools/check-airgapped.sh` (unchanged patterns, but it must still pass)

**Interfaces:**
- Produces:
  - `pub struct CodeSpec { pub cells: u16, pub cell_bits: u8, pub symbol_size: u16 }` with presets `CodeSpec::slow()`, `CodeSpec::normal()` and `CodeSpec::fast()`
  - `pub fn encode(spec: &CodeSpec, envelope: &[u8], tag: u16) -> Encoder`, where `Encoder::next_frame(&mut self) -> Vec<u8>` returns the frame payload bits
  - `pub fn render(spec: &CodeSpec, frame: &[u8], px: u32) -> GrayImage`, which draws:
    - finder marks in three corners plus an orientation mark
    - a quiet zone
    - a frame header: tag, the fountain symbol id and the transfer length, protected by a CRC
    - the payload cells
  - `pub struct Collector` with `push(&mut self, frame_bytes: &[u8], now_ms: u64) -> Vec<LightEvent>` and `expire(now_ms)`
  - `pub enum LightEvent { FrameOk{tag, got, need}, Complete{tag, bytes} }`
- The fountain code is RaptorQ. A receiver needs about K+2 distinct symbols, in any order.

- [ ] Tests:
  - round trip: encode, render, read the cells back from the clean image, collect, and get the exact envelope
  - starting at frame 37 and dropping 30% of frames at random still completes
  - two tags interleaved complete separately
  - frames with a bad CRC are rejected
  - property test: `push` never panics on arbitrary bytes
  - a 4096-byte envelope completes within a frame budget for each preset
- [ ] Commit: `feat(light): code format, fountain framing, renderer`.

### Task 2: the detector and decoder

**Files:** `crates/light/src/{detect.rs,decode.rs}`, `crates/light/tests/decode.rs`, `crates/light/tests/fixtures/` (synthetic images generated by the tests and checked in)

**Interfaces:**
- Produces: `pub fn read(spec: &CodeSpec, frame: &GrayImage) -> Option<Vec<u8>>`. It:
  1. finds the finder marks, choosing among several candidates by the header CRC
  2. solves the perspective
  3. samples the cell centres with an adaptive threshold
  4. checks the header CRC
- It returns `None` when there is no code, or when the code is unreadable.

- [ ] Tests on synthetic camera images made from `render` output:
  - rotations ±20°
  - perspective up to 30°
  - Gaussian blur at sigma 1.5
  - additive noise
  - brightness gradient
  - simulated moiré (a fine sinusoid overlay)
  - scale from 25% to 100% of the frame
  - the code off-centre
  - two codes in view, both read across successive calls
  - pure noise and a blank frame return `None`
  - success rate over 200 seeded samples per condition, at least 98% on the "normal" preset
  - decoding time under 15 ms per 1280x720 frame in release, measured and reported
- [ ] Commit: `feat(light): detector and decoder`.

### Task 3: the camera

**Files:** `crates/light/src/camera.rs`, `crates/light/src/fake.rs` (feature `fake-camera`), tests.

**Interfaces:**
- `pub trait CameraIo: Send` with:
  - `fn start(&mut self, sink: Box<dyn FnMut(GrayImage) + Send>) -> Result<(), CameraError>`
  - `fn stop(&mut self)`
  - `fn take_error(&mut self) -> Option<CameraError>`
- `CameraError { NoDevice, PermissionDenied, Unsupported(String) }`
- `AvCamera` runs on the default camera at 1280x720, frames converted to grayscale, at 30 fps or the device default.
- `FakeCamera` plays a scripted image sequence and records start and stop counts.

- [ ] Tests:
  - the fake's start and stop counts
  - a denied camera gives PermissionDenied, not a panic
  - an `#[ignore]` hardware test opens the camera, grabs 10 frames and stops. Run it by hand.
- [ ] `bash tools/check-airgapped.sh` stays clean with the camera dependency added. Add AVFoundation camera usage to the Tauri Info.plist: "Bombechat reads messages shown to it on another screen."
- [ ] Commit: `feat(light): camera input`.

### Task 4: the light transport in the node

**Files:** `crates/node/src/light.rs`, `crates/node/src/lib.rs`, `crates/node/tests/two_nodes_light.rs`

**Interfaces:**
- `LightTransport::new(cam: Box<dyn CameraIo>, spec: CodeSpec)` implements `Transport`:
  - **Sending** queues envelopes. Each tick it produces the next code frame for the current envelope and emits `TransportEvent::Display(Vec<u8>)`, the rendered frame bits, which become a `NodeEvent::Display { bits, spec }`. It keeps cycling fountain frames until a sending window ends: a default of 3x the frames needed, or until a receipt arrives, whichever comes first.
  - **Receiving** runs camera frames through `read`, then the Collector, then envelopes, through the same node paths as sound.
- `Node::set_light(LightSwitch { off, read, read_and_flash })` mirrors `set_sound`: Off closes the camera, Read never displays, and Read and flash does both.
- `send(to, text, Channel::Light)` works, and `Channel::Light` is no longer "not in this plan".
- Pairing hellos and receipts can use light when Light is at Read and flash.

- [ ] Tests:
  - two nodes over a fake "screen to camera" link that renders Display frames, distorts them and feeds the other camera. Pair, message and receipt complete by light alone.
  - joining mid-sequence
  - Off mid-read closes the camera
  - Off mid-flash stops display, and the outbox reads failed or waiting truthfully
  - two senders in view
- [ ] Commit: `feat(node): messages by light`.

### Task 5: the app and settings wiring

**Files:**
- `app/src-tauri/src/{device.rs,commands.rs,lib.rs}`
- `app/ui/{bridge.js,simulator.js,settings.html}`
- tests and `tools/check*.cjs`

- [ ] The device creates the `LightTransport` with `AvCamera`, the same way it does sound, with the same fallback and unavailable states.
- [ ] Wire the `light` and `flash` settings: untape them on both pages and in the 3D panel, and have the device accept them.
- [ ] Re-emit `NodeEvent::Display` as a bridge event `display {bits, spec, tag}`.
- [ ] Expose the measured light rate through `channels`.
- [ ] Pass camera permission and unavailability through as ChannelState Light.
- [ ] Add the drift tests for the new command and event names.
- [ ] Commit: `feat(app): light channel wired`.

### Task 6: the flashing view and the Light controls

**Files:** `app/ui/bombechat.html` (plus a canvas module if it grows), `tools/check.cjs`

- [ ] **Design the flashing view** in the machine's language. Suggested direction: when the machine flashes, the message sheet area becomes a framed "signal lamp" panel. The code grid sits behind a brass bezel on a mid-grey enamel, and the caption is typed: "Hold this screen up to their camera."
  - The view shows progress: frames shown, and the measured rate.
  - Escape stops it.
  - It comes back to the sheet when the sending window ends.
- [ ] **Photosensitivity check:** check.cjs measures the per-frame mean luminance change over the view across 120 frames and asserts it stays within the flash threshold.
- [ ] **Light controls:**
  - Untape the front Light switch.
  - The Light tile shows a small camera "eyepiece" preview while reading. Keep it low-res and grayscale, and make it optional: privacy-minded people may hide it.
  - Arriving light messages land on the drums and the sheet exactly as sound ones do, with margin note "light".
  - The Flash key works.
  - Refusals: Light Off, Read only, too long.
- [ ] Crops at 1024, 1440 and 1728 of idle, flashing, reading and refusal. Iterate.
- [ ] Commit: `feat(ui): flashing by light`.

### Task 7: pairing by light

**Files:** `app/ui/bombechat.html` (key-check card), node pairing paths, `tools/check.cjs`

- [ ] **"Pair by light" stamp:** shows your hello as a light code, which carries the full public key and name. While it shows, the card reads the other person's code.
- [ ] **Offer flow:** identical to sound (the 60-digit grid, an editable name, "Matched, add" or "Doesn't match").
- [ ] **Static tape code:** the punched-tape strip on the card can also show your key as a static code a phone camera could read later (stretch goal). Skip it if it adds risk.
- [ ] **Tests:** check.cjs runs both pairing paths, and a node test pairs by light alone.
- [ ] **Commit:** `feat: pair by light`.

### Task 8: measuring and the two-Mac walkthrough

**Files:** `crates/light/examples/measure.rs`, `docs/testing/two-laptops.md`

- [ ] **The `measure` example:**
  - shows a code sequence in a window, or replays it from a recorded image folder
  - reads with the camera
  - reports frame success, symbols per second and bytes per second for each preset
  - for a single-Mac test: point the camera at a printed or second-screen code, or decode a folder of photos with `--images`
- [ ] **Walkthrough additions:**
  - pair by light
  - send and receive by light at 30 cm, 60 cm and 1 m, in daylight and in a dim room
  - flashing stops with Escape
  - check the macOS camera light at Off
  - blank result lines for the user
- [ ] **Final checks:** `cargo test --workspace`, clippy, `npm run check`, `cargo tauri build`, the airgapped check. Commit and push.
