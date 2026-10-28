// Runs from the anatomy-asset skill's examples/ folder (github.com/Teddarific/skills), as
// examples/bombe-hero/build.mjs; its bombe-hero.svg is copied to docs/media/.
//
// A landing-page hero for Bombechat: the app's machine as a real cabinet. Its front is the
// app's front panel: three rows of twelve colour-coded drums (Plain, Heard, Key) with a brass
// pointer and a letter tab over each, the readout strip under them, and below that the
// message sheet, the key check card, the Sound and Light switch plates, the Bluetooth plate
// and the room meter. The Plain row turns, the meter needle drifts, the jewels glow.
//
//   node build.mjs            dark  → bombe-hero.svg, bombe-hero.html

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as k from "../../kit/iso-kit.mjs";
import * as a from "../../kit/asset.mjs";
import * as G from "../../kit/lathe.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const PAPER = "#141311";

const W = 1200;
const H = 640;
const SAFE = { x: 0, y: 0, w: 470, h: H };
const REGION = { x: 500, y: 40, w: 668, h: 560 };

// ---------- world ----------
// The front face is the plane y = 0, facing +y; the cabinet runs back to y = -DEPTH.
const DEPTH = 64;
const HALF = 166;
const CASE = { x: -HALF, y: -DEPTH, w: 2 * HALF, d: DEPTH, r: 3 };
const PLINTH = { x: -HALF - 6, y: -DEPTH - 6, w: 2 * HALF + 12, d: DEPTH + 12, r: 3 };
const PLINTH_H = 12;
const CASE_TOP = 232;
const LID = { x: -HALF - 3, y: -DEPTH - 3, w: 2 * HALF + 6, d: DEPTH + 6, r: 3 };
const LID_H = 6;

const COLS = Array.from({ length: 12 }, (_, i) => -112 + i * 23.2);
const ROWS = [209, 180, 151];            // Plain, Heard, Key
const DRUM_R = 9.6;

const P = a.fitInto([...k.boxCorners(PLINTH, 0, CASE_TOP + LID_H), [0, 14, 120]], REGION, { pad: 10, azimuth: 64, elevation: 24 });

const at = (p) => k.iso(p, P);
const S = (paths, style) => k.solidSvg(paths, style);
const L = (d, style) => k.lineSvg(d, style);
const D = (pts, style) => k.dotsSvg(pts, style);
const seg = (p, q) => k.segment(p, q, P);
const poly = (pts, closed = true) => `M${pts.map((p) => at(p).map((v) => v.toFixed(1)).join(" ")).join("L")}${closed ? "Z" : ""}`;
const depth = (p) => k.depthOf(p, P);
const byDepth = (items, key = (i) => i) => [...items].sort((p, q) => depth(key(p)) - depth(key(q)));
const cls = (name, svg) => `<g class="ac-${name}">${svg}</g>`;

// A frame on the front face: axis out of the face (+y), angle 0 toward +x, 90° toward +z.
const FF = (x, z, y = 0) => G.frameOf([x, y, z], [0, 1, 0], [1, 0, 0], [0, 0, 1]);
// The face plane as a flat drawing surface: plane x → world +x, plane y → world −z.
const faceMatrix = (x, z, y) => a.planeMatrix([x, y, z], [1, 0, 0], [0, 0, -1], P);
const faceRect = (x0, z0, x1, z1, y = 0) => poly([[x0, y, z0], [x1, y, z0], [x1, y, z1], [x0, y, z1]]);
// A shallow box standing out of the front face: plates, sheets, tabs, counters.
const facePlate = (x0, z0, x1, z1, y0, y1, tone = "mid", inner = "", lit = false) =>
  S(G.prismOf(G.frameOf([0, y0, 0], [0, 1, 0], [1, 0, 0], [0, 0, 1]), [[x0, z0], [x1, z0], [x1, z1], [x0, z1]], 0, y1 - y0, P), { tone, inner, lit });
