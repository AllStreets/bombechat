//! The light code: the grid of cells one frame is drawn as, and the bytes
//! one frame carries.
//!
//! # Grid
//!
//! A code is a square of `cells` by `cells` cells, each black or white
//! (one bit per cell; dark is 1). `cells` is a multiple of eight, so the
//! grid divides exactly into 8 by 8 blocks. Rows and columns are numbered
//! from the top left. When drawn, the grid sits inside a light quiet zone
//! [`QUIET`] cells wide on every side; the page puts a fixed mid-grey
//! surround outside that.
//!
//! Three corners (top left, top right, bottom left) carry a finder mark:
//! a 7 by 7 square made of a dark one-cell ring, a light one-cell ring and
//! a dark 3 by 3 centre. Each finder is cut off from the rest of the grid
//! by a one-cell light separator along its two inner edges, so every
//! finder fills exactly one 8 by 8 corner block. Crossing a finder through
//! its centre in any direction reads dark, light, dark dark dark, light,
//! dark in the ratio 1:1:3:1:1, which a detector can find at any scale or
//! rotation.
//!
//! The fourth corner (bottom right) carries the orientation mark: a 5 by 5
//! square whose outer ring is dark, whose centre is dark, and whose inner
//! ring is light except for two cells side by side on its top edge (inner
//! ring row 1, columns 2 and 3). It has a one-cell light separator along
//! its top and left edges, so it takes a 6 by 6 corner. The two dark inner
//! cells sit off the diagonal, so the mark looks different under every
//! quarter turn and also under a mirror image: a reader that lined the
//! three finders up on a mirrored picture sees the mark reflected across
//! its diagonal, and [`orientation`] reports that.
//!
//! Two timing lines run between the finders: row 6 from column 8 to
//! column `cells - 9`, and column 6 from row 8 to row `cells - 9`. Along
//! each, cells alternate dark and light, dark at even indices, so a
//! detector can count columns and rows across the grid.
//!
//! In every 8 by 8 block that has at least [`BALANCE_MIN_FREE`] cells not
//! taken by the marks or timing lines, the last [`BALANCE_CELLS`] of those
//! free cells in row-major order are balance cells. They carry no data;
//! the renderer darkens just enough of them to bring the block as close to
//! half dark as it can (see Photosensitivity below).
//!
//! Every remaining cell is a data cell. Data cells are taken in row-major
//! order (left to right within a row, rows top to bottom) and carry the
//! frame's bytes most significant bit first. The first [`FORMAT_CELLS`]
//! data cells, which fall in rows 0 and 1 between the top finders, are the
//! format field. If the number of data cells is not a multiple of eight,
//! the few cells past the last whole byte are filler, drawn alternately
//! light and dark.
//!
//! # Frame bytes
//!
//! One frame is [`CodeSpec::frame_bytes`] bytes: the 8-byte format field,
//! then the coded region.
//!
//! The format field says which whitening mask the frame uses. The preset
//! number (two bits) and mask number (four bits) form a six-bit message,
//! encoded with the first-order Reed-Muller code of length 32 (any two
//! codewords differ in at least 16 bits). Each codeword bit is written as
//! a pair of cells, dark then light for 1 and light then dark for 0, so
//! the field is always exactly half dark. A pair that reads the same
//! twice is treated as unknown, not as a bit.
//!
//! The coded region is cut into as few Reed-Solomon codewords as keep each
//! at most 255 bytes long, of near-equal length, each spending about a
//! quarter of its bytes ([`CodeSpec::parity_bytes`]) on parity. The
//! codewords are interleaved byte by byte across the region (region byte
//! `j` belongs to codeword `j mod count`), so a misread band of adjacent
//! rows spreads its damage over every codeword. The data parts of the
//! codewords, in order, hold:
//!
//! - `version u8`, currently [`VERSION`]
//! - `preset u8`: 0 slow, 1 normal, 2 fast
//! - `tag u16`: which envelope this frame belongs to
//! - `transfer_length u16`: the envelope length in bytes
//! - `symbol_id u24`: the RaptorQ encoding symbol id of the first symbol
//!   in the frame; any further symbols in the frame follow on with
//!   consecutive ids
//! - `crc u32`: CRC-32 (IEEE) of every other data byte
//! - the RaptorQ symbols, `symbol_size` bytes each, then zero padding
//!
//! All multi-byte fields are big endian. Reed-Solomon repairs scattered
//! and banded misreads; the CRC is the final check, so a frame it cannot
//! repair is rejected rather than misdelivered.
//!
//! # Photosensitivity
//!
//! The coded region is XORed with one of [`MASKS`] fixed pseudo-random
//! keystreams before drawing, chosen per frame (like a QR mask) and named
//! in the format field. The encoder only ever emits a frame in which every
//! 8 by 8 block, after balancing, is between 40 and 60 percent dark
//! ([`BLOCK_DARK_MIN`] to [`BLOCK_DARK_MAX`] of 64 cells); if no mask
//! achieves that it moves on to the next fountain symbol and tries again.
//! Mean luminance across the code therefore stays nearly constant from
//! frame to frame, block by block, whatever the envelope holds.

