# Bombechat Plan 4: the machine pass

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the front and the back read as one physical machine. The back carries exactly three rows of twelve drum connectors, signed Row 1, Row 2 and Row 3, one behind each drum of the front's three rows, and its first view shows the top of the panel. The front loses the two steel rails between its drum rows, and everything below the drums (message sheet, key check card, switch tiles, VU meter, outbox, stations, counter) gets a photoreal pass in the same materials and light as the drums and the back. The build the commissioner runs says which build it is, the self-test is found from the first moment, the README's pictures are current and include Bluetooth, and the repository has a holder, a description and topics.

**Architecture:** No new subsystems. The back stays `app/ui/settings.html` on `machine-kit.js`. The front's 3D drum bank grows into one front panel scene (`app/ui/front-panel.js`, split out of `bombechat.html`) that draws the enamel, the bank's bay and every piece of hardware on the lower deck, using the pattern the drum bank already proves: the HTML stays the model, the controls and the fallback, and the scene reads the HTML's boxes and draws the machine behind them. Paper stays HTML and gets a physically based CSS treatment lit from the same key light. Small hardware that sits on top of paper (clips, the drawing pin, plate screws) is baked once from the kit's own lights into sprites, so it is the same brass and steel as the 3D parts. The live VU meter gets its own small kit canvas so the needle never redraws the whole front.

**Tech stack:** three.js through `machine-kit.js` (MeshStandard and MeshPhysical materials, the RoomEnvironment PMREM, VSM shadows, GTAO), CSS for paper, Tauri 2, Playwright on GPU Chrome for checks and media, `gh` for the repository's metadata.

**Spec:** `docs/superpowers/specs/2026-10-24-bombechat-design.md` (Interface: front, settings, "the front and back share one set of materials and lights, so they read as one machine"; Process).

**Starts after:** Plan 3 (the Bluetooth edition) is merged to `main` from `~/Downloads/bombechat-bluetooth`. Nothing in this plan touches that checkout or branch.

**Commissioner's asks for this plan (2026-10-25), in short:**
1. Everything beneath the drums gets a photoreal beauty pass that matches the rest, impressive, and clearly the front of the machine whose back is the settings page.
2. Exactly three rows of cables on the back, signed Row 1, Row 2, Row 3 (today: Row 1 to Row 5, with Row 2 to Row 5 visible). No steel rods between the front's drum rows. Three rows on the front, three on the back, visibly the same drums.
3. The back's first view shows the top of the back in their fullscreen window (today it opens cut through the connector field).
4. They could not find the self-test: they run an old build (the front's plate reads Build 0.1.0, the back's Build 0.4). Rebuild, install, make builds tell apart, and make the self-test findable from the first moment without breaking the design.
5. New README media once all of that is done, including Bluetooth.
6. LICENSE: copyright 2026 Connor Evans, MIT.
7. A GitHub description and topics, in the voice of their other repositories.

**Designer's calls made in this plan (check them):**
- **The back's columns run 12 to 1.** Seen from behind, the front's drum 1 (front left) is at the back's right. The back's column numbers are engraved 12 on the left to 1 on the right, so a connector sits directly behind the drum it wires. Rows are not mirrored: Row 1 is the top row on both sides, and is the Plain row.
- **The back's row signs are traffolyte plates** like the front's Plain, Heard and Key plates (the kit's `plate()`), reading "Row 1", "Row 2", "Row 3" in the engraving face, not small painted text. On the front the three plates stay Plain, Heard and Key.
- **The steel shafts that run behind each row of drums stay**; they are the drums' axles. Only the two thinner rails between the rows go (3D `rails[3]`, `rails[4]`, and the flat fallback's `.bar`).
- **Hybrid, not all three.js.** Hardware moves into the 3D front panel; paper and anything a person types into, selects, or reads with a screen reader stays HTML. Reasons in Task 6.
- **Version 0.5.0**, shown with the short commit, on both pages, read from the machine.

## Global Constraints

