// The page's one way to the machine. In the app it talks to the Tauri shell;
// in a plain browser it runs simulator.js, the conversation the mockups showed.
//
// Commands (Tauri command names in brackets, arguments camelCase as Tauri expects):
//   send(contactIds, text, channel)  [send { contactIds, text, channel }]  resolves to the message id, as hex;
//                                channel 'sound' (the default) or 'light'. Rejects with 'not sent' when nothing
//                                went: in this plan only sound sends, and light reports channel 'unavailable: ...'
//   broadcast(text)              [broadcast { text }]               resolves to the message id, as hex
//   pair()                       [pair]
//   confirmPair(peerHex, name)   [confirm_pair { peerHex, name }]   resolves to the new contact id
//   setSetting(id, valueIndex)   [set_setting { id, valueIndex }]
//   unlock(passphrase)           [unlock { passphrase }]            rejects with a sentence to show, e.g. 'wrong passphrase'
//   contacts()                   [contacts]                         resolves to [{ id, name, short, checked }]; rejects
//                                with 'locked' (ask for the passphrase) or 'no store yet' (the first passphrase sets it)
//
// Events (on(name, fn)), as the node's NodeEvent after normalising:
//   chunk      { tag, index, of, view: { plain, heard, key }, text }
//                plain: one letter per byte; ASCII letters upper-cased, every other byte X (the Bombe's spacer)
//                heard, key: one letter per byte, by letter(b)
//                text: the message text decrypted so far under this tag, NOT yet checked (see below)
//   message    { id, contact, outgoing, text, channel, at_min, heard }
//   heard      { id, by }
//   broadcast  { signer, text, verified }                       signer: hex of the signing key
//   pairOffer  { peer, name, safety }                           peer: hex of sign key then dh key
//   roomDb     number
//   channel    { channel, state }       state 'receiving' while a message arrives by sound; 'failed' when one
//                                       stops arriving or fails its checks (the state it is then in follows);
//                                       anything else (e.g. 'listening', 'unavailable: ...') otherwise
//   outbox     { id, state, text?, public? }   a send's progress: 'waiting' (for the room to fall under Send under),
//                                       'playing', 'sent'. text and public come with the first report of a send
//                                       this page didn't make (one queued before it opened); the simulator sends
//                                       them, the node does not yet, so in the app such a send shows no slip.
// Ids arrive as byte arrays or hex and leave as lowercase hex. Channels arrive as "Sound" or "sound" and leave lower case.
//
// A chunk is only known to come from a holder of the content key until the
// message event that follows it. The drums may show it, and the sheet may show
// its text as provisional tape, but only `message` puts it on the sheet for good.
//
// Beyond the node's events, the bridge offers start() (begin delivering the
// simulated conversation; nothing in the app) and now() (the clock the sheet
// writes times with, in ms).
import { createSimulator } from './simulator.js';

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';
export const letter = b => ALPHABET[((b % 26) + 26) % 26];
// the plain row reads as the message: letters as letters, everything else as the spacer
export const plainLetter = b => (b >= 65 && b <= 90) || (b >= 97 && b <= 122) ? String.fromCharCode(b & ~32) : 'X';
const bytesOf = v => Array.from(v || []);
const hex = v => typeof v === 'string' ? v.toLowerCase() : Array.from(v || [], b => (b & 255).toString(16).padStart(2, '0')).join('');
const lower = c => String(c || '').toLowerCase();
const contactId = c => c && typeof c === 'object' && '0' in c ? +c[0] : +c;
const identity = p => p && typeof p === 'object' && !Array.isArray(p) && 'sign' in p ? hex(p.sign) + hex(p.dh) : hex(p);
const person = c => ({ id: contactId(c.id), name: String(c.name), short: String(c.name).split(' ')[0], checked: !!c.checked });

