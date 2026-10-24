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
// Query parameters: ?f=N starts the incoming message at chunk N (0 based),
// ?still holds everything where it lands (no room meter, no further chunks).
const SHOWN = 'Water is at the church on Fifth. Bring the radio, we leave at dusk.';
const CHUNK = 48;
const MARISOL = 1;
const TAG = 0x5a1e;

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
  const pub = (h, m, text, hops) => { setClock(h, m); emit('broadcast', { signer: MARISOL_KEY, text, verified: true, hops }); };
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
  const settings = { sound: 2, light: 0 };
  // waiting to send; the public one was queued before the page opened
  const queue = [{ id: bytes(16), text: 'Public: the library on Halsted has power and charging.', public: true }];
  let room = 47, dipUntil = 0, nextDip = 0, sending = false, paused = false, done = false, started = false;
  const later = (fn, ms = 0) => setTimeout(fn, ms);

  const sound = state => emit('channel', { channel: 'Sound', state });
  const outbox = (m, state, first) => emit('outbox', first ? { id: m.id, state, text: m.text, public: !!m.public } : { id: m.id, state });
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

  function start() {
    if (started) return; started = true;
    history(emit, setClock);
    for (const m of queue) outbox(m, 'waiting', true);
    setClock(9, 14);
    if (settings.sound > 0) { sound('receiving'); for (let i = 0; i <= c; i++) land(i); }
    setClock(9, 15);
    emit('roomDb', room);
    if (still) return;
    nextDip = performance.now() + 9000;
    setInterval(roomTick, 170);
    if (!done) later(nextChunk, 4200);
  }

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
  };

  return {
    start,
    now: () => clock,
    send(contactIds, text) {
      const m = { id: bytes(16), text };
      clock += 60000;
      if (settings.sound < 2 && settings.light === 2) {
        later(() => outbox(m, 'playing'));
        later(() => { outbox(m, 'sent'); emit('heard', { id: m.id, by: MARISOL }); }, 1500);
      } else { queue.push(m); later(() => outbox(m, 'waiting')); }
      return Promise.resolve(m.id);
    },
    broadcast(text) { const m = { id: bytes(16), text, public: true }; queue.push(m); later(() => outbox(m, 'waiting')); return Promise.resolve(m.id); },
    pair: () => Promise.resolve(),
    confirmPair: () => Promise.resolve(2),
    setSetting(id, valueIndex) { settings[id] = valueIndex; return Promise.resolve(); },
    unlock: () => Promise.resolve(),
    test,
  };
}