use std::sync::OnceLock;

use reed_solomon::{Decoder as RsDecoder, Encoder as RsEncoder};

/// Width of the light quiet zone drawn around the grid, in cells.
pub const QUIET: u32 = 3;

/// Frame format version carried in every header.
pub const VERSION: u8 = 2;

/// Bytes of header (before the CRC) in the coded data.
const HEADER_LEN: usize = 9;
/// Bytes of header including the CRC.
pub const HEADER_WITH_CRC: usize = HEADER_LEN + 4;

/// Number of data cells the format field takes.
pub const FORMAT_CELLS: usize = 64;
const FORMAT_BYTES: usize = FORMAT_CELLS / 8;

/// Number of whitening masks a frame may choose from.
pub const MASKS: u8 = 16;

/// Balance cells per block.
pub const BALANCE_CELLS: usize = 4;
/// A block gets balance cells only if it has at least this many free
/// cells.
pub const BALANCE_MIN_FREE: usize = 24;

/// Fewest dark cells an 8 by 8 block may show (40 percent of 64).
pub const BLOCK_DARK_MIN: u32 = 26;
/// Most dark cells an 8 by 8 block may show (60 percent of 64).
pub const BLOCK_DARK_MAX: u32 = 38;

/// Side of a balance block.
const BLOCK: u16 = 8;
/// Side of the square each finder reserves, separator included.
const FINDER_BOX: u16 = 8;
/// Side of the finder mark itself.
const FINDER: u16 = 7;
/// Side of the square the orientation mark reserves, separator included.
const ORIENT_BOX: u16 = 6;
/// Row and column of the timing lines.
const TIMING: u16 = 6;
/// Dark cells of the orientation mark's inner ring, as (row, col) within
/// the 5 by 5 mark.
const ORIENT_INNER_DARK: [(u16, u16); 2] = [(1, 2), (1, 3)];

/// Mixed with preset and mask for the region keystream.
const WHITEN_SEED: u64 = 0x6c69_6768_745f_776d; // "light_wm"

/// The shape of a light code: grid size, bits per cell and RaptorQ symbol
/// size. Only the three presets are valid.
///
/// # Why these presets
///
/// Grid size against the camera: a 720p laptop camera with about 65
/// degrees of horizontal view sees roughly 75 cm across at 60 cm, about
/// 1.7 px per mm. A code drawn 16 cm wide therefore covers about 270
/// camera pixels; with the quiet zone that is about 5.0 px per cell for
/// slow (54 cells across), 4.4 for normal (62) and 3.9 for fast (70).
/// About 3 px per cell is the practical floor once blur and perspective
/// are allowed for, so 64 cells is the top of the safe range.
///
/// Frame rate against the camera: each code frame should stay up for at
/// least two camera frames, so at least one camera frame sees it whole
/// rather than mid-change. Slow at 6 frames a second gives a 15 fps camera
/// 2.5 frames per code; normal at 10 gives a 30 fps camera 3; fast at 15
/// gives a 30 fps camera 2. See [`CodeSpec::frames_per_second`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeSpec {
    /// Grid side in cells.
    pub cells: u16,
    /// Bits per cell. Always 1 (black or white) for now; kept so grey
    /// levels can be added later.
    pub cell_bits: u8,
    /// RaptorQ encoding symbol size in bytes.
    pub symbol_size: u16,
}

impl CodeSpec {
    /// 48 cells across, the biggest cells, one 158-byte symbol a frame,
    /// 6 frames a second: for weak cameras, including 15 fps ones. A
    /// 4096-byte envelope takes 26 clean frames, about 4.3 seconds.
    pub fn slow() -> Self {
        CodeSpec {
            cells: 48,
            cell_bits: 1,
            symbol_size: 158,
        }
    }

    /// 56 cells across, one 229-byte symbol a frame, 10 frames a second:
    /// for a 30 fps camera. A 4096-byte envelope takes 18 clean frames,
    /// about 1.8 seconds.
    pub fn normal() -> Self {
        CodeSpec {
            cells: 56,
            cell_bits: 1,
            symbol_size: 229,
        }
    }

    /// 64 cells across, one 312-byte symbol a frame, 15 frames a second:
    /// for a good 30 fps camera close to the screen. A 4096-byte envelope
    /// takes 14 clean frames, about 0.9 seconds.
    pub fn fast() -> Self {
        CodeSpec {
            cells: 64,
            cell_bits: 1,
            symbol_size: 312,
        }
    }

    /// All presets, by preset number.
    pub fn presets() -> [CodeSpec; 3] {
        [Self::slow(), Self::normal(), Self::fast()]
    }

