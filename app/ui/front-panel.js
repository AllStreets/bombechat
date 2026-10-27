// The front of the machine in 3D: one wrinkle-finish enamel panel under everything below the top bar, the drum bank let
// into it as a bay, and the hardware of the lower deck. Built from machine-kit.js, so it is the same enamel, brass,
// steel, bakelite, glass and light as the back panel (settings.html).
//
// The HTML stays the model, the controls and the fallback: every part here is placed from the box of the HTML part it
// stands for, read on each layout, and the HTML draws whatever is read or typed (paper, plates, digits). The scene
// draws only when something changed: a drum or a knob turning, a jewel, a layout.
import { createMachine, engravingFace, engravingFont, engraveText } from './machine-kit.js';

// mountFrontPanel({app, bank, deck, cells, still, ...}) -> {ready, layout(), knob(id, pos), jewel(id, on), dirty(), renders()}
//   cells: the drum cells as the page keeps them ({el, wl, angle, from, t0, snap})
//   rims: the drums' rim colours; knobs: the page's KNOBS ({labels}); place, line, angles: where the knob's stops are
//   engraved, in the knob's SVG units (radius 25.5); api: window.__bank, which gains placement() and rails();
//   boxIn, turnedAbove: the page's measuring through a turn; onFrame: called each animation frame
export async function mountFrontPanel({ app, bank, deck, cells, still = false, rims, knobs = {}, place = [], line = 12.6, angles = [-120, 0, 120],
  api = {}, boxIn, turnedAbove, rm = matchMedia('(prefers-reduced-motion: reduce)'), noao = false, onFrame = () => {} }) {
  await engravingFace();
  const host = document.createElement('div'); host.className = 'front3d'; host.setAttribute('aria-hidden', 'true');
  app.prepend(host);
  let K;
  try { K = createMachine(host, { still, background: '#151614' }); } catch (e) { host.remove(); throw e; }
  const { THREE, renderer, scene, camera, M, tex, key, rim } = K;
  app.classList.add('gl'); bank.classList.add('gl'); api.gl = true;
  key.position.set(-6, 9, 22); key.intensity = 2.7; rim.intensity = .45; key.shadow.mapSize.set(4096, 4096);

  /* ---------- the drums (as they were in bombechat.html) ---------- */
  // the drums' bay is let into the panel this deep: the plates, numbers, axles and pointers stand on its floor
  const BAY = .14;
  const R = .46;// drum radius in world units; one unit = drum width / 0.92
  const ringTex = K.canvasTex(1024, 1024, (k, w) => { const c = w / 2;
    const g = k.createRadialGradient(c * .8, c * .6, c * .1, c, c, c); g.addColorStop(0, '#2e2a26'); g.addColorStop(.65, '#161412'); g.addColorStop(1, '#0a0908'); k.fillStyle = g; k.fillRect(0, 0, w, w);
    k.translate(c, c); k.fillStyle = '#d8d0ba'; k.font = engravingFont(90); k.textAlign = 'center'; k.textBaseline = 'middle';
    for (let i = 0; i < 26; i++) { k.save(); k.rotate(i * 2 * Math.PI / 26); engraveText(k, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'[i], 0, -c * .735, 90, .06); k.restore(); }
    k.strokeStyle = 'rgba(220,214,200,.28)'; k.lineWidth = 3; for (let i = 0; i < 26; i++) { k.save(); k.rotate((i + .5) * 2 * Math.PI / 26); k.beginPath(); k.moveTo(0, -c * .97); k.lineTo(0, -c * .88); k.stroke(); k.restore(); }
    k.strokeStyle = 'rgba(220,214,200,.1)'; k.lineWidth = 2; k.beginPath(); k.arc(0, 0, c * .5, 0, 7); k.stroke();
    for (let i = 0; i < 4; i++) { k.save(); k.rotate(Math.PI / 4 + i * Math.PI / 2); k.fillStyle = '#070605'; k.beginPath(); k.arc(0, -c * .37, c * .045, 0, 7); k.fill(); k.strokeStyle = 'rgba(220,214,200,.2)'; k.stroke(); k.restore(); } });
  ringTex.wrapS = ringTex.wrapT = THREE.ClampToEdgeWrapping; ringTex.colorSpace = THREE.SRGBColorSpace; ringTex.anisotropy = 16;
  const faceMat = new THREE.MeshStandardMaterial({ map: ringTex, roughness: .55, metalness: .05 });
  // rim: a turned profile with a bevel on both edges
  const rimGeo = (() => { const p = [[.395, 0], [.445, 0], [.462, .012], [.47, .04], [.47, .17], [.462, .2], [.445, .212], [.395, .212]].map(([r, y]) => new THREE.Vector2(r, y)); const g = new THREE.LatheGeometry(p, 96); g.rotateX(Math.PI / 2); return g; })();
  const faceGeo = new THREE.CircleGeometry(.402, 96);
  const hubGeo = (() => { const p = [[0, .0], [.075, 0], [.08, .01], [.074, .034], [.05, .052], [0, .058]].map(([r, y]) => new THREE.Vector2(r, y)); const g = new THREE.LatheGeometry(p, 48); g.rotateX(Math.PI / 2); return g; })();
  const slotGeo = new THREE.BoxGeometry(.1, .012, .02);
  const shaftGeo = new THREE.CylinderGeometry(.03, .03, .2, 16); shaftGeo.rotateX(Math.PI / 2);
  const rimMats = {}; const rimMat = c => rimMats[c] || (rimMats[c] = new THREE.MeshPhysicalMaterial({ color: new THREE.Color(c).multiplyScalar(.82), roughness: .46, metalness: .05, clearcoat: .45, clearcoatRoughness: .4, bumpMap: tex.wrinkle, bumpScale: .15 }));
  const tabMatBase = { roughness: .45, metalness: 0 };
  const pointerGeo = (() => { const s = new THREE.Shape(); s.moveTo(-.045, 0); s.lineTo(.045, 0); s.lineTo(0, -.07); s.closePath(); return new THREE.ExtrudeGeometry(s, { depth: .02, bevelEnabled: true, bevelThickness: .006, bevelSize: .006, bevelSegments: 2 }); })();
  const railMat = new THREE.MeshStandardMaterial({ color: '#8e8b84', roughness: .3, metalness: 1 });

  const drums = cells.map((cell, i) => { const r = Math.floor(i / 12), c = i % 12;
    const g = new THREE.Group(); scene.add(g);
    const rimM = new THREE.Mesh(rimGeo, rimMat(rims[(c * 5 + r * 3) % 12])); rimM.castShadow = true; rimM.receiveShadow = true;
    const rot = new THREE.Group();
    const face = new THREE.Mesh(faceGeo, faceMat); face.position.z = .19; face.receiveShadow = true;
    const hub = new THREE.Mesh(hubGeo, M.steel); hub.position.z = .192; hub.castShadow = true;
    const slot = new THREE.Mesh(slotGeo, M.hole); slot.position.z = .247; slot.rotation.z = -.5;
    rot.add(rimM, face, hub, slot); g.add(rot);
    const shaft = new THREE.Mesh(shaftGeo, M.steel); shaft.position.z = -.08; g.add(shaft);
    // reading window: cream enamel tab, fixed, with the letter
    const cv = document.createElement('canvas'); cv.width = cv.height = 128; const wt = new THREE.CanvasTexture(cv); wt.colorSpace = THREE.SRGBColorSpace; wt.anisotropy = 8;
    const side = () => new THREE.MeshStandardMaterial({ color: '#e9e1cb', ...tabMatBase });
    const tab = new THREE.Mesh(new THREE.BoxGeometry(.26, .25, .03), [side(), side(), side(), side(), new THREE.MeshStandardMaterial({ map: wt, ...tabMatBase }), new THREE.MeshStandardMaterial({ color: '#e9e1cb' })]);
    tab.position.set(0, R * .645, .225); tab.castShadow = true; g.add(tab);
    const ptr = new THREE.Mesh(pointerGeo, M.brass); ptr.position.set(0, R + .1, -BAY + .03); ptr.scale.setScalar(1.35); ptr.castShadow = true; g.add(ptr);
    return { cell, g, rot, cv, wt, last: '', r, c }; });
  function paintWindow(d) { const wl = d.cell.wl; const ch = wl.textContent, fill = wl.getAttribute('fill') || '#15130f', op = +(wl.getAttribute('opacity') || 1) * (+wl.style.opacity === 0 && wl.style.opacity !== '' ? 0 : 1);
    const sig = ch + fill + op; if (sig === d.last) return false; d.last = sig;
    const k = d.cv.getContext('2d'); k.fillStyle = '#ece4cd'; k.fillRect(0, 0, 128, 128); const sh = k.createLinearGradient(0, 0, 0, 128); sh.addColorStop(0, 'rgba(0,0,0,.09)'); sh.addColorStop(.25, 'rgba(0,0,0,0)'); k.fillStyle = sh; k.fillRect(0, 0, 128, 128);
    k.globalAlpha = op; k.fillStyle = fill; k.font = engravingFont(98); k.textAlign = 'center'; k.textBaseline = 'middle'; engraveText(k, ch, 64, 69, 98, .085); k.globalAlpha = 1;
    k.strokeStyle = 'rgba(0,0,0,.35)'; k.lineWidth = 4; k.strokeRect(2, 2, 124, 124); d.wt.needsUpdate = true; return true; }

  // the drums' axles, one steel shaft behind each row; nothing runs between the rows
  const rails = []; for (let i = 0; i < 3; i++) { const m = new THREE.Mesh(new THREE.CylinderGeometry(.05, .05, 1, 24), railMat); m.rotation.z = Math.PI / 2; m.castShadow = true; m.receiveShadow = true; scene.add(m); rails.push(m); }
  // for checks: the axles, and any rail between rows (a steel rod in the scene that is not an axle)
  api.rails = () => ({ between: scene.children.filter(m => m.material === railMat && !rails.includes(m)).length, axles: rails.length });
  const plates = ['Plain', 'Heard', 'Key'].map(t => K.plate(t.toUpperCase(), 0, 0, { h: .2, size: 56, pad: .7 }));
  // slotted steel screws, as on every other plate (the CSS plates and the back panel's bolts)
  const screwGeo = new THREE.CylinderGeometry(.035, .035, .025, 20); screwGeo.rotateX(Math.PI / 2); const screwSlot = new THREE.BoxGeometry(.052, .009, .008);
  const SLOTS = [.62, -.48, .85, -.7, .4, -.9];
  const plateScrews = plates.map((_, r) => [0, 1].map(i => { const g = new THREE.Group(); const h = new THREE.Mesh(screwGeo, M.steel); h.castShadow = true; const s = new THREE.Mesh(screwSlot, M.hole); s.position.z = .0135; s.rotation.z = SLOTS[r * 2 + i]; g.add(h, s); scene.add(g); return g; }));
  const nums = [...Array(12)].map((_, i) => K.engrave(String(i + 1), 0, 0, .13, '#b8b09a'));

  /* ---------- the panel ---------- */
  const floor = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), M.panel); floor.receiveShadow = true; scene.add(floor);
  // the floor's grain as the panel's, tile for tile
  const GRAIN = [9.8, 9];
  function floorTo(c, w, h) { floor.geometry.dispose(); const g = new THREE.PlaneGeometry(w, h), uv = g.attributes.uv, p = g.attributes.position;
    for (let i = 0; i < uv.count; i++) uv.setXY(i, (p.getX(i) + c.x) / GRAIN[0], (p.getY(i) + c.y) / GRAIN[1]); floor.geometry = g; floor.position.set(c.x, c.y, -BAY); }
  let front = null;// the panel, its frame and bolt ring: rebuilt when the bay or the window changes shape

  /* ---------- the bank's foot: odometer bezels, wheels and lamp sockets ---------- */
  const bezelMat = new THREE.MeshStandardMaterial({ color: '#2a2a27', roughness: .38, metalness: .9 });
  const wheelMat = new THREE.MeshPhysicalMaterial({ color: '#141311', roughness: .38, metalness: 0, clearcoat: .5, clearcoatRoughness: .3 });
  const windowMat = new THREE.MeshStandardMaterial({ color: '#030303', roughness: .9 });
  // a rounded frame round a window w by h (units), r its corner, t its width, d its depth
  function frameGeo(w, h, t, d, r = .02) {
    const rr = (p, w, h, r, hole) => { const x = -w / 2, y = -h / 2; p.moveTo(x + r, y); p.lineTo(x + w - r, y); p.quadraticCurveTo(x + w, y, x + w, y + r); p.lineTo(x + w, y + h - r); p.quadraticCurveTo(x + w, y + h, x + w - r, y + h); p.lineTo(x + r, y + h); p.quadraticCurveTo(x, y + h, x, y + h - r); p.lineTo(x, y + r); p.quadraticCurveTo(x, y, x + r, y); return p; };
    const s = rr(new THREE.Shape(), w + 2 * t, h + 2 * t, r + t * .6); s.holes.push(rr(new THREE.Path(), w, h, r));
    const b = Math.min(t * .45, d * .45);
    const g = new THREE.ExtrudeGeometry(s, { depth: Math.max(.001, d - 2 * b), bevelEnabled: true, bevelThickness: b, bevelSize: b * .9, bevelSegments: 3, curveSegments: 6 });
    g.translate(0, 0, b); return g;
  }
  const footParts = new THREE.Group(); scene.add(footParts);
  const wheelGeo = (() => { const g = new THREE.CylinderGeometry(1, 1, 1, 32, 1, false, -Math.PI * .32, Math.PI * .64); g.rotateZ(Math.PI / 2); g.rotateX(-Math.PI / 2); return g; })();
  const lamps = new Map();// HTML lamp (or jewel) -> {j, el, color()}

  function clear(g) { for (const o of [...g.children]) { g.remove(o); o.traverse(m => { if (m.isMesh && m.geometry && !m.geometry.userData.keep) m.geometry.dispose(); }); } }

  /* ---------- where things go ---------- */
  let U = 1, W = 1, H = 1, D = 10;
  const css = (el, k) => parseFloat(getComputedStyle(el)[k]) || 0;
  // laid out mid-turn, to the pixel: laid out again from the rects once the turn is over (frame)
  let laidTurned = false;
  const box = el => { const b = boxIn(el, host); if (b.turned) laidTurned = true; return b; };
  // the world point at host pixel (px, py) seen at depth z: things standing off the panel are set back toward the
  // middle by as much as the lens pushes them out, so each sits exactly over the HTML it stands for
  const at = (px, py, z = 0) => { const s = (D - z) / D; return new THREE.Vector3((px - W / 2) / U * s, -(py - H / 2) / U * s, z); };
  const shown = el => !!el && el.isConnected && el.offsetParent !== null && getComputedStyle(el).visibility !== 'hidden';

  function layoutFoot() {
    clear(footParts);
    for (const odo of app.querySelectorAll('.bank .odo, .panel .counter .odo')) {
      if (!shown(odo)) continue;
      const b = box(odo), z0 = odo.closest('.bank') ? -BAY : 0, t = 3.2 / U, d = .05;
      const g = new THREE.Group(); footParts.add(g);
      const c = at(b.x + b.w / 2, b.y + b.h / 2, z0); g.position.copy(c);
      const fr = new THREE.Mesh(frameGeo(b.w / U, b.h / U, t, d, 1.5 / U), bezelMat); fr.castShadow = true; fr.receiveShadow = true; g.add(fr);
      const win = new THREE.Mesh(new THREE.PlaneGeometry(b.w / U + .01, b.h / U + .01), windowMat); win.position.z = .001; g.add(win);
      // a wheel behind each digit, its face curving away top and bottom
      for (const i of odo.querySelectorAll('i')) {
        const r = box(i), wr = r.h / U * .62;
        const m = new THREE.Mesh(wheelGeo, wheelMat); m.scale.set(r.w / U * .96, wr, wr);
        const p = at(r.x + r.w / 2, r.y + r.h / 2, z0); m.position.set(p.x - c.x, p.y - c.y, .02 - wr * .25); m.receiveShadow = true; g.add(m);
      }
    }
    // the frames-checked lamps: a jewel in a socket for each, lit as the HTML lamp is
    const seen = new Set();
    for (const i of app.querySelectorAll('.lampset i')) {
      if (!shown(i)) continue; seen.add(i);
      const b = box(i), r = (b.w / 2 + 1.5) / U;
      let L = lamps.get(i);
      if (!L) { const j = K.jewel('#f0b64c', { radius: r, light: false, parent: footParts }); L = { j, el: i, kind: 'lamp' }; lamps.set(i, L); }
      else footParts.add(L.j.group);
      L.j.group.scale.setScalar(r); L.j.group.position.copy(at(b.x + b.w / 2, b.y + b.h / 2, -BAY));
      syncLamp(L);
    }
    for (const [el, L] of lamps) if (L.kind === 'lamp' && !seen.has(el)) lamps.delete(el);
  }
  // a lamp's state, as its HTML has it: n now (amber), d done (warm white), otherwise dark
  function syncLamp(L) { const c = L.el.classList; return L.j.set(c.contains('n') || c.contains('d'), c.contains('d') ? '#f4ecd6' : '#f0b64c'); }

  function layout() {
    laidTurned = false; W = css(host, 'width'); H = css(host, 'height'); if (!W || !H) return;
    const dr = box(cells[0].el).w; U = dr / .92;
    renderer.setSize(W, H, false); camera.aspect = W / H;
    const bk = box(bank);
    // the lens as the bank always had it (its depth from the bank's height), now looking at the whole panel
    D = Math.max((bk.h / U / 2) / Math.tan(19 * Math.PI / 360), (H / U / 2) / Math.tan(13 * Math.PI / 180));
    camera.fov = 2 * Math.atan((H / U / 2) / D) * 180 / Math.PI; camera.position.set(0, 0, D); camera.lookAt(0, 0, 0); camera.updateProjectionMatrix(); composer.setSize(W, H);
    // the panel: one sheet of enamel, the bay let into it, bolted all round at the window's edge
    if (front) { scene.remove(front.group); front.panel.geometry.dispose(); front.frame.geometry.dispose(); }
    const fg = new THREE.Group(); scene.add(fg);
    const bc = at(bk.x + bk.w / 2, bk.y + bk.h / 2, 0);
    const edge = 8 / U, bs = 4.2 / U / .09;
    const per = K.perimeter(W / U, H / U, 0, { across: 13, down: Math.max(3, Math.round(H / W * 13 * .78)), inset: edge, scale: bs, z: .006, holes: [{ x: bc.x, y: bc.y, w: bk.w / U, h: bk.h / U, r: 3 / U }], grain: GRAIN, parent: fg });
    per.frame.visible = false;
    front = { group: fg, ...per };
    floorTo(at(bk.x + bk.w / 2, bk.y + bk.h / 2, -BAY), bk.w / U + .3, bk.h / U + .3);
    drums.forEach(d => { const r = box(d.cell.el); d.g.position.copy(at(r.x + r.w / 2, r.y + r.w * .08 + r.w / 2, 0)); });
    const rowY = [0, 1, 2].map(r => drums[r * 12].g.position.y); const x0 = drums[0].g.position.x, x1 = drums[11].g.position.x;
    // a shaft runs behind each row
    [0, 1, 2].forEach(r => { const m = rails[r], s = (D + BAY - .05) / D; m.position.set((x0 + x1) / 2 * s, rowY[r] * s, -BAY + .05); m.scale.set(1, ((x1 - x0) + 1.2) * s, 1); });
    const lab = [...app.querySelectorAll('.rowlab')];
    const ps = (21 / U) / .2;
    plates.forEach((p, r) => { p.scale.set(ps, ps, 1); const lr = box(lab[r]); const w = p.geometry.parameters.width * ps;
      const yPix = H / 2 - rowY[r] * U, left = at(lr.x + 4, yPix, -BAY + .03);
      p.position.set(left.x + w / 2, left.y, -BAY + .03);
      // the screws sit in the plate's ends, 8 px in, as the printed plates have them
      plateScrews[r][0].position.set(p.position.x - w / 2 + 8 / U, left.y, -BAY + .055); plateScrews[r][1].position.set(p.position.x + w / 2 - 8 / U, left.y, -BAY + .055); plateScrews[r].forEach(m => m.scale.setScalar((6 / U) / .07)); });
    const cn = [...app.querySelectorAll('.colno')]; nums.forEach((n, i) => { const r = box(cn[i]); const p = at(r.x + r.w / 2, r.y + r.h / 2, -BAY + .005); n.position.set(p.x, p.y + .07, -BAY + .005); n.scale.setScalar(Math.max(1, (13 / U) / .13)); });
    layoutFoot();
    for (const f of later) f({ box, at, U, W, H, D });
    // the key's shadow covers the whole panel
    Object.assign(key.shadow.camera, { left: -W / U / 2 - 1, right: W / U / 2 + 1, top: H / U / 2 + 1, bottom: -H / U / 2 - 1, near: 1, far: 60 }); key.shadow.camera.updateProjectionMatrix();
    renderer.shadowMap.needsUpdate = true; dirty = true;
  }
  // parts 6.3 adds lay themselves out with the rest
  const later = [];

  let dirty = true, renders = 0;
  const composer = K.makeComposer({ ao: !noao });
  // laid out again on the next frame, once whatever changed has settled
  let relay = false; const soon = () => { relay = true; };
  new ResizeObserver(soon).observe(host);
  for (const el of [bank, deck, ...deck.querySelectorAll('.panel, .mods, .row2, .row3, .outbox, .bankfoot')]) new ResizeObserver(soon).observe(el);
  new ResizeObserver(soon).observe(bank.querySelector('.bankfoot'));
  addEventListener('resize', soon);
  // and once a turn ends, though nothing changed size: back from the settings page, by history or on load
  app.addEventListener('transitionend', e => { if (e.target === e.currentTarget) layout(); });
  addEventListener('pageshow', layout); document.body.getAnimations().forEach(a => a.finished.then(layout, () => {}));
  // what the HTML shows changing: a lamp, a readout appearing, a station or a slip added
  const mo = new MutationObserver(list => { for (const m of list) { if (m.type === 'attributes' && m.attributeName === 'class') { const L = lamps.get(m.target); if (L) { if (syncLamp(L)) dirty = true; continue; } } relay = true; } });
  mo.observe(bank.querySelector('.bankfoot'), { subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'style', 'hidden'] });
  mo.observe(deck.querySelector('.panel') || deck, { subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'style', 'hidden'] });

  // for checks: where the scene draws each part and where its HTML stands, in bank pixels, at rest. Each drum's centre
  // (and its axle's height), each row plate's left end (its label's left plus 4 px) and each column number's middle
  // (drawn .07 up, by design)
  api.placement = () => { camera.updateMatrixWorld(); const br = bank.getBoundingClientRect(), hr = host.getBoundingClientRect();
    const px = v => { const q = v.clone().project(camera); return { x: (q.x + 1) / 2 * W + hr.left - br.left, y: (1 - q.y) / 2 * H + hr.top - br.top }; };
    const rel = el => { const r = el.getBoundingClientRect(); return { l: r.left - br.left, t: r.top - br.top, w: r.width, h: r.height }; };
    const lab = [...app.querySelectorAll('.rowlab')], cn = [...app.querySelectorAll('.colno')];
    return { drums: drums.map(d => { const r = rel(d.cell.el); const c = px(d.g.position);
        return { at: `drum ${d.r + 1}.${d.c + 1}`, drawn: c, html: { x: r.l + r.w / 2, y: r.t + r.w * .08 + r.w / 2 }, axle: px(new THREE.Vector3(c.x, rails[d.r].position.y, rails[d.r].position.z)).y, centre: c.y }; }),
      plates: plates.map((p, r) => { const w = p.geometry.parameters.width * p.scale.x; return { at: `plate ${r + 1}`, drawn: { x: px(new THREE.Vector3(p.position.x - w / 2, p.position.y, p.position.z)).x }, html: { x: rel(lab[r]).l + 4 } }; }),
      numbers: nums.map((n, i) => { const r = rel(cn[i]); return { at: `number ${i + 1}`, drawn: px(new THREE.Vector3(n.position.x, n.position.y - .07, n.position.z)), html: { x: r.l + r.w / 2, y: r.t + r.h / 2 } }; }) }; };
  document.fonts && document.fonts.ready.then(() => { drums.forEach(d => d.last = ''); layout(); });

  // the same curve as the CSS turn: cubic-bezier(.2,.85,.25,1.04) over .95 s
  function bez(x) { const cx = 3 * .2, bx = 3 * (.25 - .2) - cx, ax = 1 - cx - bx, cy = 3 * .85, by = 3 * (1.04 - .85) - cy, ay = 1 - cy - by; let t = x;
    for (let i = 0; i < 6; i++) { const fx = ((ax * t + bx) * t + cx) * t - x, d = (3 * ax * t + 2 * bx) * t + cx; if (Math.abs(fx) < 1e-5) break; t -= fx / d; } return ((ay * t + by) * t + cy) * t; }
  const DUR = 950; let frames = 0;
  const ticking = [];// parts that animate on their own (knobs): tick(now) true while moving
  const self = { ready: false, layout, dirty() { dirty = true; }, renders: () => renders, knob() {}, jewel() {}, K, scene, at, box: el => box(el), later, ticking, lamps, syncLamp, soon, host, get U() { return U; }, get D() { return D; } };
  function frame(now) {
    let moving = false;
    if (relay || (laidTurned && !turnedAbove(host))) { relay = false; layout(); }
    for (const d of drums) { const c = d.cell; let ang = c.angle;
      if (!c.snap) { const k = (now - c.t0) / DUR; if (k < 1) { ang = c.from + (c.angle - c.from) * bez(Math.max(0, k)); moving = true; } }
      const z = -ang * Math.PI / 180; if (d.rot.rotation.z !== z) { d.rot.rotation.z = z; dirty = true; }
      if (paintWindow(d)) dirty = true; }
    for (const t of ticking) if (t(now)) moving = true;
    if (moving) { renderer.shadowMap.needsUpdate = true; dirty = true; }
    if (dirty) { composer.render(); renders++; dirty = false; }
    if (++frames === 3) { self.ready = true; api.ready = true; }
    onFrame(); requestAnimationFrame(frame); }
  api.renders = () => renders;
  layout(); requestAnimationFrame(frame);
  return self;
}
