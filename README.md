<p align="center">
  <img src="docs/media/banner.png" alt="Bombechat: messages that never touch the internet. A drum bank from the app, reading CHURCH on its top row" />
</p>

<p align="center">
  <img alt="channel" src="https://img.shields.io/badge/channel-sound_%C2%B7_light-c29a5c?style=for-the-badge&labelColor=141311" />
  <img alt="build" src="https://img.shields.io/badge/build-airgapped-2c4a3a?style=for-the-badge&labelColor=141311" />
  <img alt="crypto" src="https://img.shields.io/badge/crypto-X25519_%C2%B7_XChaCha20_%C2%B7_Ed25519-33465b?style=for-the-badge&labelColor=141311" />
  <img alt="tests" src="https://img.shields.io/badge/tests-471_passing-a5822e?style=for-the-badge&labelColor=141311" />
  <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-MIT-7a2a22?style=for-the-badge&labelColor=141311" /></a>
  <br />
  <img alt="Rust" src="https://img.shields.io/badge/Rust-workspace-6e6656?style=flat-square&labelColor=141311" />
  <img alt="Tauri 2" src="https://img.shields.io/badge/Tauri-2-6e6656?style=flat-square&labelColor=141311" />
  <img alt="three.js" src="https://img.shields.io/badge/three.js-r170-6e6656?style=flat-square&labelColor=141311" />
  <img alt="SQLCipher" src="https://img.shields.io/badge/SQLCipher-Argon2id_key-6e6656?style=flat-square&labelColor=141311" />
  <img alt="macOS" src="https://img.shields.io/badge/macOS-CoreAudio-6e6656?style=flat-square&labelColor=141311" />
</p>

<p align="center">
  <a href="#how-a-message-travels">How it travels</a>&nbsp;&nbsp;&nbsp;
  <a href="#by-light">By light</a>&nbsp;&nbsp;&nbsp;
  <a href="#by-bluetooth">By Bluetooth</a>&nbsp;&nbsp;&nbsp;
  <a href="#the-machine">The machine</a>&nbsp;&nbsp;&nbsp;
  <a href="#pairing-and-safety-numbers">Pairing</a>&nbsp;&nbsp;&nbsp;
  <a href="#privacy-and-the-airgap">Privacy</a>&nbsp;&nbsp;&nbsp;
  <a href="#settings">Settings</a>&nbsp;&nbsp;&nbsp;
  <a href="#build-and-run-it">Build it</a>&nbsp;&nbsp;&nbsp;
  <a href="#two-editions">Two editions</a>&nbsp;&nbsp;&nbsp;
  <a href="#checks">Checks</a>&nbsp;&nbsp;&nbsp;
  <a href="#roadmap">Roadmap</a>
</p>

<br />

When the network goes down, so does almost every way of sending a message. And when it's up, every message you send passes through someone else's servers.

Bombechat sends encrypted messages between laptops in the same room by sound or by light. The speaker plays the message on tones high enough that most adults barely hear them, or the screen shows it as a changing code for the other laptop's camera, and it decrypts on arrival. There is no account, no server and no network code in the build: two Macs with Wi-Fi switched off can pair and talk. It looks and sounds like the machine Alan Turing designed at Bletchley Park to break Enigma.

<p align="center">
  <img src="docs/media/demo.gif" alt="A message arriving by sound: the drums turn to each new frame, the provisional tape on the message sheet fills in, and the finished message settles in type" width="900" />
</p>

The modem carries about **16 bytes a second** on sixteen tones between **17.6 and 19.8 kHz**. Each message is sealed to its recipient with X25519 and XChaCha20-Poly1305, signed with Ed25519, and checked chunk by chunk as it lands, so the drums show it arriving before the last frame is in.

<br />

## How a message travels

<img src="docs/media/front.png" alt="The front of the machine, one framed panel: three rows of twelve code drums labelled Plain, Heard and Key, with the frame counter under them. Below, the message sheet with a message arriving as provisional tape, the key check card, the Sound and Light switches beside a blanking plate where Bluetooth would be, the room meter, the outbox slip and the stations." />

You type a reply on the message sheet and press Send by sound. The message is sealed, cut into 48-byte chunks, wrapped in Reed-Solomon parity and played as multi-tone FSK, each transmission starting with a chirp the receiver can lock onto. Before playing, the sender waits until the room is quieter than 40 dB, and by default it plays the message twice more, a minute and a half apart, in case the first was missed.