// A slotted screw head on the front face, its slot at `turn`.
const screw = (x, z, y = 0, turn = 0.6, r = 1.6) => {
  const F = FF(x, z, y);
  const dx = Math.cos(turn) * (r - 0.4);
  const dz = Math.sin(turn) * (r - 0.4);
  return S(G.disc(0, 0.6, r, F, P, { steps: 16 }), { tone: "hi" }) + L(seg([x - dx, y + 0.6, z - dz], [x + dx, y + 0.6, z + dz]), { tone: "lo" });
};

// ---------- base and cabinet ----------
function cabinet() {
  const out = [`<path class="iso-halo" d="${k.haloOf(k.insetPlan(PLINTH, -10), 0, 4, P)}"/>`];
  out.push(cls("ebony", S(k.slabOf(PLINTH, 0, PLINTH_H, P, 3, 0.6), { tone: "mid", inner: L(k.sideSeam(PLINTH, 3.5, P, 3), { tone: "faint" }) })));
  out.push(cls("case", S(k.slabOf(CASE, PLINTH_H, CASE_TOP - PLINTH_H, P, 3, 0.6), { tone: "mid" })));
  out.push(cls("case", S(k.slabOf(LID, CASE_TOP, LID_H, P, 3, 0.8), { tone: "hi", inner: L(k.sideSeam(LID, CASE_TOP + 2, P, 3), { tone: "faint" }) })));
  // The lid is a plain plate held by a row of screws along each long edge.
  const Z = CASE_TOP + LID_H;
  const lidScrews = [];
  for (let i = 0; i < 9; i++) for (const y of [LID.y + 5, LID.y + LID.d - 5]) lidScrews.push([LID.x + 14 + i * ((LID.w - 28) / 8), y]);
  out.push(L(lidScrews.map(([x, y]) => k.ring(x, y, 1.5, Z, P, 14)).join(""), { tone: "lo" }));
  out.push(D(lidScrews.map(([x, y]) => at([x, y, Z])), { size: 0.45 }));
  return out.join("");
}

// The side the viewer sees (+x): a framed plate with screws round it and the maker's plate.
function sideFace() {
  const X = HALF;
  const out = [];
  const frame = [[X, -DEPTH + 6, PLINTH_H + 8], [X, -6, PLINTH_H + 8], [X, -6, CASE_TOP - 8], [X, -DEPTH + 6, CASE_TOP - 8]];
  out.push(L(poly(frame), { tone: "lo" }));
  const inner = frame.map(([x, y, z], i) => [x, y + (i === 0 || i === 3 ? 2.4 : -2.4), z + (i < 2 ? 2.4 : -2.4)]);
  out.push(L(poly(inner), { tone: "faint" }));
  const screws = [];
  for (let i = 0; i < 6; i++) for (const y of [-DEPTH + 3, -3]) screws.push([X, y, PLINTH_H + 14 + i * ((CASE_TOP - PLINTH_H - 28) / 5)]);
  out.push(D(screws.map(at), { size: 0.55, tone: "hi" }));
  // The maker's plate, as on the back of the app's machine.
  const PZ = 150;
  const plate = [[X, -DEPTH + 14, PZ - 8], [X, -14, PZ - 8], [X, -14, PZ + 8], [X, -DEPTH + 14, PZ + 8]];
  out.push(cls("brass", `<path class="iso-top" d="${poly(plate)}"/>` + L(poly(plate), { tone: "mid" })));
  out.push(cls("brass", L([seg([X, -DEPTH + 20, PZ + 2], [X, -20, PZ + 2]), seg([X, -DEPTH + 24, PZ - 3], [X, -24, PZ - 3])].join(""), { tone: "lo", free: true })));
  out.push(cls("brass", D(plate.map(at), { size: 0.5, tone: "hi" })));
  return out.join("");
}

// ---------- front: the frame and the drum bank ----------
const FRONT = { x0: -HALF + 4, x1: HALF - 4, z0: PLINTH_H + 4, z1: CASE_TOP - 4 };
const BANK = { x0: -156, x1: 156, z0: 128, z1: 224 };