- The airgapped build stays free of Bluetooth and networking code. `npm run check:airgapped` passes after every task.
- Every root change reaches `bluetooth/` through `bash tools/sync-bluetooth.sh`, and `bash tools/sync-bluetooth.sh --check` passes at the end of every task. Where a root change breaks an edition patch's context (`bluetooth/edition/patches/*.patch`, listed in `bluetooth/edition/PATCHED`), the patch is regenerated in the same task, never hand-edited into the generated tree.
- Every change is built, tested (full-size screenshots plus 2x crops at 1024, 1440 and 1728, and function checks of every control) and iterated to a professional design-engineer bar before it counts as done. This plan adds 1512 by 982 (a 14 inch MacBook Pro in fullscreen, the commissioner's window) to the viewports checked for framing and layout.
- UI uniformity is never sacrificed. A new or changed surface is compared side by side with the parts it sits beside and with the back, at 2x, before it is accepted.
- Nothing may look AI-made: no glows or bloom (a lit jewel is an emissive glass core, never a blurred halo), no italic captions, no tracked-caps eyebrows, no card kits, no generic modals. Sentence case for text; the engraving face's small capitals are the only capitals.
- No em dashes and no emojis anywhere: code, comments, commit messages, README, alt text, the GitHub description.
- Never record the commissioner's screen or app windows. Every README picture is rendered headless from the app's own pages by `tools/media.cjs`.
- Accessibility does not regress: every control stays a real HTML control with its label, keyboard path and focus ring; the 3D canvas is `aria-hidden`; 12 px minimum text; reduced motion respected; without WebGL the page falls back to today's CSS and still works.
- No flaky tests: no wall-clock budgets in checks. Performance is measured by counting renders, not by timing them.
- Push to github.com/AllStreets/bombechat after each task once iterated.

## Review Focus

1. **Front and back correspond.** Three rows of twelve on both sides, the back's connector for drum (row r, column c) directly behind it (mirrored column), signs Row 1 to Row 3, no rails between drum rows. Test: `__bc.connectors()` on the back returns 36 entries in 3 rows with columns 12..1 left to right and labels exactly `Row 1`, `Row 2`, `Row 3`; `__bank.rails()` on the front returns 0 between-row rails and 3 axles; the fallback grid has no `.bar`.
2. **The back's first view shows its top** at every window size, including the commissioner's 1512 by 982 fullscreen and the narrow single-column layout. Test: project the top bolt row, the column numbers and the Row 1 sign; all inside the view's top 25 percent and none clipped, at 1024x768, 1280x800, 1440x900, 1512x982 and 1728x1117, in both editions (the standard edition's back has a live Bluetooth row).
3. **The front panel costs nothing while idle.** One full-window canvas with AO must not redraw every frame. Test: with nothing changing, the front panel's render count stays the same across 120 animation frames; a knob turn renders a bounded number of frames and then stops; the VU needle moving redraws only the VU's canvas.
4. **Accessibility and the fallback survive the 3D pass.** Test: every check that drives controls today passes unchanged; a pass with WebGL disabled (`--disable-webgl`) renders the CSS deck with no page errors and every control working; focus rings visible on each 3D-backed control.
5. **The build the commissioner opens is the build just made.** Test: a check fails if `app/src-tauri/Cargo.toml`, `tauri.conf.json`, `package.json` and the simulator disagree on the version; both pages' plates read the same `Build <version> <commit>`; the installed bundle's `CFBundleShortVersionString` equals it.
6. **Edition drift.** Test: `tools/sync-bluetooth.sh --check` and `cd bluetooth && npm run check` pass after each task; `check:airgapped` at the root stays clean.

---

### Task 0: baseline after the Bluetooth merge

**Files:** none changed. Writes `checks/plan4-before/` (screenshots only).

