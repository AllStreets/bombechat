// The one machine. Both pages build from this so the front and the back share
// the same enamel, brass, bakelite, cloth cable, light and lens.
import * as THREE from 'three';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { GTAOPass } from 'three/addons/postprocessing/GTAOPass.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';

export { THREE };

// The engraving face. Marcellus SC is drawn slightly heavier than it is cut
// (a hairline stroke in the same ink), which brings it to the weight the
// machine's plates had in Copperplate.
export const ENGRAVING = "'Bombechat Engraving', serif";
export const engravingFont = (px, weight = '') => `${weight ? weight + ' ' : ''}${px}px ${ENGRAVING}`;
// Marcellus's capitals stand taller and narrower than Copperplate's did: cut them
// a little smaller and space them a little wider to keep each plate's proportions
const CAP = .86, TRACK = '.1em';
function setEngraving(k, px) { k.font = engravingFont(px * CAP); k.letterSpacing = `${(px * CAP * parseFloat(TRACK)).toFixed(2)}px`; }
// canvases are drawn once, so wait for the face before drawing any of them
export function engravingFace() {
  if (!document.fonts) return Promise.resolve();
  return document.fonts.load(engravingFont(64), 'A1').then(() => document.fonts.ready).catch(() => {});
}
// fill text, then go round it once with the same ink to give it body
export function engraveText(k, text, x, y, px, weight = .03) {
  k.fillText(text, x, y);
  if (weight > 0) { k.save(); k.strokeStyle = k.fillStyle; k.lineWidth = px * weight; k.lineJoin = 'round'; k.strokeText(text, x, y); k.restore(); }
}

export function canvasTex(w, h, draw, { repeat = [1, 1], color = false } = {}) {
  const c = document.createElement('canvas'); c.width = w; c.height = h; draw(c.getContext('2d'), w, h);
  const t = new THREE.CanvasTexture(c); t.wrapS = t.wrapT = THREE.RepeatWrapping; t.repeat.set(...repeat); t.anisotropy = 8;
  if (color) t.colorSpace = THREE.SRGBColorSpace;
  return t;
}
function noise(ctx, w, h, base, amp, blob) {
  const id = ctx.createImageData(w, h);
  for (let i = 0; i < w * h; i++) { const v = base + (Math.random() - .5) * amp; id.data[i * 4] = id.data[i * 4 + 1] = id.data[i * 4 + 2] = v; id.data[i * 4 + 3] = 255; }
  ctx.putImageData(id, 0, 0);
  if (blob) { ctx.globalAlpha = .06; for (let i = 0; i < blob; i++) { ctx.fillStyle = Math.random() < .5 ? '#000' : '#fff'; ctx.beginPath(); ctx.arc(Math.random() * w, Math.random() * h, 2 + Math.random() * 7, 0, 7); ctx.fill(); } ctx.globalAlpha = 1; }
}

export const CABLE_COLORS = ['#141414', '#1b1612', '#2a1a14', '#3b1d18', '#1c2620', '#24262a', '#3a2f22', '#121010'];

