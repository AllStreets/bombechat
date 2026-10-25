//! Reading light codes out of a camera picture.
//!
//! [`read`] finds every code in view and returns the frame bytes of each
//! it can read, ready for [`crate::Collector::push`]. It needs no spec:
//! it tries each preset grid size the finder spacing allows, and the
//! frame's own Reed-Solomon parity and CRC decide which reading is right.
//!
//! # Which finders
//!
//! A picture can hold far more finder-like marks than a code has: tiles,
//! a print of dots, the edge of a patterned cloth. Each mark is first
//! tested for a code corner's surroundings (`code_corner`): light quiet
//! zone round more than half of it, and the code's own cells, not all
//! light, round the rest. A mark in a lattice of others, or alone on a
//! blank ground, fails.
//!
//! Triples are then tried in two groups, each most plausible first:
//!
//! - Triples joined by timing lines. Among the marks that look like code
//!   corners, every pair of about the same size whose distance apart
//!   suits a preset is tested for the timing line a code's finders have
//!   between them (`detect::timing_line`), and a mark with such lines to
//!   two others, each on the side facing the third, makes a triple. So a
//!   code whose finders look like corners is found among any number of
//!   other marks, however much cleaner they are than its own blurred
//!   finders, up to a bound on walking: the walks stop at `MAX_WALK`
//!   (500 thousand) pixels, pairs closest to a preset's spacing first. A
//!   pair that is not a code's costs a few cells' walk; the most any test
//!   picture has needed is 155 thousand.
//! - The best `MAX_CANDIDATES` (16) marks' triples by shape alone (code
//!   corners first, then the rest, each by how well they keep the finder
//!   ratio), for a code whose timing lines cannot be walked or whose
//!   quiet zone is partly covered. Such a code can still be crowded out
//!   by more than 16 marks that look better.
//!
//! For each triple and each grid size that fits its spacing, the reader
//! solves the perspective, samples every cell, and thresholds each cell
//! against the mean of the cells around it. That local mean sits halfway
//! between light and dark because every 8 by 8 block of a drawn code is
//! 40 to 60 percent dark, so blur, glare and dim light move the threshold
//! with the cells.
//!
//! The orientation mark says whether the picture is mirrored (front
//! cameras often mirror their feed); a mirrored picture is read
//! transposed. Cells closest to the threshold mark their bytes as
//! doubtful, and if the frame does not repair as read, it is retried with
//! the most doubtful bytes treated as Reed-Solomon erasures.
//!
//! A fit whose finders, timing lines and orientation mark each mostly
//! read right is a code in view: if no reading of it passes the CRC, the
//! result is [`ReadOutcome::Unreadable`] rather than
//! [`ReadOutcome::NoCode`], so the interface can say it sees a code it
//! cannot read. Once a code is seen its finders are spent, so an
//! unreadable code cannot use up the attempts a readable one needs.
//!
//! # How often a wrong frame could get through
//!
//! Only a fit that is a code in view is ever decoded. A wrong reading is
//! accepted only if, after Reed-Solomon repair, its version and preset
//! bytes are right and its CRC-32 matches its data. Treating a wrong
//! repair as random bytes, that is one chance in 2^16 times 2^32 = 2^48
//! per reading checked. One picture checks at most 12 fits, 2 readings
//! (straight and transposed) each, 3 repairs (errors only, then two
//! erasure counts) each, under 1 mask, or all 16 when the format field
//! is unreadable: at most 1152 readings, so a wrong frame at most once in
//! 2^48 / 1152, about 2.4 * 10^11 pictures, or once in 250 years of a
//! code in view at 30 pictures a second. Wrong readings of real frames
//! (a picture blending two frames, say) have the right version and
//! preset already, so only the CRC's 2^32 counts: at most once in 3.7
//! million pictures if every format field were unreadable, once in 60
//! million with one mask (72 readings). Repairs that use erasures keep
//! some parity spare, so Reed-Solomon itself turns most wrong guesses
//! away before the CRC is asked, and a frame that did slip through would
//! still fail the envelope's own authentication.

use std::sync::OnceLock;

use image::GrayImage;

use crate::code::{orientation, repair_frame, Cell, CodeSpec, Layout, Orientation};
use crate::detect::{find_finders, locate, sample_cells, timing_line, Finder, Picture};