- [ ] Confirm Plan 3 is merged to `main`: `bluetooth/` exists, `bash tools/sync-bluetooth.sh --check` passes, `cargo test --workspace` passes at the root and in `bluetooth/`, `npm run check` passes at the root and in `bluetooth/`, `npm run check:airgapped` is clean.
- [ ] Inventory what the Bluetooth edition added to the front and back (the live Bluetooth switch tile, the request slips tray, the peers count, the live Bluetooth row on the back) and how: overlay files under `bluetooth/edition/files/` or patches to shared files. Write the list at the top of this plan's Task 6 notes so the beauty pass covers the edition's surfaces too.
- [ ] Capture "before" images: full-page screenshots and 2x crops of the front's lower deck and the back's overview at 1024, 1440, 1512x982 and 1728, both editions. These are the side-by-side reference for every later task.

### Task 1: the back's three rows

**Files:** `app/ui/settings.html` (the upper field in `build()`), `tools/check-settings.cjs`.

**Interfaces produced:** `window.__bc.connectors()` returns `[{row, col, x, y, label}]` for the 36 drum connectors; `window.__bc.rowSigns()` returns the three plate texts.

- [ ] `CROWS=3`. Re-space the connector field so its row pitch to column pitch matches the front's drum bank at 1440 (front: row pitch about 1.08 drum widths, column pitch the grid's column width), so the back reads as the same drums seen from behind, not a squashed copy.
- [ ] Column numbers engraved 12 to 1 from left to right (Designer's call). Connector at column c sits at the mirrored x of front drum c.
- [ ] Row signs: `plate('ROW 1')` to `plate('ROW 3')` (the engraving face renders these as small capitals, matching the front's PLAIN, HEARD, KEY plates), left of each row, with two slotted screws each as on the front's plates. Remove the painted `engrave('ROW '+n)` labels.
- [ ] Shorten the panel: with two connector rows gone, bring the panel's top edge down so the space between the connector field and the maker's plate is what it was between Row 5 and the plate. Recompute `PH`, `CY`, the perimeter bolts (13 across, as now) and the trunks rising out of the top.
- [ ] Re-seed the decorative looms for three rows so the tangle still reads as dense and hand-wired (the seeded generator stays deterministic; neighbour links stay within the 36). Check nothing hangs off the panel's lower edge into the jack field.
- [ ] Tests in `check-settings.cjs`: Review Focus 1 (36 connectors, 3 rows, mirrored columns, the three sign texts); no connector or loom overlaps the maker's plate or the first group plate (projected boxes); every existing setting check still passes.
- [ ] Screenshots at full size and 2x crops of the connector field at 1024, 1440, 1728; side by side with the front's drum bank at the same width. Iterate until a viewer can count three rows on each and match them.
- [ ] Sync, `--check`, edition checks. Commit `feat(ui): the back carries the front's three rows of drums`. Push.

### Task 2: the front's rails come out

**Files:** `app/ui/bombechat.html` (the drum grid builder near `/* ---------- drums ---------- */` and the 3D bank's `/* ---- rails, plates, numbers ---- */` and `layout()`), `tools/check.cjs`.

**Interfaces produced:** `window.__bank.rails()` returns `{between: n, axles: n}`.

- [ ] 3D: create only the three axles (`rails[0..2]`); delete the two between-row rails and their placement in `layout()`.
- [ ] Flat fallback: stop inserting `<div class="bar">` between rows; remove the `.bar` rule. Keep the grid's row heights so the 3D drums still read their positions from unchanged cells, then decide whether the gap left by the bar should close: compare at 2x crops; the drum pitch must stay the same on both sides of this change unless the comparison shows the rows now float too far apart, in which case tighten `--dr`'s row allowance and re-run the bank checks.
- [ ] Tests in `check.cjs`: Review Focus 1 (no between-row rails, three axles, no `.bar`); the plain row still settles on the right letters; no layout shift; existing drum checks unchanged.
- [ ] 2x crops of the bank at 1024, 1440, 1728, 3D and fallback. Commit `feat(ui): three rows of drums, nothing between them`. Sync, `--check`. Push.

### Task 3: the back opens on its top

**Files:** `app/ui/settings.html` (`overview()`, `fitDist`, `resize`), `tools/check-settings.cjs`.

- [ ] `overview()` fits the whole shortened panel, top bolt row to cable trough, with a small margin, centred on the panel. With three rows the panel is short enough that this fits at every supported window without shrinking the jack engravings below today's size by more than about 10 percent (measure at 1512x982).
- [ ] Where a window is too wide and short for the whole panel (a check finds any), the framing anchors at the panel's top edge, never its centre: the top bolt row, the column numbers and Row 1 are always in view, and the rest is reached by focusing a row as now.
- [ ] The narrow layout (`.stage` single column, 56vh view) gets the same rule.
- [ ] `frameRow()` is unchanged, but recheck every row's framing and the tag's clearance (the Flash speed tag check) because the panel moved.
- [ ] Add 1512x982 to the settings check's viewports. Tests: Review Focus 2, in both editions.
- [ ] Screenshots of the overview at each size; compare with the commissioner's screenshot (user-URRR7N.png): the top of the back, its three rows, the maker's plate and the jack field all visible. Commit `fix(ui): the back opens on the whole panel, top first`. Sync, `--check`. Push.

**Can run in parallel:** Tasks 1 and 2 touch different files (`settings.html` and `check-settings.cjs` against `bombechat.html` and `check.cjs`) and can run at the same time. Task 3 shares `settings.html` with Task 1 and runs after it; it can run while Task 2 is still going.

### Task 4: builds that say which build they are

**Files:** `app/src-tauri/Cargo.toml`, `app/src-tauri/tauri.conf.json`, `package.json`, `app/src-tauri/build.rs`, `app/src-tauri/src/commands.rs` (`machine()`), `app/ui/simulator.js`, `app/ui/settings.html` (top plate), `app/ui/bombechat.html` (top plate, already reads `ME.version`), `tools/check.cjs`, `tools/check-settings.cjs`, `tools/media.cjs` (its stub machine), the edition's `0001-identifier.patch` (its context includes the version line).

**Interfaces produced:** `machine()` resolves `version` as `"0.5.0"` and a new `commit` (short hash, with a trailing `+` when built from a dirty tree, `unknown` without git).

- [ ] Version 0.5.0 in `Cargo.toml`, `tauri.conf.json` and `package.json`. `build.rs` reads `git rev-parse --short HEAD` and `git status --porcelain` at build time into `BOMBECHAT_COMMIT` (no network involved; airgapped check stays clean).
- [ ] Both pages' top plates read `Build 0.5.0 a1b2c3d` from `machine()`. The back stops hard-coding `Build 0.4` and asks the bridge, as the front does. The simulator returns the same version and `commit: 'browser'`. The plate is cut wide enough that the longest form never moves its neighbours (check at 1024).
- [ ] A check that fails when the four version sources disagree (Review Focus 5).
- [ ] Regenerate the edition's identifier patch for the new context. Sync, `--check`.
- [ ] Build both editions: `cd app/src-tauri && cargo tauri build`, and the same in `bluetooth/app/src-tauri`. Install: quit any running Bombechat, then `ditto` the airgapped bundle to `/Applications/Bombechat.app` and the standard bundle to `/Applications/Bombechat Standard.app` (same product name inside, different folder on disk, so both can be installed). Verify with `defaults read /Applications/Bombechat.app/Contents/Info.plist CFBundleShortVersionString` and the same for the standard app. Do not screenshot the commissioner's screen; tell them which app to open and what the plate will say.
- [ ] Tell the commissioner the old `target/release/bundle/macos/Bombechat.app` is the one they were running, that it is now replaced, and remind them to run the two-Mac tests on the new builds (sound, light, and Bluetooth on the standard app).
- [ ] Commit `feat(app): version 0.5.0, and the plates name the build and commit`. Push.

### Task 5: the self-test, found from the first moment

**Files:** `app/ui/bombechat.html` (the sheet's foot and the empty log), `tools/check.cjs`.

- [ ] Layout guarantee: at every checked viewport, including 1024x768 and 1512x982, the foot of the message sheet with the Test the machine stamp is inside the first screen with no scrolling. A check asserts the stamp's box is within the viewport on load, empty and with a full log.
- [ ] While the sheet has no messages (not only on the very first run), the empty log carries one typed line in the sheet's typewriter face, "On your own? Press Test the machine to hear it work.", with the drum's index mark (the small green triangle the passphrase form already uses) pointing at the stamp. It goes the moment anything lands on the sheet, test or message. No banner, no tooltip, no modal, no new colour.
- [ ] The stamp's tooltip and its refused states are unchanged; check them at 2x crops in rest, refused and running, at 1024, 1440, 1728.
- [ ] README: one sentence under "The machine" naming the stamp and where it is (the media lands in Task 9).
- [ ] Commit `feat(ui): an empty sheet says how to test the machine`. Sync, `--check`. Push.

**Can run in parallel:** Task 5 shares `bombechat.html` with Tasks 2 and 4 and runs after them. Task 4's Rust and config work can start while Task 3 runs, but its two page edits wait for Tasks 1 to 3.

### Task 6: the beauty pass, everything below the drums

**Decision: hybrid, hardware in three.js, paper in physically based CSS.**
The drum bank already shows the way: its SVG grid is the model and the fallback, sits at opacity 0, and the 3D scene reads the cells' boxes and draws real drums behind them. The lower deck follows the same rule. Hardware (enamel, escutcheons, knobs, jewels, bezels, glass, brass, bakelite, steel screws, the outbox tray) is what CSS gradients fake worst and what real light, shadow and environment reflection make convincing, and it is mostly static, so it costs nothing once drawn. Paper (the message sheet, the key check card, outbox slips, station cards, tape) carries text people type, select and read with screen readers, needs crisp type at any pixel ratio, and is what every existing function check drives; moving it into WebGL would break all of that for a gain paper does not need. Paper is matte and flat, so a careful CSS treatment lit from the 3D key light's direction is physically honest. Hardware that sits on top of paper cannot be drawn by a canvas that sits behind the HTML, so it is baked from the kit into sprites with the kit's own lights and environment, which keeps it the same metal as the 3D parts.

**Notes from Task 0:** the edition's front and back surfaces this task must also cover. Every one comes from the overlay `bluetooth/edition/files/app/ui/edition.js` and `edition.css` (laid over the root's empty copies); none comes from a patch. The five patches touch only the identifier, `Info.plist`, the Cargo manifests and `package.json`, so the beauty pass changes shared files without regenerating any patch, but must restyle these overlay surfaces in the overlay files themselves.

- Front, lower deck: the Bluetooth switch tile, fitted through the page's `knob()` as Sound and Light are, where the airgapped build has its blanking plate; its jewel lit only while the radio works; the top plate naming its state; a strip of tape under the switch for why it can't work (`.mod.bt .btnote`). `edition.css` widens the deck (`.app .deck` columns) and the panel to three switches of one size (`.panel .mods`).
- Front, the request slips tray above the outbox (`.col.tray`, `.intray`): a request to connect with Accept and Decline, the strangers in range to ask (folded behind a request), the key check's rule typed where Accept would be.
- Front, the peers count: who is in range, on the bank's foot.
- Front, the message sheet: Send by Bluetooth beside Flash and Send by sound, the three keys on a row of their own (`.sheet .compose`); the not confirmed mark on a station; the number to compare on the key check card (`showUnchecked`).
- Front and back plates: `--build` ('standard build') and `--radio` ('bluetooth fitted') on the top plate, the passphrase stand and the back's maker's plate.
- Back: the live Bluetooth row (`SETTING_ROWS`: Bluetooth switch, Off, Contacts, Open) with its group note on the Mac's name, in place of the airgapped build's red Not fitted cover; the left column of the jack field runs one row taller there.

#### Task 6.1: the kit grows the parts both sides share

**Files:** `app/ui/machine-kit.js`, `app/ui/settings.html` (adopts the shared Bluetooth blanking plate and perimeter).

**Interfaces produced (on the object `createMachine` returns):**
- `perimeter(w, h, cy, {across, down})`: the panel's frame and bolt ring, as the back draws it now (13 across, 10 down).
- `blank(w, h, text)`: a wrinkle-finish steel blanking plate with four slotted bolts and the text engraved, as the back's "Not fitted to this build" plate is now.
- `knob({stops, labels, radius})`: a bakelite rotary switch (lathe body, fluted skirt with `tex.knurl` bump, a cream enamel pointer inlaid, engraved stop labels around it on the enamel), returning `{group, set(i, animate)}`.
- `jewel(color)`: a glass cabochon in a turned brass bezel (MeshPhysicalMaterial, transmission about .9, ior 1.5, a little thickness, clearcoat), returning `{group, set(on)}`; on lights an emissive core and a small short-range point light that touches only the bezel lip. No bloom pass, no halo.
- `bake(object3D, {px, view})`: renders one object with the kit's lights and environment into a transparent canvas and returns a data URL, for hardware that sits on paper.
- [ ] Move the back's perimeter bolts and Bluetooth blanking plate onto these builders, so front and back draw them with the same code. Back screenshots must not change apart from noise; compare at 2x.
- [ ] Unit-style browser check: each builder renders without errors, `bake()` returns a non-empty image whose alpha is 0 at its corners.
- [ ] Commit `feat(kit): the parts the front and back share`. Sync, `--check`.

#### Task 6.2: one front panel

**Files:** new `app/ui/front-panel.js` (the 3D bank code moved out of `bombechat.html`, then grown), new `app/ui/front-panel.css` (the `.gl` rules that hide the CSS hardware the scene replaces), `app/ui/bombechat.html` (loads both; the bank section shrinks to a call).

**Interfaces produced:** `mountFrontPanel({app, bank, deck, cells, still}) -> {ready, layout(), knob(id, pos), jewel(id, on), dirty(), renders()}`.

- [ ] The canvas covers the whole front below the top bar: one wrinkle-finish enamel panel (`M.panel`, as the back), with the drum bank as a recessed bay (bevelled `M.frame` lip and real AO in its corners) instead of a separate CSS box. `perimeter()` puts the same bolt ring on the front as on the back, so the turn from one to the other shows the same edge.
- [ ] Render only when dirty: drum turns, knob turns, jewels, resize. Review Focus 3 render-count test via `renders()`.
- [ ] The bank's foot readouts (Frame, Bytes a second, Frames checked) sit on the panel with their odometer bezels and lamp sockets drawn in 3D behind the HTML digits and plates.
- [ ] Without WebGL, nothing changes from today: the `.gl` class never lands and the CSS deck shows.
- [ ] Commit `feat(ui): the front is one panel`. Sync, `--check`.

#### Task 6.3: the switch tiles, the meter and the hardware on the deck

**Files:** `app/ui/front-panel.js`, `app/ui/front-panel.css`, new `app/ui/vu.js` (the meter's own small kit canvas). Not `bombechat.html`'s CSS (that is Task 6.4).

- [ ] **Switch tiles.** Each tile becomes a pressed steel escutcheon recessed into the enamel, held by four slotted bolts at the kit's bolt scale. Sound and Light get `knob()` with their three stops engraved on the enamel; the HTML knob buttons stay in place, focusable, with their SVG hidden under `.gl` and their focus ring still drawn on the button. Each jewel is `jewel()`: amber for Sound, warm white for Light, dark glass with a clean environment reflection when off.
- [ ] **Light's eyepiece.** A turned brass ring (`M.brass`) and a dark lens bezel in 3D around the HTML canvas that shows the camera, aligned to the canvas box by `layout()`.
- [ ] **Bluetooth.** Airgapped: `blank(…, 'Not fitted')`, the same plate the back carries for its Bluetooth group. Standard edition: its live switch is a `knob()` with Off, Contacts and Open, uniform with Sound and Light.
- [ ] **VU meter.** Its own small canvas from `createMachine` with the same lights: a bakelite case, a brass bezel, a glass front with clearcoat and an environment streak, the dial as printed paper (the current SVG drawn into a texture), and a steel needle mesh. The needle redraws only this canvas, at most once per animation frame and only when it moves more than a tenth of a degree.
- [ ] **Outbox.** A shallow brass letter tray in 3D under the slips' column; the HTML slips sit in it.
- [ ] **Stations and the Passing on counter.** Brass card holders in 3D behind each station's HTML card; the counter's odometer bezel as on the bank foot.
- [ ] **Plates.** The HTML `.pl` plates take the exact gradient, edge and engraving weight of the kit's `plate()`; their CSS radial screws are replaced by `bake()`d screw sprites. Compared at 2x side by side with the 3D Plain plate until indistinguishable in material.
- [ ] Function checks of every control unchanged; Review Focus 3 (VU redraws only its canvas) and 4.
- [ ] Commit `feat(ui): the switches, meter and tray in the machine's metal`. Sync, `--check`.

#### Task 6.4: paper

**Files:** `app/ui/bombechat.html` (CSS for `.sheet`, the key check card, `.slip`, `.holder span`, tape and stamps), `app/ui/front-panel.js` (only to publish the light direction).

- [ ] **One light for paper and metal.** `front-panel.js` publishes the key light's direction as CSS custom properties on the app (`--lx`, `--ly`, derived from `K.key.position`), and every paper shadow reads them, so paper shadows fall the way the 3D shadows fall.
- [ ] **Message sheet.** Keep the buff and the fibre noise. Add: a broad falloff across the sheet from the key light (a few percent, warm at the upper left), a two-layer shadow (a tight contact shadow at the edge and a soft cast shadow offset along the light), a hairline darker edge and a one-pixel highlight on the lit edge, a slight lift at the lower right corner shown by shading only (no 3D transform on text). Two brass clips baked with `bake()` hold its top edge to the panel.
- [ ] **Key check card.** Squared paper with the same treatment; the existing pin replaced by a baked brass drawing pin; the card's pencil and stamps unchanged.
- [ ] **Slips and tape.** Torn short edges (a mask, seeded per slip as the back's tape is), thinner paper (lighter shadow, a touch of translucency over the tray's brass).
- [ ] **Station cards.** Paper slid into the 3D holders: an inset shadow at the holder's lip.
- [ ] **Stamps** keep their ink; check their texture still reads against the new paper.
- [ ] 2x crops of every paper part at 1024, 1440, 1728 against the back's wiring schedule card (same paper, same ink) for uniformity.
- [ ] Commit `feat(ui): paper lit by the machine's light`. Sync, `--check`.

#### Task 6.5: the turn shows the real back

**Files:** `app/ui/bombechat.html` (`#back`), `tools/media.cjs` (a `backstill` target), new `app/ui/img/back.webp`.

- [ ] The turn to the settings page currently shows a CSS stand-in for the rear face. Replace it with a still of the real back, rendered headless by `node tools/media.cjs backstill` from `settings.html`'s overview (the card hidden), so the half-turn shows the same three rows of connectors the page then opens on.
- [ ] Check: the still exists, its size is within budget, the turn's second half shows it; regenerate it whenever the back's geometry changes (noted in the media tool's header).
- [ ] Commit `feat(ui): the turn shows the machine's real back`. Sync, `--check`.

#### Task 6.6: review against the bar, then the commissioner

- [ ] Full checks in both editions, the WebGL-off pass, reduced motion, keyboard walk of every control.
- [ ] The uniformity board: 2x crops of each deck part next to its neighbour and next to the back's matching part (blanking plate to blanking plate, plates to plates, card to card), at 1024, 1440, 1512x982 and 1728. Critique against the No AI Look list; iterate.
- [ ] Show the commissioner crops and full screenshots (rendered headless, never their screen). Their comments come back as fixes in this task before Task 9.
- [ ] Push.

**Can run in parallel:** 6.1 first (6.2 and 6.3 build on its interfaces). Then 6.2 before 6.3 (6.3 places parts in 6.2's scene). 6.4 owns `bombechat.html`'s paper CSS and can run alongside 6.3, which keeps its CSS in `front-panel.css`; the only shared touch is the light-direction custom properties, which 6.2 publishes. 6.5 touches `#back` and `tools/media.cjs` only and can run alongside 6.3 and 6.4.

### Task 7: the license holder

**Files:** `LICENSE`, `NOTICE`, `README.md` (the License section's one line only).

- [ ] `LICENSE`: `Copyright (c) 2026 Connor Evans`, MIT text unchanged. `NOTICE`: the same holder. README: "MIT, copyright 2026 Connor Evans. See [LICENSE](LICENSE)."
- [ ] Sync (LICENSE and NOTICE are shared into `bluetooth/`), `--check`. Commit `chore: copyright holder is Connor Evans`. Push.

### Task 8: the repository's description and topics

**Files:** none. Run at execution time with `gh`.

- [ ] Description, in the voice of coranto and ONEXUS (plain, concrete, no hype):

  > An encrypted messenger for laptops that never touches the internet. Messages travel by sound, light or Bluetooth, and the interface is a photoreal Turing Bombe: three rows of drums on the front, every setting a patch cord on the back.

- [ ] Topics: `messenger`, `encryption`, `end-to-end-encryption`, `offline-first`, `airgapped`, `data-over-sound`, `bluetooth-low-energy`, `rust`, `tauri`, `threejs`.
- [ ] `gh repo edit AllStreets/bombechat --description "…" --add-topic messenger --add-topic encryption …` and confirm with `gh repo view AllStreets/bombechat --json description,repositoryTopics`. Check the description for em dashes and emojis before running.

**Can run in parallel:** Tasks 7 and 8 touch no file any other task touches (Task 9 edits README later, after Task 7 has landed) and can run at any point from Task 0 on, beside anything.

### Task 9: README media, last

**Files:** `tools/media.cjs`, `docs/media/*`, `README.md`.

**Interfaces produced:** `node tools/media.cjs [...targets] [--edition bluetooth]`: with `--edition bluetooth` it serves `bluetooth/app/ui` (the edition's simulator, which has a stranger and a contact in range) and still writes to the root's `docs/media/`.

- [ ] Re-render every existing picture, since the front, the back and the drum bank all changed: `banner.png`, `logo.png` (only if the drum it uses changed), `front.png`, `demo.gif`, `light.png`, `light.gif`, `notice.png`, `pairing-offer.png`, `pairing-matched.png`, `first-run.png`, `settings.gif`. Update each alt text to what is now shown (three rows on the back, no rails).
- [ ] New: `back.png`, the back's opening view with its three rows signed Row 1 to Row 3, beside `front.png` so the correspondence is visible in the README.
- [ ] New: `selftest.gif`, the Test the machine stamp pressed on an empty sheet, the message playing once the room is quiet and landing on the sheet.
- [ ] New: `bluetooth.gif`, from the standard edition's simulator: the Bluetooth switch turned to Open, a request arriving as a slip, Accept stamped, and a message exchanged by Bluetooth. Plus `bluetooth.png`, a still of the request tray.
- [ ] README: a "By Bluetooth" section beside "By light" with the clip and two or three plain sentences; `back.png` under Settings; `selftest.gif` under The machine. Keep the README's voice: plain, screenshots doing the visual work, no badge walls.
- [ ] Clip sizes within the existing budget (gifsicle as now). Every picture rendered headless from the app's own pages; nothing captured from a screen.
- [ ] Sync, `--check`. Commit `media: the machine pass, the back's three rows, the self-test and Bluetooth`. Push.

## Order and parallel work

1. Task 0.
2. In parallel: Task 1 (back rows), Task 2 (front rails), Task 7 (license), Task 8 (GitHub metadata).
3. Task 3 (back camera) after Task 1, alongside the end of Task 2.
4. Task 4 (version, build, install) after Tasks 1 to 3; its Rust and config steps may start earlier.
5. Task 5 (self-test) after Task 4.
6. Task 6: 6.1, then 6.2, then 6.3 with 6.4 and 6.5 in parallel, then 6.6. A second build and install (Task 4's last steps) after 6.6, so the commissioner sees the pass in the app.
7. Task 9, last, once the edition, the structure and the beauty pass are final.