export function createMachine(container, { still = false, background = '#0d0d0c' } = {}) {
  const renderer = new THREE.WebGLRenderer({ antialias: true, powerPreference: 'high-performance', preserveDrawingBuffer: still });
  if (!renderer.getContext()) throw new Error('no webgl');
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  renderer.toneMapping = THREE.ACESFilmicToneMapping; renderer.toneMappingExposure = 1.05;
  // shadows only redraw when something that casts one has moved: set renderer.shadowMap.needsUpdate = true
  renderer.shadowMap.enabled = true; renderer.shadowMap.type = THREE.VSMShadowMap; renderer.shadowMap.autoUpdate = false; renderer.shadowMap.needsUpdate = true;
  container.appendChild(renderer.domElement);

  const scene = new THREE.Scene(); scene.background = new THREE.Color(background);
  const pmrem = new THREE.PMREMGenerator(renderer); scene.environment = pmrem.fromScene(new RoomEnvironment(), .04).texture; scene.environmentIntensity = .42;
  const camera = new THREE.PerspectiveCamera(30, 1, .1, 200);

  const tex = {
    wrinkle: canvasTex(512, 512, (c, w, h) => noise(c, w, h, 128, 90, 1600), { repeat: [6, 4] }),
    panelRough: canvasTex(256, 256, (c, w, h) => noise(c, w, h, 190, 60, 300), { repeat: [3, 2] }),
    braid: canvasTex(64, 256, (c, w, h) => {
      c.fillStyle = '#808080'; c.fillRect(0, 0, w, h); c.lineWidth = 3;
      for (let y = -w; y < h + w; y += 8) { c.strokeStyle = '#d0d0d0'; c.beginPath(); c.moveTo(0, y); c.lineTo(w, y + w * .5); c.stroke(); c.strokeStyle = '#303030'; c.beginPath(); c.moveTo(0, y + 4); c.lineTo(w, y + 4 - w * .5); c.stroke(); }
    }, { repeat: [60, 1] }),
    knurl: canvasTex(128, 16, (c, w, h) => { for (let x = 0; x < w; x += 4) { c.fillStyle = x % 8 ? '#e0e0e0' : '#202020'; c.fillRect(x, 0, 2, h); } }),
  };

  const M = {
    panel: new THREE.MeshStandardMaterial({ color: '#1e1f1c', roughness: .78, metalness: .35, roughnessMap: tex.panelRough, bumpMap: tex.wrinkle, bumpScale: .9 }),
    brass: new THREE.MeshStandardMaterial({ color: '#c29a5c', roughness: .32, metalness: 1 }),
    brassDull: new THREE.MeshStandardMaterial({ color: '#9c7b45', roughness: .5, metalness: 1 }),
    steel: new THREE.MeshStandardMaterial({ color: '#b9b6ae', roughness: .35, metalness: 1 }),
    bakelite: new THREE.MeshPhysicalMaterial({ color: '#17120e', roughness: .32, metalness: 0, clearcoat: .6, clearcoatRoughness: .35 }),
    hole: new THREE.MeshStandardMaterial({ color: '#050505', roughness: .9 }),
    frame: new THREE.MeshStandardMaterial({ color: '#121311', roughness: .6, metalness: .5 }),
    // the walls of a bay let into the panel: the same enamel, sprayed thinner where the gun couldn't reach
    lip: new THREE.MeshStandardMaterial({ color: '#2e2f2b', roughness: .55, metalness: .45 }),
  };
  const cableMat = c => new THREE.MeshPhysicalMaterial({ color: c, roughness: .72, metalness: 0, sheen: .6, sheenRoughness: .6, sheenColor: new THREE.Color('#8a8070'), bumpMap: tex.braid, bumpScale: 1.4 });

  scene.add(new THREE.HemisphereLight('#d9d4c8', '#0b0a09', .25));
  const key = new THREE.DirectionalLight('#fff1dc', 2.6); key.position.set(-5, 7, 18); key.castShadow = true;
  key.shadow.mapSize.set(2048, 2048); Object.assign(key.shadow.camera, { left: -12, right: 12, top: 12, bottom: -11, near: 1, far: 40 });
  key.shadow.bias = -.0006; key.shadow.radius = 18; key.shadow.blurSamples = 24;
  scene.add(key);
  const rim = new THREE.DirectionalLight('#c9d6e8', .55); rim.position.set(10, -2, 6); scene.add(rim);

  function makeComposer({ ao = true } = {}) {
    const composer = new EffectComposer(renderer, new THREE.WebGLRenderTarget(1, 1, { samples: 4, type: THREE.HalfFloatType }));
    composer.addPass(new RenderPass(scene, camera));
    if (ao) { const p = new GTAOPass(scene, camera, 1, 1); p.output = GTAOPass.OUTPUT.Default; p.blendIntensity = .85; p.updateGtaoMaterial({ radius: .45, distanceExponent: 1.4, thickness: 1.2, scale: 1.1, samples: 16 }); composer.addPass(p); }
    composer.addPass(new OutputPass());
    return composer;
  }

  const boltGeo = new THREE.CylinderGeometry(.09, .09, .06, 20); boltGeo.rotateX(Math.PI / 2);
  const slotGeo = new THREE.BoxGeometry(.13, .022, .02);
  // shared by every bolt: a page that clears and redraws its parts keeps these (userData.keep), as it keeps M's materials
  boltGeo.userData.keep = slotGeo.userData.keep = true;
  for (const m of Object.values(M)) m.userData.keep = true;
  function bolt(x, y, { scale = 1, z = .03, parent = scene, slot = Math.random() * 3 } = {}) {
    const b = new THREE.Mesh(boltGeo, M.steel); b.scale.setScalar(scale); b.position.set(x, y, z); b.castShadow = true; parent.add(b);
    const s = new THREE.Mesh(slotGeo, M.hole); s.scale.setScalar(scale); s.position.set(x, y, z + .032 * scale); s.rotation.z = slot; parent.add(s);
    b.userData.slot = s;
    return b;
  }
  // a traffolyte plate: black (or red) with the text engraved through to white
  function plate(text, x, y, { w = null, h = .26, red = false, size = 56, align = 'center', z = .013, pad = 0, parent = scene } = {}) { // pad: room each side for screws, in units of the type size
    const c = document.createElement('canvas'); const ctx = c.getContext('2d'); setEngraving(ctx, size); const tw = ctx.measureText(text).width;
    c.width = Math.ceil(tw + size * (1.2 + 2 * pad)); c.height = Math.ceil(size * 1.5); const k = c.getContext('2d');
    const g = k.createLinearGradient(0, 0, 0, c.height); g.addColorStop(0, red ? '#8a2a22' : '#191918'); g.addColorStop(1, red ? '#6d1e18' : '#0b0b0a'); k.fillStyle = g; k.fillRect(0, 0, c.width, c.height);
    setEngraving(k, size); k.fillStyle = red ? '#f2e7da' : '#e9e2cf'; k.textBaseline = 'middle'; k.textAlign = 'center'; engraveText(k, text, c.width / 2, c.height * .54, size, .045);
    const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 8;
    const pw = w || h * c.width / c.height;
    const m = new THREE.Mesh(new THREE.BoxGeometry(pw, h, .025), [M.bakelite, M.bakelite, M.bakelite, M.bakelite, new THREE.MeshPhysicalMaterial({ map: t, roughness: .5, specularIntensity: .35 }), M.bakelite]);
    m.position.set(align === 'left' ? x + pw / 2 : x, y, z); m.castShadow = true; m.receiveShadow = true; parent.add(m);
    return m;
  }
  // letters painted into the enamel
  function engrave(text, x, y, size = .14, color = '#cfc7b2', { parent = scene } = {}) {
    const s = 64; const c = document.createElement('canvas'); const ctx = c.getContext('2d'); setEngraving(ctx, s); const tw = ctx.measureText(text).width;
    c.width = Math.ceil(tw + 16); c.height = s + 16; const k = c.getContext('2d'); setEngraving(k, s); k.textAlign = 'center'; k.textBaseline = 'middle';
    k.fillStyle = 'rgba(0,0,0,.8)'; engraveText(k, text, c.width / 2, c.height / 2 + 3, s); k.fillStyle = color; engraveText(k, text, c.width / 2, c.height / 2, s);
    const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 8;
    const h = size * (c.height / s), w = h * c.width / c.height;
    const m = new THREE.Mesh(new THREE.PlaneGeometry(w, h), new THREE.MeshStandardMaterial({ map: t, transparent: true, roughness: .7, depthWrite: false }));
    m.position.set(x, y, .004); m.receiveShadow = true; parent.add(m);
    return m;
  }
  // which build this is, as edition.css names it for every plate ('airgapped build')
  const build = () => getComputedStyle(document.documentElement).getPropertyValue('--build').trim().replace(/^['"]|['"]$/g, '').toUpperCase();
  // and the radio it has, as edition.css says it ('no radio fitted')
  const radio = () => getComputedStyle(document.documentElement).getPropertyValue('--radio').trim().replace(/^['"]|['"]$/g, '').toUpperCase();
  // the maker's plate, etched brass. Machine number one.
  function makersPlate(x, y, w = 3.6) {
    const c = document.createElement('canvas'); c.width = 1400; c.height = 260; const k = c.getContext('2d');
    const g = k.createLinearGradient(0, 0, c.width, c.height); g.addColorStop(0, '#d9bd84'); g.addColorStop(.45, '#b8955c'); g.addColorStop(1, '#8f713f'); k.fillStyle = g; k.fillRect(0, 0, c.width, c.height);
    for (let i = 0; i < 2200; i++) { k.fillStyle = `rgba(${Math.random() < .5 ? '255,240,210' : '60,40,10'},.05)`; k.fillRect(Math.random() * c.width, Math.random() * c.height, Math.random() * 60, 1); }
    k.strokeStyle = '#3b2c14'; k.lineWidth = 5; k.strokeRect(28, 28, c.width - 56, c.height - 56);
    k.fillStyle = '#2a1f0e'; k.textAlign = 'center'; setEngraving(k, 92); engraveText(k, 'BOMBECHAT', c.width / 2, 128, 92, .04);
    setEngraving(k, 40); engraveText(k, `${build()}      MACHINE NO. 1      ${radio()}`, c.width / 2, 198, 40);
    const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 8;
    const h = w * c.height / c.width;
    const mp = new THREE.Mesh(new THREE.BoxGeometry(w, h, .03), [M.brassDull, M.brassDull, M.brassDull, M.brassDull, new THREE.MeshStandardMaterial({ map: t, metalness: .85, roughness: .38 }), M.brassDull]);
    mp.position.set(x, y, .015); mp.castShadow = true; mp.receiveShadow = true; scene.add(mp);
    [[-1, 1], [1, 1], [-1, -1], [1, -1]].forEach(([sx, sy]) => bolt(x + sx * (w / 2 - .13), y + sy * (h / 2 - .11), { scale: .6, z: .05 }));
    return mp;
  }


  /* ---------- the parts the front and the back share ---------- */

  // the panel's frame and bolt ring: the enamel panel, the dark frame standing out round it, and a bolt every so often
  // along each edge (across along the top and the foot, about every two units down the sides unless down says)
  // holes: bays let into the panel ([{x, y, w, h, r}], centred, in panel units), each with a bevelled lip in the frame's
  // finish running back the panel's depth; grain: the wrinkle's tile size in units across and down, for a panel with bays
  // clear(x, y, r): whether a bolt may go there (a bolt that would meet a part is left out)
  function perimeter(w, h, cy, { across = 13, down = null, inset = .35, scale = 1, z = .03, holes = null, grain = null, clear = null, parent = scene } = {}) {
    let panel;
    if (holes && holes.length) {
      const s = new THREE.Shape(); s.moveTo(-w / 2, cy - h / 2); s.lineTo(w / 2, cy - h / 2); s.lineTo(w / 2, cy + h / 2); s.lineTo(-w / 2, cy + h / 2); s.closePath();
      const lip = .028;
      for (const o of holes) {
        // (the bevel rounds the lip inward from the hole's edge, so the front face's edge is the bay's edge)
        const hw = o.w / 2, hh = o.h / 2, r = Math.min(o.r || 0, hw, hh), p = new THREE.Path();
        p.moveTo(o.x - hw + r, o.y - hh); p.lineTo(o.x + hw - r, o.y - hh); p.quadraticCurveTo(o.x + hw, o.y - hh, o.x + hw, o.y - hh + r);
        p.lineTo(o.x + hw, o.y + hh - r); p.quadraticCurveTo(o.x + hw, o.y + hh, o.x + hw - r, o.y + hh); p.lineTo(o.x - hw + r, o.y + hh);
        p.quadraticCurveTo(o.x - hw, o.y + hh, o.x - hw, o.y + hh - r); p.lineTo(o.x - hw, o.y - hh + r); p.quadraticCurveTo(o.x - hw, o.y - hh, o.x - hw + r, o.y - hh);
        s.holes.push(p);
      }
      const g = new THREE.ExtrudeGeometry(s, { depth: .3 - 2 * lip, bevelEnabled: true, bevelThickness: lip, bevelSize: lip, bevelSegments: 4, curveSegments: 10 });
      g.translate(0, -cy, -.3 + lip);
      // the front's grain as the back's box panel has it: the texture's tiles a set size in units
      const [gx, gy] = grain || [w, h], uv = g.attributes.uv;
      for (let i = 0; i < uv.count; i++) uv.setXY(i, uv.getX(i) / gx, uv.getY(i) / gy);
      panel = new THREE.Mesh(g, [M.panel, M.lip]); panel.position.set(0, cy, 0);
    } else { panel = new THREE.Mesh(new THREE.BoxGeometry(w, h, .3), M.panel); panel.position.set(0, cy, -.15); }
    panel.receiveShadow = true; parent.add(panel);
    const frame = new THREE.Mesh(new THREE.BoxGeometry(w + .5, h + .5, .2), M.frame); frame.position.set(0, cy, -.32); parent.add(frame);
    const bolts = { top: [], bottom: [], left: [], right: [] };
    // each bolt's slot where it was left when it was tightened: the same every time the ring is drawn
    let n = 0; const slot = () => ((++n * 2.399) % Math.PI);
    const put = (list, x, y) => { const sl = slot(); if (!clear || clear(x, y, .09 * scale)) list.push(bolt(x, y, { scale, z, parent, slot: sl })); };
    for (let i = 0; i < across; i++) {
      const x = -w / 2 + inset + i * (w - 2 * inset) / (across - 1);
      put(bolts.top, x, cy + h / 2 - inset); put(bolts.bottom, x, cy - h / 2 + inset);
    }
    const side = down || Math.round((h - 2 * inset) / 2);
    for (let i = 1; i < side; i++) {
      const y = cy - h / 2 + inset + i * (h - 2 * inset) / side;
      put(bolts.left, -w / 2 + inset, y); put(bolts.right, w / 2 - inset, y);
    }
    return { panel, frame, bolts };
  }

  // a blanking plate: wrinkle-finish steel over a hole where nothing is fitted, a slotted bolt in each corner and what it
  // covers engraved across it. Centred on the group's origin, its face at z .05
  const blankMat = M.cover = new THREE.MeshStandardMaterial({ color: '#262724', roughness: .45, metalness: .6, bumpMap: tex.wrinkle, bumpScale: .4 }); blankMat.userData.keep = true;
  function blank(w, h, text, { size = .15, bolts = 1, parent = scene } = {}) {
    const g = new THREE.Group(); parent.add(g);
    const cov = new THREE.Mesh(new THREE.BoxGeometry(w, h, .05), blankMat); cov.position.z = .025; cov.castShadow = true; cov.receiveShadow = true; g.add(cov);
    const dx = w / 2 - .15 * bolts, dy = h / 2 - .12 * bolts;
    // (each bolt's head standing proud of the plate by as much as the back's always have, for its size)
    [[-dx, dy], [dx, dy], [-dx, -dy], [dx, -dy]].forEach(([x, y]) => bolt(x, y, { scale: bolts, z: .05 - .02 * bolts, parent: g }));
    const e = text ? engrave(text, 0, 0, size, '#6f6a5e', { parent: g }) : null; if (e) e.position.z = .055;
    return { group: g, cover: cov, text: e };
  }

  // a rotary switch in bakelite: a skirt turned with a bevelled edge, a fluted grip and a domed cap, a cream enamel line
  // inlaid across the cap and the skirt for its pointer, and its stops engraved on the enamel round it. In the knob's own
  // units its skirt is radius 1; place gives each stop's words ({x, y} of the first baseline's middle or end, two to break
  // after the first word, anchor 'end' to set it right), in the same units, and line the second line's drop
  const knobGeo = (() => {
    const p = [[0, .5], [.52, .5], [.6, .485], [.64, .455], [.66, .41], [.66, .2], [.7, .175], [.93, .165], [.985, .14], [1, .1], [1, .03], [.985, 0], [0, 0]]
      .reverse().map(([r, y]) => new THREE.Vector2(r, y));
    const g = new THREE.LatheGeometry(p, 240), pos = g.attributes.position;
    // 24 flutes cut into the grip (between y .2 and .41), fading out over the cap's shoulder
    for (let i = 0; i < pos.count; i++) {
      const x = pos.getX(i), y = pos.getY(i), z = pos.getZ(i), r = Math.hypot(x, z);
      if (r < .5 || y < .19 || y > .47) continue;
      const a = Math.atan2(z, x), f = Math.pow(Math.max(0, Math.cos(a * 24)), 2), k = y > .41 ? 1 - (y - .41) / .06 : 1;
      const s = 1 - .055 * f * k; pos.setX(i, x * s); pos.setZ(i, z * s);
    }
    g.computeVertexNormals(); g.rotateX(Math.PI / 2); g.userData.keep = true;
    return g;
  })();
  const cream = new THREE.MeshStandardMaterial({ color: '#e9e3d2', roughness: .4, metalness: 0, side: THREE.DoubleSide }); cream.userData.keep = true;
  // the pointer's line: a ribbon .085 wide lying on the knob's turned profile, a hair proud of it, from r .14 to .97
  const pointerRibbon = (() => {
    const prof = [[.14, .5], [.52, .5], [.6, .485], [.64, .455], [.66, .41], [.66, .2], [.7, .175], [.93, .165], [.97, .148]];
    const pos = [], idx = [], hw = .0425, up = .005;
    prof.forEach(([r, h], i) => { pos.push(-hw, r, h + up, hw, r, h + up); if (i) { const a = 2 * i - 2; idx.push(a, a + 1, a + 2, a + 1, a + 3, a + 2); } });
    const g = new THREE.BufferGeometry(); g.setAttribute('position', new THREE.Float32BufferAttribute(pos, 3)); g.setIndex(idx); g.computeVertexNormals(); g.userData.keep = true; return g;
  })();
  function knob({ labels = [], angles = [-120, 0, 120], place = [], line = .5, size = .53, radius = 1, parent = scene } = {}) {
    const g = new THREE.Group(); parent.add(g);
    const rot = new THREE.Group(); g.add(rot);
    // its own bakelite, so a pointer over it can lighten its rim
    const mat = M.bakelite.clone(); mat.userData.keep = false;
    const body = new THREE.Mesh(knobGeo, mat); body.castShadow = true; body.receiveShadow = true; rot.add(body);
    // the pointer: an enamel line inlaid from near the cap's middle, over its shoulder and down the grip, across the skirt
    rot.add(new THREE.Mesh(pointerRibbon, cream));
    // the stops: a tick and the words engraved on the enamel, the chosen stop's lighter
    const stops = labels.map((t, i) => {
      const a = angles[i] * Math.PI / 180, sx = Math.sin(a), sy = Math.cos(a);
      const tick = new THREE.Mesh(new THREE.PlaneGeometry(.047, .16), new THREE.MeshStandardMaterial({ color: '#ffffff', roughness: .7 }));
      tick.position.set(sx * 1.19, sy * 1.19, .004); tick.rotation.z = -a; g.add(tick);
      const P = place[i] || { x: sx * 1.6, y: -sy * 1.6 }, w = t.split(' ');
      const lines = P.two && w.length > 1 ? [w[0], w.slice(1).join(' ')] : [t];
      const words = lines.map((l, j) => {
        const m = engrave(l, 0, 0, size, '#ffffff', { parent: g });
        m.geometry.computeBoundingBox(); const bw = m.geometry.boundingBox.max.x - m.geometry.boundingBox.min.x;
        // engrave() centres the words on their middle: set the first baseline where the SVG had it
        m.position.set(P.anchor === 'end' ? P.x - bw / 2 : P.x, -(P.y + j * line) + size * .36, .004);
        return m;
      });
      return { tick, words };
    });
    let at = 0, from = 0, to = 0, t0 = 0;
    const DUR = 320;
    // the CSS knob's spring: cubic-bezier(.3,1.5,.5,1)
    const spring = x => { const cx = .9, bx = 3 * (.5 - .3) - cx, ax = 1 - cx - bx, cy = 4.5, by = 3 * (1 - 1.5) - cy, ay = 1 - cy - by; let t = x;
      for (let i = 0; i < 6; i++) { const fx = ((ax * t + bx) * t + cx) * t - x, d = (3 * ax * t + 2 * bx) * t + cx; if (Math.abs(fx) < 1e-5) break; t -= fx / d; } return ((ay * t + by) * t + cy) * t; };
    const light = i => stops.forEach((s, j) => { const c = j === i ? '#f1ebd9' : '#8a8476'; s.tick.material.color.set(j === i ? '#e8e1cf' : '#6d685c'); s.words.forEach(w => w.material.color.set(c)); });
    let lit = -1;
    function set(i, animate = false) {
      if (i === lit) return false; lit = i; light(i);
      to = -(angles[i] || 0) * Math.PI / 180;
      if (animate) { from = rot.rotation.z; t0 = performance.now(); } else { from = to; rot.rotation.z = to; t0 = 0; }
      return true;
    }
    // turns it on toward where it was set; true while it is still moving
    function tick(now = performance.now()) {
      if (!t0) return false;
      const k = Math.min(1, (now - t0) / DUR); rot.rotation.z = from + (to - from) * spring(k);
      if (k >= 1) { t0 = 0; rot.rotation.z = to; }
      return true;
    }
    // a pointer over it: the cap's rim a shade lighter, as bakelite catches more of the room when it is leaned toward
    const base = mat.color.clone(), over = base.clone().lerp(new THREE.Color('#5a4c3e'), .35);
    function hover(on) { const c = on ? over : base; if (mat.color.equals(c)) return false; mat.color.copy(c); mat.clearcoatRoughness = on ? .25 : .35; return true; }
    g.scale.setScalar(radius);
    set(0);
    return { group: g, rot, stops, set, tick, hover };
  }

  // a jewel lamp: a glass cabochon in a turned brass bezel. Lit, its core glows in the glass and a small light, too short
  // to reach past it, warms the bezel's lip; no bloom and no halo. Unlit, dark glass with the room in it. Radius 1 is the
  // bezel's outside
  const bezelGeo = (() => { const p = [[.62, 0], [1, 0], [1, .12], [.96, .2], [.86, .26], [.74, .27], [.66, .22], [.62, .14]].map(([r, y]) => new THREE.Vector2(r, y)); const g = new THREE.LatheGeometry(p, 64); g.rotateX(Math.PI / 2); return g; })();
  const domeGeo = (() => { const g = new THREE.SphereGeometry(.7, 48, 24, 0, Math.PI * 2, 0, Math.PI / 2.6); g.rotateX(Math.PI / 2); g.translate(0, 0, -.27); return g; })();
  const coreGeo = new THREE.SphereGeometry(.38, 24, 16);
  const seatMat = new THREE.MeshStandardMaterial({ color: '#0c0b09', roughness: .5, metalness: .2 });
  // empty: the bezel and its dark seat only, a socket with no lamp fitted
  const seatGeo = new THREE.CircleGeometry(.64, 32);
  for (const k of [bezelGeo, domeGeo, coreGeo, seatGeo]) k.userData.keep = true; seatMat.userData.keep = true;
  function jewel(color = '#f0b64c', { radius = 1, light = true, empty = false, parent = scene } = {}) {
    const g = new THREE.Group(); parent.add(g);
    const bezel = new THREE.Mesh(bezelGeo, M.brass); bezel.castShadow = true; bezel.receiveShadow = true; g.add(bezel);
    const seat = new THREE.Mesh(seatGeo, seatMat); seat.position.z = empty ? -.02 : .02; g.add(seat);
    if (empty) { g.scale.setScalar(radius); return { group: g, set: () => false, size: r => g.scale.setScalar(r), on: false }; }
    const glass = new THREE.MeshPhysicalMaterial({ color, roughness: .06, metalness: 0, transmission: .9, ior: 1.5, thickness: .35, clearcoat: 1, clearcoatRoughness: .04, attenuationColor: new THREE.Color(color), attenuationDistance: .6, emissive: new THREE.Color(color), emissiveIntensity: 0 });
    const dome = new THREE.Mesh(domeGeo, glass); dome.position.z = .02; g.add(dome);
    const coreMat = new THREE.MeshStandardMaterial({ color: '#000000', emissive: new THREE.Color(color), emissiveIntensity: 0, roughness: 1 });
    const core = new THREE.Mesh(coreGeo, coreMat); core.position.z = .05; core.scale.set(1, 1, .55); g.add(core);
    const lamp = light ? new THREE.PointLight(color, 0, 2.2 * radius, 2) : null; if (lamp) { lamp.position.z = .35; g.add(lamp); }
    let on = null, lit = color;
    // on, in its own colour or the one given (a lamp that shows more than one state); false if nothing changed
    function set(v, c = color) {
      v = !!v; if (v === on && c === lit) return false; on = v; lit = c;
      coreMat.emissive.set(c); glass.emissive.set(c); if (lamp) lamp.color.set(c);
      coreMat.emissiveIntensity = v ? 1.6 : 0; glass.emissiveIntensity = v ? .22 : 0;
      // dark, the glass is near black: only the room's reflection shows in it
      glass.color.set(v ? c : '#0c0b09'); glass.transmission = v ? .9 : .12; glass.attenuationColor.set(v ? c : '#050403');
      if (lamp) lamp.intensity = v ? .7 * radius * radius : 0;
      return true;
    }
    // drawn at another size: its light reaches only as far as its own lip
    function size(r) { radius = r; g.scale.setScalar(r); if (lamp) { lamp.distance = 1.35 * r; if (on) lamp.intensity = .7 * r * r; } }
    size(radius);
    set(false);
    return { group: g, set, size, get on() { return on; } };
  }

  // a part drawn on its own, in this machine's lights and room, into a picture with a clear background: for hardware that
  // sits on paper, where the 3D scene behind the page can't reach. view 'front' looks straight at its face; 'tilt' a
  // little from above and the left, as the key light falls. With shadow, a catcher under it takes the part's shadow from
  // the key light as see-through dark. Returns a PNG data URL px wide
  let baker = null;
  function bake(object3D, { px = 64, view = 'front', shadow = false, margin = .12 } = {}) {
    if (!baker) {
      const r = new THREE.WebGLRenderer({ antialias: true, alpha: true, preserveDrawingBuffer: true });
      r.toneMapping = renderer.toneMapping; r.toneMappingExposure = renderer.toneMappingExposure; r.setClearColor(0x000000, 0);
      r.shadowMap.enabled = true; r.shadowMap.type = THREE.PCFSoftShadowMap;
      const s = new THREE.Scene(); const pm = new THREE.PMREMGenerator(r); s.environment = pm.fromScene(new RoomEnvironment(), .04).texture; s.environmentIntensity = scene.environmentIntensity; pm.dispose();
      s.add(new THREE.HemisphereLight('#d9d4c8', '#0b0a09', .25));
      const k = new THREE.DirectionalLight(key.color, key.intensity); k.castShadow = true; k.shadow.mapSize.set(1024, 1024); k.shadow.radius = 6; k.shadow.bias = -.0004; s.add(k); s.add(k.target);
      const rm = new THREE.DirectionalLight(rim.color, rim.intensity); s.add(rm);
      baker = { r, s, k, rm };
    }
    const { r, s, k, rm } = baker;
    const box = new THREE.Box3().setFromObject(object3D), size = box.getSize(new THREE.Vector3()), mid = box.getCenter(new THREE.Vector3());
    const m = Math.max(size.x, size.y) * margin, w = size.x + 2 * m, h = size.y + 2 * m;
    const cam = new THREE.OrthographicCamera(-w / 2, w / 2, h / 2, -h / 2, .01, 100);
    const dist = 10;
    if (view === 'tilt') cam.position.set(mid.x - .08 * dist, mid.y + .1 * dist, mid.z + dist); else cam.position.set(mid.x, mid.y, mid.z + dist);
    cam.lookAt(mid);
    // the kit's key and rim, keeping their directions
    const dir = key.position.clone().normalize(); k.position.copy(mid).addScaledVector(dir, dist); k.target.position.copy(mid);
    Object.assign(k.shadow.camera, { left: -w, right: w, top: h, bottom: -h, near: .01, far: dist * 2 }); k.shadow.camera.updateProjectionMatrix();
    rm.position.copy(mid).addScaledVector(rim.position.clone().normalize(), dist);
    let catcher = null;
    if (shadow) { catcher = new THREE.Mesh(new THREE.PlaneGeometry(w * 3, h * 3), new THREE.ShadowMaterial({ opacity: .55 })); catcher.position.set(mid.x, mid.y, box.min.z + .001); catcher.receiveShadow = true; s.add(catcher); }
    const parent = object3D.parent; s.add(object3D);
    object3D.traverse(o => { if (o.isMesh) o.castShadow = true; });
    const H = Math.max(1, Math.round(px * h / w)); r.setPixelRatio(1); r.setSize(px, H, false);
    r.render(s, cam);
    const url = r.domElement.toDataURL('image/png');
    s.remove(object3D); if (parent) parent.add(object3D);
    if (catcher) { s.remove(catcher); catcher.geometry.dispose(); catcher.material.dispose(); }
    return url;
  }
  // let the baker's context go once the parts that need it are drawn
  function bakeDone() { if (baker) { baker.r.dispose(); baker.r.forceContextLoss(); baker = null; } }

  return { THREE, renderer, scene, camera, tex, M, cableMat, CABLE_COLORS, key, rim, makeComposer, bolt, plate, engrave, makersPlate, canvasTex, perimeter, blank, knob, jewel, bake, bakeDone };
}