/// Most finder candidates considered when forming triples.
const MAX_CANDIDATES: usize = 16;

/// Most pixels the timing-line walks of one picture may take together.
const MAX_WALK: usize = 500_000;

/// Most grid fits (one triple at one grid size) tried in one picture.
const MAX_ATTEMPTS: usize = 12;

/// Pictures whose longer side is over this are shrunk by a whole factor
/// before reading.
const MAX_SIDE: u32 = 1280;

/// Side of the window, in cells, a cell's threshold is the mean of.
const THRESHOLD_WINDOW: usize = 9;

/// What one camera picture showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// No light code in view.
    NoCode,
    /// A code is in view but could not be read.
    Unreadable,
    /// The frame bytes of each code read, one per code in view.
    Frames(Vec<Vec<u8>>),
}

/// Read every light code in `image`, a grey camera picture of any size.
/// A picture whose longer side is over 1280 pixels is first shrunk by the
/// smallest whole factor that brings that side to 1280 or under
/// (averaging each square of pixels), so a 4K picture, landscape or
/// portrait, costs about what a 720p one does; codes in
/// it must then still have cells of about 3 pixels or more after
/// shrinking. Never panics.
pub fn read(image: &GrayImage) -> ReadOutcome {
    if image.width() < 32 || image.height() < 32 {
        return ReadOutcome::NoCode;
    }
    let side = image.width().max(image.height());
    if side > MAX_SIDE {
        return read(&shrink(image, side.div_ceil(MAX_SIDE)));
    }
    let pic = Picture::new(image);
    // Marks that look like a code's corners first, the rest after.
    let (mut finders, rest): (Vec<Finder>, Vec<Finder>) = find_finders(&pic)
        .into_iter()
        .partition(|f| code_corner(&pic, f));
    let corners = finders.len();
    finders.extend(rest);
    // Triples joined by timing lines, then the best marks' triples by
    // shape alone.
    let mut candidates = linked_triples(&pic, &finders[..corners]);
    for t in triples(&finders) {
        if !candidates.iter().any(|c| c.finders == t.finders) {
            candidates.push(t);
        }
    }
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut seen_code = false;
    let mut used = vec![false; finders.len()];
    let mut attempts = 0;

    for triple in candidates {
        if attempts >= MAX_ATTEMPTS {
            break;
        }
        if triple.finders.iter().any(|&i| used[i]) {
            continue;
        }
        if !quiet_outside(&pic, triple.finders.map(|i| finders[i])) {
            continue;
        }
        // Fit the grid sizes the spacing allows, closest first, until one
        // is a code in view (a wrong size misses the timing lines).
        let three = triple.finders.map(|i| finders[i]);
        let mut found = None;
        for spec in &triple.specs {
            if attempts >= MAX_ATTEMPTS {
                break;
            }
            attempts += 1;
            let Some(h) = locate(&pic, three, spec.cells) else {
                continue;
            };
            let grid = Grid::new(*spec, sample_cells(&pic, &h, spec.cells));
            if grid.fixed_cells().in_view() {
                found = Some(grid);
                break;
            }
        }
        let Some(grid) = found else {
            continue;
        };
        // A code is here: its finders belong to it, read or not, so they
        // cannot spend more attempts.
        seen_code = true;
        for &i in &triple.finders {
            used[i] = true;
        }
        if let Some(bytes) = grid.decode() {
            if !frames.contains(&bytes) {
                frames.push(bytes);
            }
        }
    }

    if !frames.is_empty() {
        ReadOutcome::Frames(frames)
    } else if seen_code {
        ReadOutcome::Unreadable
    } else {
        ReadOutcome::NoCode
    }
}

