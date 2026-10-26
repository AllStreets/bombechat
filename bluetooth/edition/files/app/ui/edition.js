// What the standard build fits to the machine beyond sound and light: Bluetooth. This file is laid over the root's
// app/ui/edition.js when bluetooth/ is synced; every other page is shared.
//
// The Bluetooth switch is the setting 'bluetooth' (0 Off, 1 Contacts, 2 Open), Off until the person turns it. Its
// channel ('bluetooth', in channels() and the channel event) says what the switch has it doing: 'off', 'contacts only'
// or 'open to requests'; 'receiving' while a message arrives; 'unavailable: ' and a sentence the person can act on
// while the radio can't work ('unavailable: Bluetooth is off in System Settings.'), until the switch's own word says it
// works again. It measures no rate, so it has none. A send by Bluetooth at Off says 'not sent: bluetooth is off'.
// Requests to connect (the request, requestGone, requested, peers and contactConfirmed events, and the commands beside
// them in bridge.js) come only at Open; at Contacts the switch's own sentence refuses asking: 'Requests go out only with
// Bluetooth set to Open.' Accepting is a second way to add a contact, so New contacts need a key check is the person's
// choice here, On until they change it.

// The Bluetooth row on the back panel's wiring schedule, in the settings page's own format.
export const SETTING_ROWS = [
 {id:'bluetooth',g:'Bluetooth',name:'Bluetooth switch',values:['Off','Contacts','Open'],v:0,desc:'Contacts reaches the people you have added when they are near. Open also lets strangers nearby ask to connect, and lets you ask them.'},
];

// Requests to connect add contacts without meeting, so the key check is a choice, as the device's edition.rs has it.
export const ONLY_IN_PERSON = false;

// The switch's words for the channel, by position, as the device's edition.rs says them
const SAYS = ['off', 'contacts only', 'open to requests'];
// what the node and the transport say, word for word
const MEET_FIRST = 'Meet them and compare keys first: this machine only adds people checked in person.';
const REQUEST_GONE = 'That request is no longer waiting.';
const NOT_IN_RANGE = 'They are no longer in range.';
const NEED_OPEN = 'Requests go out only with Bluetooth set to Open.';
const SEND_OFF = 'not sent: bluetooth is off';
// Marisol, a contact, is in range whenever Bluetooth is on
const MARISOL = 1;
// Ines, a stranger in range, under a label of the node's letters; she becomes this contact if either side accepts
const INES = 3, INES_LABEL = 'K7MX', INES_NAME = 'Ines Duarte';
// how long after the switch reaches Open she asks (?askin=ms), and how long proofs and answers take over the air
const ASK_MS = 6000, PROOF_MS = 1200, ANSWER_MS = 2400, DELIVER_MS = 600;

