// The front of the machine in 3D: one wrinkle-finish enamel panel under everything below the top bar, the drum bank let
// into it as a bay, and the hardware of the lower deck. Built from machine-kit.js, so it is the same enamel, brass,
// steel, bakelite, glass and light as the back panel (settings.html).
//
// The HTML stays the model, the controls and the fallback: every part here is placed from the box of the HTML part it
// stands for, read on each layout, and the HTML draws whatever is read or typed (paper, plates, digits). The scene
// draws only when something changed: a drum or a knob turning, a jewel, a layout.
import { createMachine, engravingFace, engravingFont, engraveText } from './machine-kit.js';
import { mountVu } from './vu.js';

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
  // the key light's direction, for paper lit by the same light (its shadows fall away from it): on the screen, a unit
  // vector toward the light (--lx right, --ly down, so the light at the upper left is negative in both), and --lz how
  // far it stands out of the panel, as the cosine of its angle to the panel's face
  { const v = key.position.clone().normalize(), xy = Math.hypot(v.x, v.y);
    app.style.setProperty('--lx', (v.x / xy).toFixed(3)); app.style.setProperty('--ly', (-v.y / xy).toFixed(3)); app.style.setProperty('--lz', v.z.toFixed(3));
    // and the way its shadows fall, as a CSS gradient's angle (0deg up the screen, turning clockwise): away from the light
    app.style.setProperty('--la', (Math.atan2(-v.x / xy, -v.y / xy) * 180 / Math.PI).toFixed(1) + 'deg'); }

  /* ---------- the drums (as they were in bombechat.html) ---------- */
  // the drums' bay is let into the panel this deep: the plates, numbers, axles and pointers stand on its floor
  const BAY = .1;
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
  // (the column numbers stay legible in the shade the bay's top lip throws: they take no shadow)
  // the column numbers stand a little above their HTML, closer to the bay's top edge, but clear of its lip
  const NUP = .03;
  const nums = [...Array(12)].map((_, i) => { const m = K.engrave(String(i + 1), 0, 0, .13, '#b8b09a'); m.receiveShadow = false; return m; });

  /* ---------- the panel ---------- */
  const floor = new THREE.Mesh(new THREE.PlaneGeometry(1, 1), M.panel); floor.receiveShadow = true; scene.add(floor);
  // the floor's grain as the panel's, tile for tile
  const GRAIN = [9.8, 9];
  function floorTo(c, w, h) { floor.geometry.dispose(); const g = new THREE.PlaneGeometry(w, h), uv = g.attributes.uv, p = g.attributes.position;
    for (let i = 0; i < uv.count; i++) uv.setXY(i, (p.getX(i) + c.x) / GRAIN[0], (p.getY(i) + c.y) / GRAIN[1]); floor.geometry = g; floor.position.set(c.x, c.y, -BAY); }
  let front = null;// the panel, its frame and bolt ring: rebuilt when the bay or the window changes shape

  /* ---------- the bank's foot: odometer bezels, wheels and lamp sockets ---------- */
  const bezelMat = new THREE.MeshStandardMaterial({ color: '#2a2a27', roughness: .38, metalness: .9 });
  const windowMat = new THREE.MeshStandardMaterial({ color: '#030303', roughness: .9 });
  bezelMat.userData.keep = windowMat.userData.keep = true;
  // a rounded frame round a window w by h (units), r its corner, t its width, d its depth
  function frameGeo(w, h, t, d, r = .02) {
    const rr = (p, w, h, r, hole) => { const x = -w / 2, y = -h / 2; p.moveTo(x + r, y); p.lineTo(x + w - r, y); p.quadraticCurveTo(x + w, y, x + w, y + r); p.lineTo(x + w, y + h - r); p.quadraticCurveTo(x + w, y + h, x + w - r, y + h); p.lineTo(x + r, y + h); p.quadraticCurveTo(x, y + h, x, y + h - r); p.lineTo(x, y + r); p.quadraticCurveTo(x, y, x + r, y); return p; };
    const s = rr(new THREE.Shape(), w + 2 * t, h + 2 * t, r + t * .6); s.holes.push(rr(new THREE.Path(), w, h, r));
    const b = Math.min(t * .45, d * .45);
    const g = new THREE.ExtrudeGeometry(s, { depth: Math.max(.001, d - 2 * b), bevelEnabled: true, bevelThickness: b, bevelSize: b * .9, bevelSegments: 3, curveSegments: 6 });
    g.translate(0, 0, b); return g;
  }
  const footParts = new THREE.Group(); scene.add(footParts);
  // lamps, jewels and knobs keep their state: they live here and are only moved by a layout
  const kept = new THREE.Group(); scene.add(kept);
  const lamps = new Map();// HTML lamp (or jewel) -> {j, el, color()}

  // take a group's parts away and free what is theirs alone: geometry, materials and their pictures, never what is
  // shared (userData.keep: the kit's materials and the geometry every bolt or knob uses)
  function clear(g) { for (const o of [...g.children]) { g.remove(o); o.traverse(m => { if (!m.isMesh) return;
    if (m.geometry && !m.geometry.userData.keep) m.geometry.dispose();
    for (const mt of [].concat(m.material || [])) if (!mt.userData.keep) { if (mt.map) mt.map.dispose(); mt.dispose(); } }); } }

  /* ---------- where things go ---------- */
  let U = 1, W = 1, H = 1, D = 10;
  // the frame showing round the panel, the ring bolts' distance in from the window's edge and their radius, in px
  // (a narrow window, as front-panel.css lays it out, has the ring 12 px in and its frame 4 px)
  const NARROW = matchMedia('(max-width:1239px)'), BOLT = 4.2;
  let FRAME = 6, RING = 16;
  // the parts no ring bolt may meet
  const PARTS = '.bank, .sheet, .menu, .mods .mod, .vu, .row2 > .col, .outbox, .row3 .stn, .counter, .lamp:not([hidden])';
  const css = (el, k) => parseFloat(getComputedStyle(el)[k]) || 0;
  // laid out mid-turn, to the pixel: laid out again from the rects once the turn is over (frame)
  let laidTurned = false;
  const box = el => { const b = boxIn(el, host); if (b.turned) laidTurned = true; return b; };
  // the world point at host pixel (px, py) seen at depth z: things standing off the panel are set back toward the
  // middle by as much as the lens pushes them out, so each sits exactly over the HTML it stands for
  const at = (px, py, z = 0) => { const s = (D - z) / D; return new THREE.Vector3((px - W / 2) / U * s, -(py - H / 2) / U * s, z); };
  const shown = el => !!el && el.isConnected && el.offsetParent !== null && getComputedStyle(el).visibility !== 'hidden';
  // laid out, though the scene's own rules may hide its CSS drawing
  const placed = el => !!el && el.isConnected && el.offsetParent !== null;

  function layoutFoot() {
    clear(footParts);
    for (const odo of app.querySelectorAll('.bank .odo, .panel .counter .odo')) {
      if (!shown(odo)) continue;
      const b = box(odo), z0 = odo.closest('.bank') ? -BAY : 0, t = 3.2 / U, d = .05;
      const g = new THREE.Group(); footParts.add(g);
      const c = at(b.x + b.w / 2, b.y + b.h / 2, z0); g.position.copy(c);
      const fr = new THREE.Mesh(frameGeo(b.w / U, b.h / U, t, d, 1.5 / U), bezelMat); fr.castShadow = true; fr.receiveShadow = true; g.add(fr);
      const win = new THREE.Mesh(new THREE.PlaneGeometry(b.w / U + .01, b.h / U + .01), windowMat); win.position.z = .001; g.add(win);
      // (the wheels are the HTML's own, shaded as they always were, turning in this bezel's window)
    }
    // the frames-checked lamps: a jewel in a socket for each, lit as the HTML lamp is
    const seen = new Set();
    for (const i of app.querySelectorAll('.lampset i')) {
      if (!shown(i)) continue; seen.add(i);
      const b = box(i), r = (b.w / 2 + 1.5) / U;
      let L = lamps.get(i);
      if (!L) { const j = K.jewel('#f0b64c', { radius: r, light: false, parent: kept }); L = { j, el: i, kind: 'lamp' }; lamps.set(i, L); }
      L.j.size(r); L.j.group.position.copy(at(b.x + b.w / 2, b.y + b.h / 2, -BAY));
      syncLamp(L);
    }
    for (const [el, L] of lamps) if (L.kind === 'lamp' && !seen.has(el)) { kept.remove(L.j.group); lamps.delete(el); }
  }
  // a lamp's state, as its HTML has it: n now (amber), d done (warm white), otherwise dark
  function syncLamp(L) { const c = L.el.classList; if (L.kind !== 'lamp') return L.j.set(!c.contains('off')); return L.j.set(c.contains('n') || c.contains('d'), c.contains('d') ? '#f4ecd6' : '#f0b64c'); }

  /* ---------- the lower deck: switch tiles, eyepiece, meter case, outbox tray, card holders ---------- */
  const deckParts = new THREE.Group(); scene.add(deckParts);
  // a rounded rectangle w by h, r its corners, as a shape or a hole
  const rrect = (p, x, y, w, h, r) => { r = Math.min(r, w / 2, h / 2); p.moveTo(x + r, y); p.lineTo(x + w - r, y); p.quadraticCurveTo(x + w, y, x + w, y + r); p.lineTo(x + w, y + h - r); p.quadraticCurveTo(x + w, y + h, x + w - r, y + h); p.lineTo(x + r, y + h); p.quadraticCurveTo(x, y + h, x, y + h - r); p.lineTo(x, y + r); p.quadraticCurveTo(x, y, x + r, y); return p; };
  // a slab w by h, d deep, its face at z d, its edges rounded over by b; holes in it as [{x, y, w, h, r}] (centred)
  function slab(w, h, d, { r = .02, b = .012, holes = [] } = {}) {
    const s = rrect(new THREE.Shape(), -w / 2, -h / 2, w, h, r);
    for (const o of holes) s.holes.push(rrect(new THREE.Path(), o.x - o.w / 2, o.y - o.h / 2, o.w, o.h, o.r || 0));
    b = Math.min(b, d * .45);
    const g = new THREE.ExtrudeGeometry(s, { depth: Math.max(.0005, d - 2 * b), bevelEnabled: b > 0, bevelThickness: b, bevelSize: b * .9, bevelSegments: 3, curveSegments: 8 });
    g.translate(0, 0, b); return g;
  }
  const SVGU = 25.5;// the knob's radius in its SVG's units
  const knobParts = new Map();// knob button -> the kit's knob
  function knobFor(btn) {
    let k = knobParts.get(btn);
    if (!k) {
      const spec = knobs[btn.id] || {};
      k = K.knob({ labels: spec.labels || [], angles, place: place.map(P => ({ ...P, x: P.x / SVGU, y: P.y / SVGU })), line: line / SVGU, size: 13.6 / .86 / SVGU, parent: kept });// (engrave() cuts its capitals .86 of the size it is given: the SVG's 13.6)
      k.set(+btn.getAttribute('aria-valuenow') || 0); knobParts.set(btn, k); ticking.push(now => k.tick(now));
    }
    return k;
  }
  const JEWEL = { j1: '#f0b64c', j2: '#f4ecd6' };
  function jewelFor(el, radius) {
    let L = lamps.get(el);
    if (!L) { const j = K.jewel(JEWEL[el.id] || '#f0b64c', { light: true, parent: kept }); L = { j, el, kind: 'jewel' }; lamps.set(el, L); syncLamp(L); }
    L.j.size(radius);
    return L;
  }
  // the holders and the tray's rim: the kit's dull brass, as the back's jack nuts and maker's plate are (bright brass is
  // for small turned parts: bezels, rings)
  const trayMat = M.brassDull;
  const lensMat = new THREE.MeshPhysicalMaterial({ color: '#050607', roughness: .08, metalness: 0, clearcoat: 1, clearcoatRoughness: .12 });
  const ringMat = new THREE.MeshStandardMaterial({ color: '#0d0d0c', roughness: .45, metalness: .8 });
  const eyeMat = M.brass.clone();// its own, so a pointer over it can catch it
  for (const m of [lensMat, ringMat, eyeMat]) m.userData.keep = true;

  const eyeGeo = (() => { const p = [[.74, 0], [1, 0], [1, .1], [.97, .2], [.9, .25], [.82, .25], [.77, .2], [.76, .12], [.74, .1]].map(([r, y]) => new THREE.Vector2(r, y)); const g = new THREE.LatheGeometry(p, 64); g.rotateX(Math.PI / 2); g.userData.keep = true; return g; })();
  const bossGeo = (() => { const p = [[0, 0], [1, 0], [1, .35], [.9, .55], [.6, .66], [0, .7]].map(([r, y]) => new THREE.Vector2(r, y)); const g = new THREE.LatheGeometry(p, 32); g.rotateX(Math.PI / 2); g.userData.keep = true; return g; })();
  let shutter = null, eyeEl = null;
  // parts sitting at the given boxes; z the face of the deck under them
  let trayWell = null;// the outbox tray's well as last laid out, in host pixels
  function layoutDeck(holes) {
    clear(deckParts); trayWell = null;
    const bs = BOLT / U / .09;// the bolt ring's bolts, here too
    // each switch tile: a pressed steel escutcheon let into the enamel, a slotted bolt in each corner
    for (const mod of deck.querySelectorAll('.mods .mod')) {
      if (!shown(mod)) continue;
      const b = box(mod), c = at(b.x + b.w / 2, b.y + b.h / 2, -.012);
      const esc = new THREE.Mesh(slab(b.w / U, b.h / U, .03, { r: 2.5 / U, b: .01 }), M.cover); esc.position.set(c.x, c.y, -.042); esc.castShadow = true; esc.receiveShadow = true; deckParts.add(esc);
      holes.push({ x: c.x, y: c.y, w: b.w / U, h: b.h / U, r: 2.5 / U });
      const eye = mod.querySelector('.eyepiece'), eb = eye && box(eye);
      const k = 6.5;
      [[k, k], [b.w - k, k], [k, b.h - k], [b.w - k, b.h - k]].forEach(([x, y], i) => {
        if (eb && x > eb.x - b.x - 6 && y < eb.y - b.y + eb.h + 6) return;// under the eyepiece's ring
        const p = at(b.x + x, b.y + y, 0); K.bolt(p.x, p.y, { scale: bs * .85, z: .006, parent: deckParts, slot: (i * 1.7 + b.x * .01) % 3 });
      });
      // its switch
      const btn = mod.querySelector('.knob');
      if (btn && placed(btn)) { const kb = box(btn), s = kb.w / 138, kn = knobFor(btn);
        kn.group.position.copy(at(kb.x + 69 * s, kb.y + 51 * s, 0)); kn.group.scale.setScalar(SVGU * s / U); }
      // its jewel
      // (a tile with nothing fitted has an empty socket where its lamp would be)
      const je = mod.querySelector('.jewel'), bl = mod.querySelector('.blank');
      if (je && placed(je)) { const jb = box(je), p = at(jb.x + jb.w / 2, jb.y + jb.h / 2, 0), r = (jb.w / 2 + 2.6) / U;
        if (bl) { const sk = K.jewel('#000', { empty: true, radius: r, parent: deckParts }); sk.group.position.copy(p); }
        else { const L = jewelFor(je, r); L.j.group.position.copy(p); } }
      // a switch not fitted: the kit's blanking plate, the one the back carries for its Bluetooth group
      if (bl && placed(bl)) { const bb = box(bl), p = at(bb.x + bb.w / 2, bb.y + bb.h / 2, 0);
        const plate = K.blank(bb.w / U, bb.h / U, 'NOT FITTED', { size: 12.5 / .86 / U, bolts: bs * .85, parent: deckParts }); plate.group.position.copy(p); }
      // Light's eyepiece: a turned brass ring and a dark lens round the camera's picture, and its shutter
      if (eye && placed(eye)) {
        eyeEl = eye; const r = eb.w / 2 / U, p = at(eb.x + eb.w / 2, eb.y + eb.h / 2, .02);
        const ring = new THREE.Mesh(eyeGeo, eyeMat); ring.scale.set(r, r, r * .7); ring.position.set(p.x, p.y, 0); ring.castShadow = true; ring.receiveShadow = true; deckParts.add(ring);
        // the lens: a shallow dome of dark glass, the room in it, held by a dark retaining ring inside the brass
        const LR = 3.2, la = Math.asin(.74 / LR);
        const lens = new THREE.Mesh(new THREE.SphereGeometry(r * LR, 48, 8, 0, Math.PI * 2, 0, la).rotateX(Math.PI / 2), lensMat); lens.position.set(p.x, p.y, .012 - r * LR * Math.cos(la)); deckParts.add(lens);
        const hold = new THREE.Mesh(new THREE.TorusGeometry(r * .75, r * .035, 8, 48), ringMat); hold.position.set(p.x, p.y, .014); deckParts.add(hold);
        shutter = new THREE.Group(); shutter.position.set(p.x, p.y, .03); deckParts.add(shutter);
        const disc = new THREE.Mesh(new THREE.CylinderGeometry(r * .76, r * .76, .012, 48).rotateX(Math.PI / 2), M.steel); disc.castShadow = true; shutter.add(disc);
        const slot = new THREE.Mesh(new THREE.BoxGeometry(r * 1.1, r * .09, .01), M.hole); slot.rotation.z = Math.PI / 4; slot.position.z = .006; shutter.add(slot);
        shutter.visible = eye.classList.contains('shut');
      }
    }
    // the room meter's case: bakelite, a brass bezel round its dial and a boss for the needle's pivot
    const vu = deck.querySelector('.vu'), svg = vu && vu.querySelector('svg');
    if (vu && placed(vu)) {
      const b = box(vu), sv = box(svg), s = sv.w / 200, c = at(b.x + b.w / 2, b.y + b.h / 2, 0);
      const cs = new THREE.Mesh(slab(b.w / U, b.h / U, .07, { r: 3 / U, b: .022 }), M.bakelite); cs.position.set(c.x, c.y, 0); cs.castShadow = true; cs.receiveShadow = true; deckParts.add(cs);
      const win = { x: sv.x, y: sv.y, w: 200 * s, h: 92 * s }, wc = at(win.x + win.w / 2, win.y + win.h / 2, .07), t = 3 / U;
      const bz = new THREE.Mesh(frameGeo(win.w / U, win.h / U, t, .025, 1.5 / U), M.brass); bz.position.set(wc.x, wc.y, .07); bz.castShadow = true; bz.receiveShadow = true; deckParts.add(bz);
      const pv = at(sv.x + 100 * s, sv.y + 97.5 * s, .07), pr = 6 * s / U;
      const boss = new THREE.Mesh(bossGeo, M.bakelite); boss.scale.set(pr, pr, pr); boss.position.set(pv.x, pv.y, .07); boss.castShadow = true; deckParts.add(boss);
      const cap = new THREE.Mesh(new THREE.CircleGeometry(pr * .35, 24), M.brass); cap.position.set(pv.x, pv.y, .07 + pr * .705); deckParts.add(cap);
      if (self.vu) self.vu.layout();
    }
    // the outbox: a shallow letter tray under the slips' column, a brass rim round a steel floor
    const col = deck.querySelector('.row2 > .col.tray') || deck.querySelector('.outbox');
    if (col && shown(col)) {
      // round the column's content (the standard build's tray column reaches past it into its margins to scroll), and
      // only as deep as the slips in it: a tray the size of what it holds, on either build. When they fill the column's
      // room (they scroll in it), the tray is the room
      const b = box(col), cs = getComputedStyle(col), pl = parseFloat(cs.paddingLeft), pr = parseFloat(cs.paddingRight), pt = parseFloat(cs.paddingTop), pbm = parseFloat(cs.paddingBottom);
      let top = b.y + pt, foot = b.y + b.h - pbm;
      if (!/auto|scroll/.test(cs.overflowY) || col.scrollHeight <= col.clientHeight + 4) {
        const held = [...col.querySelectorAll('.slip:not(.gone)')].filter(placed).map(box);
        if (held.length) { top = Math.max(top, Math.min(...held.map(r => r.y))); foot = Math.min(foot, Math.max(...held.map(r => r.y + r.h))); }
      }
      const pad = 6, x = b.x + pl - pad, y = top - pad, w = b.w - pl - pr + 2 * pad, h = foot - top + 2 * pad, c = at(x + w / 2, y + h / 2, 0), rim = 4 / U;
      trayWell = { x, y, w, h };
      // its floor the switches' pressed steel, inside a brass rim
      const fl = new THREE.Mesh(slab(w / U, h / U, .012, { r: 2 / U, b: .004 }), M.cover); fl.position.set(c.x, c.y, 0); fl.receiveShadow = true; fl.castShadow = true; deckParts.add(fl);
      // nothing waiting: the words engraved in the floor where the slip would lie (the HTML keeps them, unseen, for
      // a screen reader)
      const none = col.querySelector('.outbox > .slip.empty:only-child');
      if (none && placed(none)) { const nb = box(none), e = K.engrave(none.textContent.trim().replace(/\.$/, ''), 0, 0, 13 / .86 / U, '#8f897a', { parent: deckParts });// (in the engraving face's small capitals, as every word cut in this machine's metal)
        const p = at(nb.x + nb.w / 2, nb.y + nb.h / 2, .012); e.position.set(p.x, p.y, .0125); }
      const lip = new THREE.Mesh(frameGeo(w / U - 2 * rim, h / U - 2 * rim, rim, .045, 1 / U), trayMat); lip.position.set(c.x, c.y, .004); lip.castShadow = true; lip.receiveShadow = true; deckParts.add(lip);
    }
    // the stations: each card slid into a brass holder, its jewel beside it
    for (const stn of deck.querySelectorAll('.row3 .stn')) {
      if (!shown(stn)) continue;
      const hd = stn.querySelector('.holder'), sp = hd && hd.querySelector(':scope > span');
      if (hd && sp) { const hb = box(hd), sb = box(sp), c = at(hb.x + hb.w / 2, hb.y + hb.h / 2, 0), ic = at(sb.x + sb.w / 2, sb.y + sb.h / 2, 0);
        const fr = new THREE.Mesh(slab(hb.w / U, hb.h / U, .028, { r: 2 / U, b: .009, holes: [{ x: ic.x - c.x, y: ic.y - c.y, w: sb.w / U, h: sb.h / U, r: .5 / U }] }), trayMat);
        fr.position.set(c.x, c.y, 0); fr.castShadow = true; fr.receiveShadow = true; deckParts.add(fr);
        const back = new THREE.Mesh(new THREE.PlaneGeometry(sb.w / U, sb.h / U), M.hole); back.position.set(ic.x, ic.y, .002); deckParts.add(back); }
      const je = stn.querySelector('.jewel');
      if (je && placed(je)) { const jb = box(je), L = jewelFor(je, (jb.w / 2 + 2.6) / U); L.j.group.position.copy(at(jb.x + jb.w / 2, jb.y + jb.h / 2, 0)); }
    }
    // jewels whose HTML has gone
    for (const [el, L] of lamps) if (L.kind === 'jewel' && !el.isConnected) { kept.remove(L.j.group); lamps.delete(el); }
    for (const [el, k] of knobParts) k.group.visible = placed(el);
  }

  /* ---------- hardware on paper and on the bars: baked in this scene's light ---------- */
  // Small parts the scene can't reach (they sit on the HTML: plate screws, the jewels of the top bar and the keys, the
  // keys' bakelite) are drawn once from the kit in the same lights and room and handed to the CSS as pictures
  async function bakeSprites() {
    const root = document.documentElement, set = (k, v) => root.style.setProperty(k, `url("${v}")`);
    const load = url => new Promise((res, rej) => { const i = new Image(); i.onload = () => res(i); i.onerror = rej; i.src = url; });
    // a plate's slotted screw: a cheese head, its slot turned as it was left
    const headGeo = (() => { const p = [[0, 0], [1, 0], [1, .3], [.9, .58], [.6, .74], [0, .78]].map(([r, y]) => new THREE.Vector2(r, y)); const g = new THREE.LatheGeometry(p, 40); g.rotateX(Math.PI / 2); return g; })();
    for (const [name, turn] of [['--screw-a', .62], ['--screw-b', -.48]]) {
      const g = new THREE.Group(); const h = new THREE.Mesh(headGeo, M.steel); g.add(h);
      const s = new THREE.Mesh(new THREE.BoxGeometry(1.5, .2, .3), M.hole); s.position.z = .7; s.rotation.z = turn; g.add(s);
      // (seen a little from above and the left, as the key light falls, so its steel takes the light the ring bolts do)
      set(name, K.bake(g, { px: 40, view: 'tilt', shadow: true, margin: .3 }));
    }
    // jewels, lit and dark
    const jw = K.jewel('#f0b64c', { parent: new THREE.Group() });
    set('--jewel-off', K.bake(jw.group, { px: 48, shadow: true, margin: .2 }));
    jw.set(true); set('--jewel-on', K.bake(jw.group, { px: 48, shadow: true, margin: .2 }));
    // a key's bakelite: a rounded bar, cut into its two ends and its middle so it stretches to any key
    const pill = (w, h, r, depth, bevel) => { const s = new THREE.Shape(); const x = -w / 2, y = -h / 2;
      // (each arc runs on from the last point by itself; no point twice, or the face triangulates with a hole)
      s.moveTo(x + r, y); s.absarc(x + w - r, y + r, r, -Math.PI / 2, 0); s.absarc(x + w - r, y + h - r, r, 0, Math.PI / 2); s.absarc(x + r, y + h - r, r, Math.PI / 2, Math.PI); s.absarc(x + r, y + r, r, Math.PI, Math.PI * 1.5, false);
      return new THREE.Mesh(new THREE.ExtrudeGeometry(s, { depth, bevelEnabled: true, bevelThickness: bevel, bevelSize: bevel, bevelSegments: 6, curveSegments: 24 }), keyMat); };
    // (the knobs' bakelite, its broad face taking less of the room than a knob's curves do)
    const keyMat = M.bakelite.clone(); keyMat.envMapIntensity = .45; keyMat.specularIntensity = .5;
    async function sliced(mesh, px, cap, prefix) {
      const img = await load(K.bake(mesh, { px, margin: 0 }));
      const cut = (x, w) => { const c = document.createElement('canvas'); c.width = w; c.height = img.height; c.getContext('2d').drawImage(img, x, 0, w, img.height, 0, 0, w, img.height); return c.toDataURL('image/png'); };
      const capPx = Math.round(img.width * cap);
      set(prefix + '-l', cut(0, capPx)); set(prefix + '-m', cut(capPx, img.width - 2 * capPx)); set(prefix + '-r', cut(img.width - capPx, capPx));
    }
    // the keys: 25 px tall, round-ended; the toggle's plate: square-cornered
    await sliced(pill(5, 1.6, .78, .12, .12), 200, (.8 + .12) / (5 + .24), '--key');
    await sliced(pill(3.4, 2, .16, .1, .1), 160, .12, '--toggle');
    // hardware on paper, in the same light: the sheet's two clips, a brass tab screwed to the panel over the paper's
    // top edge, and the key check card's drawing pin. Each takes its own shadow on a catcher, so on the paper it lands
    // where the key light throws it
    { const g = new THREE.Group(), w = .62, h = 1.15, r = .28;
      const sh = new THREE.Shape(); sh.moveTo(-w / 2 + r, -h / 2); sh.lineTo(w / 2 - r, -h / 2); sh.quadraticCurveTo(w / 2, -h / 2, w / 2, -h / 2 + r); sh.lineTo(w / 2, h / 2 - r); sh.quadraticCurveTo(w / 2, h / 2, w / 2 - r, h / 2);
      sh.lineTo(-w / 2 + r, h / 2); sh.quadraticCurveTo(-w / 2, h / 2, -w / 2, h / 2 - r); sh.lineTo(-w / 2, -h / 2 + r); sh.quadraticCurveTo(-w / 2, -h / 2, -w / 2 + r, -h / 2);
      const tab = new THREE.Mesh(new THREE.ExtrudeGeometry(sh, { depth: .04, bevelEnabled: true, bevelThickness: .045, bevelSize: .05, bevelSegments: 4, curveSegments: 10 }), M.brassDull); g.add(tab);
      const scr = new THREE.Group(); scr.add(new THREE.Mesh(headGeo, M.steel)); const sl = new THREE.Mesh(new THREE.BoxGeometry(1.5, .2, .3), M.hole); sl.position.z = .7; scr.add(sl);
      scr.scale.set(.2, .2, .2); scr.position.set(0, .26, .13);
      for (const [name, turn] of [['--clip-a', .5], ['--clip-b', -.9]]) { sl.rotation.z = turn; g.add(scr); set(name, K.bake(g, { px: 64, shadow: true, margin: .2 })); } }
    { const p = [[0, 0], [1, 0], [1, .07], [.96, .15], [.78, .28], [.45, .37], [0, .4]].map(([r, y]) => new THREE.Vector2(r, y));
      const head = new THREE.Mesh(new THREE.LatheGeometry(p, 48).rotateX(Math.PI / 2), M.brass);
      set('--pin', K.bake(head, { px: 56, shadow: true, margin: .32 })); }
    // the top bar's Machine sound switch: the deck's bakelite knob, small, thrown to on or off
    { const k = K.knob({ labels: [], angles: [-32, 32], parent: new THREE.Group() });
      k.set(0); set('--bat-off', K.bake(k.group, { px: 104, shadow: true, margin: .14 }));
      k.set(1); set('--bat-on', K.bake(k.group, { px: 104, shadow: true, margin: .14 })); }
    // brass frames to stretch round a window of any size (border-image): the flashing view's bezel in bright brass, its
    // card holder in the dull brass of the stations' holders
    for (const [name, mat, t, d] of [['--bezel', M.brass, .16, .16], ['--holder', M.brassDull, .1, .1]]) {
      const fr = new THREE.Mesh(frameGeo(1, 1, t, d, .015), mat), b = new THREE.Box3().setFromObject(fr), W = b.max.x - b.min.x;
      const px = 240, win = 1 - 2 * (Math.min(t * .45, d * .45) * .9);
      set(name, K.bake(fr, { px, margin: 0 })); root.style.setProperty(name + '-cut', String(Math.round(px * (W - win) / 2 / W)));
    }
    K.bakeDone();
    root.classList.add('baked');
  }

  function layout() {
    laidTurned = false; laidSig = signature(); W = css(host, 'width'); H = css(host, 'height'); if (!W || !H) return;
    const dr = box(cells[0].el).w; U = dr / .92;
    renderer.setSize(W, H, false); camera.aspect = W / H;
    const bk = box(bank);
    // the lens: the whole panel in a 22 degree view, so its top and foot are seen no more steeply than the bank's
    // rows always were, and the bay's lip hides nothing engraved on its floor
    D = (H / U / 2) / Math.tan(11 * Math.PI / 180);
    camera.fov = 2 * Math.atan((H / U / 2) / D) * 180 / Math.PI; camera.position.set(0, 0, D); camera.lookAt(0, 0, 0); camera.updateProjectionMatrix(); composer.setSize(W, H);
    // the panel: one sheet of enamel, the bay let into it, bolted all round at the window's edge
    if (front) { scene.remove(front.group); front.panel.geometry.dispose(); front.frame.geometry.dispose(); }
    const fg = new THREE.Group(); scene.add(fg);
    const bc = at(bk.x + bk.w / 2, bk.y + bk.h / 2, 0);
    // as the back has it: the panel inside a dark frame (FRAME px of it showing at the window's edge), its bolt ring RING
    // px in from the window's edge. A ring bolt that would meet any part of the front is left out
    const bs = BOLT / U / .09;
    const holes = [{ x: bc.x, y: bc.y, w: bk.w / U, h: bk.h / U, r: 3 / U }];
    layoutDeck(holes);
    FRAME = NARROW.matches ? 4 : 6; RING = NARROW.matches ? 12 : 16;
    const parts = [...app.querySelectorAll(PARTS)].filter(placed).map(box);
    const clearOf = (x, y) => { const px = x * U + W / 2, py = H / 2 - y * U, r = BOLT + 1.5;
      return !parts.some(b => px + r > b.x && px - r < b.x + b.w && py + r > b.y && py - r < b.y + b.h); };
    const pw = (W - 2 * FRAME) / U, ph = (H - 2 * FRAME) / U;
    const per = K.perimeter(pw, ph, 0, { across: 13, down: Math.max(3, Math.round(ph / pw * 13 * .78)), inset: (RING - FRAME) / U, scale: bs, z: .006, holes, grain: GRAIN, clear: clearOf, parent: fg });
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
    const cn = [...app.querySelectorAll('.colno')]; nums.forEach((n, i) => { const r = box(cn[i]); const p = at(r.x + r.w / 2, r.y + r.h / 2, -BAY + .005); n.position.set(p.x, p.y + NUP, -BAY + .005); n.scale.setScalar(Math.max(1, (13 / U) / .13)); });
    layoutFoot();
    for (const f of later) f({ box, at, U, W, H, D });
    // the key's shadow covers the whole panel
    Object.assign(key.shadow.camera, { left: -W / U / 2 - 1, right: W / U / 2 + 1, top: H / U / 2 + 1, bottom: -H / U / 2 - 1, near: 1, far: 60 }); key.shadow.camera.updateProjectionMatrix();
    renderer.shadowMap.needsUpdate = true; dirty = true;
  }
  // parts 6.3 adds lay themselves out with the rest
  const later = [];

  let dirty = true, renders = 0, laidSig = '';
  // what the scene was laid out from: every box it reads. A change in the HTML lays it out again only if one moved
  const WATCHED = '.bank, .colno, .rowlab, .bankfoot .rd, .bankfoot .odo, .lampset i, .mods .mod, .mod .knob, .mod .jewel, .mod .blank, .eyepiece, .vu, .vu svg, .row2 > .col.tray, .outbox, .row2 .slip, .outbox > .slip.empty, .row3 .stn, .row3 .holder, .row3 .holder > span, .stn .jewel, .counter .odo, .sheet, .menu';
  function signature() { const hr = host.getBoundingClientRect(); let s = `${hr.width}x${hr.height}`;
    for (const el of app.querySelectorAll(WATCHED)) { const r = el.getBoundingClientRect(); s += `|${Math.round(r.left - hr.left)},${Math.round(r.top - hr.top)},${Math.round(r.width)},${Math.round(r.height)},${el.offsetParent ? 1 : 0}`; }
    for (const el of app.querySelectorAll('.bankfoot .rd')) s += getComputedStyle(el).visibility[0];
    return s; }
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
  // (a jewel lit, a knob's stop, the eyepiece's shutter are drawn as they are; anything else lays out again)
  const mo = new MutationObserver(list => { for (const m of list) {
    if (m.type === 'attributes') {
      const t = m.target;
      if (m.attributeName === 'aria-valuenow') { const k = knobParts.get(t); if (k && k.set(+t.getAttribute('aria-valuenow') || 0, !rm.matches && !still)) dirty = true; continue; }
      if (m.attributeName === 'class') {
        const L = lamps.get(t); if (L) { if (syncLamp(L)) dirty = true; continue; }
        if (t === eyeEl) { if (shutter) shutter.visible = t.classList.contains('shut'); dirty = true; continue; }
        if (t.matches('.knob, .holder, .holder *, .slip, .slip *, .rs, .stamps *, .odo i, .ent, .pl')) continue;
      }
    }
    relay = true; } });
  mo.observe(bank.querySelector('.bankfoot'), { subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'style', 'hidden'] });
  // a pointer over a knob or the eyepiece: its bakelite or brass catches a little more of the room (no glow)
  const eyeBase = eyeMat.color.clone(), eyeOver = eyeBase.clone().lerp(new THREE.Color('#f3dfb4'), .3);
  function hover(el, on) {
    const k = knobParts.get(el); if (k) { if (k.hover(on)) dirty = true; return; }
    if (el === eyeEl) { eyeMat.color.copy(on ? eyeOver : eyeBase); eyeMat.roughness = on ? .26 : M.brass.roughness; dirty = true; }
  }
  deck.addEventListener('pointerover', e => { const t = e.target.closest('.knob, .eyepiece'); if (t && !t.contains(e.relatedTarget)) hover(t, true); });
  deck.addEventListener('pointerout', e => { const t = e.target.closest('.knob, .eyepiece'); if (t && !t.contains(e.relatedTarget)) hover(t, false); });
  mo.observe(deck.querySelector('.panel') || deck, { subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'style', 'hidden', 'aria-valuenow'] });

  // for checks: where the scene draws each part and where its HTML stands, in bank pixels, at rest. Each drum's centre
  // (and its axle's height), each row plate's left end (its label's left plus 4 px) and each column number's middle
  // (drawn NUP up, by design)
  api.placement = () => { camera.updateMatrixWorld(); const br = bank.getBoundingClientRect(), hr = host.getBoundingClientRect();
    const px = v => { const q = v.clone().project(camera); return { x: (q.x + 1) / 2 * W + hr.left - br.left, y: (1 - q.y) / 2 * H + hr.top - br.top }; };
    const rel = el => { const r = el.getBoundingClientRect(); return { l: r.left - br.left, t: r.top - br.top, w: r.width, h: r.height }; };
    const lab = [...app.querySelectorAll('.rowlab')], cn = [...app.querySelectorAll('.colno')];
    return { drums: drums.map(d => { const r = rel(d.cell.el); const c = px(d.g.position);
        return { at: `drum ${d.r + 1}.${d.c + 1}`, drawn: c, html: { x: r.l + r.w / 2, y: r.t + r.w * .08 + r.w / 2 }, axle: px(new THREE.Vector3(c.x, rails[d.r].position.y, rails[d.r].position.z)).y, centre: c.y }; }),
      plates: plates.map((p, r) => { const w = p.geometry.parameters.width * p.scale.x; return { at: `plate ${r + 1}`, drawn: { x: px(new THREE.Vector3(p.position.x - w / 2, p.position.y, p.position.z)).x }, html: { x: rel(lab[r]).l + 4 } }; }),
      numbers: nums.map((n, i) => { const r = rel(cn[i]); return { at: `number ${i + 1}`, drawn: px(new THREE.Vector3(n.position.x, n.position.y - NUP, n.position.z)), html: { x: r.l + r.w / 2, y: r.t + r.h / 2 } }; }) }; };
  document.fonts && document.fonts.ready.then(() => { drums.forEach(d => d.last = ''); layout(); });

  // the same curve as the CSS turn: cubic-bezier(.2,.85,.25,1.04) over .95 s
  function bez(x) { const cx = 3 * .2, bx = 3 * (.25 - .2) - cx, ax = 1 - cx - bx, cy = 3 * .85, by = 3 * (1.04 - .85) - cy, ay = 1 - cy - by; let t = x;
    for (let i = 0; i < 6; i++) { const fx = ((ax * t + bx) * t + cx) * t - x, d = (3 * ax * t + 2 * bx) * t + cx; if (Math.abs(fx) < 1e-5) break; t -= fx / d; } return ((ay * t + by) * t + cy) * t; }
  const DUR = 950; let frames = 0;
  const ticking = [];// parts that animate on their own (knobs): tick(now) true while moving
  const self = { ready: false, layout, dirty() { dirty = true; }, renders: () => renders,
    // a part drawn alone in this scene's light, as a picture for the CSS (the kit's bake: clips, pins, screws on paper)
    bake: (o, opts) => K.bake(o, opts),
    // a switch turned: its knob turns to the stop now, before the HTML redraws
    knob(id, pos) { const k = knobParts.get(document.getElementById(id)); if (k && k.set(pos, !rm.matches && !still)) dirty = true; },
    jewel(id, on) { const L = lamps.get(document.getElementById(id)); if (L && L.j.set(on)) dirty = true; }, K, scene, at, box: el => box(el), later, ticking, lamps, syncLamp, soon, host, get U() { return U; }, get D() { return D; } };
  function frame(now) {
    let moving = false;
    if (laidTurned && !turnedAbove(host)) { relay = false; layout(); }
    else if (relay) { relay = false; if (signature() !== laidSig) layout(); }
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
  // for checks: every ring bolt as drawn, a disc in page pixels, and the parts it must clear
  api.ringBolts = () => { const hr = host.getBoundingClientRect();
    return Object.values(front.bolts).flat().map(m => ({ x: m.position.x * U + W / 2 + hr.left + scrollX, y: H / 2 - m.position.y * U + hr.top + scrollY, r: .09 * m.scale.x * U })); };
  api.ringParts = PARTS;
  // for checks: the outbox tray's well as drawn, in page pixels (its brass rim's outer edge)
  api.trayWell = () => { if (!trayWell) return null; const hr = host.getBoundingClientRect();
    return { x: trayWell.x + hr.left + scrollX, y: trayWell.y + hr.top + scrollY, w: trayWell.w, h: trayWell.h }; };
  // the room meter behind its glass, on its own canvas
  const vuEl = deck.querySelector('.vu');
  if (vuEl) try { vuEl.classList.add('gl'); self.vu = await mountVu(vuEl, { still }); } catch (e) { vuEl.classList.remove('gl'); console.warn('the meter stays flat:', e.message); }
  layout(); requestAnimationFrame(frame);
  bakeSprites().catch(e => console.warn('the plates keep their CSS screws:', e.message));
  return self;
}