/// Whether a mark has a code corner's surroundings. Just outside a code's
/// finder, the quiet zone covers more than half a turn and the code's own
/// cells the rest. Sixteen directions are tested at points five and a
/// quarter and six of the mark's cells out (clear of its own corners,
/// which reach just under five). A direction is light when both points
/// are nearer the mark's light ring than its dark centre, and busy when
/// either falls short of three quarters of the way from dark to light.
/// At least seven light directions in a row (round the circle) and at
/// least two busy ones are needed. A mark in a lattice of others with
/// gaps under about two and a half cells has a neighbour's dark ring
/// along each of its axes at one distance or the other; one with wider
/// gaps, or alone on a blank ground, has nothing busy round it.
fn code_corner(pic: &Picture, f: &Finder) -> bool {
    const POINTS: usize = 16;
    const RUN: usize = 7;
    let at = |angle: f32, d: f32| {
        let (s, c) = angle.sin_cos();
        pic.at(f.x + c * d * f.module, f.y + s * d * f.module)
    };
    let turn = std::f32::consts::TAU;
    let dark = at(0.0, 0.0);
    // The light ring, two cells out, at whatever rotation.
    let ring = (0..8)
        .map(|k| at(turn * k as f32 / 8.0, 2.0))
        .fold(f32::MIN, f32::max);
    let mid = (dark + ring) / 2.0;
    // Quiet zone is light past halfway; a busy direction is one whose
    // nearer or further point falls short of three quarters of the way
    // from dark to light (blur leaves the code's own cells grey).
    let busy_below = dark + 0.75 * (ring - dark);
    let (mut light, mut busy) = (Vec::with_capacity(POINTS), 0);
    for k in 0..POINTS {
        let angle = turn * k as f32 / POINTS as f32;
        let (near, far) = (at(angle, 5.25), at(angle, 6.0));
        light.push(near > mid && far > mid);
        busy += (near.min(far) < busy_below) as usize;
    }
    let (mut run, mut best) = (0, 0);
    for k in 0..2 * POINTS {
        run = if light[k % POINTS] { run + 1 } else { 0 };
        best = best.max(run);
    }
    best >= RUN && busy >= 2
}

/// Whether each finder of a triple (top left, top right, bottom left) has
/// light quiet zone on its outer sides, a cell or two beyond its edge,
/// measured in its own cell size and so unaffected by perspective. Light
/// means nearer the finder's own light ring than its dark centre, so a
/// dim screen in a bright room still counts. Three finders taken from
/// different codes nearly always put one finder's outer side on another
/// code's data.
fn quiet_outside(pic: &Picture, [tl, tr, bl]: [Finder; 3]) -> bool {
    let unit = |p: &Finder| {
        let (dx, dy) = (p.x - tl.x, p.y - tl.y);
        let len = dx.hypot(dy).max(1.0);
        (dx / len, dy / len)
    };
    let (u, v) = (unit(&tr), unit(&bl));
    let mut light = 0;
    let mut total = 0;
    for (f, su, sv) in [(tl, -1.0, -1.0), (tr, 1.0, -1.0), (bl, -1.0, 1.0)] {
        let sample =
            |dx: f32, dy: f32, d: f32| pic.at(f.x + dx * d * f.module, f.y + dy * d * f.module);
        let dark = sample(0.0, 0.0, 0.0);
        let ring = [(u.0, u.1), (-u.0, -u.1), (v.0, v.1), (-v.0, -v.1)]
            .iter()
            .map(|&(dx, dy)| sample(dx, dy, 2.0))
            .fold(f32::MIN, f32::max);
        let mid = (dark + ring) / 2.0;
        // Straight out past each outer edge, and out past the corner (the
        // corner direction is one cell along each axis per step).
        let dirs = [
            (su * u.0, su * u.1),
            (sv * v.0, sv * v.1),
            (su * u.0 + sv * v.0, su * u.1 + sv * v.1),
        ];
        for (dx, dy) in dirs {
            for d in [4.75, 5.5] {
                light += (sample(dx, dy, d) > mid) as u32;
                total += 1;
            }
        }
    }
    light * 6 >= total * 5
}

/// `image` shrunk by `factor` on each side, each output pixel the mean of
/// a `factor` by `factor` square.
fn shrink(image: &GrayImage, factor: u32) -> GrayImage {
    let f = factor as usize;
    let (sw, w, h) = (
        image.width() as usize,
        image.width() as usize / f,
        image.height() as usize / f,
    );
    let src = image.as_raw();
    let mut sums = vec![0u32; w];
    let mut out = Vec::with_capacity(w * h);
    let area = (f * f) as u32;
    for y in 0..h {
        sums.fill(0);
        for row in src[y * f * sw..].chunks(sw).take(f) {
            for (x, sum) in sums.iter_mut().enumerate() {
                *sum += row[x * f..x * f + f].iter().map(|&p| p as u32).sum::<u32>();
            }
        }
        out.extend(sums.iter().map(|&s| ((s + area / 2) / area) as u8));
    }
    GrayImage::from_raw(w as u32, h as u32, out).unwrap_or_default()
}

