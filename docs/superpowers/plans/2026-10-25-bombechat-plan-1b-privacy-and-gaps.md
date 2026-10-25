# Bombechat Plan 1b: the privacy fix and the known gaps

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close what Plan 1's final review left open, before merging to main. The device's saved settings always win. The microphone really closes at Off. Every setting that has a working feature behind it changes behaviour. Broadcasts can be written in the app. The first-run card looks like part of the machine.

**Architecture:** Same workspace and app as Plan 1. Settings live in the device (`settings.json` in the app data dir, already used for Sound and Send under), and the pages read them from the device instead of pushing their own copies.

**Tech Stack:** As Plan 1. Add the `keyring` crate (macOS Keychain through Security.framework, no networking) only if Task 3 needs it.

**Spec:** `docs/superpowers/specs/2026-10-24-bombechat-design.md`

## Global Constraints

Everything in Plan 1's Global Constraints still applies. In addition:
- The device's saved settings are the only source of truth. A page never sends a setting it did not just get from the person's own action.
- A setting whose feature doesn't exist yet (relay, light) must say so honestly on the settings page, for example "comes with relay". It must not pretend to be saved and working.
- New screens follow the standing rule: build them, check them with 2x crops at 1024, 1440 and 1728 wide, iterate until they read as part of the same machine, with no AI look.

## Review Focus

1. Settings files written by an older build, or corrupted: a missing or unknown key falls back to its default and never stops the app. Owner: Task 1.
2. Switching Sound Off → Listen → Off quickly, or while a frame is playing: no panic, the microphone state ends correct, and the outbox status stays true. Owner: Task 2.
3. A Tone band change while a message is arriving: the arrival is abandoned cleanly as "did not check", and the new band works. Owner: Task 3.
4. "Delete messages after" with a clock that jumps backwards: never deletes messages newer than the cut-off. Owner: Task 3.
5. An empty or oversized broadcast: refused with a clear sentence-case reason. Owner: Task 4.

---

### Task 1: The device's settings win

**Files:**
- Modify: `app/src-tauri/src/{commands.rs,device.rs,lib.rs}`
- Modify: `app/ui/{bridge.js,simulator.js,bombechat.html,settings.html}`
- Modify: `tools/check.cjs`, `tools/check-settings.cjs`
- Test: `app/src-tauri/tests/{commands.rs,device.rs}`

**Interfaces:**
- Produces:
  - Command `settings() -> { [id]: valueIndex }`, with every setting id from settings.html.
  - `set_setting` keeps validating, and now persists every id.
  - `bridge.settings()`.

- [ ] Device and command tests:
  - `settings()` returns the persisted values.
  - Unknown keys and corrupt files fall back to defaults.
  - A page boot never calls `set_setting`. To test this, wrap the bridge in check.cjs and count calls at boot: it must be 0.
- [ ] Both pages read `bridge.settings()` at boot and render from it. localStorage keeps only per-viewer conveniences, and never settings.
- [ ] Test: a cleared localStorage with a saved Listen still boots at Listen, in the app path and in the simulator.
- [ ] Run `cargo test --workspace`, `npm run check`, and look at the 2x crops. Commit: `fix: the device's saved settings win at boot`.

### Task 2: The microphone closes at Off

**Files:**
- Modify: `crates/modem/src/{audio.rs,fake.rs}`, `crates/node/src/sound.rs`, `app/src-tauri/src/device.rs`
- Test: alongside each file.

**Interfaces:**
- Produces:
  - `AudioIo::stop_input(&mut self)`, with a default no-op for doubles that don't need it.
  - `CpalAudio` drops its input stream on stop and rebuilds it on `start_input`.
  - `FakeAudio` records start and stop counts.

- [ ] Tests:
  - Off → `stop_input` is called and the stream is gone.
  - Listen → `start_input` is called again.
  - Rapid toggles end in the right state.
  - A frame already playing is allowed to finish, and the outbox stays truthful.
  - Off reports the channel as `ChannelState{Sound,"off"}`, not "listening".
  - The stale `refusedWhy` label is cleared on every outbox event.
- [ ] Check the macOS mic indicator by hand in a built app at Off. Note the result in the report.
- [ ] Commit: `fix: the microphone closes at Off`.

### Task 3: Every setting with a feature behind it works

Wire these through the device to the node, modem and page, each with a test:

| Setting | What it does |
|---|---|
| Send volume 1-5 | gain on played samples (0.2 to 1.0) |
| Tone band | rebuilds the modem config. An arrival in progress fails cleanly as "did not check". |
| Repeats 0-3 | the QuietGate repeat count |
| Machine sound on/off and machine volume 1-5 | the page's drum and plug sounds |
| Lock with passphrase | Off keeps the store key in the macOS Keychain, so launch opens without asking. On removes it. |
| Delete messages after | the store purges older rows at unlock and hourly, by a monotonic sent time, never newer than the cut-off |
| New contacts need a key check | always on in the airgapped build, where every contact is checked in person. The row shows "always, on this build" and can't be turned off. |

Relay settings (Carry for others, Hop limit, Hold for) and Light settings (Read, Flash speed) have no feature yet. Their rows say "comes with relay" or "comes with light" in pencil, their jacks are disabled, and they are not persisted as working. Plan 2 wires the Light rows.

- [ ] Tests for each wired row. check-settings.cjs asserts the disabled rows can't be patched and say what they wait for.
- [ ] 2x crops of the settings card and the back panel at three widths. Iterate.
- [ ] Commit: `feat: settings that have a feature behind them now work`.

### Task 4: Writing a broadcast

**Files:**
- Modify: `app/ui/bombechat.html` (plus `bridge.js`/`simulator.js` if needed), `tools/check.cjs`

- [ ] Design and mock the broadcast form in the machine's own language. My suggestion is a "Public notice" form or slip on the message sheet, or the sheet's reply line switched to "public" by a small engraved toggle. Show to anyone in range, signed with your key, and explain plainly that anyone can read it.
- [ ] Your own broadcasts show on the sheet as signed public entries. Empty broadcasts are refused with a sentence-case reason, and so are oversized ones (over the envelope limit).
- [ ] Check it in the browser and app paths at 1024, 1440 and 1728 wide, and iterate until it's indistinguishable in quality from the rest. check.cjs exercises it.
- [ ] Commit: `feat: write a public broadcast`.

### Task 5: The first-run card, designed

**Files:**
- Modify: `app/ui/bombechat.html`, `tools/check.cjs`

- [ ] Redesign the name and passphrase card (first run, and unlock on later runs) so it belongs to the machine. It must lose the generic look: dark modal, cream inputs, a lede paragraph and a pill button. Suggested direction: a buff registration form like the message sheet, or a brass-and-bakelite plate with typed fields and an engraved "Lock and open" key. Keep:
  - the required name
  - the passphrase twice on first run
  - the wrong-passphrase error
  - the existing tests
- [ ] Iterate with 2x crops at three widths, in both the first-run and unlock states.
- [ ] Commit: `feat: a first-run card that belongs to the machine`.
