# Bombechat Plan 1: the sound path, end to end

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two airgapped laptops pair by sound and exchange encrypted messages by sound, through the Bombe interface, with no Bluetooth and no network code in the build.

**Architecture:** A Cargo workspace. `bombechat-core` holds identity, envelopes and sealing. `bombechat-modem` holds framing, error correction and the FSK modem. `bombechat-store` holds the encrypted database. `bombechat-node` orchestrates sending and receiving behind a `Transport` trait. The Tauri app in `app/` exposes the node to the web interface, which is moved from `mockups/` into `app/ui/`. A `bridge.js` talks to Tauri in the app and to a simulator in a plain browser, so the existing visual checks keep running.

**Tech Stack:**
- Rust (stable), Tauri 2.
- Crypto: `x25519-dalek`, `ed25519-dalek`, `chacha20poly1305` (XChaCha20-Poly1305 and its `stream` module), `hkdf`, `sha2`, `argon2`, `rand_core` with `OsRng`.
- Modem and audio: `reed-solomon` (byte error correction), `cpal` (audio), `hound` (WAV fixtures in tests).
- Storage: `rusqlite` with `bundled-sqlcipher`.
- Tests: `proptest`.
- Front end: three.js 0.170 (vendored), Playwright for checks.

**Spec:** `docs/superpowers/specs/2026-10-24-bombechat-design.md`

## Global Constraints

- The airgapped build contains no Bluetooth code and no networking of any kind: no updater, no sockets, and webview CSP `connect-src 'none'`.
- Crypto primitives are X25519, Ed25519, XChaCha20-Poly1305, HKDF-SHA256 and Argon2id only. No home-made constructions.
- Never claim "stronger than Signal". Use "the same proven primitives".
- A plaintext message, key or contact is never written to disk unencrypted and never leaves the device unencrypted.
- No private key or session key ever crosses into the web front end.
- The sound band defaults to 17.6 to 19.8 kHz, with an audible fallback band. The rate shown in the interface is the measured rate.
- Machine sounds are low-passed above 7 kHz and muted while receiving by sound.
- Safety numbers are 60 digits in 12 groups of five.
- The machine is "Machine No. 1" everywhere.
- Text in the interface: sentence case, no em dashes, no emojis, nothing under 12px, no italic captions, no glows.
- Every change runs its checks (cargo tests, `npm run check`), is critiqued from screenshots, and is iterated before it's shown. Push to `AllStreets/bombechat` after each completed task group.
- Don't fix BLE identifiers or packet sizes in ways that would clash with Bitchat. A Bitchat bridge is a planned future update.

## Review Focus

1. **Malformed input from the air.** Truncated, oversized or bit-flipped frames and envelopes must be rejected without panicking. Owner: Tasks 3 and 5 (proptest and fuzz-style tests).
2. **Two senders at once.** Frames from two transmissions interleave in the room. Each message must reassemble separately, by tag. Owner: Task 5.
3. **Messages arriving while the store is locked.** They must be held sealed in memory and delivered after unlock, not dropped. Owner: Task 9.
4. **Microphone or speaker unavailable or permission denied.** The Sound channel shows as unavailable, and nothing crashes or spins. Owner: Task 7.
5. **Clock skew between laptops.** Expiry must tolerate senders up to 24 hours off, and show order is by arrival. Owner: Task 4.

---

### Task 1: Workspace, two builds, and the airgapped gate

**Files:**
- Create: `Cargo.toml` (workspace), `crates/core/Cargo.toml`, `crates/modem/Cargo.toml`, `crates/store/Cargo.toml`, `crates/node/Cargo.toml`, each with `src/lib.rs`
- Create: `tools/check-airgapped.sh`, `.github/workflows/ci.yml`
- Modify: `package.json` (add `"check:airgapped": "bash tools/check-airgapped.sh"`)

**Interfaces:**
- Produces:
  - The Cargo feature `bluetooth` on `bombechat-node`, off by default.
  - The `airgapped` build means `--no-default-features`.
  - The script exits 0 when clean, and non-zero with the offending crate named.