/// Three finders that could be one code's, as top left, top right and
/// bottom left, with the grid sizes their spacing allows.
struct Triple {
    finders: [usize; 3],
    specs: Vec<CodeSpec>,
    /// Lower is more plausible.
    cost: f32,
}

/// Every triple whose two legs each carry a timing line on the side
/// facing the third finder, as a code's top left finder's legs do, most
/// plausible first. Pairs of finders of nearly the same size whose
/// distance apart suits a preset are walked, closest to a preset's
/// spacing first, until [`MAX_WALK`] pixels have been walked.
fn linked_triples(pic: &Picture, finders: &[Finder]) -> Vec<Triple> {
    // How far a pair's spacing, in cells, is from the nearest preset's
    // (as a log ratio). A tilted code's legs differ by up to 1.8 times,
    // so the bound for walking a pair is loose.
    let fit = |spacing: f32| {
        CodeSpec::presets()
            .iter()
            .map(|s| (spacing / (s.cells - 7) as f32).ln().abs())
            .fold(f32::MAX, f32::min)
    };
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for a in 0..finders.len() {
        for b in a + 1..finders.len() {
            let (p, q) = (&finders[a], &finders[b]);
            if p.module.max(q.module) > 1.8 * p.module.min(q.module) {
                continue;
            }
            let spacing = (p.x - q.x).hypot(p.y - q.y) / ((p.module + q.module) / 2.0);
            let off = fit(spacing);
            if off < 0.6 {
                pairs.push((off, a, b));
            }
        }
    }
    pairs.sort_by(|x, y| x.0.total_cmp(&y.0).then((x.1, x.2).cmp(&(y.1, y.2))));
    let counts_fit = |cells: usize| {
        CodeSpec::presets()
            .iter()
            .any(|s| (cells as f32 / (s.cells - 14) as f32).ln().abs() < 0.25)
    };
    // Each finder's legs: the other finder, and the side its line is on.
    let mut legs: Vec<Vec<(usize, f32)>> = vec![Vec::new(); finders.len()];
    let mut work = MAX_WALK;
    for (_, a, b) in pairs {
        for side in [1.0, -1.0] {
            if timing_line(pic, &finders[a], &finders[b], side, &mut work).is_some_and(counts_fit) {
                legs[a].push((b, side));
                legs[b].push((a, -side));
            }
        }
        if work == 0 {
            break;
        }
    }
    let mut out = Vec::new();
    for (a, mine) in legs.iter().enumerate() {
        for (i, &(b, side_b)) in mine.iter().enumerate() {
            for &(c, side_c) in &mine[i + 1..] {
                let (p, q, r) = (&finders[a], &finders[b], &finders[c]);
                let cross = (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
                if side_b * cross <= 0.0 || side_c * -cross <= 0.0 {
                    continue;
                }
                if let Some(t) = triple([a, b, c], finders).filter(|t| t.finders[0] == a) {
                    out.push(t);
                }
            }
        }
    }
    out.sort_by(|x, y| x.cost.total_cmp(&y.cost));
    out
}

/// Every plausible triple among the best finders, most plausible first.
fn triples(finders: &[Finder]) -> Vec<Triple> {
    let k = finders.len().min(MAX_CANDIDATES);
    let mut out = Vec::new();
    for a in 0..k {
        for b in a + 1..k {
            for c in b + 1..k {
                out.extend(triple([a, b, c], finders));
            }
        }
    }
    out.sort_by(|x, y| x.cost.total_cmp(&y.cost));
    out
}

/// The triple three finders make, if they could be one code's.
fn triple(idx: [usize; 3], finders: &[Finder]) -> Option<Triple> {
    let f = idx.map(|i| finders[i]);
    let modules = f.map(|f| f.module);
    let (lo, hi) = (
        modules.iter().cloned().fold(f32::MAX, f32::min),
        modules.iter().cloned().fold(f32::MIN, f32::max),
    );
    if hi > 1.8 * lo {
        return None;
    }
    let d = |p: &Finder, q: &Finder| (p.x - q.x).hypot(p.y - q.y);
    // The top left finder is the one opposite the longest side.
    let sides = [d(&f[1], &f[2]), d(&f[0], &f[2]), d(&f[0], &f[1])];
    let corner = (0..3)
        .max_by(|&i, &j| sides[i].total_cmp(&sides[j]))
        .unwrap_or(0);
    let (o, p, q) = (f[corner], f[(corner + 1) % 3], f[(corner + 2) % 3]);
    let (pi, qi) = (idx[(corner + 1) % 3], idx[(corner + 2) % 3]);
    let (u, v) = ((p.x - o.x, p.y - o.y), (q.x - o.x, q.y - o.y));
    let (lu, lv) = (u.0.hypot(u.1), v.0.hypot(v.1));
    if lu < 1.0 || lv < 1.0 || lu.max(lv) > 1.8 * lu.min(lv) {
        return None;
    }
    let cos = (u.0 * v.0 + u.1 * v.1) / (lu * lv);
    if cos.abs() > 0.5 {
        return None;
    }
    // Top right is the one a quarter turn anticlockwise of bottom left as
    // drawn (y down); a mirrored picture is caught by the orientation mark.
    let cross = u.0 * v.1 - u.1 * v.0;
    let (tr, bl) = if cross > 0.0 { (pi, qi) } else { (qi, pi) };
    let module = (modules[0] + modules[1] + modules[2]) / 3.0;
    let spacing = (lu + lv) / 2.0 / module;
    let mut specs: Vec<(f32, CodeSpec)> = CodeSpec::presets()
        .into_iter()
        .map(|spec| ((spacing / (spec.cells - 7) as f32).ln().abs(), spec))
        .filter(|&(off, _)| off < 0.35)
        .collect();
    specs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let off = specs.first()?.0;
    let shape = cos.abs() + (lu / lv).ln().abs() + (hi / lo).ln();
    let quality: f32 = f.iter().map(|f| f.rank()).sum();
    Some(Triple {
        finders: [idx[corner], tr, bl],
        specs: specs.into_iter().map(|(_, s)| s).collect(),
        cost: shape + off + quality,
    })
}

/// How well a fit's fixed cells read, by kind, each as the share right.
#[derive(Debug, Clone, Copy)]
struct FixedCells {
    /// The three finders and their separators.
    finders: f32,
    /// The two timing lines.
    timing: f32,
    /// The orientation mark, whichever way round reads better.
    mark: f32,
}

impl FixedCells {
    /// Whether this is a light code in view. Each kind of fixed cell must
    /// mostly read right on its own: QR codes share the finders and the
    /// timing lines but not the orientation mark, and a grid fitted at the
    /// wrong size gets the finders but not the timing.
    fn in_view(&self) -> bool {
        self.finders >= 0.85 && self.timing >= 0.75 && self.mark >= 0.8
    }
}

/// The layout of each preset, built once.
fn layout(spec: &CodeSpec) -> &'static Layout {
    static LAYOUTS: OnceLock<[Layout; 3]> = OnceLock::new();
    let layouts = LAYOUTS.get_or_init(|| CodeSpec::presets().map(|s| Layout::new(&s)));
    &layouts[spec.preset().unwrap_or(0) as usize]
}

