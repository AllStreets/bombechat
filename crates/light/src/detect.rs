//! Finding a light code in a camera picture: the finder marks, the
//! perspective, and the grey value of every cell.
//!
//! # Finder marks
//!
//! The picture is thresholded against its local mean (a summed-area table
//! makes any window's mean four lookups), so uneven lighting, vignetting
//! and gamma do not matter, only local contrast. The search is coarse to
//! fine: a coarse pass runs along every second row looking for five runs
//! in the ratio 1:1:3:1:1, the signature a finder shows along any line
//! through its centre at any rotation or scale; each hit is then refined
//! at full resolution by crossing the mark vertically, horizontally again
//! and diagonally through the centre found, each crossing checked for the
//! same ratio. Hits on the same mark from neighbouring rows merge, so a
//! real finder collects several and a chance pattern in clutter or in the
//! data cells usually only one.
//!
//! # Perspective
//!
//! Three finder centres fix an affine map, which is not enough for a
//! screen seen at an angle. The fourth point is the centre of the
//! orientation mark, found by search: candidate positions around the
//! affine guess are scored by how well the four-point homography they
//! give lays a template over the picture. The template is the
//! orientation mark (either way round, so a mirrored picture scores the
//! same), its separator, the quiet zone beside it and the strips of quiet
//! zone along the whole bottom and right edges, so the best candidate is
//! the one that puts the code's far edges where they are. The search runs
//! a cell at a time over a fifth of the code's width, then refines to an
//! eighth of a cell.
//!
//! # Cells
//!
//! Each cell's value is the mean of five bilinear samples around its
//! centre (the centre and four points a fifth of a cell away), which
//! averages sensor noise without reaching into the neighbouring cells.

use image::GrayImage;

use crate::code::QUIET;

/// Rows the coarse finder pass steps by.
const ROW_STEP: usize = 2;

/// Most finder candidates kept from one picture.
const MAX_FINDERS: usize = 256;

/// A grey picture with its summed-area table.
pub(crate) struct Picture<'a> {
    w: usize,
    h: usize,
    px: &'a [u8],
    /// `(w + 1) * (h + 1)` running sums, wrapping; any window under 2^32
    /// total is exact.
    sum: Vec<u32>,
    /// Half the side of the local mean window.
    half: usize,
}

impl<'a> Picture<'a> {
    pub fn new(img: &'a GrayImage) -> Self {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let px: &[u8] = img.as_raw();
        let stride = w + 1;
        let mut sum = vec![0u32; stride * (h + 1)];
        for y in 0..h {
            let mut row = 0u32;
            let (above, here) = sum.split_at_mut((y + 1) * stride);
            let above = &above[y * stride..];
            for x in 0..w {
                row = row.wrapping_add(px[y * w + x] as u32);
                here[x + 1] = above[x + 1].wrapping_add(row);
            }
        }
        Picture {
            w,
            h,
            px,
            sum,
            half: (w.min(h) / 16).clamp(8, 64),
        }
    }

    /// Mean of the window `x0..x1`, `y0..y1`, clamped to the picture.
    fn mean(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> u32 {
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        let area = ((x1 - x0) * (y1 - y0)).max(1) as u32;
        let s = self.w + 1;
        let total = self.sum[y1 * s + x1]
            .wrapping_sub(self.sum[y0 * s + x1])
            .wrapping_sub(self.sum[y1 * s + x0])
            .wrapping_add(self.sum[y0 * s + x0]);
        total / area
    }

    /// Mean of the local window around pixel `x`, `y`.
    pub fn local_mean(&self, x: usize, y: usize) -> u32 {
        self.mean(
            x.saturating_sub(self.half),
            y.saturating_sub(self.half),
            x + self.half + 1,
            y + self.half + 1,
        )
    }

    /// Whether pixel `x`, `y` reads dark against its surroundings.
    fn dark(&self, x: usize, y: usize) -> bool {
        let m = self.local_mean(x, y);
        (self.px[y * self.w + x] as u32) * 16 < m * 15
    }

    /// Bilinear grey value at `x`, `y` (pixel centres at whole numbers),
    /// clamped to the picture's edge.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let p = |x: usize, y: usize| self.px[y * self.w + x] as f32;
        let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
        let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
        top + (bottom - top) * fy
    }