- [ ] **Step 1:** Write `tools/check-airgapped.sh`:
  - Run `cargo tree --workspace --no-default-features -e normal --prefix none`.
  - Fail if any line matches `btleplug|bluer|bluest|corebluetooth|reqwest|hyper|ureq|tauri-plugin-updater|tauri-plugin-http|tokio-tungstenite`.
  - On macOS, once the app exists (Task 11), also run `otool -L` on the built binary and fail on `CoreBluetooth|IOBluetooth`.
- [ ] **Step 2:** Run `bash tools/check-airgapped.sh`. Expected: exit 0, "airgapped: clean".
- [ ] **Step 3:** Add a negative check to CI: add `btleplug` as an optional dependency of `bombechat-node` under the `bluetooth` feature, and assert the script fails when `--features bluetooth` is passed. Use a `CHECK_FEATURES` env var that the script appends.
- [ ] **Step 4:** Run `CHECK_FEATURES=bluetooth bash tools/check-airgapped.sh`. Expected: exit 1 naming `btleplug`.
- [ ] **Step 5:** CI runs:
  - `cargo test --workspace`
  - the airgapped check, both positive and negative
  - `npm run check:front`

  The GPU settings check stays local-only, and its README note says so.
- [ ] **Step 6:** Commit: `chore: workspace, feature-gated bluetooth, airgapped dependency gate`.

### Task 2: Identity and safety numbers

**Files:**
- Create: `crates/core/src/identity.rs`, `crates/core/tests/identity.rs`

**Interfaces:**
- Produces:
  - `pub struct Identity` with `generate() -> Identity`, `from_secret_bytes(&[u8; 64]) -> Identity`, `secret_bytes(&self) -> Zeroizing<[u8; 64]>` and `public(&self) -> PublicIdentity`.
  - `pub struct PublicIdentity { pub sign: [u8; 32], pub dh: [u8; 32] }` with `fingerprint(&self) -> [u8; 8]` (the first 8 bytes of SHA-256 over sign‖dh) and `short_hex(&self) -> String` (4 uppercase hex characters, as in "7F3A").
  - `pub fn safety_number(a: &PublicIdentity, b: &PublicIdentity) -> String`.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn safety_number_is_symmetric() { let (a,b)=(Identity::generate().public(),Identity::generate().public()); assert_eq!(safety_number(&a,&b), safety_number(&b,&a)); }