/// A sampled grid: each cell's grey value and the threshold it is read
/// against.
struct Grid {
    spec: CodeSpec,
    values: Vec<f32>,
    thresholds: Vec<f32>,
}

impl Grid {
    fn new(spec: CodeSpec, values: Vec<f32>) -> Self {
        let n = spec.cells as usize;
        // Summed-area table of the values for the local means.
        let mut sum = vec![0f32; (n + 1) * (n + 1)];
        for r in 0..n {
            let mut row = 0.0;
            for c in 0..n {
                row += values[r * n + c];
                sum[(r + 1) * (n + 1) + c + 1] = sum[r * (n + 1) + c + 1] + row;
            }
        }
        let half = THRESHOLD_WINDOW / 2;
        let mut thresholds = Vec::with_capacity(n * n);
        for r in 0..n {
            for c in 0..n {
                let (r0, c0) = (r.saturating_sub(half), c.saturating_sub(half));
                let (r1, c1) = ((r + half + 1).min(n), (c + half + 1).min(n));
                let s = sum[r1 * (n + 1) + c1] - sum[r0 * (n + 1) + c1] - sum[r1 * (n + 1) + c0]
                    + sum[r0 * (n + 1) + c0];
                thresholds.push(s / ((r1 - r0) * (c1 - c0)) as f32);
            }
        }
        Grid {
            spec,
            values,
            thresholds,
        }
    }