function frontFrame() {
  const out = [];
  out.push(L(faceRect(FRONT.x0, FRONT.z0, FRONT.x1, FRONT.z1), { tone: "lo" }));
  // Slotted screws round the frame, each at its own turn, as in the app.
  const spots = [];
  for (let i = 0; i <= 12; i++) {
    const x = FRONT.x0 + 3 + i * ((FRONT.x1 - FRONT.x0 - 6) / 12);
    spots.push([x, FRONT.z1 - 2.6], [x, FRONT.z0 + 2.6]);
  }
  for (let i = 1; i < 7; i++) {
    const z = FRONT.z0 + 2.6 + i * ((FRONT.z1 - FRONT.z0 - 5.2) / 7);
    spots.push([FRONT.x0 + 2.6, z], [FRONT.x1 - 2.6, z]);
  }
  spots.forEach(([x, z], i) => out.push(screw(x, z, 0, (i * 1.37) % Math.PI, 1.2)));
  // The drum bank's field: a recessed panel with a lip.
  out.push(facePlate(BANK.x0, BANK.z0, BANK.x1, BANK.z1, 0, 0.8, "lo"));
  out.push(L(faceRect(BANK.x0 + 2.4, BANK.z0 + 2.4, BANK.x1 - 2.4, BANK.z1 - 2.4, 0.8), { tone: "faint" }));
  return out.join("");
}

// Row labels: a dark plate per row with a rule for its name and a rivet at each end.
function rowLabels() {
  const out = [];
  for (const z of ROWS) {
    out.push(cls("ebony", facePlate(-150, z - 3.6, -130, z + 3.6, 0.8, 1.8, "hi")));
    out.push(L(seg([-145, 1.8, z], [-136, 1.8, z]), { tone: "hi", free: true }));
    out.push(D([at([-148, 1.8, z]), at([-132, 1.8, z])], { size: 0.4, tone: "hi" }));
  }
  return out.join("");
}

// The rods that run through each row behind the drums.
function rods() {
  const out = [];
  for (const z of ROWS) {
    const F = G.frameOf([-128, 2.6, z], [1, 0, 0], [0, 1, 0], [0, 0, 1]);
    out.push(S(G.disc(0, COLS.at(-1) + 22 - -128, 1, F, P, { steps: 12 }), { tone: "hi" }));
  }
  return out.join("");
}

// Rim colours read off the app's front panel, row by row.
const PALETTE = [
  ["red", "umber", "blue", "blue", "brass", "green", "rose", "umber", "cream", "red", "brass", "green"],
  ["blue", "brass", "green", "rose", "umber", "cream", "red", "brass", "green", "red", "umber", "blue"],
  ["red", "umber", "cream", "red", "brass", "green", "rose", "umber", "blue", "blue", "brass", "green"],
];

// A drum's face: the alphabet as 26 ticks (every fifth long) inside a ring. Drawn in face-plane
// coordinates around (0, 0), so the Plain row can turn it with amSpin.
function drumFace(r, turn) {
  const ticks = [];
  for (let i = 0; i < 26; i++) {
    const t = turn + (i / 26) * Math.PI * 2;
    const r0 = i % 5 === 0 ? r - 2.6 : r - 1.8;
    ticks.push(`M${(Math.cos(t) * r0).toFixed(2)} ${(Math.sin(t) * r0).toFixed(2)}L${(Math.cos(t) * (r - 0.7)).toFixed(2)} ${(Math.sin(t) * (r - 0.7)).toFixed(2)}`);
  }
  const ring = (rr) => `M${rr} 0A${rr} ${rr} 0 1 0 ${-rr} 0A${rr} ${rr} 0 1 0 ${rr} 0Z`;
  // The A mark: one bright tick, so the turn reads at a glance.
  const mark = `M${(Math.cos(turn) * (r - 3.6)).toFixed(2)} ${(Math.sin(turn) * (r - 3.6)).toFixed(2)}L${(Math.cos(turn) * (r - 0.7)).toFixed(2)} ${(Math.sin(turn) * (r - 0.7)).toFixed(2)}`;
  return L(ticks.join(""), { tone: "mid" }) + L(ring(r - 0.7), { tone: "lo" }) + L(ring(r - 3.2), { tone: "faint" }) + L(mark, { tone: "hi" });
}

