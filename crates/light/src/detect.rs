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
//! through its centre at any rotation or scale (patterns under
//! [`MIN_PATTERN`] pixels are too small to belong to a readable code).
//! Each hit is checked at full resolution by crossing the mark
//! vertically, horizontally again and along both diagonals through the
//! centre found, each crossing checked for the same ratio.
//!
//! Hits are chained down the picture into clusters (a hit joins one in
//! the scanned row above at nearly the same place and pattern size). A
//! finder gives hits over at most its own height, so a cluster taller
//! than one and a half times its pattern is vertical texture (stripes,
//! blinds) and is dropped without a check. Every other cluster is
//! checked hit by hit, from its middle row outwards, and the hits that
//! confirm are averaged into one mark, so one noisy crossing does not lose
//! a finder and its centre and size are measured several times.
//!
//! The work is bounded for any picture: each half of a crossing stops at
//! twice the length of the row pattern that set it off (a finder's
//! half-crossing is at most about 1.4 times it, even foreshortened to
//! half), and all crossings together stop at [`MAX_WORK`] pixels. A check
//! that fails costs at most four pattern lengths, and hits in a scanned
//! row are at least three quarters of a pattern apart, so failing checks
//! cost about 2.4 million pixels at most over a whole 1280 by 720
//! picture; the most any test picture has needed in all is 1.85 million
//! (short finder-ratio bars packed tight and leaning). Should the bound
//! ever be reached, it falls evenly, not on a place or a kind of mark:
//! checks go in rounds, every cluster getting its next check before any
//! gets another, so every cluster has had as many checks as any other,
//! give or take one; within a round the 64-pixel squares of the picture
//! take turns. Merging looks only at marks in nearby squares, so
//! thousands of small marks cost little.
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
//! the one that puts the code's far edges where they are. The first guess
//! allows for perspective from the finders' sizes (the far side of a
//! tilted screen looks smaller); the search runs a cell at a time over an
//! eighth of the code's width around it, then refines to an eighth of a
//! cell. Marks just outside the quiet zone can draw that wide search off
//! (its template is sparse), so the guess itself is refined too and the
//! better of the two fits kept.
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

/// Shortest row pattern looked at, in pixels: a finder of cells under
/// about 1.7 pixels cannot belong to a code that can be read.
const MIN_PATTERN: usize = 12;

/// A cluster of row hits taller than this many times its widest pattern
/// cannot be a finder.
const TALLEST: f32 = 1.5;

/// Most pixels the crossings of one picture may step over, all checks
/// together, spent a round at a time (see [`find_finders`]). Above what
/// failing checks can cost on a 1280 by 720 picture, and the 1.85
/// million the most demanding test picture needs.
const MAX_WORK: usize = 2_500_000;

/// A crossing may run to this many times the length of the row pattern
/// that triggered it; a real finder's crossings differ by perspective
/// alone.
const CROSS_REACH: usize = 2;

/// Side, in pixels, of the squares clusters take turns by.
const TILE: usize = 64;

/// A grey picture with its summed-area table and its thresholded form.
pub(crate) struct Picture<'a> {
    w: usize,
    h: usize,
    px: &'a [u8],
    /// `(w + 1) * (h + 1)` running sums, wrapping; any window under 2^32
    /// total is exact.
    sum: Vec<u32>,
    /// Half the side of the local mean window.
    half: usize,
    /// Each pixel thresholded against its local mean, `true` dark.
    dark: Vec<bool>,
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
        let mut pic = Picture {
            w,
            h,
            px,
            sum,
            half: (w.min(h) / 16).clamp(8, 64),
            dark: Vec::new(),
        };
        // Dark when 16 * pixel * area < 15 * window sum: no division.
        let mut dark = vec![false; w * h];
        let half = pic.half;
        for y in 0..h {
            let (y0, y1) = (y.saturating_sub(half), (y + half + 1).min(h));
            let (top, bottom) = (&pic.sum[y0 * stride..], &pic.sum[y1 * stride..]);
            for x in 0..w {
                let (x0, x1) = (x.saturating_sub(half), (x + half + 1).min(w));
                let total = bottom[x1]
                    .wrapping_sub(top[x1])
                    .wrapping_sub(bottom[x0])
                    .wrapping_add(top[x0]) as u64;
                let area = ((x1 - x0) * (y1 - y0)) as u64;
                dark[y * w + x] = 16 * px[y * w + x] as u64 * area < 15 * total;
            }
        }
        pic.dark = dark;
        pic
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

    /// Whether pixel `x`, `y` reads dark against its surroundings: under
    /// 15/16 of its local mean.
    pub fn dark(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.w + x]
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
    /// Mean departure of its crossings from the 1:1:3:1:1 ratio, 0 for
    /// perfect.
    pub error: f32,
}

