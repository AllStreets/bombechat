// What the standard build fits to the machine beyond sound and light: Bluetooth. This file is laid over the root's
// app/ui/edition.js when bluetooth/ is synced; every other page is shared.
//
// The Bluetooth switch is the setting 'bluetooth' (0 Off, 1 Contacts, 2 Open), Off until the person turns it. Its
// channel ('bluetooth', in channels() and the channel event) says what the switch has it doing: 'off', 'contacts only'
// or 'open to requests'; 'receiving' while a message arrives; 'unavailable: ' and a sentence the person can act on
// while the radio can't work ('unavailable: Bluetooth is off in System Settings.'), until the switch's own word says it
// works again; and at Contacts or Open while the store is locked, 'waiting for the passphrase' (the radio has no keys
// yet, so it is idle). It measures no rate, so it has none. A send by Bluetooth at Off says 'not sent: bluetooth is off'.
// Requests to connect (the request, requestGone, requested, peers and contactConfirmed events, and the commands beside
// them in bridge.js) come only at Open; at Contacts the switch's own sentence refuses asking: 'Requests go out only with
// Bluetooth set to Open.' Accepting is a second way to add a contact, so New contacts need a key check is the person's
// choice here, On until they change it.

// The Bluetooth row on the back panel's wiring schedule, in the settings page's own format.
export const SETTING_ROWS = [
 {id:'bluetooth',g:'Bluetooth',name:'Bluetooth switch',values:['Off','Contacts','Open'],v:0,desc:'Contacts reaches the people you have added when they are near. Open also lets strangers nearby ask to connect, and lets you ask them.'},
];

// What the wiring schedule types under a group's name. The Mac's own name is readable by anyone in Bluetooth range,
// Bombechat or not, so the Bluetooth group says so.
export const GROUP_NOTES = {
  Bluetooth: 'Anyone in Bluetooth range can read this Mac\'s name. Give it a plain name that doesn\'t say who you are, in System Settings, General, About.',
};

// Requests to connect add contacts without meeting, so the key check is a choice, as the device's edition.rs has it.
export const ONLY_IN_PERSON = false;

// The switch's words for the channel, by position, as the device's edition.rs says them
const SAYS = ['off', 'contacts only', 'open to requests'];
const WAITING = 'waiting for the passphrase';
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
// anyone else who asks (bluetoothAsk(name)) becomes a contact from this id on
const OTHERS = 10;
// how long after the switch reaches Open she asks (?askin=ms), and how long proofs and answers take over the air
const ASK_MS = 6000, PROOF_MS = 1200, ANSWER_MS = 2400, DELIVER_MS = 600;

