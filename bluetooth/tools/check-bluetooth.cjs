// The Bluetooth edition's check of its own parts, in a plain browser over the simulator (npm run check runs it after
// the shared checks):
//   node tools/check-bluetooth.cjs
// Writes checks/app-ui/bluetooth/: 2x crops of the front's panel in every Bluetooth state (off, contacts, open,
// request, request-held, requests-two, accepted, asking, send, locked, unavailable, unavailable-long) at 1024, 1440 and 1728
// (x2-<w>-<state>.png, with the deck, top bar and bank's foot for some), and the settings card's Bluetooth row.
// Fails on page errors, a request to another host, text under 12px in the Bluetooth parts, a page that scrolls
// sideways, and when: the settings card has no live Bluetooth row (Off, Contacts, Open; Off by default, kept across
// pages, its plug dragged on the machine landing where it is let go), still shows the blanking plate, or lacks the
// Mac-name note; New contacts need a key check is held instead of live; the front still shows the blanking plate or
// Not fitted; the Bluetooth switch (a knob like Sound's and Light's) at each position, by keyboard and click, doesn't
// set the setting, the channel's word, the jewel (lit only while the radio works), the top plate and who is in range
// (contacts and strangers, on the bank's foot); a send at Off isn't refused in so many words; a request doesn't arrive
// as a slip (the name as they gave it, editable, Escape putting it back, the key's first four digits, the time, Accept
// and Decline, in Tab order), an empty name isn't refused on the slip, accepting doesn't keep them unchecked under the
// corrected name with their station marked not confirmed until confirmed and the card laying out the number to
// compare, unstamped; declining doesn't keep nothing and bring the next; the key check On doesn't type the node's
// sentence in place of Accept (and once on the strangers' slip when no request says it); asking a stranger isn't
// refused at Contacts, or doesn't show asking, delivered, not delivered and accepted; Off doesn't end requests,
// strangers and who is in range; the locked machine doesn't say it waits for the passphrase; the radio's reason for
// not working isn't on the tape under the switch, within the switch's width (after a resize too), or working again
// isn't said; Send by Bluetooth isn't beside Flash and Send by sound with the line uncrowded, isn't refused under the line
// at Off, when the contact is out of range or for a public notice, doesn't send and get heard in range, or doesn't wait
// for a contact who left and go when they are back; a station's line is cut short; or, at 1280 and up, the deck or the
// stations move when the tray fills. Every wait is on a condition in the page, and one that never comes fails.
const path = require('path'), fs = require('fs');
const { chromium } = require('playwright');
const { start } = require('./serve.cjs');
const ROOTDIR = path.resolve(__dirname, '..');
const out = path.join(ROOTDIR, 'checks', 'app-ui', 'bluetooth');
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const PORT = 4813;
const url = (page, q) => `http://127.0.0.1:${PORT}/app/ui/${page}` + (q ? '?' + q : '');
const MEET_FIRST = 'Meet them and compare keys first: this machine only adds people checked in person.';
const NEED_OPEN = 'Requests go out only with Bluetooth set to Open.';
const GROUP_NOTE = 'Anyone in Bluetooth range can read this Mac\'s name. Give it a plain name that doesn\'t say who you are, in System Settings, General, About.';