    fn index(&self, row: u16, col: u16) -> usize {
        row as usize * self.spec.cells as usize + col as usize
    }

    fn dark(&self, row: u16, col: u16) -> bool {
        let i = self.index(row, col);
        self.values[i] < self.thresholds[i]
    }

    /// How far a cell is from its threshold, in grey levels.
    fn margin(&self, row: u16, col: u16) -> f32 {
        let i = self.index(row, col);
        (self.values[i] - self.thresholds[i]).abs()
    }

    /// How well the finder, timing and orientation mark cells read.
    fn fixed_cells(&self) -> FixedCells {
        let layout = layout(&self.spec);
        let n = self.spec.cells;
        let o = n - 5;
        let share = |(right, total): (u32, u32)| right as f32 / total.max(1) as f32;
        let (mut finders, mut timing) = ((0, 0), (0, 0));
        for row in 0..n {
            for col in 0..n {
                if row >= o && col >= o {
                    continue;
                }
                if let Cell::Fixed(dark) = layout.cell(row, col) {
                    let tally = if row < 8 && (col < 8 || col >= n - 8) || row >= n - 8 && col < 8 {
                        &mut finders
                    } else {
                        &mut timing
                    };
                    tally.0 += (self.dark(row, col) == dark) as u32;
                    tally.1 += 1;
                }
            }
        }
        // The mark as drawn, and reflected across its diagonal.
        let (mut normal, mut mirrored) = (0, 0);
        for r in 0..5 {
            for c in 0..5 {
                let seen = self.dark(o + r, o + c);
                let drawn = |r, c| matches!(layout.cell(o + r, o + c), Cell::Fixed(true));
                normal += (seen == drawn(r, c)) as u32;
                mirrored += (seen == drawn(c, r)) as u32;
            }
        }
        FixedCells {
            finders: share(finders),
            timing: share(timing),
            mark: share((normal.max(mirrored), 25)),
        }
    }

    /// The frame this grid carries, repaired, or `None`.
    fn decode(&self) -> Option<Vec<u8>> {
        let readings: &[bool] = match orientation(&self.spec, |r, c| self.dark(r, c)) {
            Some(Orientation::Normal) => &[false],
            Some(Orientation::Mirrored) => &[true],
            None => &[false, true],
        };
        readings
            .iter()
            .find_map(|&transposed| self.decode_as(transposed))
    }

    /// Read the frame bytes, straight or transposed, and repair them.
    fn decode_as(&self, transposed: bool) -> Option<Vec<u8>> {
        let len = self.spec.frame_bytes();
        let mut bytes = vec![0u8; len];
        let mut doubt = vec![f32::MAX; len];
        for (i, &(row, col)) in layout(&self.spec).data_positions().iter().enumerate() {
            if i / 8 >= len {
                break;
            }
            let (r, c) = if transposed { (col, row) } else { (row, col) };
            if self.dark(r, c) {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
            doubt[i / 8] = doubt[i / 8].min(self.margin(r, c));
        }
        if let Some(fixed) = repair_frame(&bytes, &[], 0) {
            return Some(fixed);
        }
        let mut ranked: Vec<usize> = (0..len).collect();
        ranked.sort_by(|&a, &b| doubt[a].total_cmp(&doubt[b]));
        let parity = self.spec.parity_bytes();
        // Erasing fewer than the parity bytes leaves Reed-Solomon some
        // slack to notice a wrong guess before the CRC has to.
        [parity / 3, 2 * parity / 3]
            .into_iter()
            .find_map(|per_word| repair_frame(&bytes, &ranked, per_word))
    }
}
