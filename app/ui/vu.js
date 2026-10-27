// The room meter behind its glass: its own small canvas from the machine kit, with the same lights and room as the
// front panel, so the needle moving redraws only this. The case, the brass bezel and the needle's pivot are the front
// panel's (front-panel.js); this draws what is seen through the window: the printed dial (the page's own SVG, drawn
// into a texture), a steel needle, and the glass over them.
import { createMachine } from './machine-kit.js';

// mountVu(vu, {still}) -> {set(deg), layout(), renders(), dispose()}. vu: the .vu element, whose SVG is the dial
// (viewBox 200 by 104, the window its top 92, the needle pivoting at 100, 96). set takes the needle's angle in degrees
export async function mountVu(vu, { still = false } = {}) {
  const svg = vu.querySelector('svg');
  const host = document.createElement('div'); host.className = 'vu3d'; host.setAttribute('aria-hidden', 'true'); vu.appendChild(host);
  const K = createMachine(host, { still, background: '#e9dfc2' });
  const { THREE, renderer, scene, camera, M, key, rim } = K;
  key.position.set(-6, 9, 22); key.intensity = 2.7; rim.intensity = .45;
  renderer.shadowMap.enabled = false;
  // the window in the dial's own units: 200 across, 92 down; the scene's unit is one dial unit
  const WW = 200, WH = 92, PX = 100, PY = 96, R = 66;
  const toW = (x, y) => new THREE.Vector3(x - WW / 2, WH / 2 - y, 0);

  // the dial: the SVG as printed, without its needle, pivot and foot
  async function paintDial(scale) {
    const c = svg.cloneNode(true);
    c.querySelector('#needle')?.remove(); [...c.querySelectorAll('circle, rect[y="88"]')].forEach(e => e.remove());
    // the words are set on the canvas in the page's own engraving face, which a picture of an SVG can't load
    const words = [...c.querySelectorAll('text')].map(t => { t.remove(); return t; });
    c.removeAttribute('style'); c.setAttribute('width', WW * scale); c.setAttribute('height', 104 * scale); c.setAttribute('viewBox', '0 0 200 104');
    const img = new Image(); img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(new XMLSerializer().serializeToString(c)); await img.decode();
    const dialCv = document.createElement('canvas'); dialCv.width = WW * scale; dialCv.height = WH * scale; const k = dialCv.getContext('2d');
    k.drawImage(img, 0, 0);
    k.save(); k.scale(scale, scale);
    for (const t of words) {
      const px = +t.getAttribute('font-size'), ink = t.getAttribute('fill');
      k.font = `${px}px ${t.getAttribute('font-family')}`; k.textAlign = t.getAttribute('text-anchor') === 'middle' ? 'center' : t.getAttribute('text-anchor') === 'end' ? 'right' : 'left';
      k.fillStyle = k.strokeStyle = ink; k.lineWidth = +(t.getAttribute('stroke-width') || 0); k.lineJoin = 'round';
      k.fillText(t.textContent, +t.getAttribute('x'), +t.getAttribute('y')); if (k.lineWidth) k.strokeText(t.textContent, +t.getAttribute('x'), +t.getAttribute('y'));
    }
    k.restore();
    // printed on card: a faint tooth, and the ink a touch into it
    k.globalCompositeOperation = 'multiply'; k.fillStyle = 'rgba(120,100,60,.05)';
    for (let i = 0; i < 900 * scale; i++) k.fillRect(Math.random() * dialCv.width, Math.random() * dialCv.height, 1 + Math.random() * 2, 1);
    k.globalCompositeOperation = 'source-over';
    // a new texture for each size: a texture keeps the size it was first drawn at
    const t = new THREE.CanvasTexture(dialCv); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 8; t.generateMipmaps = false; t.minFilter = THREE.LinearFilter;
    if (dialMat.map) dialMat.map.dispose(); dialMat.map = t; dialMat.color.set('#ddcca6'); dialMat.needsUpdate = true;
  }
  const dialMat = new THREE.MeshStandardMaterial({ color: '#e9dfc2', roughness: .82, metalness: 0 });
  const dial = new THREE.Mesh(new THREE.PlaneGeometry(WW, WH), dialMat); dial.receiveShadow = true; scene.add(dial);
  // the window's walls, just inside the bezel: shade at the dial's edges where the case stands over it
  const wallMat = new THREE.MeshStandardMaterial({ color: '#17120e', roughness: .5 });
  [[WW + 8, 4, 0, WH / 2 + 1], [WW + 8, 4, 0, -WH / 2 - 1], [4, WH + 8, -WW / 2 - 1, 0], [4, WH + 8, WW / 2 + 1, 0]].forEach(([w, h, x, y]) => {
    const m = new THREE.Mesh(new THREE.BoxGeometry(w, h, 9), wallMat); m.position.set(x, y, 4.5); scene.add(m); });
  // the needle: a steel blade from the pivot, tapering to a point, its tail counterweighted below the window
  const needle = new THREE.Group(); const pv = toW(PX, PY); needle.position.set(pv.x, pv.y, 2.2); scene.add(needle);
  { const s = new THREE.Shape(); s.moveTo(-.95, 0); s.lineTo(.95, 0); s.lineTo(.42, R - 4); s.lineTo(0, R - 2); s.lineTo(-.42, R - 4); s.closePath();
    const blade = new THREE.Mesh(new THREE.ExtrudeGeometry(s, { depth: .35, bevelEnabled: false }), new THREE.MeshStandardMaterial({ color: '#1a1714', roughness: .35, metalness: .7 }));
    blade.castShadow = true; needle.add(blade); }
  // its shadow on the dial: a soft dark copy, cast down and right from the key light
  const shadowMat = new THREE.MeshBasicMaterial({ color: '#000000', transparent: true, opacity: .16, depthWrite: false });
  const shade = new THREE.Group(); shade.position.set(pv.x + .9, pv.y - 1.3, .05); scene.add(shade);
  { const s = new THREE.Shape(); s.moveTo(-1.6, 0); s.lineTo(1.6, 0); s.lineTo(.6, R - 3); s.lineTo(-.6, R - 3); s.closePath(); shade.add(new THREE.Mesh(new THREE.ShapeGeometry(s), shadowMat)); }
  // the glass: clear, with a clearcoat that takes the room's light as a soft streak
  const glass = new THREE.Mesh(new THREE.PlaneGeometry(WW + 8, WH + 8), new THREE.MeshPhysicalMaterial({ color: '#ffffff', roughness: .02, metalness: 0, transparent: true, opacity: .03, clearcoat: .6, clearcoatRoughness: .04, envMapIntensity: .5, depthWrite: false }));
  glass.position.z = 6; scene.add(glass);
  const streak = new THREE.Mesh(new THREE.PlaneGeometry(WW * 1.4, 26), new THREE.MeshBasicMaterial({ map: K.canvasTex(256, 8, (k, w, h) => { const g = k.createLinearGradient(0, 0, w, 0);
    g.addColorStop(0, 'rgba(255,250,240,0)'); g.addColorStop(.35, 'rgba(255,250,240,.04)'); g.addColorStop(.5, 'rgba(255,250,240,.12)'); g.addColorStop(.65, 'rgba(255,250,240,.04)'); g.addColorStop(1, 'rgba(255,250,240,0)'); k.fillStyle = g; k.fillRect(0, 0, w, h); }), transparent: true, depthWrite: false, blending: THREE.AdditiveBlending }));
  streak.material.map.wrapS = streak.material.map.wrapT = THREE.ClampToEdgeWrapping; streak.material.map.colorSpace = THREE.SRGBColorSpace;
  streak.rotation.z = -.48; streak.position.set(-18, 10, 6.2); scene.add(streak);

  camera.fov = 20;
  const composer = K.makeComposer({ ao: false });
  let renders = 0, want = 0, shownAt = null, frame = 0, painted = 0;
  function draw() { frame = 0; needle.rotation.z = shade.rotation.z = -want * Math.PI / 180; shownAt = want; composer.render(); renders++; }
  function layout() {
    const sw = parseFloat(getComputedStyle(svg).width); if (!sw) return;
    const s = sw / WW, w = WW * s, h = WH * s;
    // the window: the dial's top 92 units, over the SVG where it is laid out (the bezel round it is the front panel's)
    // (from the layout, not the rects, so a turn in progress doesn't skew it)
    const cs = getComputedStyle(vu), ox = svg.offsetParent === vu ? svg.offsetLeft : parseFloat(cs.paddingLeft), oy = svg.offsetParent === vu ? svg.offsetTop : parseFloat(cs.paddingTop);
    Object.assign(host.style, { left: ox + 'px', top: oy + 'px', width: w + 'px', height: h + 'px' });
    renderer.setSize(w, h, false); composer.setSize(w, h); camera.aspect = w / h;
    const D = (WH / 2) / Math.tan(camera.fov * Math.PI / 360); camera.position.set(0, 0, D); camera.lookAt(0, 0, 0); camera.near = D / 10; camera.far = D * 2; camera.updateProjectionMatrix();
    const scale = Math.min(6, Math.max(2, Math.ceil(s * Math.min(devicePixelRatio, 2) * 2)));
    if (scale !== painted) { painted = scale; paintDial(scale).then(draw, draw); }
    draw();
  }
  new ResizeObserver(layout).observe(vu);
  return {
    // the needle at deg: drawn on the next frame, once, if it moved more than a tenth of a degree
    set(deg) { want = deg; if (shownAt !== null && Math.abs(want - shownAt) <= .1) return; if (!frame) frame = requestAnimationFrame(draw); },
    layout, renders: () => renders, host,
  };
}
