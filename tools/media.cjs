// Renders the README's pictures from the app itself, headless, on the GPU:
//   node tools/media.cjs [icon] [logo] [banner] [shots] [demo] [settings] [light] [bluetooth] [--edition bluetooth]
// (all of them when none is named). Writes to docs/media/, and the app icon to app/src-tauri/icons/. Every picture is a
// page of this repository served by tools/serve.cjs (the browser simulator for the front, a stand-in for the
// Tauri shell for the first run); nothing on the screen is captured. --edition bluetooth serves the standard edition's
// tree (bluetooth/, through its own copy of serve.cjs) instead of the root's, still writing to the root's docs/media/;
// the bluetooth target always renders from that tree, since the Bluetooth switch is only there. Run
// tools/sync-bluetooth.sh first so the edition shows what the root does.
// Needs Google Chrome (Playwright's channel 'chrome', for Metal) and ffmpeg for the GIFs, and gifsicle to make
// the clips small enough. The icon needs python3 with Pillow, and macOS's iconutil for icon.icns.
const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');
const { chromium } = require('playwright');
const { start } = require('./serve.cjs');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'docs', 'media');
const PORT = 4814;
const at = p => `http://127.0.0.1:${PORT}/${p}`;
const args = process.argv.slice(2);
// --edition <name> (or --edition=<name>): serve that edition's tree in place of the root's
const editionArg = (() => { const i = args.findIndex(a => a === '--edition' || a.startsWith('--edition='));
  if (i < 0) return null; const v = args[i].includes('=') ? args[i].split('=')[1] : args[i + 1]; args.splice(i, args[i].includes('=') ? 1 : 2); return v; })();
if (editionArg && editionArg !== 'bluetooth') { console.error(`no such edition: ${editionArg}`); process.exit(2); }
const EDITION_ROOT = path.join(ROOT, 'bluetooth');
// the server for a tree: the root's serve.cjs, or the edition's copy of it, which serves bluetooth/
const serverFor = edition => edition ? require(path.join(EDITION_ROOT, 'tools', 'serve.cjs')).start : start;
const want = new Set(args);
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
        case 'machine': return Promise.resolve({ name: 'Machine No. 1', key: 'a3f09c1e'.repeat(16), you: '', held: 0, version: '0.5.0', commit: 'a1b2c3d', broadcast_max: 3939 });
        case 'settings': return Promise.resolve({ sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 });
        case 'setting_notes': return Promise.resolve({});
        default: return new Promise(() => {});
      }
    } },
  };
}

// A clock the page runs on while a clip is made. Until freeze() the page keeps real time, so it loads as usual;
// after it, time moves only when step() is called: requestAnimationFrame, performance.now, setTimeout and the
// page's CSS transitions and animations all advance by exactly one frame, and nothing is dropped however slowly
// the frame is captured.
function virtualClock() {
  const realNow = performance.now.bind(performance), realRaf = requestAnimationFrame.bind(window);
  const realSet = setTimeout.bind(window), realClear = clearTimeout.bind(window);
  let frozen = false, vt = 0, raf = [], timers = [], nid = 1e9;
  const css = new Map();
  performance.now = () => frozen ? vt : realNow();
  window.requestAnimationFrame = cb => { if (frozen) { raf.push(cb); return raf.length; } return realRaf(t => frozen ? raf.push(cb) : cb(t)); };
  window.setTimeout = (fn, ms = 0, ...a) => { if (!frozen) return realSet(fn, ms, ...a); const id = nid++; timers.push({ id, at: vt + ms, fn: () => fn(...a) }); return id; };
  window.clearTimeout = id => { if (id >= 1e9) timers = timers.filter(t => t.id !== id); else realClear(id); };
  // the cursor, drawn into the page since a screenshot has none
  addEventListener('DOMContentLoaded', () => {
    const c = document.createElement('div');
    c.innerHTML = '<svg width="23" height="23" viewBox="0 0 23 23"><path d="M3 2v17l4.4-4.2 2.9 6.6 2.9-1.3-2.9-6.4h6.1z" fill="#fff" stroke="#000" stroke-width="1.3" stroke-linejoin="round"/></svg>';
    c.style.cssText = 'position:fixed;left:0;top:0;z-index:99999;pointer-events:none;transform-origin:3px 2px;filter:drop-shadow(0 1.5px 2px rgba(0,0,0,.55));transition:none;opacity:0';
    document.body.appendChild(c);
    const put = e => { c.style.left = (e.clientX - 3) + 'px'; c.style.top = (e.clientY - 2) + 'px'; c.style.opacity = 1; };
    addEventListener('pointermove', put, true);
    addEventListener('pointerdown', e => { put(e); c.style.transform = 'scale(.86)'; }, true);
    addEventListener('pointerup', e => { put(e); c.style.transform = ''; }, true);
    window.__press = on => { c.style.transform = on ? 'scale(.86)' : ''; };
  });
  window.__clock = {
    freeze() { vt = realNow(); frozen = true; },
    step(ms) {
      vt += ms;
      for (;;) { const due = timers.filter(t => t.at <= vt).sort((a, b) => a.at - b.at)[0]; if (!due) break; timers = timers.filter(t => t !== due); due.fn(); }
      document.body.getBoundingClientRect();
      for (const a of document.getAnimations()) {
        if (!css.has(a)) { css.set(a, vt - ms - (a.currentTime || 0)); a.pause(); }
        a.currentTime = Math.max(0, vt - css.get(a));
      }
      const q = raf; raf = []; q.forEach(cb => cb(vt));
    },
  };
}

