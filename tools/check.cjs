// Quality check for a Bombechat mockup page. Run after every change:
//   node tools/check.cjs mockups/bombechat.html
// Writes to checks/<page>/:
//   full-f<N>.png    the whole window, held still on each frame (1x)
//   detail-<area>.png 2x crops of each region, frame 4
//   motion.png       the drum bank mid-turn, in real time
// and prints the motion and offline audio results. Exits non-zero on page errors,
// a plain row that doesn't settle on the frame text, clipped audio, or drum sound
// leaking into the 17.6 to 19.8 kHz listening band.
const path = require('path');
const fs = require('fs');
const { chromium } = require('playwright');

const page = path.resolve(process.argv[2] || 'mockups/bombechat.html');
const out = path.join(path.dirname(page), '..', 'checks', path.basename(page, '.html'));
fs.mkdirSync(out, { recursive: true });
const url = q => 'file://' + page + (q ? '?' + q : '');

const AREAS = {
  top: { x: 0, y: 0, width: 1490, height: 70 },
  bank: { x: 24, y: 60, width: 1442, height: 400 },
  sheet: { x: 24, y: 440, width: 640, height: 490 },
  menu: { x: 650, y: 440, width: 360, height: 490 },
  panel: { x: 1000, y: 440, width: 470, height: 490 },
};

(async () => {
  const browser = await chromium.launch();
  const problems = [];
  const watch = p => {
    p.on('pageerror', e => problems.push('page error: ' + e.message));
    p.on('console', m => m.type() === 'error' && problems.push('console: ' + m.text()));
  };

  // still frames
  const still = await browser.newPage({ viewport: { width: 1490, height: 950 } });
  watch(still);
  const frames = await (async () => {
    await still.goto(url('still'));
    return still.evaluate(() => +document.getElementById('fnt').textContent);
  })();
  for (let f = 0; f < frames; f++) {
    await still.goto(url(`f=${f}&still`));
    await still.waitForTimeout(400);
    await still.screenshot({ path: path.join(out, `full-f${f + 1}.png`) });
  }

  // 2x detail crops
  const hi = await browser.newPage({ viewport: { width: 1490, height: 950 }, deviceScaleFactor: 2 });
  watch(hi);
  await hi.goto(url('f=3&still'));
  await hi.waitForTimeout(500);
  for (const [name, clip] of Object.entries(AREAS)) await hi.screenshot({ path: path.join(out, `detail-${name}.png`), clip });

  // motion, real time
  const live = await browser.newPage({ viewport: { width: 1490, height: 950 } });
  watch(live);
  await live.goto(url());
  await live.mouse.click(5, 5);
  await live.waitForTimeout(4550);
  await live.screenshot({ path: path.join(out, 'motion.png'), clip: AREAS.bank });
  await live.waitForTimeout(2600);
  const motion = await live.evaluate(() => {
    const plain = [...document.querySelectorAll('.wl')].slice(0, 12).map(t => t.textContent).join('');
    return { frame: document.getElementById('fno').textContent, plain, tape: document.getElementById('live').innerText.replace(/\s+/g, ' ').trim() };
  });
  const expected = await live.evaluate(n => typeof ft === 'function' ? ft(n - 1) : null, +motion.frame);
  if (expected && expected !== motion.plain) problems.push(`plain row reads ${motion.plain}, frame text is ${expected}`);

  // offline audio
  const au = await browser.newPage();
  watch(au);
  await au.goto(url('audiotest&still'));
  await au.waitForFunction(() => document.body.dataset.audiotest, null, { timeout: 60000 });
  const audio = JSON.parse(await au.evaluate(() => document.body.dataset.audiotest));
  if (audio.clipped) problems.push('drum audio clips');
  if (+audio.listeningBandShare > 1e-4) problems.push('drum audio leaks into the listening band: ' + audio.listeningBandShare);

  await browser.close();
  console.log(JSON.stringify({ out, frames, motion, audio, problems }, null, 2));
  process.exit(problems.length ? 1 : 0);
})();
