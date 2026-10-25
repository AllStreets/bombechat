//! The light code: the grid of cells one frame is drawn as, and the bytes
//! one frame carries.
//!
//! # Grid
//!
//! A code is a square of `cells` by `cells` cells, each black or white
//! (one bit per cell; dark is 1). Rows and columns are numbered from the
//! top left. When drawn, the grid sits inside a light quiet zone
//! [`QUIET`] cells wide on every side; the page puts a fixed mid-grey
//! surround outside that.
//!
//! Three corners (top left, top right, bottom left) carry a finder mark:
//! a 7 by 7 square made of a dark one-cell ring, a light one-cell ring and
//! a dark 3 by 3 centre. Each finder is cut off from the rest of the grid
//! by a one-cell light separator along its two inner edges, so every
//! finder reserves an 8 by 8 corner. Crossing a finder through its centre
//! in any direction reads dark, light, dark dark dark, light, dark in the
//! ratio 1:1:3:1:1, which a detector can find at any scale or rotation.
//!
//! The fourth corner (bottom right) carries the orientation mark: a 5 by 5
//! square whose outer ring is dark, whose inner ring is light except for
//! its top-left cell, and whose centre is dark. It has a one-cell light
//! separator along its top and left edges, so it reserves a 6 by 6
//! corner. Because one inner cell is dark, the mark looks different under
//! each quarter turn; together with the three finders it fixes both the
//! orientation and the fourth corner for perspective correction.
//!
//! Two timing lines run between the finders: row 6 from column 8 to
//! column `cells - 9`, and column 6 from row 8 to row `cells - 9`. Along
//! each, cells alternate dark and light, dark at even indices, so a
//! detector can count columns and rows across the grid.
//!
//! Every other cell is a data cell. Data cells are taken in row-major
//! order (left to right within a row, rows top to bottom), and carry the
//! frame's bytes most significant bit first. If the number of data cells
//! is not a multiple of eight, the few cells past the last whole byte are
//! filler, drawn alternately light and dark.
//!
//! # Frame bytes
//!
//! One frame is [`CodeSpec::frame_bytes`] bytes: an [`HEADER_LEN`]-byte
//! header, then [`CodeSpec::packets_per_frame`] RaptorQ encoding symbols of
//! `symbol_size` bytes each, then zero padding to the end. All multi-byte
//! fields are big endian. The header is, in order:
//!
//! - `version u8`, currently [`VERSION`]
//! - `cell_bits u8`, `cells u16`, `symbol_size u16`: the code spec, so a
//!   receiver needs no setting to read any preset
//! - `tag u16`: which envelope this frame belongs to
//! - `transfer_length u16`: the envelope length in bytes
//! - `symbol_id u32`: the RaptorQ encoding symbol id of the first symbol
//!   in the frame; any further symbols in the frame follow on with
//!   consecutive ids. Ids below the envelope's source symbol count are the
//!   envelope itself; ids above are repair symbols.
//! - `crc u32`: CRC-32 (IEEE) of every other byte of the frame, header and
//!   payload, so a misread cell anywhere rejects the whole frame.
//!
//! # Whitening
//!
//! So that every frame darkens close to half its data cells whatever the
//! envelope holds (all zeros, all ones, anything), the frame bytes are
//! XORed with a pseudo-random keystream before drawing. The header uses a
//! fixed keystream, so a receiver can always read it. The rest of the
//! frame uses a keystream keyed by the frame's tag and symbol id, so it
//! changes every frame and a receiver can rebuild it from the header.
//! Mean luminance across the drawn code therefore stays nearly constant
//! from frame to frame, which keeps the flashing view under photosensitive
//! flash thresholds.

/// Width of the light quiet zone drawn around the grid, in cells.
pub const QUIET: u32 = 2;

/// Frame format version carried in every header.
pub const VERSION: u8 = 1;

/// Length of the frame header in bytes.
pub const HEADER_LEN: usize = 18;

/// Smallest supported grid side, in cells.
pub const MIN_CELLS: u16 = 24;

/// Largest supported grid side, in cells.
pub const MAX_CELLS: u16 = 255;

/// Largest number of RaptorQ source symbols one envelope may need. Bounds
/// the decoder's work on any frame that slips past the CRC.
pub const MAX_SOURCE_SYMBOLS: u32 = 1024;