#[test] fn safety_number_is_60_digits_in_12_groups() { let s=safety_number(&Identity::generate().public(),&Identity::generate().public()); let g:Vec<&str>=s.split(' ').collect(); assert_eq!(g.len(),12); assert!(g.iter().all(|x| x.len()==5 && x.chars().all(|c| c.is_ascii_digit()))); }
#[test] fn one_bit_changes_the_number() { let a=Identity::generate().public(); let mut b=Identity::generate().public(); let s1=safety_number(&a,&b); b.dh[0]^=1; assert_ne!(s1,safety_number(&a,&b)); }
#[test] fn secret_roundtrip() { let i=Identity::generate(); assert_eq!(Identity::from_secret_bytes(&i.secret_bytes()).public(), i.public()); }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-core --test identity`. Expected: FAIL to compile.
- [ ] **Step 3:** Implement. The safety number follows Signal's fingerprint scheme:
  - For each side, hash `version(0u16 BE) ‖ sign ‖ dh` with SHA-512 iterated 5200 times, re-appending `sign ‖ dh` each round.
  - Take the first 30 bytes as six 5-byte chunks. Each chunk becomes `u40 mod 100000`, zero-padded to 5 digits.
  - The 30 digits of each side are concatenated in byte order of the two `fingerprint()`s, lower first.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(core): identities and 60-digit safety numbers`.

### Task 3: Envelope wire format

**Files:**
- Create: `crates/core/src/envelope.rs`, `crates/core/tests/envelope.rs`

**Interfaces:**
- Produces:
  - `pub enum Kind { Private = 1, Public = 2, Control = 3 }`
  - `pub struct Envelope { pub kind: Kind, pub id: [u8; 16], pub expires_min: u32, pub hops: u8, pub body: Vec<u8> }`
  - `encode(&self) -> Vec<u8>`
  - `decode(&[u8]) -> Result<Envelope, WireError>`
  - `pub const MAX_ENVELOPE: usize = 4096;`
  - `WireError { Truncated, UnknownVersion(u8), UnknownKind(u8), TooLarge }`
- Wire layout: `version u8 = 1 | kind u8 | id [16] | expires_min u32 BE (minutes since Unix epoch) | hops u8 | body_len u16 BE | body`.

- [ ] **Step 1: Failing tests**

```rust
proptest! { #[test] fn roundtrip(kind in 1u8..=3, id: [u8;16], exp: u32, hops: u8, body in proptest::collection::vec(any::<u8>(), 0..4000)) { let e=Envelope{kind:Kind::try_from(kind).unwrap(),id,expires_min:exp,hops,body}; prop_assert_eq!(Envelope::decode(&e.encode()).unwrap(), e); } }
proptest! { #[test] fn never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..5000)) { let _ = Envelope::decode(&bytes); } }
#[test] fn rejects_truncated() { let e=sample(); let b=e.encode(); assert_eq!(Envelope::decode(&b[..b.len()-1]), Err(WireError::Truncated)); }
#[test] fn rejects_version_2() { let mut b=sample().encode(); b[0]=2; assert_eq!(Envelope::decode(&b), Err(WireError::UnknownVersion(2))); }
#[test] fn rejects_oversize() { let mut e=sample(); e.body=vec![0;MAX_ENVELOPE]; assert!(matches!(Envelope::decode(&e.encode()), Err(WireError::TooLarge))); }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-core --test envelope`. Expected: FAIL.
- [ ] **Step 3:** Implement. `hops` and `expires_min` sit outside the encryption on purpose, so carriers can enforce them.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(core): envelope wire format`.

### Task 4: Sealing private messages, signing public ones

**Files:**
- Create: `crates/core/src/seal.rs`, `crates/core/tests/seal.rs`

**Interfaces:**
- Consumes: `Identity`, `PublicIdentity` (Task 2); `Envelope`, `Kind` (Task 3).
- Produces:
  - `pub const CHUNK: usize = 48;`
  - `pub fn seal_private(from: &Identity, to: &[PublicIdentity], text: &str, now_min: u32, ttl_min: u32) -> Envelope`
  - `pub fn open_private(me: &Identity, env: &Envelope) -> Result<Opened, OpenError>`
  - `pub struct Opened { pub sender: PublicIdentity, pub text: String, pub sent_min: u32 }`
  - `pub struct ChunkOpener` with `ChunkOpener::start(me: &Identity, header: &[u8]) -> Result<ChunkOpener, OpenError>` and `next(&mut self, chunk: &[u8], last: bool) -> Result<ChunkView, OpenError>`
  - `pub struct ChunkView { pub plain: Vec<u8>, pub heard: Vec<u8>, pub key: Vec<u8> }`. Here `key` is the chunk's keystream (`heard` XOR `plain`, tag excluded).
  - `pub fn split_private_body(body: &[u8]) -> Result<(&[u8] /*header*/, Vec<&[u8]> /*chunks*/), OpenError>`
  - `pub fn seal_public(from: &Identity, text: &str, now_min: u32, ttl_min: u32) -> Envelope`
  - `pub fn open_public(env: &Envelope) -> Result<(PublicIdentity, String, u32), OpenError>`
  - `OpenError { NotForMe, Tampered, BadSignature, Expired, Malformed }`
- Construction:
  - A random 32-byte content key.
  - Per recipient: X25519 between a fresh ephemeral key and the recipient's `dh`, HKDF-SHA256 with info `"bombechat seal v1"`, then XChaCha20-Poly1305 wrapping of the content key. Each wrap is tagged with the recipient `fingerprint()`.
  - The inner plaintext is `sender sign ‖ sender dh ‖ sent_min u32 ‖ text utf8 ‖ Ed25519 signature`. The signature covers the envelope id, expiry, recipient fingerprints and everything before it.
  - The inner plaintext is encrypted with the STREAM construction (`chacha20poly1305::aead::stream`, big-endian 32-bit counter) in `CHUNK`-byte segments, so each chunk can be authenticated as it arrives. The signature is checked when the last chunk lands.
  - `open_private` refuses an envelope with `expires_min < now_min - 1440`, which allows 24 hours of clock skew.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn two_recipients_both_open() { let (a,b,c)=(Identity::generate(),Identity::generate(),Identity::generate()); let e=seal_private(&a,&[b.public(),c.public()],"Water is at the church on Fifth.",100,1440); assert_eq!(open_private(&b,&e).unwrap().text,"Water is at the church on Fifth."); assert_eq!(open_private(&c,&e).unwrap().sender,a.public()); }
#[test] fn stranger_cannot_open() { let (a,b,x)=(Identity::generate(),Identity::generate(),Identity::generate()); let e=seal_private(&a,&[b.public()],"hi",100,1440); assert_eq!(open_private(&x,&e), Err(OpenError::NotForMe)); }
#[test] fn any_flipped_byte_is_tampered() { for i in 0..e_len() { let mut e=fixture(); e.body[i]^=1; assert!(matches!(open_private(&bob(),&e), Err(OpenError::Tampered|OpenError::NotForMe|OpenError::Malformed))); } }
#[test] fn chunk_by_chunk_matches_whole() { /* open every chunk with ChunkOpener; concat(view.plain) == inner plaintext prefix; view.heard XOR view.key == view.plain for each */ }
#[test] fn public_signature_checked() { let a=Identity::generate(); let mut e=seal_public(&a,"Shelter open at Benito Juarez High.",100,1440); assert_eq!(open_public(&e).unwrap().1,"Shelter open at Benito Juarez High."); e.body[40]^=1; assert_eq!(open_public(&e).unwrap_err(), OpenError::BadSignature); }
#[test] fn skew_tolerated_but_expiry_enforced() { /* expires 1000 opens at now 2000 (skew window), refused at now 2500 with Expired */ }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-core --test seal`. Expected: FAIL.
- [ ] **Step 3:** Implement with the crates in Tech Stack. Zeroize content keys and ephemeral secrets on drop.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(core): sealed private messages with per-chunk authentication, signed broadcasts`.

### Task 5: Frames, error correction and reassembly

**Files:**
- Create: `crates/modem/src/frames.rs`, `crates/modem/tests/frames.rs`

**Interfaces:**
- Produces:
  - `pub const FRAME_DATA: usize = 48;` and `pub const PARITY: usize = 16;`
  - `pub fn split(envelope: &[u8], tag: u16) -> Vec<Vec<u8>>`. Each item is the on-air frame: `tag u16 | seq u8 | total u8 | len u8 | data[len]`, then Reed-Solomon parity of `PARITY` bytes.
  - `pub struct Reassembler` with `new() -> Self`, `push(&mut self, frame: &[u8], now_ms: u64) -> Vec<Event>` and `expire(&mut self, now_ms: u64)`.
  - `pub enum Event { FrameOk { tag: u16, seq: u8, total: u8 }, FrameBad, Complete { tag: u16, bytes: Vec<u8> } }`
- Rules:
  - Duplicate frames (from repeats) are ignored.
  - Partial messages are dropped after 10 minutes without a new frame.
  - Up to `PARITY / 2` corrupted bytes per frame are corrected. More than that gives `FrameBad`.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn roundtrip_in_order() { let env=vec![7u8;300]; let mut r=Reassembler::new(); let mut out=None; for f in split(&env,9) { for e in r.push(&f,0) { if let Event::Complete{bytes,..}=e { out=Some(bytes) } } } assert_eq!(out.unwrap(), env); }
#[test] fn out_of_order_and_duplicates() { /* reverse order, every frame twice: exactly one Complete */ }
#[test] fn corrects_eight_bad_bytes() { /* flip 8 bytes in one frame: still Complete */ }
#[test] fn nine_bad_bytes_is_frame_bad_then_repeat_fixes() { /* first copy FrameBad, clean repeat completes */ }
#[test] fn two_senders_interleaved() { /* frames of tag 1 and tag 2 alternated: two Completes with the right bytes */ }
#[test] fn partial_expires_after_ten_minutes() { /* push half, expire(600_001): later frames start fresh, no Complete from stale halves */ }
proptest! { #[test] fn push_never_panics(b in proptest::collection::vec(any::<u8>(),0..300)) { let _=Reassembler::new().push(&b,0); } }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-modem --test frames`. Expected: FAIL.
- [ ] **Step 3:** Implement with the `reed-solomon` crate (`Encoder::new(PARITY)`, `Decoder::new(PARITY)`).
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(modem): framing, Reed-Solomon, reassembly by tag`.

### Task 6: The FSK modem

**Files:**
- Create: `crates/modem/src/fsk.rs`, `crates/modem/tests/fsk.rs`, `crates/modem/tests/fixtures/` (WAV files generated by the tests on first run, checked in)

**Interfaces:**
- Produces:
  - `pub enum Band { NearSilent, Audible }`. NearSilent is 17_600..=19_800 Hz and Audible is 2_000..=4_200 Hz.
  - `pub struct ModemConfig { pub sample_rate: u32, pub band: Band, pub tones: u8, pub symbol_ms: f32 }`, with `Default` = 48_000, NearSilent, 16, 20.0. These are starting values, to be tuned in Task 12.
  - `pub fn modulate(cfg: &ModemConfig, frame: &[u8]) -> Vec<f32>`: a 100 ms up-chirp across the band as preamble, then 4 bits per symbol, with raised-cosine ramps of 2 ms at symbol edges so the speaker doesn't click.
  - `pub struct Demodulator` with `new(cfg) -> Self` and `push(&mut self, samples: &[f32]) -> Vec<DemodEvent>`.
  - `pub enum DemodEvent { Frame(Vec<u8>), Level { dbfs: f32 } }`. Frame length comes from the frame header (`len`) plus `PARITY`.
  - `pub fn measured_rate(cfg: &ModemConfig) -> f32` gives payload bytes per second after framing and parity, and is the figure the interface shows.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn clean_loopback() { let f=sample_frame(); let mut d=Demodulator::new(Default::default()); let ev=d.push(&modulate(&Default::default(),&f)); assert!(ev.contains(&DemodEvent::Frame(f))); }
#[test] fn survives_10db_noise() { /* add white noise at -10 dB relative to the signal RMS, seeded RNG: Frame decoded */ }
#[test] fn survives_half_percent_clock_drift() { /* resample by 1.005 with linear interpolation: Frame decoded */ }
#[test] fn arbitrary_start_offset() { /* prepend 0..48000 random samples of silence-level noise: Frame decoded */ }
#[test] fn silence_and_speech_band_noise_give_no_frames() { /* 10 s of pink noise low-passed at 8 kHz: zero Frame events */ }
#[test] fn audible_band_loopback() { /* Band::Audible clean loopback */ }
#[test] fn near_silent_energy_stays_in_band() { /* FFT of modulate output: >= 99% of energy within 17.4-20.0 kHz */ }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-modem --test fsk`. Expected: FAIL.
- [ ] **Step 3:** Implement:
  - Detect the preamble by matched-filter correlation against the chirp.
  - Decode each symbol by Goertzel energy over the 16 tone bins in each symbol window.
  - Track symbol timing with early-late gating to absorb drift.
  - Report `Level` every 100 ms as the RMS dBFS of the full band.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(modem): 16-tone FSK modem with chirp sync and drift tracking`.

### Task 7: Audio devices and the quiet gate

**Files:**
- Create: `crates/modem/src/audio.rs`, `crates/modem/src/gate.rs`, `crates/modem/tests/gate.rs`

**Interfaces:**
- Produces:
  - `pub trait AudioIo: Send { fn start_input(&mut self, sink: Box<dyn FnMut(&[f32]) + Send>) -> Result<(), AudioError>; fn play(&mut self, samples: Vec<f32>) -> Result<(), AudioError>; fn sample_rate(&self) -> u32; }`
  - `pub struct CpalAudio` implements `AudioIo` with the default devices.
  - `AudioError { NoDevice, PermissionDenied, Unsupported(String) }`
  - `pub struct QuietGate` with `new(send_under_db: f32, repeats: u8, repeat_gap_s: u32)`, `enqueue(&mut self, item: usize)`, `observe(&mut self, room_db: f32, now_ms: u64)` and `due(&mut self, now_ms: u64) -> Option<usize>` (which queued item to play now).
  - `pub fn dbfs_to_room_db(dbfs: f32, calibration_offset: f32) -> f32`. The default offset is 94.0 and is documented as approximate.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn waits_for_quiet() { let mut g=QuietGate::new(40.0,2,90); g.enqueue(0); g.observe(47.0,0); assert_eq!(g.due(0),None); g.observe(38.0,1000); assert_eq!(g.due(1000),Some(0)); }
#[test] fn repeats_twice_ninety_seconds_apart() { /* after first play: due again at +90_000 and +180_000, then never */ }
#[test] fn denied_mic_is_reported_not_panicked() { let mut a=FakeAudio::denied(); assert_eq!(a.start_input(Box::new(|_|{})), Err(AudioError::PermissionDenied)); }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-modem --test gate`. Expected: FAIL.
- [ ] **Step 3:** Implement `QuietGate`. `CpalAudio` maps backend permission errors to `PermissionDenied`, and missing devices to `NoDevice`.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(modem): audio io trait, cpal backend, quiet gate`.

### Task 8: The encrypted store

**Files:**
- Create: `crates/store/src/lib.rs`, `crates/store/tests/store.rs`

**Interfaces:**
- Consumes: `PublicIdentity`, `Identity` (Task 2).
- Produces:
  - `pub struct Store` with:
    - `create(path: &Path, passphrase: &str, me: &Identity, my_name: &str) -> Result<Store, StoreError>`
    - `open(path: &Path, passphrase: &str) -> Result<Store, StoreError>`
    - `me(&self) -> Identity`
    - `add_contact(&self, name: &str, who: &PublicIdentity, checked: bool) -> Result<ContactId, StoreError>`
    - `contacts(&self) -> Vec<Contact>`
    - `put_message(&self, m: &StoredMessage) -> Result<(), StoreError>`
    - `conversation(&self, c: ContactId) -> Vec<StoredMessage>`
  - `StoredMessage { id: [u8; 16], contact: ContactId, outgoing: bool, text: String, channel: Channel, at_min: u32, heard: bool }`
  - `pub enum Channel { Sound, Light, Bluetooth }`
  - `StoreError { WrongPassphrase, Corrupt, Io }`
  - The SQLCipher key is Argon2id(passphrase, a 16-byte salt stored in a sidecar file, with m=64 MiB, t=3, p=1).

- [ ] **Step 1: Failing tests**

```rust
#[test] fn wrong_passphrase_refused() { let d=tempdir(); Store::create(&d.path().join("b.db"),"correct horse",&Identity::generate(),"Kitchen laptop").unwrap(); assert!(matches!(Store::open(&d.path().join("b.db"),"wrong"), Err(StoreError::WrongPassphrase))); }
#[test] fn nothing_readable_on_disk() { /* add contact "Marisol Ortega", message "Water is at the church"; read the db and sidecar bytes: neither string, nor the identity secret bytes, appears */ }
#[test] fn reopen_keeps_contacts_and_messages() { /* create, add, drop, open: same contacts and conversation */ }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-store`. Expected: FAIL.
- [ ] **Step 3:** Implement with `rusqlite` and the `bundled-sqlcipher` feature. The identity secret lives in a table inside the encrypted database.
- [ ] **Step 4:** Run the tests. Expected: PASS.
- [ ] **Step 5:** Commit: `feat(store): encrypted contacts and messages`.

### Task 9: The node: send, receive, pair, receipts

**Files:**
- Create: `crates/node/src/lib.rs`, `crates/node/src/transport.rs`, `crates/node/src/sound.rs`, `crates/node/tests/two_nodes.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub trait Transport: Send { fn name(&self) -> Channel; fn send(&mut self, frame: Vec<u8>); fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent>; }`
  - `TransportEvent { Frame(Vec<u8>), RoomDb(f32), Unavailable(String) }`
  - `SoundTransport::new(io: Box<dyn AudioIo>, cfg: ModemConfig, gate: QuietGate)` implements `Transport`.
  - `pub struct Node` with:
    - `new(store: Option<Store>, transports: Vec<Box<dyn Transport>>) -> Node`
    - `unlock(&mut self, store: Store)`
    - `send(&mut self, to: &[ContactId], text: &str) -> [u8; 16]`
    - `broadcast(&mut self, text: &str) -> [u8; 16]`
    - `start_pairing(&mut self, my_name: &str)`
    - `confirm_pairing(&mut self, peer: &PublicIdentity, name: &str) -> ContactId`
    - `tick(&mut self, now_ms: u64) -> Vec<NodeEvent>`
  - `NodeEvent` variants:
    - `ChunkLanded { tag: u16, index: u16, of: u16, view: ChunkView }`
    - `Message(StoredMessage)`
    - `Heard { id: [u8; 16], by: ContactId }`
    - `Broadcast { signer: PublicIdentity, text: String, verified: bool }`
    - `PairOffer { peer: PublicIdentity, name: String, safety: String }`
    - `RoomDb(f32)`
    - `ChannelState { channel: Channel, state: String }`
  - Pairing over sound: each side broadcasts a `Control` envelope carrying `PairHello { identity, name }`, signed. On receiving one, the node emits `PairOffer` with `safety_number`. Nothing is stored until `confirm_pairing`.
  - Receipts: on opening a private message from a contact, with receipts on, the node sends a `Control` envelope `Heard { id }` sealed to the sender.
  - While locked: complete envelopes are kept in memory, capped at 256, and processed on `unlock`.

- [ ] **Step 1: Failing tests** (two `Node`s joined by a `LoopbackAudio` pair that mixes in -10 dB noise)

```rust
#[test] fn pair_then_message_then_receipt() { /* A.start_pairing, B.start_pairing; both see PairOffer with equal safety strings; both confirm; A.send([b],"Water is at the church on Fifth."); tick both until B emits Message with that text and A emits Heard */ }
#[test] fn chunks_land_in_order_with_views() { /* B sees ChunkLanded index 0..of-1 before Message; each view.heard xor view.key == view.plain */ }
#[test] fn message_while_locked_is_kept() { /* B created with store None; A sends; B.unlock(store); next tick emits Message */ }
#[test] fn broadcast_from_stranger_is_unverified() { /* C (unpaired) broadcasts; B emits Broadcast verified=false */ }
#[test] fn mic_denied_reports_channel_state() { /* SoundTransport over FakeAudio::denied(): tick emits ChannelState{Sound,"unavailable: microphone permission denied"}, no panic */ }
```

- [ ] **Step 2:** Run `cargo test -p bombechat-node`. Expected: FAIL.
- [ ] **Step 3:** Implement. Transmission tags are random `u16` per send.
- [ ] **Step 4:** Run the tests. Expected: PASS. The two-node test must finish in under 60 s on the CI runner. Shorten `symbol_ms` in the test config if needed, but keep the default.
- [ ] **Step 5:** Commit: `feat(node): pairing, sealed messaging, receipts and broadcasts over sound`.

### Task 10: Moving the interface into the app, with a bridge

**Files:**
- Create: `app/ui/` (moved from `mockups/bombechat.html`, `mockups/settings.html`, `mockups/machine-kit.js`), `app/ui/bridge.js`, `app/ui/vendor/three/` (three@0.170 build and the addons used), `app/ui/fonts/` (Cutive Mono, Libre Caslon Text, and an engraving face to replace Copperplate)
- Modify: `tools/check.cjs`, `tools/check-settings.cjs` and `tools/serve.cjs` to point at `app/ui/`. `mockups/` stays as the frozen design reference.

**Interfaces:**
- Produces: `bridge.js` exports:
  - `bridge.send(contactIds, text)`, `bridge.broadcast(text)`, `bridge.pair()`, `bridge.confirmPair(peerHex, name)`, `bridge.setSetting(id, valueIndex)` and `bridge.unlock(passphrase)`
  - `bridge.on(event, fn)` for events `chunk`, `message`, `heard`, `broadcast`, `pairOffer`, `roomDb` and `channel`. The payloads mirror `NodeEvent`, with bytes rendered as letters by `letter(b) = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"[b % 26]`.
  - Under Tauri it uses `window.__TAURI__.core.invoke` and `.event.listen`. Otherwise it runs `simulator.js`, which is the mockups' current simulated conversation moved out of the pages.
- The drums show each landed chunk 12 letters at a time, in order. Plain is the decrypted letters, Heard the ciphertext letters, Key the keystream letters.
- The drum clicks are muted while a chunk is arriving by sound. The plain-row letters still appear, and the clicks play only for drums turning after reception ends. A check asserts that `Drum.spin` makes no sound while the simulator reports reception.
- The Sound switch positions read Off, Listen, and Listen and send. Light reads Off, Read, and Read and flash. Send implies Listen.

- [ ] **Step 1:** Change the checks to assert the drums are driven by `chunk` events: inject a known chunk through `simulator.js` and expect the plain row to read its first 12 letters. Run `npm run check:front`. Expected: FAIL.
- [ ] **Step 2:** Move the pages and replace their inline simulation with `bridge` calls. Vendor three.js and the fonts so there are zero external URLs (assert with a check that no `http` URL appears in `app/ui/**`). Replace Copperplate with the chosen embedded engraving face.
- [ ] **Step 3:** Run `npm run check`. Expected: PASS, including the no-external-URL assertion. Read the screenshots and compare them with `mockups/`; the look must not regress. Iterate until it doesn't.
- [ ] **Step 4:** Commit: `refactor(ui): interface moved into the app behind a bridge, assets vendored`.

### Task 11: The Tauri app

**Files:**
- Create: `app/src-tauri/` (Cargo package `bombechat-app`, `tauri.conf.json`, `src/main.rs`, `src/commands.rs`), `app/src-tauri/tests/commands.rs`

**Interfaces:**
- Consumes: `Node` (Task 9), `bridge.js` names (Task 10).
- Produces:
  - Commands: `send`, `broadcast`, `pair`, `confirm_pair`, `set_setting` and `unlock`, named exactly as `bridge.js` calls them.
  - The node's events are re-emitted as the bridge events, on a 20 ms tick thread.
  - `tauri.conf.json`:
    - CSP `default-src 'self'; connect-src 'none'; img-src 'self' data:; style-src 'self' 'unsafe-inline'`
    - no updater, no shell, no http plugins
    - the microphone usage description: "Bombechat listens for messages sent nearby by sound."

- [ ] **Step 1:** Write a command test with a fake `Node`: `send` returns a 32-hex id and the fake receives the text. Run `cargo test -p bombechat-app`. Expected: FAIL.
- [ ] **Step 2:** Implement the commands and the event pump.
- [ ] **Step 3:** Run `cargo test -p bombechat-app`. Expected: PASS. Run `bash tools/check-airgapped.sh` with the macOS binary check enabled. Expected: clean.
- [ ] **Step 4:** Commit: `feat(app): Tauri shell for the airgapped build`.

### Task 12: Two laptops in a room

**Files:**
- Create: `docs/testing/two-laptops.md`, `tools/measure-modem.rs` (as `crates/modem/examples/measure.rs`)

**Interfaces:**
- Consumes: the app (Task 11) and `modulate`/`Demodulator` (Task 6).

- [ ] **Step 1:** Run `cargo run -p bombechat-modem --example measure` on two laptops, 1 m, 3 m and 8 m apart, in a quiet room and with background talk. Record the frame success rate, and tune `ModemConfig::default()` (tones, `symbol_ms`) until 3 m quiet reaches at least 95% frames on the first try. Record the measured `measured_rate`.
- [ ] **Step 2:** Update the interface's bytes-per-second readout to the measured value from the core (`channel` event). Remove the mockup's fixed 64.
- [ ] **Step 3:** Walk through the script in `docs/testing/two-laptops.md`:
  - pair by sound
  - compare safety numbers
  - send "Water is at the church on Fifth."
  - watch the drums land
  - see "heard by" come back
  - send while talking and see it wait for quiet
  - lock and unlock
  - broadcast from a third, unpaired laptop and see it marked unverified

  Every step passes, and the results go in the doc.
- [ ] **Step 4:** Run `npm run check`, `cargo test --workspace` and the airgapped check. Expected: all PASS. Commit and push: `test: two-laptop acceptance for the sound path`.

---

## Later plans

Each gets its own plan once this one ships:

1. **Light channel:** animated codes, camera reading, and pairing by light, with the key-check card redesigned for 60 digits.
2. **Relay:** carry for others, hop limit, hold time, storage cap, dedup and broadcast rate limits.
3. **Settings wired to the core:** every patch cord drives a real setting.
4. **Screens not yet mocked:** contacts and conversations, several recipients, writing broadcasts, first run and unlock. Each is mocked, iterated and approved first.
5. **Standard build:** Bluetooth transport, requests to connect, and Noise sessions.
6. **Release:** signed releases, checksums, and reproducible builds as a goal.
7. **Phones.**
8. **Bitchat bridge** (future update).
