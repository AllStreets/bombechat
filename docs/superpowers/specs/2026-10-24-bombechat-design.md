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

- **Contacts** use `Noise_IK`. The initiator already holds the contact's key, so only that contact can read the first message, which names the initiator. A device that doesn't recognise the initiator sends nothing at all. Until it answers, nothing it sends reveals who it is. The answering side's view of the caller is unconfirmed until the caller's first message inside the session decrypts: before that, the first message may be a replay, or forged by someone who has stolen the answering device's key. The app does not show a contact as nearby until then.
- **Requests to connect** use `Noise_XN`. The requester's identity goes to the recipient. The recipient never sends a static key, so it stays anonymous until the person accepts. To accept, the recipient proves who it is by running a contact (`Noise_IK`) handshake to the requester. Its key certificate sent inside the request session would prove nothing, because certificates are public and the same every time, and the request handshake never uses the recipient's key. Whoever answers a request learns the requester's identity, so a request is only sent when the person chooses to send one. The requester sends its hello, then its current advertising token alone, inside the request session. On accept, the recipient sends its hello there, and once it is acknowledged runs the contact handshake on the same link naming that token; the requester answers it from the identity that hello claimed, only on that link, and takes them as a contact when the session confirms. Whatever becomes of a request short of an accept, the requester sees exactly what an ignored one shows. That covers a decline, being pushed out by the cap, a gap per link or per identity, a hello that isn't the requester's own, a requester who is already a contact, and a locked machine. The session is held quietly: its hello and anything after are acknowledged on the same random schedule, and it lasts until ten minutes after the hello. Nothing of ours goes back, and what it carried of the requester is forgotten. Tests check, for each of those paths decided 50 ms after the hello, that the requester sees the same bytes at the same moments as for an ignored request. Requests are held in memory only: at most 20, the oldest the person hasn't accepted pushed out first, at most one per link and per requester identity in ten minutes (a link that brought one gets no answer to another, so one stranger can't push out other people's), each for ten minutes. A determined stranger with many identities and many adverts can still fill the twenty; that is accepted, as no signal ties their radios together. While "new contacts need a key check" is on, requests are shown but neither sent nor accepted. An accept adds the requester at once, but the proof can fail to finish: the link can go, either side can rotate, or ten minutes can pass. Then the requester never takes us as a contact. Such a contact is kept in the store as not confirmed and shown that way, until a proven session with them confirms it. Handshakes still part way (no hello yet) are capped at four in all, with the oldest pushed out, so a stranger with four or more radios opening handshakes can keep pushing out a real requester's handshake before its hello arrives. That is accepted for the same reason as the twenty: nothing ties their radios together. Once a hello has arrived, the request is never pushed out this way.
- **Strangers nearby** are offered for a request by a short random label each, never an identity, while a link to their advert is up. The count of strangers is "seen in the last 15 minutes", by advert token, an upper bound: a stranger who rotated in that time counts twice, which errs on the safe side for someone deciding whether strangers are near.
- `Noise_XX`, Bitchat's pattern, is kept for a future bridge and not used by the link layer. IK and XN use the prologue "bombechat v1", which keeps their transcripts apart from every other use of the same patterns.
- Weak (low-order) X25519 keys from the other side are refused. Transport messages are sealed under explicit nonces (Noise's stateless transport, the nonce sent beside the ciphertext and never reused), so they decrypt in any order; an altered one, or one moved to another nonce, fails, and the link layer's replay window refuses a nonce it has already accepted.
- An IK first message carries, inside its encryption, the time it was sent and the advertising token of the device it was meant for. The answering device refuses it if the time is more than two minutes from its own clock either way, or if the token isn't its current one, one of the four before it (about an hour) or one of the three it will rotate to next (which only contacts are told, inside sessions, below), so a recorded message replayed later, or relayed to another device, gets no answer. Within the two minutes, the link layer refuses a first message whose ephemeral key it has already answered. It adds a key to that cache only after the message has been read and the caller is a contact, so strangers can't flush it.
- **Copying a token.** The token in message 1 stops a recorded message being replayed to some other device, but the token itself is public: its first six bytes are in the advert, and the whole of it goes to any device that connects and subscribes while it is current (unless this device heard that one under an earlier token), and to contacts inside their sessions. Someone near Bob can copy Bob's current token, advertise it near Alice, and pass Alice's message 1 to Bob and Bob's answer back as they happen, within the two minutes, for as long as Bob answers to that token (it and the four after it: about an hour). The relay can't read, alter or answer anything itself, but Alice and Bob each see the other as nearby when they aren't, and the relay learns that the two devices are contacts from the traffic between them. Bluetooth has no way to measure distance, so Bombechat accepts this gap, as other nearby-device protocols without distance bounding do. Rotating the token every 15 minutes, and answering to the last five only, limits how long one copy works. Alice names a token learned from Bob inside a session only on a link to an advert showing that token's tag, so a device showing some other tag, or withholding its token, gets no hello worth relaying.
- **The link layer.** Our own service `8AAC3E33-A919-4002-96A8-14E1A873404F` and characteristic `CC26A52C-3F93-439B-83DC-2301C13CA327`, never Bitchat's. Every link message carries a one-byte kind (contact hello, request, handshake, sealed) that any device may send, so the kind says what a message does, not who sent it. Whether a sealed message is an envelope, an acknowledgement, a keepalive or a token message, and how long the envelope is, are inside the encryption, padded to 256, 512, 1024 or 2048 bytes or whole 4 KiB steps (keepalives, token messages and acknowledgements to the size of a short private message). Every sealed message carries the receiver's cumulative acknowledgement inside the encryption, so an acknowledgement can't be forged, and lost writes are sent again until acknowledged. Envelopes and keepalives both draw an acknowledgement, which waits a random 0 to 400 ms unless something else carries it sooner, so whether and how soon a reply follows doesn't tell the two apart. A device with no contacts sends decoy contact hellos, and every round of hellos to a token is exactly eight, real ones padded out with decoys of the same size, so the hellos don't tell how many contacts a device has. Each token gets one round, plus one resend; a contact whose session ends is redialled with backoff outside the rounds, and a contact whose link the radio lost is redialled at once on the next link to the same advert.
- **Links and tokens.** This device closes a link when it has had nothing on it for two minutes, when this device opened it more than one token ago, in Contacts mode when it shows a stranger's token (or no token) whose round is over, and, at this device's own rotation, when it carries no confirmed contact session. On a link this device opened, closing ends the connection. On a link another device opened to it, it can't: macOS gives a peripheral no way to end a connection. Closing only stops this device using the link, and a stranger may hold such a connection as long as it likes, across rotations and after the app's Bluetooth is switched off, and so see that this Mac is still there. What it doesn't get is a new token. Each connecting device (as macOS identifies it, by its address) is given the whole token at most once, and only if it was first heard while that token was current. Every other subscription still gets a link, but its first notification is a one-byte mark saying the token is withheld, carrying no tag and nothing else, so a held connection that subscribes again after a rotation learns nothing of the new advert. Unsubscribing and subscribing again gets only the mark again. Devices are remembered while subscribed and for an hour after their last traffic on a link that was given the token (traffic on a link without it doesn't count), so one that holds a connection silent and unsubscribed for longer is taken for new. So any device first heard during an earlier token gets no token from this Mac until its address changes (macOS then gives it a new identifier) or it has been silent for an hour. Two Macs signed in to the same Apple ID may keep the same identifier for each other however their addresses change, so each may withhold its token from the other in both directions. Nothing keyed to that identifier lets anyone past this, because anyone who copies an address copies the identifier.
- **Tokens between contacts.** Contacts don't need the token in the clear. Each device draws its tokens three rotations ahead. Inside every confirmed contact session each side sends its token schedule, its current token and the next three in the order it will use them, in a token message padded like every other sealed message: when the session is confirmed, at once at each rotation, and in place of every keepalive (so a lost one is made good within 20 seconds). When a contact is removed only the tokens ahead change, and the remaining sessions carry the new schedule with their next keepalives, so a removal sends nothing out of rhythm. Each side keeps, in memory only, the latest schedule each contact gave. On every link this device opens, the advert's tag (the token's first six bytes, as the scan saw it in the advert, never from the link) is matched against those schedules first; a match is that contact's current advert, even if it has rotated up to three times since they last talked, and the hellos on that link name that token. Without a match, a token this device was given for an advert with that tag on a link it lost comes next, then the token the link was given, if it was given one; with none of these the round is all decoys, eight as always. A removed contact's sessions end at once, so it is told nothing more, and what it gave is forgotten; the three tokens it was told ahead are drawn again, and the contacts that remain are told the new schedule. A device answers to its current token, the four before it and the three it has told ahead, and a future token becomes current only when the device rotates to it. The cost: two such Macs that were apart while each rotated four or more times recognise neither new advert and are each refused the other's token, so they meet again only when one of them is taken for new, an hour after its last traffic on a link given the token. Apart through three rotations or fewer each, they meet again as soon as they are in range.
- **What an observer can and can't link.** Anyone in range sees our service UUID (every advertising Bombechat device has it), each token's tag for as long as the token lasts, and when one advert stops and another starts. Bluetooth links here aren't encrypted at the link level, so whoever sniffs a connection sees the whole token given on it, and sees whether a connecting device was given the token or had it withheld. **Accepted linker: "seen before".** Withholding is visible to the device that is refused it. An active tracker that connected to this Mac once from an address it keeps fixed (a programmable controller makes that free) can later, after this Mac's rotations or up to an hour after it was last given the token, connect to each Bombechat advert in range from that address and recognise this Mac as the one that refuses it the token, where a device that never saw it serves a token. Like timing, this is accepted: it can't be removed while a newcomer is served the token and a remembered device isn't (a decoy can always be told from the token by asking again from a fresh address, and a decoy carrying the advert's tag would hand the new advert to a held connection). It needs an active tracker that already met this Mac, it gives no token, and it tells nothing to anyone else. A sniffer of the refused device's connection learns the same, and where that device's identifier is stable across its address changes (measurement 6 below), that this Mac heard it under each of those addresses. A contact session's sealed messages don't say what they are, but every live contact session sends one at the moment of this device's rotation, which marks such a link, already linkable because it outlives the rotation (next bullet). What a stranger can't do after this device's rotation, holding a connection or reconnecting with a copied identifier: learn the new token, or which advert is this device's now. It gets no token in the clear and no tag (the withheld mark carries none), its contact hello gets no answer (it isn't a contact, whatever token it names), and no session tells it. A removed contact keeps the token it last heard, so it may recognise this Mac's advert until that token changes, at most 15 minutes, and its hellos get no answer.
- **A contact's link outlives a rotation.** A link that carries a confirmed contact session stays open across this device's rotation, for at most one more token (the contact closes a link it opened once it is older than one token). Someone sniffing that connection sees it carry on, keepalives included, so they can link the old token to the new one. This is accepted rather than closing and redialling at each rotation: a sniffer would see the old connection end and a new one between the same two radios start a second later, which links the tokens just as well; it would cost a round of eight hellos and a gap in delivery every rotation; and it only concerns two devices that are each other's contacts and in session at the time.
- **The advert and the address (macOS).** What is known: macOS lets an app advertise only a local name and service UUIDs (no service data), and keeps a local name of at most 8 bytes beside a 128-bit service UUID (Apple's documentation and developer forum reports). So the advert is our service and an 8-character tag (the token's first six bytes, URL-safe base64), and the whole token is the first notification a central receives after subscribing. An app can't choose or rotate the Bluetooth address; macOS rotates it on its own timer. At each new token the radio stops advertising and starts again. Our service UUID marks every advertising Bombechat device as one, and an observer can link an old token to the new one by timing (one advert with our service stops as another starts) and through a contact's connection carried across the rotation (above).
- **Measured in the two-Mac test, not yet known.** These are measured by the ignored hardware test `two_macs_exchange_4_kb_both_ways_then_stop` (its comment has the steps), and this list is rewritten from the results:
  - whether restarting the advert gives it a new address. If it doesn't, a continuous observer links each token to the next through the address;
  - whether macOS changes the address while a token lasts. If it does, the tag, which lasts the token's life, links the old address to the new one, undoing that rotation for this Mac's other adverts on the same address too. With the first, a continuous observer could chain every token and every address;
  - whether a connected device can read the Mac's Bluetooth (GAP) device name. On macOS that is likely the computer's name, which anyone in Bluetooth range can read whatever Bombechat does, so people are told to give the Mac a plain name;
  - whether stopping ends a connection another device opened to this Mac. "At Off, no connections" is certain only for the connections this device opened.
  - whether the other Mac's identifier as a central (what this Mac's peripheral side keys its memory on) changes when that Mac's address rotates (watched for at least 20 minutes), and whether it stays the same when this Mac's managers are made anew by a stop and start while the other holds its connection. The first decides how long a device goes without the token in practice; the second decides whether a held connection gets nothing new after the app's Bluetooth is switched off and on;
  - whether a remote's identifier is the same in both roles (the one its adverts are reported under and the one its subscriptions are), and whether it changes when its address changes. If it is the same, an advert's address names the device as a central too; if it stays the same across address changes (likely between Macs on the same Apple ID), this Mac's memory of it lasts across its address changes, so it goes without the token for that hour, and a sniffer can link its addresses through the withheld mark, as above.
- Envelopes go only to confirmed contact sessions: a private one to the sessions of its recipients, a broadcast to every one. Strangers get none, even in Open mode (whether broadcasts should reach them is left for later).
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
