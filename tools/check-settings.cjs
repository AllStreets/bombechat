// Quality check for the settings page (the back of the machine). Run after every change:
//   node tools/check-settings.cjs                         the app's interface, app/ui/settings.html
//   node tools/check-settings.cjs mockups/settings.html   the frozen design reference
// Writes checks/<folder>/settings/: overview, focused rows, mid-move frames, narrow layout, 2x details.
// Fails on page errors, a plug that does not end in the jack its setting names, or a slow frame rate,
// and for the app's interface on any request that leaves this machine, on a drum connector field that is not
// three rows of twelve signed Row 1 to Row 3 with columns 12 to 1 clear of the plates below it, and on a first
// view that does not show the panel's top (bolts, column numbers, Row 1) or puts the hint over the machine at every
// window size, and on a back without WebGL that is not the wiring schedule alone and working.
const path = require('path'), fs = require('fs');
const { chromium } = require('playwright');
const ROOTDIR = path.resolve(__dirname, '..');
const page = path.resolve(process.argv[2] || 'app/ui/settings.html');
const APP = !/[\\/]mockups[\\/]/.test(page);
const out = path.join(ROOTDIR, 'checks', path.relative(ROOTDIR, path.dirname(page)).replace(/[\\/]/g, '-'), 'settings');
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const { start } = require('./serve.cjs');
// what the device's setting_notes answers on this build (a Mac): relay waits, and where there is no other way to add
// a contact than in person, the key check is always on. The page says which this build is (window.__edition, from
// edition.js, as the device's edition.rs has it): read once the first page is open
let NOTES = { carry: { note: 'comes with relay' }, hops: { note: 'comes with relay' }, hold: { note: 'comes with relay' } };
let ONLY_IN_PERSON = true;
const url = q => 'http://127.0.0.1:4811/' + path.relative(ROOTDIR, page) + (q ? '?' + q : '');
(async () => {
  const server = await start(4811);
  // with the back/forward cache on (Playwright turns it off), so the history check meets a restored page
  const browser = await chromium.launch({ channel: 'chrome', args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'], ignoreDefaultArgs: ['--disable-back-forward-cache'] });
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
  if (APP) {
    const held = await p.evaluate(() => window.__edition && window.__edition.keyCheckHeld);
    if (typeof held !== 'boolean') problems.push('the page does not say whether the key check is held (window.__edition)');
    ONLY_IN_PERSON = held !== false;
  }
  // the top plate names the build as the machine does, as the front's does, cut for its longest form
  if (APP) {
    const { workspaceVersion } = require('./versions.cjs');
    const b = await p.evaluate(() => { const b = document.getElementById('build'), rt = b.parentElement;
      const at = () => [...rt.children].map(k => Math.round(k.getBoundingClientRect().left)).join();
      const text = b.textContent, before = at(); b.textContent = text.replace(/[^ ]+$/, 'ddddddd+'); const moved = at() !== before; b.textContent = text; return { text, moved }; });
    if (b.text !== `Build ${workspaceVersion(ROOTDIR)} \u00b7 browser`) problems.push(`the top plate says ${JSON.stringify(b.text)}`);
    if (b.moved) problems.push("the build plate's longest form moves what is beside it");
  }
  // the kit's shared parts (the front and the back draw their blanking plates, bolt rings, switches and jewels with
  // them): each builds and renders without an error, and a baked part comes back as a picture with clear corners
  if (APP) {
    const kit = await p.evaluate(async () => {
      const K0 = await import('./machine-kit.js'), host = document.createElement('div');
      host.style.cssText = 'position:fixed;left:-400px;top:0;width:200px;height:200px'; document.body.appendChild(host);
      const out = {};
      try {
        const K = K0.createMachine(host, { still: true }); K.renderer.setSize(200, 200, false);
        const per = K.perimeter(6, 4, 0, { across: 13, down: 3 }); out.perimeter = per.bolts.top.length + '/' + per.bolts.left.length;
        const b = K.blank(2, .8, 'NOT FITTED'); out.blank = b.group.children.length;
        const kn = K.knob({ labels: ['Off', 'Listen', 'Listen and send'] }); kn.set(2, true); kn.tick(performance.now() + 1000); out.knob = +(kn.rot.rotation.z).toFixed(3);
        const j = K.jewel('#f0b64c'); out.jewel = [j.set(true), j.on, j.set(true)].join();
        K.camera.position.set(0, 0, 12); K.renderer.render(K.scene, K.camera);
        const url = K.bake(K.jewel('#f0b64c', { parent: new K.THREE.Group() }).group, { px: 48, shadow: true });
        const img = new Image(); img.src = url; await img.decode();
        const c = document.createElement('canvas'); c.width = img.width; c.height = img.height; const k = c.getContext('2d'); k.drawImage(img, 0, 0);
        const a = (x, y) => k.getImageData(x, y, 1, 1).data[3], d = k.getImageData(0, 0, c.width, c.height).data;
        let solid = 0; for (let i = 3; i < d.length; i += 4) if (d[i] > 200) solid++;
        out.bake = { w: img.width, corners: [a(0, 0), a(c.width - 1, 0), a(0, c.height - 1), a(c.width - 1, c.height - 1)], solid };
        K.bakeDone(); K.renderer.dispose(); K.renderer.forceContextLoss();
      } catch (e) { out.error = e.message; }
      host.remove();
      return out;
    });
    if (kit.error || kit.perimeter !== '13/2' || !(kit.blank >= 9) || kit.knob !== -2.094 || kit.jewel !== 'true,true,false') problems.push(`the kit's shared parts: ${JSON.stringify(kit)}`);
    else if (!kit.bake || kit.bake.w !== 48 || kit.bake.corners.some(v => v !== 0) || kit.bake.solid < 200) problems.push(`the kit's bake: ${JSON.stringify(kit.bake)}`);
  }
  if (ONLY_IN_PERSON) NOTES.inperson = { note: 'always, on this build', value: 1 };
  await p.screenshot({ path: path.join(out, 'overview.png') });
  // the back carries the front's three rows of drums: 36 connectors, Rows 1 to 3 signed on plates, columns 12 to 1
  // from left to right (each behind its drum, so mirrored about the panel's middle), evenly spaced, and nothing of
  // the field (a connector or a loom hanging from one) over the maker's plate or a group's plate below it
  let field = null;
  if (APP) {
    field = await p.evaluate(() => ({ connectors: window.__bc.connectors(), signs: window.__bc.rowSigns(), parts: window.__bc.parts(), mid: window.__bc.at(0, 0, .05).x }));
    const C = field.connectors, why = [];
    if (C.length !== 36) why.push(`${C.length} connectors`);
    if (JSON.stringify(field.signs) !== JSON.stringify(['Row 1', 'Row 2', 'Row 3'])) why.push(`row signs ${JSON.stringify(field.signs)}`);
    for (const r of [1, 2, 3]) {
      const row = C.filter(c => c.row === r).sort((a, b) => a.x - b.x);
      if (row.length !== 12) { why.push(`row ${r} has ${row.length}`); continue; }
      if (row.map(c => c.col).join() !== '12,11,10,9,8,7,6,5,4,3,2,1') why.push(`row ${r} columns run ${row.map(c => c.col).join(' ')}`);
      if (row.some(c => c.label !== 'Row ' + r)) why.push(`row ${r} labelled ${[...new Set(row.map(c => c.label))].join(', ')}`);
      const steps = row.slice(1).map((c, i) => c.x - row[i].x);
      if (Math.max(...steps) - Math.min(...steps) > 2) why.push(`row ${r} unevenly spaced: ${steps.map(Math.round).join(' ')}`);
      if (Math.abs((row[0].x + row[11].x) / 2 - field.mid) > 3) why.push(`row ${r} is not centred on the panel`);
      // level, but for the lens: the camera stands a little right of and below the panel's middle
      const tilt = Math.max(...row.map(c => c.y)) - Math.min(...row.map(c => c.y));
      if (tilt > steps[0] * .08) why.push(`row ${r} is not level (${tilt.toFixed(1)} px)`);
    }
    const rowsY = [1, 2, 3].map(r => C.find(c => c.row === r).y);
    if (!(rowsY[0] < rowsY[1] && rowsY[1] < rowsY[2])) why.push(`rows out of order top to bottom: ${rowsY.map(Math.round)}`);
    const overlap = (a, b) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
    const below = [field.parts.makersPlate, ...field.parts.groupPlates];
    field.parts.connectors.forEach((c, i) => below.forEach(b => overlap(c, b) && why.push(`connector ${i} over a plate`)));
    field.parts.looms.forEach((l, i) => below.forEach(b => overlap(l, b) && why.push(`loom ${i} hangs over a plate (to ${Math.round(l.bottom)}, the plate from ${Math.round(b.top)})`)));
    field.parts.rowSigns.forEach((s, i) => field.parts.connectors.forEach(c => overlap(s, c) && why.push(`Row ${i + 1}'s sign touches a connector`)));
    if (why.length) problems.push('the back\'s drum connectors: ' + [...new Set(why)].join('; '));
    // the duct's clamps: every trunk cord passes behind its brass strap and inside its sides, goes up behind the
    // duct's lip, and ends inside the duct under its top, so no cord shows over a clamp or out of the duct
    const duct = await p.evaluate(() => window.__bc.duct()), cw = [];
    duct.cords.forEach((t, i) => { const c = duct.clamps.find(c => c.gi === t.gi);
      if (!c) { cw.push(`cord ${i} has no clamp`); return; }
      if (t.atClamp.front >= c.innerZ) cw.push(`cord ${i} stands ${(t.atClamp.front - c.innerZ).toFixed(3)} in front of its clamp`);
      if (t.atClamp.x0 < c.x0 - 1e-6 || t.atClamp.x1 > c.x1 + 1e-6) cw.push(`cord ${i} runs outside its clamp's sides`);
      if (t.underLip >= duct.back) cw.push(`cord ${i} comes in front of the duct's lip`);
      if (!(t.end.y > duct.HB && t.top < duct.HT)) cw.push(`cord ${i} ends at ${t.end.y.toFixed(2)}, reaching ${t.top.toFixed(2)}, not inside the duct (${duct.HB} to ${duct.HT})`); });
    if (duct.clamps.length !== 5) cw.push(`${duct.clamps.length} clamps`);
    if (cw.length) problems.push('the duct: ' + cw.join('; '));
    field = { signs: field.signs, rowsY: rowsY.map(Math.round), pitch: Math.round(C.filter(c => c.row === 1).sort((a, b) => a.x - b.x).slice(0, 2).reduce((a, b) => b.x - a.x)), problems: why.length };
  }
  // frame rate over 2 s
  const fps = await p.evaluate(() => new Promise(r => { let n = 0; const t0 = performance.now(); const f = () => { n++; performance.now() - t0 < 2000 ? requestAnimationFrame(f) : r(Math.round(n / 2)) }; requestAnimationFrame(f) }));
  if (fps < 30) problems.push('frame rate ' + fps);
  // focus a row by hovering it on the card
  await p.hover('.row[data-id="repeats"]');
  await p.waitForTimeout(1600);
  await p.screenshot({ path: path.join(out, 'focus-repeats.png') });
  // change it and catch the move in flight
  await p.click('.row[data-id="repeats"] .jk[data-i="3"]');
  await p.waitForTimeout(330);
  await p.screenshot({ path: path.join(out, 'move-repeats-midair.png') });
  await p.waitForTimeout(900);
  await p.screenshot({ path: path.join(out, 'move-repeats-seated.png') });
  // drag a plug on the machine itself: the repeats plug from its jack to the first jack
  const drag = await p.evaluate(() => window.__bc.screen('repeats'));
  await p.mouse.move(drag.plug.x, drag.plug.y); await p.mouse.down();
  await p.mouse.move((drag.plug.x + drag.first.x) / 2, drag.first.y, { steps: 8 });
  await p.mouse.move(drag.first.x, drag.first.y, { steps: 8 }); await p.mouse.up();
  await p.waitForTimeout(900);
  const dragged = await p.evaluate(() => window.__bc.get('repeats'));
  if (dragged !== 0) problems.push('dragging the repeats plug to jack 1 left it at ' + dragged);
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
  // every setting: set each value and confirm the plug lands in that jack. A setting the machine can't change can't be
  // patched: its row names what it waits for, its jacks are disabled, clicking one or dragging its plug sends nothing,
  // and its plug stays where it is (parked, for one waiting for relay). The Light switch and Flash speed are live
  const ids = await p.evaluate(() => window.__bc.settings);
  const held = APP ? await p.evaluate(() => window.__bc.held()) : {};
  const WAITS = { carry: 'comes with relay', hops: 'comes with relay', hold: 'comes with relay', ...(ONLY_IN_PERSON ? { inperson: 'always, on this build' } : {}) };
  for (const id of ['light', 'flash']) if (held[id] || !ids.includes(id)) problems.push(`${id} is not a live row: ${JSON.stringify(held[id])}`);
  const untaped = await p.evaluate(() => ['light', 'flash'].map(id => { const r = document.querySelector(`.row[data-id="${id}"]`); return r.classList.contains('held') || r.classList.contains('parked') || [...r.querySelectorAll('.jk')].some(b => b.disabled) ? id : null; }).filter(Boolean));
  if (untaped.length) problems.push(`still taped on the card: ${untaped.join(', ')}`);
  const results = {};
  for (const id of ids) {
    const n = await p.evaluate(id => document.querySelectorAll(`.row[data-id="${id}"] .jk`).length, id);
    if (held[id]) {
      const before = await p.evaluate(id => ({ v: window.__bc.get(id), end: window.__bc.end(id), sets: __sim.settingsSet }), id);
      const row = await p.evaluate(id => { const r = document.querySelector(`.row[data-id="${id}"]`); return { text: r.querySelector('.val').textContent, pencil: r.querySelector('.val').classList.contains('pencil'),
        disabled: [...r.querySelectorAll('.jk')].every(b => b.disabled), checked: [...r.querySelectorAll('.jk')].findIndex(b => b.getAttribute('aria-checked') === 'true') }; }, id);
      await p.click(`.row[data-id="${id}"] .jk[data-i="${(before.v + 1) % n}"]`, { force: true });
      await p.evaluate(([id, t]) => window.__bc.set(id, t), [id, (before.v + 1) % n]);
      // and on the machine: grab its plug and carry it along the row
      await p.evaluate(id => window.__bc.focus(id), id);
      await p.waitForTimeout(1300);
      const plug = await p.evaluate(id => window.__bc.screen(id), id);
      await p.mouse.move(plug.plug.x, plug.plug.y); await p.mouse.down();
      await p.mouse.move(plug.plug.x + 160, plug.plug.y, { steps: 8 }); await p.mouse.up();
      await p.waitForTimeout(700);
      const after = await p.evaluate(id => ({ v: window.__bc.get(id), end: window.__bc.end(id), sets: __sim.settingsSet }), id);
      const why = [];
      if (row.text !== WAITS[id] || !row.pencil) why.push(`says ${JSON.stringify(row.text)}`);
      if (!row.disabled) why.push('its jacks can be pressed');
      if (held[id].parked ? (!before.end.parked || row.checked !== -1 || before.end.dim < .3) : (before.end.jack !== before.v || row.checked !== before.v)) why.push(`plug ${JSON.stringify(before.end)}, row jack ${row.checked}`);
      if (after.sets !== before.sets) why.push('it was sent');
      if (after.v !== before.v || after.end.jack !== before.end.jack || after.end.parked !== before.end.parked) why.push(`it moved: ${JSON.stringify(after)}`);
      results[id] = why.length ? why.join('; ') : `held: ${row.text}`;
      if (why.length) problems.push(`${id}: ${results[id]}`);
      continue;
    }
    const target = (await p.evaluate(id => window.__bc.get(id), id) + 1) % n;
    await p.evaluate(([id, t]) => window.__bc.set(id, t), [id, target]);
    await p.waitForTimeout(950);
    const end = await p.evaluate(id => window.__bc.end(id), id);
    results[id] = end.jack === target && end.lift < .2 ? 'ok' : `wanted jack ${target}, got ${JSON.stringify(end)}`;
    if (results[id] !== 'ok') problems.push(`${id}: ${results[id]}`);
  }
  if (APP) for (const id in WAITS) if (!held[id]) problems.push(`${id} is not held: the machine has nothing behind it yet`);
  await p.evaluate(() => window.__bc.focus(null));
  await p.screenshot({ path: path.join(out, 'after-all.png') });
  // keyboard: arrow keys move the plug
  await p.focus('.row[data-id="quiet"] .jk[aria-checked="true"]');
  const before = await p.evaluate(() => window.__bc.get('quiet'));
  await p.keyboard.press('ArrowRight'); await p.waitForTimeout(900);
  const after = await p.evaluate(() => window.__bc.get('quiet'));
  if (after === before) problems.push('arrow key did not change the setting');
  await p.context().close();
  // 2x details
  const hi = await open(vp, 'focus=repeats', 2);
  await hi.waitForTimeout(1800);
  await hi.screenshot({ path: path.join(out, 'detail-focus.png'), clip: { x: 440, y: 46, width: 1000, height: 854 } });
  await hi.screenshot({ path: path.join(out, 'detail-card.png'), clip: { x: 0, y: 46, width: 460, height: 854 } });
  await hi.context().close();
  // 2x crops at three widths: the card scrolled to Passing on, the whole back panel, and the waiting rows up close
  for (const width of [1024, 1440, 1512, 1728]) {
    const height = { 1024: 768, 1440: 900, 1512: 982, 1728: 1117 }[width];
    const w = await open({ width, height }, 'still', 2);
    const cardBox = await w.locator('.cardwrap').boundingBox();
    const viewBox = await w.locator('#view').boundingBox();
    await w.screenshot({ path: path.join(out, `x2-${width}-panel.png`), clip: viewBox });
    await w.evaluate(() => document.querySelector('.grp:nth-of-type(1)') && document.querySelector('.row[data-id="hops"]').scrollIntoView({ block: 'center' }));
    await w.waitForTimeout(200);
    await w.screenshot({ path: path.join(out, `x2-${width}-card-relay.png`), clip: cardBox });
    await w.evaluate(() => { const c = document.getElementById('sched'); c.scrollTop = c.scrollHeight; });
    await w.waitForTimeout(200);
    await w.screenshot({ path: path.join(out, `x2-${width}-card-privacy.png`), clip: cardBox });
    await w.evaluate(() => document.querySelector('.row[data-id="light"]').scrollIntoView({ block: 'center' }));
    await w.waitForTimeout(200);
    await w.screenshot({ path: path.join(out, `x2-${width}-card-light.png`), clip: cardBox });
    for (const id of ['hops', 'flash', 'inperson']) {
      await w.evaluate(id => window.__bc.focus(id), id);
      await w.waitForTimeout(1600);
      await w.screenshot({ path: path.join(out, `x2-${width}-panel-${id}.png`), clip: viewBox });
    }
    // a focused plug's tag stands clear of the row above: Flash speed's never covers the Light switch's jacks
    {
      await w.evaluate(() => window.__bc.focus('flash'));
      await w.waitForTimeout(1600);
      const clear = await w.evaluate(() => ({ tag: window.__bc.tag(), above: window.__bc.rowBand('light'), row: window.__bc.rowBand('flash') }));
      if (!clear.tag.on || clear.tag.top < clear.above.bottom || clear.tag.bottom > clear.row.top) problems.push(`${width}: the Flash speed tag overlaps a row of jacks: ${JSON.stringify(clear)}`);
    }
    await w.context().close();
  }
  // the back's first view shows its top at every window: the top bolt row, the column numbers and Row 1's sign all in
  // the view's top quarter and none of them cut, including the commissioner's 1512 by 982 fullscreen and the narrow
  // single-column layout. Where the whole panel fits (top bolts to the cable trough), say so.
  const framing = {};
  if (APP) for (const [width, height] of [[1024, 768], [1280, 800], [1440, 900], [1512, 982], [1728, 1117], [900, 1000], [720, 900]]) {
    const f = await open({ width, height }, 'still');
    const got = await f.evaluate(() => window.__bc.parts());
    const v = got.view, inside = b => b.left >= v.left && b.right <= v.right && b.top >= v.top && b.bottom <= v.bottom;
    const quarter = v.top + (v.bottom - v.top) * .25, why = [];
    for (const [name, list] of [['top bolt', got.topBolts], ['column number', got.colNums], ['Row 1 sign', [got.rowSigns[0]]]])
      list.forEach((b, i) => { if (!inside(b)) why.push(`${name} ${i} cut`); else if (b.bottom > quarter) why.push(`${name} ${i} below the top quarter (${Math.round(b.bottom - v.top)} of ${Math.round(v.bottom - v.top)})`); });
    // the hint never lies on the machine: it is in the top bar, or hidden where the bar has no room for it
    const hint = await f.evaluate(() => { const h = document.getElementById('hint'), r = h.getBoundingClientRect(), v = document.getElementById('view').getBoundingClientRect();
      return { shown: !!(r.width && r.height), over: r.left < v.right && v.left < r.right && r.top < v.bottom && v.top < r.bottom, cut: h.scrollWidth > h.clientWidth + 1 }; });
    if (hint.over) why.push('the hint lies over the machine');
    if (hint.shown && hint.cut) why.push('the hint is cut short');
    const conRow = got.connectors.slice(0, 12).sort((a, b) => a.left - b.left);
    framing[`${width}x${height}`] = { whole: inside(got.makersPlate) && got.groupPlates.every(inside), hint: hint.shown, topBolts: Math.round(got.topBolts[0].top - v.top), pitch: +(((conRow[11].left - conRow[0].left) / 11)).toFixed(1), problems: why.length };
    if (why.length) problems.push(`${width}x${height}: the back's first view does not show its top: ${[...new Set(why)].slice(0, 6).join('; ')}`);
    if (width === 1512) await f.screenshot({ path: path.join(out, 'overview-1512x982.png') });
    await f.context().close();
  }
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
    // every setting as the machine keeps it; the ones it can't change (relay, the key check) come as the device
    // reports them, and the page shows those from setting_notes, not from these values
    const all = { sound: 1, volume: 2, band: 0, light: 1, flash: 2, machine: 1, machvol: 2, carry: 1, hops: 6, hold: 2, quiet: 4, repeats: 2, lock: 1, expire: 3, inperson: 1 };
    const kept = Object.fromEntries(Object.entries(all).filter(([id]) => !(id in NOTES)));
    if (where === 'app') await ctx.addInitScript(({ kept, notes }) => {
      window.__calls = [];
      window.__TAURI__ = { event: { listen: () => Promise.resolve(() => {}) }, core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
        return Promise.resolve(cmd === 'settings' ? kept : cmd === 'setting_notes' ? notes : cmd === 'channels' ? [{ channel: 'sound', state: 'listening', rate: 16.1, band: 'near silent' }] : null); } } };
    }, { kept: all, notes: NOTES });
    else await ctx.addInitScript(kept => { try { sessionStorage.setItem('bombechat.simulated-machine', JSON.stringify(kept)); } catch (e) {} }, { sound: 1, light: 1, flash: 2, hops: 6, quiet: 4, expire: 3 });
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
    // whatever value comes for a setting the machine can't change, its plug is parked (or held where the note says)
    const heldEnds = await sp.evaluate(ids => Object.fromEntries(ids.map(id => [id, window.__bc.end(id)])), Object.keys(NOTES));
    for (const [id, n] of Object.entries(NOTES)) { const e = heldEnds[id]; if (Number.isInteger(n.value) ? e.jack !== n.value : !e.parked) problems.push(`${tag}: ${id}'s plug is at ${JSON.stringify(e)}, not ${Number.isInteger(n.value) ? 'held at ' + n.value : 'parked'}`); }
    if (!stale) await sp.screenshot({ path: path.join(out, `at-boot-${where}.png`), clip: { x: 0, y: 46, width: 460, height: 854 } });
    await sp.click('.row[data-id="sound"] .jk[data-i="2"]');
    await sp.waitForTimeout(200);
    const changed = await sp.evaluate(`(${sets})()`);
    const keptAfter = where === 'browser' ? await sp.evaluate(() => import('./bridge.js').then(m => m.bridge.settings())) : null;
    if (where === 'app' ? JSON.stringify(changed) !== JSON.stringify([{ id: 'sound', valueIndex: 2 }]) : changed !== 1 || keptAfter.sound !== 2) problems.push(`${tag}: choosing Listen and send sent ${JSON.stringify(changed)}, kept ${JSON.stringify(keptAfter)}`);
    atBoot[tag] = { shown, sent, changed };
    await ctx.close();
  }
  // back here by history: settings, to the front, turn the Sound switch there, then back. The page restored from the
  // back/forward cache reads the machine again: rows and plugs match what it keeps, and it sends nothing.
  let history = null;
  if (APP) {
    const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: 2 });
    await ctx.addInitScript(() => { window.__restored = 0; addEventListener('pageshow', e => { if (e.persisted) window.__restored++; }); });
    const hp = await ctx.newPage();
    hp.on('pageerror', e => problems.push('history: page error: ' + e.message));
    await hp.goto(url('still'), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await hp.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    const sound0 = await hp.evaluate(() => window.__bc.get('sound'));
    await hp.click('#back');
    await hp.waitForURL(/bombechat\.html/, { timeout: 10000 });
    await hp.waitForSelector('#k1 svg', { timeout: 60000 });
    await hp.waitForTimeout(400);
    await hp.click('#k1');
    await hp.waitForTimeout(700);
    const turnedTo = +(await hp.getAttribute('#k1', 'aria-valuenow'));
    await hp.goBack({ waitUntil: 'commit' });
    await hp.waitForURL(/settings\.html/, { timeout: 10000, waitUntil: 'commit' });
    await hp.waitForTimeout(800);
    history = await hp.evaluate(() => ({ restored: window.__restored, row: +document.querySelector('.row[data-id="sound"] .jk[aria-checked="true"]').dataset.i,
      label: document.querySelector('.row[data-id="sound"] .val').textContent, v: window.__bc.get('sound'), plug: window.__bc.end('sound').jack, sets: __sim.settingsSet }));
    history.before = sound0; history.turnedTo = turnedTo;
    if (!history.restored) problems.push('history: the settings page was not restored from the back/forward cache, so the refresh went untested');
    if (turnedTo === sound0) problems.push(`history: the front's Sound switch did not turn (${turnedTo})`);
    if (history.row !== turnedTo || history.v !== turnedTo || history.plug !== turnedTo) problems.push(`history: back on the settings page the Sound row shows ${JSON.stringify(history)}, the machine keeps ${turnedTo}`);
    if (history.sets) problems.push(`history: the restored page sent ${history.sets} settings`);
    await hp.screenshot({ path: path.join(out, 'history-back.png'), clip: { x: 0, y: 46, width: 460, height: 400 } });
    // the row's jack for the old value now changes it again (it is not swallowed as already set)
    await hp.click(`.row[data-id="sound"] .jk[data-i="${sound0}"]`);
    await hp.waitForTimeout(200);
    const again = await hp.evaluate(() => ({ sets: __sim.settingsSet, v: window.__bc.get('sound') }));
    if (again.sets !== 1 || again.v !== sound0) problems.push(`history: choosing ${sound0} again after the refresh gave ${JSON.stringify(again)}`);
    history.again = again;
    await ctx.close();

    // a machine that cannot say its settings: every row says Not read, Sound and Light sit at Off, nothing is sent
    const uctx = await browser.newContext({ viewport: vp, deviceScaleFactor: 2 });
    await uctx.addInitScript(() => {
      window.__calls = [];
      window.__TAURI__ = { event: { listen: () => Promise.resolve(() => {}) }, core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
        return cmd === 'settings' ? Promise.reject('the machine did not answer') : Promise.resolve(cmd === 'channels' ? [{ channel: 'sound', state: 'listening', rate: 16.1 }] : null); } } };
    });
    const up = await uctx.newPage();
    up.on('pageerror', e => problems.push('unread: page error: ' + e.message));
    await up.goto(url('still'), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await up.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    await up.waitForTimeout(300);
    const unread = await up.evaluate(() => ({ sound: window.__bc.get('sound'), light: window.__bc.get('light'), soundPlug: window.__bc.end('sound').jack, lightPlug: window.__bc.end('light').jack,
      labels: [...new Set([...document.querySelectorAll('.row .val')].map(e => e.textContent))], sets: __calls.filter(c => c[0] === 'set_setting').length }));
    if (unread.sound !== 0 || unread.light !== 0 || unread.soundPlug !== 0 || unread.lightPlug !== 0) problems.push(`unread: Sound and Light are not at Off: ${JSON.stringify(unread)}`);
    if (JSON.stringify(unread.labels) !== '["Not read"]') problems.push(`unread: rows do not say Not read: ${JSON.stringify(unread.labels)}`);
    if (unread.sets) problems.push(`unread: the page sent ${unread.sets} settings`);
    await up.screenshot({ path: path.join(out, 'unread.png'), clip: { x: 0, y: 46, width: 460, height: 854 } });
    history.unread = unread;
    await uctx.close();
  }
  // the machine refuses a change (the lock can't come off while the store is locked): the plug goes back, the row
  // shows the kept value again, and the reason is shown. And Tone band on a sound card too slow for near silent says so.
  let refused = null, band = null;
  if (APP) {
    const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: 2 });
    await ctx.addInitScript(notes => {
      window.__calls = [];
      window.__TAURI__ = { event: { listen: () => Promise.resolve(() => {}) }, core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
        if (cmd === 'set_setting' && args.id === 'lock') return Promise.reject('Unlock first, then the lock can come off');
        return Promise.resolve(cmd === 'settings' ? { sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 }
          : cmd === 'setting_notes' ? notes : cmd === 'channels' ? [{ channel: 'sound', state: 'listening', rate: 12.4, band: 'audible' }] : null); } } };
    }, NOTES);
    const rp = await ctx.newPage();
    rp.on('pageerror', e => problems.push('refused: page error: ' + e.message));
    rp.on('console', m => m.type() === 'error' && problems.push('refused: console: ' + m.text()));
    await rp.goto(url('still'), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await rp.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    await rp.waitForTimeout(400);
    band = await rp.evaluate(() => { const b = document.querySelector('.row[data-id="band"] .band'); return { shown: !b.hidden, text: b.textContent, value: document.querySelector('.row[data-id="band"] .val').textContent }; });
    if (!band.shown || !/audible/.test(band.text) || !/near silent/.test(band.text)) problems.push(`band: a sound card too slow for near silent is not shown: ${JSON.stringify(band)}`);
    await rp.evaluate(() => document.querySelector('.row[data-id="band"]').scrollIntoView({ block: 'center' }));
    await rp.screenshot({ path: path.join(out, 'band-too-slow.png'), clip: { x: 0, y: 46, width: 460, height: 500 } });
    await rp.click('.row[data-id="lock"] .jk[data-i="0"]');
    await rp.waitForTimeout(1400);
    refused = await rp.evaluate(() => ({ v: window.__bc.get('lock'), plug: window.__bc.end('lock').jack, row: +document.querySelector('.row[data-id="lock"] .jk[aria-checked="true"]').dataset.i,
      label: document.querySelector('.row[data-id="lock"] .val').textContent, saved: document.getElementById('saved').textContent, sent: __calls.filter(c => c[0] === 'set_setting').length }));
    if (refused.v !== 1 || refused.plug !== 1 || refused.row !== 1 || refused.label !== 'On') problems.push(`refused: the lock row did not go back to On: ${JSON.stringify(refused)}`);
    if (refused.saved !== 'Unlock first, then the lock can come off') problems.push(`refused: the reason is not shown: ${JSON.stringify(refused)}`);
    if (refused.sent !== 1) problems.push(`refused: ${refused.sent} settings sent for one change`);
    await rp.screenshot({ path: path.join(out, 'refused-lock.png') });
    await ctx.close();
  }
  // the Keychain would not keep the key at unlock, so the lock went back on: the Lock row says so under its name, and
  // still works; setting it again sends that one change and the notice goes
  let lockNotice = null;
  if (APP) {
    const LOCK_BACK_ON = 'The Keychain did not keep the key, so the lock is back on';
    const ctx = await browser.newContext({ viewport: vp, deviceScaleFactor: 2 });
    await ctx.addInitScript(notes => {
      window.__calls = [];
      window.__TAURI__ = { event: { listen: () => Promise.resolve(() => {}) }, core: { invoke: (cmd, args) => { window.__calls.push([cmd, args]);
        return Promise.resolve(cmd === 'settings' ? { sound: 2, volume: 2, band: 0, light: 0, flash: 1, machine: 1, machvol: 2, carry: 1, hops: 3, hold: 2, quiet: 2, repeats: 2, lock: 1, expire: 0, inperson: 1 }
          : cmd === 'setting_notes' ? notes : cmd === 'channels' ? [{ channel: 'sound', state: 'listening', rate: 16.1, band: 'near silent' }] : null); } } };
    }, { ...NOTES, lock: { note: LOCK_BACK_ON, notice: true } });
    const lp = await ctx.newPage();
    lp.on('pageerror', e => problems.push('lock notice: page error: ' + e.message));
    lp.on('console', m => m.type() === 'error' && problems.push('lock notice: console: ' + m.text()));
    await lp.goto(url('still'), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await lp.waitForFunction(() => window.__ready, null, { timeout: 90000 });
    await lp.waitForTimeout(400);
    const row = () => lp.evaluate(() => { const r = document.querySelector('.row[data-id="lock"]'), n = r.querySelector('.notice');
      return { notice: n ? n.textContent : null, size: n ? parseFloat(getComputedStyle(n).fontSize) : null, held: r.classList.contains('held'), label: r.querySelector('.val').textContent,
        enabled: [...r.querySelectorAll('.jk')].every(b => !b.disabled), described: r.querySelector('.jacks').getAttribute('aria-describedby'), sent: __calls.filter(c => c[0] === 'set_setting').map(c => c[1]) }; });
    const shown = await row();
    if (shown.notice !== LOCK_BACK_ON || shown.held || shown.label !== 'On' || !shown.enabled || shown.described !== 'notice-lock' || shown.size < 12) problems.push(`lock notice: not shown on a working Lock row: ${JSON.stringify(shown)}`);
    await lp.evaluate(() => document.querySelector('.row[data-id="lock"]').scrollIntoView({ block: 'center' }));
    await lp.waitForTimeout(300);
    const lb = await lp.locator('.row[data-id="lock"]').boundingBox();
    await lp.screenshot({ path: path.join(out, 'lock-back-on.png'), clip: { x: 0, y: lb.y - 60, width: 460, height: lb.height + 120 } });
    await lp.click('.row[data-id="lock"] .jk[data-i="0"]');
    await lp.waitForTimeout(300);
    const after = await row();
    if (after.notice !== null || after.described !== null || JSON.stringify(after.sent) !== JSON.stringify([{ id: 'lock', valueIndex: 0 }])) problems.push(`lock notice: setting the lock again: ${JSON.stringify(after)}`);
    lockNotice = { shown, after };
    await ctx.close();
  }
  // a held press: on a row not yet opened, pressing a jack and holding it 60, 120 or 200 ms before letting go still
  // plugs that jack (the row's description must not open under the pointer and push the jacks away mid-press)
  // (the app's interface only: the frozen mockup keeps the old behaviour)
  const heldPress = {};
  for (const ms of APP ? [60, 120, 200] : []) {
    const hp = await open(vp, 'still');
    const row = '.row[data-id="machvol"]';
    await hp.evaluate(r => document.querySelector(r).scrollIntoView({ block: 'center' }), row);
    await hp.waitForTimeout(200);
    const was = await hp.evaluate(() => window.__bc.get('machvol'));
    const to = was === 0 ? 1 : 0;
    const jb = await hp.locator(`${row} .jk[data-i="${to}"] i`).boundingBox();
    await hp.mouse.move(jb.x + jb.width / 2, jb.y + jb.height / 2);
    await hp.mouse.down(); await hp.waitForTimeout(ms); await hp.mouse.up();
    await hp.waitForTimeout(400);
    const now = await hp.evaluate(() => window.__bc.get('machvol'));
    heldPress[ms] = { was, now };
    if (now !== to) problems.push(`a press held ${ms} ms on a jack of an unopened row missed it: ${JSON.stringify({ was, now, to })}`);
    await hp.context().close();
  }
  // keyboard focus still opens a row's description, as before
  if (APP) {
    const kp = await open(vp, 'still');
    await kp.focus('.row[data-id="machvol"] .jk[aria-checked="true"]');
    await kp.keyboard.press('Shift+Tab'); await kp.keyboard.press('Tab');
    await kp.waitForTimeout(400);
    const opened = await kp.evaluate(() => { const r = document.querySelector('.row[data-id="machvol"]'); return { focus: r.classList.contains('focus'), desc: r.querySelector('.desc').getBoundingClientRect().height }; });
    heldPress.keyboard = opened;
    if (!opened.focus || opened.desc < 10) problems.push(`keyboard focus no longer opens the row's description: ${JSON.stringify(opened)}`);
    await kp.context().close();
  }
  await browser.close();
  // without WebGL the back is the wiring schedule alone: the view says so, no hint about plugs on a machine that is not
  // drawn, every setting still changes from the card, and nothing goes wrong on the page (three.js saying it found no
  // WebGL is the fallback being taken)
  let flat = null;
  if (APP) {
    const fb = await chromium.launch({ channel: 'chrome', args: ['--disable-webgl', '--disable-3d-apis'] });
    const fp = await (await fb.newContext({ viewport: vp, deviceScaleFactor: 2 })).newPage();
    fp.on('pageerror', e => problems.push('flat: page error: ' + e.message));
    fp.on('console', m => m.type() === 'error' && !/^THREE\.WebGLRenderer: (A WebGL context could not be created|Error creating WebGL context)/.test(m.text()) && problems.push('flat: console: ' + m.text()));
    fp.on('request', r => { const u = r.url(); if (!/^(data:|blob:|http:\/\/127\.0\.0\.1:4811\/)/.test(u)) problems.push('flat: asked the network for ' + u); });
    await fp.goto(url('still'), { waitUntil: 'domcontentloaded', timeout: 120000 });
    await fp.waitForFunction(() => getComputedStyle(document.getElementById('fallback')).display !== 'none', null, { timeout: 60000 });
    const was = await fp.evaluate(() => window.__bc.get('volume'));
    await fp.click(`.row[data-id="volume"] .jk[data-i="${(was + 1) % 5}"]`);
    // the plug moved, as the card shows it (the check below says if it never did)
    await fp.waitForFunction(w => window.__bc.get('volume') !== w && document.querySelector('.row[data-id="volume"] .val').textContent === String((w + 1) % 5 + 1), was, { timeout: 5000 }).catch(() => {});
    flat = await fp.evaluate(() => ({ fallback: getComputedStyle(document.getElementById('fallback')).display, hint: !!document.getElementById('hint').getBoundingClientRect().width,
      volume: window.__bc.get('volume'), label: document.querySelector('.row[data-id="volume"] .val').textContent }));
    flat.was = was;
    if (flat.fallback === 'none' || flat.hint || flat.volume !== (was + 1) % 5 || flat.label !== String((was + 1) % 5 + 1)) problems.push(`flat: the back without WebGL: ${JSON.stringify(flat)}`);
    await fp.screenshot({ path: path.join(out, 'flat.png') });
    await fb.close();
  }
  console.log(JSON.stringify({ out, fps, field, framing, flat, results, keyboard: [before, after], atBoot, history, refused, band, lockNotice, heldPress, problems }, null, 2));
  server.close();
  process.exit(problems.length ? 1 : 0);
})();
