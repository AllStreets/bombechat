// The page's one way to the machine. In the app it talks to the Tauri shell;
// in a plain browser it runs simulator.js, the conversation the mockups showed.
//
// Commands (Tauri command names in brackets, arguments camelCase as Tauri expects):
//   send(contactIds, text)       [send { contactIds, text }]        resolves to the message id, as hex
//   broadcast(text)              [broadcast { text }]               resolves to the message id, as hex
//   pair()                       [pair]
//   confirmPair(peerHex, name)   [confirm_pair { peerHex, name }]   resolves to the new contact id
//   setSetting(id, valueIndex)   [set_setting { id, valueIndex }]
//   unlock(passphrase)           [unlock { passphrase }]
//
// Events, as the node's NodeEvent, after normalising (on(name, fn)):
//   chunk      { tag, index, of, view: { plain, heard, key } }   each view field a string of letters
//   message    { id, contact, outgoing, text, channel, at_min, heard }
//   heard      { id, by }
//   broadcast  { signer, text, verified }                       signer: hex of the signing key
//   pairOffer  { peer, name, safety }                           peer: hex of sign key then dh key
//   roomDb     number
//   channel    { channel, state }
// Ids arrive as byte arrays or hex and leave as lowercase hex. Channels arrive as
// "Sound" or "sound" and leave lower case. Bytes become letters by letter(b).
//
// A chunk is only known to come from a holder of the content key until the
// message event that follows it. The drums may show it; the sheet waits.
//
// Beyond the node's events, the bridge offers start() (begin delivering the
// simulated conversation; nothing in the app), now() (the clock the sheet
// writes times with, in ms) and pending() (sends already waiting when the page
// opened; empty in the app until the node can report its queue).
import { createSimulator } from './simulator.js';

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';
export const letter = b => ALPHABET[((b % 26) + 26) % 26];
const letters = v => typeof v === 'string' ? v : Array.from(v || [], letter).join('');
const hex = v => typeof v === 'string' ? v.toLowerCase() : Array.from(v || [], b => (b & 255).toString(16).padStart(2, '0')).join('');
const lower = c => String(c || '').toLowerCase();
const contactId = c => c && typeof c === 'object' && '0' in c ? +c[0] : +c;
const identity = p => p && typeof p === 'object' && !Array.isArray(p) && 'sign' in p ? hex(p.sign) + hex(p.dh) : hex(p);

const EVENTS = ['chunk', 'message', 'heard', 'broadcast', 'pairOffer', 'roomDb', 'channel'];
const normalise = {
  chunk: c => ({ tag: c.tag, index: c.index, of: c.of, view: { plain: letters(c.view.plain), heard: letters(c.view.heard), key: letters(c.view.key) } }),
  message: m => ({ ...m, id: hex(m.id), contact: contactId(m.contact), channel: lower(m.channel) }),
  heard: h => ({ id: hex(h.id), by: contactId(h.by) }),
  broadcast: b => ({ ...b, signer: b.signer && b.signer.sign ? hex(b.signer.sign) : hex(b.signer) }),
  pairOffer: p => ({ ...p, peer: identity(p.peer) }),
  roomDb: d => +(d && typeof d === 'object' ? d.db : d),
  channel: s => ({ ...s, channel: lower(s.channel) }),
};

const handlers = Object.fromEntries(EVENTS.map(e => [e, new Set()]));
function deliver(event, raw) {
  if (!handlers[event]) return;
  const payload = normalise[event](raw);
  for (const fn of handlers[event]) fn(payload);
}

const tauri = typeof window !== 'undefined' && window.__TAURI__ && window.__TAURI__.core ? window.__TAURI__ : null;

function tauriBackend() {
  const { invoke } = tauri.core;
  const listening = new Set();
  return {
    listen(event) { if (listening.has(event)) return; listening.add(event); tauri.event.listen(event, e => deliver(event, e.payload)); },
    send: (contactIds, text) => invoke('send', { contactIds, text }).then(hex),
    broadcast: text => invoke('broadcast', { text }).then(hex),
    pair: () => invoke('pair'),
    confirmPair: (peerHex, name) => invoke('confirm_pair', { peerHex, name }).then(contactId),
    setSetting: (id, valueIndex) => invoke('set_setting', { id, valueIndex }),
    unlock: passphrase => invoke('unlock', { passphrase }),
    start() {},
    now: () => Date.now(),
    pending: () => [],
  };
}

function simulatedBackend() {
  const sim = createSimulator(deliver, { query: new URLSearchParams(location.search) });
  window.__sim = sim.test;
  return {
    listen() {},
    send: (contactIds, text) => sim.send(contactIds, text).then(hex),
    broadcast: text => sim.broadcast(text).then(hex),
    pair: () => sim.pair(),
    confirmPair: (peerHex, name) => sim.confirmPair(peerHex, name).then(contactId),
    setSetting: (id, valueIndex) => sim.setSetting(id, valueIndex),
    unlock: passphrase => sim.unlock(passphrase),
    start: () => sim.start(),
    now: () => sim.now(),
    pending: () => sim.pending(),
  };
}

const backend = tauri ? tauriBackend() : simulatedBackend();

export const bridge = {
  simulated: !tauri,
  send: (contactIds, text) => backend.send(contactIds, text),
  broadcast: text => backend.broadcast(text),
  pair: () => backend.pair(),
  confirmPair: (peerHex, name) => backend.confirmPair(peerHex, name),
  setSetting: (id, valueIndex) => backend.setSetting(id, valueIndex),
  unlock: passphrase => backend.unlock(passphrase),
  on(event, fn) {
    if (!handlers[event]) throw new Error('unknown bridge event: ' + event);
    handlers[event].add(fn); backend.listen(event);
    return () => handlers[event].delete(fn);
  },
  start: () => backend.start(),
  now: () => backend.now(),
  pending: () => backend.pending(),
};