    /// The preset by number: 0 slow, 1 normal, 2 fast.
    pub fn from_preset(id: u8) -> Option<CodeSpec> {
        Self::presets().get(id as usize).copied()
    }

    /// This spec's preset number, or `None` if it is not a preset.
    pub fn preset(&self) -> Option<u8> {
        Self::presets()
            .iter()
            .position(|p| p == self)
            .map(|i| i as u8)
    }

    /// Whether this spec is one of the presets, the only codes drawn or
    /// read.
    pub fn is_valid(&self) -> bool {
        self.preset().is_some()
    }

    /// The frame rate this preset is meant to be shown at.
    pub fn frames_per_second(&self) -> u32 {
        match self.preset() {
            Some(0) => 6,
            Some(1) => 10,
            _ => 15,
        }
    }

    /// Number of data cells in the grid (format field included).
    pub fn data_cells(&self) -> usize {
        static PRESET_DATA_CELLS: OnceLock<[usize; 3]> = OnceLock::new();
        match self.preset() {
            Some(p) => PRESET_DATA_CELLS.get_or_init(|| {
                CodeSpec::presets().map(|s| Layout::new(&s).data_positions().len())
            })[p as usize],
            None if self.cells >= 24 && self.cells.is_multiple_of(8) => {
                Layout::new(self).data_positions().len()
            }
            None => 0,
        }
    }

    /// Number of whole bytes one frame carries, format field included.
    pub fn frame_bytes(&self) -> usize {
        self.data_cells() * self.cell_bits as usize / 8
    }

    /// Number of Reed-Solomon codewords the coded region is cut into.
    pub fn codewords(&self) -> usize {
        self.region_bytes().div_ceil(255).max(1)
    }

    /// Parity bytes in each Reed-Solomon codeword: a quarter of the
    /// shortest codeword, so each can repair up to an eighth of its bytes.
    pub fn parity_bytes(&self) -> usize {
        (self.region_bytes() / self.codewords()) / 4
    }

    /// Bytes of data (header, CRC and symbols) one frame carries after
    /// parity.
    pub fn data_bytes(&self) -> usize {
        self.region_bytes()
            .saturating_sub(self.codewords() * self.parity_bytes())
    }

    /// Number of RaptorQ symbols one frame carries.
    pub fn packets_per_frame(&self) -> usize {
        if self.symbol_size == 0 {
            return 0;
        }
        self.data_bytes().saturating_sub(HEADER_WITH_CRC) / self.symbol_size as usize
    }

    /// Number of RaptorQ source symbols an envelope of `len` bytes needs:
    /// the minimum number of distinct symbols a receiver must collect.
    pub fn source_symbols(&self, len: usize) -> u32 {
        if self.symbol_size == 0 {
            return 0;
        }
        len.div_ceil(self.symbol_size as usize) as u32
    }

    fn region_bytes(&self) -> usize {
        self.frame_bytes().saturating_sub(FORMAT_BYTES)
    }
}

/// What one grid cell is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// Part of a finder, the orientation mark, a separator or a timing
    /// line; `true` is dark.
    Fixed(bool),
    /// Carries no data; darkened as needed to balance its block.
    Balance,
    /// Carries one frame bit.
    Data,
}

/// Which way round a read grid is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// As drawn.
    Normal,
    /// Reflected across the top-left to bottom-right diagonal: the
    /// picture was mirrored.
    Mirrored,
}

/// The fixed cell at `row`, `col` of an `n`-cell grid, if it is one.
fn pattern(n: u16, row: u16, col: u16) -> Option<bool> {
    // Finders: top left, top right, bottom left.
    for (r0, c0) in [(0, 0), (0, n - FINDER), (n - FINDER, 0)] {
        let box_r0 = r0.saturating_sub(1);
        let box_c0 = c0.saturating_sub(1);
        let in_box = row >= box_r0
            && row < box_r0 + FINDER_BOX
            && col >= box_c0
            && col < box_c0 + FINDER_BOX;
        if !in_box {
            continue;
        }
        let in_mark = row >= r0 && row < r0 + FINDER && col >= c0 && col < c0 + FINDER;
        if !in_mark {
            return Some(false);
        }
        let (r, c) = (row - r0, col - c0);
        let ring = r.min(c).min(FINDER - 1 - r).min(FINDER - 1 - c);
        return Some(ring != 1);
    }

    // Orientation mark: bottom right.
    let o0 = n - ORIENT_BOX;
    if row >= o0 && col >= o0 {
        if row == o0 || col == o0 {
            return Some(false);
        }
        return Some(orient_mark(row - o0 - 1, col - o0 - 1));
    }

    // Timing lines.
    if row == TIMING && col >= FINDER_BOX && col < n - FINDER_BOX {
        return Some(col.is_multiple_of(2));
    }
    if col == TIMING && row >= FINDER_BOX && row < n - FINDER_BOX {
        return Some(row.is_multiple_of(2));
    }
    None
}

