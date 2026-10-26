// The page's one way to the machine. In the app it talks to the Tauri shell;
// in a plain browser it runs simulator.js, the conversation the mockups showed.
//
// Commands (Tauri command names in brackets, arguments camelCase as Tauri expects):
//   send(contactIds, text, channel)  [send { contactIds, text, channel }]  resolves to the message id, as hex;
//                                channel 'sound' (the default) or 'light'. Rejects with 'not sent' when nothing
//                                went. A switch that won't send it (sound short of Listen and send, light short of
//                                Read and flash) says why on its channel ('not sent: light is off', 'not sent: light is
//                                set to read only'), then the outbox reports it 'failed'
//   broadcast(text, channel)     [broadcast { text, channel }]      resolves to the message id, as hex; channel 'sound' (the
//                                default) or 'light'. Rejects with a sentence to show: 'Write the notice first' (nothing
//                                but spaces), 'Too long to broadcast: N bytes, at most M' (M is machine().broadcast_max).
//                                The channel's switch refuses it as send
//   pair(channel)                [pair { channel }]                 sends this machine's hello on channel, 'sound' (the default)
//                                or 'light': by sound once the room is quiet, by light shown until pairing ends (a peer's
//                                hello arrives as pairOffer). Rejects with the channel's state when its switch won't send,
//                                e.g. 'not sent: sound is set to listen only', 'not sent: light is off'. No switch is
//                                turned for it: the camera and the screen stay as the Light switch has them
//   stopPairing()                [stop_pairing]                     ends the pairing in progress: the hello stops showing.
//                                confirmPair ends it too, and it ends by itself after three minutes
//   finishPairing()              [finish_pairing]                   their hello is in: the hello showing by light finishes its
//                                window (so whoever pressed later still has a whole one to read it), then the screen
//                                clears (displayStopped) and pairing ends; with none showing it ends at once
//   selfTest(channel)            [self_test { channel }]            tests the machine on channel, 'sound' (the default) or
//                                'light': it seals a short message to its own key and sends it, by sound once the room is
//                                quiet, by light on the screen for a mirror held in front of it, and hears it with its own
//                                microphone (or camera). How it goes arrives as selfTest. Nothing of it is stored, sent to
//                                anyone or answered. Rejects as pair does: 'locked', or the channel's state when its switch
//                                won't send ('not sent: sound is set to listen only'). No switch is turned for it
//   stopSelfTest()               [stop_self_test]                   ends the self-test in progress: nothing more of it goes
//                                out, and what is still arriving of it is let go (selfTest 'stopped')
//   eyepiece(on)                 [eyepiece { on }]                  whether to be sent what the camera sees (the eyepiece
//                                event) while Light reads. Off until asked, and not a setting: the page asks while its
//                                eyepiece is shown and lets go when it is hidden, so a hidden one gets no picture at all
//   confirmPair(peerHex, name)   [confirm_pair { peerHex, name }]   resolves to the new contact id; name as the person
//                                corrected it on the card (the hello's name is only a suggestion)
//   setSetting(id, valueIndex)   [set_setting { id, valueIndex }]  only for a change the person just made: the
//                                machine keeps every setting and is their only record. Rejects with a sentence to show
//                                when the machine won't take it, e.g. 'Unlock first, then the lock can come off'.
//                                One exception: Light turned down when the setting can't be saved is in effect for
//                                the session all the same (Escape always stops); it rejects, and settings() says so
//   settingNotes()               [setting_notes]                    resolves to { [id]: { note, value? } } for each setting this
//                                machine can't change: note is what its row says instead ('comes with relay', 'always,
//                                on this build', 'Keychain only on macOS'); value the one it is held at,
//                                absent for a setting that waits for a feature (its plug is parked)
//   settings()                   [settings]                         resolves to { [id]: valueIndex } for every setting on the
//                                settings page, as the machine keeps them (defaults for any never set), locked or not.
//                                Pages render from this at load and never send it back
//   unlock(passphrase, name)     [unlock { passphrase, name }]      rejects with a sentence to show, e.g. 'wrong passphrase'.
//                                name: your own name, needed the first time (it creates the store and goes in your
//                                pairing hellos), ignored after
//   contacts()                   [contacts]                         resolves to [{ id, name, short, checked, safety }]; rejects
//                                with 'locked' (ask for the passphrase) or 'no store yet' (the first passphrase sets it).
//                                safety: the 60-digit safety number shared with them, in 12 groups of five
//   conversation(contactId)      [conversation { contactId }]       resolves to the stored messages, oldest first, each
//                                shaped as the message event
//   broadcasts()                 [broadcasts]                       resolves to the broadcasts kept, this machine's own and the
//                                ones heard, oldest first, each shaped as the broadcast event, with state for its own
//                                ('waiting' queued again on unlock, 'sent' or 'failed')
//   machine()                    [machine]                          resolves to { name, key, you, held, version, commit, broadcast_max }:
//                                name is the machine's plate (Machine No. 1), key its public identity as hex (sign key
//                                first, so its first four digits are the short fingerprint broadcasts are signed with),
//                                you the person's own name, held the messages it carries for others, version and commit
//                                the build (commit its short hash, + when built from uncommitted changes, 'unknown'
//                                without git, 'browser' in the simulator), broadcast_max the longest broadcast in UTF-8 bytes
//   channels()                   [channels]                         resolves to [{ channel, state, rate? }]: the last state each
//                                channel reported, for a page that opened after the event. None reported: listening.
//                                rate: the payload bytes a second the channel moves (sound at its band, light at the
//                                Flash speed), for the bytes-a-second readout. band (sound): 'near silent' or 'audible',
//                                the band it is really on (a sound card too slow for near silent stays audible, whatever
//                                Tone band says)
//
// Requests to connect, from strangers near enough for a carrier that reaches people one by one (none on the airgapped
// build, where these answer as for nobody near). Held in memory only, never stored:
//   requests()                   [requests]                         resolves to the requests waiting for the person, oldest
//                                first, each shaped as the request event; rejects with 'locked' until the store is open
//   acceptRequest(id, name)      [accept_request { id, name }]      resolves to the new contact id: kept under name (as the
//                                person corrected it), unchecked until keys are compared in person, and not confirmed
//                                until they show they hold this machine too (contactConfirmed). Rejects with a sentence
//                                to show: 'locked', 'Write their name first', 'That request is no longer waiting.', or 'Meet them
//                                and compare keys first: this machine only adds people checked in person.'
//   declineRequest(id)           [decline_request { id }]           forgets the request; nothing at all goes back. Rejects
//                                with 'locked' until the store is open, as every command here does
//   sendRequest(label)           [send_request { label }]           asks the stranger under label (from strangers()) to
//                                connect, with this machine's hello alone; how it goes arrives as requested. Rejects with
//                                a sentence to show: 'locked', the key check one above, 'They are no longer in range.',
//                                or the carrier's own (Bluetooth not at Open)
//   strangers()                  [strangers]                        resolves to the strangers a request can go to now, each
//                                by a short random label, never anything of theirs; rejects with 'locked' until unlocked
//   peers()                      [peers]                            resolves to who is near, as the last peers event said;
//                                rejects with 'locked' until unlocked
//   unconfirmed()                [unconfirmed]                      resolves to the ids of contacts an accept added that have
//                                not yet shown they hold this machine too
//
// Events (on(name, fn)), as the node's NodeEvent after normalising:
//   chunk      { tag, index, of, view: { plain, heard, key }, text, channel }
//                channel: the one it arrived on, 'sound' or 'light' (light envelopes land whole, so their
//                chunks come all at once)
//                plain: one letter per byte; ASCII letters upper-cased, every other byte X (the Bombe's spacer)
//                heard, key: one letter per byte, by letter(b)
//                text: the message text decrypted so far under this tag, NOT yet checked (see below)
//   message    { id, contact, outgoing, text, channel, at_min, heard, state }
//                state: an outgoing message's delivery as the store keeps it, 'waiting', 'sent' or
//                'failed' (received messages are 'sent'); absent in the simulator
//   heard      { id, by }
//   broadcast  { id, outgoing, signer, text, verified, hops, at_min, state? }
//                                                               signer: hex of the signing key; heard ones arrive as
//                                                               events, kept ones (and this machine's own) from broadcasts()
//   pairOffer  { peer, name, safety, channel }                  peer: hex of sign key then dh key; channel: the one the
//                                                               hello came in on, 'sound' or 'light'
//   roomDb     number
//   channel    { channel, state }       state 'receiving' while a message arrives; 'failed' when one
//                                       stops arriving or fails its checks (the state it is then in follows);
//                                       'not sent: sound is set to listen only' (or '...: sound is off', '...: light is
//                                       off', '...: light is set to read only') just before the outbox 'failed' of a send
//                                       its switch refused (then the state it is in); 'off' with the switch at Off;
//                                       light: 'seeing a code: tilt or dim the screen' while a code is in view but can't
//                                       be read (until one reads), 'unavailable: camera permission denied' (or '...: camera
//                                       permission not answered', '...: no camera', '...: camera in use by another app');
//                                       anything else (e.g. 'listening', 'unavailable: ...') otherwise
//   outbox     { id, state, text?, public? }   a send's progress: 'waiting' (for the room to fall under Send under),
//                                       'playing', 'sent', or 'failed' (it will never go out). text and public come with the first report of a send
//                                       this page didn't make (one queued before it opened); the simulator sends
//                                       them, the node does not yet, so in the app such a send shows no slip.
//   display    { tag, bits, spec, index, hello, test }  light: put this code frame on screen now, in place of the last. bits: the
//                                       whole grid, one bit a cell (dark 1), row-major from the top left, most
//                                       significant bit first; the grid is 48, 56 or 64 cells a side for spec 0 (slow),
//                                       1 (normal) or 2 (fast). index counts the frames of envelope tag shown so far.
//                                       Public code bits only, never a key. Not held before ready(): a frame is only
//                                       worth showing when it comes. hello: the frame is of this machine's pairing hello,
//                                       not a message. test: the frame is of this machine's self-test, for a mirror
//   displayStopped { tag }             light: clear the screen; tag's frame was on it
//   selfTest   { tag, channel, state, text?, bytes?, ms?, rate?, band? }  the self-test's progress. state: 'waiting' (for
//                                       a quiet room, as any send), 'playing' (by light: on screen, again under a fresh tag
//                                       each time its window ends), 'heard' (with the text that came back, the envelope's
//                                       bytes, the ms from its first play to its last chunk opening, the rate that makes in
//                                       bytes a second and, by sound, the band), 'unheard' (nothing came back in time),
//                                       'unplayed' (the carrier would not take it) or 'stopped'. tag: the one it goes out
//                                       under, so its chunk events (which turn the drums as any message's) can be told apart
//   eyepiece   { width, height, pixels }  light: what the camera sees, small and grey (64 pixels wide, one byte a
//                                       pixel, row-major), about four a second while Light reads and eyepiece(true)
//                                       was asked. Shown, never kept. Not held before ready()
//   request    { id, name, fingerprint, safety }  a stranger asks to connect: name as their hello gives it (only a suggestion),
//                                       fingerprint the first 8 bytes of their signing key as hex, safety the safety number
//                                       to compare if they meet
//   requestGone { id }                 that request can no longer be answered (it ran out, they left, or it was answered)
//   requested  { label, state, contact }  how a request this machine sent is going: 'delivered'; then 'unanswered' (it
//                                       was delivered and ran out, or the connection went, with no answer: a decline is
//                                       never told, so it ends this way too) or 'accepted' with the new contact's id;
//                                       'failed' only for a request that never reached them
//   peers      { contacts, strangers }  who is near: contact ids in range, and how many strangers were seen lately (an
//                                       upper bound). Also sent when the strangers that can be asked change, the counts
//                                       as they were: strangers() is read afresh on every one
//   contactConfirmed { contact }       a contact an accept added has shown it holds this machine too
// Ids arrive as byte arrays or hex and leave as lowercase hex. Channels arrive as "Sound" or "sound" and leave lower case.
//
// A chunk is only known to come from a holder of the content key until the
// message event that follows it. The drums may show it, and the sheet may show
// its text as provisional tape, but only `message` puts it on the sheet for good.
//
// Events are held from the moment the bridge loads until the page calls ready()
// (or start()), so nothing that arrives while the page is still setting up
// (asking for the passphrase, say) is lost; then they are delivered in order.
// start() is ready() plus, in a plain browser, beginning the simulated
// conversation. now() is the clock the sheet writes times with, in ms.
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
const person = c => ({ id: contactId(c.id), name: String(c.name), short: String(c.name).split(' ')[0], checked: !!c.checked, safety: c.safety || '' });

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
  // a later chunk of a tag whose start was missed (the page opened mid-message): its bytes
  // don't start at the sender, so they can't be cut to the text
  if (index > 0 && !arriving.has(tag)) return '';
  const a = index === 0 ? { so: [], size: 0 } : arriving.get(tag);
  if (index < of - 1) a.size = plain.length;
  a.so.push(...plain); arriving.set(tag, a);
  const last = index === of - 1;
  const end = last ? a.so.length - SIGNATURE_LEN : Math.min(a.so.length, (of - 1) * a.size + 1 - SIGNATURE_LEN);
  if (end <= SENDER_LEN) return '';
  return decoder.decode(new Uint8Array(a.so.slice(SENDER_LEN, end))).replace(/\uFFFD+$/, '');
}

