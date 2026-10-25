// The other side of the room, for a plain browser: the conversation with
// Marisol that the mockups showed, played through the same events the node
// sends. It emits what the Tauri shell would (byte arrays, "Sound"), so the
// bridge's normalising is exercised here too. Nothing in it is used in the app.
//
// The incoming message is sealed the way the node seals one: sender keys and
// time, the text in UTF-8, a signature, cut into 48-byte chunks, each with its
// keystream and ciphertext. The keys and signature are made up, but seeded, so
// every visit (and every screenshot) shows the same letters.
//
// Pairing: pair() hears Teodor say hello 2.4 s later (a pairOffer with a made-up
// key and a seeded safety number); confirmPair adds him to contacts() under the
// name as the page corrected it.
//
// Settings: the simulated machine keeps them as the device keeps settings.json,
// apart from the page's own storage: in memory, carried between the front and
// back pages in sessionStorage under MACHINE_SETTINGS (a stand-in for the file
// beside the store; clearing the page's localStorage leaves it alone).
// settings() answers every setting the settings page has, defaults for the rest; settingNotes() the ones it can't
// change, which setSetting refuses with the device's sentence.
//
// Light: a send or notice by light at Read and flash is shown as the node shows one: display frames at the Flash
// speed's rate (synthetic bits, the size of that preset's grid) for at least four seconds, then displayStopped, then
// sent. At Read or Off it is refused as the node refuses it. Turning Light down mid-flash clears the screen and the
// send waits; turning it back to Read and flash carries on.
//
// Query parameters: ?f=N starts the incoming message at chunk N (0 based),
// ?still holds everything where it lands (no room meter, no further chunks).
const SHOWN = 'Water is at the church on Fifth. Bring the radio, we leave at dusk.';
const CHUNK = 48;
const MARISOL = 1;
const TAG = 0x5a1e;

const TEODOR = 2;
// every setting on the settings page and its value until the person changes it, as the device's commands::SETTINGS
const DEFAULTS = { sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 };
// how many values each has; like the device, a value outside these is refused, and dropped if found kept
const COUNTS = { sound: 3, volume: 5, band: 2, light: 3, flash: 3, machine: 2, machvol: 5, carry: 2, hops: 7, hold: 5, quiet: 5, repeats: 4, lock: 2, expire: 4, inperson: 2 };
const isSetting = (id, v) => id in COUNTS && Number.isInteger(v) && v >= 0 && v < COUNTS[id];
// the settings this machine can't change, as the device's setting_notes: relay waits for its feature, and on the
// airgapped build every contact is checked in person. The simulated machine is a Mac, so its lock can come off
const NOTES = { carry: { note: 'comes with relay' }, hops: { note: 'comes with relay' }, hold: { note: 'comes with relay' }, inperson: { note: 'always, on this build', value: 1 } };
// what set_setting says for each, as the device says it
const REFUSED = { carry: 'Carry for others comes with relay', hops: 'Hop limit comes with relay', hold: 'Hold for comes with relay', inperson: 'On this build every new contact needs a key check' };
const MACHINE_SETTINGS = 'bombechat.simulated-machine';
function keptSettings() {
  let kept = {};
  try { kept = JSON.parse(sessionStorage.getItem(MACHINE_SETTINGS) || '{}') || {}; } catch (e) {}
  const out = { ...DEFAULTS };
  for (const id in DEFAULTS) if (isSetting(id, kept[id])) out[id] = kept[id];
  for (const id in NOTES) out[id] = NOTES[id].value ?? DEFAULTS[id];
  return out;
}
// the notices this machine sent, kept as the device's store keeps them (beside the settings, in sessionStorage):
// broadcasts() replays them after a reload, and one still waiting is queued again when the conversation starts
const MACHINE_NOTICES = 'bombechat.simulated-notices';
function keptNotices() { try { return JSON.parse(sessionStorage.getItem(MACHINE_NOTICES) || '[]') || []; } catch (e) { return []; } }
function keepNotice(n) {
  const all = keptNotices().filter(k => String(k.id) !== String(n.id));
  all.push(n);
  try { sessionStorage.setItem(MACHINE_NOTICES, JSON.stringify(all)); } catch (e) {}
}
function keepSettings(settings) { try { sessionStorage.setItem(MACHINE_SETTINGS, JSON.stringify(settings)); } catch (e) {} }
// bytes of message a second over sound, as the modem measures it at its defaults (crates/modem measured_rate)
const RATE = 16.1;
// light, by Flash speed (Slow, Normal, Fast), as the light transport's rate and code presets have them: bytes a second,
// cells a side, frames a second (crates/light CodeSpec)
const LIGHT_RATES = [948, 2290, 4680];
const LIGHT_CELLS = [48, 56, 64];
const LIGHT_FPS = [6, 10, 15];
// the shortest an envelope is shown, as the light transport's HUMAN_FLOOR_MS
const LIGHT_FLOOR_MS = 4000;
// the light refusals and states, as the node's
const LIGHT_SAYS = ['not sent: light is off', 'not sent: light is set to read only'];
// the longest broadcast, in UTF-8 bytes of text, as the node's MAX_BROADCAST (an envelope less its header and signing)
const MAX_BROADCAST = 3939;
const PAIR_MS = 2400;
// this machine's public identity, sign key then dh key, made up but seeded so its fingerprint is always C41D
const SELF_KEY = 'c41d' + Array.from({ length: 62 }, ((r) => () => r().toString(16).padStart(2, '0'))(seeded(4242))).join('');
const at = (h, m) => Math.floor(new Date(2026, 9, 24, h, m).getTime() / 60000);
const bytes = n => Array.from(crypto.getRandomValues(new Uint8Array(n)));
const signer = (a, b) => ({ sign: [a, b, ...bytes(30)], dh: bytes(32) });
const utf8 = s => Array.from(new TextEncoder().encode(s));
function seeded(seed) { let a = seed >>> 0; return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) & 255; }; }

