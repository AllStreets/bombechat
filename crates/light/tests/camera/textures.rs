//! Pictures that are not light codes but look like parts of one: finder
//! ratio stripes, blinds, checkerboards, QR codes and keyboards. They test
//! that the reader stays fast and never reads a frame where there is
//! none.

use image::{GrayImage, Luma};

use super::Noise;

/// Vertical stripes repeating dark, light, dark x3, light, dark (the
/// finder ratio) every `7 * unit` pixels, so every row is full of finder
/// hits.
pub fn finder_stripes(w: u32, h: u32, unit: u32) -> GrayImage {
    GrayImage::from_fn(w, h, |x, _| {
        let k = (x / unit) % 7;
        Luma([if k == 1 || k == 5 { 255 } else { 0 }])
    })
}

/// Slanted venetian blinds in the finder ratio, low contrast.
pub fn blinds(w: u32, h: u32) -> GrayImage {
    GrayImage::from_fn(w, h, |x, y| {
        let k = (x + y / 40) % 20;
        Luma([if k < 3 || (6..14).contains(&k) || k >= 17 {
            40
        } else {
            200
        }])
    })
}

/// Diagonal stripes in the finder ratio.
pub fn diagonal_stripes(w: u32, h: u32, unit: u32) -> GrayImage {
    GrayImage::from_fn(w, h, |x, y| {
        let k = ((x + y) / unit) % 7;
        Luma([if k == 1 || k == 5 { 230 } else { 20 }])
    })
}

/// A checkerboard of `square` pixel squares.
pub fn checkerboard(w: u32, h: u32, square: u32) -> GrayImage {
    GrayImage::from_fn(w, h, |x, y| {
        Luma([if (x / square + y / square).is_multiple_of(2) {
            20
        } else {
            230
        }])
    })
}

/// A laptop keyboard seen from above: rows of dark keys with light
/// legends on a lighter deck.
pub fn keyboard(w: u32, h: u32, key: u32, rng: &mut Noise) -> GrayImage {
    let mut img = GrayImage::from_pixel(w, h, Luma([150]));
    let gap = (key / 6).max(2);
    let pitch = key + gap;
    for row in 0..h / pitch {
        let shift = (row * key / 3) % pitch;
        for col in 0..w / pitch + 1 {
            let x0 = (col * pitch + shift) as i64 - key as i64 / 2;
            let y0 = (row * pitch) as i64;
            for y in y0..y0 + key as i64 {
                for x in x0..x0 + key as i64 {
                    if x >= 0 && (x as u32) < w && (y as u32) < h {
                        img.put_pixel(x as u32, y as u32, Luma([30]));
                    }
                }
            }
            // A legend: a few light strokes in the key's top left.
            let s = (key / 10).max(1) as i64;
            for _ in 0..3 {
                let (lx, ly) = (
                    x0 + s * 2 + (rng.next() % (3 * s as u64)) as i64,
                    y0 + s * 2 + (rng.next() % (3 * s as u64)) as i64,
                );
                let (lw, lh) = if rng.unit() < 0.5 {
                    (s * 3, s)
                } else {
                    (s, s * 3)
                };
                for y in ly..ly + lh {
                    for x in lx..lx + lw {
                        if x >= 0 && (x as u32) < w && y >= 0 && (y as u32) < h {
                            img.put_pixel(x as u32, y as u32, Luma([220]));
                        }
                    }
                }
            }
        }
    }
    img
}

/// A QR-style symbol `m` modules a side: finders in three corners with
/// separators, timing lines on row and column 6, an alignment pattern
/// centred at `m - 7`, and random data. The closest thing to a light code
/// a camera is likely to meet.
pub fn qr_like(m: usize, rng: &mut Noise) -> Vec<bool> {
    let mut grid: Vec<bool> = (0..m * m).map(|_| rng.next() & 1 == 1).collect();
    let mut set = |r: usize, c: usize, v: bool| grid[r * m + c] = v;
    for (r0, c0) in [(0, 0), (0, m - 7), (m - 7, 0)] {
        for r in 0..9 {
            for c in 0..9 {
                let (rr, cc) = (r0 as i64 + r - 1, c0 as i64 + c - 1);
                if rr < 0 || cc < 0 || rr >= m as i64 || cc >= m as i64 {
                    continue;
                }
                let (fr, fc) = (r - 1, c - 1);
                let inside = (0..7).contains(&fr) && (0..7).contains(&fc);
                let ring = fr.min(fc).min(6 - fr).min(6 - fc);
                set(rr as usize, cc as usize, inside && ring != 1);
            }
        }
    }
    for k in 8..m - 8 {
        set(6, k, k % 2 == 0);
        set(k, 6, k % 2 == 0);
    }
    let a = m - 7;
    for r in 0..5 {
        for c in 0..5 {
            let ring = r.min(c).min(4 - r).min(4 - c);
            set(a - 2 + r, a - 2 + c, ring != 1);
        }
    }
    grid
}