const EVENTS = ['chunk', 'message', 'heard', 'broadcast', 'pairOffer', 'roomDb', 'channel', 'outbox', 'display', 'displayStopped', 'eyepiece', 'selfTest', 'request', 'requestGone', 'requested', 'peers', 'contactConfirmed'];
// shown as they come, or not at all: never held for a page still setting up
const LIVE = new Set(['display', 'displayStopped', 'eyepiece']);
const normalise = {
  chunk: c => {
    const plain = bytesOf(c.view.plain);
    const text = provisional(c.tag, c.index, c.of, plain);
    if (c.index === c.of - 1) arriving.delete(c.tag);
    return { tag: c.tag, index: c.index, of: c.of, text, channel: lower(c.channel) || 'sound', view: { plain: plain.map(plainLetter).join(''), heard: bytesOf(c.view.heard).map(letter).join(''), key: bytesOf(c.view.key).map(letter).join('') } };
  },
  message: m => ({ ...m, id: hex(m.id), contact: contactId(m.contact), channel: lower(m.channel) }),
  heard: h => ({ id: hex(h.id), by: contactId(h.by) }),
  broadcast: b => ({ ...b, id: hex(b.id), outgoing: !!b.outgoing, signer: b.signer && b.signer.sign ? hex(b.signer.sign) : hex(b.signer) }),
  pairOffer: p => ({ ...p, peer: identity(p.peer), channel: lower(p.channel) || 'sound' }),
  roomDb: d => +(d && typeof d === 'object' ? d.db : d),
  channel: s => ({ ...s, channel: lower(s.channel) }),
  outbox: o => ({ ...o, id: hex(o.id), state: lower(o.state) }),
  display: d => ({ tag: +d.tag, bits: bytesOf(d.bits), spec: +d.spec, index: +d.index, hello: !!d.hello, test: !!d.test }),
  displayStopped: d => ({ tag: d && typeof d === 'object' ? +d.tag : +d }),
  eyepiece: e => ({ width: +e.width, height: +e.height, pixels: bytesOf(e.pixels) }),
  selfTest: t => ({ ...t, tag: +t.tag, channel: lower(t.channel) || 'sound', state: lower(t.state) }),
  request: r => ({ id: +r.id, name: String(r.name), fingerprint: hex(r.fingerprint), safety: r.safety || '' }),
  requestGone: r => ({ id: +r.id }),
  requested: r => ({ label: String(r.label), state: lower(r.state), contact: r.contact == null ? null : contactId(r.contact) }),
  peers: p => ({ contacts: Array.from(p.contacts || [], contactId), strangers: +p.strangers || 0 }),
  contactConfirmed: c => ({ contact: contactId(c.contact) }),
};

