//! A synthetic camera: light codes on a screen as a laptop camera sees
//! them.
//!
//! The screen is a flat plane in front of a pinhole camera with a 65
//! degree horizontal field of view. On it, each code's grid sits inside
//! its light quiet zone, inside a mid-grey surround; around the screen is
//! a cluttered room (smooth shading, boxes, lines and text-like strokes).
//! Each code is posed in 3D (distance, position, roll, and a tilt of the
//! screen about any axis in its plane), which gives a true perspective
//! homography.
//!
//! Every output pixel averages a 3 by 3 grid of sub-pixel samples of the
//! scene, which is the same as drawing at three times the resolution and
//! downsampling with a box filter. Then, in the order light meets them:
//! an illumination ramp across the frame and vignetting, lens blur
//! (Gaussian), hand shake (motion blur), screen moire (the screen's pixel
//! grid aliasing against the sensor's: a fine sinusoid whose strength
//! beats slowly across the screen, applied after the lens because it is
//! made at the sensor), slow moire banding, bloom from light over full
//! scale, shot noise in linear light, sensor clipping, the camera's
//! response curve (gamma), read noise, demosaic grain (spatially
//! correlated noise), ISP sharpening (unsharp mask, with halos), 8-bit
//! quantisation, an optional front-camera mirror, and optional JPEG
//! compression (8 by 8 DCT quantised with the standard luminance table).

#![allow(dead_code)]
// Matrix and DCT loops read clearest with plain indices.
#![allow(clippy::needless_range_loop)]

use image::{GrayImage, Luma};

pub mod textures;

/// Focal length in pixels for a 65 degree horizontal view at 1280 px.
const FOCAL_AT_1280: f64 = 1005.0;
/// Quiet zone width in cells.
const QUIET: f64 = 3.0;
/// Mid-grey surround width in cells, outside the quiet zone.
const SURROUND: f64 = 6.0;
/// Sub-pixel samples per side.
const SS: usize = 3;

/// Where a code sits in front of the camera.
#[derive(Debug, Clone)]
pub struct Pose {
    /// Height of the code and its quiet zone, as a share of frame height
    /// (before tilt).
    pub scale: f64,
    /// Centre of the code as a share of frame width and height.
    pub centre: (f64, f64),
    /// Rotation in the picture plane, degrees.
    pub roll: f64,
    /// Tilt of the screen away from facing the camera, degrees.
    pub tilt: f64,
    /// Direction of the tilt axis in the screen plane, degrees.
    pub tilt_axis: f64,
}

impl Default for Pose {
    fn default() -> Self {
        Pose {
            scale: 0.5,
            centre: (0.5, 0.5),
            roll: 0.0,
            tilt: 0.0,
            tilt_axis: 0.0,
        }
    }
}

/// One code on the screen.
#[derive(Debug, Clone)]
pub struct Code {
    /// Grid side in cells.
    pub n: usize,
    /// Row-major cells, `true` dark (from `Layout::grid`).
    pub grid: Vec<bool>,
    pub pose: Pose,
    /// A grey patch over part of the grid, as `(col0, row0, col1, row1)`
    /// in cells: a finger or a reflection hiding cells.
    pub occlude: Option<(f64, f64, f64, f64)>,
}