/// The textures built to beat a per-row budget: finder-ratio groups
/// whose sizes or spacing change from group to group, so no simple
/// periodicity rule catches them. Every second pair of rows differs.
pub const IRREGULAR: [&str; 5] = [
    "alt 2/4",
    "jitter",
    "period8",
    "period10",
    "interleave 2/3 p8",
];

/// One of [`IRREGULAR`], `w` by `h`.
pub fn irregular(name: &str, w: u32, h: u32) -> GrayImage {
    let finder = |u: u32, gap: u32| {
        vec![
            (true, u),
            (false, u),
            (true, 3 * u),
            (false, u),
            (true, u),
            (false, gap),
        ]
    };
    let rows: Vec<Vec<u8>> = (0..h)
        .map(|y| {
            let mut rng = Noise::new(0x9E37_79B9_7F4A_7C15 ^ ((y as u64 / 2 + 1) * 7919));
            let runs: Vec<(bool, u32)> = match name {
                // Units 2 and 4 alternating: neighbours differ in size.
                "alt 2/4" => [finder(2, 2), finder(4, 4)].concat(),
                // A random unit from 2 to 5 for each group.
                "jitter" => (0..400)
                    .flat_map(|_| {
                        let u = 2 + (rng.next() % 4) as u32;
                        finder(u, u)
                    })
                    .collect(),
                // An extra dark and light pair between groups.
                "period8" => vec![
                    (true, 2),
                    (false, 2),
                    (true, 6),
                    (false, 2),
                    (true, 2),
                    (false, 4),
                    (true, 2),
                    (false, 4),
                ],
                "period10" => vec![
                    (true, 2),
                    (false, 2),
                    (true, 6),
                    (false, 2),
                    (true, 2),
                    (false, 4),
                    (true, 4),
                    (false, 4),
                    (true, 4),
                    (false, 4),
                ],
                // Units 2 and 3 interleaved with bars between.
                "interleave 2/3 p8" => [
                    finder(2, 4),
                    vec![(true, 6), (false, 4)],
                    finder(3, 4),
                    vec![(true, 9), (false, 4)],
                ]
                .concat(),
                _ => panic!("no texture {name}"),
            };
            let mut row = Vec::with_capacity(w as usize);
            'fill: loop {
                for &(dark, len) in &runs {
                    for _ in 0..len {
                        row.push(if dark { 0 } else { 255 });
                        if row.len() as u32 >= w {
                            break 'fill;
                        }
                    }
                }
            }
            row
        })
        .collect();
    GrayImage::from_fn(w, h, |x, y| Luma([rows[y as usize][x as usize]]))
}

/// Whether pixel `x`, `y` of a finder mark of `unit` pixel cells, its top
/// left corner at 0, 0, is dark, or `None` outside the mark.
fn finder_pixel(x: u32, y: u32, unit: u32) -> Option<bool> {
    if x >= 7 * unit || y >= 7 * unit {
        return None;
    }
    let (r, c) = (y / unit, x / unit);
    Some(r.min(c).min(6 - r).min(6 - c) != 1)
}

/// Groups of vertical bars in the finder ratio, `unit` pixels per part
/// and `height` tall, repeated every `pitch_x` by `pitch_y` pixels, each
/// row moved `shear` pixels on from the last. Bars a little shorter than
/// one and a half times their width chain into clusters a finder could
/// make; with a shear they lean.
pub fn finder_bars(
    w: u32,
    h: u32,
    unit: u32,
    height: u32,
    (pitch_x, pitch_y): (u32, u32),
    shear: u32,
) -> GrayImage {
    GrayImage::from_fn(w, h, |x, y| {
        let (tx, ty) = ((x + shear * y) % pitch_x, y % pitch_y);
        let k = tx / unit;
        let dark = ty < height && tx < 7 * unit && (k == 0 || (2..5).contains(&k) || k == 6);
        Luma([if dark { 0 } else { 255 }])
    })
}