    /// Local mean around a sub-pixel position.
    pub fn mean_at(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.w - 1) as f32) as usize;
        let y = y.clamp(0.0, (self.h - 1) as f32) as usize;
        self.local_mean(x, y) as f32
    }
}

/// A finder mark found in the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Finder {
    pub x: f32,
    pub y: f32,
    /// One cell's width in pixels.
    pub module: f32,
    /// Rows of the coarse pass that hit this mark.
    pub hits: u32,
}

/// Every finder-like mark in the picture, most hits first.
pub(crate) fn find_finders(pic: &Picture) -> Vec<Finder> {
    let mut found: Vec<Finder> = Vec::new();
    let mut runs: Vec<(usize, usize, bool)> = Vec::with_capacity(pic.w / 2);
    for y in (ROW_STEP / 2..pic.h).step_by(ROW_STEP) {
        runs.clear();
        let mut start = 0;
        let mut colour = pic.dark(0, y);
        for x in 1..pic.w {
            let d = pic.dark(x, y);
            if d != colour {
                runs.push((start, x - start, colour));
                start = x;
                colour = d;
            }
        }
        runs.push((start, pic.w - start, colour));

        for i in 0..runs.len().saturating_sub(4) {
            if !runs[i].2 {
                continue;
            }
            let lens = [
                runs[i].1,
                runs[i + 1].1,
                runs[i + 2].1,
                runs[i + 3].1,
                runs[i + 4].1,
            ];
            if !ratio_ok(&lens) {
                continue;
            }
            let cx = runs[i + 2].0 + runs[i + 2].1 / 2;
            if let Some(f) = confirm(pic, cx, y) {
                merge(&mut found, f);
                if found.len() >= MAX_FINDERS {
                    break;
                }
            }
        }
        if found.len() >= MAX_FINDERS {
            break;
        }
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.hits));
    found
}

/// Whether five run lengths read dark, light, dark, light, dark in the
/// ratio 1:1:3:1:1, loosely enough for blur and threshold bias.
fn ratio_ok(lens: &[usize; 5]) -> bool {
    let total: usize = lens.iter().sum();
    if total < 7 {
        return false;
    }
    let m = total as f32 / 7.0;
    let tol = m * 0.7;
    (0..5).all(|i| {
        let want = if i == 2 { 3.0 * m } else { m };
        let slack = if i == 2 { 2.0 * tol } else { tol };
        (lens[i] as f32 - want).abs() < slack
    })
}

/// Cross the mark through pixel `x`, `y` along `(dx, dy)`: the five run
/// lengths and the centre of the middle run, as an offset from `x`, `y` in
/// steps. `None` if the runs do not show a finder or leave the picture.
fn cross(
    pic: &Picture,
    x: usize,
    y: usize,
    dx: isize,
    dy: isize,
    max: usize,
) -> Option<([usize; 5], f32)> {
    if !pic.dark(x, y) {
        return None;
    }
    let inside = |t: isize| {
        let (px, py) = (x as isize + t * dx, y as isize + t * dy);
        (px >= 0 && py >= 0 && (px as usize) < pic.w && (py as usize) < pic.h)
            .then_some((px as usize, py as usize))
    };
    // Walks from the centre one way: [core, light, dark] lengths, and how
    // far the core reached.
    let walk = |sign: isize| -> Option<([usize; 3], isize)> {
        let mut counts = [0usize; 3];
        let mut t = sign;
        let mut core_end = 0;
        for (k, want_dark) in [true, false, true].into_iter().enumerate() {
            loop {
                let (px, py) = inside(t)?;
                if pic.dark(px, py) != want_dark {
                    break;
                }
                counts[k] += 1;
                if counts[k] > max {
                    return None;
                }
                t += sign;
            }
            if k == 0 {
                core_end = t - sign;
            }
        }
        (counts[1] > 0 && counts[2] > 0).then_some((counts, core_end))
    };
    let (neg, neg_end) = walk(-1)?;
    let (pos, pos_end) = walk(1)?;
    let lens = [neg[2], neg[1], neg[0] + pos[0] + 1, pos[1], pos[2]];
    ratio_ok(&lens).then_some((lens, (neg_end + pos_end) as f32 / 2.0))
}