/// Whether cell `r`, `c` of the 5 by 5 orientation mark is dark.
fn orient_mark(r: u16, c: u16) -> bool {
    let ring = r.min(c).min(4 - r).min(4 - c);
    ring != 1 || ORIENT_INNER_DARK.contains(&(r, c))
}

/// Which way round a grid was read, judged from its orientation mark.
/// `dark(row, col)` gives the cell as read. `None` if the mark matches
/// neither way round closely enough (more than one cell off).
/// Also `None` for a spec that is not a preset.
pub fn orientation(spec: &CodeSpec, dark: impl Fn(u16, u16) -> bool) -> Option<Orientation> {
    if !spec.is_valid() {
        return None;
    }
    let o = spec.cells - 5;
    let (mut normal, mut mirrored) = (0, 0);
    for r in 0..5 {
        for c in 0..5 {
            let seen = dark(o + r, o + c);
            normal += (seen != orient_mark(r, c)) as u32;
            mirrored += (seen != orient_mark(c, r)) as u32;
        }
    }
    match (normal, mirrored) {
        (n, m) if n <= 1 && n < m => Some(Orientation::Normal),
        (n, m) if m <= 1 && m < n => Some(Orientation::Mirrored),
        _ => None,
    }
}

/// One 8 by 8 block: its fixed dark cells, and the grid indices of its
/// data and balance cells.
#[derive(Debug, Clone)]
struct Block {
    fixed_dark: u32,
    data: Vec<usize>,
    balance: Vec<usize>,
}

/// Where everything sits in a grid: the cell kinds, the data cells in bit
/// order and the balance blocks.
#[derive(Debug, Clone)]
pub struct Layout {
    cells: u16,
    kinds: Vec<Cell>,
    data_positions: Vec<(u16, u16)>,
    /// For each grid index, its position in `data_positions`.
    data_index: Vec<Option<usize>>,
    blocks: Vec<Block>,
}

impl Layout {
    /// The layout of `spec`'s grid.
    ///
    /// # Panics
    ///
    /// Panics if the grid is under 24 cells or not a multiple of 8.
    pub fn new(spec: &CodeSpec) -> Layout {
        let n = spec.cells;
        assert!(
            n >= 24 && n.is_multiple_of(BLOCK),
            "a {n} cell grid is not drawable"
        );
        let nu = n as usize;
        let mut kinds: Vec<Cell> = (0..nu * nu)
            .map(|i| match pattern(n, (i / nu) as u16, (i % nu) as u16) {
                Some(dark) => Cell::Fixed(dark),
                None => Cell::Data,
            })
            .collect();

        // Balance cells: the last few free cells of each roomy block.
        let per_side = nu / BLOCK as usize;
        for br in 0..per_side {
            for bc in 0..per_side {
                let free: Vec<usize> = block_indices(nu, br, bc)
                    .filter(|&i| kinds[i] == Cell::Data)
                    .collect();
                if free.len() >= BALANCE_MIN_FREE {
                    for &i in &free[free.len() - BALANCE_CELLS..] {
                        kinds[i] = Cell::Balance;
                    }
                }
            }
        }

        let mut data_positions = Vec::new();
        let mut data_index = vec![None; nu * nu];
        for (i, kind) in kinds.iter().enumerate() {
            if *kind == Cell::Data {
                data_index[i] = Some(data_positions.len());
                data_positions.push(((i / nu) as u16, (i % nu) as u16));
            }
        }

        let mut blocks = Vec::with_capacity(per_side * per_side);
        for br in 0..per_side {
            for bc in 0..per_side {
                let mut block = Block {
                    fixed_dark: 0,
                    data: Vec::new(),
                    balance: Vec::new(),
                };
                for i in block_indices(nu, br, bc) {
                    match kinds[i] {
                        Cell::Fixed(dark) => block.fixed_dark += dark as u32,
                        Cell::Balance => block.balance.push(i),
                        Cell::Data => block.data.push(i),
                    }
                }
                blocks.push(block);
            }
        }

        Layout {
            cells: n,
            kinds,
            data_positions,
            data_index,
            blocks,
        }
    }

    /// What the cell at `row`, `col` is.
    ///
    /// # Panics
    ///
    /// Panics if `row` or `col` is outside the grid.
    pub fn cell(&self, row: u16, col: u16) -> Cell {
        assert!(row < self.cells && col < self.cells, "cell outside grid");
        self.kinds[row as usize * self.cells as usize + col as usize]
    }

    /// Positions `(row, col)` of the data cells in the order they carry
    /// bits; the first [`FORMAT_CELLS`] are the format field.
    pub fn data_positions(&self) -> &[(u16, u16)] {
        &self.data_positions
    }