/// How the camera degrades what it sees.
#[derive(Debug, Clone)]
pub struct Camera {
    pub width: usize,
    pub height: usize,
    /// Front cameras often mirror their picture.
    pub mirror: bool,
    /// Lens blur, Gaussian sigma in output pixels.
    pub blur: f64,
    /// Hand shake: motion blur length in pixels, and its direction.
    pub motion: f64,
    pub motion_angle: f64,
    /// Sensor noise, standard deviation in grey levels.
    pub noise: f64,
    /// Illumination falling off across the frame: 0 none, 0.6 means the
    /// darkest side gets 40 percent of the light.
    pub gradient: f64,
    pub gradient_angle: f64,
    /// Corner darkening, 0 none.
    pub vignette: f64,
    /// Response curve exponent applied to the 0..1 signal.
    pub gamma: f64,
    /// What a dark cell and a light cell come out as, 0..1.
    pub black: f64,
    pub white: f64,
    /// Moire amplitude (share of brightness), period in pixels and
    /// direction.
    pub moire: f64,
    pub moire_period: f64,
    pub moire_angle: f64,
    /// JPEG quality, if compressed.
    pub jpeg: Option<u8>,
    /// Low-frequency moire banding: amplitude (share of brightness),
    /// period in pixels (tens to hundreds) and direction.
    pub band: f64,
    pub band_period: f64,
    pub band_angle: f64,
    /// Share of light above the sensor's full scale (`white` over 1)
    /// that bleeds into the surroundings (bloom), spread over about 2 px.
    pub bloom: f64,
    /// Shot noise: standard deviation `shot * sqrt(signal)` in linear
    /// light, as a share of full scale.
    pub shot: f64,
    /// Spatially correlated noise from demosaicing, grey levels.
    pub grain: f64,
    /// ISP unsharp-mask sharpening amount (radius about 1 px), with the
    /// halos it brings.
    pub sharpen: f64,
    /// Radial lens distortion: a point at radius `r` (in focal lengths)
    /// from the centre lands at `r * (1 + k1 * r^2)`; negative is barrel.
    pub k1: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            width: 1280,
            height: 720,
            mirror: false,
            blur: 0.7,
            motion: 0.0,
            motion_angle: 0.0,
            noise: 2.5,
            gradient: 0.1,
            gradient_angle: 0.0,
            vignette: 0.15,
            gamma: 1.0,
            black: 0.08,
            white: 0.85,
            moire: 0.0,
            moire_period: 3.0,
            moire_angle: 0.0,
            jpeg: None,
            k1: 0.0,
            band: 0.0,
            band_period: 100.0,
            band_angle: 0.0,
            bloom: 0.0,
            shot: 0.0,
            grain: 0.0,
            sharpen: 0.0,
        }
    }
}

type Mat3 = [[f64; 3]; 3];

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn invert(m: &Mat3) -> Mat3 {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (a, b) = ((j + 1) % 3, (j + 2) % 3);
            let (c, d) = ((i + 1) % 3, (i + 2) % 3);
            out[i][j] = (m[a][c] * m[b][d] - m[a][d] * m[b][c]) / det;
        }
    }
    out
}

fn apply(m: &Mat3, u: f64, v: f64) -> Option<(f64, f64)> {
    let w = m[2][0] * u + m[2][1] * v + m[2][2];
    (w > 1e-9).then(|| {
        (
            (m[0][0] * u + m[0][1] * v + m[0][2]) / w,
            (m[1][0] * u + m[1][1] * v + m[1][2]) / w,
        )
    })
}

/// The homography from a code's plane (cells, grid top left at 0, 0) to
/// output pixels.
pub fn homography(code: &Code, cam: &Camera) -> Mat3 {
    let (w, h) = (cam.width as f64, cam.height as f64);
    let f = FOCAL_AT_1280 * w / 1280.0;
    let n = code.n as f64;
    let p = &code.pose;
    let z0 = 1000.0;
    let k = p.scale * h * z0 / (f * (n + 2.0 * QUIET));
    let t = [
        (p.centre.0 * w - w / 2.0) * z0 / f,
        (p.centre.1 * h - h / 2.0) * z0 / f,
        z0,
    ];
    // Tilt about an in-plane axis (Rodrigues), then roll about the view
    // axis.
    let (ax, ay) = (
        p.tilt_axis.to_radians().cos(),
        p.tilt_axis.to_radians().sin(),
    );
    let (s, c) = (p.tilt.to_radians().sin(), p.tilt.to_radians().cos());
    let tilt: Mat3 = [
        [c + ax * ax * (1.0 - c), ax * ay * (1.0 - c), ay * s],
        [ax * ay * (1.0 - c), c + ay * ay * (1.0 - c), -ax * s],
        [-ay * s, ax * s, c],
    ];
    let (rs, rc) = (p.roll.to_radians().sin(), p.roll.to_radians().cos());
    let roll: Mat3 = [[rc, -rs, 0.0], [rs, rc, 0.0], [0.0, 0.0, 1.0]];
    let r = mul(&roll, &tilt);
    // Columns: plane u axis, plane v axis, origin.
    let half = n / 2.0 * k;
    let mut m: Mat3 = [[0.0; 3]; 3];
    for i in 0..3 {
        m[i][0] = r[i][0] * k;
        m[i][1] = r[i][1] * k;
        m[i][2] = t[i] - r[i][0] * half - r[i][1] * half;
    }
    // Pixel centres are whole numbers, so the frame spans -0.5 to w - 0.5.
    let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let kmat: Mat3 = [[f, 0.0, cx], [0.0, f, cy], [0.0, 0.0, 1.0]];
    mul(&kmat, &m)
}