/// Side of the square each finder reserves, separator included.
const FINDER_BOX: u16 = 8;
/// Side of the finder mark itself.
const FINDER: u16 = 7;
/// Side of the square the orientation mark reserves, separator included.
const ORIENT_BOX: u16 = 6;
/// Row and column of the timing lines.
const TIMING: u16 = 6;

/// Fixed keystream seed for the header.
const HEADER_SEED: u64 = 0x6c69_6768_745f_6864; // "light_hd"
/// Mixed with tag and symbol id for the payload keystream.
const PAYLOAD_SEED: u64 = 0x6c69_6768_745f_706c; // "light_pl"

/// The shape of a light code: grid size, bits per cell and RaptorQ symbol
/// size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeSpec {
    /// Grid side in cells.
    pub cells: u16,
    /// Bits per cell. Always 1 (black or white) for now; kept so grey
    /// levels can be added later without a format change.
    pub cell_bits: u8,
    /// RaptorQ encoding symbol size in bytes.
    pub symbol_size: u16,
}

impl CodeSpec {
    /// 48 cells across, one 233-byte symbol a frame. Meant for 6 frames a
    /// second.
    pub fn slow() -> Self {
        CodeSpec {
            cells: 48,
            cell_bits: 1,
            symbol_size: 233,
        }
    }

    /// 56 cells across, one 335-byte symbol a frame. Meant for 12 frames a
    /// second.
    pub fn normal() -> Self {
        CodeSpec {
            cells: 56,
            cell_bits: 1,
            symbol_size: 335,
        }
    }

    /// 64 cells across, one 453-byte symbol a frame. Meant for 20 frames a
    /// second.
    pub fn fast() -> Self {
        CodeSpec {
            cells: 64,
            cell_bits: 1,
            symbol_size: 453,
        }
    }

    /// Whether this spec describes a code that can be drawn and carries at
    /// least one symbol per frame.
    pub fn is_valid(&self) -> bool {
        self.cell_bits == 1
            && (MIN_CELLS..=MAX_CELLS).contains(&self.cells)
            && self.symbol_size > 0
            && self.packets_per_frame() > 0
    }

    /// Number of data cells in the grid.
    pub fn data_cells(&self) -> usize {
        let n = self.cells as usize;
        if self.cells < MIN_CELLS {
            return 0;
        }
        let finders = 3 * (FINDER_BOX as usize).pow(2);
        let orient = (ORIENT_BOX as usize).pow(2);
        let timing = 2 * (n - 2 * FINDER_BOX as usize);
        n * n - finders - orient - timing
    }

    /// Number of whole bytes one frame carries.
    pub fn frame_bytes(&self) -> usize {
        self.data_cells() * self.cell_bits as usize / 8
    }

    /// Number of RaptorQ symbols one frame carries.
    pub fn packets_per_frame(&self) -> usize {
        if self.symbol_size == 0 {
            return 0;
        }
        self.frame_bytes().saturating_sub(HEADER_LEN) / self.symbol_size as usize
    }

    /// Number of RaptorQ source symbols an envelope of `len` bytes needs:
    /// the minimum number of distinct symbols a receiver must collect.
    pub fn source_symbols(&self, len: usize) -> u32 {
        if self.symbol_size == 0 {
            return 0;
        }
        len.div_ceil(self.symbol_size as usize) as u32
    }