    /// The whole grid for `frame`, row-major, `true` dark: fixed cells,
    /// data cells from the frame bits (filler past the last byte), and
    /// balance cells darkened to bring each block as near half dark as
    /// they can.
    pub fn grid(&self, frame: &[u8]) -> Vec<bool> {
        let mut grid: Vec<bool> = self
            .kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| match kind {
                Cell::Fixed(dark) => *dark,
                Cell::Balance => false,
                Cell::Data => data_bit(frame, self.data_index[i].unwrap_or(0)),
            })
            .collect();
        for block in &self.blocks {
            let dark = block.fixed_dark + block.data.iter().filter(|&&i| grid[i]).count() as u32;
            for &i in block.balance.iter().take(balance_needed(dark, block)) {
                grid[i] = true;
            }
        }
        grid
    }

    /// Dark cells in each 8 by 8 block of `grid` (from [`Layout::grid`]),
    /// blocks in row-major order.
    pub fn block_dark_counts(&self, grid: &[bool]) -> Vec<u32> {
        let nu = self.cells as usize;
        let per_side = nu / BLOCK as usize;
        let mut out = Vec::with_capacity(per_side * per_side);
        for br in 0..per_side {
            for bc in 0..per_side {
                out.push(block_indices(nu, br, bc).filter(|&i| grid[i]).count() as u32);
            }
        }
        out
    }

    /// How many blocks of `frame`, once balanced, land outside
    /// [`BLOCK_DARK_MIN`] to [`BLOCK_DARK_MAX`] dark cells; 0 means the
    /// frame may be drawn.
    pub(crate) fn unbalanced_blocks(&self, frame: &[u8]) -> usize {
        self.blocks
            .iter()
            .filter(|block| {
                let dark = block.fixed_dark
                    + block
                        .data
                        .iter()
                        .filter(|&&i| data_bit(frame, self.data_index[i].unwrap_or(0)))
                        .count() as u32;
                let total = dark + balance_needed(dark, block) as u32;
                !(BLOCK_DARK_MIN..=BLOCK_DARK_MAX).contains(&total)
            })
            .count()
    }
}

/// How many balance cells to darken in a block that has `dark` dark cells
/// without them.
fn balance_needed(dark: u32, block: &Block) -> usize {
    let half = (BLOCK as u32 * BLOCK as u32) / 2;
    (half.saturating_sub(dark) as usize).min(block.balance.len())
}

/// Grid indices of block `br`, `bc` in an `nu`-cell grid.
fn block_indices(nu: usize, br: usize, bc: usize) -> impl Iterator<Item = usize> {
    let b = BLOCK as usize;
    (0..b).flat_map(move |r| (0..b).map(move |c| (br * b + r) * nu + bc * b + c))
}

