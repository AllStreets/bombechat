# Bombechat design

24 October 2026. Status: design, awaiting approval.

## What it is and why

Bombechat is an encrypted messenger that never uses the internet or the phone network. Messages travel between nearby devices by sound, by light, or by Bluetooth, and can be passed from device to device to reach people further away.

It exists for two situations. The first is a disaster that takes out power, cell towers and the internet, where people nearby still need to reach each other and public notices ("water at the church on Fifth") need to spread. The second is private conversation where you would rather no network operator is involved at all. In both, only the people a message is sealed to can read it.

It is also a design-engineering portfolio piece. The interface has to be good enough that a design engineer stops to look, and it must never read as generated.

## Goals

- Send and receive short text messages between laptops with no internet, using sound, light, and (on the standard build) Bluetooth.
- Encryption on the level of Signal, using established, audited primitives. Only intended recipients can read a message, including messages carried by other people's devices.
- An airgapped build that contains no Bluetooth code at all, so it never needs, asks for or touches a radio.
- Store-and-forward relay: devices carry sealed messages for others and pass them on, within a hop limit and hold time.
- Public broadcasts that anyone in range can read, signed so they can't be forged.
- An encrypted contacts list on the device.
- The Bombe interface from the approved mockups, built to the same quality bar and checked the same way.

## Non-goals

- Hiding that a message was sent. Sound and light are noticeable. "Covert" here means the content can't be read without the key.
- Voice, images or files in the first version. The channels are slow; text first.
- Phones in the first version. Laptops first, because the airgapped laptop is the hard case. Phones follow.
- Any server, account, phone number or cloud component, ever.
- Amateur radio bands. Encryption isn't allowed there.

## Anti-goals

These would make it a failure even if it technically worked:

- A plaintext message, a key, or a contact ever leaving the device unencrypted, or being written to disk unencrypted.
- The airgapped build containing Bluetooth code, even switched off.
- Home-made cryptography, or Enigma-style or Caesar-style ciphers in place of real encryption. The Enigma feel is the interface, never the maths.
- Claiming to be "stronger than Signal". Say it uses the same proven primitives.
- An interface that looks like a template or an AI default (see the No AI Look list: no eyebrow caps labels, no card kits, no glows, no italic captions under everything).
- Drum and plug sounds leaking into the band the app listens on.

## Decisions already made

These came from the person commissioning it and are fixed.

- **Name:** Bombechat. The machine is "Machine No. 1" everywhere.
- **Channels:** sound, light, Bluetooth. Bluetooth is the everyday channel on the standard build. Sound and light are what make it distinctive, and are the only channels on the airgapped build.
- **Airgapped build:** Bluetooth is absent from the code, not disabled. Shown in the interface as a blanking plate marked "Not fitted".
- **Hopping:** messages can pass device to device to extend range.
- **Keys:** people will usually have met and swapped keys in person, by showing a light code to each other's camera. On the standard build a stranger in Bluetooth range can send a request to connect, LinkedIn style; nothing is exchanged unless it's accepted.
- **Contacts:** stored encrypted on the device.
- **Covert:** content confidentiality only, as above.
- **Platform order:** laptops first, then phones.
- **Interface:** the Bombe direction (chosen over a "Cyber" spectrogram direction).
  - **Front:** a photorealistic drum bank, three rows of twelve drums for key, heard and plain text, turning as each frame arrives, with ratchet clicks. A printed message sheet with teleprinter tape for incoming messages and typed text for your own. A pencil key-check menu on squared paper. Rotary channel switches (Sound: Off, Listen, Send. Light: Off, Read, Flash). A VU meter, and a reply that waits for the room to go quiet before playing.
  - **Settings:** the back of the machine, photorealistic, in WebGL. Every setting is a patch cord in a row of jacks. Changing one, on the card or by dragging the plug, moves the real cord, and the camera follows. Click anywhere on the machine or press Escape for the full view.
  - The front and back share one set of materials and lights, so they read as one machine.
- **Process:** every change is built, tested with the check scripts and screenshots, critiqued, and iterated until it meets the bar before it's shown. The checks exercise every working part (the message sheet, replies, the key check, switches, settings), not only how it looks. Work is pushed to github.com/AllStreets/bombechat after each major task.

The mockups in `mockups/` are the reference for the interface: `bombechat.html` (front), `settings.html` (back), `machine-kit.js` (shared machine). Serve them with `npm run serve`.

## How it works

### Shape of the app

A desktop app built with Tauri. A Rust core does everything that touches keys, audio, camera, radio and storage. The web front end is the interface from the mockups and never sees a private key. Two builds come from one codebase: `standard` and `airgapped`. Every channel implements one transport interface (send frame, receive frame, peers in range). Bluetooth sits behind a Cargo feature that the airgapped build doesn't enable, and a CI check fails the airgapped build if any Bluetooth crate appears in its dependency tree or binary.