    /// What the cell at `row`, `col` is.
    ///
    /// # Panics
    ///
    /// Panics if the grid is smaller than [`MIN_CELLS`], or if `row` or
    /// `col` is outside it.
    pub fn cell(&self, row: u16, col: u16) -> Cell {
        let n = self.cells;
        assert!(n >= MIN_CELLS, "a {n} cell grid is below {MIN_CELLS}");
        assert!(
            row < n && col < n,
            "cell ({row}, {col}) outside a {n} cell grid"
        );

        // Finders: top left, top right, bottom left.
        let corners = [(0, 0), (0, n - FINDER), (n - FINDER, 0)];
        for (r0, c0) in corners {
            // The reserved box extends one cell towards the grid's interior.
            let box_r0 = if r0 == 0 { 0 } else { r0 - 1 };
            let box_c0 = if c0 == 0 { 0 } else { c0 - 1 };
            let in_box = row >= box_r0
                && row < box_r0 + FINDER_BOX
                && col >= box_c0
                && col < box_c0 + FINDER_BOX;
            if !in_box {
                continue;
            }
            let in_mark = row >= r0 && row < r0 + FINDER && col >= c0 && col < c0 + FINDER;
            if !in_mark {
                return Cell::Fixed(false);
            }
            let (r, c) = (row - r0, col - c0);
            let ring = r.min(c).min(FINDER - 1 - r).min(FINDER - 1 - c);
            return Cell::Fixed(ring != 1);
        }

        // Orientation mark: bottom right.
        let o0 = n - ORIENT_BOX;
        if row >= o0 && col >= o0 {
            if row == o0 || col == o0 {
                return Cell::Fixed(false);
            }
            let (r, c) = (row - o0 - 1, col - o0 - 1);
            let ring = r.min(c).min(4 - r).min(4 - c);
            let dark = match ring {
                0 => true,
                1 => r == 1 && c == 1,
                _ => true,
            };
            return Cell::Fixed(dark);
        }

        // Timing lines.
        if row == TIMING && col >= FINDER_BOX && col < n - FINDER_BOX {
            return Cell::Fixed(col.is_multiple_of(2));
        }
        if col == TIMING && row >= FINDER_BOX && row < n - FINDER_BOX {
            return Cell::Fixed(row.is_multiple_of(2));
        }

        Cell::Data
    }

    /// Positions `(row, col)` of the data cells in the order they carry
    /// bits.
    pub fn data_positions(&self) -> Vec<(u16, u16)> {
        let mut out = Vec::with_capacity(self.data_cells());
        for row in 0..self.cells {
            for col in 0..self.cells {
                if self.cell(row, col) == Cell::Data {
                    out.push((row, col));
                }
            }
        }
        out
    }
}

/// What one grid cell is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Part of a finder, the orientation mark, a separator or a timing
    /// line; `true` is dark.
    Fixed(bool),
    /// Carries one frame bit.
    Data,
}

/// One frame, read back and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub spec: CodeSpec,
    pub tag: u16,
    pub transfer_length: u16,
    pub symbol_id: u32,
    /// `spec.packets_per_frame()` symbols of `spec.symbol_size` bytes each.
    pub symbols: Vec<Vec<u8>>,
}

/// Build one frame's whitened bytes.
pub(crate) fn write_frame(
    spec: &CodeSpec,
    tag: u16,
    transfer_length: u16,
    symbol_id: u32,
    symbols: &[Vec<u8>],
) -> Vec<u8> {
    let mut out = vec![0u8; spec.frame_bytes()];
    out[0] = VERSION;
    out[1] = spec.cell_bits;
    out[2..4].copy_from_slice(&spec.cells.to_be_bytes());
    out[4..6].copy_from_slice(&spec.symbol_size.to_be_bytes());
    out[6..8].copy_from_slice(&tag.to_be_bytes());
    out[8..10].copy_from_slice(&transfer_length.to_be_bytes());
    out[10..14].copy_from_slice(&symbol_id.to_be_bytes());
    let mut at = HEADER_LEN;
    for symbol in symbols {
        out[at..at + symbol.len()].copy_from_slice(symbol);
        at += symbol.len();
    }
    let crc = frame_crc(&out);
    out[14..18].copy_from_slice(&crc.to_be_bytes());

    whiten_header(&mut out);
    whiten_payload(&mut out, tag, symbol_id);
    out
}