/// Real finder marks of `unit` pixel cells on a square lattice, `gap`
/// light pixels apart.
pub fn finder_lattice(w: u32, h: u32, unit: u32, gap: u32) -> GrayImage {
    let pitch = 7 * unit + gap;
    GrayImage::from_fn(w, h, |x, y| {
        let dark = finder_pixel(x % pitch, y % pitch, unit) == Some(true);
        Luma([if dark { 0 } else { 255 }])
    })
}

/// A finder lattice with a dark square two cells beyond each mark's
/// bottom right corner, so every mark has light on most sides and
/// something dark on one, as a code's corner does.
pub fn dotted_lattice(w: u32, h: u32, unit: u32, gap: u32) -> GrayImage {
    let pitch = 7 * unit + gap;
    GrayImage::from_fn(w, h, |x, y| {
        let (tx, ty) = (x % pitch, y % pitch);
        let dot = 8 * unit..10 * unit;
        let dark = finder_pixel(tx, ty, unit).unwrap_or(dot.contains(&tx) && dot.contains(&ty));
        Luma([if dark { 0 } else { 255 }])
    })
}

/// `count` finder marks of `unit_lo` to `unit_hi` pixel cells dropped at
/// random on a light ground, overlapping where they fall.
pub fn scattered_finders(
    w: u32,
    h: u32,
    (unit_lo, unit_hi): (u32, u32),
    count: usize,
    rng: &mut Noise,
) -> GrayImage {
    let mut img = GrayImage::from_pixel(w, h, Luma([255]));
    for _ in 0..count {
        let unit = unit_lo + (rng.next() % (unit_hi - unit_lo + 1) as u64) as u32;
        let x0 = (rng.next() % (w - 7 * unit) as u64) as u32;
        let y0 = (rng.next() % (h - 7 * unit) as u64) as u32;
        for y in 0..7 * unit {
            for x in 0..7 * unit {
                if finder_pixel(x, y, unit) == Some(true) {
                    img.put_pixel(x0 + x, y0 + y, Luma([0]));
                }
            }
        }
    }
    img
}

/// Crosses of finder-ratio bars, one across and one down, `unit` pixels
/// per part: the ratio along rows and columns but not the diagonals.
pub fn finder_crosses(w: u32, h: u32, unit: u32) -> GrayImage {
    let pitch = 8 * unit;
    GrayImage::from_fn(w, h, |x, y| {
        let (tx, ty) = (x % pitch, y % pitch);
        let across =
            (2 * unit..5 * unit).contains(&ty) && finder_pixel(tx, 3 * unit, unit) == Some(true);
        let down =
            (2 * unit..5 * unit).contains(&tx) && finder_pixel(3 * unit, ty, unit) == Some(true);
        Luma([if across || down { 0 } else { 255 }])
    })
}

/// A box blur of radius `r`, as a camera that is slightly out of focus.
pub fn box_blur(img: &GrayImage, r: i64) -> GrayImage {
    let (w, h) = (img.width() as i64, img.height() as i64);
    GrayImage::from_fn(w as u32, h as u32, |x, y| {
        let (mut sum, mut n) = (0u32, 0u32);
        for dy in -r..=r {
            for dx in -r..=r {
                let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                if (0..w).contains(&xx) && (0..h).contains(&yy) {
                    sum += img.get_pixel(xx as u32, yy as u32)[0] as u32;
                    n += 1;
                }
            }
        }
        Luma([(sum / n) as u8])
    })
}

/// Where the marks of a field of fake code corners sit: one per square of
/// `pitch` cells, each moved by up to `jitter` cells, as (left, top,
/// which quarter its data is in), with the number of squares across.
fn corner_places(
    w: u32,
    h: u32,
    unit: u32,
    pitch: u32,
    jitter: u32,
    seed: u64,
) -> (u32, Vec<(u32, u32, u8)>) {
    let p = pitch * unit;
    let across = w / p + 2;
    let mut rng = Noise::new(seed);
    let spread = (jitter * unit + 1) as u64;
    let places = (0..across * (h / p + 2))
        .map(|k| {
            let (i, j) = (k % across, k / across);
            let dx = (rng.next() % spread) as u32;
            let dy = (rng.next() % spread) as u32;
            (i * p + dx, j * p + dy, (rng.next() % 4) as u8)
        })
        .collect();
    (across, places)
}