impl Finder {
    /// How likely this is a real finder; lower is better. Close to the
    /// ratio matters most; a few rows agreeing helps, many more does not
    /// (a big mark in clutter must not outrank a small real one).
    pub fn rank(&self) -> f32 {
        self.error - 0.04 * self.hits.min(5) as f32
    }
}

/// Every finder-like mark in the picture, best first (see
/// [`Finder::rank`]).
pub(crate) fn find_finders(pic: &Picture) -> Vec<Finder> {
    let hits = row_hits(pic);
    let mut clusters = cluster(&hits);

    // A finder gives row hits over its centre's height and a little more,
    // less than the width of its pattern; taller columns of hits are
    // stripes, blinds or other vertical texture, and are dropped
    // unchecked.
    clusters.retain(|c| (c.bottom - c.top) as f32 <= TALLEST * c.widest as f32);
    let mut live = take_turns(&clusters, &hits);

    // Checks go a round at a time: in each round every cluster with hits
    // left gets one check, of its next hit from the middle out, before
    // any gets another, so if the work bound is ever reached every
    // cluster has had as many checks as any other with hits left.
    let mut work = MAX_WORK;
    let mut sums: Vec<Option<Sum>> = vec![None; clusters.len()];
    let mut round = 0;
    'rounds: while !live.is_empty() {
        for &k in &live {
            let c = &clusters[k];
            if work == 0 {
                break 'rounds;
            }
            let h = hits[c.members[middle_out(round, c.members.len())]];
            if let Some(f) = confirm(pic, h.x, h.y, CROSS_REACH * h.total, &mut work) {
                sums[k].get_or_insert_with(Sum::default).add(&f);
            }
        }
        round += 1;
        live.retain(|&k| round < clusters[k].members.len());
    }

    let mut found = Marks::new(pic.w, pic.h);
    for s in sums.iter().flatten() {
        found.merge(s.mark());
    }
    let mut found = found.into_vec();
    found.sort_by(|a, b| {
        a.rank()
            .total_cmp(&b.rank())
            .then(a.error.total_cmp(&b.error))
            .then(b.module.total_cmp(&a.module))
    });
    found
}

/// The `round`th of `len` hits, counting out from the middle: middle,
/// one above, one below, two above, and so on.
fn middle_out(round: usize, len: usize) -> usize {
    let mid = ((len - 1) / 2) as isize;
    (0..2 * len as isize)
        .map(|k| {
            if k % 2 == 0 {
                mid + k / 2
            } else {
                mid - (k + 1) / 2
            }
        })
        .filter(|&i| i >= 0 && (i as usize) < len)
        .nth(round)
        .unwrap_or(0) as usize
}