const handlers = Object.fromEntries(EVENTS.map(e => [e, new Set()]));
const HELD_MAX = 2000;
let held = [];// events that arrived before ready(), in order
function deliver(event, raw) {
  if (!handlers[event]) return;
  if (held) { if (LIVE.has(event)) return; held.push([event, raw]); if (held.length > HELD_MAX) held.shift(); return; }
  const payload = normalise[event](raw);
  for (const fn of handlers[event]) fn(payload);
}
function ready() {
  if (!held) return;
  const early = held; held = null;
  for (const [event, raw] of early) deliver(event, raw);
}

const tauri = typeof window !== 'undefined' && window.__TAURI__ && window.__TAURI__.core ? window.__TAURI__ : null;

function tauriBackend() {
  const { invoke } = tauri.core;
  // every event from the start, so none is missed while the page sets up
  for (const event of EVENTS) tauri.event.listen(event, e => deliver(event, e.payload));
  return {
    send: (contactIds, text, channel) => invoke('send', { contactIds, text, channel }).then(hex),
    broadcast: (text, channel) => invoke('broadcast', { text, channel }).then(hex),
    pair: channel => invoke('pair', { channel }),
    stopPairing: () => invoke('stop_pairing'),
    finishPairing: () => invoke('finish_pairing'),
    selfTest: channel => invoke('self_test', { channel }),
    stopSelfTest: () => invoke('stop_self_test'),
    eyepiece: on => invoke('eyepiece', { on: !!on }),
    confirmPair: (peerHex, name) => invoke('confirm_pair', { peerHex, name }).then(contactId),
    setSetting: (id, valueIndex) => invoke('set_setting', { id, valueIndex }),
    settings: () => invoke('settings'),
    settingNotes: () => invoke('setting_notes'),
    unlock: (passphrase, name) => invoke('unlock', name === undefined ? { passphrase } : { passphrase, name }),
    contacts: () => invoke('contacts').then(list => list.map(person)),
    conversation: contactId => invoke('conversation', { contactId }).then(list => list.map(normalise.message)),
    broadcasts: () => invoke('broadcasts').then(list => list.map(normalise.broadcast)),
    machine: () => invoke('machine'),
    channels: () => invoke('channels').then(list => list.map(normalise.channel)),
    requests: () => invoke('requests').then(list => list.map(normalise.request)),
    acceptRequest: (id, name) => invoke('accept_request', { id, name }).then(contactId),
    declineRequest: id => invoke('decline_request', { id }),
    sendRequest: label => invoke('send_request', { label }),
    strangers: () => invoke('strangers').then(list => list.map(String)),
    peers: () => invoke('peers').then(normalise.peers),
    unconfirmed: () => invoke('unconfirmed').then(list => list.map(contactId)),
    start() {},
    now: () => Date.now(),
  };
}