/// Bit `i` of the frame's data cells: from the frame bytes, or alternating
/// filler past them.
fn data_bit(frame: &[u8], i: usize) -> bool {
    match frame.get(i / 8) {
        Some(byte) => byte & (0x80 >> (i % 8)) != 0,
        None => i % 2 == 1,
    }
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

/// The coded region for one frame, Reed-Solomon encoded and interleaved
/// but not yet whitened.
pub(crate) fn code_region(
    spec: &CodeSpec,
    tag: u16,
    transfer_length: u16,
    symbol_id: u32,
    symbols: &[Vec<u8>],
) -> Vec<u8> {
    let preset = spec.preset().expect("only presets are drawn");
    let mut data = vec![0u8; spec.data_bytes()];
    data[0] = VERSION;
    data[1] = preset;
    data[2..4].copy_from_slice(&tag.to_be_bytes());
    data[4..6].copy_from_slice(&transfer_length.to_be_bytes());
    data[6..9].copy_from_slice(&symbol_id.to_be_bytes()[1..]);
    let mut at = HEADER_WITH_CRC;
    for symbol in symbols {
        data[at..at + symbol.len()].copy_from_slice(symbol);
        at += symbol.len();
    }
    let crc = data_crc(&data);
    data[HEADER_LEN..HEADER_WITH_CRC].copy_from_slice(&crc.to_be_bytes());
    encode_region(spec, &data)
}

/// Reed-Solomon encode and interleave the coded data.
fn encode_region(spec: &CodeSpec, data: &[u8]) -> Vec<u8> {
    let lengths = codeword_lengths(spec);
    let parity = spec.parity_bytes();
    let encoder = RsEncoder::new(parity);
    let count = lengths.len();
    let mut region = vec![0u8; spec.region_bytes()];
    let mut from = 0;
    for (w, &len) in lengths.iter().enumerate() {
        let word = encoder.encode(&data[from..from + len - parity]);
        from += len - parity;
        for (k, &b) in word.iter().enumerate() {
            region[k * count + w] = b;
        }
    }
    region
}

/// Assemble a frame from a coded region and a mask: format field, then the
/// whitened region.
pub(crate) fn finish_frame(spec: &CodeSpec, region: &[u8], mask: u8) -> Vec<u8> {
    let preset = spec.preset().expect("only presets are drawn");
    let mut out = format_field(preset, mask).to_vec();
    let mut whitened = region.to_vec();
    whiten(&mut whitened, preset, mask);
    out.extend_from_slice(&whitened);
    out
}

/// Read one frame's bytes back. `None` if they are not a frame this
/// version understands, Reed-Solomon cannot repair them, or the CRC
/// fails. Never panics.
pub(crate) fn read_frame(bytes: &[u8]) -> Option<Frame> {
    decode_frame(bytes, &[], 0, false).map(|(frame, _)| frame)
}

/// Repair one frame's bytes as read from a picture, for the detector:
/// the corrected frame bytes, ready for [`read_frame`], or `None` if they
/// cannot be repaired into a frame that passes its CRC. Never panics.
///
/// `doubtful` lists frame byte indices from least to most trusted; up to
/// `per_word` of the first ones falling in each Reed-Solomon codeword are
/// treated as erasures (known-bad positions), which repairs twice as many
/// bytes as errors at unknown positions would. If the format field cannot
/// be read, every mask is tried; the CRC decides.
pub(crate) fn repair_frame(bytes: &[u8], doubtful: &[usize], per_word: usize) -> Option<Vec<u8>> {
    decode_frame(bytes, doubtful, per_word, true).map(|(_, repaired)| repaired)
}

/// The work behind [`read_frame`] and [`repair_frame`]: the frame, and
/// its bytes with every repair applied.
fn decode_frame(
    bytes: &[u8],
    doubtful: &[usize],
    per_word: usize,
    any_mask: bool,
) -> Option<(Frame, Vec<u8>)> {
    let spec = CodeSpec::presets()
        .into_iter()
        .find(|s| s.frame_bytes() == bytes.len())?;
    let preset = spec.preset()?;
    let lengths = codeword_lengths(&spec);
    let count = lengths.len();
    let parity = spec.parity_bytes();

    // Erasure positions within each codeword, most doubtful first.
    let mut erasures: Vec<Vec<u8>> = vec![Vec::new(); count];
    for &i in doubtful {
        let Some(j) = i.checked_sub(FORMAT_BYTES) else {
            continue;
        };
        if j >= spec.region_bytes() {
            continue;
        }
        let word = &mut erasures[j % count];
        if word.len() < per_word.min(parity) {
            word.push((j / count) as u8);
        }
    }

    let masks: Vec<u8> = match read_format(preset, &bytes[..FORMAT_BYTES]) {
        Some(mask) => vec![mask],
        None if any_mask => (0..MASKS).collect(),
        None => Vec::new(),
    };
    masks
        .into_iter()
        .find_map(|mask| decode_region(&spec, bytes, mask, &lengths, &erasures))
}

/// Decode the coded region under one mask: the frame and its repaired
/// bytes, if Reed-Solomon and the CRC accept it.
fn decode_region(
    spec: &CodeSpec,
    bytes: &[u8],
    mask: u8,
    lengths: &[usize],
    erasures: &[Vec<u8>],
) -> Option<(Frame, Vec<u8>)> {
    let preset = spec.preset()?;
    let mut region = bytes[FORMAT_BYTES..].to_vec();
    whiten(&mut region, preset, mask);

    let parity = spec.parity_bytes();
    let decoder = RsDecoder::new(parity);
    let count = lengths.len();
    let mut data = Vec::with_capacity(spec.data_bytes());
    let mut repaired = vec![0u8; region.len()];
    for (w, &len) in lengths.iter().enumerate() {
        let word: Vec<u8> = (0..len).map(|k| region[k * count + w]).collect();
        let erased = Some(erasures[w].as_slice()).filter(|e| !e.is_empty());
        let fixed = decoder.correct(&word, erased).ok()?;
        data.extend_from_slice(fixed.data());
        for (k, &b) in fixed.iter().enumerate() {
            repaired[k * count + w] = b;
        }
    }

    if data[0] != VERSION || data[1] != preset {
        return None;
    }
    let crc = u32::from_be_bytes(data[HEADER_LEN..HEADER_WITH_CRC].try_into().ok()?);
    if data_crc(&data) != crc {
        return None;
    }
    let tag = u16::from_be_bytes([data[2], data[3]]);
    let transfer_length = u16::from_be_bytes([data[4], data[5]]);
    let symbol_id = u32::from_be_bytes([0, data[6], data[7], data[8]]);
    let size = spec.symbol_size as usize;
    let symbols = (0..spec.packets_per_frame())
        .map(|i| data[HEADER_WITH_CRC + i * size..HEADER_WITH_CRC + (i + 1) * size].to_vec())
        .collect();
    let frame = Frame {
        spec: *spec,
        tag,
        transfer_length,
        symbol_id,
        symbols,
    };
    Some((frame, finish_frame(spec, &repaired, mask)))
}

/// Lengths of the Reed-Solomon codewords, parity included, in order.
fn codeword_lengths(spec: &CodeSpec) -> Vec<usize> {
    let total = spec.region_bytes();
    let count = spec.codewords();
    (0..count)
        .map(|w| total / count + usize::from(w < total % count))
        .collect()
}

/// CRC-32 of the coded data, skipping the CRC field itself.
fn data_crc(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&data[..HEADER_LEN]);
    hasher.update(&data[HEADER_WITH_CRC..]);
    hasher.finalize()
}