On the other laptop, the receiver finds the chirp, corrects for the two sound cards running at slightly different speeds, and reads the tones. Every chunk is authenticated the moment it arrives. What has been heard so far is typed onto the sheet as provisional tape, *arriving by sound, not yet checked*, and once the whole message has landed and its signature checks out, it is set in type.

Light works the same way, between a screen and a camera, and is described below. Bluetooth comes only in the standard build, described after it.

## By light

<img src="docs/media/light.png" alt="The flashing view: the drums and the message sheet give way to a signal lamp in a brass bezel, showing a square code of black and white cells, with Frames shown at 13 and Bytes a second at 2290. The caption reads Hold this screen up to Marisol's camera." />

Light carries the same sealed messages as sound, from one screen to another laptop's camera. It's much faster, and it's the hardest channel to overhear, since someone has to see the screen.

Turn the Light switch to Read and flash, type a reply and press Flash. The message sheet gives way to a signal lamp: a square code of black and white cells in a brass bezel, with a caption saying whose camera to hold it up to. The message is cut into frames with a RaptorQ fountain code, so the camera doesn't need every frame, only enough of them, and it can start reading partway through. Each frame carries Reed-Solomon parity and a checksum, and three corner marks let the reader find the grid at any angle or distance. On the other laptop the camera reads the code, the drums turn, and the message is typed onto the sheet as provisional tape until its signature checks, exactly as it is for sound. A receipt flashes back the other way and the sender's line is stamped *seen by*.

Flash speed on the back of the machine has three settings:

| Speed | Code | Frames a second | About |
|---|---|---|---|
| Slow | 48 by 48 cells | 6 | 948 bytes a second |
| Normal | 56 by 56 cells | 10 | 2290 bytes a second |
| Fast | 64 by 64 cells | 15 | 4680 bytes a second |

Bigger cells read from further away and in brighter rooms. Even Slow moves a full-length message in a few seconds, against minutes by sound.

<p align="center">
  <img src="docs/media/light.gif" alt="A message going out by light. The reply is typed, the cursor presses Flash, and a signal lamp opens over the drums and the message sheet: a plain grey square for a moment, then a code of black and white cells changing ten times a second while the Frames shown counter climbs. Then the lamp closes and the message sits on the sheet, stamped seen by Marisol." width="900" />
</p>

Pairing works by light too. Both of you press Pair by light and hold the screens face to face. Each screen shows its hello as a code while its camera reads the other's, and the key check card then shows the same 60-digit safety number to read out loud.

The code is built to be gentle on the eyes. Nothing flashes across the whole screen: the code sits in a fixed square on a steady surround, and it opens on a plain grey field of its own average brightness for a moment before it starts to change. Every 8 by 8 block of every frame is kept between 40 and 60 percent dark, so the brightness of each part of the code stays nearly the same from frame to frame, whatever the message says. Escape always stops it. The lamp closes at once, the Light switch turns itself back to Read, and the message waits until you turn it to Read and flash again.

While Light reads, a small brass eyepiece on its switch shows what the camera sees, small and grey, so you can aim one laptop at the other. Click it to shut it and nothing the camera sees is shown. Turn Light to Off and the camera is let go, so its green light goes out.

