// The other side of the room, for a plain browser: the conversation with
// Marisol that the mockups showed, played through the same events the node
// sends. It emits what the Tauri shell would (byte arrays, "Sound"), so the
// bridge's normalising is exercised here too. Nothing in it is used in the app.
//
// Query parameters: ?f=N starts the incoming message at frame N (0 based),
// ?still holds everything where it lands (no room meter, no further frames).
const A = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';
// the incoming message, as the mockup spelled it on the drums: X for a space
const MSG = 'WATERXISXATXTHEXCHURCHXONXFIFTHXBRINGXTHEXRADIOXWEXLEAVEXATXDUSKX';
const KEY = 'QXMBRTLEKZPOWJFAYUVHDGCNSI';
const SHOWN = 'Water is at the church on Fifth. Bring the radio, we leave at dusk.';
const W = 12, NF = Math.ceil(MSG.length / W);
const MARISOL = 1;
const TAG = 0x5a1e;

const at = (h, m) => Math.floor(new Date(2026, 9, 24, h, m).getTime() / 60000);
const bytes = n => Array.from(crypto.getRandomValues(new Uint8Array(n)));
const signer = (a, b) => ({ sign: [a, b, ...bytes(30)], dh: bytes(32) });

function frame(n) {
  const plain = [], heard = [], key = [];
  for (let c = 0; c < W; c++) {
    const p = A.indexOf(MSG[n * W + c] || 'X'), k = A.indexOf(KEY[(n * W + c * 7) % 26]);
    plain.push(p); key.push(k); heard.push((p + k) % 26);
  }
  return { plain, heard, key };
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
  let f = query.has('f') ? Math.max(0, Math.min(NF - 1, +query.get('f') || 0)) : 3;
  let clock = at(9, 15) * 60000;
  const setClock = (h, m) => { clock = at(h, m) * 60000; };
  const settings = { sound: 2, light: 0 };
  // waiting to send, in the order the page shows them; the public one was queued before the page opened
  const queue = [{ text: 'Public: the library on Halsted has power and charging.', public: true }];
  const pendingAtOpen = queue.map(m => ({ ...m }));
  let room = 47, dipUntil = 0, nextDip = 0, sending = false, paused = false, done = false, started = false;
  const timers = [];

  const sound = state => emit('channel', { channel: 'Sound', state });
  function land(n) {
    emit('chunk', { tag: TAG, index: n, of: NF, view: frame(n) });
    if (n === NF - 1) finish();
  }
  function finish() {
    done = true;
    emit('message', { id: bytes(16), contact: MARISOL, outgoing: false, text: SHOWN, channel: 'Sound', at_min: at(9, 14), heard: false });
    sound('listening');
  }
  function nextChunk() {
    if (paused || done || settings.sound === 0) return;
    f += 1; land(f);
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
  function flush() {
    if (sending || !queue.length || settings.sound < 2) return;
    sending = true; const m = queue[0];
    setTimeout(() => { const i = queue.indexOf(m); if (i >= 0) queue.splice(i, 1); sending = false; if (m.id) setTimeout(() => emit('heard', { id: m.id, by: MARISOL }), 1500); }, 460);
  }

  function start() {
    if (started) return; started = true;
    history(emit, setClock);
    setClock(9, 14);
    if (settings.sound > 0) { sound('receiving'); land(f); }
    setClock(9, 15);
    emit('roomDb', room);
    if (still) return;
    nextDip = performance.now() + 9000;
    timers.push(setInterval(roomTick, 170), setInterval(nextChunk, 4200));
  }

  const test = {
    // a chunk of a new message, by letters: stops the scripted conversation first
    inject(plainLetters, { end = false } = {}) {
      paused = true;
      const plain = [...plainLetters.toUpperCase()].map(ch => Math.max(0, A.indexOf(ch)));
      const key = plain.map((_, i) => (i * 7 + 3) % 26), heard = plain.map((p, i) => (p + key[i]) % 26);
      sound('receiving');
      emit('chunk', { tag: 0x7e57, index: 0, of: 1, view: { plain, heard, key } });
      if (end) sound('listening');
    },
    receiving(on) { sound(on ? 'receiving' : 'listening'); },
    pause() { paused = true; },
  };

  return {
    start,
    now: () => clock,
    pending: () => pendingAtOpen,
    send(contactIds, text) {
      const id = bytes(16);
      clock += 60000;
      if (settings.sound < 2 && settings.light === 2) setTimeout(() => emit('heard', { id, by: MARISOL }), 1500);
      else queue.push({ id, text });
      return Promise.resolve(id);
    },
    broadcast(text) { const id = bytes(16); queue.push({ text, public: true }); return Promise.resolve(id); },
    pair: () => Promise.resolve(),
    confirmPair: () => Promise.resolve(2),
    setSetting(id, valueIndex) { settings[id] = valueIndex; return Promise.resolve(); },
    unlock: () => Promise.resolve(),
    test,
  };
}