// The sealed inner plaintext is: sender sign [32], sender dh [32], sent_min [4],
// the text in UTF-8, then the Ed25519 signature [64] (crates/core/src/seal.rs).
// The provisional text is what lies between. Before the last chunk the exact end
// isn't known, but `of` bounds it: every chunk but the last is full, so the
// whole is at least (of - 1) chunks and one byte, and the text ends no earlier
// than 64 bytes before that.
const SENDER_LEN = 68, SIGNATURE_LEN = 64;
const arriving = new Map(); // tag -> { bytes so far, chunk size }
const decoder = new TextDecoder('utf-8');
function provisional(tag, index, of, plain) {
  const a = index === 0 || !arriving.has(tag) ? { so: [], size: 0 } : arriving.get(tag);
  if (index < of - 1) a.size = plain.length;
  a.so.push(...plain); arriving.set(tag, a);
  const last = index === of - 1;
  const end = last ? a.so.length - SIGNATURE_LEN : Math.min(a.so.length, (of - 1) * a.size + 1 - SIGNATURE_LEN);
  if (end <= SENDER_LEN) return '';
  return decoder.decode(new Uint8Array(a.so.slice(SENDER_LEN, end))).replace(/\uFFFD+$/, '');
}

const EVENTS = ['chunk', 'message', 'heard', 'broadcast', 'pairOffer', 'roomDb', 'channel', 'outbox'];
const normalise = {
  chunk: c => {
    const plain = bytesOf(c.view.plain);
    const text = provisional(c.tag, c.index, c.of, plain);
    if (c.index === c.of - 1) arriving.delete(c.tag);
    return { tag: c.tag, index: c.index, of: c.of, text, view: { plain: plain.map(plainLetter).join(''), heard: bytesOf(c.view.heard).map(letter).join(''), key: bytesOf(c.view.key).map(letter).join('') } };
  },
  message: m => ({ ...m, id: hex(m.id), contact: contactId(m.contact), channel: lower(m.channel) }),
  heard: h => ({ id: hex(h.id), by: contactId(h.by) }),
  broadcast: b => ({ ...b, signer: b.signer && b.signer.sign ? hex(b.signer.sign) : hex(b.signer) }),
  pairOffer: p => ({ ...p, peer: identity(p.peer) }),
  roomDb: d => +(d && typeof d === 'object' ? d.db : d),
  channel: s => ({ ...s, channel: lower(s.channel) }),
  outbox: o => ({ ...o, id: hex(o.id), state: lower(o.state) }),
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
    send: (contactIds, text, channel) => invoke('send', { contactIds, text, channel }).then(hex),
    broadcast: text => invoke('broadcast', { text }).then(hex),
    pair: () => invoke('pair'),
    confirmPair: (peerHex, name) => invoke('confirm_pair', { peerHex, name }).then(contactId),
    setSetting: (id, valueIndex) => invoke('set_setting', { id, valueIndex }),
    unlock: passphrase => invoke('unlock', { passphrase }),
    contacts: () => invoke('contacts').then(list => list.map(person)),
    start() {},
    now: () => Date.now(),
  };
}

function simulatedBackend() {
  const sim = createSimulator(deliver, { query: new URLSearchParams(location.search) });
  window.__sim = sim.test;
  return {
    listen() {},
    send: (contactIds, text, channel) => sim.send(contactIds, text, channel).then(hex),
    broadcast: text => sim.broadcast(text).then(hex),
    pair: () => sim.pair(),
    confirmPair: (peerHex, name) => sim.confirmPair(peerHex, name).then(contactId),
    setSetting: (id, valueIndex) => sim.setSetting(id, valueIndex),
    unlock: passphrase => sim.unlock(passphrase),
    contacts: () => sim.contacts().then(list => list.map(person)),
    start: () => sim.start(),
    now: () => sim.now(),
  };
}

const backend = tauri ? tauriBackend() : simulatedBackend();

export const bridge = {
  simulated: !tauri,
  send: (contactIds, text, channel = 'sound') => backend.send(contactIds, text, lower(channel)),
  broadcast: text => backend.broadcast(text),
  pair: () => backend.pair(),
  confirmPair: (peerHex, name) => backend.confirmPair(peerHex, name),
  setSetting: (id, valueIndex) => backend.setSetting(id, valueIndex),
  unlock: passphrase => backend.unlock(passphrase),
  contacts: () => backend.contacts(),
  on(event, fn) {
    if (!handlers[event]) throw new Error('unknown bridge event: ' + event);
    handlers[event].add(fn); backend.listen(event);
    return () => handlers[event].delete(fn);
  },
  start: () => backend.start(),
  now: () => backend.now(),
};
