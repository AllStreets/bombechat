// Quality check for the settings page (the back of the machine). Run after every change:
//   node tools/check-settings.cjs
// Writes checks/settings/: overview, focused rows, mid-move frames, narrow layout, 2x details.
// Fails on page errors, a plug that does not end in the jack its setting names, or a slow frame rate.
const path = require('path'), fs = require('fs');
const { chromium } = require('playwright');
const page = path.resolve(process.argv[2] || 'mockups/settings.html');
const out = path.join(path.dirname(page), '..', 'checks', 'settings');
fs.mkdirSync(out, { recursive: true });
const url = q => 'file://' + page + (q ? '?' + q : '');
(async () => {
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
  const problems = [];
  const open = async (vp, q, dpr = 1) => {
    const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: dpr });
    await ctx.addInitScript(() => { try { localStorage.clear() } catch (e) {} });
    const p = await ctx.newPage();
    p.on('pageerror', e => problems.push('page error: ' + e.message));
    p.on('console', m => m.type() === 'error' && problems.push('console: ' + m.text()));
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
  await browser.close();
  console.log(JSON.stringify({ out, fps, results, keyboard: [before, after], problems }, null, 2));
  process.exit(problems.length ? 1 : 0);
})();