/// The order clusters take their turns in within a round: the squares of
/// a [`TILE`] grid take turns, one cluster each, and within a square the
/// clusters go best first (see [`Cluster::rank`]). A cluster that is
/// `k`th in its square goes before every cluster that is later than `k`th
/// in its own, wherever that is.
fn take_turns(clusters: &[Cluster], hits: &[Hit]) -> Vec<usize> {
    let mut by_tile: Vec<((usize, usize), usize)> = clusters
        .iter()
        .enumerate()
        .map(|(k, c)| {
            let h = hits[c.members[(c.members.len() - 1) / 2]];
            ((h.y / TILE, h.x / TILE), k)
        })
        .collect();
    by_tile.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(clusters[a.1].rank().total_cmp(&clusters[b.1].rank()))
    });
    // Each cluster's place within its tile, then deal by place.
    let mut place = vec![0usize; by_tile.len()];
    for i in 1..by_tile.len() {
        if by_tile[i].0 == by_tile[i - 1].0 {
            place[i] = place[i - 1] + 1;
        }
    }
    let mut dealt: Vec<(usize, usize)> = by_tile
        .iter()
        .zip(&place)
        .map(|(&(_, k), &p)| (p, k))
        .collect();
    dealt.sort_by_key(|&(p, _)| p);
    dealt.into_iter().map(|(_, k)| k).collect()
}

/// A running total of one cluster's confirmations.
#[derive(Debug, Clone, Copy, Default)]
struct Sum {
    n: u32,
    x: f32,
    y: f32,
    module: f32,
    error: f32,
}

impl Sum {
    fn add(&mut self, f: &Finder) {
        self.n += 1;
        self.x += f.x;
        self.y += f.y;
        self.module += f.module;
        self.error += f.error;
    }

    fn mark(&self) -> Finder {
        let n = self.n as f32;
        Finder {
            x: self.x / n,
            y: self.y / n,
            module: self.module / n,
            hits: self.n,
            error: self.error / n,
        }
    }
}

/// A place in a scanned row where five runs read 1:1:3:1:1.
#[derive(Debug, Clone, Copy)]
struct Hit {
    /// Centre of the middle run.
    x: usize,
    y: usize,
    /// Length of the five runs together.
    total: usize,
    error: f32,
}

/// Every row hit of the coarse pass, row by row, left to right within a
/// row.
fn row_hits(pic: &Picture) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut runs: Vec<(usize, usize, bool)> = Vec::with_capacity(pic.w / 2);
    for y in (ROW_STEP / 2..pic.h).step_by(ROW_STEP) {
        runs.clear();
        let row = &pic.dark[y * pic.w..(y + 1) * pic.w];
        let mut start = 0;
        for x in 1..pic.w {
            if row[x] != row[x - 1] {
                runs.push((start, x - start, row[start]));
                start = x;
            }
        }
        runs.push((start, pic.w - start, row[start]));
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
            let total = lens.iter().sum();
            if total < MIN_PATTERN {
                continue;
            }
            if let Some(error) = ratio_error(&lens) {
                hits.push(Hit {
                    x: runs[i + 2].0 + runs[i + 2].1 / 2,
                    y,
                    total,
                    error,
                });
            }
        }
    }
    hits
}

/// Row hits chained down the picture: one candidate mark.
#[derive(Debug, Clone)]
struct Cluster {
    top: usize,
    bottom: usize,
    /// Widest pattern among its hits.
    widest: usize,
    error_sum: f32,
    /// Indices of its hits, top to bottom.
    members: Vec<usize>,
}

impl Cluster {
    /// Lower is more finder-like, as for [`Finder::rank`].
    fn rank(&self) -> f32 {
        let n = self.members.len();
        self.error_sum / n as f32 - 0.04 * n.min(5) as f32
    }
}