[Part 3 of the two-laptop walkthrough](docs/testing/two-laptops.md#part-3-light) tests all of this on real Macs, with distances, speeds, and daylight against a dim room.

## By Bluetooth

<p align="center">
  <img src="docs/media/bluetooth.png" alt="The front panel on the standard build with the Bluetooth switch at Open and its jewel lit. In the tray beside the room meter a torn request slip reads Ines Duarte, as they gave it, key CA96, asks to connect, 09.15, with Accept and Decline stamps. Under it a strip lists the strangers in range: K7MX, R2QD and one more." width="435" />
</p>

On Bombechat Standard, the standard build, the third switch on the front of the machine is Bluetooth, where the airgapped build has a blanking plate. It has three positions. Off is where it starts, and at Off the radio is idle: nothing is advertised or scanned for, and every connection this Mac opened is closed. macOS is asked for Bluetooth permission the first time you turn it on, and not before. At Contacts it reaches the people you've added whenever they're in range, and a Send by Bluetooth key sits beside Flash and Send by sound. At Open, strangers nearby can also ask to connect, and you can ask them.

Requests work the way they do on LinkedIn. A stranger in range shows up as a random four-character label, never a name or a key. Asking sends your name and key to that one person, and nothing of theirs comes back until they accept. On their machine the request arrives as a slip with Accept and Decline. Accept adds you as a contact who hasn't been checked in person yet, and lays out the safety number to compare when you do meet. Decline keeps nothing and sends nothing back, so from your side it looks exactly like a request nobody answered. While New contacts need a key check is on, which it is until you change it, requests are shown but can't be sent or accepted.

<p align="center">
  <img src="docs/media/bluetooth.gif" alt="Bluetooth on the front of the machine. The cursor turns the Bluetooth switch from Contacts to Open, and the Near readout goes from one stranger to three as a list of strangers in range appears in the tray. A slip arrives saying Ines Duarte asks to connect. Accept is stamped, the key check card lays out her safety number, and her station is added marked not confirmed. A reply to Marisol is typed and sent with Send by Bluetooth: it lands on the sheet with bluetooth in the margin and is stamped heard by Marisol. Her answer comes back by Bluetooth, the drums turning to it as it is typed onto the sheet." width="900" />
</p>

Live sessions use the Noise protocol, through the `snow` library, with the same proven primitives as the rest of Bombechat: X25519, ChaCha20-Poly1305, SHA-256 and HKDF, with each identity signed by its Ed25519 key. Contacts open sessions with Noise IK and requests use Noise XN, so a machine never tells a stranger who it is unless its owner accepts. Bombechat uses its own Bluetooth service, separate from Bitchat's.

The advert carries no name and no key, only part of a random token that changes every 12 to 15 minutes. Contacts tell each other their next few tokens inside their sessions, so they can find each other again after a change.

Someone in range can still link some things. Every Bombechat Mac advertises the same service, so anyone can tell one is nearby. The moment one advert stops and the next starts can tie the two tokens together, and so can a contact's connection that carries on across the change. A tracker that has connected to a Mac once from a fixed address can recognise it later by whether it's given the token. And the Mac's own name can be read by anyone in Bluetooth range, which is why the app asks you to give it a plain one. Whether macOS changes the Bluetooth address when the advert restarts is one of six things still to be measured on two real Macs. The [threat model in the design spec](docs/superpowers/specs/2026-10-24-bombechat-design.md#threat-model-bluetooth-sessions) goes through all of it.

[Part 4 of the two-laptop walkthrough](docs/testing/two-laptops.md#part-4-bluetooth) tests Bluetooth on two Macs, including those measurements.

## The machine

The window is a Bombe and its paperwork. Three rows of drums show what the sender wrote, what the microphone caught, and the key you share. When a frame lands, the drums turn to it and the top row settles on the words. The drums are real 3D objects, built in three.js from the same enamel, brass and bakelite as the rest of the machine, and they make a ratchet click for every letter that passes the index and a clunk when they settle. That sound is synthesised in the browser and filtered hard above 7 kHz, so it never leaks into the band Bombechat listens on, and it goes quiet while a message is arriving.

To see the drums work before you've paired with anyone, press Test the machine, the stamp at the bottom right of the message sheet. On a new machine the empty sheet points at it. The machine seals a message to its own key, waits for the room to go quiet as any send does, plays it, and spells it out on the drums as its own microphone hears it back. If you've typed something in the reply line, that's what it sends, up to 100 bytes, so you can watch your own words come round. Otherwise it sends a short line of its own. Nothing of the test is kept.

<p align="center">
  <img src="docs/media/selftest.gif" alt="Testing the machine on a new, empty message sheet. hello world is typed in the reply line and Test the machine is pressed. The entry says it is sealed to this machine and waiting for quiet while the room meter sits near 47 dB; the room falls quiet and it plays. The wait while it plays is sped up four times. Then the frames land: the drums turn, the plain row reading HELL and then O WORLD after the sender's keys, while HELLO WORLD types onto the sheet as tape and is stamped heard by this machine." width="900" />
</p>

While a test runs the stamp reads Stop the test, in red. Press it, or Escape, and the sound stops at once, including what was already handed to the speaker. Bluetooth headphones and AirPlay speakers keep a buffer of their own, so they can play on for a moment. A test by light works the same way with a mirror held up to the screen.

<img src="docs/media/notice.png" alt="The message sheet with the Public switch thrown and a notice typed in, signed with this machine's key" />

Flip the switch beside the reply line from Reply to Public and the same line writes a public notice instead: unencrypted, signed with your key, readable by anyone in range. It's meant for things like *Water at the church on Fifth until dusk*.

## Pairing and safety numbers

<p>
  <img src="docs/media/pairing-offer.png" alt="The key check card offering Teodor Vance, with the 60-digit safety number in twelve groups of five" width="49%" />
  <img src="docs/media/pairing-matched.png" alt="The same card after Matched, add: the number kept and a Matched stamp in violet ink" width="49%" />
</p>

There is no directory to look people up in. Both of you press Pair by sound and each laptop plays a hello carrying its public keys. The key check card then shows a 60-digit safety number, worked out from both keys, in twelve groups of five. Read them to each other out loud. If they match, press Matched, add and the card is stamped. If they don't, Doesn't match keeps nothing.

On the airgapped build this is the only way to add a contact, so every contact has been checked in person. The standard build adds requests to connect by Bluetooth, which add a contact unchecked until you meet.

## Privacy and the airgap

The airgapped build has no networking or Bluetooth code in it at all, so there is nothing to switch off and nothing to trust. `tools/check-airgapped.sh` proves it on every change: it reads the build's whole dependency tree and fails if any Bluetooth or networking crate turns up (`reqwest`, `hyper`, `btleplug` and the rest), and on macOS it checks with `otool` that the built app links no Bluetooth framework. CI also runs it with the `bluetooth` feature switched on and expects it to fail, so the check itself is tested.

- **The store** is SQLCipher, keyed from your passphrase with Argon2id. Contacts, messages and settings stay on the laptop.
- **Deleting** overwrites with zeros. SQLite's `secure_delete` is on, and the space is given back, so a deleted message can't be read again even by someone with the passphrase.
- **Delete messages after** counts only the time the machine has been running, measured on a clock that can't be wound back. Changing the date can't make messages go sooner, and a laptop left closed for a month doesn't wake up to an empty sheet.
- **The interface** asks nothing of the network. The fonts are embedded, three.js is vendored, and the page check fails if the page requests anything off the machine or if an `http` address appears anywhere in `app/ui`.

<img src="docs/media/first-run.png" alt="The first-run registration form: your name as people you pair with see it, a passphrase, and the passphrase again" />

The first run asks for your name and a passphrase. Nobody can recover the passphrase for you. On a Mac you can take the lock off, and the store key then lives in the macOS Keychain so the app opens without asking. If the Keychain won't keep the key, the lock stays on and the settings say why.

## Settings

<img src="docs/media/back.png" alt="The back of the machine as it opens. On the left, the wiring schedule card with the Sound, Light, Bluetooth and Machine settings. On the right, the rear panel from its top: a cable duct along the top edge, then three rows of twelve drum connectors signed Row 1, Row 2 and Row 3, columns numbered 12 to 1, their cords looping between them and gathered into five bundles that rise into the duct. Below, the maker's plate and the rows of jacks for every setting, the Bluetooth row blanked off as not fitted." />

Settings live on the back of the machine. Press the Settings plate and the window turns round. On the left is a wiring schedule, and on the right is the rear panel, rendered in 3D. It opens on the top of the panel, where the drums come through. There are three rows of twelve connectors, Row 1 to Row 3, one behind each drum on the front, so Row 1 is the back of the Plain row. Seen from behind, the columns run 12 to 1. Their cords are gathered into five bundles that run up into a duct along the top.

Under the maker's plate, every setting is a row of jacks with a patch cord in it. On and off settings have two jacks, and number settings have one jack per value, so Repeats at 3 means the cord sits in jack 3.

<img src="docs/media/settings.gif" alt="The back of the machine. Repeats is changed from 2 to 3 on the wiring schedule card, and the camera moves down from the top of the panel onto that row while its patch cord pulls out and seats in jack 3. A click returns to the opening view of the three connector rows and the duct, then the same plug is dragged back to jack 2 on the panel itself, and the view pulls out again." />

Change a setting on the card and the camera moves in on that row while the cord pulls out, travels along the panel and seats itself in the new jack. You can also drag a plug on the panel itself, or click a jack. Settings that wait for a later feature, like the hop limit for relay, are taped over with a note saying what they're waiting for.

## Build and run it

You'll need macOS, Rust, Node and the Tauri CLI (`cargo install tauri-cli`).

```sh
git clone https://github.com/AllStreets/bombechat.git
cd bombechat/app/src-tauri
cargo tauri build
```

This is version 0.5.0. The app lands in `target/release/bundle/macos/Bombechat.app` at the top of the repository. The first time it opens, macOS asks for the microphone; without it nothing can be heard. It asks for the camera the first time Light is turned on. Its store and settings live in `~/Library/Application Support/app.bombechat.airgapped/`. That is the airgapped build, Bombechat. The standard build with Bluetooth, Bombechat Standard, is built from `bluetooth/` (see [Two editions](#two-editions)). Both sides of the machine have a plate in the top bar reading Build, the version and the commit it was built from, so you can always tell which build you have open. `docs/testing/two-laptops.md` walks through trying it on two Macs.

### Or look at it in a browser

The interface runs in any browser against a simulated conversation, with no Rust needed and no sound card involved:

```sh
npm install
npm run serve
```

Then open http://localhost:4810/app/ui/bombechat.html.

## Two editions

Bombechat comes as two builds from one repository.

The airgapped build, Bombechat, is at the root and talks by sound and light only. It has no Bluetooth or networking code in it at all. It exists for people who can't risk a radio giving them away: a build with Bluetooth in it advertises the Mac to anything in range whenever the switch is on, and a bug or a forgotten setting could leave it on. When the code isn't there, there's nothing to leave on, and `tools/check-airgapped.sh` proves on every change that it isn't.

The standard build, Bombechat Standard, is in `bluetooth/`. It's the same project plus Bluetooth, for everyday use with the people you talk to most and for requests to connect. Its app is `app.bombechat.standard`, so its store and Keychain item never mix with the airgapped app's, and its maker's plate says "standard build".

To build the airgapped app:

```sh
cd app/src-tauri
cargo tauri build
```

It lands in `target/release/bundle/macos/Bombechat.app`. To build the standard app:

```sh
cd bluetooth/app/src-tauri
cargo tauri build
```

It lands in `bluetooth/target/release/bundle/macos/Bombechat Standard.app`, so both apps can sit in `/Applications` side by side. The standard build's checks run from inside `bluetooth/`: `cargo test --workspace` (672 tests there) and `npm run check`.

`bluetooth/` is generated. `tools/sync-bluetooth.sh` copies everything the root tracks (apart from `docs/`, `mockups/`, `.github/`, this README and the script and its test) into it, applies the small patches in `bluetooth/edition/patches/` and lays `bluetooth/edition/files/` over the top. Fix things at the root and run the script; CI fails if `bluetooth/` doesn't match what it writes, so the two editions differ only where `bluetooth/edition/` says they do. The one exception is `bluetooth/Cargo.lock`, which the edition keeps for itself since its dependencies will differ. The root never gains Bluetooth code, and the airgapped check still runs on it alone.

## Checks

```sh
cargo test --workspace     # 471 tests: the crypto, the modem, the store, the node and the app's commands
npm run check              # both pages in Chrome on the GPU
npm run check:airgapped    # the dependency gate
```

`npm run check` loads the front and the back of the machine in Chrome and fails on any page error, sideways scrolling at four laptop sizes, text under 12 px, layout shift, a drum row that doesn't settle on the right letters, or a network request. It walks through pairing, public notices and the first run on both the browser path and the app path, sets every setting and checks the cord lands in the right jack, and renders a drum turn offline to measure that the clicks don't clip or leak into the listening band.

CI runs the Rust tests and the airgapped check on Linux and on macOS, where the Keychain is. The page checks need a GPU-backed Chrome, so they run locally before a change counts as done. The pictures in this README are rendered by `node tools/media.cjs` from the same pages.

## Roadmap

- **Relay.** Laptops carry messages for others, so a message can hop across a building or a block to reach someone out of earshot.
- **Bluetooth on real Macs.** The standard build's Bluetooth is built and tested in code. Next is the two-Mac test and the six measurements it still owes.
- **Phones.** The same machine on iOS and Android.
- **A bridge to Bitchat**, so messages can cross into its Bluetooth mesh.

## License

MIT, copyright 2026 Connor Evans. See [LICENSE](LICENSE).

The engraving face is Marcellus SC, with Libre Caslon Text for the forms and Cutive Mono for the typewriter, all under the SIL Open Font License with their texts beside them in `app/ui/fonts`. three.js is vendored under the MIT License in `app/ui/vendor/three`. [NOTICE](NOTICE) lists everything bundled and its license.