function simulatedBackend() {
  const sim = createSimulator(deliver, { query: new URLSearchParams(location.search) });
  window.__sim = sim.test;
  return {
    send: (contactIds, text, channel) => sim.send(contactIds, text, channel).then(hex),
    broadcast: (text, channel) => sim.broadcast(text, channel).then(hex),
    pair: channel => sim.pair(channel),
    stopPairing: () => sim.stopPairing(),
    finishPairing: () => sim.finishPairing(),
    selfTest: channel => sim.selfTest(channel),
    stopSelfTest: () => sim.stopSelfTest(),
    eyepiece: on => sim.eyepiece(!!on),
    confirmPair: (peerHex, name) => sim.confirmPair(peerHex, name).then(contactId),
    setSetting: (id, valueIndex) => sim.setSetting(id, valueIndex),
    settings: () => sim.settings(),
    settingNotes: () => sim.settingNotes(),
    unlock: (passphrase, name) => sim.unlock(passphrase, name),
    contacts: () => sim.contacts().then(list => list.map(person)),
    conversation: contactId => sim.conversation(contactId).then(list => list.map(normalise.message)),
    broadcasts: () => sim.broadcasts().then(list => list.map(normalise.broadcast)),
    machine: () => sim.machine(),
    channels: () => sim.channels().then(list => list.map(normalise.channel)),
    requests: () => sim.requests().then(list => list.map(normalise.request)),
    acceptRequest: (id, name) => sim.acceptRequest(id, name).then(contactId),
    declineRequest: id => sim.declineRequest(id),
    sendRequest: label => sim.sendRequest(label),
    strangers: () => sim.strangers().then(list => list.map(String)),
    peers: () => sim.peers().then(normalise.peers),
    unconfirmed: () => sim.unconfirmed().then(list => list.map(contactId)),
    start: () => sim.start(),
    now: () => sim.now(),
  };
}