### Messages

Every message is a sealed envelope.

- **Private message:** encrypted to one or more recipients' keys and signed by the sender. Carriers can see only an opaque ID, an expiry time and a hop count.
- **Public broadcast:** plain text, signed by the sender's key, shown with the signer's short fingerprint.
- **Relay control:** small messages such as "I already have this ID", to avoid repeating messages.

Messages are split into frames for transmission. Each frame is protected by forward error correction and a checksum, so a few lost frames can be repaired and a corrupted frame is detected rather than shown. The interface shows frames landing, as the mockups do.

### Cryptography

Use established primitives and libraries, no new constructions:

- X25519 for key agreement.
- Ed25519 for signatures.
- ChaCha20-Poly1305 (or XChaCha20-Poly1305) for encryption.
- HKDF for key derivation.
- Argon2id for the passphrase.

When two devices can talk both ways, they use a Noise-protocol session for forward secrecy, as Bitchat does. When a message has to be stored and forwarded with no live session, it is sealed to the recipient's static key with a fresh ephemeral key per message. The forward-secrecy limits of that case are documented plainly.

Identity is a long-term Ed25519 and X25519 pair per device. The safety number shown in the key check is derived from both parties' public keys, the same idea as Signal's.

### Threat model: Bluetooth sessions

Live Bluetooth sessions use Noise with `ChaChaPoly` and `SHA256` over X25519, through the `snow` library. Each device's Noise static key is its identity's X25519 key, and a static key certificate (its Ed25519 key signing "bombechat static key v1" and the X25519 key, made once) travels with the static key, so the other side learns a whole identity it can match against contacts and the safety number. The certificate says nothing about any one session, so sessions are as deniable as plain Noise: neither side can prove to anyone else that the other took part.

- **Contacts** use `Noise_IK`. The initiator already holds the contact's key, so only that contact can read the first message, which names the initiator. A device that doesn't recognise the initiator sends nothing at all. Until it answers, nothing it sends reveals who it is.
- **Requests to connect** use `Noise_XN`. The requester's identity goes to the recipient. The recipient never sends a static key, so it stays anonymous until the person accepts; then its hello goes inside the session. Whoever answers a request learns the requester's identity, so a request is only sent when the person chooses to send one.
- `Noise_XX`, Bitchat's pattern, is kept for a future bridge and not used by the link layer. IK and XN use the prologue "bombechat v1", which keeps their transcripts apart from every other use of the same patterns.
- Weak (low-order) X25519 keys from the other side are refused. Transport messages must arrive in order, and a replayed, reordered or altered one fails.
- An IK first message can be replayed. The answer is useless to whoever replayed it, but the act of answering shows that the contact is in range. The link layer has to limit this, for example by refusing first messages it has already seen.
- Memory: Bombechat's own copies of secret keys are wiped when dropped, but `snow` 0.10 does not wipe its copies of the static and ephemeral secrets, the chaining key or the transport keys. Someone who can later read the app's memory or a core dump might recover a finished session's keys. Closing that gap means a small `snow` resolver that wipes on drop, using the same primitives.

### Keys and contacts

- **In person:** each device shows its key as a flashing light code (and a printed-tape style code on screen). The other device reads it with its camera. Both then show the same safety number to compare, and the contact is marked checked.
- **Requests from strangers (standard build only):** a nearby device can send a request over Bluetooth containing its public key and a display name. You see it and can accept or decline. Accepting adds them as unchecked until you compare keys in person. A setting can require an in-person check before any contact is added.
- **Contacts and history:** stored in an encrypted local database. The key comes from the passphrase through Argon2id, and is optionally kept in the OS keychain when the passphrase lock is off.

### Channels

- **Sound:** multi-tone frequency-shift keying in 17.6 to 19.8 kHz by default, with an audible fallback band for speakers that can't reach that high. Expect tens of bytes per second, not hundreds. The rate shown in the interface must be the measured one. The receiver runs through the microphone continuously when Listen is on, and never transmits unless Send is selected. Sending waits for the room to fall below the "Send under" level, then repeats as configured.
- **Light:** the sender shows an animated sequence of codes on its screen and the receiver reads them with its camera. This is much faster than flashing a single area. The light code for pairing uses the same channel.
- **Bluetooth (standard build only):** BLE advertising and connections for discovery, requests to connect, and message exchange, following the approach Bitchat takes for its mesh.

### Relay

Devices that have "Carry for others" on keep sealed messages they can't read, for up to the hold time. They pass them to any device they meet, across any channel they share, while the hop count is under the limit. Duplicates are suppressed by message ID. Storage for carried messages has a size cap, and the oldest are dropped first. Carried messages are counted on the front panel's counter.

