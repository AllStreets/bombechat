// Quality check for a Bombechat mockup page. Run after every change:
//   node tools/check.cjs mockups/bombechat.html
// Writes to checks/<page>/:
//   vp-<w>x<h>.png        the page at each laptop size, frame 4, held still
//   vp-<w>x<h>-full.png   the same, whole scrolling page
//   full-f<N>.png         1440x900, held still on each frame
//   detail-<part>.png     2x crops of each part, found by selector
//   motion.png            the drum bank mid-turn, in real time
//   reply.png             the sheet after typing and sending a reply
// and prints what it measured. Exits non-zero on page errors, horizontal overflow,
// text under 12px, layout shift on load, a plain row that doesn't settle on the
// frame text, the sheet not keeping its place, a reply that doesn't land, clipped
// audio, or drum sound leaking into the 17.6 to 19.8 kHz listening band.
const path = require('path');
const fs = require('fs');
const { chromium } = require('playwright');

const page = path.resolve(process.argv[2] || 'mockups/bombechat.html');
const out = path.join(path.dirname(page), '..', 'checks', path.basename(page, '.html'));
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const { start } = require('./serve.cjs');
const ROOTDIR = path.resolve(__dirname, '..');
const url = q => 'http://127.0.0.1:4812/' + path.relative(ROOTDIR, page) + (q ? '?' + q : '');
const VIEWPORTS = [[1024, 768], [1280, 800], [1440, 900], [1728, 1117]];
const PARTS = { top: '.top', bank: '.bank', sheet: '.sheet', compose: '.compose', menu: '.menu', panel: '.panel' };

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
  };

  // 1. each laptop size: overflow, small text, layout shift while fonts arrive
  report.viewports = {};
  for (const [w, h] of VIEWPORTS) {
    const p = await browser.newPage({ viewport: { width: w, height: h } });
    watch(p, `${w}x${h}`);
    await p.addInitScript(() => {
      window.__cls = 0;
      new PerformanceObserver(l => { for (const e of l.getEntries()) if (!e.hadRecentInput) window.__cls += e.value; }).observe({ type: 'layout-shift', buffered: true });
    });
    await p.goto(url('f=3&still'), { waitUntil: 'networkidle' });
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
  const frames = await still.evaluate(() => +document.getElementById('fnt').textContent);
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
  await hi.goto(url('f=3&still'));
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
  await live.waitForFunction(() => __bank.rows().plain === ft(+document.getElementById('fno').textContent - 1), null, { timeout: 4000 }).catch(() => {});
  const motion = await live.evaluate(() => {
    const plain = __bank.rows().plain;
    return { frame: document.getElementById('fno').textContent, plain, expected: ft(+document.getElementById('fno').textContent - 1) };
  });
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
  const rmSpin = await rm.evaluate(() => new Promise(r => { const f0 = document.getElementById('fno').textContent; const t = setInterval(() => { if (document.getElementById('fno').textContent !== f0) { clearInterval(t); setTimeout(() => r(cells.some(c => !c.snap)), 400); } }, 20); }));
  if (rmSpin) problems.push('reduced motion still turns the 3D drums');
  await rm.close();

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
