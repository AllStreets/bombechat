//! Reading light codes out of a camera picture.
//!
//! [`read`] finds every code in view and returns the frame bytes of each
//! it can read, ready for [`crate::Collector::push`]. It needs no spec:
//! it tries each preset grid size the finder spacing allows, and the
//! frame's own Reed-Solomon parity and CRC decide which reading is right.
//!
//! For each plausible triple of finder marks and each grid size that
//! fits their spacing, it solves the perspective, samples every cell, and
//! thresholds each cell against the mean of the cells around it. That local mean sits halfway between light and dark
//! because every 8 by 8 block of a drawn code is 40 to 60 percent dark,
//! so blur, glare and dim light move the threshold with the cells.
//!
//! The orientation mark says whether the picture is mirrored (front
//! cameras often mirror their feed); a mirrored picture is read
//! transposed. Cells closest to the threshold mark their bytes as
//! doubtful, and if the frame does not repair as read, it is retried with
//! the most doubtful bytes treated as Reed-Solomon erasures.
//!
//! A triple whose fixed cells (finders, timing lines, orientation mark)
//! mostly match is a code in view: if no reading of it passes the CRC,
//! the result is [`ReadOutcome::Unreadable`] rather than
//! [`ReadOutcome::NoCode`], so the interface can say it sees a code it
//! cannot read.

use std::sync::OnceLock;

use image::GrayImage;

use crate::code::{orientation, repair_frame, Cell, CodeSpec, Layout, Orientation};
use crate::detect::{find_finders, locate, sample_cells, Finder, Picture};

/// Most finder candidates considered when forming triples.
const MAX_CANDIDATES: usize = 12;

/// Most grid fits tried in one picture.
const MAX_ATTEMPTS: usize = 12;

/// Share of fixed cells that must read right for a fit to count as a code
/// in view.
const FIXED_MATCH: f32 = 0.8;

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
/// Never panics.
pub fn read(image: &GrayImage) -> ReadOutcome {
    if image.width() < 32 || image.height() < 32 {
        return ReadOutcome::NoCode;
    }
    let pic = Picture::new(image);
    let finders = find_finders(&pic);
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut seen_code = false;
    let mut used = vec![false; finders.len()];
    let mut attempts = 0;

    for fit in fits(&finders) {
        if attempts >= MAX_ATTEMPTS {
            break;
        }
        if fit.finders.iter().any(|&i| used[i]) {
            continue;
        }
        attempts += 1;
        let [tl, tr, bl] = fit.finders.map(|i| (finders[i].x, finders[i].y));
        let Some(h) = locate(&pic, tl, tr, bl, fit.spec.cells) else {
            continue;
        };
        let values = sample_cells(&pic, &h, fit.spec.cells);
        let grid = Grid::new(fit.spec, values);
        if grid.fixed_match() < FIXED_MATCH {
            continue;
        }
        seen_code = true;
        if let Some(bytes) = grid.decode() {
            for &i in &fit.finders {
                used[i] = true;
            }
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

/// One way to fit a code to three finders: which is top left, top right
/// and bottom left, and the grid size.
struct Fit {
    finders: [usize; 3],
    spec: CodeSpec,
    /// Lower is more plausible.
    cost: f32,
}

/// Every plausible fit, most plausible first.
fn fits(finders: &[Finder]) -> Vec<Fit> {
    let k = finders.len().min(MAX_CANDIDATES);
    let mut out = Vec::new();
    for a in 0..k {
        for b in a + 1..k {
            for c in b + 1..k {
                triple_fits([a, b, c], finders, &mut out);
            }
        }
    }
    out.sort_by(|x, y| x.cost.total_cmp(&y.cost));
    out
}

/// The fits three finders allow, if they could be one code's.
fn triple_fits(idx: [usize; 3], finders: &[Finder], out: &mut Vec<Fit>) {
    let f = idx.map(|i| finders[i]);
    let modules = f.map(|f| f.module);
    let (lo, hi) = (
        modules.iter().cloned().fold(f32::MAX, f32::min),
        modules.iter().cloned().fold(f32::MIN, f32::max),
    );
    if hi > 1.8 * lo {
        return;
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
        return;
    }
    let cos = (u.0 * v.0 + u.1 * v.1) / (lu * lv);
    if cos.abs() > 0.5 {
        return;
    }
    // Top right is the one a quarter turn anticlockwise of bottom left as
    // drawn (y down); a mirrored picture is caught by the orientation mark.
    let cross = u.0 * v.1 - u.1 * v.0;
    let (tr, bl) = if cross > 0.0 { (pi, qi) } else { (qi, pi) };
    let module = (modules[0] + modules[1] + modules[2]) / 3.0;
    let spacing = (lu + lv) / 2.0 / module;
    for spec in CodeSpec::presets() {
        let want = (spec.cells - 7) as f32;
        let off = (spacing / want).ln().abs();
        if off < 0.35 {
            let shape = cos.abs() + (lu / lv).ln().abs() + (hi / lo).ln();
            out.push(Fit {
                finders: [idx[corner], tr, bl],
                spec,
                cost: shape + off,
            });
        }
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

    /// Share of the finder, timing and separator cells that read as
    /// drawn (the orientation mark either way round).
    fn fixed_match(&self) -> f32 {
        let layout = layout(&self.spec);
        let n = self.spec.cells;
        let (mut right, mut total) = (0u32, 0u32);
        for row in 0..n {
            for col in 0..n {
                if row >= n - 5 && col >= n - 5 {
                    continue;
                }
                if let Cell::Fixed(dark) = layout.cell(row, col) {
                    total += 1;
                    right += (self.dark(row, col) == dark) as u32;
                }
            }
        }
        let mark = orientation(&self.spec, |r, c| self.dark(r, c)).is_some();
        let mark_cells = 25;
        total += mark_cells;
        if mark {
            right += mark_cells;
        }
        right as f32 / total.max(1) as f32
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
