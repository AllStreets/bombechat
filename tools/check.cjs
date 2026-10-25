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
// and prints what it measured. Exits non-zero on page errors, horizontal overflow,
// text under 12px, layout shift on load, a plain row that doesn't settle on the
// frame text, the sheet not keeping its place, a reply that doesn't land, clipped
// audio, or drum sound leaking into the 17.6 to 19.8 kHz listening band.
// For the app's interface it also fails when the drums don't follow an injected
// chunk, when the drums make a sound while a message is arriving by sound, when
// pairing by sound doesn't run in the browser or on the app path (first run asking your name,
// pair, a 60-digit offer legible at 1024, 1440 and 1728 under an editable name, Matched, add
// keeping the corrected name and stamping the card in place, Doesn't match keeping nothing,
// a held hello lapsing), when writing a public notice doesn't work in the browser or on the app
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
const url = q => 'http://127.0.0.1:4812/' + path.relative(ROOTDIR, page) + (q ? '?' + q : '');
// held still on a frame of the incoming message: the app's ?f counts chunks (chunk 1 ends on
// a frame of the text), the mockup's counts frames
const HELD = APP ? 'f=1&still' : 'f=3&still';
const VIEWPORTS = [[1024, 768], [1280, 800], [1440, 900], [1728, 1117]];
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
function tauriStub({ hello, safety, people, talk, afterUnlock, settings, broadcastMax = 3939, kept = [], refuseBroadcast, keychain, unlockMs = 0, wrong = 0, wrongWhy = 'wrong passphrase', refuseSetting }) {
  let unlocked = !!keychain, refusals = wrong;
  const KEPT = 'stub.notices';
  const notices = () => JSON.parse(sessionStorage.getItem(KEPT) || 'null') || kept;
  const keep = list => sessionStorage.setItem(KEPT, JSON.stringify(list));
  const settle = (id, state) => keep(notices().map(n => String(n.id) === String(id) ? { ...n, state } : n));
  const listeners = {};
  window.__calls = [];
  window.__emit = (name, payload) => (listeners[name] || []).forEach(f => f({ payload }));
  window.__TAURI__ = {
    event: { listen: (name, f) => { (listeners[name] = listeners[name] || []).push(f); return Promise.resolve(() => {}); } },
    core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
      switch (cmd) {
        case 'contacts': return unlocked ? Promise.resolve(people || []) : Promise.reject(people ? 'locked' : 'no store yet');
        case 'unlock': return new Promise(r => setTimeout(r, unlockMs)).then(() => {
          if (refusals > 0) { refusals--; return Promise.reject(wrongWhy); }
          unlocked = true; for (const [name, payload] of afterUnlock || []) window.__emit(name, payload); return null; });
        case 'machine': return Promise.resolve({ name: 'Machine No. 1', key: 'a3f09c1e'.repeat(16), you: 'Ana Lima', held: 0, version: '0.1.0', broadcast_max: broadcastMax });
        case 'conversation': return Promise.resolve((talk || {})[args.contactId] || []);
        case 'settings': return settings === 'unread' ? Promise.reject('the machine did not answer') : Promise.resolve(settings || { sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 });
        // as the device answers on this build: relay and light wait for their features, the key check is always on
        case 'setting_notes': return settings === 'unread' ? Promise.reject('the machine did not answer') : Promise.resolve({ carry: { note: 'comes with relay' }, hops: { note: 'comes with relay' }, hold: { note: 'comes with relay' },
          light: { note: 'comes with light' }, flash: { note: 'comes with light' }, inperson: { note: 'always, on this build', value: 1 } });
        case 'channels': return Promise.resolve([{ channel: 'sound', state: 'listening', rate: 16.1 }]);
        case 'pair': setTimeout(() => window.__emit('pairOffer', { peer: { sign: Array(32).fill(5), dh: Array(32).fill(6) }, name: hello, safety }), 1200); return Promise.resolve(null);
        case 'confirm_pair': return Promise.resolve(7);
        case 'set_setting': return refuseSetting ? Promise.reject(refuseSetting) : Promise.resolve(null);
        case 'send': return Promise.reject('not sent');
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

  // 6b. the drums follow the bridge: a known chunk injected through the simulator
  // lands on the plain row, and nothing clicks while it arrives by sound
  if (APP) {
    const br = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    watch(br, 'bridge');
    await br.goto(url());
    await br.mouse.click(5, 5);
    await br.waitForFunction(() => window.__bank && __bank.ready && window.__sim, null, { timeout: 60000 });
    await br.waitForFunction(() => __drum.running, null, { timeout: 5000 }).catch(() => {});
    // a chunk of real text: letters read as themselves on the plain row, anything else as X
    const KNOWN = 'Look under the blue stairs for the radio.';
    const WANT = [...KNOWN.slice(0, 12)].map(ch => /[a-z]/i.test(ch) ? ch.toUpperCase() : 'X').join('');
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
        sets: __calls.filter(c => c[0] === 'set_setting').map(c => c[1]) }));
      if (got.sound !== '2' || !/rotate\(120deg\)/.test(got.cap) || got.top !== 'Listening by sound' || got.sendOff === 'true') problems.push(`knob refused ${w}: the Sound switch did not go back to Listen and send: ${JSON.stringify(got)}`);
      if (got.said !== 'The setting was not saved: disk full' || got.shown < .9 || got.size < 12) problems.push(`knob refused ${w}: the reason is not shown: ${JSON.stringify(got)}`);
      if (JSON.stringify(got.sets) !== JSON.stringify([{ id: 'sound', valueIndex: 0 }])) problems.push(`knob refused ${w}: sent ${JSON.stringify(got.sets)}`);
      await kr.locator('.top').screenshot({ path: path.join(out, `knob-refused-top-${w}.png`) });
      report.knobRefused[w] = got;
      await kr.close();
    }
    // Machine sound and Machine volume are the machine's settings: the title bar's lever shows what it keeps and the
    // drums play at its volume; moving the lever sends that one change. The Light switch, whose feature isn't fitted
    // yet, is taped down at Off, says what it waits for, and turning it sends nothing.
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
      await ms.locator('.mod:has(#k2)').screenshot({ path: path.join(out, `light-taped-${where}.png`) });
      await ms.click('#k2', { force: true });
      await ms.focus('#k2'); await ms.keyboard.press('ArrowRight');
      await ms.waitForTimeout(400);
      const light = await ms.evaluate(`({ light: document.getElementById('k2').getAttribute('aria-valuenow'), sets: (${sets})() })`);
      await ms.click('#snd');
      await ms.waitForTimeout(200);
      const flipped = await ms.evaluate(`({ lever: document.getElementById('snd').getAttribute('aria-pressed'), on: __drum.on, sets: (${sets})() })`);
      const kept = where === 'browser' ? await ms.evaluate(() => import('./bridge.js').then(m => m.bridge.settings())) : null;
      const none = s => (Array.isArray(s) ? s.length : s) === 0;
      if (booted.lever !== 'false' || booted.on !== false) problems.push(`${where}: the machine keeps Machine sound off, the lever shows ${JSON.stringify(booted)}`);
      if (Math.abs(booted.gain - .9) > 1e-6) problems.push(`${where}: Machine volume 5 plays the drums at ${booted.gain}, not 0.9`);
      if (booted.label !== 'Machine sound') problems.push(`${where}: the lever is labelled ${booted.label}`);
      if (!none(booted.sets)) problems.push(`${where}: settings sent at load: ${JSON.stringify(booted.sets)}`);
      if (booted.light !== '0' || booted.lightOff !== 'true' || booted.tape !== 'comes with light') problems.push(`${where}: the Light switch is not taped down at Off: ${JSON.stringify(booted)}`);
      if (light.light !== '0' || !none(light.sets)) problems.push(`${where}: turning the taped Light switch did something: ${JSON.stringify(light)}`);
      if (flipped.lever !== 'true' || !flipped.on || (where === 'app' ? JSON.stringify(flipped.sets) !== JSON.stringify([{ id: 'machine', valueIndex: 1 }]) : flipped.sets !== 1 || kept.machine !== 1))
        problems.push(`${where}: the lever sent ${JSON.stringify(flipped)}, kept ${JSON.stringify(kept)}`);
      report[`machineSound-${where}`] = { booted, light, flipped };
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
        if (heldNote !== 'Teodor Vance said hello. Press Pair by sound to answer.' || /said hello/.test(lapsed)) problems.push(`${tag}: a held hello's note: ${JSON.stringify({ heldNote, lapsed })}`);
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
      // Flash never sends a notice as a private reply
      await bp.click('#flash', { force: true }); await bp.waitForTimeout(150);
      const flashed = await bp.evaluate(line);
      if (flashed.note !== 'A public notice goes by sound only. Use Send by sound.' || flashed.box !== TEXT || flashed.replies !== composing.replies || (flashed.calls && flashed.calls.length)) problems.push(`${tag}: Flash did not refuse a notice: ${JSON.stringify(flashed)}`);
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
  console.log(JSON.stringify({ out, frames, ...report, problems }, null, 2));
  server.close();
  process.exit(problems.length ? 1 : 0);
})();