/// Read one frame's bytes back. `None` if they are not a frame this
/// version understands or fail the CRC. Never panics.
pub(crate) fn read_frame(bytes: &[u8]) -> Option<Frame> {
    if bytes.len() < HEADER_LEN {
        return None;
    }
    let mut buf = bytes.to_vec();
    whiten_header(&mut buf);

    if buf[0] != VERSION {
        return None;
    }
    let spec = CodeSpec {
        cell_bits: buf[1],
        cells: u16::from_be_bytes([buf[2], buf[3]]),
        symbol_size: u16::from_be_bytes([buf[4], buf[5]]),
    };
    if !spec.is_valid() || spec.frame_bytes() != buf.len() {
        return None;
    }
    let tag = u16::from_be_bytes([buf[6], buf[7]]);
    let transfer_length = u16::from_be_bytes([buf[8], buf[9]]);
    let symbol_id = u32::from_be_bytes([buf[10], buf[11], buf[12], buf[13]]);
    let crc = u32::from_be_bytes([buf[14], buf[15], buf[16], buf[17]]);

    whiten_payload(&mut buf, tag, symbol_id);
    if frame_crc(&buf) != crc {
        return None;
    }

    let size = spec.symbol_size as usize;
    let symbols = (0..spec.packets_per_frame())
        .map(|i| buf[HEADER_LEN + i * size..HEADER_LEN + (i + 1) * size].to_vec())
        .collect();
    Some(Frame {
        spec,
        tag,
        transfer_length,
        symbol_id,
        symbols,
    })
}

/// CRC-32 of a plain (unwhitened) frame, skipping the CRC field itself.
fn frame_crc(plain: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&plain[..14]);
    hasher.update(&plain[HEADER_LEN..]);
    hasher.finalize()
}

fn whiten_header(buf: &mut [u8]) {
    let mut ks = Keystream::new(HEADER_SEED);
    for b in &mut buf[..HEADER_LEN] {
        *b ^= ks.next_byte();
    }
}

fn whiten_payload(buf: &mut [u8], tag: u16, symbol_id: u32) {
    let key = PAYLOAD_SEED ^ ((tag as u64) << 32) ^ symbol_id as u64;
    let mut ks = Keystream::new(key);
    for b in &mut buf[HEADER_LEN..] {
        *b ^= ks.next_byte();
    }
}

/// SplitMix64, used only to scramble bits, never for anything secret.
struct Keystream {
    state: u64,
    word: u64,
    left: u8,
}

impl Keystream {
    fn new(seed: u64) -> Self {
        Keystream {
            state: seed,
            word: 0,
            left: 0,
        }
    }

    fn next_byte(&mut self) -> u8 {
        if self.left == 0 {
            self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            self.word = z ^ (z >> 31);
            self.left = 8;
        }
        let b = self.word as u8;
        self.word >>= 8;
        self.left -= 1;
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_cell_formula_matches_the_grid() {
        for spec in [CodeSpec::slow(), CodeSpec::normal(), CodeSpec::fast()] {
            assert_eq!(spec.data_positions().len(), spec.data_cells());
            assert!(spec.is_valid());
            assert_eq!(spec.packets_per_frame(), 1);
        }
        for cells in MIN_CELLS..=MAX_CELLS {
            let spec = CodeSpec {
                cells,
                cell_bits: 1,
                symbol_size: 16,
            };
            assert_eq!(
                spec.data_positions().len(),
                spec.data_cells(),
                "cells {cells}"
            );
        }
    }

    #[test]
    fn orientation_mark_differs_under_every_quarter_turn() {
        let spec = CodeSpec::slow();
        let n = spec.cells;
        let o = n - 5;
        let mark: Vec<Vec<bool>> = (0..5)
            .map(|r| {
                (0..5)
                    .map(|c| spec.cell(o + r, o + c) == Cell::Fixed(true))
                    .collect()
            })
            .collect();
        let rotate = |m: &Vec<Vec<bool>>| -> Vec<Vec<bool>> {
            (0..5)
                .map(|r| (0..5).map(|c| m[4 - c][r]).collect())
                .collect()
        };
        let mut turned = mark.clone();
        for _ in 0..3 {
            turned = rotate(&turned);
            assert_ne!(turned, mark);
        }
    }

    #[test]
    fn frame_round_trips() {
        let spec = CodeSpec::normal();
        let sym = vec![(0..spec.symbol_size).map(|i| i as u8).collect::<Vec<u8>>()];
        let bytes = write_frame(&spec, 0xbeef, 4096, 42, &sym);
        let frame = read_frame(&bytes).unwrap();
        assert_eq!(frame.spec, spec);
        assert_eq!(frame.tag, 0xbeef);
        assert_eq!(frame.transfer_length, 4096);
        assert_eq!(frame.symbol_id, 42);
        assert_eq!(frame.symbols, sym);
    }
}
