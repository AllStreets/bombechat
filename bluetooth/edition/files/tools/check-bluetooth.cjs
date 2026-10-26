// The Bluetooth edition's check of its own parts, in a plain browser over the simulator (npm run check runs it after
// the shared checks):
//   node tools/check-bluetooth.cjs
// Writes checks/app-ui/bluetooth/: 2x crops of the front's Bluetooth tile at each switch position and the settings
// card's Bluetooth row at 1024, 1440 and 1728.
// Fails on page errors, a request to another host, and when: the settings card has no live Bluetooth row (Off,
// Contacts, Open; Off by default, kept across pages, its plug dragged on the machine landing where it is let go) or
// still shows the blanking plate; New contacts need a key check
// is held instead of live; the Bluetooth channel doesn't say the switch's word ('off', 'contacts only', 'open to
// requests'), or 'unavailable: ' and why, and its own word again once the radio works; the front's Bluetooth tile
// doesn't follow (its jewel lit only while the radio works, its title saying the state, who is near and the requests
// waiting); Marisol isn't counted in range or the stranger isn't offered at Open; the stranger's request doesn't arrive
// as a request event and in requests(); accepting is not refused with the key check's sentence while it is On, or
// doesn't add her once it is Off (requestGone, unconfirmed, then contactConfirmed and in range); declining doesn't
// forget the request and keep nothing; asking isn't refused at Contacts with the switch's sentence, or doesn't go by
// label at Open (requested delivered, then accepted with her id); a send by Bluetooth at Off isn't refused in so many
// words; or the switch at Off doesn't end requests and empty who is near.
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
  const tile = p => p.evaluate(() => { const t = document.querySelector('.mod .blank').closest('.mod'), j = t.querySelector('.jewel');
    return { state: t.dataset.bluetooth, title: t.title, lit: !j.classList.contains('off'), opacity: j.style.opacity }; });
  const setting = (p, id, v) => p.evaluate(([id, v]) => window.__bt.setSetting(id, v), [id, v]);
  const crop = async (p, name) => { const b = await p.locator('.mod .blank').evaluate(e => { const r = e.closest('.mod').getBoundingClientRect(); return { x: r.x - 12, y: r.y - 12, width: r.width + 24, height: r.height + 24 }; });
    await p.screenshot({ path: path.join(out, name), clip: b }); };

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
        blank: !!document.querySelector('.notfitted'), small: [...document.querySelectorAll('.grp')].find(g => g.firstChild.textContent === 'Bluetooth').querySelector('small').textContent }; });
    if (!row) fail('settings: no Bluetooth row');
    else {
      if (JSON.stringify(row.jacks) !== JSON.stringify(['Off', 'Contacts', 'Open'])) fail(`settings: the Bluetooth jacks are ${JSON.stringify(row.jacks)}`);
      if (row.on !== 0) fail(`settings: Bluetooth starts at ${row.on}, not Off`);
      if (row.disabled) fail('settings: the Bluetooth row is taped');
      if (row.blank) fail('settings: the blanking plate is still there');
      if (row.small !== '1 plug') fail(`settings: the Bluetooth group says ${JSON.stringify(row.small)}`);
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

  // 2. Off: the channel and the tile say so; a send by Bluetooth is refused in so many words; nobody near
  {
    const ctx = await machine(null);
    const p = await front(ctx, 'off');
    const ch = (await call(p, 'channels')).ok.find(c => c.channel === 'bluetooth');
    if (!ch || ch.state !== 'off' || 'rate' in ch) fail(`off: channels() says ${JSON.stringify(ch)}`);
    const t = await tile(p);
    if (t.state !== 'off' || t.lit || t.opacity !== '0.25' || t.title !== 'Bluetooth: off.') fail(`off: the tile shows ${JSON.stringify(t)}`);
    await crop(p, 'x2-1440-tile-off.png');
    const peers = (await call(p, 'peers')).ok;
    if (JSON.stringify(peers) !== JSON.stringify({ contacts: [], strangers: 0 })) fail(`off: peers ${JSON.stringify(peers)}`);
    if ((await call(p, 'strangers')).ok.length) fail('off: a stranger is offered');
    await call(p, 'send', [1], 'hi', 'bluetooth');
    await p.waitForTimeout(200);
    const s = await said(p);
    if (JSON.stringify(s.slice(-2)) !== JSON.stringify(['not sent: bluetooth is off', 'off'])) fail(`off: a send said ${JSON.stringify(s)}`);
    if (!(await seen(p, 'outbox')).some(o => o.state === 'failed')) fail('off: the send did not fail');
    await ctx.close();
  }

  // 3. Contacts: Marisol in range, no stranger offered, asking refused with the switch's sentence
  {
    const ctx = await machine({ bluetooth: 1, inperson: 0 });
    const p = await front(ctx, 'contacts');
    const t = await tile(p);
    if (t.state !== 'contacts only' || !t.lit || t.opacity !== '' || t.title !== 'Bluetooth: contacts only. 1 contact in range, 1 stranger seen lately.') fail(`contacts: the tile shows ${JSON.stringify(t)}`);
    await crop(p, 'x2-1440-tile-contacts.png');
    const peers = (await call(p, 'peers')).ok;
    if (JSON.stringify(peers.contacts) !== '[1]') fail(`contacts: Marisol not in range: ${JSON.stringify(peers)}`);
    if ((await call(p, 'strangers')).ok.length) fail('contacts: a stranger is offered');
    const r = await call(p, 'sendRequest', 'K7MX');
    if (r.err !== NEED_OPEN) fail(`contacts: asking said ${JSON.stringify(r)}`);
    await ctx.close();
  }

  // 4. Open, key check On: the stranger asks; accepting is refused in so many words, and the request stays; then Off
  //    the key check and she is added, unconfirmed until her own answer, then in range
  {
    const ctx = await machine({ bluetooth: 2 });
    const p = await front(ctx, 'open');
    if (JSON.stringify((await call(p, 'strangers')).ok) !== '["K7MX"]') fail('open: the stranger is not offered');
    if ((await call(p, 'peers')).ok.strangers !== 1) fail('open: the stranger is not counted');
    await p.evaluate(() => window.__sim.bluetoothAsk());
    await p.waitForTimeout(100);
    const asked = await seen(p, 'request');
    if (asked.length !== 1 || asked[0].name !== 'Ines Duarte' || !/^[0-9a-f]{16}$/.test(asked[0].fingerprint) || !/^(\d{5} ){11}\d{5}$/.test(asked[0].safety)) fail(`open: the request came as ${JSON.stringify(asked)}`);
    const listed = (await call(p, 'requests')).ok;
    if (JSON.stringify(listed) !== JSON.stringify(asked)) fail(`open: requests() lists ${JSON.stringify(listed)}`);
    const t = await tile(p);
    if (t.title !== 'Bluetooth: open to requests. 1 contact in range, 1 stranger seen lately. 1 request waits.') fail(`open: the tile's title is ${JSON.stringify(t.title)}`);
    await crop(p, 'x2-1440-tile-open.png');
    const refused = await call(p, 'acceptRequest', asked[0].id, 'Ines');
    if (refused.err !== MEET_FIRST) fail(`open: accepting with the key check on said ${JSON.stringify(refused)}`);
    if ((await call(p, 'requests')).ok.length !== 1) fail('open: the refused request did not stay');
    const ask = await call(p, 'sendRequest', 'K7MX');
    if (ask.err !== MEET_FIRST) fail(`open: asking with the key check on said ${JSON.stringify(ask)}`);
    await setting(p, 'inperson', 0);
    const id = (await call(p, 'acceptRequest', asked[0].id, ' Ines from the market ')).ok;
    if (id !== 3) fail(`open: accepting gave ${JSON.stringify(id)}`);
    const right = await p.evaluate(() => window.__seen.filter(e => e[0] === 'requestGone').map(e => e[1].id));
    if (JSON.stringify(right) !== JSON.stringify([asked[0].id])) fail(`open: requestGone ${JSON.stringify(right)}`);
    if (JSON.stringify((await call(p, 'unconfirmed')).ok) !== '[3]') fail('open: the accept is not unconfirmed at first');
    const added = (await call(p, 'contacts')).ok.find(c => c.id === 3);
    if (!added || added.name !== 'Ines from the market' || added.checked) fail(`open: the contact kept is ${JSON.stringify(added)}`);
    await p.waitForTimeout(1500);
    if (JSON.stringify(await seen(p, 'contactConfirmed')) !== JSON.stringify([{ contact: 3 }])) fail('open: not confirmed once');
    if ((await call(p, 'unconfirmed')).ok.length) fail('open: still unconfirmed');
    const near = (await call(p, 'peers')).ok;
    if (JSON.stringify(near) !== JSON.stringify({ contacts: [1, 3], strangers: 0 })) fail(`open: who is near after: ${JSON.stringify(near)}`);
    if ((await call(p, 'strangers')).ok.length) fail('open: Ines is still a stranger');
    // the radio can't work, then works again
    await p.evaluate(() => window.__sim.bluetoothUnavailable('Bluetooth is off in System Settings.'));
    await p.waitForTimeout(100);
    const down = await tile(p);
    if (down.state !== 'unavailable: Bluetooth is off in System Settings.' || down.lit || down.title !== 'Bluetooth is off in System Settings.') fail(`open: unavailable shows ${JSON.stringify(down)}`);
    if ((await call(p, 'channels')).ok.find(c => c.channel === 'bluetooth').state !== down.state) fail('open: channels() does not say it is unavailable');
    await crop(p, 'x2-1440-tile-unavailable.png');
    await p.evaluate(() => window.__sim.bluetoothAvailable());
    await p.waitForTimeout(100);
    const up = await tile(p);
    if (up.state !== 'open to requests' || !up.lit) fail(`open: available again shows ${JSON.stringify(up)}`);
    await ctx.close();
  }

  // 5. Open: a decline forgets the request and keeps nothing; asking goes by label and she accepts; Off ends it all
  {
    const ctx = await machine({ bluetooth: 2, inperson: 0 });
    const p = await front(ctx, 'decline');
    await p.evaluate(() => window.__sim.bluetoothAsk());
    await p.waitForTimeout(100);
    const [q] = await seen(p, 'request');
    await call(p, 'declineRequest', q.id);
    await p.waitForTimeout(100);
    if (JSON.stringify((await seen(p, 'requestGone')).map(g => g.id)) !== JSON.stringify([q.id])) fail('decline: no requestGone');
    if ((await call(p, 'requests')).ok.length) fail('decline: the request is still listed');
    if ((await call(p, 'contacts')).ok.some(c => c.id === 3)) fail('decline: she was kept');
    const gone = await call(p, 'acceptRequest', q.id, 'Ines');
    if (gone.err !== 'That request is no longer waiting.') fail(`decline: accepting after said ${JSON.stringify(gone)}`);
    const nope = await call(p, 'sendRequest', 'AAAA');
    if (nope.err !== 'They are no longer in range.') fail(`ask: an unknown label said ${JSON.stringify(nope)}`);
    await call(p, 'sendRequest', 'K7MX');
    await p.waitForTimeout(2800);
    const how = await seen(p, 'requested');
    if (JSON.stringify(how) !== JSON.stringify([{ label: 'K7MX', state: 'delivered', contact: null }, { label: 'K7MX', state: 'accepted', contact: 3 }])) fail(`ask: requested ${JSON.stringify(how)}`);
    if (!(await call(p, 'contacts')).ok.some(c => c.id === 3)) fail('ask: she was not added');
    // a second stranger's request waits, then Off ends it and empties who is near
    await p.evaluate(() => window.__sim.bluetoothAsk());
    await setting(p, 'bluetooth', 0);
    await p.waitForTimeout(200);
    const t = await tile(p);
    if (t.state !== 'off' || t.lit) fail(`off again: the tile shows ${JSON.stringify(t)}`);
    if (JSON.stringify((await call(p, 'peers')).ok) !== JSON.stringify({ contacts: [], strangers: 0 })) fail('off again: someone is near');
    if ((await call(p, 'requests')).ok.length) fail('off again: a request still waits');
    await ctx.close();
  }

  // 6. the tile at each laptop size, Open with someone near
  for (const [w, h] of [[1024, 768], [1440, 900], [1728, 1117]]) {
    const ctx = await machine({ bluetooth: 2 });
    const p = await front(ctx, `tile ${w}`, w, h);
    await crop(p, `x2-${w}-tile-open.png`);
    await ctx.close();
  }

  await browser.close();
  server.close();
  console.log(JSON.stringify({ out: path.relative(ROOTDIR, out), problems }, null, 1));
  if (problems.length) process.exit(1);
})().catch(e => { console.error(e); process.exit(1); });