/// Whether the code and its whole quiet zone land inside the frame
/// (touching the edge counts).
pub fn in_view(code: &Code, cam: &Camera) -> bool {
    let m = homography(code, cam);
    let (lo, hi) = (-QUIET, code.n as f64 + QUIET);
    [(lo, lo), (hi, lo), (lo, hi), (hi, hi)]
        .iter()
        .all(|&(u, v)| {
            apply(&m, u, v).is_some_and(|(x, y)| {
                let (w, h) = (cam.width as f64, cam.height as f64);
                let slack = 0.5 + 1e-6;
                x >= -slack && y >= -slack && x <= w - 1.0 + slack && y <= h - 1.0 + slack
            })
        })
}

/// SplitMix64, for fast seeded noise.
pub struct Noise(u64);

impl Noise {
    pub fn new(seed: u64) -> Self {
        Noise(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `lo..hi`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// Standard normal (Box-Muller).
    pub fn gauss(&mut self) -> f64 {
        let u = self.unit().max(1e-12);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// A cluttered room behind and around the screen, 0..1.
fn room(w: usize, h: usize, rng: &mut Noise) -> Vec<f32> {
    // Smooth shading from a coarse random lattice.
    let step = 96;
    let (gw, gh) = (w / step + 2, h / step + 2);
    let lattice: Vec<f64> = (0..gw * gh).map(|_| rng.range(0.15, 0.8)).collect();
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (gy, fy) = (y / step, (y % step) as f64 / step as f64);
        for x in 0..w {
            let (gx, fx) = (x / step, (x % step) as f64 / step as f64);
            let at = |i: usize, j: usize| lattice[j * gw + i];
            let top = at(gx, gy) * (1.0 - fx) + at(gx + 1, gy) * fx;
            let bottom = at(gx, gy + 1) * (1.0 - fx) + at(gx + 1, gy + 1) * fx;
            out[y * w + x] = (top * (1.0 - fy) + bottom * fy) as f32;
        }
    }
    let mut fill = |x0: f64, y0: f64, x1: f64, y1: f64, v: f32| {
        let (x0, x1) = (x0.max(0.0) as usize, (x1.min(w as f64)).max(0.0) as usize);
        let (y0, y1) = (y0.max(0.0) as usize, (y1.min(h as f64)).max(0.0) as usize);
        for y in y0..y1 {
            for x in x0..x1 {
                out[y * w + x] = v;
            }
        }
    };
    // Boxes: furniture, posters, windows.
    for _ in 0..30 {
        let (x, y) = (rng.range(0.0, w as f64), rng.range(0.0, h as f64));
        let (bw, bh) = (rng.range(8.0, 200.0), rng.range(8.0, 200.0));
        fill(x, y, x + bw, y + bh, rng.range(0.05, 0.95) as f32);
    }
    // Text-like strokes: rows of small dark marks on light patches.
    for _ in 0..6 {
        let (x, y) = (rng.range(0.0, w as f64), rng.range(0.0, h as f64));
        let size = rng.range(3.0, 9.0);
        fill(x - size, y - size, x + 40.0 * size, y + 6.0 * size, 0.85);
        for line in 0..4 {
            let mut cx = x;
            let cy = y + line as f64 * 1.4 * size;
            while cx < x + 38.0 * size {
                let gw = rng.range(0.2, 1.0) * size;
                fill(cx, cy, cx + gw, cy + size * rng.range(0.4, 1.0), 0.1);
                cx += gw + rng.range(0.3, 1.2) * size;
            }
        }
    }
    out
}

/// Take a picture of `codes` on a screen.
pub fn shoot(codes: &[Code], cam: &Camera, seed: u64) -> GrayImage {
    let (w, h) = (cam.width, cam.height);
    let mut rng = Noise::new(seed ^ 0x5eed_c0de);
    let back = room(w, h, &mut rng);
    let mut img = back.clone();
    let mut screen = vec![0f32; w * h];
    const GREY: f32 = 0.5;

    for code in codes {
        let m = homography(code, cam);
        let inv = invert(&m);
        let n = code.n as f64;
        let outer = QUIET + SURROUND;
        let (lo, hi) = (-outer, n + outer);
        let corners: Vec<(f64, f64)> = [(lo, lo), (hi, lo), (lo, hi), (hi, hi)]
            .iter()
            .filter_map(|&(u, v)| apply(&m, u, v))
            .collect();
        // Distortion moves points by up to a few percent of the frame.
        let pad = if cam.k1 == 0.0 { 1.0 } else { 0.1 * w as f64 };
        let x0 = (corners.iter().map(|c| c.0).fold(f64::MAX, f64::min) - pad).max(0.0) as usize;
        let x1 =
            (corners.iter().map(|c| c.0).fold(f64::MIN, f64::max) + pad).min(w as f64) as usize;
        let y0 = (corners.iter().map(|c| c.1).fold(f64::MAX, f64::min) - pad).max(0.0) as usize;
        let y1 =
            (corners.iter().map(|c| c.1).fold(f64::MIN, f64::max) + pad).min(h as f64) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let (mut total, mut on_screen) = (0f32, 0f32);
                for sy in 0..SS {
                    for sx in 0..SS {
                        let px = x as f64 + (sx as f64 + 0.5) / SS as f64 - 0.5;
                        let py = y as f64 + (sy as f64 + 0.5) / SS as f64 - 0.5;
                        let (px, py) = undistort(cam, px, py);
                        let Some((u, v)) = apply(&inv, px, py) else {
                            total += back[y * w + x];
                            continue;
                        };
                        let value = if u >= lo && u < hi && v >= lo && v < hi {
                            on_screen += 1.0;
                            let hidden = code.occlude.is_some_and(|(c0, r0, c1, r1)| {
                                u >= c0 && u < c1 && v >= r0 && v < r1
                            });
                            if hidden {
                                GREY
                            } else if u >= 0.0 && u < n && v >= 0.0 && v < n {
                                if code.grid[v as usize * code.n + u as usize] {
                                    0.0
                                } else {
                                    1.0
                                }
                            } else if u >= -QUIET && u < n + QUIET && v >= -QUIET && v < n + QUIET {
                                1.0
                            } else {
                                GREY
                            }
                        } else {
                            back[y * w + x]
                        };
                        total += value;
                    }
                }
                let k = (SS * SS) as f32;
                if on_screen > 0.0 {
                    img[y * w + x] = total / k;
                    screen[y * w + x] = on_screen / k;
                }
            }
        }
    }

    // Screen pixels come out between the camera's black and white levels.
    for (v, &s) in img.iter_mut().zip(&screen) {
        let shown = cam.black as f32 + (cam.white - cam.black) as f32 * *v;
        *v = *v * (1.0 - s) + shown * s;
    }

    // Illumination ramp and vignetting.
    let (ga, gs) = (
        cam.gradient_angle.to_radians().cos(),
        cam.gradient_angle.to_radians().sin(),
    );
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let r_max = cx.hypot(cy);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let along = (dx * ga + dy * gs) / r_max; // -1..1
            let light = 1.0 - cam.gradient * (0.5 - 0.5 * along);
            let r = dx.hypot(dy) / r_max;
            let vig = 1.0 - cam.vignette * r * r;
            img[y * w + x] *= (light * vig) as f32;
        }
    }

    if cam.blur > 0.0 {
        img = gaussian(&img, w, h, cam.blur);
    }
    if cam.motion > 0.0 {
        img = motion(&img, w, h, cam.motion, cam.motion_angle);
    }

    // Moire where the screen is: the screen's pixel grid aliasing against
    // the sensor's, so it lands after the lens blur.
    if cam.moire > 0.0 {
        let (ma, ms) = (
            cam.moire_angle.to_radians().cos(),
            cam.moire_angle.to_radians().sin(),
        );
        let beat = cam.moire_period * 17.3;
        for y in 0..h {
            for x in 0..w {
                let s = screen[y * w + x];
                if s == 0.0 {
                    continue;
                }
                let (fx, fy) = (x as f64, y as f64);
                let fine = (std::f64::consts::TAU * (fx * ma + fy * ms) / cam.moire_period).sin();
                let slow = (std::f64::consts::TAU * (fx * ms - fy * ma) / beat).sin();
                let m = cam.moire * fine * (0.6 + 0.4 * slow);
                img[y * w + x] *= 1.0 + (m as f32) * s;
            }
        }
    }

    // Banding moire: slow bands of brightness across the screen.
    if cam.band > 0.0 {
        let (ba, bs) = (
            cam.band_angle.to_radians().cos(),
            cam.band_angle.to_radians().sin(),
        );
        for y in 0..h {
            for x in 0..w {
                let s = screen[y * w + x];
                if s == 0.0 {
                    continue;
                }
                let t = (x as f64 * ba + y as f64 * bs) / cam.band_period;
                let m = cam.band * (std::f64::consts::TAU * t).sin();
                img[y * w + x] *= 1.0 + (m as f32) * s;
            }
        }
    }

    // Bloom: light over full scale bleeds into its surroundings.
    if cam.bloom > 0.0 {
        let excess: Vec<f32> = img.iter().map(|v| (v - 1.0).max(0.0)).collect();
        let spread = gaussian(&excess, w, h, 2.0);
        for (v, e) in img.iter_mut().zip(&spread) {
            *v += cam.bloom as f32 * e;
        }
    }

    // Shot noise in linear light, then the sensor clips and the response
    // curve applies, then read noise.
    let mut sig = vec![0f32; w * h];
    for (out, &v) in sig.iter_mut().zip(&img) {
        let mut v = v as f64;
        if cam.shot > 0.0 {
            v += cam.shot * v.max(0.0).sqrt() * rng.gauss();
        }
        let v = v.clamp(0.0, 1.0).powf(cam.gamma);
        *out = (v * 255.0 + cam.noise * rng.gauss()) as f32;
    }

    // Demosaic grain: blurred white noise, rescaled to `grain` levels.
    if cam.grain > 0.0 {
        let white: Vec<f32> = (0..w * h).map(|_| rng.gauss() as f32).collect();
        let soft = gaussian(&white, w, h, 0.8);
        // A sigma-0.8 Gaussian leaves white noise about 0.35 as strong.
        for (v, g) in sig.iter_mut().zip(&soft) {
            *v += cam.grain as f32 * g / 0.35;
        }
    }

    // ISP sharpening: unsharp mask, overshooting at edges.
    if cam.sharpen > 0.0 {
        let soft = gaussian(&sig, w, h, 1.0);
        for (v, s) in sig.iter_mut().zip(&soft) {
            *v += cam.sharpen as f32 * (*v - s);
        }
    }

    let mut out = GrayImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let sx = if cam.mirror { w - 1 - x } else { x };
            let v = sig[y * w + x].round().clamp(0.0, 255.0) as u8;
            out.put_pixel(sx as u32, y as u32, Luma([v]));
        }
    }
    if let Some(q) = cam.jpeg {
        jpeg(&mut out, q);
    }
    out
}