(async () => {
  const server = await start(PORT);
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
  const problems = [];
  const fail = m => problems.push(m);
  const watch = (p, tag) => {
    p.on('pageerror', e => fail(`${tag}: page error: ${e.message}`));
    p.on('console', m => m.type() === 'error' && fail(`${tag}: console: ${m.text()}`));
    p.on('request', r => { const u = r.url(); if (!/^(data:|blob:|http:\/\/127\.0\.0\.1:4813\/)/.test(u)) fail(`${tag}: asked the network for ${u}`); });
  };
  // the front in the simulator, held still on the incoming message, the stranger asking only when asked to; the
  // machine's settings (sessionStorage) as given. Events the bridge hands the page are kept in window.__seen.
  const front = async (ctx, tag, w = 1440, h = 900) => {
    const p = await ctx.newPage();
    await p.setViewportSize({ width: w, height: h });
    watch(p, tag);
    await p.goto(url('bombechat.html', 'f=1&still'), { waitUntil: 'networkidle' });
    await p.waitForFunction(() => window.__bank && __bank.ready, null, { timeout: 60000 });
    await p.evaluate(() => document.fonts.ready);
    await p.evaluate(() => import('./bridge.js').then(({ bridge }) => {
      window.__seen = [];
      for (const e of ['channel', 'request', 'requestGone', 'requested', 'peers', 'contactConfirmed', 'outbox']) bridge.on(e, x => window.__seen.push([e, x]));
      window.__bt = bridge;
    }));
    await wait(p, `${tag}: the Bluetooth face`, () => !!(document.querySelector('.mod.bt') || {}).dataset?.bluetooth);
    return p;
  };
  const machine = async settings => {
    const ctx = await browser.newContext({ deviceScaleFactor: 2 });
    await ctx.addInitScript(s => { try { localStorage.clear(); if (s) sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify(s)); } catch (e) {} }, settings);
    return ctx;
  };
  const call = (p, f, ...a) => p.evaluate(([f, a]) => window.__bt[f](...a).then(v => ({ ok: v }), e => ({ err: String(e) })), [f, a]);
  const seen = (p, name) => p.evaluate(n => window.__seen.filter(e => e[0] === n).map(e => e[1]), name);
  const said = async p => (await seen(p, 'channel')).filter(c => c.channel === 'bluetooth').map(c => c.state);
  // the face as it stands: the tile (its state, jewel, title, the tape under it, the knob), the top plate, who is in
  // range, the tray and the stations
  const face = p => p.evaluate(() => { const t = document.querySelector('.mod.bt'), j = t.querySelector('.jewel'), k = document.getElementById('k3'), near = document.getElementById('btnear');
    const slip = document.querySelector('#intray .slip.ask'), ns = document.querySelector('#neartray .slip.near');
    return { state: t.dataset.bluetooth, title: t.title, lit: !j.classList.contains('off'), note: t.querySelector('#btnote b').textContent, knob: k.getAttribute('aria-valuenow'), knobText: k.getAttribute('aria-valuetext'),
      top: document.getElementById('radio').textContent, near: getComputedStyle(near).visibility === 'hidden' ? null : [...near.querySelectorAll('.odo')].map(o => o.textContent),
      request: slip && { id: +slip.dataset.id, name: slip.querySelector('.askname').value, small: slip.querySelector('small').textContent, accept: !slip.querySelector('.rs.ok').hidden, decline: !slip.querySelector('.rs.no').hidden,
        rule: slip.querySelector('.rule').textContent, why: slip.querySelector('.why').textContent, label: slip.getAttribute('aria-label'), gone: slip.classList.contains('gone') },
      more: [...document.querySelectorAll('#intray .slip.empty')].map(e => e.textContent),
      strangers: ns && [...ns.querySelectorAll('li')].map(li => ({ label: li.dataset.label, ask: !!li.querySelector('button'), said: (li.querySelector('small') || {}).textContent || '' })),
      strangersRule: ns && (ns.querySelector('.rule') || {}).textContent || '',
      stations: [...document.querySelectorAll('.row3 .stn')].map(s => ({ id: +s.dataset.contact || null, name: s.querySelector('.holder span').firstChild.nodeValue, line: s.querySelector('.holder small').textContent, lit: !s.querySelector('.jewel').classList.contains('off') })),
      card: { who: document.querySelector('.menu h2 span').textContent, name: (document.querySelector('.menu .pfrom') || {}).textContent || '', groups: document.querySelectorAll('.menu .sn li').length, stamp: !!document.querySelector('.menu .gstmp'), note: document.querySelector('.menu .note').textContent, label: document.querySelector('.menu').getAttribute('aria-label') },
      said: document.getElementById('said').textContent, blank: !!document.querySelector('.mod .blank, .pl.red') }; });
  // every piece of text in the Bluetooth parts, with the size it is drawn at (the knob's engraving through its scale)
  const smallText = p => p.evaluate(() => { const out = [];
    for (const root of document.querySelectorAll('.mod.bt, #intray, #neartray, #btnear, #radio, .row3')) {
      const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      while (w.nextNode()) { const n = w.currentNode, el = n.parentElement; if (!n.nodeValue.trim() || !el.getBoundingClientRect().width) continue;
        let px = parseFloat(getComputedStyle(el).fontSize); if (el instanceof SVGElement) { const m = el.getScreenCTM(); px = parseFloat(el.getAttribute('font-size')) * Math.hypot(m.a, m.b); }
        if (px < 11.75) out.push(`${n.nodeValue.trim()} ${px.toFixed(1)}px`); } }
    return out; });
  // the request slip shown whole: inside the tray's visible room (never under its fold), none of its text cut;
  // returns what is wrong, or nothing
  const slipWhole = p => p.evaluate(() => { const s = document.querySelector('#intray .slip.ask'); if (!s) return 'no request slip';
    const r = s.getBoundingClientRect(), col = document.querySelector('.col.tray').getBoundingClientRect();
    if (r.top < col.top - 4 || r.bottom > col.bottom + 2 || r.left < col.left - 4 || r.right > col.right + 4) return `the slip runs under the tray's fold: ${JSON.stringify([r.top, r.bottom, col.top, col.bottom])}`;
    const cut = [...s.querySelectorAll('.who, small, .rule, .why, .rs')].filter(e => e.offsetParent && e.scrollWidth > e.clientWidth + 1).map(e => e.textContent);
    if (cut.length) return `text cut on the slip: ${JSON.stringify(cut)}`;
    return ''; });
  // no slip lies over the room meter or a switch's tile, and the meter is whole in the panel
  const overlaps = p => p.evaluate(() => { const box = e => e.getBoundingClientRect(), hit = (a, b) => a.left < b.right - 1 && b.left < a.right - 1 && a.top < b.bottom - 1 && b.top < a.bottom - 1;
    const solid = [document.querySelector('.vu'), ...document.querySelectorAll('.mods .mod')].map(e => [e.className, box(e)]);
    const out = [];
    for (const s of document.querySelectorAll('.row2 .slip')) { const r = box(s); if (!r.width) continue; for (const [n, b] of solid) if (hit(r, b)) out.push(`${s.textContent.slice(0, 20)} over ${n}`); }
    const vu = box(document.querySelector('.vu')), row = box(document.querySelector('.row2'));
    if (vu.bottom > row.bottom + 1) out.push('the meter runs out of its row');
    return out; });
  const setting = (p, id, v) => p.evaluate(([id, v]) => window.__bt.setSetting(id, v), [id, v]);
  // waits on a condition in the page; one that never comes is a failure, named
  const wait = (p, what, f, arg) => p.waitForFunction(f, arg, { timeout: 15000 }).then(() => true, () => { fail(`${what}: never came`); return false; });
  // waits until a value read from the page holds still (a camera move, a plug landing)
  const still = async (p, what, f, arg) => { let last = null, same = 0;
    for (let i = 0; i < 200 && same < 3; i++) { const v = JSON.stringify(await p.evaluate(f, arg)); same = v === last ? same + 1 : 0; last = v; await new Promise(r => setTimeout(r, 60)); }
    if (same < 3) fail(`${what}: never held still`); };

  // 1. the settings card: a live Bluetooth row, Off by default, and the key check live
  {
    const ctx = await machine(null);
    const p = await ctx.newPage();
    await p.setViewportSize({ width: 1440, height: 900 });
    watch(p, 'settings');
    await p.goto(url('settings.html', 'still'), { waitUntil: 'domcontentloaded' });
    await p.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    const row = await p.evaluate(() => { const r = document.querySelector('.row[data-id="bluetooth"]');
      return r && { jacks: [...r.querySelectorAll('.jk')].map(b => b.textContent.trim()), on: +r.querySelector('.jk[aria-checked="true"]').dataset.i, disabled: [...r.querySelectorAll('.jk')].some(b => b.disabled),
        blank: !!document.querySelector('.notfitted'), small: [...document.querySelectorAll('.grp')].find(g => g.firstChild.textContent === 'Bluetooth').querySelector('small').textContent,
        note: ([...document.querySelectorAll('.grp')].find(g => g.firstChild.textContent === 'Bluetooth').nextElementSibling || {}).textContent }; });
    if (!row) fail('settings: no Bluetooth row');
    else {
      if (JSON.stringify(row.jacks) !== JSON.stringify(['Off', 'Contacts', 'Open'])) fail(`settings: the Bluetooth jacks are ${JSON.stringify(row.jacks)}`);
      if (row.on !== 0) fail(`settings: Bluetooth starts at ${row.on}, not Off`);
      if (row.disabled) fail('settings: the Bluetooth row is taped');
      if (row.blank) fail('settings: the blanking plate is still there');
      if (row.small !== '1 plug') fail(`settings: the Bluetooth group says ${JSON.stringify(row.small)}`);
      if (row.note !== GROUP_NOTE) fail(`settings: the Bluetooth group's note is ${JSON.stringify(row.note)}`);
    }
    if (await p.evaluate(() => window.__edition && window.__edition.keyCheckHeld) !== false) fail('settings: the page says the key check is held on this build');
    // drag the Bluetooth plug on the machine itself, from Off to Open, then back to Off
    await p.evaluate(() => window.__bc.focus('bluetooth'));
    await still(p, 'the camera on the Bluetooth row', () => window.__bc.screen('bluetooth').plug);
    for (const to of [2, 0]) {
      const g = await p.evaluate(to => { const s = window.__bc.screen('bluetooth'), r = window.__bc.row('bluetooth'); return { plug: s.plug, jack: window.__bc.at(r.jacks[to], r.y, .3) }; }, to);
      await p.mouse.move(g.plug.x, g.plug.y); await p.mouse.down();
      await p.mouse.move((g.plug.x + g.jack.x) / 2, g.jack.y, { steps: 8 });
      await p.mouse.move(g.jack.x, g.jack.y, { steps: 8 }); await p.mouse.up();
      await wait(p, `the plug at jack ${to}`, to => window.__bc.get('bluetooth') === to && window.__bc.end('bluetooth').jack === to, to);
      await still(p, `the plug at jack ${to}`, () => window.__bc.screen('bluetooth').plug);
      const now = await p.evaluate(() => ({ v: window.__bc.get('bluetooth'), end: window.__bc.end('bluetooth'), on: +document.querySelector('.row[data-id="bluetooth"] .jk[aria-checked="true"]').dataset.i }));
      const kept = await p.evaluate(() => import('./bridge.js').then(m => m.bridge.settings()));
      if (now.v !== to || now.end.jack !== to || now.on !== to || kept.bluetooth !== to) fail(`settings: dragging the Bluetooth plug to jack ${to} left ${JSON.stringify({ ...now, kept: kept.bluetooth })}`);
      await p.screenshot({ path: path.join(out, `drag-bluetooth-${to}.png`) });
    }
    await p.evaluate(() => window.__bc.focus(null));
    const held = await p.evaluate(() => window.__bc.held());
    if (held.inperson) fail(`settings: New contacts need a key check is held: ${JSON.stringify(held.inperson)}`);
    if (held.bluetooth) fail('settings: the Bluetooth switch is held');
    await p.evaluate(() => window.__bc.set('bluetooth', 2));
    await wait(p, 'the plug at Open', () => window.__bc.end('bluetooth').jack === 2);
    if ((await p.evaluate(() => window.__bc.end('bluetooth'))).jack !== 2) fail('settings: the Bluetooth plug did not land at Open');
    const kept = await p.evaluate(() => import('./bridge.js').then(m => m.bridge.settings()));
    if (kept.bluetooth !== 2) fail(`settings: Open not kept: ${JSON.stringify(kept)}`);
    await ctx.close();
    for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
      const c = await machine(null), q = await c.newPage();
      await q.setViewportSize({ width: w, height: h });
      watch(q, `settings ${w}`);
      await q.goto(url('settings.html', 'still&focus=bluetooth'), { waitUntil: 'domcontentloaded' });
      await q.waitForFunction(() => window.__ready, null, { timeout: 90000 });
      await still(q, 'the camera on the Bluetooth row', () => window.__bc.screen('bluetooth').plug);
      await q.evaluate(() => document.querySelector('.row[data-id="bluetooth"]').scrollIntoView({ block: 'center' }));
      await q.screenshot({ path: path.join(out, `x2-${w}-settings.png`) });
      await c.close();
    }
  }


  // 2. the switch at every position, by keyboard and by click: knob, setting, channel, jewel, top plate, who is in
  //    range; nothing of the airgapped plate left
  {
    const ctx = await machine(null);
    const p = await front(ctx, 'switch');
    let f = await face(p);
    if (f.blank) fail('switch: the blanking plate or the Not fitted plate is still on the front');
    if (f.state !== 'off' || f.lit || f.knob !== '0' || f.top !== 'Bluetooth off' || f.near || f.title !== 'Bluetooth: off.') fail(`switch: at Off the face shows ${JSON.stringify(f)}`);
    const ch = (await call(p, 'channels')).ok.find(c => c.channel === 'bluetooth');
    if (!ch || ch.state !== 'off' || 'rate' in ch) fail(`switch: channels() says ${JSON.stringify(ch)}`);
    const want = [[1, 'contacts only', 'Bluetooth to contacts'], [2, 'open to requests', 'Bluetooth open'], [0, 'off', 'Bluetooth off']];
    await p.focus('#k3');
    for (const [v, word, top] of want) {
      await p.keyboard.press(v === 0 ? 'Home' : 'ArrowRight');
      // (the knob says its reading once its turn has drawn)
      await wait(p, `switch to ${v}`, ([w, v]) => document.querySelector('.mod.bt').dataset.bluetooth === w && document.getElementById('k3').getAttribute('aria-valuenow') === String(v), [word, v]);
      f = await face(p);
      const kept = (await call(p, 'settings')).ok.bluetooth;
      if (f.knob !== String(v) || kept !== v || f.state !== word || f.top !== top || f.lit !== (v > 0) || !!f.near !== (v > 0)) fail(`switch: at ${v} by keyboard the face shows ${JSON.stringify({ ...f, kept })}`);
      if (v > 0 && JSON.stringify(f.near) !== JSON.stringify(['01', '01'])) fail(`switch: who is in range reads ${JSON.stringify(f.near)}`);
    }
    await p.click('#k3');
    await wait(p, 'a click to Contacts', () => document.querySelector('.mod.bt').dataset.bluetooth === 'contacts only' && document.getElementById('k3').getAttribute('aria-valuenow') === '1');
    await ctx.close();
  }

  // 3. Send by Bluetooth: beside Flash and Send by sound, refused under the line at Off and when the sheet's contact
  //    is out of range, with nothing sent and what was written kept; in range it goes and is heard; one that goes
  //    as they leave waits for them, and goes when they are back
  {
    const ctx = await machine(null);
    const p = await front(ctx, 'send');
    const keys = await p.evaluate(() => [...document.querySelectorAll('.compose .keys .key')].map(k => k.textContent.trim()));
    if (JSON.stringify(keys) !== JSON.stringify(['Flash', 'Send by sound', 'Send by Bluetooth'])) fail(`send: the keys are ${JSON.stringify(keys)}`);
    const keyFit = await p.evaluate(() => { const ks = [...document.querySelectorAll('.compose .keys .key')], s = document.querySelector('.sheet').getBoundingClientRect(), line = document.querySelector('.compose .line').getBoundingClientRect();
      return { inside: ks.every(k => { const r = k.getBoundingClientRect(); return r.left >= s.left && r.right <= s.right; }), apart: ks.every((k, i) => !i || k.getBoundingClientRect().left >= ks[i - 1].getBoundingClientRect().right + 4), line: Math.round(line.width) }; });
    if (!keyFit.inside || !keyFit.apart || keyFit.line < 300) fail(`send: the keys crowd the sheet: ${JSON.stringify(keyFit)}`);
    const sends = () => p.evaluate(() => window.__seen.filter(e => e[0] === 'outbox').length);
    const note = () => p.evaluate(() => ({ note: document.getElementById('pubnote').textContent, refused: document.getElementById('pubnote').classList.contains('refused'), kept: document.getElementById('reply').value }));
    const disabled = () => p.evaluate(() => document.getElementById('sendbluetooth').getAttribute('aria-disabled'));
    if (await disabled() !== 'true') fail('send: at Off the key is not disabled for a screen reader');
    await p.fill('#reply', 'By the radio.');
    await p.click('#sendbluetooth', { force: true });
    let n = await note();
    if (n.note !== 'Bluetooth is off. Turn it to Contacts or Open to send this way.' || !n.refused || n.kept !== 'By the radio.' || await sends()) fail(`send: at Off ${JSON.stringify(n)}`);
    await setting(p, 'bluetooth', 1);
    await wait(p, 'Contacts', () => document.querySelector('.mod.bt').dataset.bluetooth === 'contacts only');
    await p.evaluate(() => window.__sim.bluetoothAway(1));
    await wait(p, 'Marisol away', () => !document.querySelector('#sendbluetooth .jewel:not(.off)'));
    await p.click('#sendbluetooth', { force: true });
    n = await note();
    if (n.note !== 'Marisol is not in Bluetooth range.' || n.kept !== 'By the radio.' || await sends()) fail(`send: out of range ${JSON.stringify(n)}`);
    if (await disabled() !== 'true') fail('send: out of range the key is not disabled');
    await p.evaluate(() => window.__sim.bluetoothBack(1));
    await wait(p, 'Marisol back', () => !!document.querySelector('#sendbluetooth .jewel:not(.off)'));
    if (await disabled() !== 'false') fail('send: in range the key is still disabled');
    await p.click('#sendbluetooth', { force: true });
    await wait(p, 'heard by Bluetooth', () => { const e = [...document.querySelectorAll('#log .ent')].pop(); return e && /heard by Marisol/.test(e.textContent); });
    const sent = await p.evaluate(() => { const e = [...document.querySelectorAll('#log .ent')].pop(); return { how: e.querySelector('.when span').textContent, typed: e.querySelector('.typed').textContent }; });
    if (sent.how !== 'bluetooth' || sent.typed !== 'By the radio.') fail(`send: the sheet shows ${JSON.stringify(sent)}`);
    // goes as she leaves: it waits for her, says so, and goes when she is back
    await p.fill('#reply', 'Wait for me.');
    await p.evaluate(() => { document.getElementById('sendbluetooth').click(); window.__sim.bluetoothAway(1); });
    await wait(p, 'waiting for her', () => { const e = [...document.querySelectorAll('#log .ent')].pop(); return e && e.querySelector('.state') && e.querySelector('.state').textContent === 'waiting for Marisol in Bluetooth range'; });
    const slip = await p.evaluate(() => [...document.querySelectorAll('#outbox .slip')].map(s => s.textContent));
    if (!slip.some(t => t === 'Wait for me.sends when Marisol is in Bluetooth range')) fail(`send: the outbox slip reads ${JSON.stringify(slip)}`);
    await p.evaluate(() => window.__sim.bluetoothBack(1));
    await wait(p, 'heard once she is back', () => { const e = [...document.querySelectorAll('#log .ent')].pop(); return e && /heard by Marisol/.test(e.textContent); });
    // a public notice doesn't go by Bluetooth
    await p.click('#mode'); await p.fill('#reply', 'Public.');
    if (await disabled() !== 'true') fail('send: writing a notice, the key is not disabled');
    await p.click('#sendbluetooth', { force: true });
    n = await note();
    if (n.note !== 'A public notice goes by sound or light, not by Bluetooth.' || n.kept !== 'Public.') fail(`send: a notice ${JSON.stringify(n)}`);
    await ctx.close();
  }

  // 4. a request arrives at Open, the key check Off: a slip with the name as they gave it, the key's first four
  //    digits, the time, Accept and Decline; Escape puts back a corrected name; accepted under the corrected name, it
  //    is kept unchecked, its station says not confirmed until confirmed, and the card lays out the number under
  //    their name
  {
    const ctx = await machine({ bluetooth: 2, inperson: 0 });
    const p = await front(ctx, 'accept');
    await p.evaluate(() => window.__sim.bluetoothAsk());
    await wait(p, 'the slip', () => !!document.querySelector('#intray .slip.ask'));
    const [q] = await seen(p, 'request');
    let f = await face(p);
    const fp = q && q.fingerprint.slice(0, 4).toUpperCase();
    if (!f.request || f.request.name !== 'Ines Duarte' || !f.request.small.startsWith(`as they gave it, key ${fp}asks to connect, `) || !/, \d\d\.\d\d$/.test(f.request.small) || !f.request.accept || !f.request.decline || f.request.rule || f.request.label !== 'Ines Duarte asks to connect')
      fail(`accept: the request slip shows ${JSON.stringify(f.request)}`);
    if (f.said !== 'Ines Duarte asks to connect.') fail(`accept: the top bar said ${JSON.stringify(f.said)}`);
    if (!/1 request waits\.$/.test(f.title)) fail(`accept: the tile's title is ${JSON.stringify(f.title)}`);
    const fold = await p.evaluate(() => (document.querySelector('#neartray .slip.near.folded') || {}).textContent);
    if (fold !== 'Strangers: K7MX') fail(`accept: the strangers wait behind the request as ${JSON.stringify(fold)}`);
    // keyboard: the name, then Accept, then Decline
    await p.focus('#intray .askname');
    await p.keyboard.press('End'); await p.keyboard.type('s');
    await p.keyboard.press('Escape');
    if ((await face(p)).request.name !== 'Ines Duarte') fail('accept: Escape did not put back the name as they gave it');
    await p.keyboard.press('Tab');
    if (await p.evaluate(() => document.activeElement.textContent) !== 'Accept') fail('accept: Tab from the name does not reach Accept');
    await p.keyboard.press('Tab');
    if (await p.evaluate(() => document.activeElement.textContent) !== 'Decline') fail('accept: Tab from Accept does not reach Decline');
    await p.fill('#intray .askname', ' ');
    await p.click('#intray .rs.ok');
    await wait(p, 'the empty name refused', () => document.querySelector('#intray .why').textContent === 'Write their name first.');
    await p.fill('#intray .askname', 'Ines from the market');
    await p.press('#intray .askname', 'Enter');
    await wait(p, 'the slip gone', () => !document.querySelector('#intray .slip.ask'));
    f = await face(p);
    const added = (await call(p, 'contacts')).ok.find(c => c.id === 3);
    if (!added || added.name !== 'Ines from the market' || added.checked) fail(`accept: kept ${JSON.stringify(added)}`);
    if (JSON.stringify((await seen(p, 'requestGone')).map(g => g.id)) !== JSON.stringify([q.id])) fail('accept: no requestGone');
    // she was K7MX, so nothing is left to do in the tray: focus goes back to the switch
    await wait(p, 'K7MX off the list', () => !document.querySelector('li[data-label="K7MX"]'));
    const focus = await p.evaluate(() => document.activeElement && document.activeElement.id);
    if (focus !== 'k3') fail(`accept: focus went to ${JSON.stringify(focus)}`);
    const st = f.stations.find(s => s.id === 3);
    if (!st || st.name !== 'Ines from the market' || st.line !== 'not confirmed') fail(`accept: her station shows ${JSON.stringify(f.stations)}`);
    if (f.stations.length !== 3) fail(`accept: the stations lost someone: ${JSON.stringify(f.stations)}`);
    if (f.card.who !== 'Ines, by Bluetooth' || f.card.name !== 'Ines from the market' || f.card.groups !== 12 || f.card.stamp || f.card.label !== 'Key check with Ines from the market, not yet checked' || !/^Ines from the market is added, not yet checked\./.test(f.card.note)) fail(`accept: the card shows ${JSON.stringify(f.card)}`);
    await wait(p, 'confirmed', () => { const s = document.querySelector('.stn[data-contact="3"]'); return s && !s.querySelector('small.nc') && !s.querySelector('.jewel.off'); });
    f = await face(p);
    if (JSON.stringify(f.near) !== JSON.stringify(['02', '00'])) fail(`accept: who is in range reads ${JSON.stringify(f.near)}`);
    if (f.stations.find(s => s.id === 3).line !== 'not heard yet') fail(`accept: once confirmed her line reads ${JSON.stringify(f.stations)}`);
    await ctx.close();
  }

  // 5. declined: the slip leaves, nothing is kept, the next comes, and answering it after says it is gone
  {
    const ctx = await machine({ bluetooth: 2, inperson: 0 });
    const p = await front(ctx, 'decline');
    await p.evaluate(() => { window.__sim.bluetoothAsk(); window.__sim.bluetoothAsk('Tomas Reyes'); });
    await wait(p, 'two requests', () => document.querySelectorAll('#intray .slip').length === 2);
    let f = await face(p);
    if (JSON.stringify(f.more) !== '["and 1 more asking"]') fail(`decline: two requests show ${JSON.stringify(f.more)}`);
    const first = f.request.id;
    await p.click('#intray .rs.no');
    await wait(p, 'the next request', id => { const s = document.querySelectorAll('#intray .slip.ask'); return s.length === 1 && +s[0].dataset.id !== id; }, first);
    f = await face(p);
    if (!f.request || f.request.name !== 'Tomas Reyes' || f.more.length) fail(`decline: the next request shows ${JSON.stringify(f)}`);
    if ((await call(p, 'contacts')).ok.some(c => c.id === 3)) fail('decline: she was kept');
    if (f.said !== 'Declined. Nothing of the request was kept.') fail(`decline: the top bar said ${JSON.stringify(f.said)}`);
    if ((await call(p, 'acceptRequest', first, 'Ines')).err !== 'That request is no longer waiting.') fail('decline: accepting after was not refused');
    await ctx.close();
  }

  // 6. the key check On: accepting is refused by the setting: the node's sentence typed in place of Accept, Decline
  //    beside their name; Enter in the name does nothing; the strangers' slip says why it offers no asking; and if
  //    the setting changes elsewhere, the refusal turns the slip over
  {
    const ctx = await machine({ bluetooth: 2, inperson: 1 });
    const p = await front(ctx, 'held');
    await p.evaluate(() => window.__sim.bluetoothAsk());
    await wait(p, 'the slip', () => !!document.querySelector('#intray .slip.ask'));
    let f = await face(p);
    if (!f.request || f.request.accept || !f.request.decline || f.request.rule !== MEET_FIRST) fail(`held: the slip shows ${JSON.stringify(f.request)}`);
    const folded = await p.evaluate(() => { const s = document.querySelector('#neartray .slip.near.folded'); return s && { text: s.textContent, title: s.title, under: !!s.closest('.col.meter') }; });
    if (!folded || folded.text !== 'K7MX near, not asked' || !/Not asked while new contacts need a key check\.$/.test(folded.title) || !folded.under) fail(`held: the strangers' line ${JSON.stringify(folded)}`);
    const whole = await slipWhole(p);
    if (whole) fail(`held: ${whole}`);
    await p.press('#intray .askname', 'Enter');
    if ((await call(p, 'requests')).ok.length !== 1 || (await call(p, 'contacts')).ok.some(c => c.id === 3)) fail('held: Enter accepted');
    await setting(p, 'inperson', 0);
    await p.evaluate(() => dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
    await wait(p, 'Accept back', () => !document.querySelector('#intray .rs.ok').hidden);
    await setting(p, 'inperson', 1);
    await p.click('#intray .rs.ok');
    await wait(p, 'the slip turned over', () => document.querySelector('#intray .rs.ok').hidden);
    f = await face(p);
    if (f.request.rule !== MEET_FIRST) fail(`held: the refusal did not turn the slip over: ${JSON.stringify(f.request)}`);
    // with no request on show, the strangers' slip has the whole sentence
    await p.click('#intray .rs.no');
    await wait(p, 'the slip gone', () => !document.querySelector('#intray .slip.ask'));
    if ((await face(p)).strangersRule !== MEET_FIRST) fail('held: the strangers slip does not say why there is nothing to ask');
    const small = await smallText(p);
    if (small.length) fail(`held: text under 12px: ${small.join('; ')}`);
    await ctx.close();
  }

  // 7. asking strangers: refused at Contacts in so many words; at Open by label, asking, delivered, not delivered, then
  //    accepted (she is added, and leaves the list once she is no longer offered); Off ends it all
  {
    const ctx = await machine({ bluetooth: 1, inperson: 0 });
    const p = await front(ctx, 'ask');
    if ((await face(p)).strangers) fail('ask: strangers are shown at Contacts');
    if ((await call(p, 'sendRequest', 'K7MX')).err !== NEED_OPEN) fail('ask: asking at Contacts was not refused with the switch sentence');
    await setting(p, 'bluetooth', 2);
    await wait(p, 'Open', () => !!document.querySelector('#neartray li'));
    await p.evaluate(() => window.__sim.bluetoothStrangers(['P2QD', 'W9HT'], ['W9HT']));
    await wait(p, 'three strangers', () => document.querySelectorAll('#neartray li').length === 3);
    let f = await face(p);
    if (JSON.stringify(f.strangers.map(s => s.label)) !== '["K7MX","P2QD","W9HT"]' || f.strangers.some(s => !s.ask)) fail(`ask: strangers ${JSON.stringify(f.strangers)}`);
    if (JSON.stringify(f.near) !== JSON.stringify(['01', '03'])) fail(`ask: who is in range reads ${JSON.stringify(f.near)}`);
    await p.click('li[data-label="W9HT"] .rs');
    await p.focus('li[data-label="K7MX"] .rs'); await p.keyboard.press('Enter');
    if ((await face(p)).strangers.find(s => s.label === 'K7MX').said !== 'asking') fail('ask: asking is not shown at once');
    await wait(p, 'delivered and not', () => { const t = l => (document.querySelector(`li[data-label="${l}"] small`) || {}).textContent; return t('K7MX') === 'asked, delivered' && t('W9HT') === 'not delivered'; });
    await wait(p, 'said she accepted', () => document.getElementById('said').textContent === 'K7MX accepted: Ines Duarte is added.');
    if (JSON.stringify((await seen(p, 'requested')).filter(r => r.label === 'K7MX')) !== JSON.stringify([{ label: 'K7MX', state: 'delivered', contact: null }, { label: 'K7MX', state: 'accepted', contact: 3 }])) fail('ask: requested events');
    await wait(p, 'on the stations', () => !!document.querySelector('.stn[data-contact="3"]'));
    await wait(p, 'off the list', () => !document.querySelector('li[data-label="K7MX"]'));
    // Off ends it all: requests, strangers, who is in range
    await p.evaluate(() => window.__sim.bluetoothAsk('Tomas Reyes'));
    await setting(p, 'bluetooth', 0);
    await wait(p, 'Off', () => document.querySelector('.mod.bt').dataset.bluetooth === 'off' && !document.querySelector('#intray .slip.ask:not(.gone)'));
    f = await face(p);
    if (f.lit || f.near || f.strangers) fail(`ask: Off left ${JSON.stringify(f)}`);
    if ((await call(p, 'requests')).ok.length) fail('ask: a request still waits at Off');
    await ctx.close();
  }

  // 7b. the simulator's own story: the one stranger in range is asked, accepts, and the strangers' slip goes with her
  {
    const ctx = await machine({ bluetooth: 2, inperson: 0 });
    const p = await front(ctx, 'sole stranger');
    await wait(p, 'K7MX offered', () => !!document.querySelector('li[data-label="K7MX"] button'));
    await p.click('li[data-label="K7MX"] .rs');
    await wait(p, 'said she accepted', () => document.getElementById('said').textContent === 'K7MX accepted: Ines Duarte is added.');
    await wait(p, 'the strangers slip gone', () => !document.querySelector('#neartray .slip'));
    await ctx.close();
  }

  // 8. the locked machine: the channel waits for the passphrase, said on the tape under the switch, its jewel dark,
  //    nobody counted; the radio can't work: its reason on the tape, kept within the switch's width; then it works
  //    again, and the top bar says so
  {
    const ctx = await machine({ bluetooth: 2 });
    const p = await front(ctx, 'radio');
    const tapeFit = () => p.evaluate(() => { const b = document.querySelector('#btnote b').getBoundingClientRect(), t = document.querySelector('.mod.bt').getBoundingClientRect(); return b.left >= t.left - 1 && b.right <= t.right + 1; });
    await p.evaluate(() => window.__sim.bluetoothLocked());
    await wait(p, 'locked', () => document.querySelector('.mod.bt').dataset.bluetooth === 'waiting for the passphrase');
    let f = await face(p);
    if (f.lit || f.note !== 'Waits for the passphrase.' || f.top !== 'Bluetooth locked' || f.near || f.strangers) fail(`locked: the face shows ${JSON.stringify(f)}`);
    if ((await call(p, 'channels')).ok.find(c => c.channel === 'bluetooth').state !== 'waiting for the passphrase') fail('locked: channels() does not say it waits');
    if (!await tapeFit()) fail('locked: the tape is wider than the switch');
    await p.evaluate(() => window.__sim.bluetoothUnlocked());
    await wait(p, 'unlocked', () => document.querySelector('.mod.bt').dataset.bluetooth === 'open to requests');
    if ((await face(p)).note) fail('unlocked: the tape stayed');
    await p.evaluate(() => window.__sim.bluetoothUnavailable('Bluetooth is off in System Settings.'));
    await wait(p, 'unavailable', () => /^unavailable/.test(document.querySelector('.mod.bt').dataset.bluetooth));
    f = await face(p);
    if (f.lit || f.note !== 'Bluetooth is off in System Settings.' || f.top !== 'Bluetooth unavailable' || f.title !== 'Bluetooth is off in System Settings.' || f.near) fail(`unavailable: the face shows ${JSON.stringify(f)}`);
    await p.evaluate(() => window.__sim.bluetoothUnavailable('Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.'));
    await wait(p, 'the long reason', () => /^Bombechat/.test(document.querySelector('#btnote b').textContent));
    if (!await tapeFit()) fail('unavailable: the long reason is wider than the switch');
    // a resize keeps it there
    await p.setViewportSize({ width: 1024, height: 768 });
    await wait(p, 'the narrow layout', () => innerWidth === 1024);
    if (!await tapeFit()) fail('unavailable: after a resize the tape is wider than the switch');
    await p.evaluate(() => window.__sim.bluetoothAvailable());
    await wait(p, 'available again', () => document.querySelector('.mod.bt').dataset.bluetooth === 'open to requests');
    f = await face(p);
    if (!f.lit || f.note || f.said !== 'Bluetooth works again: open to requests.') fail(`available again: the face shows ${JSON.stringify(f)}`);
    await ctx.close();
  }

  // 9. every state at each laptop size: 2x crops of the panel as it is on screen; nothing scrolls sideways, nothing is
  //    under 12px, no station's line is cut short; where the panel stands beside the sheet (1280 and up), the deck and
  //    the stations are where they are at Off, whatever the tray holds
  const settle = async (p, what, f) => wait(p, what, f);
  const CROPS = [
    ['off', null, null],
    ['contacts', { bluetooth: 1 }, null],
    ['open', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothStrangers(['P2QD', 'W9HT'])); await settle(p, 'strangers', () => document.querySelectorAll('#neartray li').length === 3); }],
    ['request', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothAsk()); await settle(p, 'the slip', () => !!document.querySelector('#intray .slip.ask')); }],
    ['request-held', { bluetooth: 2, inperson: 1 }, async p => { await p.evaluate(() => window.__sim.bluetoothAsk()); await settle(p, 'the slip', () => !!document.querySelector('#intray .slip.ask')); }],
    ['requests-two', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => { window.__sim.bluetoothAsk(); window.__sim.bluetoothAsk('Tomas Reyes'); }); await settle(p, 'two', () => document.querySelectorAll('#intray .slip').length === 2); }],
    ['accepted', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothAsk()); await settle(p, 'the slip', () => !!document.querySelector('#intray .slip.ask'));
      await p.click('#intray .rs.ok'); await settle(p, 'added', () => !document.querySelector('#intray .slip.ask') && !!document.querySelector('.stn small.nc')); }],
    ['asking', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothStrangers(['P2QD', 'W9HT'], ['W9HT'])); await settle(p, 'strangers', () => document.querySelectorAll('#neartray li').length === 3);
      await p.click('li[data-label="P2QD"] .rs'); await p.click('li[data-label="W9HT"] .rs'); await settle(p, 'answers', () => (document.querySelector('li[data-label="W9HT"] small') || {}).textContent === 'not delivered' && (document.querySelector('li[data-label="P2QD"] small') || {}).textContent === 'asked, delivered'); }],
    ['send', { bluetooth: 1 }, async p => { await p.fill('#reply', 'By the radio.'); await p.click('#sendbluetooth', { force: true }); await settle(p, 'heard', () => { const e = [...document.querySelectorAll('#log .ent')].pop(); return e && /heard by Marisol/.test(e.textContent); }); }],
    ['locked', { bluetooth: 2 }, async p => { await p.evaluate(() => window.__sim.bluetoothLocked()); await settle(p, 'locked', () => !!document.querySelector('#btnote b').textContent); }],
    ['unavailable', { bluetooth: 2 }, async p => { await p.evaluate(() => window.__sim.bluetoothUnavailable('Bluetooth is off in System Settings.')); await settle(p, 'tape', () => !!document.querySelector('#btnote b').textContent); }],
    ['unavailable-long', { bluetooth: 1 }, async p => { await p.evaluate(() => window.__sim.bluetoothUnavailable('Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.')); await settle(p, 'tape', () => !!document.querySelector('#btnote b').textContent); }],
  ];
  for (const [w, h] of [[1024, 768], [1280, 800], [1440, 900], [1728, 1117]]) {
    let atOff = null;
    for (const [name, settings, run] of CROPS) {
      const ctx = await machine(settings);
      const p = await front(ctx, `${name} ${w}`, w, h);
      if (run) await run(p);
      const m = await p.evaluate(() => ({ over: document.documentElement.scrollWidth - innerWidth, deck: Math.round(document.querySelector('.deck').getBoundingClientRect().height), row3: Math.round(document.querySelector('.row3').getBoundingClientRect().top + scrollY),
        cut: [...document.querySelectorAll('.row3 .stn .holder span, .row3 .stn .holder small')].filter(e => e.scrollWidth > e.clientWidth + 1 && getComputedStyle(e).textOverflow !== 'ellipsis').map(e => e.textContent),
        uncut: [...document.querySelectorAll('.row3 .stn .holder small')].filter(e => e.scrollWidth > e.clientWidth + 1 && !e.closest('.holder').title).map(e => e.textContent) }));
      if (m.over > 0) fail(`${name} ${w}: the page scrolls sideways by ${m.over}px`);
      if (m.cut.length || m.uncut.length) fail(`${name} ${w}: a station's line is cut short: ${JSON.stringify(m)}`);
      if (name.startsWith('request')) { const whole = await slipWhole(p); if (whole) fail(`${name} ${w}: ${whole}`); }
      const sliced = await p.evaluate(() => { const out = [];
        for (const col of document.querySelectorAll('.row2 .col')) { const c = col.getBoundingClientRect(); if (!c.height) continue;
          const w = document.createTreeWalker(col, NodeFilter.SHOW_TEXT);
          while (w.nextNode()) { const n = w.currentNode, el = n.parentElement, cs = getComputedStyle(el);
            if (!n.nodeValue.trim() || cs.visibility === 'hidden' || cs.color === 'rgba(0, 0, 0, 0)') continue;
            const range = document.createRange(); range.selectNodeContents(n);
            for (const r of range.getClientRects()) if ((r.top < c.top - .5 && r.bottom > c.top + .5) || (r.top < c.bottom - .5 && r.bottom > c.bottom + .5)) out.push(n.nodeValue.trim().slice(0, 24)); } }
        return out; });
      if (sliced.length) fail(`${name} ${w}: a line of type in the tray is cut through: ${JSON.stringify(sliced)}`);
      const over = await overlaps(p);
      if (over.length) fail(`${name} ${w}: ${over.join('; ')}`);
      if (name === 'off') atOff = m;
      else if (w >= 1280 && (m.deck !== atOff.deck || m.row3 !== atOff.row3)) fail(`${name} ${w}: the deck or the stations moved: ${JSON.stringify({ off: atOff, now: m })}`);
      const small = await smallText(p);
      if (small.length) fail(`${name} ${w}: text under 12px: ${small.join('; ')}`);
      if (w !== 1280) {
        await p.locator('.panel').screenshot({ path: path.join(out, `x2-${w}-${name}.png`) });
        if (['accepted', 'request', 'send'].includes(name)) await p.locator('.deck').screenshot({ path: path.join(out, `x2-${w}-${name}-deck.png`) });
        if (name === 'contacts' || name === 'request') await p.locator('.top').screenshot({ path: path.join(out, `x2-${w}-${name}-top.png`) });
        if (name === 'open') await p.locator('.bankfoot').screenshot({ path: path.join(out, `x2-${w}-${name}-bankfoot.png`) });
      }
      await ctx.close();
    }
  }

  await browser.close();
  server.close();
  console.log(JSON.stringify({ out: path.relative(ROOTDIR, out), problems }, null, 1));
  if (problems.length) process.exit(1);
})().catch(e => { console.error(e); process.exit(1); });