// sender sign [32] | sender dh [32] | sent_min [4] | text | signature [64], as crates/core/src/seal.rs lays it out
function sealedInner(text, minute, seed) {
  const r = seeded(seed), n = k => Array.from({ length: k }, r);
  return [0x7f, 0x3a, ...n(30), ...n(32), (minute >>> 24) & 255, (minute >>> 16) & 255, (minute >>> 8) & 255, minute & 255, ...utf8(text), ...n(64)];
}
// each chunk as the node reports it: plain, keystream, and the ciphertext heard (plain XOR key)
function chunked(inner, seed) {
  const r = seeded(seed ^ 0x9e3779b9), out = [];
  for (let s = 0; s < inner.length; s += CHUNK) {
    const plain = inner.slice(s, s + CHUNK), key = plain.map(() => r());
    out.push({ plain, key, heard: plain.map((p, i) => p ^ key[i]) });
  }
  return out;
}

// what was on the sheet before the page opened. A newline starts a new strip of tape.
// `via` and `hops` are the simulator's own: the node has no relays yet.
function history(emit, setClock) {
  const MARISOL_KEY = signer(0x7f, 0x3a);
  const msg = (h, m, outgoing, channel, text, extra = {}) => { setClock(h, m); const id = bytes(16); emit('message', { id, contact: MARISOL, outgoing, text, channel, at_min: at(h, m), heard: false, ...extra }); return id; };
  const pub = (h, m, text, hops) => { setClock(h, m); emit('broadcast', { id: bytes(16), outgoing: false, signer: MARISOL_KEY, text, verified: true, hops, at_min: at(h, m) }); };
  msg(6, 52, false, 'Light', 'Power went around two. Anything on your side?');
  msg(6, 58, true, 'Light', 'Same. The whole block is dark and the phone shows no service.');
  pub(7, 20, 'Red and Blue lines stopped. Buses running on Ashland.', 2);
  msg(7, 40, false, 'Light', 'Going to check on my mother on 106th.\nBack by nine.');
  msg(7, 46, true, 'Light', "Go. Battery at 60. I'll keep the laptop on sound from now.");
  msg(8, 12, false, 'Sound', 'Teodor here. Generator at the library is up.\nCharging by the back stairs.', { via: 'stairwell relay' });
  pub(8, 51, 'Shelter open at Benito Juarez High. Cots and water.', 1);
  msg(9, 2, false, 'Light', 'Back. My mother is fine. Ewing is jammed.');
  const good = msg(9, 7, true, 'Sound', "Good. I'm at the library on Halsted with the laptop.");
  setClock(9, 8); emit('heard', { id: good, by: MARISOL });
}