/// Check a row hit at full resolution: vertically, horizontally again and
/// diagonally through the refined centre.
fn confirm(pic: &Picture, cx: usize, y: usize) -> Option<Finder> {
    let max = pic.w.max(pic.h) / 4;
    let (v_lens, dy) = cross(pic, cx, y, 0, 1, max)?;
    let cy = (y as f32 + dy).round() as usize;
    let (h_lens, dx) = cross(pic, cx, cy, 1, 0, max)?;
    let fx = cx as f32 + dx;
    let fy = y as f32 + dy;
    let rx = fx.round() as usize;
    let (d1, _) = cross(pic, rx, cy, 1, 1, max)?;
    let (d2, _) = cross(pic, rx, cy, 1, -1, max)?;
    let (tv, th) = (
        v_lens.iter().sum::<usize>() as f32,
        h_lens.iter().sum::<usize>() as f32,
    );
    let td = (d1.iter().sum::<usize>() + d2.iter().sum::<usize>()) as f32 / 2.0;
    // Perspective and rotation stretch the crossings, but not wildly.
    let (lo, hi) = (tv.min(th).min(td), tv.max(th).max(td));
    if hi > 2.0 * lo {
        return None;
    }
    Some(Finder {
        x: fx,
        y: fy,
        module: (tv + th) / 14.0,
        hits: 1,
    })
}

/// Add a hit to the candidates, merging it into one it repeats.
fn merge(found: &mut Vec<Finder>, f: Finder) {
    for g in found.iter_mut() {
        let near = (g.x - f.x).hypot(g.y - f.y) < 2.0 * g.module.max(f.module);
        let similar = g.module < 2.0 * f.module && f.module < 2.0 * g.module;
        if near && similar {
            let n = g.hits as f32;
            g.x = (g.x * n + f.x) / (n + 1.0);
            g.y = (g.y * n + f.y) / (n + 1.0);
            g.module = (g.module * n + f.module) / (n + 1.0);
            g.hits += 1;
            return;
        }
    }
    found.push(f);
}

/// A plane-to-picture perspective map.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Homography([f64; 9]);