function drum(x, z, colour, turn, { spin = null } = {}) {
  const F = FF(x, z, 0.8);
  const R = DRUM_R;
  const out = [];
  out.push(cls(colour, S(G.disc(0, 4.6, R, F, P, { bevel: 0.5 }), { tone: "hi", inner: L(G.arcOf(F, 1.6, R, P), { tone: "faint" }) })));
  out.push(cls("ebony", S(G.disc(4.6, 5.2, R - 1.8, F, P), { tone: "mid" })));
  const face = drumFace(R - 1.8, turn);
  const m = faceMatrix(x, z, 6);
  out.push(spin ? a.amSpin(face, m, spin) : `<g transform="${m}">${face}</g>`);
  // The silver hub and its slotted screw.
  out.push(S(G.disc(5.2, 6.4, 2.4, F, P, { steps: 20 }), { tone: "hi" }));
  // The hub's slot turns with the drum, in step with its face.
  const s = 1.7;
  const slot = L(`M${(-Math.cos(turn) * s).toFixed(2)} ${(Math.sin(turn) * s).toFixed(2)}L${(Math.cos(turn) * s).toFixed(2)} ${(-Math.sin(turn) * s).toFixed(2)}`, { tone: "lo" });
  const hm = faceMatrix(x, z, 7.2);
  out.push(spin ? a.amSpin(slot, hm, spin) : `<g transform="${hm}">${slot}</g>`);
  // The letter tab at the top of the drum: a cream card that stays put while the drum turns.
  out.push(cls("paper", facePlate(x - 2.6, z + R - 6.2, x + 2.6, z + R - 1.2, 6, 6.5, "hi")));
  out.push(L(seg([x - 1, 6.5, z + R - 4.4], [x + 1, 6.5, z + R - 2.6]), { tone: "lo", free: true }));
  // The brass pointer over the drum.
  out.push(cls("brass", S(G.prismOf(FF(0, 0, 0.8), [[x - 1.8, z + R + 3.4], [x + 1.8, z + R + 3.4], [x, z + R + 0.6]], 0, 0.8, P), { tone: "hi" })));
  return out.join("");
}

// The Plain row turns as a message lands; Heard and Key hold still.
function drums() {
  const out = [];
  const all = [];
  ROWS.forEach((z, r) => COLS.forEach((x, c) => all.push({ x, z, r, c })));
  for (const d of byDepth(all, (d) => [d.x, 0, d.z])) {
    const turn = ((d.c * 7 + d.r * 11) % 26) / 26 * Math.PI * 2;
    const spin = d.r === 0 ? { period: 9000 + (d.c % 4) * 1700, delay: d.c * 700, reverse: d.c % 2 === 1 } : null;
    out.push(drum(d.x, d.z, PALETTE[d.r][d.c], turn, { spin }));
  }
  return out.join("");
}

// The readout strip under the drums: Frame, Bytes a second, Over the room, Frames checked.
function readouts() {
  const out = [];
  const z = BANK.z0 + 6;
  const label = (x0, x1) => {
    out.push(cls("ebony", facePlate(x0, z - 3, x1, z + 3, 0.8, 1.6, "hi")));
    out.push(L(seg([x0 + 3, 1.6, z], [x1 - 3, 1.6, z]), { tone: "mid", free: true }));
  };
  const counter = (x0, n) => {
    out.push(cls("ebony", facePlate(x0, z - 3.6, x0 + n * 4.6 + 1, z + 3.6, 0.8, 2.2, "mid")));
    for (let i = 0; i < n; i++) out.push(cls("paper", facePlate(x0 + 1 + i * 4.6, z - 2.4, x0 + 4.6 + i * 4.6, z + 2.4, 2.2, 2.4, "mid")));
  };
  label(-128, -108); counter(-105, 1);
  label(-86, -60); counter(-57, 3);
  label(-36, -6); counter(-3, 2);
  label(16, 40);
  // Frames checked: five little jewels, the first two lit.
  for (let i = 0; i < 5; i++) {
    const x = 46 + i * 5;
    const F = FF(x, z, 0.8);
    out.push(cls("brass", S(G.disc(0, 0.8, 1.9, F, P, { steps: 16 }), { tone: "mid" })));
    const jewel = cls(i < 2 ? "amber" : "ebony", S(G.disc(0.8, 1.6, 1.2, F, P, { steps: 16 }), { tone: "hi", lit: i < 2 }));
    out.push(i === 1 ? a.amPulse(jewel, { period: 2200, min: 0.4 }) : jewel);
  }
  return out.join("");
}