/// Group hits into clusters: a hit joins the cluster of a hit in the
/// scanned row above at nearly the same place and size.
fn cluster(hits: &[Hit]) -> Vec<Cluster> {
    let mut id = vec![usize::MAX; hits.len()];
    let mut clusters: Vec<Cluster> = Vec::new();
    // The hits of the row above, and where the current row starts.
    let (mut above, mut row_start) = (0..0, 0);
    for i in 0..hits.len() {
        let h = hits[i];
        if i > 0 && h.y != hits[i - 1].y {
            above = if hits[i - 1].y + ROW_STEP == h.y {
                row_start..i
            } else {
                i..i
            };
            row_start = i;
        }
        let tolerance = (h.total / 4).max(2);
        // Hits in a row are in x order, so the ones near enough are found
        // by binary search rather than a scan of the whole row.
        let from = above.start + hits[above.clone()].partition_point(|a| a.x + tolerance < h.x);
        let joined = (from..above.end)
            .take_while(|&j| hits[j].x <= h.x + tolerance)
            .find(|&j| hits[j].total.abs_diff(h.total) <= tolerance);
        match joined.map(|j| id[j]) {
            Some(c) => {
                id[i] = c;
                let k = &mut clusters[c];
                k.members.push(i);
                k.bottom = h.y;
                k.widest = k.widest.max(h.total);
                k.error_sum += h.error;
            }
            None => {
                id[i] = clusters.len();
                clusters.push(Cluster {
                    top: h.y,
                    bottom: h.y,
                    widest: h.total,
                    error_sum: h.error,
                    members: vec![i],
                });
            }
        }
    }
    clusters
}

/// How far five run lengths are from dark, light, dark, light, dark in
/// the ratio 1:1:3:1:1, as a share of their total, or `None` if they are
/// too far (the tolerance allows for blur and threshold bias).
fn ratio_error(lens: &[usize; 5]) -> Option<f32> {
    let total: usize = lens.iter().sum();
    if total < 7 {
        return None;
    }
    let m = total as f32 / 7.0;
    let tol = m * 0.7;
    let mut error = 0.0;
    for (i, &len) in lens.iter().enumerate() {
        let (want, slack) = if i == 2 {
            (3.0 * m, 2.0 * tol)
        } else {
            (m, tol)
        };
        let off = (len as f32 - want).abs();
        if off >= slack {
            return None;
        }
        error += off;
    }
    Some(error / total as f32)
}