// The simulator's Bluetooth: Marisol in range, Ines a stranger in range who asks to connect once the switch is at
// Open, accept, decline and the key check's refusal, sends by Bluetooth, and who is near, all through the events the
// node sends. window.__sim gains bluetoothAway(id) and bluetoothBack(id) (a contact leaves range and comes back; what
// waits for them goes when they are back), bluetoothAsk(name) (Ines asks now, or someone else under that name),
// bluetoothStrangers(labels, failing) (more strangers in range at Open, who never answer; a request to one in failing
// is not delivered), bluetoothUnavailable(why), bluetoothAvailable(), bluetoothLocked() and bluetoothUnlocked().
export function simulate({ emit, settings, people, test, later, seeded, outbox }) {
  const query = typeof location === 'undefined' ? new URLSearchParams() : new URLSearchParams(location.search);
  const still = query.has('still');
  const askIn = query.has('askin') ? +query.get('askin') : ASK_MS;
  const r = seeded(4711);
  const print = () => Array.from({ length: 8 }, r);
  const number = () => Array.from({ length: 12 }, () => Array.from({ length: 5 }, () => r() % 10).join('')).join(' ');
  const fingerprint = print(), safety = number();
  let unavailable = null;// 'unavailable: ...' while the radio can't work
  let locked = false;// the store is locked: the radio waits for the passphrase
  let asked = false, askT = null, nextId = 0, said = null, nextOther = OTHERS;
  const waiting = [];// requests waiting for the person, each with the contact id it becomes
  const unconfirmed = [];
  const inRange = new Set([MARISOL]);// contacts in range while the radio works
  let extra = [], failing = [];// more strangers at Open, who never answer
  const away = new Set();// contacts out of range for now
  const queued = [];// [m, contact id] sends waiting for someone out of range
  const at = () => settings.bluetooth || 0;
  const on = () => at() > 0 && !unavailable && !locked;
  const state = () => at() === 0 ? SAYS[0] : locked ? WAITING : unavailable || SAYS[at()];
  const say = s => emit('channel', { channel: 'Bluetooth', state: s });
  const inesKnown = () => people.some(p => p.id === INES);
  const strangers = () => at() === 2 && on() ? [...(inesKnown() ? [] : [INES_LABEL]), ...extra] : [];
  // who is near, told when it changes, as the node tells it
  function near() {
    const now = { contacts: on() ? people.filter(p => inRange.has(p.id) && !away.has(p.id) && !unconfirmed.includes(p.id)).map(p => p.id) : [],
      strangers: on() ? (inesKnown() ? 0 : 1) + (at() === 2 ? extra.length : 0) : 0 };
    const key = JSON.stringify(now);
    if (key !== said) { said = key; emit('peers', now); }
    return now;
  }
  function endRequests() { for (const w of waiting.splice(0)) emit('requestGone', { id: w.id }); }
  function request(w) { waiting.push(w); emit('request', { id: w.id, name: w.name, fingerprint: w.fingerprint, safety: w.safety }); }
  function ask() {
    clearTimeout(askT); askT = null;
    if (at() !== 2 || !on() || asked || inesKnown()) return;
    asked = true;
    request({ id: ++nextId, name: INES_NAME, fingerprint, safety, becomes: INES });
  }
  function scheduleAsk() { if (!still && !asked && !askT && at() === 2) askT = later(ask, askIn); }
  function add(id, name) { if (!people.some(p => p.id === id)) people.push({ id, name, checked: false }); }
  function settle() { say(state()); if (!on()) endRequests(); near(); }
  // a message goes to whoever it is for once they are in range, and they say they have it
  function deliver(m, c, here) {
    if (!here.includes(c)) { queued.push([m, c]); return; }
    later(() => { outbox(m, 'sent'); emit('heard', { id: m.id, by: c }); }, DELIVER_MS);
  }

  Object.assign(test, {
    bluetoothAsk(name) {
      if (!name || name === INES_NAME) { asked = false; ask(); return; }
      if (at() === 2 && on()) request({ id: ++nextId, name, fingerprint: print(), safety: number(), becomes: nextOther++ });
    },
    bluetoothAway(id) { away.add(id); near(); },
    bluetoothBack(id) { away.delete(id); const here = near().contacts; for (const [m, c] of queued.splice(0)) deliver(m, c, here); },
    bluetoothStrangers(labels, fail = []) { extra = labels.slice(); failing = fail.slice(); near(); },
    bluetoothUnavailable(why) { unavailable = 'unavailable: ' + why; if (at() > 0) settle(); },
    bluetoothAvailable() { if (!unavailable) return; unavailable = null; if (at() > 0) { settle(); scheduleAsk(); } },
    bluetoothLocked() { locked = true; if (at() > 0) settle(); },
    bluetoothUnlocked() { if (!locked) return; locked = false; if (at() > 0) { settle(); scheduleAsk(); } },
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
      // who is in range is known when it goes, a moment after it is written
      later(() => { const here = near().contacts; for (const c of contactIds) deliver(m, c, here); }, DELIVER_MS / 2);
    },
    commands: {
      requests: () => Promise.resolve(waiting.map(w => ({ id: w.id, name: w.name, fingerprint: w.fingerprint, safety: w.safety }))),
      acceptRequest(id, name) {
        const i = waiting.findIndex(q => q.id === +id);
        if (i < 0) return Promise.reject(REQUEST_GONE);
        if (settings.inperson) return Promise.reject(MEET_FIRST);
        if (!String(name).trim()) return Promise.reject('Write their name first');
        const [w] = waiting.splice(i, 1), cid = w.becomes;
        add(cid, String(name).trim().slice(0, 40));
        emit('requestGone', { id: w.id });
        unconfirmed.push(cid); inRange.add(cid);
        // their own answer shows they took this machine too
        later(() => { const k = unconfirmed.indexOf(cid); if (k < 0 || !on()) return; unconfirmed.splice(k, 1); emit('contactConfirmed', { contact: cid }); near(); }, PROOF_MS);
        near();
        return Promise.resolve(cid);
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
        if (label !== INES_LABEL) {
          const fails = failing.includes(label);
          later(() => emit('requested', { label, state: fails ? 'failed' : 'delivered', contact: null }), DELIVER_MS);
          return Promise.resolve();
        }
        later(() => emit('requested', { label, state: 'delivered', contact: null }), DELIVER_MS);
        later(() => {
          if (!on()) { emit('requested', { label, state: 'failed', contact: null }); return; }
          add(INES, INES_NAME); inRange.add(INES);
          for (const w of waiting.filter(q => q.becomes === INES)) { waiting.splice(waiting.indexOf(w), 1); emit('requestGone', { id: w.id }); }
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

// The front of the machine on this build: the Bluetooth switch where the airgapped build has its blanking plate, who
// is in range on the bank's foot, and a tray above the outbox for requests to connect and strangers to ask. Called
// once the page is up, before the bridge starts, with the bridge and what the page lends it (bombechat.html):
// knob(id, spec) fits a rotary switch built as Sound and Light are; say(text, full) writes on the top bar;
// contactAdded(p) puts a new contact on the page; onStations(fn) is told each time the stations are drawn; and
// showUnchecked(name, safety, who, note) lays the number to compare on the key check card.
//
// The channel's word is the machine's: the jewel is lit only while the radio works (contacts only, open to requests,
// receiving), the top plate names the state, and a strip of tape under the switch (as under Light's) says why it
// can't work, or that it waits for the passphrase. The tile carries the state as data-bluetooth for checks.
export function front(bridge, kit) {
  const blank = typeof document !== 'undefined' && kit && document.querySelector('.mod .blank');
  if (!blank) return;
  const RM = matchMedia('(prefers-reduced-motion: reduce)');
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const sentence = s => { s = String(s); return s.charAt(0).toUpperCase() + s.slice(1) + (/[.!?]$/.test(s) ? '' : '.'); };
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
  const hhmm = ms => { const d = new Date(ms); return String(d.getHours()).padStart(2, '0') + '.' + String(d.getMinutes()).padStart(2, '0'); };

  // the switch, in the blanking plate's place: jewel, knob and plate, as Sound and Light have them
  const tile = document.createElement('div');
  tile.className = 'mod bt';
  tile.innerHTML = '<span class="jewel off" id="j3" aria-hidden="true"></span><button class="knob" id="k3" role="slider" aria-label="Bluetooth" aria-valuemin="0" aria-valuemax="2"></button><span class="pl screw">Bluetooth</span>'
    + '<span class="lightnote btnote" id="btnote" role="status" aria-live="polite"><b></b></span>';
  blank.closest('.mod').replaceWith(tile);
  const jewel = tile.querySelector('.jewel'), note = tile.querySelector('#btnote b');
  const knob = kit.knob('k3', { setting: 'bluetooth', labels: ['Off', 'Contacts', 'Open'], pos: 0, name: 'Bluetooth', say: SAYS.slice() });
  const top = document.getElementById('radio');

  // Send by Bluetooth, beside Flash and Send by sound: lit while it would go now, to the sheet's contact in range
  const contact = () => kit.contact();
  const inRange = () => lit() && peers.contacts.includes(contact().id);
  const sendRefusal = pub => pub ? 'A public notice goes by sound or light, not by Bluetooth'
    : !contact().id ? 'Pair with someone first, then send to them by Bluetooth'
    : /^unavailable: /.test(state) ? state.slice(13).replace(/\.$/, '')
    : state === 'off' ? 'Bluetooth is off. Turn it to Contacts or Open to send this way'
    : state === WAITING ? 'Bluetooth waits for the passphrase'
    : !inRange() ? `${contact().short} is not in Bluetooth range` : null;
  const key = kit.sendKey('bluetooth', { label: 'Send by Bluetooth', refusal: sendRefusal, sending: 'sending by Bluetooth',
    waiting: () => `waiting for ${contact().short} in Bluetooth range`, slip: () => `sends when ${contact().short} is in Bluetooth range` });
  function drawKey() {
    const ok = !sendRefusal(false);
    key.setAttribute('aria-disabled', String(!!sendRefusal(kit.isPublic())));
    key.querySelector('.jewel').classList.toggle('off', !ok);
    key.title = ok ? `Sends the message to ${contact().short}, in Bluetooth range now.` : sentence(sendRefusal(false));
  }

  // who is in range, on the bank's foot with the other readouts: contacts now, and strangers seen lately
  const near = document.createElement('span');
  near.className = 'rd'; near.id = 'btnear'; near.style.visibility = 'hidden';
  near.innerHTML = '<span class="pl">Near</span><span class="odo" id="btcontacts"></span><span class="of">contacts</span><span class="odo" id="btstrangers"></span><span class="of">strangers</span>';
  document.querySelector('.bankfoot').appendChild(near);

  // the tray: a request to connect, then the strangers to ask, then the outbox's own slips, in the outbox's column. It
  // has the room the panel has and no more: what doesn't fit scrolls inside it, so nothing under it moves
  const row2 = document.querySelector('.row2'), outboxEl = row2.querySelector('.outbox'), vu = row2.querySelector('.vu');
  const right = document.createElement('div'), left = document.createElement('div');
  right.className = 'col tray'; left.className = 'col meter';
  row2.insertBefore(right, outboxEl); right.append(outboxEl);
  row2.insertBefore(left, vu); left.append(vu);
  const fade = () => right.classList.toggle('more', right.scrollHeight - right.scrollTop - right.clientHeight > 2);
  right.addEventListener('scroll', fade);
  new ResizeObserver(fade).observe(right);
  const tray = document.createElement('div');
  tray.className = 'intray'; tray.id = 'intray';
  tray.setAttribute('role', 'region'); tray.setAttribute('aria-label', 'Requests to connect');
  right.prepend(tray);
  const nearTray = document.createElement('div');
  nearTray.className = 'intray'; nearTray.id = 'neartray';
  nearTray.setAttribute('role', 'region'); nearTray.setAttribute('aria-label', 'Strangers in range');
  right.insertBefore(nearTray, outboxEl);

  let state = 'off', peers = { contacts: [], strangers: 0 }, strangers = [];
  // New contacts need a key check, as the machine keeps it: On until it says otherwise
  let inPerson = 1;
  const waiting = new Map();// request id -> { id, name, fingerprint, safety, at }
  const asked = new Map();// label -> { state: 'asking', 'delivered', 'failed', 'accepted' or 'refused', why }
  const unconfirmed = new Set();
  const lit = () => state === SAYS[1] || state === SAYS[2] || state === 'receiving';
  const atOpen = () => state === SAYS[2] || (state === 'receiving' && knob.pos === 2);
  const reason = () => /^unavailable: /.test(state) ? state.slice(13) : state === WAITING ? 'Waits for the passphrase.' : '';

  function drawTile() {
    const on = lit();
    jewel.classList.toggle('off', !on);
    tile.dataset.bluetooth = state;
    const why = reason();
    if (note.textContent !== why) {
      note.textContent = why;
      note.parentElement.setAttribute('aria-label', why ? `Bluetooth: ${why}` : '');
    }
    const parts = [/^unavailable: /.test(state) ? why.replace(/\.$/, '') : `Bluetooth: ${state}`];
    if (on) parts.push(`${plural(peers.contacts.length, 'contact', 'contacts')} in range, ${plural(peers.strangers, 'stranger', 'strangers')} seen lately`);
    if (waiting.size) parts.push(plural(waiting.size, 'request waits', 'requests wait'));
    tile.title = parts.join('. ') + '.';
    if (top) {
      top.textContent = /^unavailable: /.test(state) ? 'Bluetooth unavailable' : state === WAITING ? 'Bluetooth locked' : { off: 'Bluetooth off', 'contacts only': 'Bluetooth to contacts', 'open to requests': 'Bluetooth open', receiving: 'Bluetooth receiving' }[state] || 'Bluetooth';
      top.title = parts[0] + '.';
    }
    // kept in its place while the radio isn't working, as the rate's is before it is measured, so the bank's foot
    // never wraps or unwraps as the switch turns
    near.style.visibility = on ? '' : 'hidden';
    near.setAttribute('aria-hidden', String(!on));
    const wheels = (el, n) => { el.innerHTML = String(Math.min(99, n)).padStart(2, '0').split('').map(d => `<i>${d}</i>`).join(''); };
    wheels(near.querySelector('#btcontacts'), peers.contacts.length);
    wheels(near.querySelector('#btstrangers'), peers.strangers);
    near.title = `In Bluetooth range: ${plural(peers.contacts.length, 'contact', 'contacts')} now, and ${plural(peers.strangers, 'stranger', 'strangers')} seen in the last 15 minutes. A stranger whose advert changed in that time counts twice.`;
    stations(); drawKey();
  }

  // the stations: a contact's jewel lit while it is in Bluetooth range, and one an accept added marked until it
  // shows it holds this machine too
  function stations() {
    for (const s of document.querySelectorAll('.stn[data-contact]')) {
      const id = +s.dataset.contact, here = lit() && peers.contacts.includes(id);
      s.querySelector('.jewel').classList.toggle('off', !here);
      if (here) s.title = 'In Bluetooth range'; else s.removeAttribute('title');
      const small = s.querySelector('.holder small');
      if (!small) continue;
      // while unconfirmed the line says only that, in the refusal red; then it says what it said before
      if (unconfirmed.has(id)) {
        if (!small.classList.contains('nc')) { small.dataset.was = small.textContent; small.classList.add('nc'); }
        small.textContent = 'not confirmed';
        small.title = 'Added from their request. Not confirmed until their machine shows it has added this one too.';
      } else if (small.classList.contains('nc')) { small.classList.remove('nc'); small.textContent = small.dataset.was; small.removeAttribute('title'); }
    }
  }
  kit.onStations(stations);

  /* ---------- the tray ---------- */
  const slips = new Map();// request id -> its slip, kept while it waits so what is typed in it stays
  let leaving = [];// slips on their way out
  const fit = i => { i.style.width = Math.max(4, Math.min(26, i.value.length)) + 0.6 + 'ch'; };
  // a request: their name as they gave it, on a dotted rule to correct, the key's first four digits and when it came,
  // and the stamps; where New contacts need a key check, the node's sentence typed in place of Accept
  function requestSlip(r) {
    let s = slips.get(r.id);
    if (!s) {
      s = document.createElement('div');
      s.className = 'slip ask'; s.dataset.id = r.id; s.setAttribute('role', 'group');
      s.innerHTML = `<label class="who"><input class="askname" maxlength="40" spellcheck="false" autocomplete="off"></label><small></small>`
        + '<p class="rule"></p><p class="why" role="status"></p><div class="stamps"><button type="button" class="rs ok">Accept</button><button type="button" class="rs no">Decline</button></div>';
      const name = s.querySelector('.askname');
      name.value = r.name; fit(name);
      name.setAttribute('aria-label', `Their name, as they gave it: ${r.name}. Correct it before accepting.`);
      name.title = 'Their name as they gave it. Correct it before accepting.';
      name.addEventListener('input', () => { fit(name); s.querySelector('.why').textContent = ''; });
      name.addEventListener('keydown', e => {
        if (e.key === 'Enter') { e.preventDefault(); if (!inPerson) accept(r, s); }
        else if (e.key === 'Escape' && name.value !== r.name) { e.preventDefault(); e.stopPropagation(); name.value = r.name; fit(name); }
      });
      s.querySelector('.rs.ok').addEventListener('click', () => accept(r, s));
      s.querySelector('.rs.no').addEventListener('click', () => decline(r, s));
      // under the name, what it is (as they gave it, their key's first four digits), then what they ask and when
      s.querySelector('small').innerHTML = `as they gave it, key ${r.fingerprint.slice(0, 4).toUpperCase()}<span class="asks">asks to connect${r.at != null ? ', ' + hhmm(r.at) : ''}</span>`;
      s.querySelector('.rs.ok').title = 'Adds them as a contact, not checked in person, and sends your hello to them alone.';
      s.querySelector('.rs.no').title = 'Nothing goes back to them, and nothing of this request is kept.';
      slips.set(r.id, s);
    }
    s.setAttribute('aria-label', `${r.name} asks to connect`);
    const ok = s.querySelector('.rs.ok');
    ok.hidden = !!inPerson;
    s.querySelector('.rule').textContent = inPerson ? MEET_FIRST : '';
    s.classList.toggle('held', !!inPerson);
    return s;
  }
  function strangersSlip() {
    const s = document.createElement('div');
    s.className = 'slip near'; s.setAttribute('role', 'group'); s.setAttribute('aria-label', 'Strangers in range');
    const labels = listed();
    // while a request is on show, the strangers wait behind it, folded to one typed line, so the tray holds both
    // beside the room meter; answered, they open again
    if (waiting.size) {
      s.classList.add('folded');
      const t = document.createElement('small');
      // held by the key check, the line says so itself, as the request beside it does
      const few = `${labels.slice(0, 3).join(', ')}${labels.length > 3 ? ', …' : ''}`;
      t.textContent = inPerson ? `${few} near, not asked` : `Strangers: ${few}`;
      const all = `${plural(labels.length, 'stranger', 'strangers')} in range: ${labels.join(', ')}`;
      s.title = inPerson ? `${all}. Not asked while new contacts need a key check.` : `${all}. Answer the request to ask them.`;
      s.setAttribute('aria-label', s.title);
      s.appendChild(t);
      return s;
    }
    const shown = labels.slice(0, 3);
    s.innerHTML = '<small class="hd">Strangers in range</small><ul></ul>';
    const ul = s.querySelector('ul');
    for (const label of shown) {
      const li = document.createElement('li'); li.dataset.label = label;
      li.appendChild(document.createElement('span')).className = 'lab';
      li.querySelector('.lab').textContent = label;
      const a = asked.get(label);
      if (a) {
        const t = li.appendChild(document.createElement('small'));
        t.className = 'said' + (a.state === 'failed' || a.state === 'refused' ? ' bad' : '');
        t.textContent = { asking: 'asking', delivered: 'asked, delivered', failed: 'not delivered', accepted: 'accepted, added' }[a.state] || sentence(a.why || 'not asked');
      } else if (!inPerson) {
        const b = li.appendChild(document.createElement('button'));
        b.type = 'button'; b.className = 'rs ask'; b.textContent = 'Ask to connect';
        b.setAttribute('aria-label', `Ask ${label} to connect`);
        b.title = `Sends your hello to ${label} alone. Nothing else goes until they accept.`;
        b.addEventListener('click', () => askThem(label));
      }
      ul.appendChild(li);
    }
    if (labels.length > shown.length) { const m = document.createElement('small'); m.textContent = `and ${labels.length - shown.length} more in range`; s.appendChild(m); }
    // the key check's rule, as the request's slip has it
    if (inPerson && shown.some(l => !asked.has(l))) { const p = document.createElement('p'); p.className = 'rule'; p.textContent = MEET_FIRST; s.appendChild(p); }
    return s;
  }
  let nearSlip = null;
  const listed = () => [...new Set([...[...asked.keys()].filter(l => asked.get(l).state !== 'gone'), ...strangers])];
  function drawTray() {
    const active = document.activeElement, focusIn = tray.contains(active) || nearTray.contains(active);
    const focusKey = focusIn && active.closest('li[data-label]') ? active.closest('li[data-label]').dataset.label : null;
    const nodes = [...leaving];
    const reqs = [...waiting.values()];
    if (reqs.length) nodes.push(requestSlip(reqs[0]));
    if (reqs.length > 1) { const m = document.createElement('div'); m.className = 'slip empty'; m.textContent = `and ${reqs.length - 1} more asking`; nodes.push(m); }
    // only while there is someone to show: a stranger who accepted and left the list takes the slip with them
    nearSlip = atOpen() && listed().length ? strangersSlip() : null;
    nearTray.replaceChildren(...(nearSlip ? [nearSlip] : []));
    // folded behind a request, the strangers' line waits under the room meter; open, it heads the tray's column
    if (nearSlip && nearSlip.classList.contains('folded')) { if (nearTray.parentElement !== left) left.append(nearTray); }
    else if (nearTray.parentElement !== right) right.insertBefore(nearTray, outboxEl);
    nodes.forEach((n, i) => { if (tray.children[i] !== n) tray.insertBefore(n, tray.children[i] || null); });
    while (tray.children.length > nodes.length) tray.lastChild.remove();
    for (const [id, s] of slips) if (!waiting.has(id) && !leaving.includes(s)) slips.delete(id);
    fade();
    // focus that was in a part of the tray now gone goes to what can be done next there, or back to the switch
    if (focusIn && !tray.contains(document.activeElement) && !nearTray.contains(document.activeElement)) {
      const again = focusKey && nearSlip && nearSlip.querySelector(`li[data-label="${CSS.escape(focusKey)}"] button`);
      (again || next()).focus({ preventScroll: true });
    }
  }
  // what can be done next in the tray, or the switch
  const next = () => tray.querySelector('.slip:not(.gone) input, .slip:not(.gone) button:not([hidden])') || nearTray.querySelector('button') || document.getElementById('k3');
  function draw() { drawTile(); drawTray(); }

  function why(s, text) { s.querySelector('.why').textContent = text; }
  function busy(s, on) { for (const b of s.querySelectorAll('button')) b.disabled = on; }
  // a request answered or gone: its slip leaves the tray as a sent slip leaves the outbox
  function gone(id) {
    if (!waiting.has(id)) return;
    waiting.delete(id);
    const s = slips.get(id);
    if (s && s.isConnected && !RM.matches) {
      const back = s.contains(document.activeElement);
      s.classList.add('gone'); leaving.push(s);
      setTimeout(() => { leaving = leaving.filter(x => x !== s); s.remove(); slips.delete(id); drawTray(); }, 460);
      if (back) { drawTray(); next().focus({ preventScroll: true }); }
    }
    draw();
  }
  async function accept(r, s) {
    const input = s.querySelector('.askname'), name = input.value.trim();
    if (!name) { why(s, 'Write their name first.'); input.focus(); return; }
    busy(s, true);
    try {
      const id = await bridge.acceptRequest(r.id, name);
      unconfirmed.add(id);
      kit.contactAdded({ id, name, short: name.split(' ')[0], checked: false, safety: r.safety });
      kit.showUnchecked(name, r.safety, `${name.split(' ')[0]}, by Bluetooth`, `${name} is added, not yet checked. Compare these with theirs when you meet, then pair by sound or light.`);
      kit.say(`${name} added, not checked in person.`);
      gone(r.id);
    } catch (e) {
      e = String(e && e.message || e); busy(s, false);
      if (e === MEET_FIRST) { inPerson = 1; drawTray(); s.querySelector('.rs.no').focus(); }
      else why(s, sentence(e));
    }
  }
  async function decline(r, s) {
    busy(s, true);
    try { await bridge.declineRequest(r.id); kit.say('Declined. Nothing of the request was kept.'); gone(r.id); }
    catch (e) { busy(s, false); why(s, sentence(String(e && e.message || e))); }
  }
  async function askThem(label) {
    asked.set(label, { state: 'asking' });
    drawTray();
    try { await bridge.sendRequest(label); kit.say(`Asked ${label} to connect.`); }
    catch (e) {
      e = String(e && e.message || e);
      if (e === MEET_FIRST) { inPerson = 1; asked.delete(label); }
      else asked.set(label, { state: 'refused', why: e });
      drawTray();
    }
  }
  function refreshStrangers() {
    if (!atOpen()) { strangers = []; return; }
    bridge.strangers().then(list => {
      strangers = list;
      // one who accepted is a contact now: they leave the list once they are no longer offered
      for (const [l, a] of asked) if (a.state === 'accepted' && !list.includes(l)) asked.set(l, { state: 'gone' });
      drawTray();
    }, () => {});
  }
  function refreshKeyCheck() { bridge.settings().then(s => { if (s && Number.isInteger(s.inperson)) { inPerson = s.inperson; drawTray(); } }, () => {}); }

  bridge.on('channel', s => {
    if (s.channel !== 'bluetooth' || /^not sent: /.test(s.state)) return;
    const was = state, open = atOpen();
    state = s.state;
    if (/^unavailable: /.test(was) && !/^unavailable: /.test(state) && lit()) kit.say(`Bluetooth works again: ${state}.`);
    if (open && !atOpen() && state !== 'receiving') asked.clear();
    if (atOpen() && !open) refreshStrangers();
    if (!atOpen()) strangers = [];
    draw();
  });
  bridge.on('peers', p => { const more = p.strangers !== peers.strangers; peers = p; if (more) refreshStrangers(); draw(); });
  bridge.on('request', q => {
    waiting.set(q.id, { ...q, at: bridge.now() });
    kit.say(`${q.name} asks to connect.`, `${q.name} asks to connect by Bluetooth. The request is in the tray above the outbox.`);
    draw();
  });
  bridge.on('requestGone', q => gone(q.id));
  bridge.on('requested', q => {
    asked.set(q.label, { state: q.state });
    if (q.state === 'accepted' && q.contact != null) bridge.contacts().then(list => { const p = list.find(c => c.id === q.contact); if (p) kit.contactAdded(p); kit.say(`${q.label} accepted: ${p ? p.name : 'a new contact'} is added.`); refreshStrangers(); }, () => {});
    else if (q.state === 'failed') kit.say(`${q.label} was not asked: not delivered.`);
    draw();
  });
  bridge.on('contactConfirmed', c => { unconfirmed.delete(c.contact); stations(); });
  addEventListener('pageshow', e => { if (e.persisted) refreshKeyCheck(); });

  // a page that opens late learns what the machine already knows
  bridge.channels().then(list => { const b = list.find(c => c.channel === 'bluetooth'); if (b) { state = b.state; refreshStrangers(); draw(); } }, () => {});
  bridge.peers().then(p => { if (p) { peers = p; draw(); } }, () => {});
  bridge.requests().then(list => { for (const q of list) if (!waiting.has(q.id)) waiting.set(q.id, { ...q, at: null }); draw(); }, () => {});
  bridge.unconfirmed().then(list => { list.forEach(id => unconfirmed.add(id)); stations(); }, () => {});
  refreshKeyCheck();
  draw();
}
