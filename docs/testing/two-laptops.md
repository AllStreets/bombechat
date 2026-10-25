# Two laptops in a room

The acceptance test for Plan 1, the sound path, and Plan 2, the light path. Two Macs running the airgapped build talk to each other by sound, then by light (one screen flashing a code at the other's camera): no Wi-Fi needed, no Bluetooth in the build. A third Mac joins for the last sound step.

Part 1 is the modem measured on real hardware. Part 2 is the sound walkthrough, step by step, with what to look for and a line for the result. Part 3 does the same for light, with its own measuring tool, a table to fill in, and a troubleshooting list. At the very end is what to send back.

## Building and running the app

On each Mac, from the repository root:

```sh
cd app/src-tauri
cargo tauri build
```

The app lands at `target/release/bundle/macos/Bombechat.app` (relative to the repository root). Copy it to `/Applications` or open it where it is. The first time it opens, macOS asks for microphone access: allow it, or the sound channel reports "unavailable" and nothing can be heard.

The first run asks for your name and a passphrase. Your name is what the people you pair with see; the passphrase locks the store. It creates the store in the app data folder (`~/Library/Application Support/app.bombechat.airgapped/bombechat.db`). To start a Mac over from first run, quit the app and delete that file and its `.salt` beside it.

When a message is erased (Delete messages after), the store overwrites it with zeros and gives the space back, so nobody who opens the store, even with the passphrase, can read it again; on an SSD with APFS the drive may still keep older encrypted copies of those blocks until it reuses them, which only the store's key could ever read.

Before you start:

- Turn Wi-Fi off on both Macs if you want to be sure nothing else is talking. The app doesn't need it.
- Set the output volume to about half. Plug nothing into the headphone jack.
- Check the build is clean: `bash tools/check-airgapped.sh` should end with `airgapped: clean`.

## Part 1: the modem on real hardware

### First: each laptop's quiet room

The app only plays a message, a receipt or a pairing hello while the room is quieter than **Send under** (40 dB unless you change it on the settings page). The room level is the microphone's loudness across all frequencies plus a fixed 94 dB, so it is only a rough figure, and some laptops read a quiet room well above what it is. A laptop whose idle room reads 41 dB or more never sends anything, and nothing says why except the waiting line on the sheet.

On each Mac, in the room you will test in, with nobody talking:

1. Open the app with the Sound switch at "Listen and send", leave it for half a minute, and read the room meter (the needle on the front, marked 30 to 60 dB). Or run `measure` (below) and read the line `room before the first frame: N dB`.
2. Write it down here.

| Mac | Idle room reading | Send under used |
|---|---|---|
| A | | |
| B | | |
| C | | |

If a reading is at or above Send under: turn the machine round and raise Send under to the next step above the reading (45 or 50 dB), and write down the value you used. If even 50 dB is not enough, lower the input volume in System Settings, Sound, Input and read it again. A reading that high in a quiet room is a finding in itself: note the Mac and microphone.

`crates/modem/examples/measure.rs` plays a run of full-size frames through the speaker, records the microphone while they play, decodes the recording and prints what came back.

```sh
cargo run --release -p bombechat-modem --example measure                      # near-silent band, 20 frames
cargo run --release -p bombechat-modem --example measure -- --band audible --gain 0.3
cargo run --release -p bombechat-modem --example measure -- --frames 30 --save heard.wav
cargo run --release -p bombechat-modem --example measure -- --wav heard.wav --frames 30
```

Flags: `--band near|audible`, `--symbol-ms`, `--tones 2|4|16`, `--frames N`, `--gap-ms`, `--gain`, `--save FILE` (keep the recording), `--wav FILE` (decode a recording instead of playing). The frames are the same every run, so a recording made on the receiving laptop can be scored with `--wav` and the same `--frames`.

For two laptops: on the receiving Mac, record with QuickTime or `measure --save` while the sending Mac runs `measure`, then score the recording on the receiver with `--wav`. Or simply run `measure` on the sender with the receiver's app open and watch the drums.

It reports:

- **frames**: how many of the frames came back through Reed-Solomon correction, and how many were exact before correction
- **byte errors corrected**: how much the error correction had to fix
- **snr in band**: signal against the room's noise, in the band in use
- **measured_rate**: the bytes of message a second at this configuration, counting the preamble, framing and parity. This is the number the app's "Bytes a second" readout shows.

### Results so far: one Mac into its own microphone

24 October 2026, MacBook Pro, built-in speakers into the built-in microphone, 48 kHz, a quiet room, defaults (16 tones, 20 ms symbols). The speaker and microphone are a few centimetres apart, so this is the easiest case there is. It shows the modem works through real hardware; it doesn't stand in for the distance tests below.

| Band | Volume | Frames | Decoded | Exact before correction | SNR in band | measured_rate |
|---|---|---|---|---|---|---|
| Near silent, 17.6 to 19.8 kHz | 56% | 10 | 10 (100%) | 10 | 81 dB | 16.1 B/s |
| Near silent | 56% | 30 | 30 (100%) | 30 | 73 dB | 16.1 B/s |
| Near silent | 25% | 30 | 30 (100%) | 30 | 70 dB | 16.1 B/s |
| Audible, 2.0 to 4.2 kHz | 25% | 30 | 30 (100%) | 30 | 55 dB | 16.1 B/s |

No byte needed correcting in any run. `ModemConfig::default()` is unchanged: nothing here shows the defaults failing, and the loopback can't say how much faster they could safely go. That is for the distance tests.

### Distance tests (to do on two laptops)

Run `measure` on one Mac and score on the other. Goal from the plan: at 3 m in a quiet room, at least 95% of frames on the first try. If it falls short, try `--symbol-ms 30` or `--tones 4` and record which configuration gets there; that becomes the new default.

| Distance | Room | Band | Frames decoded | SNR | Notes |
|---|---|---|---|---|---|
| 1 m | quiet | near silent | | | |
| 3 m | quiet | near silent | | | |
| 8 m | quiet | near silent | | | |
| 1 m | people talking | near silent | | | |
| 3 m | people talking | near silent | | | |
| 8 m | people talking | near silent | | | |
| 3 m | quiet | audible | | | |

## Part 2: the walkthrough

Call the two Macs **A** and **B**, about 3 m apart in a quiet room. Both run the freshly built app with the Sound switch at "Listen and send". The third Mac, **C**, comes in at step 8.

Every step lists what to look for. Write what actually happened on the result line.

### 1. Open both machines

On A, open Bombechat, write `Ana` as your name and choose a passphrase. On B, do the same as `Ben`.

Look for: the card titled "Your name and a passphrase" (pressing Lock and open with no name says "Write your name."), then the machine. The top reads "Listening by sound", the Bytes a second readout shows 016 (hover it for 16.1), the key check card says "not yet paired" with a "Pair by sound" stamp, and the station reads "No contacts yet, pair by sound to add one".

Result: ______________________________________________

### 2. Pair by sound

Press "Pair by sound" on A, then on B.

Look for: each card's corner reads "pairing by sound", it pencils "listening for a hello…" and the stamp turns into "Stop listening". Each machine waits for the room to drop under 40 dB on its meter, then plays its hello (near silent: you may hear a faint tick at most). The drums don't turn for a hello. Within about 15 to 30 seconds each card shows the other's name ("Ben said hello" on A) on a dotted pencil line you can write in, over a ruled grid of 60 digits in 12 groups of five, the note "Compare these with theirs, out loud or side by side.", and two stamps, "Matched, add" and "Doesn't match". If B was pressed second it may show A's hello straight away: a hello heard before pressing is held for two minutes (the card notes "Ana said hello. Press Pair by sound to answer." and the note clears when the two minutes are up).

"Stop listening" only puts the card back. It can't call back a hello that is already queued: if the room hasn't gone quiet yet, your hello still plays when it does, and anyone listening nearby gets your offer. That shares only your name and public key; nothing is added on either side until someone presses "Matched, add".

Result: ______________________________________________

### 3. Compare safety numbers

Read the 12 groups on A aloud; B checks each against its own grid.

Look for: the two grids are identical, digit for digit, group for group. Every digit is easy to read at arm's length. Then try the refusal once: on one machine press "Doesn't match". The card clears to "not added", with "Not added. Nothing was kept." and no number or stamp left on it, and nothing is added. Press "Pair by sound" on both again and compare. Before adding, correct the name if you like: on A, change "Ben" to "Ben Okafor". Then press "Matched, add" on both.

Look for after "Matched, add": the number stays exactly where it was, the round violet stamp "Matched" with today's date is pressed beside it, the name above it and in the card's corner is the one you wrote ("Ben Okafor" on A), the note reads "Checked in person, by sound.", the sheet now reads "With Ben Okafor", and the station shows him.

Result (numbers matched?): ______________________________________________

Result (refusal kept nothing?): ______________________________________________

### 4. Send "Water is at the church on Fifth."

On A, type `Water is at the church on Fifth.` and press Enter.

Look for on A: the line types onto the sheet in your own typewriter face with "waiting for quiet, the room is at N dB" under it while the room is loud, then "playing by sound" as it goes. Its slip leaves the outbox. When it has finished playing, "sent, not heard yet".

Result: ______________________________________________

### 5. Watch the drums land

Watch B while A plays.

Look for on B: the top channel shows the message arriving; the drums turn frame by frame, the Plain row spelling WATERXISXATXTHE... (anything that isn't a letter is the X spacer), Heard and Key turning with it. The Frame odometer counts up. The drum clicks stay silent while the message arrives by sound. On the sheet, grey provisional tape types the text with "arriving by sound, not yet checked", then turns into proper teleprinter tape once the signature checks. Total time for this message is roughly half a minute.

Result: ______________________________________________

### 6. See "heard by" come back

Keep watching A.

Look for on A: shortly after B finishes receiving, B plays a receipt (once its room is quiet) and A's line gains the violet stamp "heard by Ben, HH.MM" (the first word of the name you kept).

Result: ______________________________________________

### 7. Send while talking, and see it wait

Two people talk normally next to A (the meter should sit over 40 dB). On A, send `Second message, sent while we talk.`

Look for on A: the slip stays in the outbox reading "sends once under 40 dB", and the line reads "waiting for quiet, the room is at N dB" with the number following the meter. Stop talking. Within a second or two of the needle dropping under 40, it reads "playing by sound" and goes. B receives it as in step 5.

Result: ______________________________________________

### 8. Lock and unlock

Quit Bombechat on B and open it again.

Look for: it asks for the passphrase ("Enter your passphrase"). A wrong one reads "Wrong passphrase." and stays. The right one opens the machine with the same contact, the conversation from steps 4 to 7 on the sheet, and the key check card still showing the number (stamped "Matched", without a date: the store doesn't keep when you paired). Then send one more message from A to confirm B is listening again after the restart.

Result: ______________________________________________

### 9. Turn the Sound switch to Off and watch the recording indicator

On A, turn the Sound knob to "Off". Look at the Mac's own microphone indicator: the orange dot in the menu bar (next to the clock, or in Control Centre) that macOS shows while any app is recording.

Look for: the top reads "Not listening", and within a second or two the orange recording dot goes out. Turn the knob back to "Listen and send": the dot comes back. This is the one check in this document a human has to make by eye; nothing in the test suite can read the menu bar.

Result: ______________________________________________

### 10. A broadcast from a third, unpaired laptop

Bring in C, a third Mac with its own fresh store (name it `Cal`), paired with nobody. Plan 1 has no screen for writing a broadcast, so on C build the app with `cargo tauri build --debug` (debug builds keep the Web Inspector: right-click the window, Inspect), unlock it, and in the inspector's console run `window.__TAURI__.core.invoke('broadcast', { text: 'Road north is open.' })`. It waits for quiet like any send.

Look for on A and B: a new entry on the sheet marked "public", the text on tape, and under it "signed, not checked," followed by the first four hex digits of C's signing key. Nothing on C is marked as a contact on A or B. A broadcast from A (paired, checked) would instead read "signed".

Result: ______________________________________________

## Sign-off

| Step | Pass | Notes |
|---|---|---|
| 1. Open both machines | | |
| 2. Pair by sound | | |
| 3. Compare safety numbers | | |
| 4. Send the water message | | |
| 5. Drums land | | |
| 6. Heard by returns | | |
| 7. Waits for quiet | | |
| 8. Lock and unlock | | |
| 9. Sound Off, recording dot goes out | | |
| 10. Unverified broadcast | | |

Tested by: ____________ Date: ____________ Macs: ____________

## Part 3: light

Light sends the same sealed messages as sound, as a code that flashes on one screen for the other Mac's camera to read. Nothing flashes across the whole screen: the code sits in a fixed square on a steady grey surround, and Escape always stops it.

You need the same two Macs, **A** (Ana) and **B** (Ben), a tape measure, and a phone with a stopwatch. Do it twice: once in daylight (by a window in the day, room lights on) and once in a dim room (curtains closed or after dark, one lamp on).

On the front of the machine, light has three controls:

- **The Light switch**: the round knob on the right, below the drums, between the Sound switch and the Bluetooth blank. The plate under it reads Light. Its three engraved positions are Off, Read and Read and flash. Click the knob to turn it one step. Read watches the camera for a code; Read and flash also flashes yours.
- **The Flash key**: the bakelite key beside "Send by sound", under the reply line. It sends what you wrote by light.
- **The eyepiece**: the small brass ring at the top right of the Light switch's tile. While Light reads, it shows what the camera sees, small and grey, so you can aim. Clicking it shuts it (nothing is shown); click again to open it.

Under the Light switch, a strip of typed tape appears when the camera has something to say: "Tilt or dim the screen." or why the camera isn't working. See Troubleshooting below.

The speed is set on the back of the machine. Click the **Settings** plate at the top right: the machine turns round. On the "Wiring schedule" card, in the Light group, the **Flash speed** row has Slow, Normal and Fast. Click one. Then click the **Front of the machine** plate to turn back. Slow shows 48 cells at 6 frames a second, Normal 56 cells at 10, Fast 64 cells at 15. The bigger the cells, the further away a camera can read them.

### 3.1 The camera itself

This checks the Mac's camera works from code and that stopping really lets it go (the camera light goes out). Run it on each Mac, from the repository root, with nothing else using the camera (quit FaceTime, Photo Booth, Zoom and Bombechat).

First give Terminal camera access: open System Settings, Privacy and Security, Camera, and turn on Terminal (or whichever terminal app you use). If Terminal isn't in the list, run the command below once: macOS asks, and you click Allow. Then run:

```sh
cargo test -p bombechat-light --lib camera -- --ignored --nocapture
```

Success looks something like this (your numbers will differ), ending with `test result: ok. 1 passed`:

```text
started in 600 ms
10 pictures, 1280x720, 30.0 a second
after stop: session isRunning Some(false)
after 40 ms: session freed true, delegate freed true, a camera running false
```

The green light beside the camera comes on for a second or two, goes out, comes on again briefly and goes out for good.

Failure looks like `panicked at ... start the default camera: PermissionDenied` (Terminal has no camera access: see above), `close other apps using the camera` (something else has it), or `picture 0 within 5 s` (the camera gave nothing). Copy the whole output into your notes.

| Mac | Passed | Size and rate printed | Camera light went out |
|---|---|---|---|
| A | | | |
| B | | | |

### 3.2 Measuring without the app

`crates/light/examples/measure.rs` flashes a fixed run of codes in the browser, or reads them with the camera, and prints what got through. It is a separate tool: it isn't built into Bombechat. Build it in release mode, or it reads too slowly to keep up:

```sh
cargo run --release -p bombechat-light --example measure -- --selftest
```

That one needs no camera: it draws every code, feeds the pictures straight to the reader and must end with `selftest passed`. If it doesn't, stop and send back the output.

**Two Macs.** On A, show the codes:

```sh
cargo run --release -p bombechat-light --example measure -- --show normal
```

A browser window opens with a code on a grey background, and a line under it such as "Normal, 56 cells at 10 frames a second. Message 1 of 6, frame 12 of 40. Escape stops it." Make the window full screen (Control, Command and F). Escape stops it; Space starts it again; the keys 1, 2 and 3 switch to Slow, Normal and Fast. Leave out the word `normal` to cycle through all three.

On B, quit Bombechat (the camera can only be used by one app at a time), point B's camera at A's screen at the distance you are testing, and read for 30 seconds:

```sh
cargo run --release -p bombechat-light --example measure -- --read normal --seconds 30
```

Every 5 seconds it prints how many pictures it has taken and read. At the end it prints one line for the preset, for example:

```text
normal  frames seen   812  frames ok   790 ( 97%)  symbols/s  10.0  bytes/s  2290 of 2290  first message   0.9 s  messages 6
```

- **frames seen**: camera pictures with a code in view
- **frames ok**: those it read and checked
- **symbols/s** and **bytes/s**: how fast pieces of a message came in while one was arriving; "of 2290" is the most this speed can give (the same number as the app's "Bytes a second" readout)
- **first message**: from the first frame read of a message to the last of it
- **messages**: how many of the six came back whole. Each one is checked byte for byte; if one ever comes back wrong, a line starting `WRONG` says so.

Success: frames ok is most of frames seen, bytes/s is close to the most, and messages counts up. Failure looks like `frames ok 0`, `messages 0`, or one of these lines under the result:

- `no code in view in N pictures: aim the camera at the code`
- `most codes seen were not read: tilt or dim the screen, or come closer`
- `camera access denied: allow Terminal in System Settings, Privacy and Security, Camera, then run this again`

**One Mac.** Run `--show` on a second display, or open the page it writes (the path is printed after `wrote`) on another computer's browser, and run `--read` against it. Or take a burst of photos of the flashing page with a phone, copy them to a folder on the Mac (here `~/Desktop/photos`), and decode them:

```sh
mkdir pngs
sips -s format png ~/Desktop/photos/* --out pngs
cargo run --release -p bombechat-light --example measure -- --images pngs
```

It prints a line for each photo ("1 frame(s) read", "a code, not read" or "no code") and then the same result lines, timed as if the photos came 30 a second (`--fps N` to change it), so for photos only frames seen and frames ok mean much.

Fill in one line for each combination, with B's camera facing A's screen. Measure from screen to screen.

| Distance | Light | Speed | Frames ok / seen | bytes/s | First message | Messages (of 6) |
|---|---|---|---|---|---|---|
| 30 cm | daylight | Slow | | | | |
| 30 cm | daylight | Normal | | | | |
| 30 cm | daylight | Fast | | | | |
| 60 cm | daylight | Slow | | | | |
| 60 cm | daylight | Normal | | | | |
| 60 cm | daylight | Fast | | | | |
| 1 m | daylight | Slow | | | | |
| 1 m | daylight | Normal | | | | |
| 1 m | daylight | Fast | | | | |
| 30 cm | dim | Slow | | | | |
| 30 cm | dim | Normal | | | | |
| 30 cm | dim | Fast | | | | |
| 60 cm | dim | Slow | | | | |
| 60 cm | dim | Normal | | | | |
| 60 cm | dim | Fast | | | | |
| 1 m | dim | Slow | | | | |
| 1 m | dim | Normal | | | | |
| 1 m | dim | Fast | | | | |

What to expect: on a 13-inch screen the code is about 13.5 cm across, the same size the app shows it. At 1 m each cell is only two or three camera pixels, which is at the edge of what the reader can do, so Fast and Normal may well fail there. A miss is a result, not a mistake: write it down.

### 3.3 The walkthrough by light

Start both Macs fresh, so pairing by light starts from nothing. On each, quit Bombechat and delete the store:

```sh
rm ~/Library/Application\ Support/app.bombechat.airgapped/bombechat.db ~/Library/Application\ Support/app.bombechat.airgapped/bombechat.db.salt
```

Open Bombechat on A as `Ana` and on B as `Ben`, as in step 1. On both, turn the Sound switch to Off (so nothing can go by sound) and leave Flash speed at Normal.

Put the two Macs on a table facing each other, lids open, screens about 30 cm apart. Tilt each lid so its camera points at the other's screen.

### 11. Turn Light to Read, and the camera comes on

On both, click the Light knob once, to Read. The first time, macOS asks whether Bombechat may use the camera: click Allow.

Look for: the green light beside each camera comes on, the plate in the top bar reads "Reading by light", and the eyepiece shows a tiny grey picture of the room (wave a hand in front of the camera and it moves). If the tape under the Light switch says "Camera permission denied.", see Troubleshooting.

Result: ______________________________________________

### 12. Pair by light

Click the Light knob once more on both, to Read and flash. On A, press the **Pair by light** stamp on the Key check card (the squared paper card between the message sheet and the switches). Then press it on B.

Look for, on each: the card's corner reads "pairing by light", with a pencil sketch of two screens facing each other, "reading for a hello…" and the stamp now reads "Stop reading". The message sheet gives way to a grey panel in a brass bezel with the plate "Pairing by light": for about a second it shows a plain grey square inside a white border (the still moment), then the code starts changing. The caption reads "Hold your screens face to face." and under it "reading theirs, Escape stops it". The "Frames shown" counter counts up.

Within a few seconds of both showing their codes, each card shows the other's name ("Ben said hello" on A) over a grid of 60 digits in 12 groups, and the caption changes to "Their key is read. Yours finishes showing." A few seconds later the panel closes by itself and the stamps "Matched, add" and "Doesn't match" are under the number. The offer's heading says "pairing by light".

Compare the 12 groups as in step 3, then press **Matched, add** on both. The note reads "Checked in person, by light."

Result: ______________________________________________

### 13. Send by light at 30 cm

On A, type `Water is at the church on Fifth.` in the reply line and press the **Flash** key (not Enter: Enter sends by sound, which is off).

Look for on A: the line types onto the sheet, then the panel opens with the plate "Flashing by light", the caption "Hold this screen up to Ben's camera." and "flashing, Escape stops it". The "Bytes a second" readout shows 2290 at Normal (0948 at Slow, 4680 at Fast). Under the line on the sheet: "flashing on screen".

Look for on B: grey provisional tape types the text with "arriving by light, not yet checked" under it and "light" in the margin, and the drums turn. Then it turns into proper tape once the signature checks. Then B flashes a receipt back for a few seconds (its own panel opens; leave it facing A).

Look for on A afterwards: the panel closes, and A's line gains the stamp "seen by Ben, HH.MM".

Start the stopwatch when A's code starts changing and stop it when B's tape turns from grey to proper tape.

Result: ______________________________________________

### 14. Stop flashing with Escape

On A, send `Second message, then Escape.` with the Flash key. As soon as the code starts changing, press Escape.

Look for on A: the panel closes at once, the Light knob turns itself to Read, and the top bar briefly reads "Flashing stopped. Light is at Read, and the message waits." The line reads "waiting for Read and flash" and its slip in the outbox reads "flashes at Read and flash". Nothing flashes again. Then click the Light knob once (to Read and flash): the panel opens and the message goes as in step 13.

Try the panel's **Stop** key the same way once: it does exactly what Escape does.

Result (Escape): ______________________________________________

Result (Stop key): ______________________________________________

### 15. Distances and light

Send one short message from A to B for every line of the table below, with the Flash key. Change speed on the back panel (Settings plate, Flash speed row) on both Macs before each group of lines. Move the Macs apart for 60 cm and 1 m, and tilt the lids so the cameras still point at the other screen (use B's eyepiece to aim). Give each try up to 30 seconds.

- **Arrived**: yes if the message landed on B as proper tape
- **Seconds**: the stopwatch, as in step 13
- **Seen by**: yes if A's line got "seen by Ben" (B's receipt has to make it back the other way)
- **Hint**: yes if the tape under B's Light switch said "Tilt or dim the screen."

| Distance | Light | Speed | Arrived | Seconds | Seen by | Hint | Notes |
|---|---|---|---|---|---|---|---|
| 30 cm | daylight | Slow | | | | | |
| 30 cm | daylight | Normal | | | | | |
| 30 cm | daylight | Fast | | | | | |
| 60 cm | daylight | Slow | | | | | |
| 60 cm | daylight | Normal | | | | | |
| 60 cm | daylight | Fast | | | | | |
| 1 m | daylight | Slow | | | | | |
| 1 m | daylight | Normal | | | | | |
| 1 m | daylight | Fast | | | | | |
| 30 cm | dim | Slow | | | | | |
| 30 cm | dim | Normal | | | | | |
| 30 cm | dim | Fast | | | | | |
| 60 cm | dim | Slow | | | | | |
| 60 cm | dim | Normal | | | | | |
| 60 cm | dim | Fast | | | | | |
| 1 m | dim | Slow | | | | | |
| 1 m | dim | Normal | | | | | |
| 1 m | dim | Fast | | | | | |

If nothing has arrived on B after 30 seconds, write "no" and move on. If you then try again at a shorter distance or a slower speed, note that in the Notes column.

### 16. Turn Light to Off and watch the camera light

On B, click the Light knob until it points at Off.

Look for: within a second or two the green light beside B's camera goes out, and so does the camera icon in the menu bar if your macOS shows one. The eyepiece goes dark and the plate in the top bar reads "Not listening". Turn it back to Read: the light comes back. Like step 9, this is a check only your eyes can make.

Result: ______________________________________________

### 17. Refusals

On A, turn Light to Read (not Read and flash). Write something and press Flash.

Look for: nothing flashes, and under the reply line: "Light is set to read only. Turn it to Read and flash to send by light." What you wrote stays. Turn Light to Off and press Flash: "Light is off. Turn it to Read and flash to send by light." Press **Pair by light** at Off: the card says "Light is off. Turn it to Read and flash to pair by light." and nothing is asked of the camera.

Result: ______________________________________________

### Troubleshooting

**"Tilt or dim the screen."** on the tape under the Light switch. The camera sees a code but can't read it. In order of what usually helps:

1. Tilt the sending screen a few degrees, so the room's lights or a window aren't reflected in it.
2. Turn the sending screen's brightness down a few steps. A screen at full brightness in a dim room blooms on the camera and the cells run together.
3. Move closer, or hold both Macs still. Cells under about three camera pixels can't be read, and a moving screen blurs.
4. Go one speed slower on both Macs (Settings plate, Flash speed). Slow has the biggest cells.

The hint stays for a moment after the last unreadable code and clears by itself. In `measure`, the same advice is the line `most codes seen were not read: tilt or dim the screen, or come closer`.

**Nothing in the eyepiece, or the eyepiece is dark.** Light is at Off, or the eyepiece is shut (click it once), or the camera isn't working: read the tape under the Light switch.

**What the tape under the Light switch can say about the camera:**

- "Camera permission denied." Open System Settings, Privacy and Security, Camera and turn on Bombechat. Quit Bombechat and open it again.
- "Camera permission not answered." macOS asked and nobody clicked. Turn Light to Off, then to Read again, and click Allow.
- "Camera in use by another app." Quit FaceTime, Photo Booth, Zoom, and `measure` if it is still reading. Then turn Light to Off and back to Read.
- "No camera." The Mac has no camera it can use (a closed lid on an external display, for instance).
- "Camera unsupported (…)" Write down the whole text and send it back.

**Refusals when you press Flash or Pair by light:**

- "Light is off. Turn it to Read and flash to send by light." or "… to pair by light." Turn the Light knob to Read and flash.
- "Light is set to read only. Turn it to Read and flash to send by light." Read only reads; turn it one more step.
- "The Light switch was not read from the machine. …" The page couldn't read the switch. Quit and reopen Bombechat; if it stays, send the text back.
- "Too long to send: N bytes, at most 2814." Shorten the message. What you wrote is kept.

**The message says "waiting for Read and flash".** Escape or Stop turned Light to Read, and the message is waiting, as it should. Turn the knob to Read and flash and it goes.

**"Not stopped." on the flashing panel.** The machine couldn't save the switch's new position, so it is still flashing. The rest of the line says why. Press Stop again; if it keeps failing, quit Bombechat.

**The message arrived but "seen by" never comes back.** The receipt goes back by light, so B's screen has to face A's camera too, and B must be at Read and flash. Turn both lids so each camera sees the other's screen.

**`measure` problems.** `camera access denied` means Terminal has no camera access (see 3.1). `no code in view` means the camera is pointing somewhere else. `frames ok 0` with many frames seen is the "Tilt or dim" case above.

### Light sign-off

| Step | Pass | Notes |
|---|---|---|
| 3.1 Camera test, A | | |
| 3.1 Camera test, B | | |
| 3.2 measure selftest | | |
| 11. Light to Read, camera on | | |
| 12. Pair by light | | |
| 13. Send by light at 30 cm, seen by back | | |
| 14. Escape and Stop | | |
| 15. Distance table filled in | | |
| 16. Light Off, camera light goes out | | |
| 17. Refusals | | |

Tested by: ____________ Date: ____________ Macs (model and year): ____________

## What to send back

Send these in one message. Photos of filled-in paper pages are fine.

1. The result lines and sign-off tables from Parts 2 and 3, filled in.
2. The full output of the camera test (3.1) from each Mac.
3. The output of `measure --selftest`, and the result line of every `measure --read` you ran, with the distance, the light and the speed beside each.
4. The Mac models and years, and which Mac showed and which read.
5. Anything the tape under the Light switch said that isn't in Troubleshooting, word for word.
6. For any step that failed: what you did just before, what the screen said, and a photo of the screen if you can. Please don't send screen recordings.
