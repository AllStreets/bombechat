//! Drawing a frame as a grey image, and reading a clean drawing back.
//!
//! [`render`] draws the grid described in [`crate::code`] inside its
//! light quiet zone, square, `px` pixels a side. Cells are whole pixels;
//! any pixels left over are split evenly into extra quiet zone, so the
//! grid stays centred. [`grid_bits`] packs the same grid for a page
//! that draws it itself, and [`render_bits`] draws such packed bits.
//! [`read_clean`] is the exact inverse for an image
//! straight out of [`render`]; reading a camera picture is the detector's
//! job.

use image::{GrayImage, Luma};

use crate::code::{CodeSpec, Layout, QUIET};

/// Luma of a dark cell.
pub const DARK: u8 = 0;
/// Luma of a light cell and of the quiet zone.
pub const LIGHT: u8 = 255;

/// Where the grid lands in an image `px` pixels a side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Side of one cell in pixels.
    pub cell_px: u32,
    /// Pixel offset of the grid's top-left corner from the image's, on
    /// both axes.
    pub origin: u32,
}

impl Geometry {
    /// The pixel at the centre of cell `row`, `col`, as `(x, y)`.
    pub fn centre(&self, row: u16, col: u16) -> (u32, u32) {
        (
            self.origin + col as u32 * self.cell_px + self.cell_px / 2,
            self.origin + row as u32 * self.cell_px + self.cell_px / 2,
        )
    }
}

/// The geometry [`render`] uses for `spec` at `px` pixels.
///
/// # Panics
///
/// Panics if `px` is too small to give each cell, quiet zone included, at
/// least one pixel.
pub fn geometry(spec: &CodeSpec, px: u32) -> Geometry {
    let span = spec.cells as u32 + 2 * QUIET;
    let cell_px = px / span;
    assert!(cell_px > 0, "{px} pixels is too small for {span} cells");
    let origin = (px - cell_px * spec.cells as u32) / 2;
    Geometry { cell_px, origin }
}

/// Draw `frame` (from [`crate::Encoder::next_frame`]) as a square image
/// `px` pixels a side, balance cells filled in.
///
/// # Panics
///
/// Panics if `spec` is not a preset, `frame` is not exactly
/// `spec.frame_bytes()` long, or `px` is too small (see [`geometry`]).
pub fn render(spec: &CodeSpec, frame: &[u8], px: u32) -> GrayImage {
    render_bits(spec, &grid_bits(spec, frame), px)
}

/// The whole grid `frame` is drawn as, packed for whoever draws it: one
/// bit a cell, dark 1, row-major from the top left, most significant bit
/// first, `cells * cells` bits rounded up to whole bytes (the spare bits
/// of the last byte are 0). Balance cells are filled in, so drawing these
/// bits is all there is to drawing the frame; [`render_bits`] does it.
///
/// # Panics
///
/// Panics if `spec` is not a preset or `frame` is not exactly
/// `spec.frame_bytes()` long.
pub fn grid_bits(spec: &CodeSpec, frame: &[u8]) -> Vec<u8> {
    assert!(spec.is_valid(), "{spec:?} is not a light code preset");
    assert_eq!(
        frame.len(),
        spec.frame_bytes(),
        "frame is {} bytes, this spec draws {}",
        frame.len(),
        spec.frame_bytes()
    );
    let grid = Layout::new(spec).grid(frame);
    let mut bits = vec![0u8; grid.len().div_ceil(8)];
    for (i, _) in grid.iter().enumerate().filter(|(_, &dark)| dark) {
        bits[i / 8] |= 0x80 >> (i % 8);
    }
    bits
}

/// Draw a grid packed by [`grid_bits`] as a square image `px` pixels a
/// side.
///
/// # Panics
///
/// Panics if `spec` is not a preset, `bits` is not exactly the
/// [`grid_bits`] length for it, or `px` is too small (see [`geometry`]).
pub fn render_bits(spec: &CodeSpec, bits: &[u8], px: u32) -> GrayImage {
    assert!(spec.is_valid(), "{spec:?} is not a light code preset");
    let n = spec.cells as usize;
    assert_eq!(
        bits.len(),
        (n * n).div_ceil(8),
        "{} bytes of grid bits, this spec draws {}",
        bits.len(),
        (n * n).div_ceil(8)
    );
    let g = geometry(spec, px);
    let mut img = GrayImage::from_pixel(px, px, Luma([LIGHT]));
    for i in (0..n * n).filter(|i| bits[i / 8] & (0x80 >> (i % 8)) != 0) {
        let x0 = g.origin + (i % n) as u32 * g.cell_px;
        let y0 = g.origin + (i / n) as u32 * g.cell_px;
        for y in y0..y0 + g.cell_px {
            for x in x0..x0 + g.cell_px {
                img.put_pixel(x, y, Luma([DARK]));
            }
        }
    }
    img
}

/// Read the frame bytes back from an image exactly as [`render`] drew it,
/// by sampling the centre pixel of every data cell.
///
/// # Panics
///
/// Panics if `img` is not square, or too small for `spec` (see
/// [`geometry`]).
pub fn read_clean(spec: &CodeSpec, img: &GrayImage) -> Vec<u8> {
    assert_eq!(img.width(), img.height(), "a rendered code is square");
    let g = geometry(spec, img.width());
    let mut out = vec![0u8; spec.frame_bytes()];
    let layout = Layout::new(spec);
    for (i, &(row, col)) in layout.data_positions().iter().enumerate() {
        if i / 8 >= out.len() {
            break;
        }
        let (x, y) = g.centre(row, col);
        if img.get_pixel(x, y).0[0] < 128 {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fountain::encode;

    #[test]
    fn grid_bits_are_the_layout_grid_packed_and_draw_back_to_the_frame() {
        for spec in CodeSpec::presets() {
            let frame = encode(&spec, &[7; 500], 3).next_frame();
            let bits = grid_bits(&spec, &frame);
            let grid = Layout::new(&spec).grid(&frame);
            assert_eq!(bits.len(), grid.len().div_ceil(8));
            for (i, dark) in grid.iter().enumerate() {
                assert_eq!(bits[i / 8] & (0x80 >> (i % 8)) != 0, *dark, "cell {i}");
            }
            let img = render_bits(&spec, &bits, 400);
            assert_eq!(read_clean(&spec, &img), frame);
        }
    }
}
