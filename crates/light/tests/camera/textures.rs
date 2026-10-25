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