/// Whether a pixel at `(lx, ly)` from the top left of a fake code corner
/// of `unit` pixel cells is dark: a finder mark with a quarter ring of
/// "data" five to six and two thirds cells from its centre, in quarter
/// `quarter`. It has a code corner's surroundings, light round most of it
/// and busy on one side. `None` outside the ring's reach.
fn fake_corner_pixel(lx: f64, ly: f64, unit: u32, quarter: u8) -> Option<bool> {
    let u = unit as f64;
    let (dx, dy) = (lx - 3.5 * u, ly - 3.5 * u);
    let r = dx.hypot(dy) / u;
    if r >= 6.8 {
        return None;
    }
    if (0.0..7.0 * u).contains(&lx) && (0.0..7.0 * u).contains(&ly) {
        return finder_pixel(lx as u32, ly as u32, unit);
    }
    let (sx, sy) = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)][quarter as usize % 4];
    Some((5.0..6.7).contains(&r) && dx * sx >= 0.0 && dy * sy >= 0.0)
}

/// Fake code corners (see [`fake_corner_pixel`]) of `unit` pixel cells on
/// a light ground, one every `pitch` cells each way, each moved by up to
/// `jitter` cells. On a pitch near a preset's finder spacing, many pairs
/// of them are as far apart as a code's finders.
pub fn fake_corners(w: u32, h: u32, unit: u32, pitch: u32, jitter: u32, seed: u64) -> GrayImage {
    corner_field(w, h, unit, pitch, jitter, seed, false)
}

/// [`fake_corners`] on a checkerboard of one-cell squares instead of a
/// light ground, so a timing-line walk between any two of them reads
/// timing all the way: every pair at a preset's spacing becomes a linked
/// pair, the most walking a picture can ask for.
pub fn fake_corners_on_checks(
    w: u32,
    h: u32,
    unit: u32,
    pitch: u32,
    jitter: u32,
    seed: u64,
) -> GrayImage {
    corner_field(w, h, unit, pitch, jitter, seed, true)
}

fn corner_field(
    w: u32,
    h: u32,
    unit: u32,
    pitch: u32,
    jitter: u32,
    seed: u64,
    checks: bool,
) -> GrayImage {
    let (across, places) = corner_places(w, h, unit, pitch, jitter, seed);
    let p = (pitch * unit) as i64;
    GrayImage::from_fn(w, h, |x, y| {
        let (i, j) = (x as i64 / p, y as i64 / p);
        // A mark's ring can reach into the tiles beside its own.
        for (di, dj) in (-1..=1).flat_map(|dj| (-1..=1).map(move |di| (di, dj))) {
            let (ii, jj) = (i + di, j + dj);
            if ii < 0 || jj < 0 || ii >= across as i64 {
                continue;
            }
            let Some(&(ox, oy, quarter)) = places.get((jj * across as i64 + ii) as usize) else {
                continue;
            };
            let (lx, ly) = (x as f64 + 0.5 - ox as f64, y as f64 + 0.5 - oy as f64);
            if let Some(dark) = fake_corner_pixel(lx, ly, unit, quarter) {
                return Luma([if dark { 0 } else { 255 }]);
            }
        }
        let dark = checks && (x / unit + y / unit).is_multiple_of(2);
        Luma([if dark { 0 } else { 255 }])
    })
}

/// Up to `count` printed QR-style codes (see [`qr_like`]) of `m` cells,
/// `unit` pixels each, with four-cell quiet zones, packed from the top
/// left of a light picture; and how many fitted.
pub fn qr_wall(
    w: u32,
    h: u32,
    unit: u32,
    m: usize,
    count: usize,
    rng: &mut Noise,
) -> (GrayImage, usize) {
    let mut img = GrayImage::from_pixel(w, h, Luma([255]));
    let side = (m as u32 + 8) * unit;
    let mut placed = 0;
    for by in 0..h / side {
        for bx in 0..w / side {
            if placed == count {
                return (img, placed);
            }
            let grid = qr_like(m, rng);
            for (k, _) in grid.iter().enumerate().filter(|(_, &dark)| dark) {
                let (r, c) = ((k / m) as u32, (k % m) as u32);
                for yy in 0..unit {
                    for xx in 0..unit {
                        let x = bx * side + (c + 4) * unit + xx;
                        let y = by * side + (r + 4) * unit + yy;
                        img.put_pixel(x, y, Luma([0]));
                    }
                }
            }
            placed += 1;
        }
    }
    (img, placed)
}