// ---------- front: the lower panel ----------
const LOW_TOP = 124;

// The message sheet: a cream sheet clipped at the top, ruled with its header and entries.
function messageSheet() {
  const X0 = -154, X1 = -48, Z0 = 22, Z1 = LOW_TOP - 2;
  const out = [];
  out.push(cls("paper", facePlate(X0, Z0, X1, Z1, 0, 0.8, "hi")));
  const y = 0.8;
  const rule = (z, x0 = X0, x1 = X1, tone = "lo") => out.push(L(seg([x0, y, z], [x1, y, z]), { tone }));
  // Title, then the header's three columns under a double rule.
  out.push(cls("green", L(seg([X0 + 5, y, Z1 - 8], [X0 + 34, y, Z1 - 8]), { tone: "hi", free: true })));
  rule(Z1 - 13); rule(Z1 - 14.2);
  for (const x of [X0 + 38, X0 + 72]) out.push(L(seg([x, y, Z1 - 14.2], [x, y, Z1 - 25]), { tone: "faint" }));
  rule(Z1 - 25);
  // Entries: a time in the margin and a line of type each, one still arriving.
  const lines = [[Z1 - 33, 70], [Z1 - 43, 62], [Z1 - 53, 74], [Z1 - 63, 48]];
  for (const [i, [z, len]] of lines.entries()) {
    out.push(L(seg([X0 + 5, y, z], [X0 + 13, y, z]), { tone: "mid", free: true }));
    const typed = L(seg([X0 + 22, y, z], [X0 + 22 + len, y, z]), { tone: i === 3 ? "mid" : "lo", free: true });
    out.push(i === 3 ? a.amPulse(typed, { period: 1800, min: 0.3 }) : typed);
  }
  out.push(L(seg([X0 + 18, y, Z1 - 25], [X0 + 18, y, Z0 + 18]), { tone: "faint" }));
  rule(Z0 + 18);
  // The reply line and its buttons.
  out.push(cls("ebony", facePlate(X0 + 4, Z0 + 6, X0 + 16, Z0 + 13, 0.8, 1.8, "hi")));
  out.push(L(seg([X0 + 20, y, Z0 + 8], [X0 + 62, y, Z0 + 8]), { tone: "faint", free: true }));
  out.push(cls("ebony", facePlate(X0 + 66, Z0 + 6, X0 + 80, Z0 + 12, 0.8, 1.8, "hi")));
  out.push(cls("ebony", facePlate(X0 + 83, Z0 + 6, X1 - 4, Z0 + 12, 0.8, 1.8, "hi")));
  out.push(cls("amber", D([at([X0 + 86, 1.8, Z0 + 9])], { size: 0.9, tone: "lit" })));
  // Two brass clips hold it at the top.
  for (const x of [X0 + 18, X1 - 18]) {
    out.push(cls("brass", facePlate(x - 2.4, Z1 - 4, x + 2.4, Z1 + 3, 0.8, 2.4, "hi", "", true)));
    out.push(D([at([x, 2.4, Z1 + 0.6])], { size: 0.5, tone: "lo" }));
  }
  return out.join("");
}

