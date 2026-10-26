// The Bluetooth edition's check of its own parts, in a plain browser over the simulator (npm run check runs it after
// the shared checks):
//   node tools/check-bluetooth.cjs
// Writes checks/app-ui/bluetooth/: 2x crops of the front's panel in every Bluetooth state (off, contacts, open,
// request, request-held, requests-two, accepted, asking, locked, unavailable, unavailable-long) at 1024, 1440 and 1728
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
// not working isn't on the tape under the switch (kept on the panel and off the room meter), or working again isn't said.
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
    await p.waitForTimeout(300);
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
      top: document.getElementById('radio').textContent, near: near.hidden ? null : [...near.querySelectorAll('.odo')].map(o => o.textContent),
      request: slip && { id: +slip.dataset.id, name: slip.querySelector('.askname').value, small: slip.querySelector('small').textContent, accept: !slip.querySelector('.rs.ok').hidden, decline: !slip.querySelector('.rs.no').hidden,
        rule: slip.querySelector('.rule').textContent, why: slip.querySelector('.why').textContent, label: slip.getAttribute('aria-label'), gone: slip.classList.contains('gone') },
      more: [...document.querySelectorAll('#intray .slip.empty')].map(e => e.textContent),
      strangers: ns && [...ns.querySelectorAll('li')].map(li => ({ label: li.dataset.label, ask: !!li.querySelector('button'), said: (li.querySelector('small') || {}).textContent || '' })),
      strangersRule: ns && (ns.querySelector('.rule') || {}).textContent || '',
      stations: [...document.querySelectorAll('.stn[data-contact]')].map(s => ({ id: +s.dataset.contact, name: s.querySelector('.holder span').firstChild.nodeValue, mark: (s.querySelector('.nc') || {}).textContent || '', lit: !s.querySelector('.jewel').classList.contains('off') })),
      card: { who: document.querySelector('.menu h2 span').textContent, name: (document.querySelector('.menu .pfrom') || {}).textContent || '', groups: document.querySelectorAll('.menu .sn li').length, stamp: !!document.querySelector('.menu .gstmp'), note: document.querySelector('.menu .note').textContent },
      said: document.getElementById('said').textContent, blank: !!document.querySelector('.mod .blank, .pl.red') }; });
  // every piece of text in the Bluetooth parts, with the size it is drawn at (the knob's engraving through its scale)
  const smallText = p => p.evaluate(() => { const out = [];
    for (const root of document.querySelectorAll('.mod.bt, #intray, #neartray, #btnear, #radio, .row3')) {
      const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      while (w.nextNode()) { const n = w.currentNode, el = n.parentElement; if (!n.nodeValue.trim() || !el.getBoundingClientRect().width) continue;
        let px = parseFloat(getComputedStyle(el).fontSize); if (el instanceof SVGElement) { const m = el.getScreenCTM(); px = parseFloat(el.getAttribute('font-size')) * Math.hypot(m.a, m.b); }
        if (px < 11.75) out.push(`${n.nodeValue.trim()} ${px.toFixed(1)}px`); } }
    return out; });
  const setting = (p, id, v) => p.evaluate(([id, v]) => window.__bt.setSetting(id, v), [id, v]);
  const shot = async (p, name, w) => { const b = await p.evaluate(() => { const r = document.querySelector('.panel').getBoundingClientRect(); return { x: r.x - 10, y: r.y + scrollY - 10, width: r.width + 20, height: r.height + 20 }; });
    await p.screenshot({ path: path.join(out, `x2-${w}-${name}.png`), clip: b, fullPage: true }); };
  const wait = (p, f, arg) => p.waitForFunction(f, arg, { timeout: 8000 }).catch(() => {});

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
    await p.waitForTimeout(1600);
    for (const to of [2, 0]) {
      const g = await p.evaluate(to => { const s = window.__bc.screen('bluetooth'), r = window.__bc.row('bluetooth'); return { plug: s.plug, jack: window.__bc.at(r.jacks[to], r.y, .3) }; }, to);
      await p.mouse.move(g.plug.x, g.plug.y); await p.mouse.down();
      await p.mouse.move((g.plug.x + g.jack.x) / 2, g.jack.y, { steps: 8 });
      await p.mouse.move(g.jack.x, g.jack.y, { steps: 8 }); await p.mouse.up();
      await p.waitForTimeout(1000);
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
    await p.waitForTimeout(900);
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
      await q.waitForTimeout(1800);
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
      if (v === 0) await p.keyboard.press('Home'); else await p.keyboard.press('ArrowRight');
      await wait(p, w => document.querySelector('.mod.bt').dataset.bluetooth === w, word);
      await p.waitForTimeout(400);
      f = await face(p);
      const kept = (await call(p, 'settings')).ok.bluetooth;
      if (f.knob !== String(v) || kept !== v || f.state !== word || f.top !== top || f.lit !== (v > 0) || !!f.near !== (v > 0)) fail(`switch: at ${v} by keyboard the face shows ${JSON.stringify({ ...f, kept })}`);
      if (v > 0 && JSON.stringify(f.near) !== JSON.stringify(['01', '01'])) fail(`switch: who is in range reads ${JSON.stringify(f.near)}`);
    }
    await p.click('#k3'); await wait(p, () => document.querySelector('.mod.bt').dataset.bluetooth === 'contacts only');
    // (the knob says its new reading once its turn has drawn, 330 ms)
    await wait(p, () => document.getElementById('k3').getAttribute('aria-valuenow') === '1');
    if ((await face(p)).knob !== '1') fail('switch: a click does not turn it to Contacts');
    // Marisol in range: her station's jewel lit (once the stations are the contacts)
    if ((await call(p, 'peers')).ok.contacts[0] !== 1) fail('switch: Marisol not in range');
    // a send by Bluetooth at Off is refused in so many words
    await setting(p, 'bluetooth', 0); await p.waitForTimeout(200);
    await call(p, 'send', [1], 'hi', 'bluetooth'); await p.waitForTimeout(200);
    const s = await said(p);
    if (JSON.stringify(s.slice(-2)) !== JSON.stringify(['not sent: bluetooth is off', 'off'])) fail(`switch: a send at Off said ${JSON.stringify(s)}`);
    if ((await face(p)).state !== 'off') fail('switch: the refusal stuck on the tile');
    const small = await smallText(p);
    if (small.length) fail(`switch: text under 12px: ${small.join('; ')}`);
    await ctx.close();
  }

  // 3. a request arrives at Open, the key check Off: a slip with the name as they gave it, the key's first four
  //    digits, the time, Accept and Decline; Escape puts back a corrected name; accepted under the corrected name, it
  //    is kept unchecked, marked not confirmed on its station until confirmed, and the card lays out the number
  {
    const ctx = await machine({ bluetooth: 2, inperson: 0 });
    const p = await front(ctx, 'accept');
    await p.evaluate(() => window.__sim.bluetoothAsk()); await p.waitForTimeout(150);
    const [q] = await seen(p, 'request');
    let f = await face(p);
    const fp = q && q.fingerprint.slice(0, 4).toUpperCase();
    if (!f.request || f.request.name !== 'Ines Duarte' || !f.request.small.startsWith(`name as they gave it, key ${fp}, `) || !/, \d\d\.\d\d$/.test(f.request.small) || !f.request.accept || !f.request.decline || f.request.rule || f.request.label !== 'Ines Duarte asks to connect')
      fail(`accept: the request slip shows ${JSON.stringify(f.request)}`);
    if (f.said !== 'Ines Duarte asks to connect.') fail(`accept: the top bar said ${JSON.stringify(f.said)}`);
    if (!/1 request waits\.$/.test(f.title)) fail(`accept: the tile's title is ${JSON.stringify(f.title)}`);
    if (JSON.stringify(f.strangers) !== JSON.stringify([{ label: 'K7MX', ask: true, said: '' }])) fail(`accept: strangers ${JSON.stringify(f.strangers)}`);
    // keyboard: the name, then Accept, then Decline
    await p.focus('#intray .askname');
    await p.keyboard.press('End'); await p.keyboard.type('s');
    await p.keyboard.press('Escape');
    if ((await face(p)).request.name !== 'Ines Duarte') fail('accept: Escape did not put back the name as they gave it');
    await p.keyboard.press('Tab');
    if (await p.evaluate(() => document.activeElement.textContent) !== 'Accept') fail('accept: Tab from the name does not reach Accept');
    await p.keyboard.press('Tab');
    if (await p.evaluate(() => document.activeElement.textContent) !== 'Decline') fail('accept: Tab from Accept does not reach Decline');
    // an empty name is refused on the slip
    await p.fill('#intray .askname', ' ');
    await p.click('#intray .rs.ok'); await p.waitForTimeout(100);
    if ((await face(p)).request.why !== 'Write their name first.') fail('accept: an empty name was not refused on the slip');
    await p.fill('#intray .askname', 'Ines from the market');
    await p.press('#intray .askname', 'Enter');
    await wait(p, () => !document.querySelector('#intray .slip.ask:not(.gone)'));
    await p.waitForTimeout(600);
    f = await face(p);
    const added = (await call(p, 'contacts')).ok.find(c => c.id === 3);
    if (!added || added.name !== 'Ines from the market' || added.checked) fail(`accept: kept ${JSON.stringify(added)}`);
    if (JSON.stringify((await seen(p, 'requestGone')).map(g => g.id)) !== JSON.stringify([q.id])) fail('accept: no requestGone');
    if (f.request) fail('accept: the slip stayed');
    const st = f.stations.find(s => s.id === 3);
    if (!st || st.name !== 'Ines from the market' || st.mark !== 'not confirmed, ') fail(`accept: her station shows ${JSON.stringify(f.stations)}`);
    if (f.card.who !== 'by Bluetooth' || f.card.name !== 'Ines from the market' || f.card.groups !== 12 || f.card.stamp || !/^Added, not yet checked\./.test(f.card.note)) fail(`accept: the card shows ${JSON.stringify(f.card)}`);
    if (await p.evaluate(() => document.activeElement && document.activeElement.id) !== 'k3' && !(await p.evaluate(() => !!document.activeElement.closest('#neartray')))) fail('accept: focus was lost when the slip left');
    await p.waitForTimeout(1500);
    f = await face(p);
    const st2 = f.stations.find(s => s.id === 3);
    if (!st2 || st2.mark || !st2.lit) fail(`accept: once confirmed her station shows ${JSON.stringify(st2)}`);
    if (JSON.stringify(f.near) !== JSON.stringify(['02', '00'])) fail(`accept: who is in range reads ${JSON.stringify(f.near)}`);
    await ctx.close();
  }

  // 4. declined: the slip leaves, nothing is kept, and answering it after says it is gone
  {
    const ctx = await machine({ bluetooth: 2, inperson: 0 });
    const p = await front(ctx, 'decline');
    await p.evaluate(() => { window.__sim.bluetoothAsk(); window.__sim.bluetoothAsk('Tomas Reyes'); }); await p.waitForTimeout(150);
    let f = await face(p);
    if (JSON.stringify(f.more) !== '["and 1 more asking"]') fail(`decline: two requests show ${JSON.stringify(f.more)}`);
    const first = f.request.id;
    await p.click('#intray .rs.no');
    await wait(p, id => { const s = document.querySelector('#intray .slip.ask:not(.gone)'); return s && +s.dataset.id !== id; }, first);
    await p.waitForTimeout(600);
    f = await face(p);
    if (!f.request || f.request.name !== 'Tomas Reyes' || f.more.length) fail(`decline: the next request shows ${JSON.stringify(f)}`);
    if ((await call(p, 'contacts')).ok.some(c => c.id === 3)) fail('decline: she was kept');
    if (f.said !== 'Declined. Nothing of the request was kept.') fail(`decline: the top bar said ${JSON.stringify(f.said)}`);
    if ((await call(p, 'acceptRequest', first, 'Ines')).err !== 'That request is no longer waiting.') fail('decline: accepting after was not refused');
    await ctx.close();
  }

  // 5. the key check On: accepting is refused by the setting: the node's sentence typed in place of Accept, Decline
  //    kept; Enter in the name does nothing; and if the setting changes elsewhere the refusal turns the slip over
  {
    const ctx = await machine({ bluetooth: 2, inperson: 1 });
    const p = await front(ctx, 'held');
    await p.evaluate(() => window.__sim.bluetoothAsk()); await p.waitForTimeout(150);
    let f = await face(p);
    if (!f.request || f.request.accept || !f.request.decline || f.request.rule !== MEET_FIRST) fail(`held: the slip shows ${JSON.stringify(f.request)}`);
    if (JSON.stringify(f.strangers) !== JSON.stringify([{ label: 'K7MX', ask: false, said: '' }]) || f.strangersRule) fail(`held: strangers ${JSON.stringify([f.strangers, f.strangersRule])}`);
    await p.press('#intray .askname', 'Enter'); await p.waitForTimeout(200);
    if ((await call(p, 'requests')).ok.length !== 1 || (await call(p, 'contacts')).ok.some(c => c.id === 3)) fail('held: Enter accepted');
    // the setting turned Off and On again behind the page's back: Accept shows, is refused, and the slip turns over
    await setting(p, 'inperson', 0);
    await p.evaluate(() => dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }))); await p.waitForTimeout(200);
    if (!(await face(p)).request.accept) fail('held: the key check Off again does not bring Accept back');
    await setting(p, 'inperson', 1);
    await p.click('#intray .rs.ok'); await p.waitForTimeout(200);
    f = await face(p);
    if (f.request.accept || f.request.rule !== MEET_FIRST) fail(`held: the refusal did not turn the slip over: ${JSON.stringify(f.request)}`);
    // with no request on show, the strangers' slip says it once
    await p.click('#intray .rs.no'); await p.waitForTimeout(700);
    if ((await face(p)).strangersRule !== MEET_FIRST) fail('held: the strangers slip does not say why there is nothing to ask');
    const small = await smallText(p);
    if (small.length) fail(`held: text under 12px: ${small.join('; ')}`);
    await ctx.close();
  }

  // 6. asking strangers: refused at Contacts in so many words; at Open by label, delivered then accepted (she is
  //    added); one not delivered says so
  {
    const ctx = await machine({ bluetooth: 1, inperson: 0 });
    const p = await front(ctx, 'ask');
    if ((await face(p)).strangers) fail('ask: strangers are shown at Contacts');
    if ((await call(p, 'sendRequest', 'K7MX')).err !== NEED_OPEN) fail('ask: asking at Contacts was not refused with the switch sentence');
    await setting(p, 'bluetooth', 2); await p.waitForTimeout(300);
    await p.evaluate(() => window.__sim.bluetoothStrangers(['P2QD', 'W9HT'], ['W9HT'])); await p.waitForTimeout(200);
    let f = await face(p);
    if (JSON.stringify(f.strangers.map(s => s.label)) !== '["K7MX","P2QD","W9HT"]' || f.strangers.some(s => !s.ask)) fail(`ask: strangers ${JSON.stringify(f.strangers)}`);
    if (JSON.stringify(f.near) !== JSON.stringify(['01', '03'])) fail(`ask: who is in range reads ${JSON.stringify(f.near)}`);
    await p.click('li[data-label="W9HT"] .rs');
    await p.focus('li[data-label="K7MX"] .rs'); await p.keyboard.press('Enter');
    await p.waitForTimeout(150);
    f = await face(p);
    if (f.strangers.find(s => s.label === 'K7MX').said !== 'asking') fail(`ask: asking shows ${JSON.stringify(f.strangers)}`);
    await p.waitForTimeout(700);
    f = await face(p);
    if (f.strangers.find(s => s.label === 'K7MX').said !== 'asked, delivered' || f.strangers.find(s => s.label === 'W9HT').said !== 'not delivered') fail(`ask: delivered shows ${JSON.stringify(f.strangers)}`);
    await p.waitForTimeout(2200);
    f = await face(p);
    if (f.strangers.find(s => s.label === 'K7MX').said !== 'accepted, added') fail(`ask: accepted shows ${JSON.stringify(f.strangers)}`);
    if (JSON.stringify((await seen(p, 'requested')).filter(r => r.label === 'K7MX')) !== JSON.stringify([{ label: 'K7MX', state: 'delivered', contact: null }, { label: 'K7MX', state: 'accepted', contact: 3 }])) fail('ask: requested events');
    if (!f.stations.some(s => s.id === 3 && s.name === 'Ines Duarte' && !s.mark)) fail(`ask: she is not on the stations: ${JSON.stringify(f.stations)}`);
    // Off ends it all: requests, strangers, who is in range
    await p.evaluate(() => window.__sim.bluetoothAsk('Tomas Reyes'));
    await setting(p, 'bluetooth', 0); await p.waitForTimeout(800);
    f = await face(p);
    if (f.lit || f.near || f.request || f.strangers) fail(`ask: Off left ${JSON.stringify(f)}`);
    if ((await call(p, 'requests')).ok.length) fail('ask: a request still waits at Off');
    await ctx.close();
  }

  // 7. the locked machine: the channel waits for the passphrase; the tile says so on its tape, its jewel dark, nobody
  //    counted; the radio can't work: its reason on the tape, then it works again, and the top bar says so
  {
    const ctx = await machine({ bluetooth: 2 });
    const p = await front(ctx, 'radio');
    await p.evaluate(() => window.__sim.bluetoothLocked()); await p.waitForTimeout(150);
    let f = await face(p);
    if (f.state !== 'waiting for the passphrase' || f.lit || f.note !== 'Waits for the passphrase.' || f.top !== 'Bluetooth locked' || f.near || f.strangers) fail(`locked: the face shows ${JSON.stringify(f)}`);
    if ((await call(p, 'channels')).ok.find(c => c.channel === 'bluetooth').state !== 'waiting for the passphrase') fail('locked: channels() does not say it waits');
    await p.evaluate(() => window.__sim.bluetoothUnlocked()); await p.waitForTimeout(150);
    f = await face(p);
    if (f.state !== 'open to requests' || !f.lit || f.note) fail(`unlocked: the face shows ${JSON.stringify(f)}`);
    await p.evaluate(() => window.__sim.bluetoothUnavailable('Bluetooth is off in System Settings.')); await p.waitForTimeout(150);
    f = await face(p);
    if (f.state !== 'unavailable: Bluetooth is off in System Settings.' || f.lit || f.note !== 'Bluetooth is off in System Settings.' || f.top !== 'Bluetooth unavailable' || f.title !== 'Bluetooth is off in System Settings.' || f.near) fail(`unavailable: the face shows ${JSON.stringify(f)}`);
    const tape = await p.evaluate(() => { const b = document.querySelector('#btnote b').getBoundingClientRect(), panel = document.querySelector('.panel').getBoundingClientRect(); return { right: b.right, edge: panel.right + 4 }; });
    if (tape.right > tape.edge) fail(`unavailable: the tape runs off the panel: ${JSON.stringify(tape)}`);
    await p.evaluate(() => window.__sim.bluetoothUnavailable('Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.')); await p.waitForTimeout(150);
    const two = await p.evaluate(() => { const b = document.querySelector('#btnote b').getBoundingClientRect(), vu = document.querySelector('.vu').getBoundingClientRect(); return { bottom: b.bottom, vu: vu.top }; });
    if (two.bottom > two.vu + 10) fail(`unavailable: a long reason covers the room meter: ${JSON.stringify(two)}`);
    await p.evaluate(() => window.__sim.bluetoothAvailable()); await p.waitForTimeout(150);
    f = await face(p);
    if (f.state !== 'open to requests' || !f.lit || f.note || f.said !== 'Bluetooth works again: open to requests.') fail(`available again: the face shows ${JSON.stringify(f)}`);
    await ctx.close();
  }

  // 8. every state at each laptop size, 2x crops of the panel; nothing scrolls sideways and nothing is under 12px
  const CROPS = [
    ['off', null, null],
    ['contacts', { bluetooth: 1 }, null],
    ['open', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothStrangers(['P2QD', 'W9HT'])); }],
    ['request', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothAsk()); }],
    ['request-held', { bluetooth: 2, inperson: 1 }, async p => { await p.evaluate(() => window.__sim.bluetoothAsk()); }],
    ['requests-two', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => { window.__sim.bluetoothAsk(); window.__sim.bluetoothAsk('Tomas Reyes'); }); }],
    ['accepted', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothAsk()); await p.waitForTimeout(150); await p.click('#intray .rs.ok'); await p.waitForTimeout(700); }],
    ['asking', { bluetooth: 2, inperson: 0 }, async p => { await p.evaluate(() => window.__sim.bluetoothStrangers(['P2QD', 'W9HT'], ['W9HT'])); await p.waitForTimeout(150);
      await p.click('li[data-label="P2QD"] .rs'); await p.click('li[data-label="W9HT"] .rs'); await p.waitForTimeout(900); }],
    ['locked', { bluetooth: 2 }, async p => { await p.evaluate(() => window.__sim.bluetoothLocked()); }],
    ['unavailable', { bluetooth: 2 }, async p => { await p.evaluate(() => window.__sim.bluetoothUnavailable('Bluetooth is off in System Settings.')); }],
    ['unavailable-long', { bluetooth: 1 }, async p => { await p.evaluate(() => window.__sim.bluetoothUnavailable('Bombechat may not use Bluetooth. Allow it in System Settings, Privacy and Security, Bluetooth.')); }],
  ];
  for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) for (const [name, settings, run] of CROPS) {
    const ctx = await machine(settings);
    const p = await front(ctx, `${name} ${w}`, w, h);
    if (run) await run(p);
    await p.waitForTimeout(250);
    // (the top bar's passing sentence aside: at 1024 it runs past the window on every build, as the root's does)
    const over = await p.evaluate(() => { const s = document.getElementById('said'), t = s.textContent; s.textContent = ''; const o = document.documentElement.scrollWidth - innerWidth; s.textContent = t; return o; });
    if (over > 0) fail(`${name} ${w}: the page scrolls sideways by ${over}px`);
    const small = await smallText(p);
    if (small.length) fail(`${name} ${w}: text under 12px: ${small.join('; ')}`);
    await shot(p, name, w);
    if (name === 'accepted' || name === 'request') await p.screenshot({ path: path.join(out, `x2-${w}-${name}-deck.png`), clip: await p.evaluate(() => { const r = document.querySelector('.deck').getBoundingClientRect(); return { x: r.x, y: r.y + scrollY, width: r.width, height: r.height }; }), fullPage: true });
    if (name === 'contacts' || name === 'request') await p.screenshot({ path: path.join(out, `x2-${w}-${name}-top.png`), clip: await p.evaluate(() => { const r = document.querySelector('.top').getBoundingClientRect(); return { x: 0, y: 0, width: r.width, height: r.height }; }) });
    if (name === 'open') await p.screenshot({ path: path.join(out, `x2-${w}-${name}-bankfoot.png`), clip: await p.evaluate(() => { const r = document.querySelector('.bankfoot').getBoundingClientRect(); return { x: r.x, y: r.y + scrollY, width: r.width, height: r.height }; }), fullPage: true });
    await ctx.close();
  }

  await browser.close();
  server.close();
  console.log(JSON.stringify({ out: path.relative(ROOTDIR, out), problems }, null, 1));
  if (problems.length) process.exit(1);
})().catch(e => { console.error(e); process.exit(1); });
