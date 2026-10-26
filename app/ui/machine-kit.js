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
  function bolt(x, y, { scale = 1, z = .03 } = {}) {
    const b = new THREE.Mesh(boltGeo, M.steel); b.scale.setScalar(scale); b.position.set(x, y, z); b.castShadow = true; scene.add(b);
    const s = new THREE.Mesh(slotGeo, M.hole); s.scale.setScalar(scale); s.position.set(x, y, z + .032 * scale); s.rotation.z = Math.random() * 3; scene.add(s);
    return b;
  }
  // a traffolyte plate: black (or red) with the text engraved through to white
  function plate(text, x, y, { w = null, h = .26, red = false, size = 56, align = 'center', z = .013, pad = 0 } = {}) { // pad: room each side for screws, in units of the type size
    const c = document.createElement('canvas'); const ctx = c.getContext('2d'); setEngraving(ctx, size); const tw = ctx.measureText(text).width;
    c.width = Math.ceil(tw + size * (1.2 + 2 * pad)); c.height = Math.ceil(size * 1.5); const k = c.getContext('2d');
    const g = k.createLinearGradient(0, 0, 0, c.height); g.addColorStop(0, red ? '#8a2a22' : '#191918'); g.addColorStop(1, red ? '#6d1e18' : '#0b0b0a'); k.fillStyle = g; k.fillRect(0, 0, c.width, c.height);
    setEngraving(k, size); k.fillStyle = red ? '#f2e7da' : '#e9e2cf'; k.textBaseline = 'middle'; k.textAlign = 'center'; engraveText(k, text, c.width / 2, c.height * .54, size, .045);
    const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 8;
    const pw = w || h * c.width / c.height;
    const m = new THREE.Mesh(new THREE.BoxGeometry(pw, h, .025), [M.bakelite, M.bakelite, M.bakelite, M.bakelite, new THREE.MeshStandardMaterial({ map: t, roughness: .55 }), M.bakelite]);
    m.position.set(align === 'left' ? x + pw / 2 : x, y, z); m.castShadow = true; m.receiveShadow = true; scene.add(m);
    return m;
  }
  // letters painted into the enamel
  function engrave(text, x, y, size = .14, color = '#cfc7b2') {
    const s = 64; const c = document.createElement('canvas'); const ctx = c.getContext('2d'); setEngraving(ctx, s); const tw = ctx.measureText(text).width;
    c.width = Math.ceil(tw + 16); c.height = s + 16; const k = c.getContext('2d'); setEngraving(k, s); k.textAlign = 'center'; k.textBaseline = 'middle';
    k.fillStyle = 'rgba(0,0,0,.8)'; engraveText(k, text, c.width / 2, c.height / 2 + 3, s); k.fillStyle = color; engraveText(k, text, c.width / 2, c.height / 2, s);
    const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 8;
    const h = size * (c.height / s), w = h * c.width / c.height;
    const m = new THREE.Mesh(new THREE.PlaneGeometry(w, h), new THREE.MeshStandardMaterial({ map: t, transparent: true, roughness: .7, depthWrite: false }));
    m.position.set(x, y, .004); m.receiveShadow = true; scene.add(m);
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

  return { THREE, renderer, scene, camera, tex, M, cableMat, CABLE_COLORS, key, rim, makeComposer, bolt, plate, engrave, makersPlate, canvasTex };
}