// The key check card: graph paper with the menu drawn on it, letters joined in a loop.
function keyCheck() {
  const X0 = -42, X1 = 18, Z0 = 22, Z1 = LOW_TOP - 4;
  const y = 0.8;
  const out = [cls("card", facePlate(X0, Z0, X1, Z1, 0, 0.8, "hi"))];
  const grid = [];
  for (let x = X0 + 4; x < X1 - 2; x += 4) grid.push(seg([x, y, Z0 + 2], [x, y, Z1 - 2]));
  for (let z = Z0 + 4; z < Z1 - 2; z += 4) grid.push(seg([X0 + 2, y, z], [X1 - 2, y, z]));
  out.push(L(grid.join(""), { tone: "faint" }));
  // Grid lines stop at a faint border so every end lands on something.
  out.push(L(faceRect(X0 + 2, Z0 + 2, X1 - 2, Z1 - 2, y), { tone: "faint" }));
  const nodes = [[-34, 86], [-22, 92], [-10, 80], [4, 88], [10, 72], [-4, 60], [-18, 54], [-30, 64]];
  const edges = [[0, 1], [1, 2], [2, 3], [3, 4], [4, 5], [5, 6], [6, 7], [7, 0], [2, 5], [1, 7]];
  out.push(L(edges.map(([p, q]) => seg([nodes[p][0], y, nodes[p][1]], [nodes[q][0], y, nodes[q][1]])).join(""), { tone: "lo" }));
  for (const [x, z] of nodes) out.push(`<g class="ac-card"><path class="iso-top" d="${G.circleOf(FF(x, z, y), 0, 2, P, 20)}"/></g>` + L(G.circleOf(FF(x, z, y), 0, 2, P, 20), { tone: "lo" }));
  // The "Matched" stamp, in violet ink, round one letter.
  out.push(cls("violet", L(G.circleOf(FF(-30, 46, y), 0, 7, P, 36), { tone: "hi" })));
  // Her key and mine, two ruled lines of figures, and the punched key along the bottom.
  for (const z of [38, 33]) out.push(L(seg([X0 + 4, y, z], [X1 - 8, y, z]), { tone: "lo", free: true }));
  const holes = [];
  for (let i = 0; i < 22; i++) for (const dz of [0, 2]) if ((i * 5 + dz * 3) % 4 !== 0) holes.push(at([X0 + 5 + i * 2.4, y, Z0 + 5 + dz]));
  out.push(D(holes, { size: 0.5, tone: "lo" }));
  return out.join("");
}

// A switch plate: a dark plate with a jewel lamp, a rotary switch and its name plate.
function switchPlate(x0, x1, { on, turn, delay }) {
  const Z0 = 74, Z1 = LOW_TOP - 2;
  const out = [cls("ebony", facePlate(x0, Z0, x1, Z1, 0, 1.2, "mid"))];
  for (const [x, z] of [[x0 + 2.6, Z0 + 2.6], [x1 - 2.6, Z0 + 2.6], [x0 + 2.6, Z1 - 2.6], [x1 - 2.6, Z1 - 2.6]]) out.push(D([at([x, 1.2, z])], { size: 0.45, tone: "hi" }));
  const cx = (x0 + x1) / 2;
  // The jewel.
  const JF = FF(cx, Z1 - 8, 1.2);
  out.push(cls("brass", S(G.disc(0, 1, 3, JF, P, { steps: 20 }), { tone: "mid" })));
  const jewel = cls(on ? "amber" : "ebony", S(G.disc(1, 2.4, 2, JF, P, { steps: 20 }), { tone: "hi", lit: on }));
  out.push(on ? a.amPulse(jewel, { period: 3600, delay, min: 0.55 }) : jewel);
  // The rotary switch: a scale arc, a knurled knob and its pointer.
  const KZ = (Z0 + Z1) / 2 - 2;
  const KF = FF(cx, KZ, 1.2);
  out.push(L(G.arcOf(KF, 0, 8.6, P, { least: -1, from: Math.PI * 0.95, to: Math.PI * 2.05 }), { tone: "faint" }));
  out.push(D([Math.PI * 0.95, Math.PI * 1.5, Math.PI * 2.05].map((t) => at([cx + Math.cos(t) * 8.6, 1.2, KZ + Math.sin(t) * 8.6])), { size: 0.5, tone: "mid" }));
  out.push(S(G.disc(0, 3.6, 5.6, KF, P, { bevel: 0.4 }), { tone: "hi", inner: L(G.ribsOf(KF, 0.4, 3.2, 5.6, 28, P), { tone: "lo" }) }));
  out.push(L(seg([cx, 4.8, KZ], [cx + Math.cos(turn) * 4.4, 4.8, KZ - Math.sin(turn) * 4.4]), { tone: "hi" }));
  out.push(D([at([cx, 4.8, KZ])], { size: 0.5, tone: "hi" }));
  // The name plate under it.
  out.push(cls("ebony", facePlate(cx - 8, Z0 + 4, cx + 8, Z0 + 9, 1.2, 2, "hi")));
  out.push(L(seg([cx - 4, 2, Z0 + 6.5], [cx + 4, 2, Z0 + 6.5]), { tone: "hi", free: true }));
  return out.join("");
}