(async () => {
  const server = await serverFor(editionArg)(PORT);
  // the Bluetooth clip's own server, on the edition's tree, unless that is already the one serving
  const BT_PORT = PORT + 1;
  const btServer = doing('bluetooth') && !editionArg ? await serverFor('bluetooth')(BT_PORT) : null;
  const btAt = p => `http://127.0.0.1:${btServer ? BT_PORT : PORT}/${p}`;
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
  const ctx = (w, h, scale = 2) => browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: scale, reducedMotion: 'no-preference' });
  const errors = p => p.on('pageerror', e => console.error('page error:', e.message));
  try {
    if (doing('icon')) await icon();
    if (doing('logo')) { await logo(); await logoSvg(); }
    if (doing('banner')) await banner();
    if (doing('shots')) await shots();
    if (doing('demo')) await demo();
    if (doing('settings')) await settingsClip();
    if (doing('light')) await lightClip();
    if (doing('bluetooth')) await bluetoothClip();
    shrink();
  } finally {
    await browser.close();
    server.close();
    if (btServer) btServer.close();
  }

  // Shrink the PNGs: full-window shots down to 1.5x, then everything but the logo to 256 colours (Pillow,
  // when python3 has it; the pictures are left as rendered otherwise).
  function shrink() {
    const full = ['front.png', 'light.png'];
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
  // The app icon: one drum off the bank seated on an enamel plate, on Apple's grid (an 824 px plate in a 1024 px
  // canvas). It is rendered in four cuts, each for the sizes it reads at: the whole drum from 128 px up; without the
  // tick marks and with a heavier ring at 48 and 64; at 32 without the ring, with a bigger window and a plain hub; at
  // 16 without the hub too. Each size is scaled down from its cut's 1024 px render. Writes the PNGs tauri.conf.json
  // lists (and the 64 px one tauri icon makes), icon.icns through iconutil, and icon.ico.
  async function icon() {
    const dir = path.join(ROOT, 'app', 'src-tauri', 'icons'), tmp = path.join(ROOT, 'checks', 'media', 'icon');
    fs.rmSync(tmp, { recursive: true, force: true }); fs.mkdirSync(tmp, { recursive: true });
    for (const cut of [0, 1, 2, 3]) {
      const c = await ctx(1024, 1024, 2);
      const p = await c.newPage(); errors(p);
      await p.goto(at(`tools/media/drum.html?rim=7a2a22&letter=B&turn=1&icon&simple=${cut}`));
      await p.waitForFunction(() => window.__drum && window.__drum.ready, null, { timeout: 30000 });
      const png = await p.evaluate(() => window.__icon());
      fs.writeFileSync(path.join(tmp, `cut${cut}.png`), Buffer.from(png.split(',')[1], 'base64'));
      await c.close();
    }
    const cutFor = px => px <= 16 ? 3 : px <= 32 ? 2 : px <= 64 ? 1 : 0;
    const iconset = path.join(tmp, 'icon.iconset'); fs.mkdirSync(iconset);
    const jobs = [[1024, path.join(dir, 'icon.png')], [32, path.join(dir, '32x32.png')], [64, path.join(dir, '64x64.png')],
      [128, path.join(dir, '128x128.png')], [256, path.join(dir, '128x128@2x.png')]];
    for (const pt of [16, 32, 128, 256, 512]) for (const k of [1, 2]) jobs.push([pt * k, path.join(iconset, `icon_${pt}x${pt}${k === 2 ? '@2x' : ''}.png`)]);
    const ico = [16, 24, 32, 48, 64, 128, 256]; for (const px of ico) jobs.push([px, path.join(tmp, `ico${px}.png`)]);
    // Lanczos from the 1024 px render, then at 32 px and under a light unsharp mask to bring back the edges the
    // scaling softens
    const py = `
import sys, json
from PIL import Image, ImageFilter
cuts = {}
for px, cut, out in json.loads(sys.argv[2]):
    src = cuts.get(cut) or cuts.setdefault(cut, Image.open(sys.argv[1] + '/cut%d.png' % cut).convert('RGBA'))
    im = src if px == src.width else src.resize((px, px), Image.LANCZOS, reducing_gap=3.0)
    if px <= 32: im = im.filter(ImageFilter.UnsharpMask(radius=0.6, percent=60, threshold=1))
    im.save(out, optimize=True)
`;
    execFileSync('python3', ['-c', py, tmp, JSON.stringify(jobs.map(([px, out]) => [px, cutFor(px), out]))], { stdio: 'inherit' });
    execFileSync('iconutil', ['-c', 'icns', iconset, '-o', path.join(dir, 'icon.icns')]);
    // icon.ico: PNG entries, one per size, each from its own cut
    const pngs = ico.map(px => fs.readFileSync(path.join(tmp, `ico${px}.png`)));
    const head = Buffer.alloc(6 + 16 * ico.length); head.writeUInt16LE(1, 2); head.writeUInt16LE(ico.length, 4);
    let off = head.length;
    ico.forEach((px, i) => { const e = 6 + 16 * i; head[e] = px & 255; head[e + 1] = px & 255; head.writeUInt16LE(1, e + 4); head.writeUInt16LE(32, e + 6);
      head.writeUInt32LE(pngs[i].length, e + 8); head.writeUInt32LE(off, e + 12); off += pngs[i].length; });
    fs.writeFileSync(path.join(dir, 'icon.ico'), Buffer.concat([head, ...pngs]));
    for (const f of ['icon.png', '32x32.png', '64x64.png', '128x128.png', '128x128@2x.png', 'icon.icns', 'icon.ico'])
      console.log(f, (fs.statSync(path.join(dir, f)).size / 1024).toFixed(0), 'KB');
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
<radialGradient id="hub" cx="35%" cy="28%" r="80%"><stop offset="0" stop-color="#f6f5f1"/><stop offset=".25" stop-color="#c9c7c0"/><stop offset=".65" stop-color="#7f7d77"/><stop offset="1" stop-color="#34332f"/></radialGradient>
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
  // the back of the machine, as a loop: Repeats moved from 2 to 3 on the wiring schedule card, the camera in on the
  // row while the cord travels, back out; then the same plug dragged back to 2 on the panel, and out again to the
  // opening frame. Every frame is stepped on the page's own clock (virtualClock), so none is dropped.
  async function settingsClip() {
    const FPS = 20, DT = 1000 / FPS;
    const tmp = path.join(ROOT, 'checks', 'media', 'settings'); fs.rmSync(tmp, { recursive: true, force: true }); fs.mkdirSync(tmp, { recursive: true });
    const c = await ctx(1280, 800, 2);
    await c.addInitScript(() => { try { localStorage.clear(); sessionStorage.clear(); } catch (e) {} });
    await c.addInitScript(virtualClock);
    const p = await c.newPage(); errors(p);
    await p.goto(at('app/ui/settings.html'));
    await p.waitForFunction(() => window.__ready, null, { timeout: 60000 });
    await p.evaluate(() => document.fonts.ready);
    // the card scrolled to Quiet sending, so the Repeats row sits level with the gap above the plug field
    await p.evaluate(() => { const s = document.getElementById('sched'), g = [...s.querySelectorAll('.grp')].find(e => e.textContent.startsWith('Quiet'));
      s.scrollTop += g.getBoundingClientRect().top - s.getBoundingClientRect().top; });
    const jackOnCard = i => p.evaluate(i => { const r = document.querySelector(`.row[data-id="repeats"] .jk[data-i="${i}"] i`).getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; }, i);
    const world = (x, y, z = .05) => p.evaluate(([x, y, z]) => window.__bc.at(x, y, z), [x, y, z]);
    const row = await p.evaluate(() => window.__bc.row('repeats'));
    // where the cursor rests: on the panel between the drum connectors and the plug field, level with the row
    // on the card, where nothing can be picked up
    const j3 = await jackOnCard(3);
    const rest = { x: (await world(-1.2, 2)).x, y: j3.y };
    await p.mouse.move(rest.x, rest.y);
    await p.waitForTimeout(2500);
    await p.evaluate(() => window.__clock.freeze());
    // three seconds on the page's clock before the first frame, so the camera has settled where the loop ends
    for (let i = 0; i < 3 * FPS; i++) await p.evaluate(dt => window.__clock.step(dt), DT);
    let n = 0, cur = { ...rest }; const log = [];
    async function frame() {
      await p.evaluate(dt => window.__clock.step(dt), DT);
      await p.screenshot({ path: path.join(tmp, `f${String(n++).padStart(4, '0')}.jpg`), type: 'jpeg', quality: 94 });
      log.push(await p.evaluate(() => window.__bc.focused()));
    }
    const hold = async s => { for (let i = Math.round(s * FPS); i > 0; i--) await frame(); };
    const ease = k => k < .5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
    const easeSoft = k => k < .5 ? 2 * k * k : 1 - Math.pow(-2 * k + 2, 2) / 2;
    // the cursor along a smooth path through the waypoints, eased over the whole of it; each waypoint is asked
    // for again every frame, so a point on the panel is followed while the camera moves
    async function glide(points, s, e = ease, until = () => false) {
      const N = Math.round(s * FPS), from = { ...cur };
      for (let i = 1; i <= N; i++) {
        const pts = [from]; for (const f of points) pts.push(typeof f === 'function' ? await f() : f);
        const P = catmull(pts, 12), L = [0]; for (let k = 1; k < P.length; k++) L.push(L[k - 1] + Math.hypot(P[k].x - P[k - 1].x, P[k].y - P[k - 1].y));
        const want = e(i / N) * L[L.length - 1]; let k = 1; while (k < L.length - 1 && L[k] < want) k++;
        const t = L[k] === L[k - 1] ? 0 : (want - L[k - 1]) / (L[k] - L[k - 1]);
        cur = { x: P[k - 1].x + (P[k].x - P[k - 1].x) * t, y: P[k - 1].y + (P[k].y - P[k - 1].y) * t };
        await p.mouse.move(cur.x, cur.y); await frame();
        if (until()) return;
      }
    }
    function catmull(pts, per) {
      if (pts.length < 3) return pts;
      const out = [], g = i => pts[Math.max(0, Math.min(pts.length - 1, i))];
      for (let i = 0; i < pts.length - 1; i++) for (let s = 0; s < per; s++) {
        const t = s / per, a = g(i - 1), b = g(i), c2 = g(i + 1), d = g(i + 2), f = k => .5 * (2 * b[k] + (-a[k] + c2[k]) * t + (2 * a[k] - 5 * b[k] + 4 * c2[k] - d[k]) * t * t + (-a[k] + 3 * b[k] - 3 * c2[k] + d[k]) * t * t * t);
        out.push({ x: f('x'), y: f('y') });
      }
      out.push(pts[pts.length - 1]); return out;
    }
    // a click lands at once (a press held across frames would let the card's row open under it); the cursor
    // shows the press for the next two frames
    const click = async () => { await p.mouse.down(); await p.mouse.up(); await p.evaluate(() => window.__press(true)); await frame(); await frame(); await p.evaluate(() => window.__press(false)); };
    const plug = async () => (await p.evaluate(() => window.__bc.screen('repeats'))).plug;
    // the cursor held on the plug while the camera comes in, a frame behind it, as a hand would be
    const ride = async s => { for (let i = 0, N = Math.round(s * FPS); i < N; i++) { const q = await plug(), a = Math.min(1, .45 + i * .14);
      cur = { x: cur.x + (q.x - cur.x) * a, y: cur.y + (q.y - cur.y) * a }; await p.mouse.move(cur.x, cur.y); await frame(); } };

    await hold(.4);
    // on the card: across to jack 3 of Repeats; the camera comes in on the row as the cursor reaches it, and the
    // click waits for it to arrive so the cord travels in a still frame
    await glide([() => jackOnCard(3)], 1.2);
    await hold(1.0);
    await click();
    await hold(1.4);
    // back to the full view: a click anywhere on the machine away from a plug
    await glide([rest], .9);
    await click();
    await hold(.5);
    // on the panel: down the gap, along under the row, and up onto the plug, which brings the camera in again
    const under = row.y - .36, j = row.jacks[3];
    // once the plug is under the cursor the camera starts in, and the cursor stays on the plug from then on
    const onPlug = () => log[log.length - 1] === 'repeats';
    await glide([await world(-1.2, under), await world(j, under)], 1.3, ease, onPlug);
    if (!onPlug()) await glide([plug], .4, easeSoft, onPlug);
    await ride(.9);
    // the plug dragged back along the row to jack 2, and let go: it drops into the nearest jack
    await p.mouse.down(); await frame(); await frame();
    await glide([() => world(row.jacks[2], row.y, .5)], .7);
    await hold(.1);
    await p.mouse.up();
    await hold(.9);
    // and out: along the row past its name, up to where the cursor started, a click
    await glide([await world(row.name + 1.6, row.y + .02), rest], 1.1);
    await click();
    await hold(2.6);
    await c.close();
    console.log('focus by frame:', log.map((f, i) => i === 0 || f !== log[i - 1] ? `${i}:${f}` : null).filter(Boolean).join(' '));
    encode(tmp, n, FPS);
  }
  // A message going out by light: the reply typed, the cursor to Flash, and the signal lamp opening in the message
  // sheet's place, grey for its still moment, then the code at Normal (56 cells, 10 frames a second) until the message
  // has shown its window, and the sheet again with the message sent. Stepped on the page's clock like the settings
  // clip, at 20 frames a second, so each code frame is held for exactly two GIF frames: the code changes at its real
  // rate. PLAY=<fps> plays the GIF back slower (the README says so if it is). Also the still of the flashing view.
  async function lightClip() {
    const FPS = 20, DT = 1000 / FPS, PLAY = +(process.env.PLAY || FPS);
    const kept = { sound: 2, volume: 2, band: 0, light: 2, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 };
    const setUp = kept => { try { localStorage.clear(); sessionStorage.clear(); sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify(kept)); } catch (e) {} };
    const TEXT = 'Water at the church on Fifth until dusk.';
    // the core's own frames, as tools/check.cjs takes them (crates/light/examples/page_frames.rs), so the lamp shows
    // real codes: finders, orientation mark, quiet zone and all. Handed to the simulator before the page loads.
    const CODE = JSON.parse(execFileSync('cargo', ['run', '-q', '-p', 'bombechat-light', '--example', 'page_frames', '--', 'frames', String(kept.flash), '80', '4660'], { cwd: ROOT, encoding: 'utf8', maxBuffer: 1 << 28 }));
    const real = ([spec, frames]) => { window.__lightFrames = { [spec]: frames }; };
    const realArgs = [CODE.spec, CODE.frames.map(f => f.bits)];
    fs.writeFileSync(path.join(ROOT, 'checks', 'media', 'light-frames.json'), JSON.stringify(CODE.frames.map(f => f.frame)));
    // the still: the lamp mid-message, at 2x
    {
      const c = await ctx(1440, 900, 2);
      await c.addInitScript(setUp, kept);
      await c.addInitScript(real, realArgs);
      const p = await front(c, 'still');
      await p.fill('#reply', TEXT);
      await p.click('#flash');
      await p.waitForFunction(() => +document.getElementById('lampn').textContent > 12, null, { timeout: 20000 });
      await p.evaluate(() => { const a = document.activeElement; if (a && a.blur) a.blur(); });
      await p.screenshot({ path: path.join(OUT, 'light.png') });
      await c.close();
      console.log('light.png');
    }
    const tmp = path.join(ROOT, 'checks', 'media', 'light'); fs.rmSync(tmp, { recursive: true, force: true }); fs.mkdirSync(tmp, { recursive: true });
    const c = await ctx(1280, 800, 1.5);
    await c.addInitScript(setUp, kept);
    await c.addInitScript(real, realArgs);
    await c.addInitScript(virtualClock);
    const p = await front(c, 'still');
    await p.fill('#reply', TEXT);
    const centre = async sel => { const b = await p.locator(sel).boundingBox(); return { x: b.x + b.width / 2, y: b.y + b.height / 2 }; };
    const r = await p.locator('#reply').boundingBox();
    let cur = { x: r.x + r.width * .62, y: r.y + r.height / 2 };
    await p.mouse.move(cur.x, cur.y);
    await p.waitForTimeout(800);
    await p.evaluate(() => window.__clock.freeze());
    let n = 0;
    const frame = async () => { await p.evaluate(dt => window.__clock.step(dt), DT); await p.screenshot({ path: path.join(tmp, `f${String(n++).padStart(4, '0')}.jpg`), type: 'jpeg', quality: 94 }); };
    const hold = async s => { for (let i = Math.round(s * FPS); i > 0; i--) await frame(); };
    const ease = k => k < .5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
    const glide = async (to, s) => { const from = { ...cur }, N = Math.round(s * FPS);
      for (let i = 1; i <= N; i++) { const e = ease(i / N); cur = { x: from.x + (to.x - from.x) * e, y: from.y + (to.y - from.y) * e }; await p.mouse.move(cur.x, cur.y); await frame(); } };
    await hold(.8);
    await glide(await centre('#flash'), 1.0);
    await hold(.3);
    await p.mouse.down(); await p.mouse.up(); await p.evaluate(() => window.__press(true)); await frame(); await frame(); await p.evaluate(() => window.__press(false));
    // the cursor off the code, onto the lamp's foot, while it shows
    await glide({ x: cur.x + 40, y: cur.y + 60 }, .6);
    // until the lamp has closed, then a moment on the sheet with the message sent
    for (let i = 0; i < 20 * FPS && await p.evaluate(() => document.getElementById('lamp').hidden) === false; i++) await frame();
    await hold(2.2);
    await c.close();
    encode(tmp, n, FPS, { W: 960, H: 600, name: 'light.gif', playFps: PLAY });
  }
  // Bluetooth, on the standard edition's front (bluetooth/app/ui, its own simulator): the switch turned from Contacts to
  // Open, its jewel lit and the Near readout counting the strangers in range; Ines Duarte asking to connect, on a slip in
  // the tray; Accept stamped, and her station added as not confirmed; a reply to Marisol sent by Bluetooth and stamped
  // heard; then Marisol's answer coming in by Bluetooth, the drums turning to it. The page can't take Ines off again, so
  // the clip's last frames dissolve into its first. Stepped on the page's clock, as the light clip is. Also
  // bluetooth.png, the panel's column with the request waiting, at 2x.
  //
  // The edition's simulator plays the room's side through the node's own events; its emit is kept on window.__btEmit
  // (and its sealing on window.__btSeal) by the copy this page is served, so Marisol's answer arrives as the node hands
  // over a Bluetooth envelope: every chunk at once, then the message. The file on disk is not changed.
  async function bluetoothClip() {
    const FPS = 20, DT = 1000 / FPS;
    const SIM = path.join(EDITION_ROOT, 'app', 'ui', 'simulator.js');
    if (!fs.existsSync(SIM)) throw new Error('bluetooth/ has no simulator: run tools/sync-bluetooth.sh');
    const kept = { sound: 2, volume: 2, band: 0, light: 2, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 0, bluetooth: 1 };
    const setUp = kept => { try { localStorage.clear(); sessionStorage.clear(); sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify(kept)); } catch (e) {} };
    const REPLY = 'On my way. Ines is with me, she has a radio.';
    const ANSWER = 'Good. Come up the back stairs.';
    const hooked = () => {
      let src = fs.readFileSync(SIM, 'utf8');
      const open = /(export function createSimulator\(emit\b[^\n]*\{\n)/;
      if (!open.test(src)) throw new Error('simulator.js: createSimulator(emit, ...) not found; update the bluetooth clip');
      src = src.replace(open, '$1  window.__btEmit = emit;\n');
      if (!/\bfunction chunked\(/.test(src) || !/\bfunction sealedInner\(/.test(src)) throw new Error('simulator.js: chunked or sealedInner not found; update the bluetooth clip');
      return src + '\nwindow.__btSeal = (text, minute, seed) => chunked(sealedInner(text, minute, seed), seed);\n';
    };
    // the front as it opens: the incoming message whole (?f past its end), the room held still, two more strangers in
    // range for Open to show
    async function open(c) {
      await c.addInitScript(setUp, kept);
      await c.route('**/app/ui/simulator.js', r => r.fulfill({ body: hooked(), contentType: 'text/javascript', headers: { 'Cache-Control': 'no-store' } }));
      const p = await c.newPage(); errors(p);
      await p.goto(btAt('app/ui/bombechat.html?still&f=99'));
      await p.waitForFunction(() => window.__bank && window.__bank.ready && window.__btEmit && document.getElementById('k3'), null, { timeout: 30000 });
      await p.evaluate(() => document.fonts.ready);
      await p.evaluate(() => window.__sim.bluetoothStrangers(['R2QD', 'T8WN']));
      return p;
    }
    const tmp = path.join(ROOT, 'checks', 'media', 'bluetooth'); fs.rmSync(tmp, { recursive: true, force: true }); fs.mkdirSync(tmp, { recursive: true });

    // the still: the switch at Open, the request waiting in the tray, the strangers folded under the meter
    {
      const c = await ctx(1440, 900, 2);
      const p = await open(c);
      await p.waitForTimeout(1500);
      await p.click('#k3');
      await p.waitForTimeout(900);
      await p.evaluate(() => window.__sim.bluetoothAsk());
      await p.waitForSelector('.slip.ask .rs.ok');
      await p.mouse.move(0, 0);
      await p.waitForTimeout(3200);// the top bar's line has faded
      // the panel: the three switches, the meter and the tray, the stations
      const b = await p.locator('.panel').boundingBox();
      await p.screenshot({ path: path.join(OUT, 'bluetooth.png'), clip: { x: b.x - 12, y: b.y - 12, width: b.width + 24, height: Math.min(900 - b.y + 12, b.height + 24) } });
      await c.close();
      console.log('bluetooth.png');
    }

    const c = await ctx(1440, 900, 1.5);
    await c.addInitScript(virtualClock);
    const p = await open(c);
    const centre = async sel => { const b = await p.locator(sel).first().boundingBox(); return { x: b.x + b.width / 2, y: b.y + b.height / 2 }; };
    // where the cursor rests: on the panel's dark face beside the room meter, clear of every control
    const vu = await p.locator('.vu').boundingBox();
    const rest = { x: vu.x + vu.width * .7, y: vu.y - 22 };
    let cur = { ...rest };
    await p.mouse.move(cur.x, cur.y);
    await p.waitForTimeout(2500);
    await p.evaluate(() => window.__clock.freeze());
    for (let i = 0; i < 2 * FPS; i++) await p.evaluate(dt => window.__clock.step(dt), DT);
    let n = 0;
    const frame = async () => { await p.evaluate(dt => window.__clock.step(dt), DT); await p.screenshot({ path: path.join(tmp, `f${String(n++).padStart(4, '0')}.jpg`), type: 'jpeg', quality: 94 }); };
    const hold = async s => { for (let i = Math.round(s * FPS); i > 0; i--) await frame(); };
    const ease = k => k < .5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
    const glide = async (to, s) => { const from = { ...cur }, N = Math.round(s * FPS);
      for (let i = 1; i <= N; i++) { const e = ease(i / N); cur = { x: from.x + (to.x - from.x) * e, y: from.y + (to.y - from.y) * e }; await p.mouse.move(cur.x, cur.y); await frame(); } };
    const click = async () => { await p.mouse.down(); await p.mouse.up(); await p.evaluate(() => window.__press(true)); await frame(); await frame(); await p.evaluate(() => window.__press(false)); };

    await hold(.7);
    // the switch, Contacts to Open
    await glide(await centre('#k3'), .8);
    await hold(.15);
    await click();
    await hold(.5);
    // the cursor off the switch, so the jewel and the Near readout are seen
    await glide({ x: cur.x - 30, y: cur.y + 70 }, .5);
    await hold(.6);
    // Ines asks
    await p.evaluate(() => window.__sim.bluetoothAsk());
    await hold(1.1);
    await glide(await centre('.slip.ask .rs.ok'), .8);
    await hold(.25);
    await click();
    // her station: not confirmed, until her machine shows it holds this one too
    await glide({ x: cur.x - 10, y: cur.y + 50 }, .5);
    await hold(.6);
    // a reply to Marisol, typed and sent by Bluetooth
    const r = await p.locator('#reply').boundingBox();
    await glide({ x: r.x + 120, y: r.y + r.height / 2 }, 1.0);
    await click();
    for (let i = 0; i < REPLY.length; i += 2) { await p.keyboard.type(REPLY.slice(i, i + 2)); await frame(); }
    await hold(.25);
    await glide(await centre('#sendbluetooth'), .7);
    await hold(.15);
    await click();
    await hold(1.4);
    // Marisol's answer, by Bluetooth: the envelope whole, so every chunk lands at once and the drums turn to it
    await p.evaluate(text => {
      const e = window.__btEmit, ents = document.querySelectorAll('.ent.mine'), minute = Math.floor(+ents[ents.length - 1].dataset.at / 60000) + 1;
      e('channel', { channel: 'Bluetooth', state: 'receiving' });
      const parts = window.__btSeal(text, minute, 2718);
      parts.forEach((view, index) => e('chunk', { tag: 0x2718, index, of: parts.length, view, channel: 'Bluetooth' }));
      e('message', { id: Array.from({ length: 16 }, (_, i) => 0x40 + i), contact: 1, outgoing: false, text, channel: 'Bluetooth', at_min: minute, heard: false });
    }, ANSWER);
    await glide(rest, 1.2);
    await p.evaluate(() => window.__btEmit('channel', { channel: 'Bluetooth', state: 'open to requests' }));
    await hold(1.3);
    // and back to the opening frame
    const FADE = Math.round(.7 * FPS);
    await hold(FADE / FPS);
    await c.close();
    encode(tmp, n, FPS, { W: 960, H: 600, name: 'bluetooth.gif', fade: FADE });
  }
  // A GIF of a photoreal scene in motion: scaled down, softened a touch (the enamel's grain is what costs the most),
  // then held still wherever a pixel moved less than the eye can see, so the frames carry only what changed. One
  // palette for the whole clip, no dithering, then gifsicle's lossy pass when it is installed.
  // fade: that many last frames dissolve into the first, for a clip whose last state can't be undone on the page.
  function encode(dir, n, FPS, { W = 800, H = 500, name = 'settings.gif', playFps = FPS, fade = 0 } = {}) {
    const T = 8, size = W * H * 3;
    const raw = path.join(dir, 'frames.rgb'), held = path.join(dir, 'held.rgb'), gif = path.join(dir, 'clip.gif'), out = path.join(OUT, name);
    execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-framerate', String(FPS), '-i', path.join(dir, 'f%04d.jpg'), '-vf', `scale=${W}:${H}:flags=lanczos,gblur=sigma=.6`, '-f', 'rawvideo', '-pix_fmt', 'rgb24', raw]);
    // once a move has come to rest (a frame where nothing passes the threshold), the whole frame is brought up to
    // date, so what the threshold held back never lingers; the last frame is too, so the loop meets its first
    const fin = fs.openSync(raw, 'r'), fout = fs.openSync(held, 'w'), prev = Buffer.alloc(size), cur = Buffer.alloc(size);
    const first = Buffer.alloc(size);
    let settled = true;
    for (let f = 0; f < n && fs.readSync(fin, cur, 0, size, f * size) === size; f++) {
      if (f === 0) cur.copy(first);
      if (fade && f >= n - fade) { const a = (f - (n - fade) + 1) / (fade + 1); for (let i = 0; i < size; i++) cur[i] = Math.round(cur[i] + (first[i] - cur[i]) * a); }
      let moved = 0;
      if (f > 0) for (let i = 0; i < size; i += 3) {
        if (Math.abs(cur[i] - prev[i]) > T || Math.abs(cur[i + 1] - prev[i + 1]) > T || Math.abs(cur[i + 2] - prev[i + 2]) > T) { prev[i] = cur[i]; prev[i + 1] = cur[i + 1]; prev[i + 2] = cur[i + 2]; moved++; }
      }
      if (f === 0 || f === n - 1 || (!moved && !settled)) cur.copy(prev);
      settled = !moved;
      fs.writeSync(fout, prev);
    }
    fs.closeSync(fin); fs.closeSync(fout);
    const vf = 'split[a][b];[a]palettegen=max_colors=128:stats_mode=full[p];[b][p]paletteuse=dither=none:diff_mode=rectangle';
    execFileSync('ffmpeg', ['-y', '-loglevel', 'error', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-s', `${W}x${H}`, '-framerate', String(playFps), '-i', held, '-vf', vf, '-loop', '0', gif]);
    try { execFileSync('gifsicle', ['-O3', '--lossy=80', '-o', out, gif]); } catch (e) { console.warn('gifsicle not found, so the GIF is left unoptimised'); fs.copyFileSync(gif, out); }
    fs.rmSync(raw); fs.rmSync(held);
    console.log(name, n, 'frames,', (n / playFps).toFixed(2), 's,', (fs.statSync(out).size / 1e6).toFixed(2), 'MB');
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
