// Quality check for the Bombechat front page. Run after every change:
//   node tools/check.cjs                          the app's interface, app/ui/bombechat.html
//   node tools/check.cjs mockups/bombechat.html   the frozen design reference, for comparison
// Writes to checks/<folder>/<page>/ (checks/app-ui/bombechat, checks/mockups/bombechat):
//   vp-<w>x<h>.png        the page at each laptop size, frame 4, held still
//   vp-<w>x<h>-full.png   the same, whole scrolling page
//   full-f<N>.png         1440x900, held still on each frame
//   detail-<part>.png     2x crops of each part, found by selector
//   motion.png            the drum bank mid-turn, in real time
//   reply.png             the sheet after typing and sending a reply
//   card-<path>-<w>-<state>.png  2x crops of the key check card through pairing, in the
//                         browser (simulator) and on the app path, at 1024, 1440 and 1728
//   unlock-<w>-<state>.png  2x crops of the passphrase form on the app path at 1024, 1440 and 1728
//   broadcast-<path>-<w>-<state>.png  2x crops of the message sheet writing a public notice
//                         (idle, composing, sent; refused at 1440), browser and app path
//   empty-<w>.png         2x crops of the empty sheet saying how to test the machine (app path)
//   selftest-<path>-<w>-<state>.png  2x crops of the sheet testing the machine (rest, refused, waiting,
//                         heard, stopped; not heard at 1440), browser and app path, and its stamps beside
//                         the key check card's (selftest-<path>-<w>-stamps-foot.png, -stamps-card.png);
//                         selftest-browser-1440-light-<state>.png for Test by light
// and prints what it measured. Exits non-zero on page errors, horizontal overflow,
// text under 12px, layout shift on load, a plain row that doesn't settle on the
// frame text, the sheet not keeping its place, a reply that doesn't land, clipped
// audio, or drum sound leaking into the 17.6 to 19.8 kHz listening band.
// For the app's interface it also fails when the drums don't follow an injected
// chunk, when the drums make a sound while a message is arriving by sound, when
// pairing by sound doesn't run in the browser or on the app path (first run asking your name,
// pair, a 60-digit offer legible at 1024, 1440 and 1728 under an editable name, Matched, add
// keeping the corrected name and stamping the card in place, Doesn't match keeping nothing,
// a held hello lapsing), when testing the machine doesn't run in the browser or on the app path (its stamps
// not cut as Pair by sound's are, refused at Off and Listen in sentence case with nothing asked of the machine, the
// run waiting for quiet, the drums not spelling it, its entry not under the margin note self-test with what came
// back, the rate and band and the stamp, a drum left partly under the lamp while it flashes, tests or pairs at any
// window size (or the page not held at its top under it, or not put back where it was), anything of it in the outbox, Escape not stopping it, a test nobody hears
// not saying what to try, Test by light not on the lamp for a mirror, an empty sheet's typed line missing or its
// mark not pointing at the stamp, or at any window size, empty or full, Test the machine outside the first screen, the
// sheet's head running together or its foot running past the sheet), when writing a public notice doesn't work in the browser or on the app
// path (the Reply/Public toggle, the line saying who can read it and the key that signs it,
// empty, too long and Sound at Listen refused in sentence case with nothing sent, a sent
// notice on the sheet as a signed public entry with its outbox status, the toggle springing
// back to Reply), when the passphrase form (first run, refusals, the key being worked out, a later run,
// a wrong passphrase) is off-centre, unlabelled, out of Tab order, or not refused in place and announced,
// or appears at all on a Keychain launch, when the bytes-a-second readout
// doesn't show the rate the bridge reports, when
// the page asks for anything off this machine, or when an http(s) URL appears
// anywhere in app/ui (licence texts and comments in vendored three.js aside).
const path = require('path');
const fs = require('fs');
const { chromium } = require('playwright');

const ROOTDIR = path.resolve(__dirname, '..');
const page = path.resolve(process.argv[2] || 'app/ui/bombechat.html');
const APP = !/[\\/]mockups[\\/]/.test(page);
const out = path.join(ROOTDIR, 'checks', path.relative(ROOTDIR, path.dirname(page)).replace(/[\\/]/g, '-'), path.basename(page, '.html'));
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const { start } = require('./serve.cjs');
const { externalUrls } = require('./offline.cjs');
const { versionProblems, workspaceVersion } = require('./versions.cjs');
const url = q => 'http://127.0.0.1:4812/' + path.relative(ROOTDIR, page) + (q ? '?' + q : '');
// held still on a frame of the incoming message: the app's ?f counts chunks (chunk 1 ends on
// a frame of the text), the mockup's counts frames
const HELD = APP ? 'f=1&still' : 'f=3&still';
const VIEWPORTS = [[1024, 768], [1280, 800], [1440, 900], [1512, 982], [1728, 1117]];
const PARTS = { top: '.top', bank: '.bank', sheet: '.sheet', compose: '.compose', menu: '.menu', panel: '.panel' };

// The app's side of the bridge, for checking the app path in a plain browser: a first-run machine
// (no store yet) whose commands answer as the Tauri shell's do. Every call is kept in __calls;
// __emit sends an event as the node would. pair() hears `hello` say hello 1.2 s later.
// Given `people`, the store exists and is locked instead: contacts are those, conversation(id)
// answers talk[id], and the unlock emits `afterUnlock` ([name, payload] pairs) before it resolves,
// as the node's first tick after an unlock opens what it heard while locked. `settings` is what the
// machine has kept (every setting, as the settings command answers); the defaults if not given.
// Broadcasts: `broadcastMax` is the longest the machine reports (3939 if not given); the store's kept
// notices (`kept`, then each accepted one) live in sessionStorage, so a reload replays them as the device
// would; `refuseBroadcast` is a reason the machine gives for refusing every broadcast.
// Unlocking: `keychain` opens the store before the page asks (the Lock Off launch); `unlockMs` is how long
// working out the key takes (0 if not given); the first `wrong` unlocks are refused as 'wrong passphrase'.
// Testing the machine: self_test reports the run waiting, then holds there until the check moves it on: __selfTestStep()
// sends its next report (playing, each chunk, then heard; or with `selfTest` 'unheard', playing then unheard), and
// __selfTestGo() sends the rest in turn. `noSound`: the machine has no sound to test with. stop_self_test reports it
// stopped.
// what the plain row shows once a message's text is in: its last twelve characters from drum 1, letters as themselves,
// anything else as the spacer X
const plainRowOf = t => [...t].slice(-12).map(ch => /[a-z]/i.test(ch) ? ch.toUpperCase() : 'X').join('').padEnd(12, 'X');

function tauriStub({ hello, safety, pairOfferMs = 1200, people, talk, afterUnlock, settings, broadcastMax = 3939, kept = [], refuseBroadcast, keychain, unlockMs = 0, wrong = 0, wrongWhy = 'wrong passphrase', refuseSetting, sendId, selfTest = 'heard', noSound }) {
  let unlocked = !!keychain, refusals = wrong;
  const KEPT = 'stub.notices';
  const notices = () => JSON.parse(sessionStorage.getItem(KEPT) || 'null') || kept;
  const keep = list => sessionStorage.setItem(KEPT, JSON.stringify(list));
  const settle = (id, state) => keep(notices().map(n => String(n.id) === String(id) ? { ...n, state } : n));
  const listeners = {};
  window.__calls = [];
  window.__emit = (name, payload) => (listeners[name] || []).forEach(f => f({ payload }));
  // a setting moved elsewhere (on the settings page): what the machine answers from now on
  window.__moveSetting = (id, value) => { if (settings && typeof settings === 'object') settings[id] = value; };
  window.__TAURI__ = {
    event: { listen: (name, f) => { (listeners[name] = listeners[name] || []).push(f); return Promise.resolve(() => {}); } },
    core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
      switch (cmd) {
        case 'contacts': return unlocked ? Promise.resolve(people || []) : Promise.reject(people ? 'locked' : 'no store yet');
        case 'unlock': return new Promise(r => setTimeout(r, unlockMs)).then(() => {
          if (refusals > 0) { refusals--; return Promise.reject(wrongWhy); }
          unlocked = true; for (const [name, payload] of afterUnlock || []) window.__emit(name, payload); return null; });
        case 'machine': return Promise.resolve({ name: 'Machine No. 1', key: 'a3f09c1e'.repeat(16), you: 'Ana Lima', held: 0, version: '0.5.0', commit: 'a1b2c3d', broadcast_max: broadcastMax, reply_max: 2814 });
        case 'conversation': return Promise.resolve((talk || {})[args.contactId] || []);
        case 'settings': return settings === 'unread' ? Promise.reject('the machine did not answer') : Promise.resolve(settings || { sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 });
        // as the device answers on this build: relay waits for its feature, the key check is always on
        case 'setting_notes': return settings === 'unread' ? Promise.reject('the machine did not answer') : Promise.resolve({ carry: { note: 'comes with relay' }, hops: { note: 'comes with relay' }, hold: { note: 'comes with relay' },
          inperson: { note: 'always, on this build', value: 1 } });
        case 'channels': return Promise.resolve([{ channel: 'sound', state: 'listening', rate: 16.1 }, { channel: 'light', state: 'off', rate: 2290 }]);
        case 'stop_pairing': return Promise.resolve(null);
        case 'eyepiece': return Promise.resolve(null);
        // refused as the node refuses it when the channel's switch won't send; pairOfferMs null: no hello comes by itself
        case 'pair': {
          const light = (settings && settings !== 'unread' ? settings.light : 0);
          if (args.channel === 'light' && light < 2) return Promise.reject(light === 0 ? 'not sent: light is off' : 'not sent: light is set to read only');
          if (pairOfferMs !== null) setTimeout(() => window.__emit('pairOffer', { peer: { sign: Array(32).fill(5), dh: Array(32).fill(6) }, name: hello, safety, channel: args.channel === 'light' ? 'Light' : 'Sound' }), pairOfferMs);
          return Promise.resolve(null);
        }
        case 'confirm_pair': return Promise.resolve(7);
        // refused as the node refuses it when the channel's switch won't send
        case 'self_test': {
          const s = settings && settings !== 'unread' ? settings : { sound: 2, light: 0 };
          if (noSound) return Promise.reject('not sent: no sound or light to send on');
          if (args.channel === 'sound' && s.sound < 2) return Promise.reject(s.sound === 0 ? 'not sent: sound is off' : 'not sent: sound is set to listen only');
          if (args.channel === 'light' && s.light < 2) return Promise.reject(s.light === 0 ? 'not sent: light is off' : 'not sent: light is set to read only');
          // the reply line's words, or the machine's own; refused over 100 bytes, as the node refuses them
          const words = (args.text || '').replace(/^[\s\u0085]+|[\s\u0085]+$/g, ''), said = words || 'The machine hears itself.', n = new TextEncoder().encode(said).length;
          if (new TextEncoder().encode(words).length > 100) return Promise.reject('not sent: too long for a test');
          const tag = 0x7e00 + (window.__calls.length & 255), channel = args.channel === 'light' ? 'Light' : 'Sound';
          const says = (state, extra) => () => window.__emit('selfTest', { tag, channel, state, ...extra });
          const steps = [says('playing')];
          if (selfTest === 'heard') {
            // sealed as the node seals it: sender keys and time, the text, a signature, in 48-byte chunks
            const inner = [...Array(68).fill(0x51), ...new TextEncoder().encode(said), ...Array(64).fill(0x52)], of = Math.ceil(inner.length / 48);
            for (let i = 0; i < of; i++) steps.push(() => { const plain = inner.slice(i * 48, i * 48 + 48); window.__emit('chunk', { tag, index: i, of, channel, view: { plain, heard: plain.map(b => b ^ 9), key: plain.map(() => 9) } }); });
            steps.push(says('heard', { text: said, bytes: 354 + n, ms: Math.ceil((354 + n) / 0.0161), rate: 16.1, band: 'near silent' }));
          } else steps.push(says('unheard'));
          const run = window.__selfTest = { tag, channel, steps };
          window.__selfTestStep = () => { if (window.__selfTest !== run || !run.steps.length) return false; run.steps.shift()(); if (!run.steps.length) window.__selfTest = null; return true; };
          window.__selfTestGo = () => { const next = () => { if (window.__selfTestStep()) setTimeout(next, 0); }; next(); };
          setTimeout(says('waiting'), 0);
          // its time on air, at 16.1 bytes a second
          return Promise.resolve(Math.ceil((354 + n) / 0.0161));
        }
        case 'stop_self_test': { const run = window.__selfTest; window.__selfTest = null;
          if (run) window.__emit('selfTest', { tag: run.tag, channel: run.channel, state: 'stopped' }); return Promise.resolve(null); }
        // refused as the device refuses a save that fails: left as it was, except Light turned down, which the
        // machine keeps for the session (Escape always stops) and its settings then answer
        case 'set_setting': if (!refuseSetting) return Promise.resolve(null);
          if (args.id === 'light' && settings && typeof settings === 'object' && args.valueIndex < settings.light) settings.light = args.valueIndex;
          return Promise.reject(refuseSetting);
        case 'send': return sendId ? Promise.resolve(sendId) : Promise.reject('not sent');
        // as the broadcast command answers; an accepted one waits, plays and is sent, as the node reports it
        case 'broadcast': {
          if (refuseBroadcast) return Promise.reject(refuseBroadcast);
          if (!args.text.trim()) return Promise.reject('Write the notice first');
          const n = new TextEncoder().encode(args.text).length;
          if (n > broadcastMax) return Promise.reject(`Too long to broadcast: ${n} bytes, at most ${broadcastMax}`);
          const id = Array.from({ length: 16 }, (_, i) => (i * 17 + window.__calls.length + Date.now()) & 255);
          keep([...notices(), { id, outgoing: true, signer: { sign: Array(32).fill(0xa3) }, text: args.text, verified: true, hops: 0, at_min: Math.floor(Date.now() / 60000), state: 'waiting' }]);
          setTimeout(() => window.__emit('outbox', { id, state: 'waiting' }), 50);
          setTimeout(() => window.__emit('outbox', { id, state: 'playing' }), 1500);
          setTimeout(() => { settle(id, 'sent'); window.__emit('outbox', { id, state: 'sent' }); }, 2600);
          return Promise.resolve(id);
        }
        case 'broadcasts': return unlocked ? Promise.resolve(notices()) : Promise.reject('locked');
        default: return Promise.resolve(null);
      } } },
  };
}

// every visible piece of text, HTML or SVG, with the size it is actually drawn at.
// Decorative scales (the 26 letters round each drum) are marked data-decor and skipped.
// The drums the flashing view covers only in part: the lamp's shape is its column (--nx wide) and, when it reaches over
// the drum bay, the band over it (--ny deep). Each drum on the screen lies wholly under it or wholly clear of it; any
// partly covered is returned, with how much of it shows
function lampCuts() {
  const l = document.getElementById('lamp'); if (l.hidden) return ['the lamp is not open'];
  const r = l.getBoundingClientRect(), cs = getComputedStyle(l), nx = parseFloat(cs.getPropertyValue('--nx')) || r.width, ny = parseFloat(cs.getPropertyValue('--ny')) || r.height;
  const parts = [{ l: r.left, t: r.top, r: r.left + nx, b: r.bottom }, { l: r.left, t: r.top, r: r.right, b: r.top + ny }];
  const meet = (a, b) => ({ l: Math.max(a.l, b.l), t: Math.max(a.t, b.t), r: Math.min(a.r, b.r), b: Math.min(a.b, b.b) });
  const area = a => Math.max(0, a.r - a.l) * Math.max(0, a.b - a.t);
  const view = { l: 0, t: 0, r: innerWidth, b: innerHeight }, out = [];
  document.querySelectorAll('#drums .drum').forEach((d, i) => {
    const q = d.getBoundingClientRect(), box = meet({ l: q.left, t: q.top, r: q.right, b: q.bottom }, view), all = area(box); if (all < 1) return;
    const under = area(meet(box, parts[0])) + area(meet(box, parts[1])) - area(meet(meet(box, parts[0]), parts[1]));
    if (under > 1 && under < all - 1) out.push(`drum ${Math.floor(i / 12) + 1}.${i % 12 + 1} ${Math.round(100 * under / all)}% under the lamp`);
  });
  return out;
}

function measureText() {
  const small = [];
  const seen = new Set();
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  while (walk.nextNode()) {
    const n = walk.currentNode;
    if (!n.nodeValue.trim()) continue;
    const el = n.parentElement;
    if (!el || seen.has(el) || el.closest('[data-decor],script,style,[hidden]')) continue;
    seen.add(el);
    const r = el.getBoundingClientRect();
    if (!r.width || !r.height) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility === 'hidden' || +cs.opacity === 0) continue;
    let px = parseFloat(cs.fontSize);
    const svg = el.closest('svg');
    if (svg && el instanceof SVGElement) {
      const m = el.getScreenCTM();
      px = parseFloat(el.getAttribute('font-size') || cs.fontSize) * (m ? Math.hypot(m.a, m.b) : 1);
    }
    if (px < 11.75) small.push(`${n.nodeValue.trim().slice(0, 24)} (${px.toFixed(1)}px, ${el.tagName.toLowerCase()}.${String(el.className.baseVal ?? el.className).split(' ')[0]})`);
  }
  return small;
}