/// First-order Reed-Muller (32, 6) codeword for a six-bit message.
fn reed_muller(message: u8) -> u32 {
    let mut word = 0u32;
    for i in 0..32u32 {
        let mut bit = message & 1;
        for j in 0..5 {
            bit ^= (message >> (j + 1)) & ((i >> j) & 1) as u8;
        }
        word |= (bit as u32 & 1) << i;
    }
    word
}

/// The format field for a preset and mask: 32 codeword bits, each written
/// as a dark-light or light-dark pair.
fn format_field(preset: u8, mask: u8) -> [u8; FORMAT_BYTES] {
    let word = reed_muller((preset << 4) | (mask & 0x0f));
    let mut out = [0u8; FORMAT_BYTES];
    for i in 0..32 {
        let pair = if word >> i & 1 == 1 { 0b10 } else { 0b01 };
        out[i / 4] |= pair << (6 - 2 * (i % 4));
    }
    out
}

/// The mask named by a format field, if it reads clearly as one of this
/// preset's.
fn read_format(preset: u8, field: &[u8]) -> Option<u8> {
    let (cost, mask) = format_distance(preset, field);
    (cost < 16).then_some(mask)
}

/// How far a format field is from the nearest of this preset's sixteen,
/// and that one's mask. The distance counts each pair read the wrong way
/// round as two and each pair that reads the same twice (unknown) as one,
/// so a field that reads exactly is 0 and one that is no field at all
/// (random cells) is nearly always 10 or more; under 16 it names one
/// field unambiguously.
pub(crate) fn format_distance(preset: u8, field: &[u8]) -> (u32, u8) {
    let mut bits = 0u32;
    let mut known = 0u32;
    for i in 0..32 {
        let pair = (field[i / 4] >> (6 - 2 * (i % 4))) & 0b11;
        match pair {
            0b10 => {
                bits |= 1 << i;
                known |= 1 << i;
            }
            0b01 => known |= 1 << i,
            _ => {}
        }
    }
    let erased = 32 - known.count_ones();
    (0..MASKS)
        .map(|mask| {
            let errors = ((reed_muller((preset << 4) | mask) ^ bits) & known).count_ones();
            (2 * errors + erased, mask)
        })
        .min()
        .unwrap_or((u32::MAX, 0))
}