// The Bluetooth plate, not fitted on this build: an empty panel and a red name plate.
function bluetooth() {
  const x0 = 116, x1 = 154, Z0 = 74, Z1 = LOW_TOP - 2;
  const out = [cls("ebony", facePlate(x0, Z0, x1, Z1, 0, 1.2, "mid"))];
  out.push(L(faceRect(x0 + 5, Z0 + 14, x1 - 5, Z1 - 12, 1.2), { tone: "lo" }));
  out.push(L(seg([x0 + 12, 1.2, (Z0 + Z1) / 2 + 1], [x1 - 12, 1.2, (Z0 + Z1) / 2 + 1]), { tone: "faint", free: true }));
  const JF = FF((x0 + x1) / 2, Z1 - 6, 1.2);
  out.push(cls("brass", S(G.ringBand(0, 1, 1.6, 2.8, JF, P, { steps: 20 }), { tone: "mid" })));
  out.push(cls("red", facePlate(x0 + 4, Z0 + 4, x1 - 4, Z0 + 10, 1.2, 2, "hi")));
  out.push(cls("red", L(seg([x0 + 10, 2, Z0 + 7], [x1 - 10, 2, Z0 + 7]), { tone: "hi", free: true })));
  return out.join("");
}

// The room meter: a cream dial in a dark case, its scale running into red, the needle drifting.
function meter() {
  const x0 = 24, x1 = 80, Z0 = 24, Z1 = 66;
  const out = [cls("ebony", facePlate(x0, Z0, x1, Z1, 0, 2.4, "mid"))];
  out.push(cls("card", facePlate(x0 + 3, Z0 + 3, x1 - 3, Z1 - 3, 2.4, 2.8, "hi")));
  const cx = (x0 + x1) / 2;
  const pz = Z0 + 6;
  const R = 26;
  const ticks = [];
  const reds = [];
  for (let i = 0; i <= 16; i++) {
    const t = Math.PI * (0.78 - i * (0.56 / 16));
    const len = i % 4 === 0 ? 3.4 : 2;
    const d = seg([cx + Math.cos(t) * R, 2.8, pz + Math.sin(t) * R], [cx + Math.cos(t) * (R - len), 2.8, pz + Math.sin(t) * (R - len)]);
    (i >= 10 ? reds : ticks).push(d);
  }
  const arc = [];
  for (let i = 0; i <= 32; i++) {
    const t = Math.PI * (0.78 - i * (0.56 / 32));
    arc.push([cx + Math.cos(t) * R, 2.8, pz + Math.sin(t) * R]);
  }
  out.push(L(poly(arc, false), { tone: "lo" }));
  out.push(L(ticks.join(""), { tone: "lo" }));
  out.push(cls("red", L(reds.join("") + poly(arc.slice(20), false), { tone: "hi" })));
  // The needle, pivoted under the scale, drifting with the room.
  const needle = L(`M0 0L0 ${-(R - 1)}`, { tone: "lo" });
  out.push(a.amSwing(needle, faceMatrix(cx, pz, 3), { from: -14, to: 6, period: 5200 }));
  out.push(S(G.disc(0, 1, 2.2, FF(cx, pz, 2.8), P, { steps: 16 }), { tone: "lo" }));
  return out.join("");
}