impl Homography {
    /// The map taking each `from` point to the matching `to` point, or
    /// `None` if the points are degenerate.
    pub fn from_points(from: [(f64, f64); 4], to: [(f64, f64); 4]) -> Option<Self> {
        let mut a = [[0f64; 9]; 8];
        for i in 0..4 {
            let ((u, v), (x, y)) = (from[i], to[i]);
            a[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
            a[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -u * y, -v * y, y];
        }
        // Gaussian elimination with partial pivoting.
        for col in 0..8 {
            let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
            if a[pivot][col].abs() < 1e-9 {
                return None;
            }
            a.swap(col, pivot);
            let pivot_row = a[col];
            for (r, row) in a.iter_mut().enumerate() {
                let f = row[col] / pivot_row[col];
                if r != col && f != 0.0 {
                    for (x, p) in row[col..].iter_mut().zip(&pivot_row[col..]) {
                        *x -= f * p;
                    }
                }
            }
        }
        let mut h = [1f64; 9];
        for i in 0..8 {
            h[i] = a[i][8] / a[i][i];
        }
        h.iter().all(|v| v.is_finite()).then_some(Homography(h))
    }

    /// Where plane point `u`, `v` lands in the picture.
    pub fn map(&self, u: f64, v: f64) -> (f32, f32) {
        let h = &self.0;
        let w = h[6] * u + h[7] * v + h[8];
        (
            ((h[0] * u + h[1] * v + h[2]) / w) as f32,
            ((h[3] * u + h[4] * v + h[5]) / w) as f32,
        )
    }
}

/// One cell of the corner template: its centre in grid coordinates and
/// whether it should read dark.
#[derive(Debug, Clone, Copy)]
struct TemplateCell {
    u: f64,
    v: f64,
    dark: bool,
}

/// The template the fourth point is searched with: cells shared by both
/// readings, and the cells that differ between the normal and the
/// mirrored orientation mark.
struct Template {
    common: Vec<TemplateCell>,
    normal: Vec<TemplateCell>,
    mirrored: Vec<TemplateCell>,
}

impl Template {
    fn new(n: u16) -> Self {
        let nf = n as f64;
        let cell = |row: f64, col: f64, dark: bool| TemplateCell {
            u: col + 0.5,
            v: row + 0.5,
            dark,
        };
        let mut common = Vec::new();
        // The orientation mark corner, separator and quiet zone beside it.
        let o = nf - 6.0;
        for r in 0..(6 + QUIET as usize - 1) {
            for c in 0..(6 + QUIET as usize - 1) {
                let (row, col) = (o + r as f64, o + c as f64);
                let in_mark = (1..6).contains(&r) && (1..6).contains(&c);
                let (mr, mc) = (r as i32 - 1, c as i32 - 1);
                let ring = mr.min(mc).min(4 - mr).min(4 - mc);
                let variant = in_mark && ring == 1 && (mr == 1 || mc == 1) && (mr, mc) != (1, 1);
                if variant {
                    continue;
                }
                let dark = in_mark && ring != 1;
                common.push(cell(row, col, dark));
            }
        }
        // Quiet zone strips along the bottom and right edges.
        for k in (0..n as usize - 6).step_by(2) {
            let k = k as f64;
            common.push(cell(nf, k, false));
            common.push(cell(nf + 1.0, k, false));
            common.push(cell(k, nf, false));
            common.push(cell(k, nf + 1.0, false));
        }
        // Inner ring cells next to the mark's top and left edges: the
        // normal mark darkens two on the top edge, the mirrored one two on
        // the left edge.
        let at = |mr: f64, mc: f64, dark: bool| cell(o + 1.0 + mr, o + 1.0 + mc, dark);
        let normal = vec![
            at(1.0, 2.0, true),
            at(1.0, 3.0, true),
            at(2.0, 1.0, false),
            at(3.0, 1.0, false),
        ];
        let mirrored = vec![
            at(1.0, 2.0, false),
            at(1.0, 3.0, false),
            at(2.0, 1.0, true),
            at(3.0, 1.0, true),
        ];
        Template {
            common,
            normal,
            mirrored,
        }
    }
}

/// Where a code's grid sits in the picture: the homography from grid
/// coordinates (columns across, rows down, a cell one unit) to pixels.
pub(crate) fn locate(
    pic: &Picture,
    tl: (f32, f32),
    tr: (f32, f32),
    bl: (f32, f32),
    n: u16,
) -> Option<Homography> {
    let nf = n as f64;
    let (a, b) = (3.5, nf - 3.5);
    let corner = nf - 2.5;
    let p = |q: (f32, f32)| (q.0 as f64, q.1 as f64);
    let (tl, tr, bl) = (p(tl), p(tr), p(bl));
    let span = b - a;
    // The affine guess for the orientation mark's centre.
    let t = (corner - a) / span;
    let guess = (
        tl.0 + t * (tr.0 - tl.0) + t * (bl.0 - tl.0),
        tl.1 + t * (tr.1 - tl.1) + t * (bl.1 - tl.1),
    );
    let leg = (tr.0 - tl.0)
        .hypot(tr.1 - tl.1)
        .max((bl.0 - tl.0).hypot(bl.1 - tl.1));
    let cell = leg / span;
    // Also refuses a NaN from degenerate finder positions.
    if cell.is_nan() || cell < 0.8 {
        return None;
    }

    let template = Template::new(n);
    let contrast = finder_contrast(pic, tl, cell);
    let from = [(a, a), (b, a), (a, b), (corner, corner)];
    let score_at = |q: (f64, f64)| -> Option<f32> {
        let h = Homography::from_points(from, [tl, tr, bl, q])?;
        Some(score(pic, &h, &template, contrast))
    };

    let mut best = (guess, score_at(guess).unwrap_or(f32::MIN));
    let reach = (0.2 * span).ceil() as i32;
    let search = |centre: (f64, f64), steps: i32, step: f64, best: &mut ((f64, f64), f32)| {
        for i in -steps..=steps {
            for j in -steps..=steps {
                let q = (centre.0 + i as f64 * step, centre.1 + j as f64 * step);
                if let Some(s) = score_at(q) {
                    if s > best.1 {
                        *best = (q, s);
                    }
                }
            }
        }
    };
    search(guess, reach, cell, &mut best);
    search(best.0, 2, cell / 2.0, &mut best);
    search(best.0, 2, cell / 4.0, &mut best);
    search(best.0, 1, cell / 8.0, &mut best);
    Homography::from_points(from, [tl, tr, bl, best.0])
}

/// Half the grey difference between light and dark near a finder, from
/// its dark centre and the light ring around it; at least a few levels.
fn finder_contrast(pic: &Picture, centre: (f64, f64), cell: f64) -> f32 {
    let (x, y) = (centre.0 as f32, centre.1 as f32);
    let dark = pic.at(x, y);
    let r = (2.0 * cell) as f32;
    let light = [(r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)]
        .iter()
        .map(|&(dx, dy)| pic.at(x + dx, y + dy))
        .fold(f32::MIN, f32::max);
    ((light - dark) / 2.0).max(4.0)
}

/// How well the corner template fits the picture under `h`: each cell
/// scores +1 for a clear match down to -1 for a clear mismatch.
fn score(pic: &Picture, h: &Homography, t: &Template, contrast: f32) -> f32 {
    let one = |c: &TemplateCell| {
        let (x, y) = h.map(c.u, c.v);
        if x < 0.0 || y < 0.0 || x > (pic.w - 1) as f32 || y > (pic.h - 1) as f32 {
            return -1.0;
        }
        let s = ((pic.mean_at(x, y) - pic.at(x, y)) / contrast).clamp(-1.0, 1.0);
        if c.dark {
            s
        } else {
            -s
        }
    };
    let common: f32 = t.common.iter().map(one).sum();
    let normal: f32 = t.normal.iter().map(one).sum();
    let mirrored: f32 = t.mirrored.iter().map(one).sum();
    common + normal.max(mirrored)
}

/// The grey value of every cell of an `n`-cell grid, row-major.
pub(crate) fn sample_cells(pic: &Picture, h: &Homography, n: u16) -> Vec<f32> {
    const OFF: f64 = 0.2;
    let mut out = Vec::with_capacity(n as usize * n as usize);
    for r in 0..n {
        for c in 0..n {
            let (u, v) = (c as f64 + 0.5, r as f64 + 0.5);
            let total: f32 = [(0.0, 0.0), (-OFF, 0.0), (OFF, 0.0), (0.0, -OFF), (0.0, OFF)]
                .iter()
                .map(|&(du, dv)| {
                    let (x, y) = h.map(u + du, v + dv);
                    pic.at(x, y)
                })
                .sum();
            out.push(total / 5.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homography_maps_its_points() {
        let from = [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (10.0, 10.0)];
        let to = [(5.0, 7.0), (105.0, 12.0), (2.0, 98.0), (95.0, 110.0)];
        let h = Homography::from_points(from, to).unwrap();
        for (f, t) in from.iter().zip(to) {
            let (x, y) = h.map(f.0, f.1);
            assert!((x as f64 - t.0).abs() < 1e-3 && (y as f64 - t.1).abs() < 1e-3);
        }
    }

    #[test]
    fn degenerate_points_give_no_homography() {
        let from = [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)];
        assert!(Homography::from_points(from, from).is_none());
    }

    #[test]
    fn finder_ratio() {
        assert!(ratio_ok(&[3, 3, 9, 3, 3]));
        assert!(ratio_ok(&[2, 4, 8, 3, 3]));
        assert!(!ratio_ok(&[3, 3, 3, 3, 3]));
        assert!(!ratio_ok(&[9, 3, 9, 3, 3]));
    }
}
