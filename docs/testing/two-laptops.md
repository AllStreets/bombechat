# Two laptops in a room

The acceptance test for Plan 1, the sound path. Two Macs running the airgapped build talk to each other by sound only: no Wi-Fi needed, no Bluetooth in the build. A third Mac joins for the last step.

Part 1 is the modem measured on real hardware. Part 2 is the walkthrough, step by step, with what to look for and a line for the result.

## Building and running the app

On each Mac, from the repository root:

```sh
cd app/src-tauri
cargo tauri build
```

The app lands at `target/release/bundle/macos/Bombechat.app` (relative to the repository root). Copy it to `/Applications` or open it where it is. The first time it opens, macOS asks for microphone access: allow it, or the sound channel reports "unavailable" and nothing can be heard.

The first run asks for your name and a passphrase. Your name is what the people you pair with see; the passphrase locks the store. It creates the store in the app data folder (`~/Library/Application Support/app.bombechat.airgapped/bombechat.db`). To start a Mac over from first run, quit the app and delete that file and its `.salt` beside it.

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

### 9. A broadcast from a third, unpaired laptop

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
| 9. Unverified broadcast | | |

Tested by: ____________ Date: ____________ Macs: ____________
