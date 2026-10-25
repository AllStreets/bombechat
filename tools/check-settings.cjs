// Quality check for the settings page (the back of the machine). Run after every change:
//   node tools/check-settings.cjs                         the app's interface, app/ui/settings.html
//   node tools/check-settings.cjs mockups/settings.html   the frozen design reference
// Writes checks/<folder>/settings/: overview, focused rows, mid-move frames, narrow layout, 2x details.
// Fails on page errors, a plug that does not end in the jack its setting names, or a slow frame rate,
// and for the app's interface on any request that leaves this machine.
const path = require('path'), fs = require('fs');
const { chromium } = require('playwright');
const ROOTDIR = path.resolve(__dirname, '..');
const page = path.resolve(process.argv[2] || 'app/ui/settings.html');
const APP = !/[\\/]mockups[\\/]/.test(page);
const out = path.join(ROOTDIR, 'checks', path.relative(ROOTDIR, path.dirname(page)).replace(/[\\/]/g, '-'), 'settings');
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const { start } = require('./serve.cjs');
const url = q => 'http://127.0.0.1:4811/' + path.relative(ROOTDIR, page) + (q ? '?' + q : '');
(async () => {
  const server = await start(4811);
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
  const problems = [];
  const open = async (vp, q, dpr = 1) => {
    const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: dpr });
    await ctx.addInitScript(() => { try { localStorage.clear() } catch (e) {} });
    const p = await ctx.newPage();
    p.on('pageerror', e => problems.push('page error: ' + e.message));
    p.on('console', m => m.type() === 'error' && problems.push('console: ' + m.text()));
    if (APP) p.on('request', r => { const u = r.url(); if (!/^(data:|blob:|http:\/\/127\.0\.0\.1:4811\/)/.test(u)) problems.push('asked the network for ' + u); });
    await p.goto(url(q), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await p.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    await p.waitForTimeout(600);
    return p;
  };
  const vp = { width: 1440, height: 900 };
  const p = await open(vp);
  await p.screenshot({ path: path.join(out, 'overview.png') });
  // frame rate over 2 s
  const fps = await p.evaluate(() => new Promise(r => { let n = 0; const t0 = performance.now(); const f = () => { n++; performance.now() - t0 < 2000 ? requestAnimationFrame(f) : r(Math.round(n / 2)) }; requestAnimationFrame(f) }));
  if (fps < 30) problems.push('frame rate ' + fps);
  // focus a row by hovering it on the card
  await p.hover('.row[data-id="hops"]');
  await p.waitForTimeout(1600);
  await p.screenshot({ path: path.join(out, 'focus-hops.png') });
  // change it and catch the move in flight
  await p.click('.row[data-id="hops"] .jk[data-i="6"]');
  await p.waitForTimeout(330);
  await p.screenshot({ path: path.join(out, 'move-hops-midair.png') });
  await p.waitForTimeout(900);
  await p.screenshot({ path: path.join(out, 'move-hops-seated.png') });
  // drag a plug on the machine itself: hops plug from its jack to the first jack
  const drag = await p.evaluate(() => window.__bc.screen('hops'));
  await p.mouse.move(drag.plug.x, drag.plug.y); await p.mouse.down();
  await p.mouse.move((drag.plug.x + drag.first.x) / 2, drag.first.y, { steps: 8 });
  await p.mouse.move(drag.first.x, drag.first.y, { steps: 8 }); await p.mouse.up();
  await p.waitForTimeout(900);
  const dragged = await p.evaluate(() => window.__bc.get('hops'));
  if (dragged !== 0) problems.push('dragging the hops plug to jack 1 left it at ' + dragged);
  // back to the full view: click the machine away from any plug, then Escape from a hover pin
  const vb = await p.locator('#view').boundingBox();
  await p.mouse.click(vb.x + 30, vb.y + vb.height * .25);
  await p.waitForTimeout(1400);
  const afterClick = await p.evaluate(() => window.__bc.focused());
  if (afterClick) problems.push('clicking the machine did not return to the full view, still on ' + afterClick);
  await p.screenshot({ path: path.join(out, 'back-to-full.png') });
  await p.click('.row[data-id="volume"] .jk[data-i="4"]');
  await p.waitForTimeout(900);
  const plateShown = await p.isVisible('#fullview');
  if (!plateShown) problems.push('Full view plate not shown while zoomed');
  await p.mouse.move(vb.x + vb.width / 2, vb.y + 5);
  await p.keyboard.press('Escape');
  await p.waitForTimeout(300);
  if (await p.evaluate(() => window.__bc.focused())) problems.push('Escape did not return to the full view');
  await p.click('.row[data-id="volume"] .jk[data-i="2"]');
  await p.waitForTimeout(900);
  await p.click('#fullview');
  await p.waitForTimeout(300);
  if (await p.evaluate(() => window.__bc.focused())) problems.push('Full view plate did not return to the full view');
  // every setting: set each value and confirm the plug lands in that jack
  const ids = await p.evaluate(() => window.__bc.settings);
  const results = {};
  for (const id of ids) {
    const n = await p.evaluate(id => document.querySelectorAll(`.row[data-id="${id}"] .jk`).length, id);
    const target = (await p.evaluate(id => window.__bc.get(id), id) + 1) % n;
    await p.evaluate(([id, t]) => window.__bc.set(id, t), [id, target]);
    await p.waitForTimeout(950);
    const end = await p.evaluate(id => window.__bc.end(id), id);
    results[id] = end.jack === target && end.lift < .2 ? 'ok' : `wanted jack ${target}, got ${JSON.stringify(end)}`;
    if (results[id] !== 'ok') problems.push(`${id}: ${results[id]}`);
  }
  await p.screenshot({ path: path.join(out, 'after-all.png') });
  // keyboard: arrow keys move the plug
  await p.focus('.row[data-id="quiet"] .jk[aria-checked="true"]');
  const before = await p.evaluate(() => window.__bc.get('quiet'));
  await p.keyboard.press('ArrowRight'); await p.waitForTimeout(900);
  const after = await p.evaluate(() => window.__bc.get('quiet'));
  if (after === before) problems.push('arrow key did not change the setting');
  await p.context().close();
  // 2x details
  const hi = await open(vp, 'focus=hops', 2);
  await hi.waitForTimeout(1800);
  await hi.screenshot({ path: path.join(out, 'detail-focus.png'), clip: { x: 440, y: 46, width: 1000, height: 854 } });
  await hi.screenshot({ path: path.join(out, 'detail-card.png'), clip: { x: 0, y: 46, width: 460, height: 854 } });
  await hi.context().close();
  // narrow
  const nar = await open({ width: 900, height: 1000 });
  await nar.screenshot({ path: path.join(out, 'narrow.png'), fullPage: false });
  const overflow = await nar.evaluate(() => document.documentElement.scrollWidth > innerWidth);
  if (overflow) problems.push('horizontal overflow at 900px');
  // the machine's saved settings are the only record: the page renders every row from settings() and sends
  // nothing at load, whatever localStorage holds (cleared, or an old copy). In the simulator and on the app
  // path (window.__TAURI__ stubbed); a change the person makes then sends exactly that one setting.
  const atBoot = {};
  if (APP) for (const where of ['browser', 'app']) for (const stale of [false, true]) {
    const tag = `${where}, localStorage ${stale ? 'stale' : 'cleared'}`;
    const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: 2 });
    await ctx.addInitScript(stale => { try { localStorage.clear(); if (stale) localStorage.setItem('bombechat.settings', JSON.stringify({ sound: 2, hops: 0, quiet: 0 })); } catch (e) {} }, stale);
    const kept = { sound: 1, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 6, hold: 2, quiet: 4, repeats: 2, lock: 1, expire: 0, inperson: 1 };
    if (where === 'app') await ctx.addInitScript(kept => {
      window.__calls = [];
      window.__TAURI__ = { event: { listen: () => Promise.resolve(() => {}) }, core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
        return Promise.resolve(cmd === 'settings' ? kept : cmd === 'channels' ? [{ channel: 'sound', state: 'listening', rate: 16.1 }] : null); } } };
    }, kept);
    else await ctx.addInitScript(kept => { try { sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify(kept)); } catch (e) {} }, { sound: 1, hops: 6, quiet: 4, expire: 0 });
    const sp = await ctx.newPage();
    sp.on('pageerror', e => problems.push(`${tag}: page error: ` + e.message));
    sp.on('console', m => m.type() === 'error' && problems.push(`${tag}: console: ` + m.text()));
    await sp.goto(url('still'), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await sp.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    await sp.waitForTimeout(300);
    const sets = () => window.__TAURI__ ? __calls.filter(c => c[0] === 'set_setting').map(c => c[1]) : __sim.settingsSet;
    const shown = await sp.evaluate(ids => Object.fromEntries(ids.map(id => [id, +document.querySelector(`.row[data-id="${id}"] .jk[aria-checked="true"]`).dataset.i])), Object.keys(kept));
    const plugs = await sp.evaluate(ids => Object.fromEntries(ids.map(id => [id, window.__bc.end(id).jack])), Object.keys(kept));
    const sent = await sp.evaluate(`(${sets})()`);
    if (JSON.stringify(shown) !== JSON.stringify(kept)) problems.push(`${tag}: the rows show ${JSON.stringify(shown)}, the machine keeps ${JSON.stringify(kept)}`);
    if (JSON.stringify(plugs) !== JSON.stringify(kept)) problems.push(`${tag}: the plugs sit at ${JSON.stringify(plugs)}, the machine keeps ${JSON.stringify(kept)}`);
    if ((Array.isArray(sent) ? sent.length : sent) !== 0) problems.push(`${tag}: the page sent settings at load: ${JSON.stringify(sent)}`);
    if (!stale) await sp.screenshot({ path: path.join(out, `at-boot-${where}.png`), clip: { x: 0, y: 46, width: 460, height: 854 } });
    await sp.click('.row[data-id="sound"] .jk[data-i="2"]');
    await sp.waitForTimeout(200);
    const changed = await sp.evaluate(`(${sets})()`);
    const keptAfter = where === 'browser' ? await sp.evaluate(() => import('./bridge.js').then(m => m.bridge.settings())) : null;
    if (where === 'app' ? JSON.stringify(changed) !== JSON.stringify([{ id: 'sound', valueIndex: 2 }]) : changed !== 1 || keptAfter.sound !== 2) problems.push(`${tag}: choosing Listen and send sent ${JSON.stringify(changed)}, kept ${JSON.stringify(keptAfter)}`);
    atBoot[tag] = { shown, sent, changed };
    await ctx.close();
  }
  await browser.close();
  console.log(JSON.stringify({ out, fps, results, keyboard: [before, after], atBoot, problems }, null, 2));
  server.close();
  process.exit(problems.length ? 1 : 0);
})();
