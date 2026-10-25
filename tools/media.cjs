// Renders the README's pictures from the app itself, headless, on the GPU:
//   node tools/media.cjs [logo] [banner] [shots] [demo]     (all of them when none is named)
// Writes to docs/media/. Every picture is a page of this repository served by
// tools/serve.cjs (the browser simulator for the front, a stand-in for the
// Tauri shell for the first run); nothing on the screen is captured.
// Needs Google Chrome (Playwright's channel 'chrome', for Metal) and ffmpeg for the GIF.
const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');
const { chromium } = require('playwright');
const { start } = require('./serve.cjs');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'docs', 'media');
const PORT = 4814;
const at = p => `http://127.0.0.1:${PORT}/${p}`;
const want = new Set(process.argv.slice(2));
const doing = k => !want.size || want.has(k);
fs.mkdirSync(OUT, { recursive: true });

// the first run as the app shows it: no store yet, so the page asks for a name and a passphrase
function firstRun() {
  const listeners = {};
  window.__TAURI__ = {
    event: { listen: (n, f) => { (listeners[n] = listeners[n] || []).push(f); return Promise.resolve(() => {}); } },
    core: { invoke: cmd => {
      switch (cmd) {
        case 'contacts': return Promise.reject('no store yet');
        case 'machine': return Promise.resolve({ name: 'Machine No. 1', key: 'a3f09c1e'.repeat(16), you: '', held: 0, version: '0.1.0', broadcast_max: 3939 });
        case 'settings': return Promise.resolve({ sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 });
        case 'setting_notes': return Promise.resolve({});
        default: return new Promise(() => {});
      }
    } },
  };
}