/// Where a pixel of the distorted picture looks in the ideal pinhole
/// picture (first-order inverse of the radial model).
fn undistort(cam: &Camera, x: f64, y: f64) -> (f64, f64) {
    if cam.k1 == 0.0 {
        return (x, y);
    }
    let f = FOCAL_AT_1280 * cam.width as f64 / 1280.0;
    let (cx, cy) = (cam.width as f64 / 2.0, cam.height as f64 / 2.0);
    let (nx, ny) = ((x - cx) / f, (y - cy) / f);
    let k = 1.0 - cam.k1 * (nx * nx + ny * ny);
    (cx + nx * k * f, cy + ny * k * f)
}

/// Separable Gaussian blur.
fn gaussian(img: &[f32], w: usize, h: usize, sigma: f64) -> Vec<f32> {
    let r = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f32> = (-r..=r)
        .map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32)
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (k, &kv) in kernel.iter().enumerate() {
                let xi = (x as isize + k as isize - r).clamp(0, w as isize - 1) as usize;
                s += img[y * w + xi] * kv;
            }
            tmp[y * w + x] = s;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (k, &kv) in kernel.iter().enumerate() {
                let yi = (y as isize + k as isize - r).clamp(0, h as isize - 1) as usize;
                s += tmp[yi * w + x] * kv;
            }
            out[y * w + x] = s;
        }
    }
    out
}