### Interface

The two mockup pages become the app's two views. The real app wires them to the Rust core through Tauri commands and events, replacing the mockups' simulated messages, room level and frames. Everything in the mockups that is simulated (the looping message, the drifting room meter) becomes real data, and anything that can't be real is removed rather than faked. If WebGL isn't available, the front falls back to the 2D drums and the settings still work from the wiring schedule card. Reduced motion, keyboard use, screen-reader labels and the 12px minimum text size stay as they are in the mockups.

### Sound of the machine

Drum clicks and plug sounds are synthesised, low-passed hard above 7 kHz, and follow the Machine sound and Machine volume settings. They're also muted while the app is receiving by sound, so the machine never talks over the channel it's listening to.

## Bitchat

Bombechat does not need to talk to Bitchat in the first version. It is a planned future update, decided by the commissioner. So the design keeps the door open: the message envelope and the sealing are separate from the channel framing, and Bluetooth sits behind a transport interface. A later Bitchat bridge can then translate Bitchat's packets and Noise sessions to and from Bombechat envelopes without changing sound, light or storage. Nothing in the first version should make that harder. For example, avoid fixing BLE service identifiers or packet sizes in a way that clashes with Bitchat's.

## Calls made during review

A builder read this document and asked questions. These were settled without going back to the commissioner, and are marked as the designer's calls so they can be checked.

- **What the drum rows show (designer's call).** The rows are a truthful picture of each frame. Plain shows the decrypted text. Heard shows the received ciphertext bytes, mapped onto letters. Key shows that frame's keystream bytes, mapped the same way. The keystream for a frame that is already decrypted reveals nothing beyond what Plain and Heard together already show, and it is not a key: no private key or session key ever reaches the interface. The "plain row settles on the right letters" check stays. The letter mapping replaces the mockup's Vigenère arithmetic.
- **Screens the mockups don't show (designer's call).** These are: contacts and conversations, several recipients, writing a broadcast, the pairing flow, passphrase unlock and first run, and the standard build's Bluetooth panel and request inbox. Each one is added as a view or overlay in the same machine language. Each gets a mockup first, goes through the build, check and iterate loop, and is shown to the commissioner before it's wired to the core.
- **Safety number length (designer's call).** Signal level means a full-strength number: 60 digits in 12 groups of five, derived as Signal derives it. The light code carries the full public key, not a short fingerprint. The key-check card is redesigned to fit the longer number, for example as a pencilled grid.
- **What airgapped means (designer's call).** No Bluetooth, and no networking of any kind. The airgapped build contains no updater, opens no sockets, and its webview is blocked from the network by content security policy. The CI check scans for network and Bluetooth crates and symbols. Releases are signed, with a published checksum that can be checked on another machine before the file is copied across on removable media. Reproducible builds are a goal, not a gate, for the first release.
- **Broadcast trust (designer's call).** Broadcasts from checked contacts are shown normally. Broadcasts from unknown signers are shown marked "unverified", with the signer's fingerprint. A setting can hide them. Carriers relay at most a fixed number of broadcasts per signer per hour, and drop a signer's broadcasts once that rate is exceeded.
- **Listen, Send and receipts (designer's call).** Send implies Listen, so the Sound switch reads Off, Listen, and Listen and send; Light works the same way. Sound and light are two-way when both sides have Send on, and one-way otherwise. Delivery receipts ("heard by Marisol") are real, are sent only to contacts, and can be turned off in Privacy, because a receipt reveals that you're nearby.

## Constraints

- **Hardware:** runs on ordinary laptops (macOS first, then Windows and Linux) using built-in speakers, microphone and camera. No extra hardware required.
- **Legal:** ultrasonic and light signalling are unlicensed. Bluetooth uses standard BLE. No amateur bands. Open-source cryptography is publicly available, which matters for export rules.
- **Fonts:** Copperplate ships with macOS only. The release needs a licensed engraving face or a close open alternative, embedded.
- **Permissions:** the microphone and camera need OS permissions. The app explains why, and asks only when the user turns a channel on.
- **Battery and noise:** listening continuously costs power. The design shows when the microphone is open.

## Left to the builder

- Exact modem parameters: tone count, symbol rate, error correction scheme, framing and sync, chosen by measuring real laptops in real rooms.
- The light code format and frame rate.
- Database engine and on-disk layout, within "encrypted at rest".
- The Noise pattern and the exact sealed-message construction, within the primitives above.
- Internal crate structure, and how the mockup pages are split into components.
- Test strategy beyond the existing check scripts. The modem needs recorded-audio fixtures. The cryptography needs known-answer tests and fuzzing of the parsers.
- Order of work, as long as the airgapped laptop path (sound and light, no Bluetooth) works end to end first.