(async () => {
  const server = await start(PORT);
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
  const ctx = (w, h, scale = 2) => browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: scale, reducedMotion: 'no-preference' });
  const errors = p => p.on('pageerror', e => console.error('page error:', e.message));
  try {
    if (doing('logo')) { await logo(); await logoSvg(); }
    if (doing('banner')) await banner();
    if (doing('shots')) await shots();
    if (doing('demo')) await demo();
    shrink();
  } finally {
    await browser.close();
    server.close();
  }

  // Shrink the PNGs: full-window shots down to 1.5x, then everything but the logo to 256 colours (Pillow,
  // when python3 has it; the pictures are left as rendered otherwise).
  function shrink() {
    const full = ['front.png', 'settings.png'];
    const py = `
import sys
from PIL import Image
for name in sys.argv[2:]:
    p = sys.argv[1] + '/' + name
    im = Image.open(p)
    if im.info.get('Software') == 'bombechat-shrunk': continue
    alpha = im.mode == 'RGBA' and im.getextrema()[3][0] < 255
    im = im.convert('RGBA' if alpha else 'RGB')
    if name in ${JSON.stringify(full)} and im.width > 2160:
        im = im.resize((2160, round(im.height * 2160 / im.width)), Image.LANCZOS)
    q = im.quantize(256, method=Image.Quantize.FASTOCTREE, dither=Image.Dither.FLOYDSTEINBERG)
    from PIL import PngImagePlugin
    meta = PngImagePlugin.PngInfo(); meta.add_text('Software', 'bombechat-shrunk')
    q.save(p, optimize=True, pnginfo=meta)
`;
    const names = fs.readdirSync(OUT).filter(n => n.endsWith('.png') && n !== 'logo.png');// the logo stays as rendered
    try { execFileSync('python3', ['-c', py, OUT, ...names], { stdio: 'inherit' }); } catch (e) { console.warn('PNGs left as rendered:', e.message); }
  }
  // one drum, face on, cut out round
  async function logo() {
    const c = await ctx(512, 512, 2);
    const p = await c.newPage(); errors(p);
    await p.goto(at('tools/media/drum.html?rim=7a2a22&letter=B&turn=1'));
    await p.waitForFunction(() => window.__drum && window.__drum.ready, null, { timeout: 30000 });
    const png = await p.evaluate(async () => {
      const gl = document.querySelector('canvas');
      const out = document.createElement('canvas'); out.width = out.height = 1024; const k = out.getContext('2d');
      const r = window.__drum.edge * 2 - 1;
      k.save(); k.beginPath(); k.arc(512, 512, r, 0, 7); k.clip(); k.drawImage(gl, 0, 0, 1024, 1024); k.restore();
      return out.toDataURL('image/png');
    });
    fs.writeFileSync(path.join(OUT, 'logo.png'), Buffer.from(png.split(',')[1], 'base64'));
    await c.close();
    console.log('logo.png');
  }
  // the front page held on a frame of the message, the plain row reading CHURCH
  async function front(c, q = 'f=1&still') {
    const p = await c.newPage(); errors(p);
    await p.goto(at('app/ui/bombechat.html?' + q));
    await p.waitForFunction(() => window.__bank && window.__bank.ready, null, { timeout: 30000 });
    await p.evaluate(() => document.fonts.ready);
    await p.waitForTimeout(1200);
    return p;
  }
  // the logo and the wordmark on the machine's enamel, with the drum bank on the right
  // the same drum drawn as a vector, after the flat drums the front page falls back to,
  // with the engraving face embedded so it draws the same anywhere
  async function logoSvg() {
    const A = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', TURN = 1;
    const font = fs.readFileSync(path.join(ROOT, 'app/ui/fonts/marcellus-sc.woff2')).toString('base64');
    let ring = '', ticks = '';
    for (let i = 0; i < 26; i++) {
      const a = (i - TURN) * 360 / 26;
      ring += `<text transform="rotate(${a.toFixed(3)})" y="-26.4">${A[i]}</text>`;
      ticks += `<line transform="rotate(${(a + 180 / 26).toFixed(3)})" y1="-35.4" y2="-32.2"/>`;
    }
    const holes = [45, 135, 225, 315].map(a => `<circle transform="rotate(${a})" cy="-13.4" r="1.65"/>`).join('');
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="-44 -44 88 88" width="512" height="512">
<title>Bombechat</title>
<defs>
<style>@font-face{font-family:'Marcellus SC';src:url(data:font/woff2;base64,${font}) format('woff2')}
.ring text{font-family:'Marcellus SC',serif;font-size:8.4px;fill:#ddd5c0;text-anchor:middle;dominant-baseline:central;stroke:#ddd5c0;stroke-width:.3px}</style>
<radialGradient id="face" cx="40%" cy="30%" r="80%"><stop offset="0" stop-color="#3a3530"/><stop offset=".65" stop-color="#1d1a17"/><stop offset="1" stop-color="#0d0c0a"/></radialGradient>
<linearGradient id="rim" x1="0" y1="0" x2=".35" y2="1"><stop offset="0" stop-color="#b34a3d"/><stop offset=".5" stop-color="#8e3328"/><stop offset="1" stop-color="#5e1f18"/></linearGradient>
<linearGradient id="bevel" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".22"/><stop offset=".5" stop-color="#fff" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".35"/></linearGradient>
<radialGradient id="hub" cx="35%" cy="28%" r="80%"><stop offset="0" stop-color="#fff3d2"/><stop offset=".25" stop-color="#e2be78"/><stop offset=".65" stop-color="#9a7232"/><stop offset="1" stop-color="#4a3414"/></radialGradient>
<linearGradient id="tab" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ddd4bb"/><stop offset=".22" stop-color="#ece4cd"/><stop offset="1" stop-color="#e6ddc4"/></linearGradient>
</defs>
<circle r="42" fill="url(#rim)"/><circle r="42" fill="url(#bevel)"/>
<circle r="41.6" fill="none" stroke="#000" stroke-opacity=".45" stroke-width=".8"/>
<circle r="36.9" fill="none" stroke="#000" stroke-opacity=".35" stroke-width="1.2"/>
<circle r="36.2" fill="url(#face)"/>
<g stroke="#dcd6c8" stroke-opacity=".3" stroke-width=".55">${ticks}</g>
<g class="ring">${ring}</g>
<circle r="18" fill="none" stroke="#dcd6c8" stroke-opacity=".1" stroke-width=".4"/>
<g transform="rotate(${-TURN * 360 / 26})" fill="#070605" stroke="#dcd6c8" stroke-opacity=".2" stroke-width=".3">${holes}</g>
<rect x="-11.4" y="-36.6" width="23.4" height="22.8" rx=".8" fill="#000" opacity=".45" transform="translate(1 1.4)"/>
<rect x="-11.7" y="-37.4" width="23.4" height="22.8" rx=".8" fill="url(#tab)" stroke="#000" stroke-opacity=".35" stroke-width=".7"/>
<text x="0" y="-19.4" text-anchor="middle" font-family="'Marcellus SC',serif" font-size="18.5" fill="#15130f" stroke="#15130f" stroke-width=".6">B</text>
<circle r="7.6" cx=".6" cy="1" fill="#000" opacity=".5"/>
<circle r="7.3" fill="url(#hub)"/><circle r="7.3" fill="none" stroke="#000" stroke-opacity=".45" stroke-width=".5"/>
<rect x="-4.6" y="-.55" width="9.2" height="1.1" rx=".3" fill="#0b0a09" transform="rotate(${(-1 + TURN * 2 * Math.PI / 26) * -180 / Math.PI})"/>
</svg>
`;
    fs.writeFileSync(path.join(OUT, 'logo.svg'), svg);
    console.log('logo.svg');
  }
  async function banner() {
    const tmp = path.join(ROOT, 'checks', 'media'); fs.mkdirSync(tmp, { recursive: true });
    const c = await ctx(1440, 900, 3);
    const p = await front(c);
    const b = await p.locator('.bank').boundingBox();
    const d = await p.locator('.drum').nth(5).boundingBox();
    await p.screenshot({ path: path.join(tmp, 'bank.png'), clip: { x: b.x, y: b.y, width: d.x + d.width + 22 - b.x, height: 335 } });
    await c.close();
    const bc = await ctx(1200, 480, 2);
    const bp = await bc.newPage(); errors(bp);
    await bp.goto(at('tools/media/banner.html'));
    await bp.waitForFunction(() => window.__ready, null, { timeout: 30000 });
    await bp.evaluate(() => document.fonts.ready);
    await bp.screenshot({ path: path.join(OUT, 'banner.png') });
    await bc.close();
    console.log('banner.png');
  }
  async function shots() {
    // the front: the message sheet and the drum bank, held on a frame of the message
    {
      const c = await ctx(1440, 900, 2);
      const p = await front(c);
      await p.screenshot({ path: path.join(OUT, 'front.png') });
      await c.close();
      console.log('front.png');
    }
    // pairing on the key check card: the offer with its 60 digits, then matched
    {
      const c = await ctx(1440, 900, 2);
      const p = await front(c, 'still');
      const card = async name => {
        const b = await p.locator('.menu').boundingBox();
        await p.screenshot({ path: path.join(OUT, name), clip: { x: b.x - 14, y: b.y - 14, width: b.width + 28, height: b.height + 28 } });
        console.log(name);
      };
      await p.click('#pairbtn');
      await p.waitForFunction(() => document.querySelector('.menu').dataset.pair === 'offer', null, { timeout: 10000 });
      await p.waitForTimeout(500);
      await card('pairing-offer.png');
      await p.click('#pairyes');
      await p.waitForTimeout(900);
      await card('pairing-matched.png');
      await c.close();
    }
    // writing a public notice on the message sheet
    {
      const c = await ctx(1440, 900, 2);
      const p = await front(c, 'still');
      await p.evaluate(() => { const l = document.getElementById('log'); l.scrollTop = l.scrollHeight; });
      await p.click('#mode');
      await p.fill('#reply', 'Water at the church on Fifth until dusk. Bring containers.');
      await p.waitForTimeout(300);
      const b = await p.locator('.sheet').boundingBox();
      await p.screenshot({ path: path.join(OUT, 'notice.png'), clip: { x: b.x - 12, y: b.y - 12, width: b.width + 24, height: b.height + 24 } });
      await c.close();
      console.log('notice.png');
    }
    // the back of the machine with a plug on its way to a new jack
    {
      const c = await ctx(1440, 900, 2);
      await c.addInitScript(() => { try { localStorage.clear(); sessionStorage.clear(); } catch (e) {} });
      const p = await c.newPage(); errors(p);
      await p.goto(at('app/ui/settings.html'));
      await p.waitForFunction(() => window.__ready, null, { timeout: 60000 });
      await p.evaluate(() => document.fonts.ready);
      await p.waitForTimeout(800);
      await p.hover('.row[data-id="repeats"]');
      await p.waitForTimeout(1600);
      await p.click('.row[data-id="repeats"] .jk[data-i="3"]');
      await p.waitForTimeout(+(process.env.MIDAIR || 330));
      await p.screenshot({ path: path.join(OUT, 'settings.png') });
      await c.close();
      console.log('settings.png');
    }
    // the first run, as the app shows it before there is a store
    {
      const c = await ctx(1440, 900, 2);
      await c.addInitScript(firstRun);
      const p = await c.newPage(); errors(p);
      await p.goto(at('app/ui/bombechat.html?still'));
      await p.waitForSelector('.unlock input[name=who]');
      await p.evaluate(() => document.fonts.ready);
      await p.fill('.unlock input[name=who]', 'Ana Lima');
      await p.fill('.unlock input[name=pass]', 'correct horse battery');
      await p.waitForTimeout(1500);
      const b = await p.locator('.unlock .stand').boundingBox();
      await p.screenshot({ path: path.join(OUT, 'first-run.png'), clip: { x: b.x - 190, y: b.y - 120, width: b.width + 380, height: b.height + 220 } });
      await c.close();
      console.log('first-run.png');
    }
  }
  // a message arriving: the drums turn, the tape types in, the sheet stamps it.
  // Frames come from Chrome's screencast (what the page painted, with their times), then ffmpeg
  // makes one palette for the whole clip and dithers to it.
  async function demo() {
    const tmp = path.join(ROOT, 'checks', 'media', 'frames'); fs.rmSync(tmp, { recursive: true, force: true }); fs.mkdirSync(tmp, { recursive: true });
    const from = +(process.env.FROM || 0), to = +(process.env.TO || 24);
    const c = await ctx(1440, 900, 1);
    const p = await c.newPage(); errors(p);
    const cdp = await c.newCDPSession(p);
    const frames = [];
    cdp.on('Page.screencastFrame', f => { frames.push({ t: f.metadata.timestamp, data: f.data }); cdp.send('Page.screencastFrameAck', { sessionId: f.sessionId }).catch(() => {}); });
    // from the next to last chunk, so the last one lands and the message is stamped inside the clip
    await p.goto(at('app/ui/bombechat.html?f=' + (process.env.F || 3)));
    await p.waitForFunction(() => window.__bank && window.__bank.ready, null, { timeout: 30000 });
    await cdp.send('Page.startScreencast', { format: 'jpeg', quality: 92, maxWidth: 1440, maxHeight: 900, everyNthFrame: 1 });
    await p.waitForTimeout(to * 1000);
    await cdp.send('Page.stopScreencast');
    await c.close();
    const t0 = frames[0].t;
    const kept = frames.filter(f => f.t - t0 >= from && f.t - t0 <= to);
    let list = '';
    kept.forEach((f, i) => {
      const name = `f${String(i).padStart(5, '0')}.jpg`;
      fs.writeFileSync(path.join(tmp, name), Buffer.from(f.data, 'base64'));
      const next = kept[i + 1] ? kept[i + 1].t : f.t + .5;
      list += `file '${name}'\nduration ${(next - f.t).toFixed(4)}\n`;
    });
    fs.writeFileSync(path.join(tmp, 'list.txt'), list);
    const crop = process.env.CROP || 'iw:ih:0:0';
    const vf = `crop=${crop},fps=${process.env.FPS || 20},scale=900:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=192:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle`;
    execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-f', 'concat', '-safe', '0', '-i', path.join(tmp, 'list.txt'), '-vf', vf, '-loop', '0', path.join(OUT, 'demo.gif')]);
    console.log('demo.gif', kept.length, 'frames over', (kept[kept.length - 1].t - kept[0].t).toFixed(1), 's');
  }
})().catch(e => { console.error(e); process.exit(1); });
