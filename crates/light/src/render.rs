//! Drawing a frame as a grey image, and reading a clean drawing back.
//!
//! [`render`] draws the grid described in [`crate::code`] inside its
//! light quiet zone, square, `px` pixels a side. Cells are whole pixels;
//! any pixels left over are split evenly into extra quiet zone, so the
//! grid stays centred. [`read_clean`] is the exact inverse for an image
//! straight out of [`render`]; reading a camera picture is the detector's
//! job.

use image::{GrayImage, Luma};

use crate::code::{Cell, CodeSpec, QUIET};

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
/// `px` pixels a side.
///
/// # Panics
///
/// Panics if `frame` is not exactly `spec.frame_bytes()` long, or if `px`
/// is too small (see [`geometry`]).
pub fn render(spec: &CodeSpec, frame: &[u8], px: u32) -> GrayImage {
    assert_eq!(
        frame.len(),
        spec.frame_bytes(),
        "frame is {} bytes, this spec draws {}",
        frame.len(),
        spec.frame_bytes()
    );
    let g = geometry(spec, px);
    let mut img = GrayImage::from_pixel(px, px, Luma([LIGHT]));

    let mut data_index = 0usize;
    for row in 0..spec.cells {
        for col in 0..spec.cells {
            let dark = match spec.cell(row, col) {
                Cell::Fixed(dark) => dark,
                Cell::Data => {
                    let i = data_index;
                    data_index += 1;
                    match frame.get(i / 8) {
                        Some(byte) => byte & (0x80 >> (i % 8)) != 0,
                        // Filler past the last whole byte.
                        None => i % 2 == 1,
                    }
                }
            };
            if dark {
                fill_cell(&mut img, &g, row, col);
            }
        }
    }
    img
}

fn fill_cell(img: &mut GrayImage, g: &Geometry, row: u16, col: u16) {
    let x0 = g.origin + col as u32 * g.cell_px;
    let y0 = g.origin + row as u32 * g.cell_px;
    for y in y0..y0 + g.cell_px {
        for x in x0..x0 + g.cell_px {
            img.put_pixel(x, y, Luma([DARK]));
        }
    }
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
    for (i, (row, col)) in spec.data_positions().into_iter().enumerate() {
        if i / 8 >= out.len() {
            break;
        }
        let x = g.origin + col as u32 * g.cell_px + g.cell_px / 2;
        let y = g.origin + row as u32 * g.cell_px + g.cell_px / 2;
        if img.get_pixel(x, y).0[0] < 128 {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}