// The simulator's Bluetooth: Marisol in range, Ines a stranger in range who asks to connect once the switch is at
// Open, accept, decline and the key check's refusal, sends by Bluetooth, and who is near, all through the events the
// node sends. window.__sim gains bluetoothAsk() (Ines asks now), bluetoothUnavailable(why) and bluetoothAvailable().
export function simulate({ emit, settings, people, test, later, seeded, outbox }) {
  const query = typeof location === 'undefined' ? new URLSearchParams() : new URLSearchParams(location.search);
  const still = query.has('still');
  const askIn = query.has('askin') ? +query.get('askin') : ASK_MS;
  const r = seeded(4711);
  const fingerprint = Array.from({ length: 8 }, r);
  const safety = Array.from({ length: 12 }, () => Array.from({ length: 5 }, () => r() % 10).join('')).join(' ');
  let unavailable = null;// 'unavailable: ...' while the radio can't work
  let asked = false, askT = null, nextId = 0, said = null;
  const waiting = [];// requests waiting for the person
  const unconfirmed = [];
  const at = () => settings.bluetooth || 0;
  const on = () => at() > 0 && !unavailable;
  const state = () => at() > 0 && unavailable ? unavailable : SAYS[at()];
  const say = s => emit('channel', { channel: 'Bluetooth', state: s });
  const inesKnown = () => people.some(p => p.id === INES);
  const strangers = () => at() === 2 && on() && !inesKnown() ? [INES_LABEL] : [];
  // who is near, told when it changes, as the node tells it
  function near() {
    const now = { contacts: on() ? people.filter(p => p.id === MARISOL || (p.id === INES && !unconfirmed.includes(INES))).map(p => p.id) : [], strangers: on() && !inesKnown() ? 1 : 0 };
    const key = JSON.stringify(now);
    if (key !== said) { said = key; emit('peers', now); }
    return now;
  }
  function endRequests() { for (const w of waiting.splice(0)) emit('requestGone', { id: w.id }); }
  function ask() {
    clearTimeout(askT); askT = null;
    if (at() !== 2 || !on() || asked || inesKnown()) return;
    asked = true;
    const w = { id: ++nextId, name: INES_NAME, fingerprint, safety };
    waiting.push(w);
    emit('request', w);
  }
  function scheduleAsk() { if (!still && !asked && !askT && at() === 2) askT = later(ask, askIn); }
  function addInes(name) {
    if (!inesKnown()) people.push({ id: INES, name, checked: false });
    for (const w of waiting.splice(0)) emit('requestGone', { id: w.id });
  }

  Object.assign(test, {
    bluetoothAsk() { asked = false; ask(); },
    bluetoothUnavailable(why) { unavailable = 'unavailable: ' + why; if (at() > 0) { say(unavailable); endRequests(); near(); } },
    bluetoothAvailable() { if (!unavailable) return; unavailable = null; if (at() > 0) { say(state()); near(); scheduleAsk(); } },
  });

  return {
    start() { say(state()); near(); scheduleAsk(); },
    setting(id, value, was) {
      if (id !== 'bluetooth' || value === was) return;
      if (value === 0) unavailable = null;
      later(() => {
        say(state());
        if (value < 2) { endRequests(); clearTimeout(askT); askT = null; }
        near(); scheduleAsk();
      });
    },
    channels: () => [{ channel: 'Bluetooth', state: state() }],
    carries: channel => channel === 'bluetooth',
    // refused at Off as the node refuses it: the channel says why, the outbox fails, then the state; otherwise it goes
    // to whoever of them is in range, and they say they have it
    send(m, contactIds) {
      if (at() === 0) { later(() => { say(SEND_OFF); outbox(m, 'failed'); say(state()); }); return; }
      later(() => outbox(m, 'waiting'));
      const here = near().contacts.filter(c => contactIds.includes(c));
      if (!here.length) return;
      later(() => { outbox(m, 'sent'); for (const by of here) emit('heard', { id: m.id, by }); }, DELIVER_MS);
    },
    commands: {
      requests: () => Promise.resolve(waiting.map(w => ({ ...w }))),
      acceptRequest(id, name) {
        const w = waiting.find(q => q.id === +id);
        if (!w) return Promise.reject(REQUEST_GONE);
        if (settings.inperson) return Promise.reject(MEET_FIRST);
        if (!String(name).trim()) return Promise.reject('Write their name first');
        addInes(String(name).trim().slice(0, 40));
        unconfirmed.push(INES);
        // her own answer shows she took this machine too
        later(() => { const i = unconfirmed.indexOf(INES); if (i < 0 || !on()) return; unconfirmed.splice(i, 1); emit('contactConfirmed', { contact: INES }); near(); }, PROOF_MS);
        near();
        return Promise.resolve(INES);
      },
      declineRequest(id) {
        const i = waiting.findIndex(q => q.id === +id);
        if (i >= 0) { waiting.splice(i, 1); emit('requestGone', { id: +id }); }
        return Promise.resolve();
      },
      sendRequest(label) {
        if (settings.inperson) return Promise.reject(MEET_FIRST);
        if (at() !== 2) return Promise.reject(NEED_OPEN);
        if (!strangers().includes(label)) return Promise.reject(NOT_IN_RANGE);
        later(() => emit('requested', { label, state: 'delivered', contact: null }), DELIVER_MS);
        later(() => {
          if (!on()) { emit('requested', { label, state: 'failed', contact: null }); return; }
          addInes(INES_NAME);
          emit('requested', { label, state: 'accepted', contact: INES });
          near();
        }, ANSWER_MS);
        return Promise.resolve();
      },
      strangers: () => Promise.resolve(strangers()),
      peers: () => Promise.resolve(near()),
      unconfirmed: () => Promise.resolve(unconfirmed.slice()),
    },
  };
}

// The front's hook until the Bluetooth face is drawn: the Bluetooth tile's jewel lights while the switch has the radio
// working, and the tile's title says the state, who is near and how many requests wait. The tile carries the state as
// data-bluetooth for checks.
export function front(bridge) {
  const plate = typeof document !== 'undefined' && document.querySelector('.mod .blank');
  if (!plate) return;
  const tile = plate.closest('.mod'), jewel = tile.querySelector('.jewel');
  let state = 'off', peers = { contacts: [], strangers: 0 };
  const waiting = new Set();
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
  function draw() {
    const lit = state === SAYS[1] || state === SAYS[2] || state === 'receiving';
    jewel.classList.toggle('off', !lit);
    jewel.style.opacity = lit ? '' : '.25';
    tile.dataset.bluetooth = state;
    const what = /^unavailable: /.test(state) ? state.slice(13).replace(/\.$/, '') : `Bluetooth: ${state}`;
    const parts = [what];
    if (lit) parts.push(`${plural(peers.contacts.length, 'contact', 'contacts')} in range, ${plural(peers.strangers, 'stranger', 'strangers')} seen lately`);
    if (waiting.size) parts.push(plural(waiting.size, 'request waits', 'requests wait'));
    tile.title = parts.join('. ') + '.';
  }
  bridge.on('channel', s => { if (s.channel !== 'bluetooth' || /^not sent: /.test(s.state)) return; state = s.state; draw(); });
  bridge.on('peers', p => { peers = p; draw(); });
  bridge.on('request', q => { waiting.add(q.id); draw(); });
  bridge.on('requestGone', q => { waiting.delete(q.id); draw(); });
  bridge.channels().then(list => { const b = list.find(c => c.channel === 'bluetooth'); if (b) { state = b.state; draw(); } }, () => {});
  bridge.peers().then(p => { peers = p; draw(); }, () => {});
  bridge.requests().then(list => { list.forEach(q => waiting.add(q.id)); draw(); }, () => {});
  draw();
}