/// Cross the mark through pixel `x`, `y` along `(dx, dy)`: the total
/// length of the five runs, the centre of the middle run as an offset
/// from `x`, `y` in steps, and the ratio error. `None` if the runs do not
/// show a finder, leave the picture, or run past `max` steps altogether.
fn cross(
    pic: &Picture,
    x: usize,
    y: usize,
    dx: isize,
    dy: isize,
    max: usize,
    work: &mut usize,
) -> Option<(usize, f32, f32)> {
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
    let mut walk = |sign: isize| -> Option<([usize; 3], isize)> {
        let mut counts = [0usize; 3];
        let mut t = sign;
        let mut walked = 0;
        let mut core_end = 0;
        for (k, want_dark) in [true, false, true].into_iter().enumerate() {
            loop {
                let (px, py) = inside(t)?;
                if pic.dark(px, py) != want_dark {
                    break;
                }
                counts[k] += 1;
                walked += 1;
                if walked > max || *work == 0 {
                    return None;
                }
                *work -= 1;
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
    let error = ratio_error(&lens)?;
    Some((lens.iter().sum(), (neg_end + pos_end) as f32 / 2.0, error))
}

/// Check a row hit at full resolution: vertically, horizontally again and
/// along both diagonals through the refined centre, no crossing running
/// past `reach` pixels, each pixel stepped over taken from `work`.
fn confirm(pic: &Picture, cx: usize, y: usize, reach: usize, work: &mut usize) -> Option<Finder> {
    let (tv, dy, ev) = cross(pic, cx, y, 0, 1, reach, work)?;
    let cy = (y as f32 + dy).round() as usize;
    let (th, dx, eh) = cross(pic, cx, cy, 1, 0, reach, work)?;
    let fx = cx as f32 + dx;
    let fy = y as f32 + dy;
    let rx = fx.round() as usize;
    let (d1, _, e1) = cross(pic, rx, cy, 1, 1, reach, work)?;
    let (d2, _, e2) = cross(pic, rx, cy, 1, -1, reach, work)?;
    let (tv, th) = (tv as f32, th as f32);
    // Diagonal steps are longer than straight ones.
    let td = (d1 + d2) as f32 / 2.0 * std::f32::consts::SQRT_2;
    // Perspective and rotation stretch the crossings, but not wildly.
    let (lo, hi) = (tv.min(th).min(td), tv.max(th).max(td));
    if hi > 2.0 * lo {
        return None;
    }
    // A crossing at angle a to the mark's own axes is 1 / cos(a) too
    // long; of four crossings 45 degrees apart one is within 22.5 degrees
    // of an axis, so the shortest is within 8 percent of the truth at any
    // rotation.
    let shortest = tv
        .min(th)
        .min(d1 as f32 * std::f32::consts::SQRT_2)
        .min(d2 as f32 * std::f32::consts::SQRT_2);
    Some(Finder {
        x: fx,
        y: fy,
        module: shortest / 7.0,
        hits: 1,
        error: (ev + eh + e1 + e2) / 4.0,
    })
}

/// The marks found so far, with a grid of [`BUCKET`] pixel squares over
/// the picture so a new mark meets only the marks near it.
struct Marks {
    marks: Vec<Finder>,
    /// Indices of the marks whose centres lie in each square, row-major.
    buckets: Vec<Vec<usize>>,
    across: usize,
    down: usize,
}

/// Side of a [`Marks`] grid square, in pixels.
const BUCKET: usize = 32;

impl Marks {
    fn new(w: usize, h: usize) -> Self {
        let (across, down) = (w.div_ceil(BUCKET).max(1), h.div_ceil(BUCKET).max(1));
        Marks {
            marks: Vec::new(),
            buckets: vec![Vec::new(); across * down],
            across,
            down,
        }
    }

    fn bucket(&self, x: f32, y: f32) -> (usize, usize) {
        let col = (x.max(0.0) as usize / BUCKET).min(self.across - 1);
        let row = (y.max(0.0) as usize / BUCKET).min(self.down - 1);
        (col, row)
    }

    /// Add a mark, merging it into the earliest one it repeats (near, and
    /// of a similar size), weighted by how many hits each holds.
    fn merge(&mut self, f: Finder) {
        // Two marks merge within twice the larger cell size, so a mark
        // that could merge with `f` is at most four of its cells away,
        // and a square further (a merged mark's centre moves a little).
        let reach = 4.0 * f.module + BUCKET as f32;
        let (c0, r0) = self.bucket(f.x - reach, f.y - reach);
        let (c1, r1) = self.bucket(f.x + reach, f.y + reach);
        let mut repeat = None;
        for row in r0..=r1 {
            for col in c0..=c1 {
                for &i in &self.buckets[row * self.across + col] {
                    let g = &self.marks[i];
                    let near = (g.x - f.x).hypot(g.y - f.y) < 2.0 * g.module.max(f.module);
                    let similar = g.module < 2.0 * f.module && f.module < 2.0 * g.module;
                    if near && similar && repeat.is_none_or(|j| i < j) {
                        repeat = Some(i);
                    }
                }
            }
        }
        match repeat {
            Some(i) => {
                let g = &mut self.marks[i];
                let (n, m) = (g.hits as f32, f.hits as f32);
                let mix = |a: f32, b: f32| (a * n + b * m) / (n + m);
                g.x = mix(g.x, f.x);
                g.y = mix(g.y, f.y);
                g.module = mix(g.module, f.module);
                g.error = mix(g.error, f.error);
                g.hits += f.hits;
            }
            None => {
                let (col, row) = self.bucket(f.x, f.y);
                self.buckets[row * self.across + col].push(self.marks.len());
                self.marks.push(f);
            }
        }
    }

    fn into_vec(self) -> Vec<Finder> {
        self.marks
    }
}

/// How many cells a timing line between finders `a` and `b` has, if one
/// runs there. On a code, two finders' centres are on row (or column) 3
/// and their outer rings and the timing line between them on row 6, so
/// the line walked is the one three cells to one side of the centres'
/// (`side` 1 or -1 picks the side). It must read as a dark run (the
/// finder's own outer ring) of at most six and a half cells, then short
/// alternating runs, then another such dark run. Any run of more than
/// four cells (cells sized by the two finders, blended along the line)
/// ends the walk, so another finder, a gap or a bar on the line costs
/// only a few cells' walk. Counting runs rather than predicting where
/// each cell falls makes this indifferent to perspective. The count is
/// the number of short runs, given if seven in ten are each between 0.4
/// and 1.8 cells long. Each pixel walked is taken from `work`.
pub(crate) fn timing_line(
    pic: &Picture,
    a: &Finder,
    b: &Finder,
    side: f32,
    work: &mut usize,
) -> Option<usize> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d = dx.hypot(dy);
    if d < 1.0 {
        return None;
    }
    let (ex, ey) = (dx / d, dy / d);
    let (nx, ny) = (-ey * side, ex * side);
    let steps = d.round() as usize;
    let module = |t: usize| a.module + (b.module - a.module) * t as f32 / d;
    // (dark, length, cell size where it started) of each run so far.
    let mut runs: Vec<(bool, usize, f32)> = Vec::new();
    for t in 0..=steps {
        if *work == 0 {
            return None;
        }
        *work -= 1;
        let m = module(t);
        let off = 3.0 * m;
        let (x, y) = (
            a.x + ex * t as f32 + nx * off,
            a.y + ey * t as f32 + ny * off,
        );
        if x < 0.0 || y < 0.0 || x > (pic.w - 1) as f32 || y > (pic.h - 1) as f32 {
            return None;
        }
        let dark = pic.dark(x.round() as usize, y.round() as usize);
        match runs.last_mut() {
            Some(r) if r.0 == dark => r.1 += 1,
            _ => {
                if runs.is_empty() && !dark {
                    return None;
                }
                runs.push((dark, 1, m));
            }
        }
        let last = runs.len() - 1;
        let (_, len, at) = runs[last];
        let limit = if last == 0 { 6.5 } else { 4.0 } * at;
        // The far finder's ring may be as long as the first run; it is
        // judged once the walk is over.
        if len as f32 > limit && !(dark && t as f32 > d - 6.5 * b.module) {
            return None;
        }
    }
    let (first, rest) = runs.split_first()?;
    let (last, middle) = rest.split_last()?;
    if !last.0 || last.1 as f32 > 6.5 * b.module || first.1 as f32 > 6.5 * a.module {
        return None;
    }
    let good = middle
        .iter()
        .filter(|r| (0.4 * r.2..=1.8 * r.2).contains(&(r.1 as f32)))
        .count();
    (good * 10 >= middle.len() * 7).then_some(middle.len())
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
    /// The template for an `n`-cell grid, with every `strip`th cell of the
    /// quiet zone strips (a sparser template is cheaper to score).
    fn new(n: u16, strip: usize) -> Self {
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
        for k in (0..n as usize - 6).step_by(strip) {
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
pub(crate) fn locate(pic: &Picture, finders: [Finder; 3], n: u16) -> Option<Homography> {
    let nf = n as f64;
    let (a, b) = (3.5, nf - 3.5);
    let corner = nf - 2.5;
    let [tl, tr, bl] = finders.map(|f| (f.x as f64, f.y as f64));
    let span = b - a;
    // The first guess for the orientation mark's centre. Under
    // perspective the far side of the code looks smaller, and a finder's
    // size shows how much: a homography scales lengths near a point by
    // roughly its denominator w to the power -1.5, so each finder's size
    // against the top left's gives w at the top right and bottom left
    // (w = 1 at the top left), and w fixes where the fourth corner falls.
    let w = |f: &Finder| {
        let ratio = (finders[0].module / f.module) as f64;
        ratio.powf(1.0 / 1.5).clamp(0.6, 1.6)
    };
    let (wu, wv) = (w(&finders[1]), w(&finders[2]));
    let t = (corner - a) / span;
    let along = |k: usize| {
        let (o, u, v) = ([tl.0, tl.1][k], [tr.0, tr.1][k], [bl.0, bl.1][k]);
        o + t * (u * wu - o) + t * (v * wv - o)
    };
    let denominator = 1.0 + t * (wu - 1.0) + t * (wv - 1.0);
    let guess = (along(0) / denominator, along(1) / denominator);
    let leg = (tr.0 - tl.0)
        .hypot(tr.1 - tl.1)
        .max((bl.0 - tl.0).hypot(bl.1 - tl.1));
    let cell = leg / span;
    // Also refuses a NaN from degenerate finder positions.
    if cell.is_nan() || cell < 0.8 {
        return None;
    }

    // The wide search scores a sparser template; the refinement the full
    // one.
    let (coarse, fine) = (Template::new(n, 4), Template::new(n, 2));
    let axes = [
        ((tr.0 - tl.0) / span, (tr.1 - tl.1) / span),
        ((bl.0 - tl.0) / span, (bl.1 - tl.1) / span),
    ];
    let contrast = finder_contrast(pic, tl, axes);
    let from = [(a, a), (b, a), (a, b), (corner, corner)];
    let score_at = |q: (f64, f64), template: &Template| -> f32 {
        Homography::from_points(from, [tl, tr, bl, q])
            .map_or(f32::MIN, |h| score(pic, &h, template, contrast))
    };
    // The best point within `steps` steps of `centre`, by `template`.
    let search = |centre: (f64, f64), steps: i32, step: f64, template: &Template| {
        let mut best = (centre, score_at(centre, template));
        for i in -steps..=steps {
            for j in -steps..=steps {
                let q = (centre.0 + i as f64 * step, centre.1 + j as f64 * step);
                let s = score_at(q, template);
                if s > best.1 {
                    best = (q, s);
                }
            }
        }
        best.0
    };
    // From the perspective-aware guess, the mark is within 2 cells up to
    // 30 degrees of tilt and within 7 for 99 percent of tilts to 45.
    let reach = (0.12 * span).ceil() as i32;
    let refine = |start: (f64, f64)| {
        let mut best = search(start, 2, cell / 2.0, &fine);
        best = search(best, 2, cell / 4.0, &fine);
        search(best, 1, cell / 8.0, &fine)
    };
    // The wide search can be drawn off by marks just outside the quiet
    // zone, which the sparse template sees little of; the guess is
    // refined too and the better fit kept.
    let wide = refine(search(guess, reach, cell, &coarse));
    let near = refine(guess);
    let best = if score_at(near, &fine) > score_at(wide, &fine) {
        near
    } else {
        wide
    };
    Homography::from_points(from, [tl, tr, bl, best])
}

/// Half the grey difference between light and dark at a finder: its dark
/// centre against its light ring, two cells out along the grid's own axes
/// (one cell along each, in pixels, is `axes`); at least a few levels.
fn finder_contrast(pic: &Picture, centre: (f64, f64), axes: [(f64, f64); 2]) -> f32 {
    let (x, y) = centre;
    let dark = pic.at(x as f32, y as f32);
    let light = axes
        .iter()
        .flat_map(|&(ax, ay)| [(2.0 * ax, 2.0 * ay), (-2.0 * ax, -2.0 * ay)])
        .map(|(dx, dy)| pic.at((x + dx) as f32, (y + dy) as f32))
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
        assert_eq!(ratio_error(&[3, 3, 9, 3, 3]), Some(0.0));
        assert!(ratio_error(&[2, 4, 8, 3, 3]).is_some_and(|e| e > 0.0));
        assert_eq!(ratio_error(&[3, 3, 3, 3, 3]), None);
        assert_eq!(ratio_error(&[9, 3, 9, 3, 3]), None);
    }
}