/// Motion blur: the mean along a line of `length` pixels.
fn motion(img: &[f32], w: usize, h: usize, length: f64, angle: f64) -> Vec<f32> {
    let steps = length.ceil() as usize + 1;
    let (dx, dy) = (angle.to_radians().cos(), angle.to_radians().sin());
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for i in 0..steps {
                let t = (i as f64 / (steps - 1) as f64 - 0.5) * length;
                let sx = (x as f64 + t * dx).round().clamp(0.0, w as f64 - 1.0) as usize;
                let sy = (y as f64 + t * dy).round().clamp(0.0, h as f64 - 1.0) as usize;
                s += img[sy * w + sx];
            }
            out[y * w + x] = s / steps as f32;
        }
    }
    out
}

/// Compress and decompress as baseline JPEG would: 8 by 8 DCT, quantised
/// with the standard luminance table at quality `q`.
fn jpeg(img: &mut GrayImage, q: u8) {
    const TABLE: [u16; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    let q = q.clamp(1, 100) as u32;
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let quant: Vec<f32> = TABLE
        .iter()
        .map(|&t| ((t as u32 * scale + 50) / 100).clamp(1, 255) as f32)
        .collect();
    let mut cos = [[0f32; 8]; 8];
    for (u, row) in cos.iter_mut().enumerate() {
        let a = if u == 0 { (0.125f32).sqrt() } else { 0.5 };
        for (x, c) in row.iter_mut().enumerate() {
            *c = a * (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos();
        }
    }
    let (w, h) = (img.width() as usize, img.height() as usize);
    for by in (0..h - h % 8).step_by(8) {
        for bx in (0..w - w % 8).step_by(8) {
            let mut b = [[0f32; 8]; 8];
            for y in 0..8 {
                for x in 0..8 {
                    b[y][x] = img.get_pixel((bx + x) as u32, (by + y) as u32).0[0] as f32 - 128.0;
                }
            }
            // Forward: rows then columns.
            let mut t = [[0f32; 8]; 8];
            for y in 0..8 {
                for u in 0..8 {
                    t[y][u] = (0..8).map(|x| cos[u][x] * b[y][x]).sum();
                }
            }
            let mut f = [[0f32; 8]; 8];
            for v in 0..8 {
                for u in 0..8 {
                    let c: f32 = (0..8).map(|y| cos[v][y] * t[y][u]).sum();
                    let qv = quant[v * 8 + u];
                    f[v][u] = (c / qv).round() * qv;
                }
            }
            // Inverse.
            for y in 0..8 {
                for u in 0..8 {
                    t[y][u] = (0..8).map(|v| cos[v][y] * f[v][u]).sum();
                }
            }
            for y in 0..8 {
                for x in 0..8 {
                    let p: f32 = (0..8).map(|u| cos[u][x] * t[y][u]).sum();
                    img.put_pixel(
                        (bx + x) as u32,
                        (by + y) as u32,
                        Luma([(p + 128.0).round().clamp(0.0, 255.0) as u8]),
                    );
                }
            }
        }
    }
}