(async () => {
  const server = await start(4812);
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
  const problems = [];
  const report = {};
  const watch = (p, tag) => {
    p.on('pageerror', e => problems.push(`${tag}: page error: ${e.message}`));
    p.on('console', m => m.type() === 'error' && !/fonts\.g/.test(m.text()) && problems.push(`${tag}: console: ${m.text()}`));
    // the app ships everything it needs: nothing may be asked of any other host
    if (APP) p.on('request', r => { const u = r.url(); if (!/^(data:|blob:|http:\/\/127\.0\.0\.1:4812\/)/.test(u)) problems.push(`${tag}: asked the network for ${u}`); });
  };
  // the app's files carry no web addresses
  if (APP) { report.externalUrls = externalUrls(path.dirname(page)); for (const u of report.externalUrls) problems.push(`external URL in ${u}`); }
  // one version everywhere it is written: the Cargo workspace, tauri.conf.json, package.json and the simulator
  if (APP) for (const v of versionProblems(ROOTDIR)) problems.push(v);

  // 1. each laptop size: overflow, small text, layout shift while fonts arrive
  report.viewports = {};
  for (const [w, h] of VIEWPORTS) {
    const p = await browser.newPage({ viewport: { width: w, height: h } });
    watch(p, `${w}x${h}`);
    await p.addInitScript(() => {
      window.__cls = 0;
      new PerformanceObserver(l => { for (const e of l.getEntries()) if (!e.hadRecentInput) window.__cls += e.value; }).observe({ type: 'layout-shift', buffered: true });
    });
    await p.goto(url(HELD), { waitUntil: 'networkidle' });
    await p.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
    await p.evaluate(() => document.fonts.ready);
    await p.waitForTimeout(400);
    const r = await p.evaluate(() => ({
      overflowX: document.documentElement.scrollWidth - innerWidth,
      pageHeight: document.documentElement.scrollHeight,
      drum: getComputedStyle(document.querySelector('.bank')).getPropertyValue('--dr').trim(),
      sheetScrolls: (l => l.scrollHeight > l.clientHeight)(document.getElementById('log')),
      cls: +window.__cls.toFixed(4),
    }));
    r.smallText = await p.evaluate(`(${measureText.toString()})()`);
    report.viewports[`${w}x${h}`] = { ...r, smallText: r.smallText.length };
    if (r.overflowX > 0) problems.push(`${w}x${h}: page scrolls sideways by ${r.overflowX}px`);
    if (r.smallText.length) problems.push(`${w}x${h}: text under 12px: ${r.smallText.slice(0, 6).join('; ')}`);
    if (r.cls > 0.02) problems.push(`${w}x${h}: layout shift on load ${r.cls}`);
    if (APP) {
      // the top plate names the build as the machine does, and is cut for its longest form: a commit of the widest
      // digits built from uncommitted changes moves nothing beside it, and the bar holds every plate
      const b = await p.evaluate(() => {
        const b = document.getElementById('build'), top = document.querySelector('.top'), rt = b.parentElement;
        const at = () => [...rt.children].map(k => Math.round(k.getBoundingClientRect().left)).join();
        const text = b.textContent, before = at();
        b.textContent = text.replace(/[^ ]+$/, 'ddddddd+');
        const r = { text, moved: at() !== before, over: Math.max(top.scrollWidth - top.clientWidth, rt.scrollWidth - rt.clientWidth) };
        b.textContent = text; return r;
      });
      if (b.text !== `Build ${workspaceVersion(ROOTDIR)} \u00b7 browser`) problems.push(`${w}x${h}: the top plate says ${JSON.stringify(b.text)}`);
      if (b.moved) problems.push(`${w}x${h}: the build plate's longest form moves the plates beside it`);
      if (b.over > 0) problems.push(`${w}x${h}: the top bar overflows by ${b.over}px`);
    }
    // the front panel's bolt ring, as the back's: every ring bolt whole inside the page, clear of the top bar, and
    // meeting no part of the front (the bank, the sheet, the card, the switch tiles, the meter, the tray, the stations)
    if (APP) {
      const ring = await p.evaluate(() => { if (!window.__bank.ringBolts) return null;
        const B = __bank.ringBolts(), W = document.documentElement.scrollWidth, H = document.documentElement.scrollHeight, top = document.querySelector('.top').getBoundingClientRect().bottom + scrollY;
        const parts = [...document.querySelectorAll(__bank.ringParts)].filter(e => e.offsetParent).map(e => { const r = e.getBoundingClientRect(); return { l: r.left + scrollX, t: r.top + scrollY, r: r.right + scrollX, b: r.bottom + scrollY, n: e.className }; });
        const bad = [];
        for (const b of B) {
          if (b.x - b.r < 4 || b.x + b.r > W - 4 || b.y - b.r < top + 4 || b.y + b.r > H - 4) bad.push(`bolt at ${Math.round(b.x)},${Math.round(b.y)} outside the page`);
          for (const q of parts) if (b.x + b.r > q.l && b.x - b.r < q.r && b.y + b.r > q.t && b.y - b.r < q.b) bad.push(`bolt at ${Math.round(b.x)},${Math.round(b.y)} meets ${q.n}`);
        }
        return { n: B.length, bad }; });
      if (ring) { r.ringBolts = ring.n; if (ring.n < 12 || ring.bad.length) problems.push(`${w}x${h}: the front's bolt ring: ${ring.n} bolts; ${ring.bad.slice(0, 4).join('; ')}`); }
    }
    // the outbox tray, on either build: a well the size of the slips in it (no more than about 24 px of bare floor under
    // the last), with the slip and with nothing waiting, its words then engraved in its floor. And the panel's column
    // in proportion: on a wide window the meter's row stands in the middle of the room between the switches and the
    // stations; on a narrow one the panel ends about where the sheet beside it does
    if (APP) {
      const tray = async () => { await p.waitForFunction(() => { const t = __bank.trayWell && __bank.trayWell(), s = [...document.querySelectorAll('.row2 .slip:not(.gone)')].filter(e => e.offsetParent);
        return t && s.length && Math.abs(t.y + t.h - 6 - Math.max(...s.map(e => e.getBoundingClientRect().bottom + scrollY))) < 30; }, null, { timeout: 8000 }).catch(() => {});
        return p.evaluate(() => { const t = __bank.trayWell(), col = document.querySelector('.row2 > .col.tray') || document.querySelector('.outbox');
          const s = [...document.querySelectorAll('.row2 .slip:not(.gone)')].filter(e => e.offsetParent).map(e => e.getBoundingClientRect());
          const box = s => { const r = document.querySelector(s).getBoundingClientRect(); return { t: r.top + scrollY, b: r.bottom + scrollY }; };
          const vu = box('.vu'), mods = box('.mods'), row3 = box('.row3'), sheet = box('.sheet'), panel = box('.panel');
          const top = Math.min(vu.t, ...s.map(r => r.top + scrollY)), foot = Math.max(vu.b, ...s.map(r => r.bottom + scrollY));
          return { bare: t ? Math.round(t.y + t.h - 6 - Math.max(...s.map(r => r.bottom + scrollY))) : null, scrolls: /auto|scroll/.test(getComputedStyle(col).overflowY) && col.scrollHeight > col.clientHeight + 4,
            above: Math.round(top - mods.b), below: Math.round(row3.t - foot), past: Math.round(panel.b - sheet.b) }; }); };
      const rest = await tray();
      await p.evaluate(() => { document.getElementById('outbox').innerHTML = '<div class="slip empty">Nothing waiting to send.</div>'; });
      const empty = await tray();
      r.tray = { rest, empty }; report.viewports[`${w}x${h}`].tray = r.tray;
      for (const [k, t] of Object.entries(r.tray)) if (t.bare === null || t.bare > 24) problems.push(`${w}x${h}: the outbox tray ${k} is not sized to its slips: ${JSON.stringify(t)}`);
      if (w >= 1400 && Math.abs(rest.above - rest.below) > 8) problems.push(`${w}x${h}: the meter's row is not in the middle of the panel's room: ${JSON.stringify(rest)}`);
      if (w < 1400 && rest.past > 32) problems.push(`${w}x${h}: the panel runs ${rest.past}px past the sheet beside it`);
    }
    await p.screenshot({ path: path.join(out, `vp-${w}x${h}.png`) });
    if (r.pageHeight > h) await p.screenshot({ path: path.join(out, `vp-${w}x${h}-full.png`), fullPage: true });
    await p.close();
  }

  // 2. every frame, held still, at 1440x900
  const still = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  watch(still, 'frames');
  await still.goto(url('still'));
  // the app's ?f= counts chunks of the incoming message; the mockup's counts frames
  const frames = await still.evaluate(() => window.__sim ? __sim.chunks : +document.getElementById('fnt').textContent);
  for (let f = 0; f < frames; f++) {
    await still.goto(url(`f=${f}&still`));
    await still.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
    await still.evaluate(() => document.fonts.ready);
    await still.waitForTimeout(250);
    await still.screenshot({ path: path.join(out, `full-f${f + 1}.png`) });
  }
  await still.close();

  // 3. 2x crops of each part, by selector
  const hi = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  watch(hi, 'detail');
  await hi.goto(url(HELD));
  await hi.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
  await hi.evaluate(() => document.fonts.ready);
  await hi.waitForTimeout(300);
  for (const [name, sel] of Object.entries(PARTS)) await hi.locator(sel).first().screenshot({ path: path.join(out, `detail-${name}.png`) });
  await hi.close();

  // 4. live: drums mid-turn, the sheet keeping its place, the "new below" tab
  const live = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  watch(live, 'live');
  await live.goto(url());
  await live.mouse.click(5, 5);
  await live.waitForTimeout(400);
  const before = await live.evaluate(() => { const l = document.getElementById('log'); l.scrollTop = 0; return { top: l.scrollTop, room: l.scrollHeight - l.clientHeight }; });
  if (before.room <= 0) problems.push('sheet has nothing to scroll at 1440x900');
  await live.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
  report.gl = await live.evaluate(() => __bank.gl);
  if (!report.gl) problems.push('drum bank fell back to flat drums (no WebGL)');
  // three rows of drums with nothing between them: an axle behind each row, no rail between rows, no bar in the grid
  if (APP) {
    report.rails = await live.evaluate(() => ({ ...__bank.rails(), bars: document.querySelectorAll('#drums .bar').length, drums: document.querySelectorAll('#drums .drum').length }));
    if (report.rails.between !== 0 || report.rails.axles !== 3 || report.rails.bars !== 0 || report.rails.drums !== 36) problems.push(`the drum bank is not three rows with nothing between them: ${JSON.stringify(report.rails)}`);
    // the drums stay on their axles after a turn: lay the bank out while #app is mid-turn (as it is coming back from
    // the settings page), end the turn, and every drum must be drawn on its cell and its row's axle, every row plate at
    // its label and every column number on its own, to a quarter of a pixel, at 1440 and at 1512 (the commissioner's)
    const afterTurn = pg => pg.evaluate(async () => {
      const app = document.getElementById('app'), frames = n => new Promise(r => { const f = () => --n ? requestAnimationFrame(f) : r(); requestAnimationFrame(f); });
      app.style.transition = 'none'; app.style.transform = 'rotateY(55deg)'; app.getBoundingClientRect();
      dispatchEvent(new Event('resize')); await frames(2);
      const worst = () => { const P = __bank.placement(), off = [...P.drums, ...P.plates, ...P.numbers].map(p => ({ at: p.at, off: Math.hypot(p.drawn.x - p.html.x, (p.drawn.y ?? 0) - (p.html.y ?? 0)) }));
        P.drums.forEach(d => off.push({ at: d.at + ' axle', off: Math.abs(d.centre - d.axle) }));
        return off.reduce((a, b) => (b.off > a.off ? b : a)); };
      // as laid out mid-turn, from the offsets, before the next frame lays it out again: to the pixel
      app.style.transform = ''; app.getBoundingClientRect();
      const midTurn = worst();
      // and once a frame has seen the turn over, from the rects: to the sub-pixel
      await frames(4); app.style.transition = '';
      return { midTurn, settled: worst() };
    });
    report.afterTurn = { 1440: await afterTurn(live) };
    {
      const wide = await browser.newPage({ viewport: { width: 1512, height: 982 } });
      watch(wide, 'after a turn 1512');
      await wide.goto(url());
      await wide.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      report.afterTurn[1512] = await afterTurn(wide);
      await wide.close();
    }
    for (const [w, { midTurn, settled }] of Object.entries(report.afterTurn)) {
      if (midTurn.off > 1) problems.push(`laid out mid-turn at ${w}, the bank is off its HTML: ${midTurn.at} by ${midTurn.off.toFixed(2)} px`);
      if (settled.off > .25) problems.push(`after a turn at ${w} the bank is off its HTML: ${settled.at} by ${settled.off.toFixed(2)} px`);
    }
  }
  // the front panel costs nothing while idle: one canvas under the whole front, drawn only when something changed. Held
  // still, 120 frames draw nothing; a knob turned draws a few frames and stops; the room meter's needle moving draws
  // only the meter's own canvas
  if (APP) {
    const idle = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    watch(idle, 'idle');
    await idle.goto(url(HELD));
    await idle.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
    await idle.evaluate(() => document.fonts.ready);
    const frames = n => idle.evaluate(n => new Promise(r => { const n0 = __bank.renders(); const f = () => --n ? requestAnimationFrame(f) : r(__bank.renders() - n0); requestAnimationFrame(f); }), n);
    await frames(30);
    const r = { idle: await frames(120) };
    // (counted from before the click: the turn's first frame may be drawn before a count started after it)
    const k0 = await idle.evaluate(() => __bank.renders());
    await idle.click('#k1'); await frames(90);
    r.knob = await idle.evaluate(n => __bank.renders() - n, k0);
    r.after = await frames(120);
    if (await idle.evaluate(() => !!window.__vu)) {
      const vu0 = await idle.evaluate(() => __vu.renders());
      await idle.evaluate(() => window.__sim && __sim.room ? __sim.room(55) : __vu.set(55));
      r.needle = await frames(40); r.vu = await idle.evaluate(n => __vu.renders() - n, vu0);
      if (r.needle !== 0 || r.vu < 1) problems.push(`the room meter's needle moving redraws more than its own canvas: ${JSON.stringify(r)}`);
    }
    report.renders = r;
    if (r.idle !== 0 || r.after !== 0) problems.push(`the front panel draws while nothing changes: ${JSON.stringify(r)}`);
    if (r.knob < 1 || r.knob > 45) problems.push(`a knob turn draws ${r.knob} frames of the front panel (1 to 45 expected)`);
    await idle.close();
    // and in motion (not held still): once the message has landed the panel goes quiet, a knob turned springs to its
    // stop over a few frames, then the panel is quiet again
    const live2 = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    watch(live2, 'idle, live');
    await live2.goto(url('f=99'));
    await live2.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
    const lframes = n => live2.evaluate(n => new Promise(r => { const n0 = __bank.renders(); const f = () => --n ? requestAnimationFrame(f) : r(__bank.renders() - n0); requestAnimationFrame(f); }), n);
    let quiet = -1; for (let i = 0; i < 12 && quiet !== 0; i++) quiet = await lframes(120);
    const lr = { quiet };
    const l0 = await live2.evaluate(() => __bank.renders());
    await live2.click('#k1'); await lframes(60);
    lr.knob = await live2.evaluate(n => __bank.renders() - n, l0); lr.after = await lframes(120);
    report.renders.live = lr;
    if (lr.quiet !== 0 || lr.knob < 4 || lr.knob > 45 || lr.after !== 0) problems.push(`the front panel in motion: ${JSON.stringify(lr)} (quiet, a knob springing over 4 to 45 frames, quiet again)`);
    await live2.close();
  }
  report.fps = await live.evaluate(() => new Promise(r => { let n = 0; const t0 = performance.now(); const f = () => { n++; performance.now() - t0 < 2000 ? requestAnimationFrame(f) : r(Math.round(n / 2)); }; requestAnimationFrame(f); }));
  if (report.fps < 50) problems.push('front frame rate ' + report.fps);
  await live.waitForTimeout(1750);
  await live.screenshot({ path: path.join(out, 'motion.png'), clip: await live.locator('.bank').boundingBox() });
  await live.waitForTimeout(2600);
  const scroll = await live.evaluate(() => ({ top: document.getElementById('log').scrollTop, tab: !document.getElementById('newtab').hidden }));
  if (Math.abs(scroll.top - before.top) > 2) problems.push(`sheet jumped from ${before.top} to ${scroll.top} while the reader was scrolled up`);
  if (!scroll.tab) problems.push('"new below" tab did not appear after a frame landed off screen');
  await live.locator('.sheet').screenshot({ path: path.join(out, 'newtab.png') });
  await live.click('#newtab');
  await live.waitForTimeout(700);
  const after = await live.evaluate(() => { const l = document.getElementById('log'); return { gap: l.scrollHeight - l.scrollTop - l.clientHeight, tab: !document.getElementById('newtab').hidden }; });
  if (after.gap > 30 || after.tab) problems.push(`"new below" did not take the reader to the bottom (gap ${after.gap})`);
  // wait for the drums to come to rest, then read the plain row
  const frameText = () => window.__bank.frame ? __bank.frame().plain : ft(+document.getElementById('fno').textContent - 1);
  await live.waitForFunction(`__bank.rows().plain === (${frameText})()`, null, { timeout: 4000 }).catch(() => {});
  const motion = await live.evaluate(`({ frame: document.getElementById('fno').textContent, plain: __bank.rows().plain, expected: (${frameText})() })`);
  if (motion.expected !== motion.plain) problems.push(`plain row reads ${motion.plain}, frame text is ${motion.expected}`);
  report.motion = motion;
  report.scroll = { before, scroll, after };

  // 5. replying by keyboard
  const n0 = await live.locator('.ent.mine').count();
  await live.focus('#reply');
  await live.keyboard.type('On my way. Twenty minutes.');
  await live.keyboard.press('Shift+Enter');
  await live.keyboard.type('Bringing the radio.');
  await live.keyboard.press('Enter');
  await live.waitForTimeout(300);
  const sent = await live.evaluate(() => {
    const e = [...document.querySelectorAll('.ent.mine')].pop();
    return { count: document.querySelectorAll('.ent.mine').length, text: e && e.querySelector('.typed').textContent, state: e && (e.querySelector('.state') || {}).textContent, box: document.getElementById('reply').value, outbox: document.getElementById('outbox').innerText };
  });
  if (sent.count !== n0 + 1) problems.push('Enter did not add the reply to the sheet');
  if (sent.text !== 'On my way. Twenty minutes.\nBringing the radio.') problems.push(`reply text came out as ${JSON.stringify(sent.text)}`);
  if (sent.box !== '') problems.push('reply box was not cleared after sending');
  await live.locator('.deck').screenshot({ path: path.join(out, 'reply.png') });
  let stamped = false;
  for (let i = 0; i < 40 && !stamped; i++) { await live.waitForTimeout(500); stamped = await live.evaluate(() => !![...document.querySelectorAll('.ent.mine')].pop().querySelector('.stamp')); }
  if (!stamped) problems.push('the reply was never heard (no stamp within 20 s)');
  await live.locator('.sheet').screenshot({ path: path.join(out, 'reply-heard.png') });
  report.reply = { ...sent, stamped };
  // a disabled channel refuses politely
  await live.click('#flash', { force: true });
  const flashRefused = await live.evaluate(() => document.querySelectorAll('.ent.mine').length);
  if (flashRefused !== n0 + 1) problems.push('Flash sent while Light was off');
  await live.close();

  // the Settings plate turns the machine round, then goes to the back panel
  const live2 = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  watch(live2, 'settings');
  await live2.goto(url('still'));
  await live2.click('#settings');
  await live2.waitForTimeout(420);
  const turning = await live2.evaluate(() => document.getElementById('app').classList.contains('turning'));
  await live2.screenshot({ path: path.join(out, 'turn.png') });
  await live2.waitForURL(/settings\.html/, { timeout: 2000 }).catch(() => {});
  if (!turning) problems.push('Settings did not turn the machine round');
  if (!/settings\.html/.test(live2.url())) problems.push('Settings did not go to settings.html');
  await live2.close();

  // 2x crops of the bank, turning and at rest
  const hb = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  watch(hb, 'bank 2x');
  await hb.goto(url());
  await hb.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
  await hb.evaluate(() => new Promise(r => { const f0 = document.getElementById('fno').textContent; const t = setInterval(() => { if (document.getElementById('fno').textContent !== f0) { clearInterval(t); r(); } }, 20); }));
  await hb.waitForTimeout(650);
  await hb.locator('.bank').screenshot({ path: path.join(out, 'bank-turning.png') });
  await hb.waitForTimeout(1600);
  await hb.locator('.bank').screenshot({ path: path.join(out, 'bank-settled.png') });
  await hb.close();

  // 6. reduced motion: drums change letters without spinning
  const rm = await browser.newPage({ viewport: { width: 1440, height: 900 }, reducedMotion: 'reduce' });
  watch(rm, 'reduced motion');
  await rm.goto(url());
  const rmT = await rm.evaluate(() => getComputedStyle(document.querySelector('.rotor')).transitionDuration);
  if (!/^0s/.test(rmT)) problems.push(`reduced motion still spins drums (${rmT})`);
  await rm.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
  const rmSpin = await rm.evaluate(() => new Promise(r => { const f0 = document.getElementById('fno').textContent; const t = setInterval(() => { if (document.getElementById('fno').textContent !== f0) { clearInterval(t); setTimeout(() => r((window.__bank.cells || cells).some(c => !c.snap)), 400); } }, 20); }));
  if (rmSpin) problems.push('reduced motion still turns the 3D drums');
  await rm.close();

  // 6a. the turn to the back panel: its second half shows a still of the settings page as it opens (tools/media.cjs
  // backstill), each within its budget, fixed to the window, and framed as the page it lands on: at each window size the
  // panel in the still (laid as the front lays it) stands where settings.html's opening view has it, to a few pixels
  if (APP) {
    const dir = path.join(path.dirname(page), 'img'), indexFile = path.join(dir, 'back.json');
    const index = fs.existsSync(indexFile) ? JSON.parse(fs.readFileSync(indexFile, 'utf8')) : { sizes: [] };
    if (!index.sizes.length) problems.push(`the turn: no stills of the back listed in ${path.relative(ROOTDIR, indexFile)}`);
    for (const st of index.sizes) {
      const bytes = fs.existsSync(path.join(dir, st.file)) ? fs.statSync(path.join(dir, st.file)).size : 0;
      if (!bytes) problems.push(`the turn: no still of the back at ${st.file}`);
      else if (bytes > 400 * 1024) problems.push(`the turn: the back's still ${st.file} is ${bytes} bytes, over its 400 KB`);
    }
    report.turn = {};
    for (const [w, h] of VIEWPORTS) {
      const tp = await browser.newPage({ viewport: { width: w, height: h } });
      watch(tp, `turn ${w}x${h}`);
      await tp.goto(url('still'));
      await tp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      await tp.evaluate(() => { scrollTo(0, 0); document.body.classList.add('turning'); document.getElementById('app').classList.add('turning'); });
      // the turn's second half over: the back shown and still
      await tp.waitForFunction(() => { const b = document.getElementById('back'); return getComputedStyle(b).visibility === 'visible' && !b.getAnimations().length; }, null, { timeout: 8000 }).catch(() => {});
      const shown = await tp.evaluate(async () => {
        const b = document.getElementById('back'), cs = getComputedStyle(b), src = (cs.backgroundImage.match(/url\("?([^")]+)"?\)/) || [])[1] || '', r = b.getBoundingClientRect();
        const img = src && await new Promise(res => { const i = new Image(); i.onload = () => res([i.naturalWidth, i.naturalHeight]); i.onerror = () => res(null); i.src = src; });
        return { visible: cs.visibility, file: src.split('/').pop(), img, rect: [r.left, r.top, r.width, r.height].map(Math.round), size: cs.backgroundSize, at: cs.backgroundPosition, view: [innerWidth, innerHeight] };
      });
      await tp.screenshot({ path: path.join(out, `turn-back-${w}x${h}.png`) });
      await tp.close();
      const st = index.sizes.find(s => s.file === shown.file);
      // where the settings page, opened at this size, has its panel once its camera has settled
      const sp = await browser.newPage({ viewport: { width: w, height: h } });
      watch(sp, `turn ${w}x${h}: settings`);
      await sp.goto(url('still').replace('bombechat.html', 'settings.html'));
      await sp.waitForFunction(() => { if (!window.__ready) return false; const r = JSON.stringify(__bc.panel());
        window.__still = r === window.__panelWas ? (window.__still || 0) + 1 : 0; window.__panelWas = r; return window.__still > 10; }, null, { timeout: 60000, polling: 'raf' });
      const real = await sp.evaluate(() => __bc.panel());
      await sp.close();
      const t = report.turn[`${w}x${h}`] = { shown, real };
      if (shown.visible !== 'visible' || !st || !shown.img || shown.size !== 'cover' || shown.at !== '0% 0%' || shown.rect.join() !== [0, 0, w, h].join()) { problems.push(`the turn at ${w}x${h} does not show a still of the back fixed to the window: ${JSON.stringify(shown)}`); continue; }
      const k = Math.max(w / st.w, h / st.h), placed = Object.fromEntries(Object.entries(st.panel).map(([e, v]) => [e, v * k]));
      t.placed = placed;
      const off = Math.max(...['left', 'top', 'right', 'bottom'].map(e => Math.abs(placed[e] - real[e])));
      if (off > 3) problems.push(`the turn at ${w}x${h} shows the back's panel ${off.toFixed(1)}px from where settings.html opens it: ${JSON.stringify({ file: st.file, placed, real })}`);
    }
  }

  // 6b. the drums follow the bridge: a known chunk injected through the simulator
  // lands on the plain row, and nothing clicks while it arrives by sound
  if (APP) {
    const br = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    watch(br, 'bridge');
    await br.goto(url());
    await br.mouse.click(5, 5);
    await br.waitForFunction(() => window.__bank && __bank.ready && window.__sim, null, { timeout: 60000 });
    await br.waitForFunction(() => __drum.running, null, { timeout: 5000 }).catch(() => {});
    // a sealed message of real text: the plain row ends on its last twelve characters, letters as themselves and
    // anything else as X, never the sender's keys or the signature
    const KNOWN = 'Look under the blue stairs for the radio.';
    const WANT = [...KNOWN.slice(-12)].map(ch => /[a-z]/i.test(ch) ? ch.toUpperCase() : 'X').join('');
    const bridged = await br.evaluate(async ([known, want]) => {
      const audio = __drum.running;
      __sim.inject(known);
      const receiving = __drum.receiving;
      const during = __drum.spin(5, 1);
      // every turn of the drums goes through Drum.spin: count the turns, and the sounds they made
      const calls0 = __drum.calls, sounds0 = __drum.sounds;
      const t0 = performance.now();
      while (performance.now() - t0 < 25000 && __bank.rows().plain !== want) await new Promise(r => setTimeout(r, 100));
      const plain = __bank.rows().plain;
      await new Promise(r => setTimeout(r, 1200));
      const turns = __drum.calls - calls0, sounds = __drum.sounds - sounds0;
      __sim.receiving(false);
      const after = __drum.spin(5, 1);
      return { audio, receiving, during, turns, sounds, plain, after, ms: Math.round(performance.now() - t0) };
    }, [KNOWN, WANT]);
    report.bridge = bridged;
    if (bridged.plain !== WANT) problems.push(`injected chunk: plain row reads ${bridged.plain}, wanted ${WANT}`);
    if (!bridged.receiving) problems.push('the page did not hear that a message was arriving by sound');
    if (bridged.during !== 0) problems.push(`Drum.spin made ${bridged.during} sounds while a message was arriving by sound`);
    if (!bridged.turns) problems.push('the drums never turned for the injected chunk');
    if (bridged.sounds) problems.push(`the drums made ${bridged.sounds} sounds turning while a message was arriving by sound`);
    if (!bridged.audio) problems.push('audio never started, so muting could not be tested');
    else if (!(bridged.after > 0)) problems.push('Drum.spin stayed silent after reception ended');
    await br.close();

    // a message's last chunk heard first, the ones before it missed: its frames still stand where it ends in the sealed
    // message (the node's chunk size, not the short last chunk's), so its last frame runs through the whole message
    const lc = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    watch(lc, 'last chunk first');
    await lc.goto(url());
    await lc.waitForFunction(() => window.__bank && __bank.ready && window.__sim, null, { timeout: 60000 });
    const LONG = 'Water and blankets at the church on Fifth. The clinic has a radio and the generator runs until Sunday night.!';
    const sealed = await lc.evaluate(t => window.__sim.inject(t, { only: [Math.ceil((68 + new TextEncoder().encode(t).length + 64) / 48) - 1] }), LONG);
    await lc.waitForFunction(n => { const f = __bank.frame(); return f && f.through === n; }, sealed, { timeout: 10000 }).catch(() => {});
    report.lastChunkFirst = { sealed, through: await lc.evaluate(() => (__bank.frame() || {}).through) };
    if (report.lastChunkFirst.through !== sealed) problems.push(`a last chunk heard first stands at the wrong place in its message: ${JSON.stringify(report.lastChunkFirst)}`);
    await lc.close();

    // provisional tape: chunks type onto the sheet in grey, not yet checked; the message
    // from this sheet's contact replaces it, another contact's takes it away, and one
    // that never finishes is struck through (stall shortened to 1.5 s for the test)
    const pv = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    watch(pv, 'provisional');
    await pv.goto(url('stall=1500'));
    await pv.waitForFunction(() => window.__bank && __bank.ready && window.__sim, null, { timeout: 60000 });
    const sheet = () => {
      const ents = [...document.querySelectorAll('#log .ent')];
      const last = ents[ents.length - 1];
      const strips = l => l ? [...l.querySelectorAll('.strip')].map(s => s.textContent).join(' ') : '';
      return { arriving: document.querySelectorAll('#log .ent.arriving').length, failed: document.querySelectorAll('#log .ent.failed').length,
        provisional: strips(document.querySelector('#log .ent.arriving')), note: ((document.querySelector('#log .ent.arriving') || last || document).querySelector('.state') || {}).textContent || '',
        lastText: strips(last), lastProvisional: !!(last && last.querySelector('.provisional')), struck: last ? getComputedStyle(last.querySelector('.strip') || last).textDecorationLine : '' };
    };
    const TEXT = 'Meet at the library at noon.';
    await pv.evaluate(t => __sim.arrive(t, { complete: false }), TEXT);
    await pv.waitForTimeout(300);
    const hanging = await pv.evaluate(`(${sheet})()`);
    if (hanging.provisional !== TEXT) problems.push(`provisional tape reads ${JSON.stringify(hanging.provisional)}, wanted ${JSON.stringify(TEXT)}`);
    if (hanging.note !== 'arriving by sound, not yet checked') problems.push(`provisional note reads ${JSON.stringify(hanging.note)}`);
    await pv.locator('.sheet').screenshot({ path: path.join(out, 'provisional.png') });
    await pv.waitForTimeout(2200);
    const stalled = await pv.evaluate(`(${sheet})()`);
    if (!stalled.failed || stalled.arriving || stalled.note !== 'did not check' || stalled.struck !== 'line-through') problems.push(`a stalled arrival was not struck through: ${JSON.stringify(stalled)}`);
    await pv.locator('.sheet').screenshot({ path: path.join(out, 'provisional-failed.png') });
    await pv.evaluate(() => __sim.arrive('Not for this sheet.', { contact: 2 }));
    await pv.waitForTimeout(3200);
    const other = await pv.evaluate(`(${sheet})()`);
    if (other.arriving || /Not for this sheet/.test(other.lastText)) problems.push(`another contact's message stayed on this sheet: ${JSON.stringify(other)}`);
    await pv.evaluate(() => __sim.arrive('Bring water.'));
    await pv.waitForTimeout(3200);
    const done = await pv.evaluate(`(${sheet})()`);
    if (done.arriving || done.lastText !== 'Bring water.' || done.lastProvisional) problems.push(`the message did not replace its provisional tape: ${JSON.stringify(done)}`);
    report.provisional = { hanging, stalled, other, done };
    await pv.close();

    // a message heard while locked: the node's first tick after the unlock opens it (chunk, then
    // message, held by the bridge while the page sets up) and the store already has it, so the
    // sheet loads it too. It shows once, with no provisional tape left behind. A send the store
    // kept as failed says Not sent.
    const lk = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
    watch(lk, 'locked');
    {
      const said = 'Heard while locked.';
      const bytes = [...Buffer.from(said)];
      const id = Array.from({ length: 16 }, (_, i) => i + 1);
      const heard = { id, contact: 1, outgoing: false, text: said, channel: 'sound', at_min: 29_000_000, heard: false, state: 'sent' };
      const unsent = { id: id.map(b => b + 100), contact: 1, outgoing: true, text: 'Thanks, on my way.', channel: 'sound', at_min: 29_000_001, heard: false, state: 'failed' };
      const plain = [...Array(68).fill(0), ...bytes, ...Array(64).fill(0)];
      const chunk = { tag: 0x0b0e, index: 0, of: 1, view: { plain, heard: plain.map(b => b ^ 1), key: plain.map(() => 1) } };
      await lk.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '90817 26354 55102 83746 19283 74650 31415 92653 58979 32384 62643 38327' }],
        talk: { 1: [heard, unsent] }, afterUnlock: [['chunk', chunk], ['message', heard]] });
      await lk.goto(url(''));
      await lk.waitForSelector('.unlock input[name=pass]');
      await lk.fill('.unlock input[name=pass]', 'correct horse');
      await lk.click('.unlock button');
      await lk.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      await lk.waitForTimeout(3500);
      const shown = await lk.evaluate(t => {
        const ents = [...document.querySelectorAll('#log .ent')];
        const text = e => [...e.querySelectorAll('.strip,.typed')].map(s => s.textContent).join(' ');
        const unsentEnt = ents.find(e => text(e) === 'Thanks, on my way.');
        return { copies: ents.filter(e => text(e) === t).length, arriving: document.querySelectorAll('#log .ent.arriving').length,
          provisional: document.querySelectorAll('#log .provisional').length,
          unsent: unsentEnt ? { failed: unsentEnt.classList.contains('failed'), state: (unsentEnt.querySelector('.state') || {}).textContent } : null };
      }, said);
      if (shown.copies !== 1 || shown.arriving || shown.provisional) problems.push(`a message heard while locked shows ${shown.copies} times after unlock: ${JSON.stringify(shown)}`);
      if (!shown.unsent || !shown.unsent.failed || shown.unsent.state !== 'Not sent.') problems.push(`a send the store kept as failed does not say Not sent: ${JSON.stringify(shown.unsent)}`);
      await lk.locator('.sheet').screenshot({ path: path.join(out, 'locked-then-unlocked.png') });
      report.lockedThenUnlocked = shown;
    }
    await lk.close();

    // the machine's saved settings are the only record: the page draws the switches from settings() and never
    // sends one back at load (set_setting is called 0 times), whatever localStorage holds. A saved Listen
    // boots at Listen with localStorage cleared, or holding an old Listen and send, on the app path and in the
    // simulator; turning the switch then sends exactly that one change.
    report.settingsAtBoot = {};
    for (const where of ['app', 'browser']) for (const stale of [false, true]) {
      const tag = `${where}, localStorage ${stale ? 'stale' : 'cleared'}`;
      const sb = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
      watch(sb, `settings at boot, ${tag}`);
      await sb.addInitScript(stale => { try { localStorage.clear(); if (stale) localStorage.setItem('bombechat.settings', JSON.stringify({ sound: 2, light: 2, quiet: 0 })); } catch (e) {} }, stale);
      if (where === 'app') {
        await sb.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }],
          settings: { sound: 1, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 4, repeats: 2, lock: 1, expire: 0, inperson: 1 } });
        await sb.goto(url('still'));
        await sb.waitForSelector('.unlock input[name=pass]');
        await sb.fill('.unlock input[name=pass]', 'correct horse');
        await sb.click('.unlock button');
      } else {
        // the simulated machine's own record, apart from the page's storage
        await sb.addInitScript(() => { try { sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify({ sound: 1, quiet: 4 })); } catch (e) {} });
        await sb.goto(url('still'));
      }
      await sb.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      await sb.waitForTimeout(400);
      const sets = () => window.__TAURI__ ? __calls.filter(c => c[0] === 'set_setting').map(c => c[1]) : __sim.settingsSet;
      const booted = await sb.evaluate(`({ sound: document.getElementById('k1').getAttribute('aria-valuenow'), light: document.getElementById('k2').getAttribute('aria-valuenow'),
        sendLamp: document.getElementById('sendsound').getAttribute('aria-disabled'), vu: document.querySelector('.vu').title, sets: (${sets})() })`);
      if (booted.sound !== '1') problems.push(`${tag}: a saved Listen booted with the Sound switch at ${booted.sound}`);
      if (booted.light !== '0') problems.push(`${tag}: the Light switch booted at ${booted.light}, the machine keeps Off`);
      if (booted.sendLamp !== 'true') problems.push(`${tag}: Send was offered at Listen`);
      if (!/under 50 dB/.test(booted.vu)) problems.push(`${tag}: Send under is not the machine's 50 dB: ${booted.vu}`);
      if ((Array.isArray(booted.sets) ? booted.sets.length : booted.sets) !== 0) problems.push(`${tag}: the page sent settings at load: ${JSON.stringify(booted.sets)}`);
      if (!stale) await sb.locator('.panel').screenshot({ path: path.join(out, `settings-at-boot-${where}.png`) });
      await sb.click('#k1');
      await sb.waitForTimeout(200);
      const turned = await sb.evaluate(`(${sets})()`);
      const kept = where === 'browser' ? await sb.evaluate(() => import('./bridge.js').then(m => m.bridge.settings())) : null;
      if (where === 'app' ? JSON.stringify(turned) !== JSON.stringify([{ id: 'sound', valueIndex: 2 }]) : turned !== 1 || kept.sound !== 2) problems.push(`${tag}: turning the Sound switch sent ${JSON.stringify(turned)}, kept ${JSON.stringify(kept)}`);
      report.settingsAtBoot[tag] = { ...booted, turned };
      await sb.close();
    }
    // a machine that cannot say its settings: the switches show Off and say so, nothing is sent
    {
      const un = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
      watch(un, 'settings unread');
      await un.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }], settings: 'unread' });
      await un.goto(url('still'));
      await un.waitForSelector('.unlock input[name=pass]');
      await un.fill('.unlock input[name=pass]', 'correct horse');
      await un.click('.unlock button');
      await un.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      const unread = await un.evaluate(() => ({ sound: document.getElementById('k1').getAttribute('aria-valuenow'), light: document.getElementById('k2').getAttribute('aria-valuenow'),
        top: document.getElementById('toplisten').textContent, title: document.getElementById('k1').title, sendOff: document.getElementById('sendsound').getAttribute('aria-disabled'),
        sets: __calls.filter(c => c[0] === 'set_setting').length }));
      if (unread.sound !== '0' || unread.light !== '0' || unread.sendOff !== 'true') problems.push(`unread settings: the switches are not at Off: ${JSON.stringify(unread)}`);
      if (unread.top !== 'Switches not read' || !/not read from the machine/.test(unread.title)) problems.push(`unread settings: the page does not say the switches were not read: ${JSON.stringify(unread)}`);
      if (unread.sets) problems.push(`unread settings: the page sent ${unread.sets} settings`);
      await un.locator('.top').screenshot({ path: path.join(out, 'settings-unread-top.png') });
      await un.locator('.panel').screenshot({ path: path.join(out, 'settings-unread-panel.png') });
      report.settingsUnread = unread;
      await un.close();
    }
    // the machine won't take a turn of the Sound switch (the setting could not be saved): the knob goes back to where
    // the machine keeps it, the switches say so, and the top bar says why, at 1024, 1440 and 1728
    report.knobRefused = {};
    for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
      const kr = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
      watch(kr, `knob refused ${w}`);
      const WHY = 'the setting was not saved: disk full';
      await kr.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }], refuseSetting: WHY });
      await kr.goto(url('still'));
      await kr.waitForSelector('.unlock input[name=pass]');
      await kr.fill('.unlock input[name=pass]', 'correct horse');
      await kr.click('.unlock button');
      await kr.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      await kr.waitForTimeout(300);
      await kr.click('#k1');
      await kr.waitForTimeout(500);
      const got = await kr.evaluate(() => ({ sound: document.getElementById('k1').getAttribute('aria-valuenow'), cap: document.querySelector('#k1 .cap').style.transform,
        top: document.getElementById('toplisten').textContent, sendOff: document.getElementById('sendsound').getAttribute('aria-disabled'),
        said: document.getElementById('said').textContent, shown: +getComputedStyle(document.getElementById('said')).opacity, size: parseFloat(getComputedStyle(document.getElementById('said')).fontSize),
        sets: __calls.filter(c => c[0] === 'set_setting').map(c => c[1]),
        // the bar keeps every plate whole and inside the window, and the sentence is never cut
        fit: (() => { const t = document.querySelector('.top'), s = document.getElementById('said'), set = document.getElementById('settings').getBoundingClientRect(), r = s.getBoundingClientRect();
          return { bar: t.scrollWidth - t.clientWidth, settings: Math.round(set.right) <= innerWidth, saidCut: s.scrollWidth - s.clientWidth, saidIn: r.left >= 0 && Math.round(r.right) <= innerWidth }; })() }));
      if (got.fit.bar > 0 || !got.fit.settings || got.fit.saidCut > 1 || !got.fit.saidIn) problems.push(`knob refused ${w}: the top bar overflows or cuts what it says: ${JSON.stringify(got.fit)}`);
      if (got.sound !== '2' || !/rotate\(120deg\)/.test(got.cap) || got.top !== 'Listening by sound' || got.sendOff === 'true') problems.push(`knob refused ${w}: the Sound switch did not go back to Listen and send: ${JSON.stringify(got)}`);
      if (got.said !== 'The setting was not saved: disk full' || got.shown < .9 || got.size < 12) problems.push(`knob refused ${w}: the reason is not shown: ${JSON.stringify(got)}`);
      if (JSON.stringify(got.sets) !== JSON.stringify([{ id: 'sound', valueIndex: 0 }])) problems.push(`knob refused ${w}: sent ${JSON.stringify(got.sets)}`);
      await kr.locator('.top').screenshot({ path: path.join(out, `knob-refused-top-${w}.png`) });
      report.knobRefused[w] = got;
      await kr.close();
    }
    // the stations, drawn again when a pairing adds someone: the sheet's contact first, with the newer of the store's
    // last message and one heard on this page since; the new contact after; every line whole or cut with an ellipsis
    {
      const sp = await browser.newPage({ viewport: { width: 1440, height: 900 } });
      watch(sp, 'stations');
      const OLD = Math.floor(Date.now() / 60000) - 90;
      await sp.addInitScript(tauriStub, { hello: 'Teodor Vance', safety: '11111 22222 33333 44444 55555 66666 77777 88888 99999 00000 12121 34343', pairOfferMs: 300,
        people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }], talk: { 1: [{ id: Array(16).fill(1), contact: 1, outgoing: false, text: 'Old news.', channel: 'Sound', at_min: OLD, heard: false }] } });
      await sp.goto(url('still'));
      await sp.waitForSelector('.unlock input[name=pass]');
      await sp.fill('.unlock input[name=pass]', 'correct horse'); await sp.click('.unlock button');
      await sp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      const line = () => sp.evaluate(() => [...document.querySelectorAll('.row3 .stn')].map(s => ({ id: s.dataset.contact, text: s.querySelector('.holder span').innerText.replace(/\s+/g, ' ').trim(),
        cut: [...s.querySelectorAll('.holder span, .holder small')].some(e => e.scrollWidth > e.clientWidth + 1 && getComputedStyle(e).textOverflow !== 'ellipsis') })));
      const before = await line();
      const hhmm = ms => { const d = new Date(ms); return String(d.getHours()).padStart(2, '0') + '.' + String(d.getMinutes()).padStart(2, '0'); };
      if (JSON.stringify(before) !== JSON.stringify([{ id: '1', text: `Marisol Ortega sound, ${hhmm(OLD * 60000)}`, cut: false }])) problems.push(`stations: at open ${JSON.stringify(before)}`);
      const NOW = Math.floor(Date.now() / 60000);
      await sp.evaluate(at => __emit('message', { id: Array(16).fill(2), contact: 1, outgoing: false, text: 'New news.', channel: 'Light', at_min: at, heard: false }), NOW);
      await sp.click('#pairbtn');
      await sp.waitForFunction(() => document.querySelector('.menu').dataset.pair === 'offer', null, { timeout: 8000 });
      await sp.click('#pairyes');
      await sp.waitForFunction(() => document.querySelectorAll('.row3 .stn').length === 2, null, { timeout: 8000 }).catch(() => {});
      const after = await line();
      if (JSON.stringify(after) !== JSON.stringify([{ id: '1', text: `Marisol Ortega light, ${hhmm(NOW * 60000)}`, cut: false }, { id: '7', text: 'Teodor Vance not heard yet', cut: false }])) problems.push(`stations: after pairing ${JSON.stringify(after)}`);
      await sp.close();
    }
    // Machine sound and Machine volume are the machine's settings: the title bar's lever shows what it keeps and the
    // drums play at its volume; moving the lever sends that one change. The Light switch is live: no tape, it boots at
    // the machine's Off, and each turn sends that one change. At Read and flash, a notice by light reaches the page as
    // display frames (the Normal grid, 56 cells a side) and then displayStopped for the same tag.
    for (const where of ['app', 'browser']) {
      const ms = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
      watch(ms, `machine sound, ${where}`);
      if (where === 'app') {
        await ms.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }],
          settings: { sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 0, machvol: 4, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 } });
        await ms.goto(url('still'));
        await ms.waitForSelector('.unlock input[name=pass]');
        await ms.fill('.unlock input[name=pass]', 'correct horse');
        await ms.click('.unlock button');
      } else {
        await ms.addInitScript(() => { try { sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify({ machine: 0, machvol: 4 })); } catch (e) {} });
        await ms.goto(url('still'));
      }
      await ms.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      await ms.waitForTimeout(300);
      const sets = () => window.__TAURI__ ? __calls.filter(c => c[0] === 'set_setting').map(c => c[1]) : __sim.settingsSet;
      const booted = await ms.evaluate(`({ lever: document.getElementById('snd').getAttribute('aria-pressed'), on: __drum.on, gain: __drum.gain, label: document.querySelector('.sndlab').textContent,
        light: document.getElementById('k2').getAttribute('aria-valuenow'), lightOff: document.getElementById('k2').getAttribute('aria-disabled'), tape: (document.querySelector('.tapedown') || {}).textContent, sets: (${sets})() })`);
      await ms.locator('.mod:has(#k2)').screenshot({ path: path.join(out, `light-live-${where}.png`) });
      await ms.click('#k2', { force: true });
      await ms.focus('#k2'); await ms.keyboard.press('ArrowRight');
      await ms.waitForTimeout(400);
      const light = await ms.evaluate(`({ light: document.getElementById('k2').getAttribute('aria-valuenow'), flash: document.getElementById('flash').getAttribute('aria-disabled'), sets: (${sets})() })`);
      await ms.locator('.mod:has(#k2)').screenshot({ path: path.join(out, `light-flash-${where}.png`) });
      // the frames a notice by light hands the page, through the bridge as the page gets them
      const shown = await ms.evaluate(where === 'app'
        ? () => import('./bridge.js').then(({ bridge }) => { const got = { frames: [], stopped: [] };
            bridge.on('display', d => got.frames.push(d)); bridge.on('displayStopped', d => got.stopped.push(d));
            window.__emit('display', { tag: 9, bits: Array(392).fill(165), spec: 1, index: 0 }); window.__emit('displayStopped', { tag: 9 });
            return got; })
        : () => import('./bridge.js').then(async ({ bridge }) => { const got = { frames: [], stopped: [] };
            bridge.on('display', d => got.frames.push(d)); bridge.on('displayStopped', d => got.stopped.push(d));
            await bridge.broadcast('Water at the church.', 'light'); await new Promise(r => setTimeout(r, 5800));
            const light = (await bridge.channels()).find(c => c.channel === 'light');
            return { ...got, rate: light && light.rate }; }));
      const lightSets = where === 'app' ? light.sets.length : light.sets;
      await ms.click('#snd');
      await ms.waitForTimeout(200);
      const flipped = await ms.evaluate(`({ lever: document.getElementById('snd').getAttribute('aria-pressed'), on: __drum.on, sets: (${sets})() })`);
      if (where === 'app') flipped.sets = flipped.sets.slice(lightSets); else flipped.sets -= lightSets;
      const kept = where === 'browser' ? await ms.evaluate(() => import('./bridge.js').then(m => m.bridge.settings())) : null;
      const none = s => (Array.isArray(s) ? s.length : s) === 0;
      if (booted.lever !== 'false' || booted.on !== false) problems.push(`${where}: the machine keeps Machine sound off, the lever shows ${JSON.stringify(booted)}`);
      if (Math.abs(booted.gain - .9) > 1e-6) problems.push(`${where}: Machine volume 5 plays the drums at ${booted.gain}, not 0.9`);
      if (booted.label !== 'Machine sound') problems.push(`${where}: the lever is labelled ${booted.label}`);
      if (!none(booted.sets)) problems.push(`${where}: settings sent at load: ${JSON.stringify(booted.sets)}`);
      if (booted.light !== '0' || booted.lightOff === 'true' || booted.tape !== undefined) problems.push(`${where}: the Light switch is not live at Off: ${JSON.stringify(booted)}`);
      if (light.light !== '2' || light.flash !== 'false' || (where === 'app' ? JSON.stringify(light.sets) !== JSON.stringify([{ id: 'light', valueIndex: 1 }, { id: 'light', valueIndex: 2 }]) : light.sets !== 2 || kept.light !== 2))
        problems.push(`${where}: turning the Light switch twice did not reach Read and flash: ${JSON.stringify(light)}, kept ${JSON.stringify(kept)}`);
      const frames = shown.frames, tag = frames.length && frames[0].tag;
      if (!frames.length || frames.some(f => f.tag !== tag || f.bits.length !== 392 || f.spec !== 1) || frames.some((f, i) => f.index !== i)
        || JSON.stringify(shown.stopped) !== JSON.stringify([{ tag }]) || (where === 'browser' && (frames.length !== 52 || shown.rate !== 2290)))
        problems.push(`${where}: a notice by light did not reach the page as frames: ${frames.length} frames, first ${JSON.stringify(frames[0] && { ...frames[0], bits: frames[0].bits.length })}, stopped ${JSON.stringify(shown.stopped)}, rate ${shown.rate}`);
      if (flipped.lever !== 'true' || !flipped.on || (where === 'app' ? JSON.stringify(flipped.sets) !== JSON.stringify([{ id: 'machine', valueIndex: 1 }]) : flipped.sets !== 1 || kept.machine !== 1))
        problems.push(`${where}: the lever sent ${JSON.stringify(flipped)}, kept ${JSON.stringify(kept)}`);
      report[`machineSound-${where}`] = { booted, light, flipped, frames: shown.frames.length };
      await ms.close();
    }

    // the passphrase form on the app path (window.__TAURI__ stubbed) at 1024, 1440 and 1728, with 2x crops
    // of each state (unlock-<w>-<state>.png): first run empty and focused on the name; refused without a
    // name, with no passphrase and with two that differ, each typed under its field and announced; the
    // stamp down and the key's wheels turning while the key is worked out; a later run asking once and
    // refusing a wrong passphrase; and the Keychain launch (Lock Off), where the form never appears.
    // Every field has a label, Tab runs name, passphrase, again, stamp, and Enter submits.
    report.unlockForm = {};
    const LATER = [{ id: 3, name: 'Marisol Ortega', checked: true }];
    for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
      const uf = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
      watch(uf, `unlock form ${w}`);
      await uf.addInitScript(tauriStub, { unlockMs: 1100 });
      await uf.goto(url('still'));
      await uf.waitForSelector('.unlock input[name=who]');
      await uf.evaluate(() => document.fonts.ready);
      // crops wait for everything but the key's turning wheels to come to rest
      const settle = pg => pg.evaluate(() => Promise.all(document.getAnimations().filter(a => a.effect && a.effect.getComputedTiming().iterations !== Infinity).map(a => a.finished.catch(() => {}))));
      const crop = async st => { await settle(uf); const r = await uf.locator('.unlock .stand').boundingBox(); await uf.screenshot({ path: path.join(out, `unlock-${w}-${st}.png`), clip: { x: r.x - 12, y: r.y - 12, width: r.width + 24, height: r.height + 24 } }); };
      const state = () => uf.evaluate(() => {
        const f = document.querySelector('.unlock'), e = f && f.querySelector('.err'), bad = f && f.querySelector('.row.bad input');
        const labels = f ? [...f.querySelectorAll('input')].map(i => i.labels.length ? i.labels[0].textContent.trim() : '') : [];
        const text = f ? f.innerText : '';
        return { shown: !!f, focus: document.activeElement && document.activeElement.name, err: e ? e.textContent : '', alert: e ? e.getAttribute('role') : '', bad: bad ? bad.name : '',
          invalid: bad ? bad.getAttribute('aria-invalid') : '', working: !!f && f.classList.contains('working'), status: f ? f.querySelector('[role=status]').textContent : '', labels,
          dashes: /—|–|--/.test(text), turning: f ? getComputedStyle(f.querySelector('.turn .odo i span')).animationName : '' };
      });
      const check = (cond, what, st) => { if (!cond) problems.push(`unlock form ${w}: ${what}: ${JSON.stringify(st)}`); };
      let st = await state();
      check(st.focus === 'who' && JSON.stringify(st.labels) === JSON.stringify(['Your nameas people you pair with see it', 'Passphrase', 'Againto check it']) && !st.dashes, 'first run should focus a labelled name field', st);
      const box = await uf.locator('.unlock .stand').boundingBox();
      check(box.y >= 0 && box.y + box.height <= h && Math.abs(box.x + box.width / 2 - w / 2) < 2, 'the form should sit in the middle of the window', box);
      const small = (await uf.evaluate(`(${measureText.toString()})()`));
      check(!small.length, 'text under 12px', small);
      // keyboard order: name, passphrase, again, the stamp
      const order = [];
      for (let i = 0; i < 4; i++) { order.push(await uf.evaluate(() => document.activeElement.name || document.activeElement.textContent)); await uf.keyboard.press('Tab'); }
      check(JSON.stringify(order) === JSON.stringify(['who', 'pass', 'again', 'Lock and open']), 'Tab order', order);
      await uf.focus('.unlock input[name=who]');
      await crop('firstrun');
      // Enter with no name: refused under the name, and refused again (said again) on a second Enter
      const turnAt = () => uf.evaluate(() => ['.turn .jewel', '.turn .odo'].map(q => Math.round(document.querySelector('.unlock ' + q).getBoundingClientRect().left)));
      const restAt = await turnAt();
      await uf.evaluate(() => { window.__said = []; new MutationObserver(() => { const e = document.querySelector('.unlock .err'); if (e && e.textContent) window.__said.push(e.textContent); }).observe(document.querySelector('.unlock .card'), { childList: true, subtree: true, characterData: true }); });
      await uf.keyboard.press('Enter');
      await uf.keyboard.press('Enter');
      st = await state();
      check(st.err === 'Write your name.' && st.bad === 'who' && st.alert === 'alert' && st.invalid === 'true' && st.focus === 'who', 'no name should be refused under the name', st);
      const said = await uf.evaluate(() => window.__said);
      check(said.length === 2 && said.every(t => t === 'Write your name.'), 'the same refusal twice should be said twice', said);
      const errShown = await uf.evaluate(() => { const e = document.querySelector('.unlock .err'); return getComputedStyle(e).display; });
      check(errShown !== 'none', 'the refusal region should stay rendered', errShown);
      await crop('no-name');
      await uf.keyboard.type('Ana Lima'); await uf.keyboard.press('Enter');
      st = await state();
      check(st.err === 'Choose a passphrase.' && st.bad === 'pass', 'no passphrase should be refused', st);
      await uf.keyboard.type('correct horse'); await uf.keyboard.press('Tab'); await uf.keyboard.type('correct horsx'); await uf.keyboard.press('Enter');
      st = await state();
      check(st.err === 'The two passphrases do not match.' && st.bad === 'again' && st.focus === 'again', 'differing passphrases should be refused under Again', st);
      await crop('mismatch');
      const none = await uf.evaluate(() => __calls.filter(c => c[0] === 'unlock').length);
      check(none === 0, 'an unlock was sent before the form was right', none);
      await uf.keyboard.type('correct horse'); await uf.keyboard.press('Enter');
      await uf.waitForTimeout(450);
      st = await state();
      check(st.working && st.status === 'Locking' && st.err === '' && st.turning === 'wheel', 'the key should be worked out with the stamp down', st);
      const workAt = await turnAt();
      check(JSON.stringify(workAt) === JSON.stringify(restAt), 'the lamp and counter moved when the plate changed', { restAt, workAt });
      await crop('locking');
      await uf.waitForFunction(() => !document.querySelector('.unlock'), null, { timeout: 10000 });
      const sent = await uf.evaluate(() => __calls.filter(c => c[0] === 'unlock').map(c => c[1]));
      check(JSON.stringify(sent) === JSON.stringify([{ passphrase: 'correct horse', name: 'Ana Lima' }]), 'the unlock carried', sent);
      report.unlockForm[w] = { firstRun: 'ok' };
      await uf.close();

      // a later run: the passphrase once; a wrong one is refused and the form stays
      const lr = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
      watch(lr, `unlock form later ${w}`);
      await lr.addInitScript(tauriStub, { people: LATER, unlockMs: 900, wrong: 1 });
      await lr.goto(url('still'));
      await lr.waitForSelector('.unlock input[name=pass]');
      await lr.evaluate(() => document.fonts.ready);
      const lcrop = async st2 => { await settle(lr); const r = await lr.locator('.unlock .stand').boundingBox(); await lr.screenshot({ path: path.join(out, `unlock-${w}-${st2}.png`), clip: { x: r.x - 12, y: r.y - 12, width: r.width + 24, height: r.height + 24 } }); };
      const later = await lr.evaluate(() => ({ fields: [...document.querySelectorAll('.unlock input')].map(i => i.name), focus: document.activeElement.name, head: document.querySelector('.unlock h2').textContent,
        numbered: !!document.querySelector('.unlock ol') }));
      if (JSON.stringify(later) !== JSON.stringify({ fields: ['pass'], focus: 'pass', head: 'Unlock', numbered: false })) problems.push(`unlock form ${w}: a later run should ask the passphrase once: ${JSON.stringify(later)}`);
      await lcrop('unlock');
      await lr.keyboard.type('wrong horse'); await lr.keyboard.press('Enter');
      await lr.waitForTimeout(300);
      const opening = await lr.evaluate(() => document.querySelector('.unlock h2').textContent);
      if (opening !== 'Unlocking') problems.push(`unlock form ${w}: the heading while working is ${opening}`);
      await lcrop('opening');
      await lr.waitForFunction(() => !document.querySelector('.unlock.working'), null, { timeout: 10000 });
      const wrongShown = await lr.evaluate(() => ({ err: document.querySelector('.unlock .err').textContent, bad: (document.querySelector('.unlock .row.bad input') || {}).name, focus: document.activeElement.name,
        selected: document.activeElement.selectionEnd - document.activeElement.selectionStart }));
      if (wrongShown.err !== 'Wrong passphrase.' || wrongShown.bad !== 'pass' || wrongShown.focus !== 'pass' || wrongShown.selected !== 11) problems.push(`unlock form ${w}: a wrong passphrase: ${JSON.stringify(wrongShown)}`);
      await lcrop('wrong');
      await lr.keyboard.type('correct horse'); await lr.keyboard.press('Enter');
      await lr.waitForFunction(() => !document.querySelector('.unlock'), null, { timeout: 10000 });
      await lr.close();
    }
    // a store made by a newer build: the unlock form says so, in its own words, under the passphrase
    {
      const nw = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
      watch(nw, 'unlock newer store');
      await nw.addInitScript(tauriStub, { people: LATER, wrong: 1, wrongWhy: 'This store was made by a newer Bombechat.' });
      await nw.goto(url('still'));
      await nw.waitForSelector('.unlock input[name=pass]');
      await nw.fill('.unlock input[name=pass]', 'correct horse');
      await nw.click('.unlock button');
      await nw.waitForFunction(() => !document.querySelector('.unlock.working') && document.querySelector('.unlock .err').textContent, null, { timeout: 10000 });
      const newer = await nw.evaluate(() => ({ err: document.querySelector('.unlock .err').textContent, bad: (document.querySelector('.unlock .row.bad input') || {}).name }));
      if (newer.err !== 'This store was made by a newer Bombechat.' || newer.bad !== 'pass') problems.push(`unlock newer store: ${JSON.stringify(newer)}`);
      const r = await nw.locator('.unlock .stand').boundingBox();
      await nw.screenshot({ path: path.join(out, 'unlock-1440-newer.png'), clip: { x: r.x - 12, y: r.y - 12, width: r.width + 24, height: r.height + 24 } });
      report.unlockForm.newer = newer;
      await nw.close();
    }
    // the Keychain launch (Lock Off): the store is open before the page asks, and no form appears
    {
      const kc = await browser.newPage({ viewport: { width: 1440, height: 900 } });
      watch(kc, 'keychain launch');
      await kc.addInitScript(tauriStub, { people: [{ id: 3, name: 'Marisol Ortega', checked: true }], keychain: true });
      await kc.addInitScript(() => new MutationObserver(m => { if (document.querySelector('.unlock')) window.__formSeen = true; }).observe(document, { childList: true, subtree: true }));
      await kc.goto(url('still'));
      await kc.waitForFunction(() => document.documentElement.classList.contains('real'), null, { timeout: 60000 });
      const kcSeen = await kc.evaluate(() => ({ seen: !!window.__formSeen, unlocks: __calls.filter(c => c[0] === 'unlock').length }));
      if (kcSeen.seen || kcSeen.unlocks) problems.push(`keychain launch showed the passphrase form: ${JSON.stringify(kcSeen)}`);
      report.unlockForm.keychain = kcSeen;
      await kc.close();
    }

    // pairing by sound on the key check card, in a plain browser (the simulator) and in the app
    // (window.__TAURI__ stubbed, from first run), at 1024, 1440 and 1728, with 2x crops of each state
    // (card-<path>-<width>-<state>.png): listening; the offer's 60 digits in 12 pencilled groups,
    // legible and inside the card, under their name in an editable field; Matched, add keeps the
    // name as corrected and stamps the card without moving the number; Doesn't match keeps nothing
    // and leaves a clean card. The bytes-a-second readout shows the rate the bridge reports.
    report.pairing = {};
    for (const where of ['browser', 'app']) for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
      const tag = `${where} ${w}`;
      const pr = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
      watch(pr, `pairing ${tag}`);
      if (where === 'app') await pr.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '90817 26354 55102 83746 19283 74650 31415 92653 58979 32384 62643 38327' });
      await pr.goto(url(where === 'app' ? 'still&hold=1500' : 'still'));
      if (where === 'app') {
        // first run: the passphrase card asks for your name, and the unlock carries it
        await pr.waitForSelector('.unlock input[name=who]');
        await pr.screenshot({ path: path.join(out, `card-app-${w}-firstrun.png`), clip: await pr.locator('.unlock .stand').boundingBox() });
        await pr.fill('.unlock input[name=pass]', 'correct horse'); await pr.fill('.unlock input[name=again]', 'correct horse');
        await pr.click('.unlock button');
        const refusedNameless = await pr.evaluate(() => document.querySelector('.unlock .err').textContent);
        if (refusedNameless !== 'Write your name.') problems.push(`${tag}: first run went on without a name (${refusedNameless})`);
        await pr.fill('.unlock input[name=who]', '  Ana Lima ');
        await pr.click('.unlock button');
      }
      await pr.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      if (where === 'app') {
        const unlocked = await pr.evaluate(() => __calls.filter(c => c[0] === 'unlock').map(c => c[1]));
        if (JSON.stringify(unlocked) !== JSON.stringify([{ passphrase: 'correct horse', name: 'Ana Lima' }])) problems.push(`${tag}: unlock was not given the name: ${JSON.stringify(unlocked)}`);
      }
      const card = () => {
        const m = document.querySelector('.menu'), sn = [...m.querySelectorAll('.sn li')], g = m.querySelector('.sn');
        const vis = e => e && !e.hidden && !e.closest('[hidden]') && e.getBoundingClientRect().width > 0;
        const nm = document.getElementById('peername');
        return { state: m.dataset.pair, pencil: (m.querySelector('#pencil:not([hidden])') || { innerText: '' }).innerText.replace(/\s+/g, ' ').trim(),
          name: nm ? nm.value : '', groups: sn.map(li => li.textContent), groupPx: [...new Set(sn.map(li => parseFloat(getComputedStyle(li).fontSize)))],
          groupsFit: sn.every(li => { const r = li.getBoundingClientRect(), c = m.getBoundingClientRect(); return r.left >= c.left && r.right <= c.right && li.scrollWidth <= li.clientWidth + 1; }),
          gridAt: g ? Math.round(g.getBoundingClientRect().top * 2) / 2 : null,
          note: m.querySelector('.note').textContent, stamp: vis(m.querySelector('.gstmp')) ? m.querySelector('.gstmp').innerText.replace(/\s+/g, ' ').trim() : '',
          oldStamp: vis(m.querySelector('.stmp')), yes: vis(document.getElementById('pairyes')), pairBtn: vis(document.getElementById('pairbtn')) ? document.getElementById('pairbtn').textContent : '',
          who: m.querySelector('h2 span').textContent, sheetWith: document.querySelector('.fields div:first-child b').textContent };
      };
      const contacts = () => window.__TAURI__ ? __calls.filter(c => c[0] === 'confirm_pair').map(c => c[1].name) : import('./bridge.js').then(m => m.bridge.contacts()).then(l => l.map(c => c.name));
      const shot = st => pr.locator('.menu').screenshot({ path: path.join(out, `card-${where}-${w}-${st}.png`) });
      const rate = await pr.evaluate(() => ({ shown: document.getElementById('rate').textContent, visible: getComputedStyle(document.getElementById('raterd')).visibility === 'visible' && !document.getElementById('raterd').hidden }));
      if (!rate.visible || rate.shown !== '016') problems.push(`${tag}: bytes a second reads ${JSON.stringify(rate)}, wanted the reported 16.1 as 016`);
      await shot('rest');
      if (where === 'app' && w === 1440) {
        // a hello heard while not listening is offered for a while, then the note clears (hold shortened to 1.5 s)
        await pr.evaluate(() => __emit('pairOffer', { peer: { sign: Array(32).fill(3), dh: Array(32).fill(4) }, name: 'Teodor Vance', safety: '11111 22222 33333 44444 55555 66666 77777 88888 99999 00000 12121 34343' }));
        const heldNote = await pr.evaluate(() => document.querySelector('.menu .note').textContent);
        await pr.waitForTimeout(1900);
        const lapsed = await pr.evaluate(() => document.querySelector('.menu .note').textContent);
        if (heldNote !== 'Teodor Vance said hello by sound. Press Pair by sound to answer.' || /said hello/.test(lapsed)) problems.push(`${tag}: a held hello's note: ${JSON.stringify({ heldNote, lapsed })}`);
      }
      await pr.click('#pairbtn');
      await pr.waitForTimeout(300);
      const listening = await pr.evaluate(`(${card})()`);
      if (listening.state !== 'listening' || !/listening for a hello/.test(listening.pencil) || listening.pairBtn !== 'Stop listening' || listening.who !== 'pairing by sound') problems.push(`${tag}: Pair by sound did not start listening: ${JSON.stringify(listening)}`);
      await shot('listening');
      await pr.waitForFunction(() => document.querySelector('.menu').dataset.pair === 'offer', null, { timeout: 8000 }).catch(() => {});
      const offered = await pr.evaluate(`(${card})()`);
      const hello = where === 'app' ? 'Marisol Ortega' : 'Teodor Vance';
      if (offered.state !== 'offer') problems.push(`${tag}: no offer reached the card after Pair by sound`);
      if (offered.groups.length !== 12 || offered.groups.some(g => !/^\d{5}$/.test(g))) problems.push(`${tag}: the offer's safety number is not 12 groups of five: ${offered.groups.join(' ')}`);
      if (offered.groupPx.some(px => px < 12)) problems.push(`${tag}: safety number drawn under 12px: ${offered.groupPx}`);
      if (!offered.groupsFit) problems.push(`${tag}: safety number groups spill out of the card`);
      if (offered.name !== hello || offered.note !== 'Compare these with theirs, out loud or side by side.' || !offered.yes) problems.push(`${tag}: the offer card reads wrong: ${JSON.stringify(offered)}`);
      await shot('offer');
      // correct their name before adding them
      const kept = where === 'app' ? 'Marisol O.' : 'Teodor V.';
      await pr.fill('#peername', kept);
      await shot('offer-named');
      await pr.click('#pairyes');
      await pr.waitForTimeout(500);
      const matched = await pr.evaluate(`(${card})()`);
      if (matched.state !== '' || !/^Matched \d+ [A-Z][a-z]{2}$/.test(matched.stamp) || matched.oldStamp || matched.groups.join(' ') !== offered.groups.join(' ') || matched.who !== kept || matched.note !== 'Checked in person, by sound.') problems.push(`${tag}: Matched, add did not stamp the card: ${JSON.stringify(matched)}`);
      if (where === 'browser' && matched.stamp !== 'Matched 24 Oct') problems.push(`${tag}: the stamp is not dated today: ${matched.stamp}`);
      if (Math.abs(matched.gridAt - offered.gridAt) > 1) problems.push(`${tag}: the number moved from ${offered.gridAt} to ${matched.gridAt} when it was matched`);
      if (where === 'app' && matched.sheetWith !== kept) problems.push(`${tag}: the first contact did not become the sheet's: ${matched.sheetWith}`);
      const added = await pr.evaluate(contacts);
      if (!added.includes(kept)) problems.push(`${tag}: confirmPair was not given the name as corrected: ${added}`);
      await shot('matched');
      // the other way: an offer that doesn't match keeps nothing and leaves a clean card
      await pr.click('#pairbtn');
      await pr.waitForFunction(() => document.querySelector('.menu').dataset.pair === 'offer', null, { timeout: 8000 }).catch(() => {});
      await pr.click('#pairno');
      await pr.waitForTimeout(300);
      const refused = await pr.evaluate(`(${card})()`);
      const after = await pr.evaluate(contacts);
      if (refused.state !== 'refused' || refused.note !== 'Not added. Nothing was kept.' || refused.groups.length || refused.stamp || refused.oldStamp || refused.yes || refused.pairBtn !== 'Pair by sound' || !/not added/.test(refused.pencil)) problems.push(`${tag}: Doesn't match did not leave a clean card: ${JSON.stringify(refused)}`);
      if (JSON.stringify(after) !== JSON.stringify(added)) problems.push(`${tag}: Doesn't match changed the contacts: ${JSON.stringify(added)} to ${JSON.stringify(after)}`);
      await shot('refused');
      if (where === 'app') {
        // Pair by sound asks for sound only, and Doesn't match ends the pairing in the machine
        const asked = await pr.evaluate(() => ({ pairs: __calls.filter(c => c[0] === 'pair').map(c => c[1]), stops: __calls.filter(c => c[0] === 'stop_pairing').length }));
        if (asked.pairs.length !== 2 || asked.pairs.some(a => JSON.stringify(a) !== '{"channel":"sound"}') || asked.stops < 1) problems.push(`${tag}: pairing by sound asked the machine ${JSON.stringify(asked)}`);
      }
      report.pairing[tag] = { listening: listening.state, offered: { name: offered.name, groupPx: offered.groupPx, gridAt: offered.gridAt }, matched: { stamp: matched.stamp, gridAt: matched.gridAt, who: matched.who }, refused: refused.note, contacts: after };
      await pr.close();
    }

    // writing a public notice, in the browser (the simulator) and on the app path, at 1024, 1440 and 1728:
    // the toggle makes the reply line a public notice that says anyone in range can read it and which key
    // signs it, and a reader at the bottom of the sheet stays there; empty and too long (by the machine's
    // broadcast_max) are refused with the reason and nothing is sent; Flash refuses a notice; one that goes
    // lands on the sheet as a signed public entry with its status, has a public outbox slip, and the toggle
    // springs back to Reply; at 1440 it is still there, once, after a reload. 2x crops of the sheet.
    report.broadcast = {};
    const people = [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }];
    const switches = sound => ({ sound, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 });
    const unlock = async bp => {
      await bp.waitForSelector('.unlock input[name=pass]');
      await bp.fill('.unlock input[name=pass]', 'correct horse');
      await bp.click('.unlock button');
    };
    const settle = async bp => {
      await bp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
      await bp.evaluate(() => document.fonts.ready);
      await bp.waitForTimeout(150);
    };
    // sound: the machine's Sound switch (browser: the simulated machine's record), or 'unread'
    const openSheet = async (where, w, h, { sound = 2, live = false, stub = {} } = {}) => {
      const bp = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
      watch(bp, `broadcast ${where} ${w} sound ${sound}`);
      if (where === 'app') {
        await bp.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people, settings: sound === 'unread' ? 'unread' : switches(sound), ...stub });
        await bp.goto(url('still'));
        await unlock(bp);
      } else {
        if (sound !== 2) await bp.addInitScript(s => { try { sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify({ sound: s })); } catch (e) {} }, sound);
        await bp.goto(url(live ? '' : 'still'));
      }
      await settle(bp);
      return bp;
    };
    const line = () => ({ public: document.getElementById('mode').getAttribute('aria-checked'), note: document.getElementById('pubnote').textContent,
      noteShown: getComputedStyle(document.getElementById('pubnote')).display !== 'none', refused: document.getElementById('pubnote').classList.contains('refused'),
      box: document.getElementById('reply').value, placeholder: document.getElementById('reply').placeholder,
      calls: window.__TAURI__ ? __calls.filter(c => c[0] === 'broadcast').map(c => c[1].text) : null,
      replies: document.querySelectorAll('#log .ent.mine:not(.public)').length,
      mine: [...document.querySelectorAll('#log .ent.public.mine')].map(e => ({ how: e.querySelector('.when span').textContent, text: e.querySelector('.typed').textContent, state: (e.querySelector('.state') || {}).textContent, failed: e.classList.contains('failed') })),
      slips: [...document.querySelectorAll('#outbox .slip:not(.empty)')].map(s => s.innerText.replace(/\s+/g, ' ').trim()),
      // the newest entry's foot, against the foot of the sheet's scrolling log
      lastInside: (l => { const e = l.lastElementChild; return !e || e.getBoundingClientRect().bottom <= l.getBoundingClientRect().bottom + 1; })(document.getElementById('log')) });
    const MAX = 3939;
    for (const where of ['browser', 'app']) for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
      const tag = `${where} ${w}`;
      const fp = where === 'app' ? 'A3F0' : 'C41D';
      const explained = `Anyone in range can read it. Signed with your key, ${fp}.`;
      const live = where === 'browser' && w === 1440;
      const bp = await openSheet(where, w, h, { live, stub: { broadcastMax: MAX } });
      try { await bp.evaluate(() => sessionStorage.removeItem('bombechat.simulated-notices')); } catch (e) {}
      const max = await bp.evaluate(() => import('./bridge.js').then(m => m.bridge.machine()).then(m => m.broadcast_max));
      if (max !== MAX) problems.push(`${tag}: the machine says a broadcast holds ${max}, the core ${MAX}`);
      const shot = st => bp.locator('.sheet').screenshot({ path: path.join(out, `broadcast-${where}-${w}-${st}.png`) });
      await bp.evaluate(() => { const l = document.getElementById('log'); l.scrollTop = l.scrollHeight; });
      const idle = await bp.evaluate(line);
      if (idle.public !== 'false' || idle.noteShown || idle.placeholder !== 'Reply to Marisol') problems.push(`${tag}: the reply line did not start as a reply: ${JSON.stringify(idle)}`);
      await shot('idle');
      await bp.click('#mode');
      const on = await bp.evaluate(line);
      if (on.public !== 'true' || !on.noteShown || on.note !== explained || on.placeholder !== 'Public notice') problems.push(`${tag}: the toggle did not make a public notice that says who can read it: ${JSON.stringify(on)}`);
      if (!on.lastInside) problems.push(`${tag}: throwing the toggle hid the foot of the newest entry`);
      // empty: refused, nothing sent, the newest entry still in view
      await bp.focus('#reply'); await bp.keyboard.press('Enter'); await bp.waitForTimeout(150);
      const empty = await bp.evaluate(line);
      if (empty.note !== 'Write the notice first.' || !empty.refused || empty.mine.length || (empty.calls && empty.calls.length)) problems.push(`${tag}: an empty notice was not refused: ${JSON.stringify(empty)}`);
      if (!empty.lastInside) problems.push(`${tag}: a refusal hid the foot of the newest entry`);
      // one byte longer than the machine says a broadcast holds: refused with its length, kept, nothing sent
      const long = 'x'.repeat(MAX + 1);
      await bp.fill('#reply', long); await bp.keyboard.press('Enter'); await bp.waitForTimeout(150);
      const tooLong = await bp.evaluate(line);
      if (tooLong.note !== `Too long to broadcast: ${MAX + 1} bytes, at most ${MAX}.` || tooLong.box !== long || tooLong.mine.length || (tooLong.calls && tooLong.calls.length)) problems.push(`${tag}: an oversize notice was not refused: ${JSON.stringify({ ...tooLong, box: tooLong.box.length })}`);
      if (w === 1440) await shot('refused-long');
      const TEXT = 'Water at the church on Fifth until dusk. Bring containers.';
      await bp.fill('#reply', TEXT);
      const composing = await bp.evaluate(line);
      if (composing.refused || composing.note !== explained) problems.push(`${tag}: writing again did not clear the refusal: ${JSON.stringify(composing)}`);
      // Flash sends a notice by light, never as a private reply; with Light off it is refused, saying why
      await bp.click('#flash', { force: true }); await bp.waitForTimeout(150);
      const flashed = await bp.evaluate(line);
      if (flashed.note !== 'Light is off. Turn it to Read and flash to broadcast by light.' || flashed.box !== TEXT || flashed.replies !== composing.replies || (flashed.calls && flashed.calls.length)) problems.push(`${tag}: Flash did not refuse a notice: ${JSON.stringify(flashed)}`);
      await bp.fill('#reply', TEXT);
      await shot('composing');
      await bp.focus('#reply'); await bp.keyboard.press('Enter'); await bp.waitForTimeout(400);
      const went = await bp.evaluate(line);
      const ent = went.mine[went.mine.length - 1];
      if (!ent || ent.how !== 'public' || ent.text !== TEXT || !new RegExp(`^signed ${fp}, (waiting for quiet|playing)`).test(ent.state)) problems.push(`${tag}: the notice is not on the sheet as a signed public entry: ${JSON.stringify(went.mine)}`);
      if (went.public !== 'false' || went.box !== '' || went.noteShown) problems.push(`${tag}: the toggle did not spring back to Reply once the notice went: ${JSON.stringify(went)}`);
      if (went.calls && JSON.stringify(went.calls) !== JSON.stringify([TEXT])) problems.push(`${tag}: broadcast was asked ${JSON.stringify(went.calls)}`);
      if (!went.slips.some(t => t.startsWith(TEXT) && /public, sends once under 40 dB/.test(t))) problems.push(`${tag}: no public outbox slip for the notice: ${JSON.stringify(went.slips)}`);
      // it goes: the app's node reports it within 3 s, the simulated room falls quiet within 20
      if (where === 'app' || w === 1440) {
        await bp.waitForFunction(t => [...document.querySelectorAll('#log .ent.public.mine')].some(e => e.querySelector('.typed').textContent === t && /sent to anyone in range/.test(e.querySelector('.state').textContent)), TEXT, { timeout: 22000 }).catch(() => {});
        const done = await bp.evaluate(line);
        const last = done.mine[done.mine.length - 1];
        if (!last || last.state !== `signed ${fp}, sent to anyone in range`) problems.push(`${tag}: the notice never said it was sent: ${JSON.stringify(last)}`);
        await bp.waitForTimeout(600);
      }
      await shot('sent');
      // kept: after a reload it is on the sheet again, once, as sent
      if (w === 1440) {
        await bp.reload();
        if (where === 'app') await unlock(bp);
        await settle(bp);
        const again = await bp.evaluate(line);
        const copies = again.mine.filter(m => m.text === TEXT);
        if (copies.length !== 1 || copies[0].state !== `signed ${fp}, sent to anyone in range`) problems.push(`${tag}: after a reload the notice shows ${JSON.stringify(copies)}`);
        await shot('reloaded');
        report.broadcast[`${tag} reloaded`] = copies;
      }
      report.broadcast[tag] = { on: on.note, empty: empty.note, tooLong: tooLong.note, went: ent && ent.state };
      await bp.close();
    }
    // the store's broadcasts replayed on the app path: in time order among the messages, a heard one marked
    // unverified with its hops and not drawn again when its event follows, one of this machine's own still
    // waiting shown waiting with its slip
    {
      const tag = 'app replay';
      const T = 29_000_000;
      const msg = (n, text) => ({ id: Array(16).fill(n), contact: 1, outgoing: false, text, channel: 'sound', at_min: T + n, heard: false, state: 'sent' });
      const heard = { id: Array(16).fill(9), outgoing: false, signer: { sign: Array(32).fill(0x7f), dh: Array(32).fill(1) }, text: 'Road north is open.', verified: false, hops: 1, at_min: T + 5 };
      const own = { id: Array(16).fill(8), outgoing: true, signer: { sign: Array(32).fill(0xa3) }, text: 'Shelter open until dawn.', verified: true, hops: 0, at_min: T + 3, state: 'waiting' };
      const bp = await openSheet('app', 1440, 900, { stub: { talk: { 1: [msg(1, 'First.'), msg(4, 'Fourth.')] }, kept: [own, { ...heard, state: 'sent' }],
        afterUnlock: [['broadcast', heard], ['outbox', { id: own.id, state: 'waiting' }]] } });
      await bp.waitForTimeout(300);
      const replay = await bp.evaluate(() => ({ order: [...document.querySelectorAll('#log .ent')].map(e => [...e.querySelectorAll('.strip,.typed')].map(s => s.textContent).join(' ')),
        notes: [...document.querySelectorAll('#log .ent .state')].map(s => s.textContent), slips: [...document.querySelectorAll('#outbox .slip:not(.empty)')].map(s => s.innerText.replace(/\s+/g, ' ').trim()) }));
      const want = ['First.', 'Shelter open until dawn.', 'Fourth.', 'Road north is open.'];
      // tape is upper-cased by CSS only: compare case-blind
      if (JSON.stringify(replay.order.map(t => t.toLowerCase())) !== JSON.stringify(want.map(t => t.toLowerCase()))) problems.push(`${tag}: the sheet replayed ${JSON.stringify(replay.order)}`);
      if (!replay.notes.includes('signed 7F7F, unverified, one hop')) problems.push(`${tag}: the heard notice is not marked unverified: ${JSON.stringify(replay.notes)}`);
      if (!replay.notes.some(n => /^signed A3F0, waiting for quiet/.test(n)) || !replay.slips.some(t => t.startsWith('Shelter open until dawn.'))) problems.push(`${tag}: this machine's waiting notice is not shown waiting: ${JSON.stringify(replay)}`);
      await bp.locator('.sheet').screenshot({ path: path.join(out, 'broadcast-app-1440-replayed.png') });
      report.broadcast[tag] = replay;
      await bp.close();
    }
    // refused before anything is asked of the machine, with the reason and what was written kept: Sound at
    // Listen, at Off, or not read from the machine; and refused by the machine itself (its reason, the text back)
    const refusals = [['browser', 1, 'Sound is set to listen only. Turn it to Listen and send to broadcast.'], ['app', 1, 'Sound is set to listen only. Turn it to Listen and send to broadcast.'],
      ['app', 0, 'Sound is off. Turn it to Listen and send to broadcast.'], ['app', 'unread', 'The Sound switch was not read from the machine. Turn it to Listen and send to broadcast.'],
      ['app', 2, 'Not sent.', { refuseBroadcast: 'not sent' }]];
    for (const [where, sound, said, stub] of refusals) {
      const tag = `${where} sound ${sound}${stub ? ', refused by the machine' : ''}`;
      const bp = await openSheet(where, 1440, 900, { sound, stub });
      await bp.click('#mode');
      await bp.fill('#reply', 'Road north is open.');
      await bp.keyboard.press('Enter'); await bp.waitForTimeout(400);
      const r = await bp.evaluate(line);
      const asked = stub ? 1 : 0;
      if (r.note !== said || !r.refused || r.box !== 'Road north is open.' || r.public !== 'true' || r.mine.length || (r.calls && r.calls.length !== asked) || !r.lastInside) problems.push(`${tag}: a notice was not refused as it should be: ${JSON.stringify(r)}`);
      if (sound === 1) await bp.locator('.sheet').screenshot({ path: path.join(out, `broadcast-${where}-1440-refused-listen.png`) });
      if (stub) await bp.locator('.sheet').screenshot({ path: path.join(out, 'broadcast-app-1440-refused-by-machine.png') });
      report.broadcast[tag] = r.note;
      await bp.close();
    }

    // testing the machine, in the browser (the simulator) and on the app path (from first run), at 1024, 1440 and 1728,
    // with 2x crops of the sheet: the stamps cut as the key check card's are, in the foot's own row, taking no height
    // from the log; refused at Off and at Listen in sentence case, typed in the foot in place of its fields, nothing
    // asked of the machine; a run waiting for quiet, then the drums spelling it, then its entry under the margin note
    // self-test with what came back, the rate and band and the stamp, never typed as a message to the sheet's contact;
    // nothing of it in the outbox or sent; Escape stopping one; the first run's line going once a test has run. At 1440:
    // one nobody hears saying what to try, a Mac with no sound saying so, a message by light landing while a test
    // arrives, the simulated conversation carrying on after a test, and (in the browser) Test by light on the lamp for a
    // mirror. Every wait is for a state on the page, never a time.
    report.selfTest = {};
    const TESTED = 'The machine hears itself.';
    const EMPTY_LINE = 'On your own? Type something and press Test the machine.';
    const testView = () => { const ents = [...document.querySelectorAll('#log .ent.selftest')], e = ents[ents.length - 1], q = s => e && e.querySelector(s);
      return { entries: ents.length, how: q('.when span') ? q('.when span').textContent : '', typed: !!q('.typed'), mine: !!e && e.classList.contains('mine'),
        tape: e ? [...e.querySelectorAll('.strip')].map(s => s.textContent).join(' ') : '', state: q('.state') ? q('.state').textContent : '', failed: !!e && e.classList.contains('failed'),
        stamp: q('.stamp') ? q('.stamp').textContent : '', note: document.getElementById('testnote').textContent,
        fields: [...document.querySelectorAll('.sh-foot .fld')].map(f => getComputedStyle(f).visibility),
        button: document.getElementById('testbtn').textContent, lightHidden: document.getElementById('testlight').hidden,
        emptyLine: (l => l.hidden ? null : l.textContent)(document.getElementById('emptyline')),
        calls: window.__TAURI__ ? __calls.filter(c => /self_test/.test(c[0])).map(c => c[0] + (c[1] ? ':' + c[1].channel : '')) : null,
        sent: window.__TAURI__ ? __calls.filter(c => c[0] === 'send' || c[0] === 'broadcast').length : 0,
        slips: [...document.querySelectorAll('#outbox .slip:not(.empty)')].map(s => s.textContent),
        logH: document.getElementById('log').clientHeight, footH: document.querySelector('.sh-foot').offsetHeight }; };
    // the stamps as they are cut: the same face, size, rule and height as Pair by sound's, in the sheet's ink
    const cut = () => ['testbtn', 'testlight', 'pairbtn', 'pairlight'].map(id => { const b = document.getElementById(id), c = getComputedStyle(b);
      return { font: c.fontFamily.split(',')[0] + ' ' + c.fontSize, border: c.borderTopWidth + ' ' + c.borderTopStyle, height: b.offsetHeight, opacity: c.opacity, padding: c.padding }; });
    // wait for a state on the page; a problem, not a hang, if it never comes
    const until = async (pg, tag, what, fn, arg) => { const ok = await pg.waitForFunction(fn, arg, { timeout: 30000 }).then(() => true, () => false); if (!ok) problems.push(`${tag}: never came: ${what}`); return ok; };
    const testState = re => { const e = [...document.querySelectorAll('#log .ent.selftest')].pop(); return !!e && new RegExp(re).test((e.querySelector('.state') || {}).textContent || ''); };
    // a stamp come to rest: in place, its stamping finished
    const stamped = () => { const e = [...document.querySelectorAll('#log .ent.selftest')].pop(), s = e && e.querySelector('.stamp'); return !!s && s.getAnimations().every(a => a.playState === 'finished'); };
    const turn = async (pg, tag, id, to) => { await pg.click('#' + id); await until(pg, tag, `${id} at ${to}`, ([k, v]) => document.getElementById(k).getAttribute('aria-valuenow') === v, [id, String(to)]); };
    const testPage = async (where, w, h, { query = '&testpace=250&testhold', stub = {}, still = true } = {}) => {
      const tp = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
      watch(tp, `self-test ${where} ${w}`);
      if (where === 'app') {
        await tp.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', ...stub });
        await tp.goto(url('still'));
        if (stub.people) await unlock(tp);
        else {
          // first run: the registration card, then the sheet
          await tp.waitForSelector('.unlock input[name=who]');
          await tp.fill('.unlock input[name=who]', 'Ana Lima'); await tp.fill('.unlock input[name=pass]', 'correct horse'); await tp.fill('.unlock input[name=again]', 'correct horse');
          await tp.click('.unlock button');
        }
      } else await tp.goto(url((still ? 'still' : '') + query));
      await settle(tp);
      return tp;
    };
    // the run held at waiting for quiet moves on: the simulated room falls quiet, or the stub's machine plays it
    const release = (tp, where) => tp.evaluate(w => w === 'app' ? __selfTestGo() : __sim.quiet(), where);
    // the log's height with the stamps in place, against the same sheet without them: they must take none of it
    const logWithout = tp => tp.evaluate(() => { const r = document.querySelector('.teststamps'), n = document.getElementById('testnote'), h0 = document.getElementById('log').clientHeight;
      r.style.display = 'none'; n.style.display = 'none'; const h1 = document.getElementById('log').clientHeight; r.style.display = ''; n.style.display = ''; return { with: h0, without: h1 }; });
    // the stamp is found from the first moment: at every window size, with the sheet empty (the app, first run) and full
    // (the simulator's conversation), Test the machine is inside the first screen with nothing scrolled, the sheet's head
    // keeps its spacing (12px at least between its parts) and nothing in its foot runs past the sheet. On the empty sheet
    // one typed line in the foot says to press it, in the sheet's own face and ink, set against the index mark, which
    // stands level with the stamp just short of its left edge and points at it, with no key in the way
    report.emptySheet = {};
    for (const [w, h] of VIEWPORTS) for (const where of ['app', 'browser']) {
      const tag = `stamp in view ${where} ${w}x${h}`;
      const tp = await testPage(where, w, h, { query: '' });
      const v = await tp.evaluate(() => {
        const box = e => { const r = e.getBoundingClientRect(); return { l: r.left, r: r.right, t: r.top, b: r.bottom }; };
        const s = box(document.getElementById('testbtn')), l = document.getElementById('emptyline'), log = document.getElementById('log'), sheet = box(document.querySelector('.sheet'));
        const head = [...document.querySelector('.sh-head').children].filter(e => e.getClientRects().length).map(box);
        const foot = [...document.querySelectorAll('.sh-foot *')].filter(e => e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden').map(box);
        const r = { stamp: [s.l, s.t, s.r, s.b].map(Math.round), view: [innerWidth, innerHeight], scrolled: scrollY, entries: log.querySelectorAll('.ent').length, line: l.hidden ? null : l.textContent,
          headGaps: head.slice(1).map((b, i) => Math.round(b.l - head[i].r)), footOut: Math.round(Math.max(0, ...foot.map(b => b.r - sheet.r), ...foot.map(b => sheet.l - b.l))) };
        if (!l.hidden) {
          const f = box(l.parentElement), m = getComputedStyle(l, '::after'), c = getComputedStyle(l);
          const mk = { l: f.l + parseFloat(m.left), t: f.t + parseFloat(m.top) }; mk.r = mk.l + parseFloat(m.borderLeftWidth); mk.b = mk.t + parseFloat(m.borderTopWidth) + parseFloat(m.borderBottomWidth);
          const range = document.createRange(); range.selectNodeContents(l); const text = [...range.getClientRects()];
          const keys = [...document.querySelectorAll('.compose .key')].map(box).filter(k => k.r > mk.l && k.l < mk.r && k.b > mk.t && k.t < mk.b);
          r.mark = { at: [mk.l, mk.t, mk.r, mk.b].map(Math.round), right: parseFloat(m.borderLeftWidth) > 0 && m.borderTopColor === 'rgba(0, 0, 0, 0)' && m.borderBottomColor === 'rgba(0, 0, 0, 0)', ink: m.borderLeftColor, keys: keys.length };
          r.textEnd = Math.round(Math.max(...text.map(t => t.right)));
          r.face = c.fontFamily.split(',')[0] + ' ' + c.color;
          r.inFoot = text.every(t => t.top >= f.t && t.bottom <= f.b && t.left >= sheet.l);
        }
        return r;
      });
      // the index mark placed mid-turn (arriving from the settings page) comes to the same place as at rest once the
      // page has come round
      if (where === 'app' && w === 1512) {
        const mark = await tp.evaluate(async () => {
          const l = document.getElementById('emptyline'), app = document.getElementById('app'), vars = () => ['--line-pad', '--mark-x', '--mark-y'].map(k => l.style.getPropertyValue(k));
          const frames = n => new Promise(r => { const f = () => --n ? requestAnimationFrame(f) : r(); requestAnimationFrame(f); });
          const rest = vars();
          app.style.transition = 'none'; app.style.transform = 'rotateY(55deg)'; dispatchEvent(new Event('resize'));
          const turned = vars();
          app.style.transform = ''; await frames(3); app.style.transition = '';
          return { rest, turned, after: vars() };
        });
        if (JSON.stringify(mark.after) !== JSON.stringify(mark.rest) || Math.max(...mark.turned.map((t, i) => Math.abs(parseFloat(t) - parseFloat(mark.rest[i])))) > 1) problems.push(`${tag}: the empty sheet's mark moves through a turn: ${JSON.stringify(mark)}`);
        report.emptySheet[`${tag} through a turn`] = mark;
      }
      const [x0, y0, x1, y1] = v.stamp;
      if (v.scrolled || x0 < 0 || y0 < 0 || x1 > v.view[0] || y1 > v.view[1]) problems.push(`${tag}: Test the machine is not in the first screen: ${JSON.stringify(v)}`);
      if (v.headGaps.some(g => g < 12)) problems.push(`${tag}: the sheet's head runs together: gaps ${v.headGaps.join(', ')}`);
      if (v.footOut > 0) problems.push(`${tag}: the sheet's foot runs ${v.footOut}px past the sheet`);
      if (where === 'app') {
        const typed = await tp.evaluate(() => { const t = document.createElement('div'); t.className = 'typed'; document.body.append(t); const c = getComputedStyle(t), f = c.fontFamily.split(',')[0] + ' ' + c.color; t.remove(); return f; });
        const ink = await tp.evaluate(() => getComputedStyle(document.querySelector('.sh-head')).color);
        if (v.entries || v.line !== EMPTY_LINE) problems.push(`${tag}: the empty sheet does not say how to test the machine: ${JSON.stringify(v)}`);
        else {
          const [ml, mt, mr, mb] = v.mark.at, mid = (mt + mb) / 2;
          if (v.face !== typed || !v.inFoot) problems.push(`${tag}: the empty sheet's line is not typed as the sheet is, in its foot: ${JSON.stringify(v)} (typed ${typed})`);
          if (!v.mark.right || v.mark.ink !== ink || mr > x0 || x0 - mr > 10 || mid < y0 || mid > y1 || v.mark.keys || v.textEnd > ml) problems.push(`${tag}: the index mark does not point at the stamp: ${JSON.stringify(v)} (ink ${ink})`);
        }
        if (w === 1440 || w === 1024 || w === 1728) await tp.locator('.sheet').screenshot({ path: path.join(out, `empty-${w}.png`) });
      } else if (!v.entries || v.line !== null) problems.push(`${tag}: a sheet with messages on it still says how to test the machine: ${JSON.stringify(v)}`);
      report.emptySheet[tag] = { stamp: v.stamp, line: !!v.line, mark: v.mark && v.mark.at, headGaps: v.headGaps };
      await tp.close();
    }
    for (const where of ['browser', 'app']) for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
      const tag = `self-test ${where} ${w}`;
      const tp = await testPage(where, w, h);
      const shot = st => tp.locator('.sheet').screenshot({ path: path.join(out, `selftest-${where}-${w}-${st}.png`) });
      const rest = await tp.evaluate(testView);
      if (rest.button !== 'Test the machine' || rest.lightHidden || rest.entries || rest.note) problems.push(`${tag}: the stamps are not at rest: ${JSON.stringify(rest)}`);
      // an empty sheet (the app's, from first run) suggests it: one typed line; the simulator's sheet has its conversation
      const wantEmpty = where === 'app' ? EMPTY_LINE : null;
      if (rest.emptyLine !== wantEmpty) problems.push(`${tag}: the empty sheet's line reads ${JSON.stringify(rest.emptyLine)}`);
      const stamps = await tp.evaluate(cut);
      if (stamps.some(s => JSON.stringify(s) !== JSON.stringify(stamps[2]))) problems.push(`${tag}: the test stamps are not cut as Pair by sound's: ${JSON.stringify(stamps)}`);
      const placed = await tp.evaluate(() => { const s = document.querySelector('.sheet').getBoundingClientRect(), f = document.querySelector('.sh-foot').getBoundingClientRect();
        return ['testbtn', 'testlight'].every(id => { const r = document.getElementById(id).getBoundingClientRect(); return r.left >= s.left && r.right <= s.right && r.bottom <= s.bottom && r.top >= f.top - 4; }); });
      if (!placed) problems.push(`${tag}: a test stamp is not in the foot of the sheet`);
      const room = await logWithout(tp);
      if (room.with < room.without) problems.push(`${tag}: the stamps take ${room.without - room.with}px from the log`);
      await tp.locator('.sh-foot').screenshot({ path: path.join(out, `selftest-${where}-${w}-stamps-foot.png`) });
      await tp.locator('.pairact').screenshot({ path: path.join(out, `selftest-${where}-${w}-stamps-card.png`) });
      await shot('rest');
      // refused at Off, then at Listen: the switch named, in sentence case, typed in place of the foot's fields, nothing
      // asked of the machine and no height taken; a switch turned takes it away
      for (const [at, pos] of [['Off', 0], ['Listen', 1]]) {
        await turn(tp, tag, 'k1', pos);
        await tp.click('#testbtn');
        await until(tp, tag, `the refusal at ${at}`, () => !!document.getElementById('testnote').textContent);
        const r = await tp.evaluate(testView);
        if (r.note !== 'Turn Sound to Listen and send to test the machine.' || r.fields.some(v => v !== 'hidden') || r.entries || (r.calls && r.calls.length) || r.logH !== rest.logH || r.footH !== rest.footH) problems.push(`${tag}: at ${at} the test was not refused in place: ${JSON.stringify(r)}`);
        if (at === 'Listen') await shot('refused');
      }
      await turn(tp, tag, 'k1', 2);
      const cleared = await tp.evaluate(testView);
      // the foot's fields come back, unless the empty sheet's line stands in their place
      if (cleared.note || cleared.fields.some(v => v !== (cleared.emptyLine ? 'hidden' : 'visible'))) problems.push(`${tag}: turning Sound did not clear the refusal: ${JSON.stringify(cleared)}`);
      // a run: held waiting for quiet, then the drums spelling it, then on the sheet as heard
      await tp.click('#testbtn');
      await until(tp, tag, 'the test waiting for quiet', testState, '^sealed to this machine, waiting for quiet');
      // its entry fed onto the sheet and come to rest
      await until(tp, tag, 'the test entry at rest', () => [...document.querySelectorAll('#log .ent.selftest')].pop().getAnimations().every(a => a.playState === 'finished'));
      const waiting = await tp.evaluate(testView);
      if (waiting.how !== 'self-test' || waiting.typed || waiting.mine || waiting.button !== 'Stop the test' || !waiting.lightHidden || waiting.note) problems.push(`${tag}: the test did not start as this machine's own, waiting for quiet: ${JSON.stringify(waiting)}`);
      await shot('waiting');
      await release(tp, where);
      // the plain row spells the test's text (its last twelve characters, it being longer), never the sender's keys
      const spelled = await until(tp, tag, 'the drums spelling the test', want => __bank.rows().plain === want, plainRowOf(TESTED));
      await until(tp, tag, 'the test ending', () => { const e = [...document.querySelectorAll('#log .ent.selftest')].pop(); return !!e && (!!e.querySelector('.stamp') || e.classList.contains('failed')); });
      await until(tp, tag, 'the stamp coming to rest', stamped);
      const heard = await tp.evaluate(testView);
      if (!spelled || heard.tape.toLowerCase() !== TESTED.toLowerCase() || heard.failed || heard.typed || heard.mine || !/^Heard back after \d+ s: \d+\.\d bytes a second on the near silent band\.$/.test(heard.state)
        || !/^heard by this machine, \d\d\.\d\d$/.test(heard.stamp) || heard.button !== 'Test the machine' || heard.lightHidden) problems.push(`${tag}: the test did not land on the sheet as heard: ${JSON.stringify(heard)}`);
      if (heard.slips.some(s => s.includes(TESTED)) || heard.sent || (heard.calls && JSON.stringify(heard.calls) !== '["self_test:sound"]')) problems.push(`${tag}: the test went further than itself: ${JSON.stringify(heard)}`);
      if (heard.emptyLine !== null) problems.push(`${tag}: the empty sheet's line stayed after a test landed: ${JSON.stringify(heard.emptyLine)}`);
      await shot('heard');
      // Escape stops one
      await tp.click('#testbtn');
      await until(tp, tag, 'a second test started', () => document.querySelectorAll('#log .ent.selftest').length === 2 && document.getElementById('testbtn').textContent === 'Stop the test');
      await tp.keyboard.press('Escape');
      await until(tp, tag, 'Escape stopping the test', testState, '^Test stopped\\.$');
      const stopped = await tp.evaluate(testView);
      if (stopped.failed || stopped.button !== 'Test the machine' || stopped.entries !== 2 || (stopped.calls && !stopped.calls.includes('stop_self_test'))) problems.push(`${tag}: Escape did not stop the test: ${JSON.stringify(stopped)}`);
      await shot('stopped');
      report.selfTest[tag] = { emptyLine: rest.emptyLine, waiting: waiting.state, heard: heard.state, stamp: heard.stamp, stopped: stopped.state, log: room };
      await tp.close();
      if (w !== 1440) continue;
      // nobody hears it: given up, saying what to try
      {
        const dp = await testPage(where, w, h, { query: '&testpace=250&selftest=deaf&testcap=1500', stub: { selfTest: 'unheard' } });
        await dp.click('#testbtn');
        if (where === 'app') { await until(dp, tag, 'the deaf test waiting', testState, 'waiting'); await release(dp, where); }
        await until(dp, tag, 'the deaf test given up', () => { const e = [...document.querySelectorAll('#log .ent.selftest')].pop(); return !!e && e.classList.contains('failed'); });
        const deaf = await dp.evaluate(testView);
        if (deaf.state !== 'Not heard. Raise the volume, or set Band to Audible in Settings, and test again.' || deaf.tape) problems.push(`${tag}: a test nobody heard reads ${JSON.stringify(deaf)}`);
        await dp.locator('.sheet').screenshot({ path: path.join(out, `selftest-${where}-${w}-unheard.png`) });
        report.selfTest[`${tag} unheard`] = deaf.state;
        await dp.close();
      }
      // in its own words: typed in the reply line they go out as the test (the machine asked with them), typed in its
      // entry, spelled on the drums and read back on its tape, and the line clears as a send clears it. Over 100 bytes
      // it is refused in place with the words kept; long enough to take a while, its line says about how long on air
      {
        const op = await testPage(where, w, h), WORDS = 'hello world';
        await op.fill('#reply', WORDS);
        await op.click('#testbtn');
        await until(op, tag, 'the own-words test waiting', testState, '^sealed to this machine, waiting for quiet');
        const begun = await op.evaluate(() => ({ reply: document.getElementById('reply').value, typed: ([...document.querySelectorAll('#log .ent.selftest')].pop().querySelector('.typed') || {}).textContent,
          asked: window.__TAURI__ ? __calls.filter(c => c[0] === 'self_test').map(c => c[1].text) : null }));
        if (begun.reply || begun.typed !== WORDS || (begun.asked && JSON.stringify(begun.asked) !== JSON.stringify([WORDS]))) problems.push(`${tag}: own words did not go out as the test: ${JSON.stringify(begun)}`);
        await release(op, where);
        // the words whole on the plain row, laid from drum 1, and still there once the test is heard and stamped
        const spelledOwn = await until(op, tag, 'the drums spelling the own words', want => __bank.rows().plain === want, plainRowOf(WORDS));
        await until(op, tag, 'the own-words test heard', stamped);
        // to the last frame on the drums (the signature's), with the row at rest
        await until(op, tag, 'the own-words test on its last frame', () => { const f = __bank.frame(); return !!f && document.getElementById('fno').textContent === document.getElementById('fnt').textContent && __bank.rows().plain === f.plain; });
        const ownRow = await op.evaluate(() => __bank.rows().plain);
        if (ownRow !== 'HELLOXWORLDX') problems.push(`${tag}: own words: the plain row reads ${ownRow} once heard, not HELLOXWORLDX`);
        const own = await op.evaluate(testView);
        if (!spelledOwn || own.tape.toLowerCase() !== WORDS || !/^Heard back/.test(own.state) || own.failed) problems.push(`${tag}: own words did not come back on the sheet: ${JSON.stringify(own)}`);
        await op.locator('.sheet').screenshot({ path: path.join(out, `selftest-${where}-${w}-own-words.png`) });
        const LONG = 'a'.repeat(101);
        await op.fill('#reply', LONG);
        await op.click('#testbtn');
        await until(op, tag, 'the too-long refusal', () => /^Too long to test with: 101 bytes, at most 100\./.test(document.getElementById('testnote').textContent));
        const refused = await op.evaluate(() => ({ reply: document.getElementById('reply').value, tests: document.querySelectorAll('#log .ent.selftest').length, button: document.getElementById('testbtn').textContent }));
        if (refused.reply !== LONG || refused.tests !== 1 || refused.button !== 'Test the machine') problems.push(`${tag}: too long a test was not refused in place: ${JSON.stringify(refused)}`);
        await op.fill('#reply', 'b'.repeat(90));
        await op.click('#testbtn');
        const air = await until(op, tag, 'the long test saying its time on air', testState, '^sealed to this machine, about \\d+ s on air, (waiting for quiet|playing next)');
        if (!air) problems.push(`${tag}: a long test does not say its time on air`);
        await op.keyboard.press('Escape');
        // trimmed as the node trims it: 100 bytes inside a byte order mark and a next line are not too long
        await until(op, tag, 'the long test stopped', () => document.getElementById('testbtn').textContent === 'Test the machine');
        await op.fill('#reply', '\ufeff' + 'c'.repeat(100) + '\u0085');
        await op.click('#testbtn');
        if (!await until(op, tag, 'a 100-byte test inside marks starting', testState, '^sealed to this machine')) problems.push(`${tag}: a line trimmed to 100 bytes was refused: ${await op.evaluate(() => document.getElementById('testnote').textContent)}`);
        await op.keyboard.press('Escape');
        report.selfTest[`${tag} own words`] = { begun, heard: own.state, tape: own.tape };
        await op.close();
      }
      if (where === 'app') {
        // a Mac with no sound to test with says so, rather than asking for a switch already turned
        const np = await testPage('app', w, h, { stub: { noSound: true } });
        await np.click('#testbtn');
        await until(np, tag, 'the no-sound refusal', () => !!document.getElementById('testnote').textContent);
        const none = await np.evaluate(testView);
        if (none.note !== 'This Mac has no sound to test with.' || none.entries) problems.push(`${tag}: a Mac with no sound was told ${JSON.stringify(none.note)}`);
        await np.locator('.sheet').screenshot({ path: path.join(out, 'selftest-app-1440-no-sound.png') });
        await np.close();
        // a message by light lands while the test is arriving by sound: each entry ends as its own
        const ip = await testPage('app', w, h, { stub: { people: [{ id: 1, name: 'Marisol Ortega', checked: true }] } });
        await ip.click('#testbtn');
        await until(ip, tag, 'the test waiting', testState, 'waiting');
        await ip.evaluate(() => { __selfTestStep(); __selfTestStep(); });
        await until(ip, tag, 'the test arriving', testState, '^arriving by sound');
        const LIT = 'Road north is open.';
        await ip.evaluate(t => { const plain = [...Array(68).fill(0x41), ...new TextEncoder().encode(t), ...Array(64).fill(0x42)];
          __emit('chunk', { tag: 0x4444, index: 0, of: 1, channel: 'Light', view: { plain, heard: plain, key: plain.map(() => 0) } });
          __emit('message', { id: Array(16).fill(0x44), contact: 1, outgoing: false, text: t, channel: 'Light', at_min: Math.floor(Date.now() / 60000), heard: false, state: 'sent' });
          __selfTestGo(); }, LIT);
        await until(ip, tag, 'the test heard after a message by light', stamped);
        await until(ip, tag, 'the message by light on the sheet', t => [...document.querySelectorAll('#log .ent:not(.selftest)')].some(e => !e.classList.contains('arriving') && e.querySelector('.strip') && e.querySelector('.strip').textContent === t), LIT);
        const both = await ip.evaluate(t => ({ test: (() => { const e = [...document.querySelectorAll('#log .ent.selftest')].pop(); return { state: e.querySelector('.state').textContent, failed: e.classList.contains('failed') }; })(),
          message: [...document.querySelectorAll('#log .ent:not(.selftest)')].filter(e => [...e.querySelectorAll('.strip')].some(s => s.textContent === t)).map(e => ({ how: e.querySelector('.when span').textContent, failed: e.classList.contains('failed') })) }), LIT);
        if (both.test.failed || !/^Heard back/.test(both.test.state) || both.message.length !== 1 || both.message[0].failed || both.message[0].how !== 'light') problems.push(`${tag}: a message by light during a test: ${JSON.stringify(both)}`);
        await ip.locator('.sheet').screenshot({ path: path.join(out, 'selftest-app-1440-with-a-message.png') });
        report.selfTest[`${tag} with a message`] = both;
        await ip.close();
      } else {
        // the simulated conversation holds still for a test and carries on after it: Marisol's message, part way in
        // when the test starts, lands after it (its next chunk held off at first, then due 4.2 s after the test)
        const NC = 5;
        const cp = await testPage('browser', w, h, { query: `f=${NC - 2}&next=600000&testpace=250&testhold`, still: false });
        await cp.click('#testbtn');
        await until(cp, tag, 'the test waiting', testState, 'waiting');
        await release(cp, where);
        await until(cp, tag, 'the test heard', stamped);
        await until(cp, tag, "Marisol's message landing after the test", () => [...document.querySelectorAll('#log .ent:not(.selftest)')].some(e => /we leave at dusk/i.test(e.textContent) && !e.classList.contains('arriving') && !e.classList.contains('failed') && !e.querySelector('.strip.provisional')));
        const after = await cp.evaluate(() => [...document.querySelectorAll('#log .ent')].slice(-2).map(e => e.innerText.replace(/\s+/g, ' ').trim()));
        report.selfTest[`${tag} conversation after`] = after;
        await cp.close();
      }
    }
    // Test by light, in the browser at 1440: refused at Off; at Read and flash it goes on the lamp for a mirror, Escape
    // ends the test and leaves Light where it is, and one read back lands on the sheet as seen
    {
      const tag = 'self-test by light';
      // read back 2.4 s after it first shows, as the simulator has it
      const lp = await testPage('browser', 1440, 900, { query: '' });
      const shot = (st, sel = '.sheet') => lp.locator(sel).screenshot({ path: path.join(out, `selftest-browser-1440-light-${st}.png`) });
      await lp.click('#testlight');
      await until(lp, tag, 'the refusal at Off', () => !!document.getElementById('testnote').textContent);
      const off = await lp.evaluate(testView);
      if (off.note !== 'Turn Light to Read and flash to test by light.' || off.entries) problems.push(`${tag}: at Off it was not refused: ${JSON.stringify(off)}`);
      await shot('refused');
      await turn(lp, tag, 'k2', 1); await turn(lp, tag, 'k2', 2);
      const lamp = () => ({ open: !document.getElementById('lamp').hidden, plate: document.querySelector('.lamp-head .pl').textContent, say: document.getElementById('lampsay').innerText.replace(/\s+/g, ' ').trim(),
        light: document.getElementById('k2').getAttribute('aria-valuenow'), button: document.getElementById('testlight').textContent });
      await lp.click('#testlight');
      await until(lp, tag, 'the lamp open on the test', () => !document.getElementById('lamp').hidden);
      const cutT = await lp.evaluate(lampCuts); if (cutT.length) problems.push(`${tag}: testing by light cuts drums: ${cutT.join('; ')}`);
      const on = await lp.evaluate(lamp);
      if (on.plate !== 'Testing by light' || !/^Hold a mirror up to this screen\./.test(on.say) || on.button !== 'Stop the test') problems.push(`${tag}: the lamp does not show the test for a mirror: ${JSON.stringify(on)}`);
      await shot('lamp', '.lamp');
      await lp.keyboard.press('Escape');
      await until(lp, tag, 'Escape ending the test by light', () => document.getElementById('lamp').hidden && /^Test stopped\.$/.test(([...document.querySelectorAll('#log .ent.selftest')].pop().querySelector('.state') || {}).textContent));
      const esc = { ...(await lp.evaluate(lamp)), ...(await lp.evaluate(testView)) };
      if (esc.light !== '2') problems.push(`${tag}: Escape turned the Light switch: ${JSON.stringify(esc)}`);
      await lp.click('#testlight');
      await until(lp, tag, 'the test read back', stamped);
      const seen = { ...(await lp.evaluate(lamp)), ...(await lp.evaluate(testView)) };
      if (seen.open || !/^Read back in the mirror after \d+ s\.$/.test(seen.state) || !/^seen by this machine, \d\d\.\d\d$/.test(seen.stamp) || seen.tape.toLowerCase() !== TESTED.toLowerCase()) problems.push(`${tag}: a test read back did not land as seen: ${JSON.stringify(seen)}`);
      await shot('seen');
      // in its own words, the same way: the line's words go on the lamp, the line clears, and they are read back
      await lp.fill('#reply', 'hello mirror');
      await lp.click('#testlight');
      await until(lp, tag, 'the own-words test on the lamp', () => !document.getElementById('lamp').hidden);
      const ownOn = await lp.evaluate(() => ({ reply: document.getElementById('reply').value, typed: ([...document.querySelectorAll('#log .ent.selftest')].pop().querySelector('.typed') || {}).textContent }));
      await until(lp, tag, 'the own-words test read back', stamped);
      const ownSeen = await lp.evaluate(testView);
      if (ownOn.reply || ownOn.typed !== 'hello mirror' || ownSeen.tape.toLowerCase() !== 'hello mirror' || !/^Read back in the mirror/.test(ownSeen.state)) problems.push(`${tag}: own words by light: ${JSON.stringify({ ownOn, ownSeen })}`);
      await shot('own-words');
      report.selfTest[tag] = { refused: off.note, lamp: on.say, seen: seen.state, own: ownSeen.tape };
      await lp.close();
    }

    // Light: the flashing view, the Light switch's eyepiece and note, and Flash, on the app path (window.__TAURI__
    // stubbed, the node's events emitted as it emits them) at 1024, 1440 and 1728, with 2x crops (light-<w>-<state>.png):
    // idle; ready (the still field before the first frame); flashing, with the frames shown and the rate the machine
    // reports; Escape stopping it at once by turning the Light switch to Read, and frames after that not shown; reading,
    // with what the camera sees in the eyepiece; the camera's hint typed by the eyepiece; Flash refused at Read in
    // sentence case. The frames are the core's own (crates/light/examples/page_frames.rs), and a screenshot of the
    // flashing view read back with the camera's decoder gives exactly the frame's bytes, at every width and preset.
    // Then the photosensitivity check: 120 consecutive frames of the view, from the still field to the sheet coming
    // back, measured over sliding windows a sixteenth of the view each, against WCAG 2.3.1's general flash threshold.
    {
      const { execFileSync } = require('child_process');
      const example = args => execFileSync('cargo', ['run', '-q', '-p', 'bombechat-light', '--example', 'page_frames', '--', ...args], { cwd: ROOTDIR, encoding: 'utf8', maxBuffer: 1 << 28 });
      const CODE = [0, 1, 2].map(preset => JSON.parse(example(['frames', String(preset), '120', '4660'])));
      const readBack = files => example(['read', ...files]).trim().split('\n').map(l => JSON.parse(l));
      const TILT = 'seeing a code: tilt or dim the screen';
      const openLight = async (w, h, light, { dpr = 2, q = 'still&ready=400', stub = {} } = {}) => {
        const lp = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: dpr });
        watch(lp, `light ${w}`);
        await lp.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }],
          settings: { sound: 2, volume: 2, band: 0, light, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 }, ...stub });
        await lp.goto(url(q));
        await lp.waitForSelector('.unlock input[name=pass]');
        await lp.fill('.unlock input[name=pass]', 'correct horse');
        await lp.click('.unlock button');
        await lp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
        await lp.evaluate(() => document.fonts.ready);
        await lp.waitForTimeout(200);
        return lp;
      };
      const emit = (lp, name, payload) => lp.evaluate(([n, p]) => window.__emit(n, p), [name, payload]);
      const frame = (spec, i, tag = 4660) => ({ tag, bits: CODE[spec].frames[i].bits, spec, index: i });
      // a frame of this machine's pairing hello, as the node marks it
      const helloFrame = (spec, i, tag) => ({ ...frame(spec, i, tag), hello: true });
      const paint = lp => lp.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
      const view = () => { const l = document.getElementById('lamp'), c = document.getElementById('lampcode'), k = c.getContext('2d');
        const mid = k.getImageData(c.width >> 1, c.height >> 1, 1, 1).data[0];
        return { shown: !l.hidden, sheet: getComputedStyle(document.querySelector('.sheet')).visibility, say: document.getElementById('lampsay').innerText.replace(/\s+/g, ' ').trim(),
          frames: document.getElementById('lampn').textContent, rate: document.getElementById('lamprate').textContent, mid,
          light: document.getElementById('k2').getAttribute('aria-valuenow'), said: document.getElementById('said').textContent, title: document.getElementById('said').title,
          saidClip: { scroll: document.getElementById('said').scrollWidth, client: document.getElementById('said').clientWidth },
          sets: __calls.filter(c => c[0] === 'set_setting').map(c => c[1]), eyepiece: __calls.filter(c => c[0] === 'eyepiece').map(c => c[1].on),
          code: { css: parseFloat(c.style.width), px: c.width, vh: innerHeight, dpr: devicePixelRatio, cell: Math.floor(c.width / ([48, 56, 64][1] + 6)) },
          note: document.querySelector('#lightnote b').textContent, pubnote: document.getElementById('pubnote').textContent,
          sends: __calls.filter(c => c[0] === 'send' || c[0] === 'broadcast').length };
      };
      report.light = {};
      // the lamp over the drum bay at every window size, in each of its three uses: no drum partly under it
      report.lampCuts = {};
      for (const [w, h] of VIEWPORTS) {
        const tag = `lamp over the bay ${w}x${h}`, seen = {};
        // flashing a message, on the app path
        const fp = await openLight(w, h, 2, { dpr: 1 });
        await emit(fp, 'display', frame(1, 0)); await paint(fp); seen.flashing = await fp.evaluate(lampCuts);
        await fp.close();
        // testing and pairing by light, in the browser, each started from where its stamp is, scrolled to it
        const lp = await browser.newPage({ viewport: { width: w, height: h } });
        watch(lp, tag);
        await lp.goto(url('still'));
        await lp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
        await lp.focus('#k2'); await lp.keyboard.press('ArrowRight'); await lp.keyboard.press('ArrowRight');
        for (const [use, stamp] of [['testing', '#testlight'], ['pairing', '#pairlight']]) {
          await lp.evaluate(s => document.querySelector(s).scrollIntoView({ block: 'center' }), stamp);
          const from = await lp.evaluate(() => scrollY);
          await lp.click(stamp);
          await lp.waitForFunction(() => !document.getElementById('lamp').hidden, null, { timeout: 8000 }).catch(() => {});
          await paint(lp);
          seen[use] = await lp.evaluate(lampCuts); seen[use + 'Scroll'] = await lp.evaluate(() => scrollY);
          await lp.keyboard.press('Escape');
          await lp.waitForFunction(() => document.getElementById('lamp').hidden, null, { timeout: 8000 }).catch(() => {});
          await paint(lp);
          seen[use + 'Back'] = await lp.evaluate(f => ({ lamp: !document.getElementById('lamp').hidden, scroll: Math.round(scrollY), from: Math.round(f), overflow: getComputedStyle(document.body).overflowY }), from);
        }
        report.lampCuts[`${w}x${h}`] = seen;
        for (const k of ['flashing', 'testing', 'pairing']) if (seen[k].length) problems.push(`${tag}: ${k} by light, ${seen[k].join('; ')}`);
        if (seen.testingScroll || seen.pairingScroll) problems.push(`${tag}: the page was not held at its top under the lamp: ${JSON.stringify(seen)}`);
        for (const k of ['testingBack', 'pairingBack']) { const b = seen[k]; if (b.lamp || b.overflow === 'hidden' || Math.abs(b.scroll - b.from) > 1) problems.push(`${tag}: the page did not come back from the lamp where it was: ${JSON.stringify(b)}`); }
        await lp.close();
      }
      for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
        const tag = `light ${w}`, r = report.light[w] = {};
        const lp = await openLight(w, h, 2);
        const shot = async (st, sel) => lp.locator(sel).screenshot({ path: path.join(out, `light-${w}-${st}.png`) });
        await shot('idle', '.deck');
        const idle = await lp.evaluate(view);
        if (idle.shown || !idle.eyepiece.length || idle.eyepiece[idle.eyepiece.length - 1] !== true) problems.push(`${tag}: idle at Read and flash: ${JSON.stringify(idle)}`);
        // the first frame opens the lamp still: the plain grey field, no frame yet
        await emit(lp, 'display', frame(1, 0)); await paint(lp);
        const ready = await lp.evaluate(view);
        if (!ready.shown || ready.sheet !== 'hidden' || ready.mid !== 0xbc || ready.say !== 'Hold this screen up to their camera. ready, Escape stops it' || ready.frames !== '000') problems.push(`${tag}: the lamp did not open still: ${JSON.stringify(ready)}`);
        r.small = await lp.evaluate(`(${measureText.toString()})()`);
        if (r.small.length) problems.push(`${tag}: text under 12px in the flashing view: ${r.small.join('; ')}`);
        await shot('ready', '.lamp');
        const cutF = await lp.evaluate(lampCuts); if (cutF.length) problems.push(`${tag}: the flashing view cuts drums: ${cutF.join('; ')}`);
        await lp.waitForTimeout(450);
        for (let i = 1; i <= 5; i++) { await emit(lp, 'display', frame(1, i)); await paint(lp); }
        const flashing = await lp.evaluate(view);
        if (flashing.frames !== '006' || flashing.rate !== '2290' || flashing.say !== 'Hold this screen up to their camera. flashing, Escape stops it') problems.push(`${tag}: the flashing view reads ${JSON.stringify(flashing)}`);
        await shot('flashing', '.lamp');
        // a code legible at arm's length: at least 60% of the window's height, and 12 cm on a 13-inch 1440x900 screen
        // (about 50 points a centimetre), in whole device pixels
        r.code = flashing.code;
        if (flashing.code.css < 0.6 * h || (w === 1440 && flashing.code.css < 604) || flashing.code.px !== Math.round(flashing.code.css * 2)) problems.push(`${tag}: the code is ${JSON.stringify(flashing.code)}, wanted at least ${Math.ceil(0.6 * h)} px${w === 1440 ? ' and 604 px (12 cm)' : ''} in whole device pixels`);
        // what is drawn is what the core means: the camera's decoder reads the frame's bytes back off a screenshot, for
        // each preset (every preset, at every width)
        const files = [], want = [];
        for (const spec of [0, 1, 2]) {
          await emit(lp, 'display', frame(spec, 7)); await paint(lp);
          const f = path.join(out, `light-${w}-decode-${spec}.png`);
          await lp.locator('.lamp').screenshot({ path: f }); files.push(f); want.push(CODE[spec].frames[7].frame);
        }
        const got = readBack(files);
        r.decoded = got.map((g, i) => Array.isArray(g) && g.includes(want[i]));
        got.forEach((g, i) => { if (!r.decoded[i]) problems.push(`${tag}: the decoder read ${JSON.stringify(g).slice(0, 80)} off ${path.basename(files[i])}, not the frame drawn`); });
        // Escape: gone at once, the Light switch turned to Read, and nothing more shown
        await lp.keyboard.press('Escape'); await paint(lp);
        const gone = await lp.evaluate(() => document.getElementById('lamp').hidden);
        await emit(lp, 'display', frame(1, 8)); await lp.waitForTimeout(400);
        const stopped = await lp.evaluate(view);
        if (!gone || stopped.shown || stopped.sheet !== 'visible' || stopped.light !== '1' || JSON.stringify(stopped.sets) !== JSON.stringify([{ id: 'light', valueIndex: 1 }]) || stopped.said !== 'Flashing stopped. Light is at Read, and the message waits.')
          problems.push(`${tag}: Escape did not stop flashing: ${JSON.stringify(stopped)}`);
        await emit(lp, 'displayStopped', { tag: 4660 }); await paint(lp);
        // reading: what the camera sees, small and grey, in the eyepiece
        const pic = Array.from({ length: 64 * 48 }, (_, i) => { const x = i % 64, y = (i / 64) | 0; return Math.min(255, 50 + y * 2 + (x > 40 && y < 22 ? 130 : 0) + (Math.hypot(x - 22, y - 26) < 11 ? -40 : 0)); });
        await emit(lp, 'eyepiece', { width: 64, height: 48, pixels: pic }); await paint(lp);
        const reading = await lp.evaluate(() => { const c = document.querySelector('#eyepiece canvas'), d = c.getContext('2d').getImageData(50, 10, 1, 1).data; return { px: d[0], dark: document.getElementById('eyepiece').classList.contains('dark') }; });
        if (reading.dark || reading.px !== pic[10 * 64 + 50]) problems.push(`${tag}: the eyepiece does not show what the camera sees: ${JSON.stringify(reading)}`);
        await shot('reading', '.mod:has(#k2)');
        // the camera's hint, typed by the eyepiece, then gone
        await emit(lp, 'channel', { channel: 'light', state: TILT }); await paint(lp);
        const hint = await lp.evaluate(view);
        if (hint.note !== 'Tilt or dim the screen.') problems.push(`${tag}: the hint reads ${JSON.stringify(hint.note)}`);
        // the tape covers nothing: not the switch, its engravings, plates or jewels, the eyepiece, nor the meter and slips
        const covers = await lp.evaluate(() => { const n = document.querySelector('#lightnote b').getBoundingClientRect();
          const hit = r => r.width && n.left < r.right && n.right > r.left && n.top < r.bottom && n.bottom > r.top;
          return [...document.querySelectorAll('.mod .knob, .mod .pl, .mod .jewel, .mod .blank, #eyepiece, .vu, .outbox .slip, .row3 > *')].filter(e => hit(e.getBoundingClientRect())).map(e => e.id || e.className); });
        if (covers.length) problems.push(`${tag}: the hint tape covers ${covers.join(', ')}`);
        await lp.screenshot({ path: path.join(out, `light-${w}-hint.png`), clip: await lp.evaluate(() => { const a = document.querySelector('.mods').getBoundingClientRect(), b = document.querySelector('.row2').getBoundingClientRect(); return { x: a.left - 4, y: a.top - 4, width: a.width + 8, height: b.bottom - a.top + 8 }; }) });
        await emit(lp, 'channel', { channel: 'light', state: 'listening' });
        if ((await lp.evaluate(view)).note) problems.push(`${tag}: the hint stayed after the camera read on`);
        // at Read, Flash is refused for a notice and for a reply, in sentence case, and nothing is asked of the machine
        await lp.click('#mode'); await lp.fill('#reply', 'Road north is open.'); await lp.click('#flash', { force: true }); await paint(lp);
        const refusedNotice = await lp.evaluate(view);
        await shot('refusal', '.sheet');
        await lp.click('#mode'); await lp.fill('#reply', 'On my way.'); await lp.click('#flash', { force: true }); await paint(lp);
        const refusedReply = await lp.evaluate(view);
        if (refusedNotice.pubnote !== 'Light is set to read only. Turn it to Read and flash to broadcast by light.' || refusedReply.pubnote !== 'Light is set to read only. Turn it to Read and flash to send by light.' || refusedReply.sends)
          problems.push(`${tag}: Flash at Read was not refused as it should be: ${JSON.stringify([refusedNotice.pubnote, refusedReply.pubnote, refusedReply.sends])}`);
        {
          // Escape when the setting can't be saved: the machine stops flashing all the same (Light is at Read for this
          // session), so the lamp goes, the switch is at Read, and the page says the setting wasn't kept
          const rp = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
          watch(rp, `${tag} escape refused`);
          await rp.addInitScript(tauriStub, { hello: 'Marisol Ortega', safety: '', people: [{ id: 1, name: 'Marisol Ortega', checked: true, safety: '' }], refuseSetting: 'the setting was not saved: disk full',
            settings: { sound: 2, volume: 2, band: 0, light: 2, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 } });
          await rp.goto(url('still&ready=0'));
          await rp.waitForSelector('.unlock input[name=pass]'); await rp.fill('.unlock input[name=pass]', 'correct horse'); await rp.click('.unlock button');
          await rp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
          await emit(rp, 'display', frame(1, 0)); await paint(rp); await rp.waitForTimeout(50);
          // the knob is redrawn when its turn has played (330 ms): wait for it rather than a guess at the time
          await rp.keyboard.press('Escape'); await rp.waitForFunction(() => document.getElementById('k2').getAttribute('aria-valuenow') === '1', null, { timeout: 5000 }).catch(() => {});
          const held = await rp.evaluate(view);
          await emit(rp, 'display', frame(1, 1)); await paint(rp);
          const after = await rp.evaluate(view);
          if (held.shown || after.light !== '1' || held.said !== 'Flashing stopped, but not saved.' || after.shown
            || held.title !== 'Flashing stopped. Light is at Read for now. The setting was not saved: disk full. So Light goes back to Read and flash next time.')
            problems.push(`${tag}: Escape when the setting can't be saved: ${JSON.stringify({ held, after })}`);
          // the top bar never cuts its sentence short
          if (held.saidClip.scroll > held.saidClip.client) problems.push(`${tag}: the top bar clips the failed-save sentence: ${JSON.stringify(held.saidClip)}`);
          await rp.locator('.top').screenshot({ path: path.join(out, `light-${w}-stop-unsaved.png`) });
          // turning the Light switch down by hand when it can't be saved: the same short sentence, the full one in its title
          await rp.focus('#k2'); await rp.keyboard.press('Home'); await rp.waitForFunction(() => document.getElementById('k2').getAttribute('aria-valuenow') === '0', null, { timeout: 5000 }).catch(() => {});
          const knob = await rp.evaluate(view);
          if (knob.light !== '0' || knob.said !== 'Light at Off, not saved.' || knob.saidClip.scroll > knob.saidClip.client
            || knob.title !== 'Light is at Off for now. The setting was not saved: disk full. So it goes back to Read next time.')
            problems.push(`${tag}: turning Light down when it can't be saved: ${JSON.stringify(knob)}`);
          r.escapeUnsaved = { said: held.said, clip: held.saidClip };
          await rp.close();
        }
        // the eyepiece's shutter: shut, the machine is asked for no picture; open again, it is
        if (w === 1440) {
          await lp.click('#eyepiece'); await paint(lp);
          const shut = await lp.evaluate(() => ({ on: __calls.filter(c => c[0] === 'eyepiece').map(c => c[1].on).pop(), shut: document.getElementById('eyepiece').getAttribute('aria-checked'), kept: localStorage.getItem('bombechat.eyepiece') }));
          await lp.locator('.mod:has(#k2)').screenshot({ path: path.join(out, `light-${w}-shut.png`) });
          await lp.click('#eyepiece'); await paint(lp);
          const open = await lp.evaluate(() => ({ on: __calls.filter(c => c[0] === 'eyepiece').map(c => c[1].on).pop(), kept: localStorage.getItem('bombechat.eyepiece') }));
          if (shut.on !== false || shut.shut !== 'false' || shut.kept !== 'shut' || open.on !== true || open.kept !== null) problems.push(`${tag}: the eyepiece's shutter: ${JSON.stringify({ shut, open })}`);
          // leaving the page lets go of the eyepiece; back from the back/forward cache, it is asked for again
          const lastEye = () => lp.evaluate(() => __calls.filter(c => c[0] === 'eyepiece').map(c => c[1].on).pop());
          await lp.evaluate(() => dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
          const hid = await lastEye();
          await lp.evaluate(() => dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
          const back = await lastEye();
          if (hid !== false || back !== true) problems.push(`${tag}: the eyepiece on pagehide and pageshow: ${JSON.stringify({ hid, back })}`);
          // a message by light lands on the drums and the sheet as one by sound does, its margin saying light
          const said = 'Meet by the north door.', bytes = [...Buffer.from(said)];
          const plain = [...Array(68).fill(0), ...bytes, ...Array(64).fill(0)];
          await emit(lp, 'channel', { channel: 'light', state: 'receiving' });
          const quiet = await lp.evaluate(() => __drum.receiving);
          if (!quiet) problems.push(`${tag}: a message arriving by light did not quiet the drums as one by sound does`);
          await emit(lp, 'chunk', { tag: 0x0c0d, index: 0, of: 1, channel: 'light', view: { plain, heard: plain.map(b => b ^ 3), key: plain.map(() => 3) } });
          await emit(lp, 'message', { id: Array(16).fill(0x31), contact: 1, outgoing: false, text: said, channel: 'light', at_min: 29_000_100, heard: false, state: 'sent' });
          await emit(lp, 'channel', { channel: 'light', state: 'listening' });
          await lp.waitForTimeout(3200);
          const landed = await lp.evaluate(t => { const ents = [...document.querySelectorAll('#log .ent')].filter(e => [...e.querySelectorAll('.strip')].map(s => s.textContent).join(' ') === t);
            return { copies: ents.length, how: ents[0] && ents[0].querySelector('.when span').textContent, provisional: document.querySelectorAll('#log .provisional').length, drums: !!__bank.frame() }; }, said);
          if (landed.copies !== 1 || landed.how !== 'light' || landed.provisional || !landed.drums) problems.push(`${tag}: a message by light did not land as one by sound does: ${JSON.stringify(landed)}`);
          await lp.locator('.sheet').screenshot({ path: path.join(out, `light-${w}-arrived.png`) });
          r.landed = landed;
        }
        Object.assign(r, { ready: ready.mid, flashing: flashing.frames, stopped: stopped.light, hint: hint.note, refused: refusedNotice.pubnote });
        await lp.close();
      }

      // Pairing by light on the key check card, on the app path (the node's events emitted as it emits them, its frames
      // the core's own) and in a plain browser (the simulator shows the hello and hears Teodor's), at 1024, 1440 and
      // 1728, with 2x crops (card-<path>-<w>-light-<state>.png, light-pair-<path>-<w>-<state>.png): the card at rest with
      // both stamps; Pair by light asks for light only and the lamp shows your hello, saying to hold the screens face to
      // face; their hello in, the card takes the offer under the lamp, the machine is told to finish, and the lamp closes
      // when the screen clears, focus on Matched, add; Matched, add stamps the card "by light". Then (app, 1440): Escape
      // ends the pairing (stop_pairing, the switch untouched, a late frame of the hello not shown); Doesn't match ends it;
      // three minutes (shortened) end it; at Read it is refused on the card in sentence case with nothing asked.
      report.lightPairing = {};
      const SAFETY = '90817 26354 55102 83746 19283 74650 31415 92653 58979 32384 62643 38327';
      const pairCard = () => { const m = document.querySelector('.menu'), l = document.getElementById('lamp');
        return { state: m.dataset.pair, who: m.querySelector('h2 span').textContent, note: m.querySelector('.note').textContent,
          pencil: (m.querySelector('#pencil:not([hidden])') || { innerText: '' }).innerText.replace(/\s+/g, ' ').trim(),
          stamps: [...m.querySelectorAll('.pairact .rs')].filter(b => !b.hidden).map(b => b.textContent),
          stamp: (m.querySelector('.gstmp') || { innerText: '' }).innerText.replace(/\s+/g, ' ').trim(),
          groups: [...m.querySelectorAll('.sn li')].map(li => li.textContent), gridAt: (g => g ? Math.round((g.getBoundingClientRect().top - m.getBoundingClientRect().top) * 2) / 2 : null)(m.querySelector('.sn')),
          lamp: !l.hidden, plate: l.querySelector('.lamp-head .pl.screw').textContent, say: document.getElementById('lampsay').innerText.replace(/\s+/g, ' ').trim(),
          focus: document.activeElement && document.activeElement.id, light: document.getElementById('k2').getAttribute('aria-valuenow'),
          calls: window.__calls ? __calls.filter(c => /pair|set_setting/.test(c[0])).map(c => c[1] ? `${c[0]} ${JSON.stringify(c[1])}` : c[0]) : [] }; };
      for (const where of ['app', 'browser']) for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
        const tag = `light pairing ${where} ${w}`, r = report.lightPairing[`${where} ${w}`] = {};
        let lp;
        if (where === 'app') lp = await openLight(w, h, 2, { stub: { pairOfferMs: null } });
        else {
          lp = await browser.newPage({ viewport: { width: w, height: h }, deviceScaleFactor: 2 });
          watch(lp, tag);
          await lp.addInitScript(() => sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify({ light: 2 })));
          await lp.goto(url('still&ready=300'));
          await lp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
          await lp.evaluate(() => document.fonts.ready);
        }
        const st = () => lp.evaluate(pairCard);
        const cardShot = n => lp.locator('.menu').screenshot({ path: path.join(out, `card-${where}-${w}-light-${n}.png`) });
        const rest = await st();
        if (JSON.stringify(rest.stamps) !== '["Pair by sound","Pair by light"]') problems.push(`${tag}: the card's stamps at rest: ${JSON.stringify(rest.stamps)}`);
        await cardShot('rest');
        await lp.click('#pairlight');
        // the card reading, before the first frame of the hello reaches the lamp (at 1440 and 1728 the lamp then covers
        // the card, so this is the card's only moment in view there; the browser's simulator flashes at once)
        if (where === 'app') { await paint(lp); await cardShot('reading'); }
        if (where === 'app') { for (let i = 0; i <= 3; i++) { await emit(lp, 'display', helloFrame(1, i, 0x4e1)); await paint(lp); } await lp.waitForTimeout(450); for (let i = 4; i <= 6; i++) { await emit(lp, 'display', helloFrame(1, i, 0x4e1)); await paint(lp); } }
        else await lp.waitForTimeout(1100);
        const reading = await st();
        const cutP = await lp.evaluate(lampCuts); if (cutP.length) problems.push(`${tag}: pairing by light cuts drums: ${cutP.join('; ')}`);
        if (reading.state !== 'reading' || reading.who !== 'pairing by light' || !reading.lamp || reading.plate !== 'Pairing by light' || reading.say !== 'Hold your screens face to face. reading theirs, Escape stops it' || !/reading for a hello/.test(reading.pencil))
          problems.push(`${tag}: Pair by light did not show the hello: ${JSON.stringify(reading)}`);
        if (where === 'app' && JSON.stringify(reading.calls) !== '["pair {\\"channel\\":\\"light\\"}"]') problems.push(`${tag}: Pair by light asked the machine ${JSON.stringify(reading.calls)}`);
        r.small = await lp.evaluate(`(${measureText.toString()})()`);
        if (r.small.length) problems.push(`${tag}: text under 12px while pairing by light: ${r.small.join('; ')}`);
        await lp.locator('.lamp').screenshot({ path: path.join(out, `light-pair-${where}-${w}-reading.png`) });
        if (where === 'browser' && w === 1024) await cardShot('reading');
        // their hello is in: the card has the offer under the lamp, and the lamp shows yours to the end of its window
        if (where === 'app') { await emit(lp, 'pairOffer', { peer: { sign: Array(32).fill(5), dh: Array(32).fill(6) }, name: 'Marisol Ortega', safety: SAFETY, channel: 'Light' }); await paint(lp); }
        else await lp.waitForFunction(() => document.querySelector('.menu').dataset.pair === 'offer', null, { timeout: 8000 }).catch(() => {});
        const finishing = await st();
        if (finishing.state !== 'offer' || !finishing.lamp || finishing.say !== 'Their key is read. Yours finishes showing. Escape stops it') problems.push(`${tag}: the offer did not wait under the lamp for the hello to finish: ${JSON.stringify(finishing)}`);
        if (where === 'app' && !finishing.calls.includes('finish_pairing')) problems.push(`${tag}: the machine was not told to finish: ${JSON.stringify(finishing.calls)}`);
        await lp.locator('.lamp').screenshot({ path: path.join(out, `light-pair-${where}-${w}-finishing.png`) });
        if (where === 'app') { await emit(lp, 'displayStopped', { tag: 0x4e1 }); await paint(lp); }
        else await lp.waitForFunction(() => document.getElementById('lamp').hidden, null, { timeout: 8000 }).catch(() => {});
        await lp.waitForTimeout(150);
        const offered = await st();
        if (offered.lamp || offered.state !== 'offer' || offered.who !== 'pairing by light' || offered.groups.length !== 12 || offered.focus !== 'pairyes' || offered.note !== 'Compare these with theirs, out loud or side by side.')
          problems.push(`${tag}: the lamp did not close on the offer: ${JSON.stringify(offered)}`);
        await cardShot('offer');
        await lp.click('#pairyes'); await lp.waitForTimeout(500);
        const matched = await st();
        if (matched.state !== '' || !/^Matched \d+ [A-Z][a-z]{2}$/.test(matched.stamp) || matched.note !== 'Checked in person, by light.' || Math.abs(matched.gridAt - offered.gridAt) > 1 || matched.lamp || matched.focus !== 'pairlight')
          problems.push(`${tag}: Matched, add after pairing by light: ${JSON.stringify(matched)}`);
        await cardShot('matched');
        Object.assign(r, { reading: reading.say, finishing: finishing.say, offered: offered.state, matched: matched.note });
        if (where === 'app' && w === 1440) {
          // Escape ends the pairing, not the switch, and a frame of the hello still on its way is not shown
          const before = (await st()).calls.length;
          await lp.click('#pairlight'); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 0, 0x4e2)); await lp.waitForTimeout(500); await emit(lp, 'display', helloFrame(1, 1, 0x4e2)); await paint(lp);
          await lp.keyboard.press('Escape'); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 2, 0x4e2)); await lp.waitForTimeout(300);
          const esc = await st();
          if (esc.lamp || esc.state !== '' || esc.note !== 'Pairing by light stopped. Nothing was kept.' || esc.light !== '2' || JSON.stringify(esc.calls.slice(before)) !== '["pair {\\"channel\\":\\"light\\"}","stop_pairing"]' || esc.focus !== 'pairlight')
            problems.push(`${tag}: Escape did not end the pairing by light: ${JSON.stringify(esc)}`);
          await emit(lp, 'displayStopped', { tag: 0x4e2 });
          // Doesn't match ends it
          const before2 = (await st()).calls.length;
          await lp.click('#pairlight'); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 0, 0x4e3)); await lp.waitForTimeout(500);
          await emit(lp, 'pairOffer', { peer: { sign: Array(32).fill(7), dh: Array(32).fill(8) }, name: 'Someone Else', safety: SAFETY, channel: 'Light' }); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 1, 0x4e3)); await paint(lp);
          await lp.keyboard.press('Escape'); await paint(lp);
          const escOffer = await st();
          if (escOffer.lamp || escOffer.state !== 'offer' || escOffer.focus !== 'pairyes') problems.push(`${tag}: Escape while the hello finished did not leave the offer: ${JSON.stringify(escOffer)}`);
          await lp.click('#pairno'); await paint(lp);
          const declined = await st();
          if (declined.state !== 'refused' || declined.lamp || declined.focus !== 'pairlight' || JSON.stringify(declined.calls.slice(before2)) !== '["pair {\\"channel\\":\\"light\\"}","finish_pairing","stop_pairing","stop_pairing"]')
            problems.push(`${tag}: Doesn't match after pairing by light: ${JSON.stringify(declined)}`);
          r.escape = esc.note;
          // a message by light on the lamp while pairing keeps its own caption, and Escape stops it the Task 6 way (Light
          // to Read), which ends the pairing too: the card rests and says so. The hello's frames after it are let go
          // only by their own tag, never the message's
          await lp.click('#pairlight'); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 0, 0x4e5)); await lp.waitForTimeout(500);
          await emit(lp, 'display', frame(1, 0, 0x4e6)); await lp.waitForTimeout(450); await emit(lp, 'display', frame(1, 1, 0x4e6)); await paint(lp);
          const mixed = await st();
          if (!mixed.lamp || mixed.plate !== 'Flashing by light' || mixed.say !== 'Hold this screen up to their camera. flashing, Escape stops it' || mixed.state !== 'reading')
            problems.push(`${tag}: a message by light during pairing does not keep its own lamp: ${JSON.stringify(mixed)}`);
          await lp.locator('.lamp').screenshot({ path: path.join(out, 'light-pair-app-1440-message.png') });
          // (the knob's reading settles once its turn has drawn, 330 ms)
          await lp.keyboard.press('Escape'); await lp.waitForTimeout(600);
          const msgEsc = await st();
          if (msgEsc.lamp || msgEsc.light !== '1' || !msgEsc.calls.includes('set_setting {"id":"light","valueIndex":1}') || msgEsc.state !== '' || msgEsc.note !== 'Pairing by light stopped. Light is at Read, and nothing was kept.')
            problems.push(`${tag}: Escape on a message during pairing: ${JSON.stringify(msgEsc)}`);
          await emit(lp, 'displayStopped', { tag: 0x4e6 });
          r.messageDuringPairing = { plate: mixed.plate, escape: msgEsc.note };
          // Light moved off Read and flash on the settings page while the hello finishes, focus on the lamp's Stop key:
          // back on the front, the lamp closes, the offer stays, and focus goes to Matched, add (not lost with the lamp)
          await lp.focus('#k2'); await lp.keyboard.press('End'); await lp.waitForTimeout(600);
          await lp.click('#pairlight'); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 0, 0x4e7)); await lp.waitForTimeout(500);
          await emit(lp, 'pairOffer', { peer: { sign: Array(32).fill(9), dh: Array(32).fill(10) }, name: 'Marisol Ortega', safety: SAFETY, channel: 'Light' }); await paint(lp);
          await emit(lp, 'display', helloFrame(1, 1, 0x4e7)); await paint(lp);
          await lp.focus('#lampstop');
          const beforeMove = await st();
          await lp.evaluate(() => { window.__moveSetting('light', 1); dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })); });
          await lp.waitForTimeout(600);
          const moved = await st();
          if (!beforeMove.lamp || beforeMove.focus !== 'lampstop' || moved.lamp || moved.state !== 'offer' || moved.light !== '1' || moved.focus !== 'pairyes')
            problems.push(`${tag}: Light moved off Read and flash while the hello finished: ${JSON.stringify({ before: { lamp: beforeMove.lamp, focus: beforeMove.focus }, moved })}`);
          await emit(lp, 'displayStopped', { tag: 0x4e7 });
          r.lightMovedWhileFinishing = { state: moved.state, focus: moved.focus };
        }
        await lp.close();
      }
      {
        // three minutes, shortened to 1.5 s: the pairing ends, the screen clears, and the card says so
        const tag = 'light pairing cap 1440';
        const lp = await openLight(1440, 900, 2, { q: 'still&ready=300&paircap=1500', stub: { pairOfferMs: null } });
        await lp.click('#pairlight'); await paint(lp);
        await emit(lp, 'display', helloFrame(1, 0, 0x4e4)); await lp.waitForTimeout(1700);
        await emit(lp, 'display', helloFrame(1, 1, 0x4e4)); await paint(lp);
        const capped = await lp.evaluate(pairCard);
        if (capped.lamp || capped.state !== '' || capped.note !== 'Pairing by light ended after three minutes. Nothing was kept.' || !capped.calls.includes('stop_pairing')) problems.push(`${tag}: the cap did not end it: ${JSON.stringify(capped)}`);
        await lp.locator('.menu').screenshot({ path: path.join(out, 'card-app-1440-light-capped.png') });
        await lp.close();
        // at Read: refused on the card, nothing asked of the machine
        const rp = await openLight(1440, 900, 1);
        await rp.click('#pairlight'); await paint(rp);
        const refused = await rp.evaluate(pairCard);
        if (refused.lamp || refused.note !== 'Light is set to read only. Turn it to Read and flash to pair by light.' || refused.calls.some(c => /^pair/.test(c))) problems.push(`light pairing at Read: ${JSON.stringify(refused)}`);
        await rp.locator('.menu').screenshot({ path: path.join(out, 'card-app-1440-light-refused.png') });
        await rp.close();
        // browser: Escape ends the simulator's hello too
        const bp = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
        watch(bp, 'light pairing browser escape');
        await bp.addInitScript(() => sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify({ light: 2 })));
        await bp.goto(url('still&ready=300&paircap=60000'));
        await bp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
        await bp.click('#pairlight'); await bp.waitForTimeout(900);
        const up = await bp.evaluate(pairCard);
        await bp.keyboard.press('Escape'); await bp.waitForTimeout(800);
        const esc = await bp.evaluate(pairCard);
        if (!up.lamp || esc.lamp || esc.state !== '' || esc.note !== 'Pairing by light stopped. Nothing was kept.' || esc.light !== '2') problems.push(`light pairing browser: Escape: ${JSON.stringify({ up: up.lamp, esc })}`);
        await bp.close();
        report.lightPairing.cap = capped.note; report.lightPairing.refused = refused.note;
      }

      // the lamp names whose camera only for the message it belongs to: a receipt that goes on screen before a sent
      // message is for any camera, and the message, once the machine says it is playing, is for Marisol's
      {
        const np = await openLight(1440, 900, 2, { stub: { sendId: 'c0ffee'.repeat(5) + 'ab' } });
        await np.fill('#reply', 'On my way.'); await np.click('#flash'); await np.waitForTimeout(100);
        await emit(np, 'display', frame(1, 0, 21)); await paint(np);
        const receipt = (await np.evaluate(view)).say;
        await emit(np, 'displayStopped', { tag: 21 }); await paint(np);
        await emit(np, 'outbox', { id: 'c0ffee'.repeat(5) + 'ab', state: 'playing' });
        await emit(np, 'display', frame(1, 0, 22)); await paint(np);
        const message = (await np.evaluate(view)).say;
        await emit(np, 'displayStopped', { tag: 22 }); await paint(np);
        if (!/^Hold this screen up to their camera\./.test(receipt) || !/^Hold this screen up to Marisol's camera\./.test(message))
          problems.push(`the lamp's caption went to the wrong message: ${JSON.stringify({ receipt, message })}`);
        report.lampCaption = { receipt, message };
        await np.close();
      }

      // Photosensitivity. WCAG 2.3.1, general flash: a flash is a pair of opposing changes in relative luminance of 10% or
      // more of the maximum where the darker state is below 0.80; no more than three in any one second over any area as
      // large as about a quarter of the central field of view. Here the view is the lamp's box; each window is a quarter
      // of its width by a quarter of its height (a sixteenth of it), stepped by an eighth, so 49 windows. 120 consecutive
      // frames: the sheet, the still field as the lamp opens, 117 code frames, and the sheet back, at the preset's rate
      // (every frame one 1/fps step).
      report.photosensitivity = {};
      for (const spec of [0, 1, 2]) {
        const fps = [6, 10, 15][spec];
        const lp = await openLight(1440, 900, 2, { dpr: 1, q: 'still&ready=300' });
        const caps = [];
        let box = null;
        const cap = async () => caps.push((await lp.screenshot({ clip: box })).toString('base64'));
        // where the lamp will stand: open it once, then let it close, and start from the sheet
        await emit(lp, 'display', frame(spec, 0, 1)); await paint(lp);
        // (the lamp's column, where the code is: the band it reaches over the drum bay with is the same still grey)
        box = await lp.evaluate(() => { const l = document.getElementById('lamp'), r = l.getBoundingClientRect(), nx = parseFloat(getComputedStyle(l).getPropertyValue('--nx')) || r.width;
          return { x: r.left, y: r.top, width: Math.min(r.width, nx), height: r.height }; });
        await emit(lp, 'displayStopped', { tag: 1 }); await paint(lp); await cap();
        await emit(lp, 'display', frame(spec, 0)); await paint(lp); await cap();
        await lp.waitForTimeout(350);
        for (let i = 1; i <= 117; i++) { await emit(lp, 'display', frame(spec, i)); await paint(lp); await cap(); }
        await emit(lp, 'displayStopped', { tag: 4660 }); await paint(lp); await cap();
        const m = await lp.evaluate(async ([caps, fps]) => {
          const lin = c => { c /= 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
          const LUT = Array.from({ length: 256 }, (_, i) => lin(i));
          const series = [];
          let W = 0, H = 0;
          for (const b64 of caps) {
            const bmp = await createImageBitmap(await (await fetch('data:image/png;base64,' + b64)).blob());
            W = bmp.width; H = bmp.height;
            const c = new OffscreenCanvas(W, H), k = c.getContext('2d'); k.drawImage(bmp, 0, 0);
            const d = k.getImageData(0, 0, W, H).data, Y = new Float64Array(W * H);
            for (let i = 0; i < W * H; i++) Y[i] = 0.2126 * LUT[d[i * 4]] + 0.7152 * LUT[d[i * 4 + 1]] + 0.0722 * LUT[d[i * 4 + 2]];
            const ww = Math.floor(W / 4), wh = Math.floor(H / 4), means = [];
            for (let gy = 0; gy < 7; gy++) for (let gx = 0; gx < 7; gx++) {
              const x0 = Math.floor(gx * W / 8), y0 = Math.floor(gy * H / 8);
              let s = 0; for (let y = y0; y < y0 + wh; y++) for (let x = x0; x < x0 + ww; x++) s += Y[y * W + x];
              means.push(s / (ww * wh));
            }
            series.push(means);
          }
          let worst = 0, worstFrames = 0, worstAt = null, flashes = 0;
          for (let wi = 0; wi < 49; wi++) {
            const L = series.map(m => m[wi]);
            for (let t = 1; t < L.length; t++) {
              const d = Math.abs(L[t] - L[t - 1]);
              if (d > worst) { worst = d; worstAt = { window: wi, frame: t }; }
              if (t > 2 && t < L.length - 1 && d > worstFrames) worstFrames = d;
            }
            // transitions: a change of 0.1 or more from the last extreme, the darker side under 0.8
            let ext = L[0], dir = 0; const tr = [];
            for (let t = 1; t < L.length; t++) {
              const v = L[t];
              if ((dir === 1 && v >= ext) || (dir === -1 && v <= ext)) { ext = v; continue; }
              if (Math.abs(v - ext) >= 0.1 && Math.min(v, ext) < 0.8) { dir = Math.sign(v - ext); ext = v; tr.push(t); }
            }
            for (let t0 = 0; t0 < L.length; t0++) flashes = Math.max(flashes, Math.floor(tr.filter(t => t >= t0 && t < t0 + fps).length / 2));
          }
          return { size: [W, H], captures: series.length, worstChange: +worst.toFixed(4), worstAt, worstBetweenCodeFrames: +worstFrames.toFixed(4), flashesPerSecond: flashes };
        }, [caps, fps]);
        report.photosensitivity[['slow', 'normal', 'fast'][spec]] = m;
        if (m.captures !== 120 || m.flashesPerSecond > 3) problems.push(`photosensitivity (${['slow', 'normal', 'fast'][spec]}): ${JSON.stringify(m)}`);
        // the code frames themselves stay far under the 10% a single change may be
        if (m.worstBetweenCodeFrames >= 0.1) problems.push(`photosensitivity (${['slow', 'normal', 'fast'][spec]}): one code frame to the next changes a window by ${m.worstBetweenCodeFrames}`);
        await lp.close();
      }

      // Flash in the browser (the simulator) at Read and flash: a reply goes by light, the lamp shows it for Marisol's
      // camera, still first, and the sheet comes back when it is sent; a notice goes by light too, signed; a notice too
      // long is refused; a message by light lands with its margin saying light; the eyepiece shows the simulated camera
      {
        const bp = await browser.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
        watch(bp, 'light browser');
        await bp.addInitScript(() => { try { sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify({ light: 2 })); sessionStorage.removeItem('bombechat.simulated-notices'); } catch (e) {} });
        await bp.goto(url('still'));
        await bp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
        await bp.waitForTimeout(300);
        const eye = await bp.evaluate(() => { const c = document.querySelector('#eyepiece canvas'); return { dark: document.getElementById('eyepiece').classList.contains('dark'), px: c.getContext('2d').getImageData(52, 12, 1, 1).data[0] }; });
        if (eye.dark || eye.px < 100) problems.push(`browser: the eyepiece shows no simulated camera: ${JSON.stringify(eye)}`);
        await bp.fill('#reply', 'Meet at the library at noon.'); await bp.click('#flash');
        await bp.waitForTimeout(250);
        const opened = await bp.evaluate(() => ({ shown: !document.getElementById('lamp').hidden, say: document.getElementById('lampsay').firstChild.nodeValue, how: [...document.querySelectorAll('#log .ent.mine')].pop().querySelector('.when span').textContent }));
        await bp.waitForTimeout(1500);
        const during = await bp.evaluate(() => ({ state: [...document.querySelectorAll('#log .ent.mine')].pop().querySelector('.state').textContent, frames: +document.getElementById('lampn').textContent }));
        await bp.locator('.deck').screenshot({ path: path.join(out, 'light-browser-1440-flashing.png') });
        await bp.waitForFunction(() => document.getElementById('lamp').hidden, null, { timeout: 8000 }).catch(() => {});
        await bp.waitForTimeout(300);
        const after = await bp.evaluate(() => ({ shown: !document.getElementById('lamp').hidden, stamp: ([...document.querySelectorAll('#log .ent.mine')].pop().querySelector('.stamp') || {}).textContent }));
        if (!opened.shown || opened.say !== "Hold this screen up to Marisol's camera." || opened.how !== 'light' || during.state !== 'flashing on screen' || !(during.frames > 3) || after.shown || !/^seen by Marisol/.test(after.stamp || ''))
          problems.push(`browser: Flash did not send a reply by light: ${JSON.stringify({ opened, during, after })}`);
        await bp.fill('#reply', 'x'.repeat(2815)); await bp.click('#flash'); await bp.waitForTimeout(150);
        const longReply = await bp.evaluate(() => ({ note: document.getElementById('pubnote').textContent, kept: document.getElementById('reply').value.length }));
        if (longReply.note !== 'Too long to send: 2815 bytes, at most 2814.' || longReply.kept !== 2815) problems.push(`browser: a reply too long for light: ${JSON.stringify(longReply)}`);
        await bp.click('#mode'); await bp.fill('#reply', 'x'.repeat(3940)); await bp.click('#flash'); await bp.waitForTimeout(150);
        const long = await bp.evaluate(() => document.getElementById('pubnote').textContent);
        if (long !== 'Too long to broadcast: 3940 bytes, at most 3939.') problems.push(`browser: a notice too long for light: ${long}`);
        await bp.fill('#reply', 'Water at the church.'); await bp.click('#flash');
        await bp.waitForFunction(() => [...document.querySelectorAll('#log .ent.public.mine')].some(e => /sent to anyone in range/.test(e.querySelector('.state').textContent)), null, { timeout: 9000 }).catch(() => {});
        const notice = await bp.evaluate(() => { const e = [...document.querySelectorAll('#log .ent.public.mine')].pop(); return e && { text: e.querySelector('.typed').textContent, state: e.querySelector('.state').textContent }; });
        if (!notice || notice.text !== 'Water at the church.' || notice.state !== 'signed C41D, sent to anyone in range') problems.push(`browser: a notice by light: ${JSON.stringify(notice)}`);
        await bp.evaluate(() => __sim.arrive('Meet by the north door.', { channel: 'light' }));
        await bp.waitForTimeout(3200);
        const arrived = await bp.evaluate(() => { const e = [...document.querySelectorAll('#log .ent')].pop(); return { how: e.querySelector('.when span').textContent, text: [...e.querySelectorAll('.strip')].map(s => s.textContent).join(' ') }; });
        if (arrived.how !== 'light' || arrived.text !== 'Meet by the north door.') problems.push(`browser: a message by light: ${JSON.stringify(arrived)}`);
        report.light.browser = { opened, during, after, notice, arrived };
        await bp.close();
      }
    }
  }

  // 7. offline audio
  const au = await browser.newPage();
  watch(au, 'audio');
  await au.goto(url('audiotest&still'));
  await au.waitForFunction(() => document.body.dataset.audiotest, null, { timeout: 60000 });
  const audio = JSON.parse(await au.evaluate(() => document.body.dataset.audiotest));
  if (audio.clipped) problems.push('drum audio clips');
  if (+audio.listeningBandShare > 1e-4) problems.push('drum audio leaks into the listening band: ' + audio.listeningBandShare);
  report.audio = audio;

  await browser.close();

  // 8. without WebGL the bank stays the flat drums: three rows of twelve, nothing between the rows, the plain row
  // settling on the frame text, no page errors
  if (APP) {
    const flat = await chromium.launch({ channel: 'chrome', args: ['--disable-webgl', '--disable-3d-apis'] });
    const fp = await flat.newPage({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
    // three.js says on the console that it found no WebGL: that is the fallback being taken, not a fault
    fp.on('pageerror', e => problems.push('flat: page error: ' + e.message));
    fp.on('console', m => m.type() === 'error' && !/^THREE\.WebGLRenderer: (A WebGL context could not be created|Error creating WebGL context)/.test(m.text()) && problems.push('flat: console: ' + m.text()));
    fp.on('request', r => { const u = r.url(); if (!/^(data:|blob:|http:\/\/127\.0\.0\.1:4812\/)/.test(u)) problems.push(`flat: asked the network for ${u}`); });
    await fp.goto(url());
    await fp.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
    const fpText = () => window.__bank.frame ? __bank.frame().plain : null;
    await fp.waitForFunction(`__bank.frame() && __bank.rows().plain === (${fpText})()`, null, { timeout: 8000 }).catch(() => {});
    report.flat = await fp.evaluate(`({ gl: __bank.gl, bars: document.querySelectorAll('#drums .bar').length, drums: document.querySelectorAll('#drums .drum').length, plain: __bank.rows().plain, expected: (${fpText})() })`);
    if (report.flat.gl || report.flat.bars || report.flat.drums !== 36) problems.push(`flat drum bank: ${JSON.stringify(report.flat)}`);
    if (report.flat.plain !== report.flat.expected) problems.push(`flat drum bank: the plain row reads ${report.flat.plain}, the frame is ${report.flat.expected}`);
    await fp.locator('.bank').screenshot({ path: path.join(out, 'bank-flat.png') });
    await flat.close();
  }
  console.log(JSON.stringify({ out, frames, ...report, problems }, null, 2));
  server.close();
  process.exit(problems.length ? 1 : 0);
})();