/// XOR `buf` with the keystream for a preset and mask.
fn whiten(buf: &mut [u8], preset: u8, mask: u8) {
    let mut ks = Keystream::new(WHITEN_SEED ^ ((preset as u64) << 8) ^ mask as u64);
    for b in buf {
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
    fn presets_fill_their_frames() {
        for spec in CodeSpec::presets() {
            assert!(spec.is_valid());
            assert_eq!(spec.packets_per_frame(), 1, "{spec:?}");
            // The symbol uses every data byte after the header.
            assert_eq!(
                spec.symbol_size as usize,
                spec.data_bytes() - HEADER_WITH_CRC,
                "{spec:?}"
            );
            assert!(codeword_lengths(&spec).iter().all(|&l| l <= 255));
        }
    }

    #[test]
    fn format_field_sits_in_rows_0_and_1_clear_of_balance_cells() {
        for spec in CodeSpec::presets() {
            let layout = Layout::new(&spec);
            for &(row, _) in &layout.data_positions()[..FORMAT_CELLS] {
                assert!(row <= 1);
            }
        }
    }

    #[test]
    fn reed_muller_codewords_differ_in_at_least_16_bits() {
        for a in 0..64u8 {
            for b in a + 1..64 {
                assert!((reed_muller(a) ^ reed_muller(b)).count_ones() >= 16);
            }
        }
    }

    #[test]
    fn format_field_survives_seven_bit_errors() {
        let field = format_field(1, 9);
        let mut damaged = field;
        // Swap 7 pairs: 7 codeword bit errors.
        for i in [0usize, 5, 9, 14, 20, 26, 31] {
            damaged[i / 4] ^= 0b11 << (6 - 2 * (i % 4));
        }
        assert_eq!(read_format(1, &damaged), Some(9));
        // Swapping 8 pairs can no longer be told apart safely.
        let mut worse = damaged;
        worse[6 / 4] ^= 0b11 << (6 - 2 * (6 % 4));
        assert_ne!(read_format(1, &worse), Some(9));
    }

    #[test]
    fn orientation_mark_differs_under_every_quarter_turn_and_mirror() {
        let mark: Vec<Vec<bool>> = (0..5)
            .map(|r| (0..5).map(|c| orient_mark(r, c)).collect())
            .collect();
        let rotate = |m: &Vec<Vec<bool>>| -> Vec<Vec<bool>> {
            (0..5)
                .map(|r| (0..5).map(|c| m[4 - c][r]).collect())
                .collect()
        };
        let transpose = |m: &Vec<Vec<bool>>| -> Vec<Vec<bool>> {
            (0..5).map(|r| (0..5).map(|c| m[c][r]).collect()).collect()
        };
        let mut seen = vec![mark.clone()];
        let mut turned = mark.clone();
        for _ in 0..3 {
            turned = rotate(&turned);
            seen.push(turned.clone());
        }
        let mut mirrored = transpose(&mark);
        for _ in 0..4 {
            seen.push(mirrored.clone());
            mirrored = rotate(&mirrored);
        }
        for i in 0..seen.len() {
            for j in i + 1..seen.len() {
                assert_ne!(seen[i], seen[j], "views {i} and {j} match");
            }
        }
    }

    #[test]
    fn a_frame_whose_crc_does_not_match_is_rejected() {
        let spec = CodeSpec::fast();
        let sym = vec![vec![7u8; spec.symbol_size as usize]];
        let good = code_region(&spec, 1, 300, 0, &sym);
        assert!(read_frame(&finish_frame(&spec, &good, 0)).is_some());

        // Same data with one symbol byte changed after the CRC was taken:
        // Reed-Solomon is consistent, so only the CRC can catch it.
        let mut data = Vec::new();
        let lengths = codeword_lengths(&spec);
        let count = lengths.len();
        for (w, &len) in lengths.iter().enumerate() {
            let word: Vec<u8> = (0..len).map(|k| good[k * count + w]).collect();
            data.extend_from_slice(&word[..len - spec.parity_bytes()]);
        }
        data[HEADER_WITH_CRC + 5] ^= 1;
        let bad = encode_region(&spec, &data);
        assert!(read_frame(&finish_frame(&spec, &bad, 0)).is_none());
    }

    #[test]
    fn orientation_of_a_spec_that_is_not_a_preset_is_none() {
        for cells in [0, 3, 4, 5, 40] {
            let spec = CodeSpec {
                cells,
                cell_bits: 1,
                symbol_size: 100,
            };
            assert_eq!(orientation(&spec, |_, _| true), None);
        }
    }

    /// A frame with more damaged bytes than Reed-Solomon can find on its
    /// own repairs when the damaged bytes are named as doubtful, and the
    /// repaired bytes are exactly the frame drawn.
    #[test]
    fn erasures_repair_what_errors_alone_cannot() {
        let spec = CodeSpec::normal();
        let sym = vec![(0..spec.symbol_size)
            .map(|i| (i * 7) as u8)
            .collect::<Vec<u8>>()];
        let good = finish_frame(&spec, &code_region(&spec, 3, 900, 5, &sym), 11);
        let count = spec.codewords();
        let parity = spec.parity_bytes();
        // Damage 3/4 of the parity's worth of bytes in every codeword:
        // over the parity / 2 that errors alone can fix. Consecutive
        // region bytes fall in successive codewords.
        let per_word = 3 * parity / 4;
        let damaged_at: Vec<usize> = (0..per_word * count).map(|k| FORMAT_BYTES + k).collect();
        let mut bad = good.clone();
        for &i in &damaged_at {
            bad[i] ^= 0x5a;
        }
        assert!(read_frame(&bad).is_none());
        assert_eq!(repair_frame(&bad, &[], 0), None);
        let repaired = repair_frame(&bad, &damaged_at, per_word).unwrap();
        assert_eq!(repaired, good);
        assert!(read_frame(&repaired).is_some());
    }

    #[test]
    fn repair_tries_every_mask_when_the_format_field_is_lost() {
        let spec = CodeSpec::slow();
        let sym = vec![vec![42u8; spec.symbol_size as usize]];
        let good = finish_frame(&spec, &code_region(&spec, 8, 300, 0, &sym), 13);
        let mut bad = good.clone();
        bad[..FORMAT_BYTES].fill(0);
        assert!(read_frame(&bad).is_none());
        assert_eq!(repair_frame(&bad, &[], 0), Some(good));
    }

    #[test]
    fn frame_round_trips() {
        let spec = CodeSpec::normal();
        let sym = vec![(0..spec.symbol_size).map(|i| i as u8).collect::<Vec<u8>>()];
        let region = code_region(&spec, 0xbeef, 4096, 42, &sym);
        let bytes = finish_frame(&spec, &region, 5);
        assert_eq!(bytes.len(), spec.frame_bytes());
        let frame = read_frame(&bytes).unwrap();
        assert_eq!(frame.spec, spec);
        assert_eq!(frame.tag, 0xbeef);
        assert_eq!(frame.transfer_length, 4096);
        assert_eq!(frame.symbol_id, 42);
        assert_eq!(frame.symbols, sym);
    }
}