const backend = tauri ? tauriBackend() : simulatedBackend();

export const bridge = {
  simulated: !tauri,
  send: (contactIds, text, channel = 'sound') => backend.send(contactIds, text, lower(channel)),
  broadcast: (text, channel = 'sound') => backend.broadcast(text, lower(channel)),
  pair: (channel = 'sound') => backend.pair(lower(channel)),
  stopPairing: () => backend.stopPairing(),
  finishPairing: () => backend.finishPairing(),
  selfTest: (channel = 'sound') => backend.selfTest(lower(channel)),
  stopSelfTest: () => backend.stopSelfTest(),
  eyepiece: on => backend.eyepiece(on),
  confirmPair: (peerHex, name) => backend.confirmPair(peerHex, name),
  setSetting: (id, valueIndex) => backend.setSetting(id, valueIndex),
  settings: () => backend.settings(),
  settingNotes: () => backend.settingNotes(),
  unlock: (passphrase, name) => backend.unlock(passphrase, name),
  contacts: () => backend.contacts(),
  conversation: contactId => backend.conversation(contactId),
  broadcasts: () => backend.broadcasts(),
  machine: () => backend.machine(),
  channels: () => backend.channels(),
  requests: () => backend.requests(),
  acceptRequest: (id, name) => backend.acceptRequest(+id, String(name)),
  declineRequest: id => backend.declineRequest(+id),
  sendRequest: label => backend.sendRequest(String(label)),
  strangers: () => backend.strangers(),
  peers: () => backend.peers(),
  unconfirmed: () => backend.unconfirmed(),
  on(event, fn) {
    if (!handlers[event]) throw new Error('unknown bridge event: ' + event);
    handlers[event].add(fn);
    return () => handlers[event].delete(fn);
  },
  ready,
  start: () => { ready(); backend.start(); },
  now: () => backend.now(),
};

// What the top plates on both pages say of the build, from machine(): its version and the commit it was made from.
export const buildPlate = m => `Build ${m.version} \u00b7 ${m.commit}`;