export function createSimulator(emit, { query = new URLSearchParams() } = {}) {
  const still = query.has('still');
  const incoming = chunked(sealedInner(SHOWN, at(9, 14), 1931), 1931);
  const NC = incoming.length;
  let c = query.has('f') ? Math.max(0, Math.min(NC - 1, +query.get('f') || 0)) : 1;
  let clock = at(9, 15) * 60000;
  const setClock = (h, m) => { clock = at(h, m) * 60000; };
  const settings = keptSettings();
  // waiting to send; the public one was queued before the page opened
  const queue = [{ id: bytes(16), text: 'Public: the library on Halsted has power and charging.', public: true }];
  let room = 47, dipUntil = 0, nextDip = 0, sending = false, paused = false, done = false, started = false;
  const later = (fn, ms = 0) => setTimeout(fn, ms);

  let soundState = null;// the last state the sound channel reported, for channels()
  const sound = state => { soundState = state; emit('channel', { channel: 'Sound', state }); };
  let lightState = null;// likewise for light
  const light = state => { lightState = state; emit('channel', { channel: 'Light', state }); };
  const lightAt = () => settings.light === 0 ? 'off' : 'listening';
  const outbox = (m, state, first) => {
    if (m.kept) keepNotice({ ...m.kept, state: state === 'playing' ? 'waiting' : state });
    emit('outbox', first ? { id: m.id, state, text: m.text, public: !!m.public } : { id: m.id, state });
  };
  function land(n) {
    emit('chunk', { tag: TAG, index: n, of: NC, view: incoming[n] });
    if (n === NC - 1) finish();
  }
  function finish() {
    done = true;
    emit('message', { id: bytes(16), contact: MARISOL, outgoing: false, text: SHOWN, channel: 'Sound', at_min: at(9, 14), heard: false });
    sound('listening');
  }
  // a chunk is four frames on the drums; send the next once they have had time to show
  function nextChunk() {
    if (paused || done || settings.sound === 0) return;
    c += 1; land(c);
    if (!done) later(nextChunk, 4 * 3400);
  }
  // the room hums around 47 dB with a quiet spell every so often; queued sends go out in the quiet
  function roomTick() {
    const t = performance.now();
    if (t > nextDip && !dipUntil) { dipUntil = t + 3600; nextDip = t + 15000; }
    if (dipUntil && t > dipUntil) dipUntil = 0;
    const target = dipUntil ? 36.5 : 47;
    room += (target - room) * 0.12 + (Math.random() - .5) * 1.5;
    emit('roomDb', room);
    if (room < 40) flush();
  }
  // what the node's quiet gate does: one at a time, under 40 dB, only when Sound may send
  function flush() {
    if (sending || !queue.length || settings.sound < 2) return;
    sending = true; const m = queue.shift();
    outbox(m, 'playing');
    later(() => { sending = false; }, 460);
    later(() => { outbox(m, 'sent'); if (!m.public) emit('heard', { id: m.id, by: MARISOL }); }, 1960);
  }

  // light: one envelope on screen at a time, its frames paced at the Flash speed it went on screen at
  const flashes = [];
  let flashing = null;// { m, spec, shown, window, lit, announced, heard }
  let flashT = null;
  const frameBits = (spec, tag, index) => { const r = seeded((tag << 12) ^ index ^ (spec << 28)); return Array.from({ length: Math.ceil(LIGHT_CELLS[spec] ** 2 / 8) }, r); };
  function flashTick() {
    if (!flashing) { flashing = flashes.shift() || null; if (flashing) { flashing.spec = settings.flash; flashing.window = Math.ceil(LIGHT_FLOOR_MS * LIGHT_FPS[flashing.spec] / 1000); } }
    const f = flashing;
    if (!f) { flashT = null; return; }
    if (settings.light < 2) {
      // Light turned down: the screen clears and the send waits, carrying on where it stopped
      if (f.lit) { f.lit = false; emit('displayStopped', { tag: f.tag }); }
      if (f.announced) { f.announced = false; outbox(f.m, 'waiting'); }
      flashT = later(flashTick, 200); return;
    }
    if (!f.announced) { f.announced = true; outbox(f.m, 'playing'); }
    emit('display', { tag: f.tag, bits: frameBits(f.spec, f.tag, f.shown), spec: f.spec, index: f.shown });
    f.lit = true; f.shown += 1;
    if (f.shown >= f.window) {
      flashing = null;
      later(() => { if (!flashing) emit('displayStopped', { tag: f.tag }); outbox(f.m, 'sent'); if (f.heard) emit('heard', { id: f.m.id, by: MARISOL }); }, 1000 / LIGHT_FPS[f.spec]);
    }
    flashT = later(flashTick, 1000 / LIGHT_FPS[f.spec]);
  }
  function flash(m, heard) {
    flashes.push({ m, tag: Math.floor(Math.random() * 65536), shown: 0, lit: false, announced: false, heard });
    later(() => outbox(m, 'waiting'));
    if (!flashT) flashT = later(flashTick, 0);
  }
  // refused by the Light switch, as the node refuses a send: the channel says why, the outbox fails, then the state
  function refuseLight(m) { later(() => { light(LIGHT_SAYS[settings.light]); outbox(m, 'failed'); light(lightAt()); }); }

  function start() {
    if (started) return; started = true;
    history(emit, setClock);
    for (const m of queue) outbox(m, 'waiting', true);
    // a notice of this machine's still waiting when the page last closed goes again, as the node queues it on unlock
    // on the channel it was sent on, as the node keeps it
    for (const k of keptNotices().filter(k => k.state === 'waiting')) { const m = { id: k.id, text: k.text, public: true, kept: k };
      if (k.channel === 'light') { if (settings.light < 2) refuseLight(m); else flash(m, false); } else { queue.push(m); outbox(m, 'waiting'); } }
    setClock(9, 14);
    if (settings.sound > 0) { sound('receiving'); for (let i = 0; i <= c; i++) land(i); }
    setClock(9, 15);
    emit('roomDb', room);
    if (still) return;
    nextDip = performance.now() + 9000;
    setInterval(roomTick, 170);
    if (!done) later(nextChunk, 4200);
  }

  const people = [{ id: MARISOL, name: 'Marisol Ortega', checked: true }];
  const safety = (() => { const r = seeded(2207); return Array.from({ length: 12 }, () => Array.from({ length: 5 }, () => r() % 10).join('')).join(' '); })();
  let testTag = 0x7e00;
  const test = {
    // number of chunks in the scripted incoming message
    chunks: NC,
    // one chunk whose plain bytes are exactly this text; stops the scripted conversation first
    inject(text, { end = false } = {}) {
      paused = true;
      const plain = utf8(text), r = seeded(77), key = plain.map(() => r());
      sound('receiving');
      emit('chunk', { tag: ++testTag, index: 0, of: 1, view: { plain, key, heard: plain.map((p, i) => p ^ key[i]) } });
      if (end) sound('listening');
    },
    // a whole sealed message from Marisol, chunk by chunk at once; `complete` false leaves it hanging
    arrive(text, { complete = true, contact = MARISOL } = {}) {
      paused = true;
      const tag = ++testTag, parts = chunked(sealedInner(text, clock / 60000, tag), tag);
      sound('receiving');
      parts.forEach((view, index) => emit('chunk', { tag, index, of: parts.length, view }));
      if (complete) { emit('message', { id: bytes(16), contact, outgoing: false, text, channel: 'Sound', at_min: Math.floor(clock / 60000), heard: false }); sound('listening'); }
    },
    fail() { sound('failed'); },
    receiving(on) { sound(on ? 'receiving' : 'listening'); },
    pause() { paused = true; },
    // how many times a setting was set, for the check that a page's load sets none
    settingsSet: 0,
  };

  return {
    start,
    now: () => clock,
    send(contactIds, text, channel) {
      const m = { id: bytes(16), text };
      clock += 60000;
      if (channel === 'light') { if (settings.light < 2) refuseLight(m); else flash(m, true); }
      else { queue.push(m); later(() => outbox(m, 'waiting')); }
      return Promise.resolve(m.id);
    },
    // refused as the broadcast command refuses; at Listen or Off (light: Read or Off), as the node does: the channel
    // says why, then the outbox fails
    broadcast(text, channel = 'sound') {
      if (channel !== 'sound' && channel !== 'light') return Promise.reject(`no such channel: ${channel}`);
      if (!text.trim()) return Promise.reject('Write the notice first');
      const n = utf8(text).length;
      if (n > MAX_BROADCAST) return Promise.reject(`Too long to broadcast: ${n} bytes, at most ${MAX_BROADCAST}`);
      const m = { id: bytes(16), text, public: true };
      clock += 60000;
      m.kept = { id: m.id, outgoing: true, signer: SELF_KEY.slice(0, 64), text, verified: true, hops: 0, at_min: Math.floor(clock / 60000), state: 'waiting', channel };
      keepNotice(m.kept);
      if (channel === 'light') { if (settings.light < 2) refuseLight(m); else flash(m, false); }
      else if (settings.sound < 2) later(() => { sound(settings.sound === 0 ? 'not sent: sound is off' : 'not sent: sound is set to listen only'); outbox(m, 'failed'); sound(settings.sound === 0 ? 'off' : 'listening'); });
      else { queue.push(m); later(() => outbox(m, 'waiting')); }
      return Promise.resolve(m.id);
    },
    pair() { later(() => emit('pairOffer', { peer: signer(0x2c, 0x91), name: 'Teodor Vance', safety }), PAIR_MS); return Promise.resolve(); },
    stopPairing: () => Promise.resolve(),
    confirmPair(peerHex, name) {
      if (!people.some(p => p.id === TEODOR)) people.push({ id: TEODOR, name, checked: true, safety });
      return Promise.resolve(TEODOR);
    },
    setSetting(id, valueIndex) {
      test.settingsSet += 1;
      if (!(id in COUNTS)) return Promise.reject(`no such setting: ${id}`);
      if (!isSetting(id, valueIndex)) return Promise.reject(`no value ${valueIndex} for ${id}`);
      if (REFUSED[id]) return Promise.reject(REFUSED[id]);
      const was = settings[id];
      settings[id] = valueIndex; keepSettings(settings);
      // the light channel says when its camera closes or opens, as the node does on its next tick
      if (id === 'light' && (was === 0) !== (valueIndex === 0)) later(() => light(lightAt()));
      return Promise.resolve();
    },
    // read from the record each time, as the device answers from its own: another page may have changed it
    settings: () => Promise.resolve(Object.assign(settings, keptSettings()) && { ...settings }),
    settingNotes: () => Promise.resolve(JSON.parse(JSON.stringify(NOTES))),
    unlock: () => Promise.resolve(),
    contacts: () => Promise.resolve(people.map(p => ({ ...p }))),
    // the sheet's history arrives as events when the conversation starts, as the mockups showed it
    conversation: () => Promise.resolve([]),
    // this machine's own notices, kept across reloads; the heard ones arrive as history, as the mockups showed them
    broadcasts: () => Promise.resolve(keptNotices()),
    // you are Teodor here: his key signs your broadcasts
    machine: () => Promise.resolve({ name: 'Machine No. 1', key: SELF_KEY, you: 'Teodor Vance', held: 6, version: '0.4', broadcast_max: MAX_BROADCAST }),
    channels: () => Promise.resolve([{ channel: 'Sound', state: soundState || 'listening', rate: RATE, band: settings.band ? 'audible' : 'near silent' },
      { channel: 'Light', state: lightState || lightAt(), rate: LIGHT_RATES[settings.flash] }]),
    test,
  };
}