// The public notice slip in its brass frame, and the contact tags under it.
function slips() {
  const out = [];
  const tag = (x0, z0, x1, z1) => {
    out.push(cls("brass", facePlate(x0, z0, x1, z1, 0, 1.4, "mid")));
    out.push(cls("paper", facePlate(x0 + 2, z0 + 2, x1 - 2, z1 - 2, 1.4, 1.8, "hi")));
  };
  tag(86, 40, 154, 66);
  for (const [i, z] of [60, 54, 48].entries()) out.push(L(seg([90, 1.8, z], [140 - i * 9, 1.8, z]), { tone: "lo", free: true }));
  tag(86, 22, 118, 36);
  tag(122, 22, 154, 36);
  for (const x of [90, 126]) {
    out.push(L(seg([x, 1.8, 31], [x + 20, 1.8, 31]), { tone: "lo", free: true }));
    out.push(L(seg([x, 1.8, 27], [x + 12, 1.8, 27]), { tone: "faint", free: true }));
  }
  return out.join("");
}

// Paper stays cream on a dark page: the accent ramp darkens light colours for dark themes, so
// the paper, card and ink tokens are set by hand here.
const ramp = (n, t) => `#bombe-hero .ac-${n}{${Object.entries(t).map(([key, v]) => `--anatomy-${key}:${v}`).join(";")}}`;
const PAPER_CSS = [
  ramp("paper", { paper: "#d8cdb5", face: "#ddd2ba", top: "#e3d9c2", "shade-0": "#b3a88f", "shade-1": "#bdb299", "shade-2": "#c7bca3", "shade-3": "#cfc4ab", "lit-top": "#ece3cf", "lit-shade": "#d2c7ae", hi: "#3b3429", mid: "#6d6350", lo: "#5a5141", faint: "#a3987f", lit: "#2f2a21", dot: "#3b3429" }),
  ramp("card", { paper: "#e4ddcd", face: "#e9e2d2", top: "#efe9db", "shade-0": "#bfb7a5", "shade-1": "#c9c1af", "shade-2": "#d3cbb9", "shade-3": "#dbd3c1", "lit-top": "#f5f0e5", "lit-shade": "#ddd5c3", hi: "#3b3429", mid: "#6d6350", lo: "#5a5141", faint: "#b4b9c4", lit: "#2f2a21", dot: "#3b3429" }),
  ramp("ink", { hi: "#3b3429", mid: "#6d6350", lo: "#4f473a", faint: "#a3987f", dot: "#3b3429" }),
].join("\n");

// ---------- assemble ----------
const body = [
  cabinet(),
  sideFace(),
  frontFrame(),
  rowLabels(),
  rods(),
  drums(),
  readouts(),
  cls("ink", messageSheet()),
  cls("ink", keyCheck()),
  switchPlate(24, 66, { on: true, turn: -0.7, delay: 0 }),
  switchPlate(70, 112, { on: true, turn: -0.5, delay: 1500 }),
  bluetooth(),
  cls("ink", meter()),
  cls("ink", slips()),
].join("");

const name = "bombe-hero";
const svg = a.assetSvg({
  width: W,
  height: H,
  label: "Bombechat's machine as an isometric cabinet: three rows of twelve colour-coded drums labelled Plain, Heard and Key, the top row turning, and below them a clipped message sheet, a key check card, two lit switch plates, the Bluetooth plate and a room meter whose needle drifts.",
  body,
  paper: PAPER,
  accents: {
    brass: "#c29a5c",
    red: "#a4473a",
    rose: "#b25a5e",
    blue: "#4f6a8a",
    green: "#4a7a5e",
    cream: "#d9cfbb",
    umber: "#8a6446",
    ebony: "#0c0b0a",
    case: "#36322c",
    amber: "#f0b54a",
    violet: { hex: "#7a5aa8", mode: "ink" },
  },
  weight: 0.7,
  id: name,
  css: PAPER_CSS,
});
writeFileSync(join(HERE, `${name}.svg`), svg);
writeFileSync(join(HERE, `${name}.html`), a.previewHtml({ title: name, svg, paper: PAPER, width: W, safe: SAFE }));
console.log(`wrote ${name}.svg (${(svg.length / 1024).toFixed(0)} KB) and ${name}.html`);
