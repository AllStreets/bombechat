//! Reading light codes out of a camera picture.
//!
//! [`read`] finds every code in one picture and returns the frame bytes
//! of each it can read, ready for [`crate::Collector::push`]. A camera
//! stream should go through a [`Reader`] instead, which remembers where it
//! saw each code. Neither needs a spec: they try each preset grid size the
//! finder spacing allows, and the frame's own Reed-Solomon parity and CRC
//! decide which reading is right.
//!
//! # Time
//!
//! Every reading has a wall-clock budget for its search, [`DEFAULT_BUDGET`]
//! (10 ms) unless given another ([`read_within`], [`Reader::with_budget`]).
//! The clock starts once the picture is ready: shrunk, if it is over 1280
//! pixels on its longer side, and thresholded. That preparation is a
//! fixed cost the budget neither counts nor cuts (about 2.5 ms for
//! 1280x720 and 7 ms for 4K on the machine the tests were measured on, an
//! Apple M5 Pro), so a 4K picture gets as long to search as a 720p one.
//! The budget is checked between stages and inside every loop: every
//! sixteen finder checks, before each timing-line walk, each format-field
//! check and each fit. Once it has passed, the reading stops and gives
//! what it has: the frames read so far, else [`ReadOutcome::Unreadable`]
//! if it saw a code (or had a fit whose format field read still to try),
//! else [`ReadOutcome::NoCode`]. Each stage stops at its own share of the
//! time left when it starts, so a slow stage leaves the later ones time:
//! a [`Reader`] following the codes it has seen stops at 30 percent of the
//! budget, the finder search at 65 percent of what is left after that,
//! the walks at half of what is left after the finder search, the fits
//! whose format field reads at four fifths of what is left after the
//! walks.
//!
//! What this does and does not promise:
//!
//! - A reading takes its preparation and its budget, and runs over by at
//!   most one unit of work, the longest being one fit (under a
//!   millisecond). `decode_time_per_frame` asserts that bound for every
//!   test picture, run alone in release.
//! - The budget is wall-clock time, not work: on a slower or busier core
//!   less of the search gets done in it. A camera reader should run on a
//!   thread that gets a performance core; on an efficiency core a picture
//!   takes about three times as long. A [`Reader`] makes up for it: after
//!   3 pictures in a row it reads nothing from, it doubles its budget, and
//!   again after each 3 more, up to 40 ms; the first picture it reads
//!   from puts it back to its own budget. A code that a slow or busy
//!   machine cannot find in the default budget is read a few pictures
//!   later instead.
//! - Running out of time is not an error and loses nothing for good: at
//!   30 pictures a second the next picture comes 33 ms later, a [`Reader`]
//!   that has read a code once tries it first in every later picture, and
//!   one that has read nothing for a while gives each picture longer.
//! - It does not promise that a code is found within the budget. The
//!   search below is ordered so that clutter cannot push a code far back,
//!   but every mark found costs a little, and enough of them can use the
//!   budget up first. The tests measure how often that happens.
//!
//! # Following a code
//!
//! A code on a screen stays where it is while its frames change. A
//! [`Reader`] that has seen a code (read or not) starts each later picture
//! there, before any search: it finds each of the code's three finders
//! again within a few cells of where it was (so a code that drifts with a
//! hand-held camera stays followed), fits the grid on them, and reads it.
//! That costs three finder searches over a few cells each and one or two
//! fits, whatever else is in the picture, and needs only the code's own
//! cells, not its quiet zone, so clutter that
//! comes right up to the code, or that the search could not see past in
//! the time, does not matter once the code has been found. The rest of
//! the budget goes on a search of the rest of the picture for other codes.
//! Following takes at most 30 percent of the budget, however many codes
//! are followed and however they read, so the search always has the rest.
//! A followed code missing from 15 pictures in a row is forgotten, as is
//! one in view but unread in 8 pictures in a row (a code that cannot be
//! read, or something code-like that is not a code, does not hold its
//! place for long), and everything when the picture size changes. At most
//! two codes are followed, codes that read when last seen before codes
//! that did not.
//!
//! # Which finders
//!
//! A picture can hold far more finder-like marks than a code has: tiles,
//! a print of dots, other kinds of code. The search keeps clutter from
//! getting ahead of a code by taking turns, never by ranking marks by how
//! crisp they look, and checks the cheapest sign of a real code before
//! spending a fit on it.
//!
//! - **Corners.** Each mark is tested for a code corner's surroundings
//!   (`code_corner`): light quiet zone round more than half of it, and the
//!   code's own cells, not all light, round the rest. A mark in a lattice
//!   of others, or alone on a blank ground, fails.
//! - **Timing lines.** Each mark that looks like a corner walks to its
//!   partners, marks of about its size at a distance that suits a preset
//!   (those that look like corners first), looking for the timing line a
//!   code's finders have between them, beside the line through their
//!   centres (`detect::timing_lines`, which allows for perspective: each
//!   finder's own size along and across the line sets the walk). The
//!   marks take turns, one walk each, before any takes another, the
//!   64-pixel squares of the picture taking turns within a turn; walks
//!   stop at `MAX_WALK` (500 thousand) pixels or the walks' share of the
//!   budget. A mark with timing lines to two others, each on the side
//!   facing the third, makes a linked triple, so a code whose finders all
//!   look like corners, or two of them do, is linked.
//! - **Triples by shape.** The best `MAX_CANDIDATES` (16) marks' triples
//!   by shape alone (corners first) are added, for a code with only one
//!   finder that looks like a corner. These can be crowded out by more
//!   than 16 marks that look better.
//! - **Order.** Each triple's top left mark gives one triple (linked ones
//!   first, then by how near the three are to a code's geometry: a right
//!   angle, equal legs, equal finder sizes, a preset's spacing) before
//!   any mark gives another, the squares of the picture again taking
//!   turns. So a code's own triple comes in the first turn, after at most
//!   one triple from each other top left mark.
//! - **Format field first.** Before a triple is fitted, its format field
//!   (in the code's first two rows, between the top finders, where even a
//!   rough grid from the three finders falls right) is read, straight and
//!   transposed, by comparing the two cells of each pair. A fit is tried
//!   at once if the field is within 9 of a field of its preset (see
//!   `crate::code::format_distance`): 99 percent of real codes in the
//!   sweep read 0 to 9, clutter reads 17 to 29 and passes about three
//!   times in ten thousand. The others are tried after, linked ones first,
//!   while attempts and time remain. A picture tries at most
//!   `MAX_ATTEMPTS` (12) fits.
//!
//! For each fit the reader solves the perspective, samples every cell,
//! and thresholds each cell against the mean of the cells around it. That
//! local mean sits halfway between light and dark because every 8 by 8
//! block of a drawn code is 40 to 60 percent dark, so blur, glare and dim
//! light move the threshold with the cells.
//!
//! The orientation mark says whether the picture is mirrored (front
//! cameras often mirror their feed); a mirrored picture is read
//! transposed. Cells closest to the threshold mark their bytes as
//! doubtful, and if the frame does not repair as read, it is retried with
//! the most doubtful bytes treated as Reed-Solomon erasures.
//!
//! A fit whose finders, timing lines and orientation mark each mostly
//! read right is a code in view: if no reading of it passes the CRC, the
//! result is [`ReadOutcome::Unreadable`] rather than
//! [`ReadOutcome::NoCode`], so the interface can say it sees a code it
//! cannot read. Once a code is seen its finders are spent, so an
//! unreadable code cannot use up the attempts a readable one needs.
//!
//! # How often a wrong frame could get through
//!
//! Only a fit that is a code in view is ever decoded. A wrong reading is
//! accepted only if, after Reed-Solomon repair, its version and preset
//! bytes are right and its CRC-32 matches its data. Treating a wrong
//! repair as random bytes, that is one chance in 2^16 times 2^32 = 2^48
//! per reading checked. One picture checks at most 16 fits (12 in the
//! search, and two for each of the two codes a [`Reader`] follows), 2
//! readings (straight and transposed) each, 3 repairs (errors only, then
//! two erasure counts) each, under 1 mask, or all 16 when the format
//! field is unreadable: at most 1536 readings, so a wrong frame at most
//! once in 2^48 / 1536, about 1.8 * 10^11 pictures, or once in 190 years
//! of a code in view at 30 pictures a second. Wrong readings of real
//! frames (a picture blending two frames, say) have the right version and
//! preset already, so only the CRC's 2^32 counts: at most once in 2.8
//! million pictures if every format field were unreadable, once in 45
//! million with one mask (96 readings). Repairs that use erasures keep
//! some parity spare, so Reed-Solomon itself turns most wrong guesses
//! away before the CRC is asked, and a frame that did slip through would
//! still fail the envelope's own authentication.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use image::GrayImage;

use crate::code::{
    format_distance, orientation, repair_frame, Cell, CodeSpec, Layout, Orientation, FORMAT_CELLS,
};
use crate::detect::{
    contrast, find_finders, locate, rough, sample_cells, timing_lines, Deadline, Finder,
    Homography, Picture,
};

/// How long [`read`] and [`Reader::read`] search one picture once it is
/// prepared: they stop once it has passed, over by at most one fit (about
/// half a millisecond). Preparing the picture comes before and is not
/// counted (see the module docs, Time).
pub const DEFAULT_BUDGET: Duration = Duration::from_millis(10);

/// Most a [`Reader`] raises its budget to after pictures it reads nothing
/// from (unless its own budget is more).
const MOST_BUDGET: Duration = Duration::from_millis(40);

/// Pictures in a row a [`Reader`] reads nothing from before it doubles its
/// budget.
const RAISE_AFTER: u32 = 3;

/// Share of the budget, in thousandths, following known codes may take.
const FOLLOW_SHARE: u32 = 300;

/// Share of the time left when the search starts, in thousandths, the
/// finder search may take.
const FINDERS_SHARE: u32 = 650;

/// Share of the time left after the finder search, in thousandths, the
/// timing-line walks may take.
const WALKS_SHARE: u32 = 500;

/// Share of the time left after the walks, in thousandths, the fits whose
/// format field reads may take; the other fits are tried in the rest.
const CHECKS_SHARE: u32 = 800;

/// Furthest a format field may read from one of its preset's (see
/// `format_field`) for its fit to be tried first.
const FORMAT_GATE: u32 = 9;

/// Most finder candidates considered when forming triples by shape alone.
const MAX_CANDIDATES: usize = 16;

/// Most pixels the timing-line walks of one picture may take together.
const MAX_WALK: usize = 500_000;

/// Most grid fits (one triple at one grid size) tried in one picture's
/// search.
const MAX_ATTEMPTS: usize = 12;

/// Most codes a [`Reader`] follows from picture to picture.
const MAX_TRACKS: usize = 2;

/// Pictures in a row a followed code may be missing from before a
/// [`Reader`] forgets it.
const LOST_AFTER: u32 = 15;

/// Pictures in a row a followed code may be in view but unread before a
/// [`Reader`] forgets it.
const UNREAD_AFTER: u32 = 8;

/// Pictures whose longer side is over this are shrunk by a whole factor
/// before reading.
const MAX_SIDE: u32 = 1280;

/// Side of the window, in cells, a cell's threshold is the mean of.
const THRESHOLD_WINDOW: usize = 9;

/// Side, in pixels, of the squares marks take turns by.
const TILE: f32 = 64.0;

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

/// Read every light code in `image`, a grey camera picture of any size,
/// within [`DEFAULT_BUDGET`]. A picture whose longer side is over 1280
/// pixels is first shrunk by the smallest whole factor that brings that
/// side to 1280 or under (averaging each square of pixels), so a 4K
/// picture, landscape or portrait, costs about what a 720p one does; codes
/// in it must then still have cells of about 3 pixels or more after
/// shrinking. Never panics.
///
/// This reads each picture on its own. A camera stream should go through
/// a [`Reader`], which looks first where it last saw a code.
pub fn read(image: &GrayImage) -> ReadOutcome {
    read_within(image, DEFAULT_BUDGET)
}

/// [`read`] with a time budget of `budget` instead of the default. When
/// the budget runs out the reading stops where it is and gives what it has
/// found: the frames read so far, else [`ReadOutcome::Unreadable`] if it
/// saw a code or had a likely one still to try, else
/// [`ReadOutcome::NoCode`].
pub fn read_within(image: &GrayImage, budget: Duration) -> ReadOutcome {
    let Some(image) = prepare(image) else {
        return ReadOutcome::NoCode;
    };
    let pic = Picture::new(&image);
    let deadline = Deadline::new(budget);
    let mut found = Found::default();
    search(&pic, deadline, &[], &mut found);
    found.outcome()
}

/// Reads light codes out of successive pictures from one camera.
///
/// A code on a screen stays put from picture to picture while its frames
/// change, so once a code has been seen (read or not) the reader tries the
/// same place first in the next picture: it looks for each finder again
/// close to where it was (so a code that drifts a little is kept), fits
/// the grid there, and only then searches the rest of the picture with
/// what is left of the budget. However cluttered the picture, a code that
/// has been seen once is tried first, and following takes at most 30
/// percent of the budget. A code missing from 15 pictures in a row is
/// forgotten, as is one in view but unread in 8 pictures in a row, and
/// everything when the picture size changes.
///
/// Each picture is searched for at most the reader's current budget, as
/// for [`read_within`]. That starts as the budget it was made with; after
/// 3 pictures in a row it reads nothing from, it doubles, and again after
/// each 3 more, up to 40 ms (or the reader's own budget, if more), so a
/// slow or busy machine still gets to a code; the first picture it reads
/// from puts it back.
#[derive(Debug, Clone)]
pub struct Reader {
    /// The budget it was made with.
    base: Duration,
    /// The budget the next picture gets.
    budget: Duration,
    /// Pictures in a row it has read nothing from since the budget last
    /// changed.
    fruitless: u32,
    tracks: Vec<Track>,
    size: (u32, u32),
}

/// A code a [`Reader`] is following.
#[derive(Debug, Clone, Copy)]
struct Track {
    finders: [Finder; 3],
    spec: CodeSpec,
    /// Pictures in a row it was not found in.
    misses: u32,
    /// Pictures in a row it was found in but not read.
    unread: u32,
}

impl Default for Reader {
    fn default() -> Self {
        Reader::new()
    }
}

impl Reader {
    /// A reader with the [`DEFAULT_BUDGET`] per picture.
    pub fn new() -> Self {
        Reader::with_budget(DEFAULT_BUDGET)
    }

    /// A reader that takes at most `budget` per picture.
    pub fn with_budget(budget: Duration) -> Self {
        Reader {
            base: budget,
            budget,
            fruitless: 0,
            tracks: Vec::new(),
            size: (0, 0),
        }
    }

    /// The budget the next picture gets: the reader's own, or more after
    /// pictures it read nothing from.
    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// Read every light code in the next camera picture.
    pub fn read(&mut self, image: &GrayImage) -> ReadOutcome {
        let outcome = self.read_picture(image);
        if matches!(outcome, ReadOutcome::Frames(_)) {
            self.budget = self.base;
            self.fruitless = 0;
        } else {
            self.fruitless += 1;
            if self.fruitless >= RAISE_AFTER {
                let most = self.base.max(MOST_BUDGET);
                self.budget = self.budget.saturating_mul(2).min(most);
                self.fruitless = 0;
            }
        }
        outcome
    }

    fn read_picture(&mut self, image: &GrayImage) -> ReadOutcome {
        let Some(image) = prepare(image) else {
            self.tracks.clear();
            return ReadOutcome::NoCode;
        };
        if image.dimensions() != self.size {
            self.tracks.clear();
            self.size = image.dimensions();
        }
        let pic = Picture::new(&image);
        let deadline = Deadline::new(self.budget);
        let follow_end = deadline.share(FOLLOW_SHARE);
        let mut found = Found::default();
        let mut known = Vec::new();
        for track in &mut self.tracks {
            if follow_end.passed() {
                break;
            }
            match follow(&pic, track, follow_end) {
                Some((finders, grid)) => {
                    track.finders = finders;
                    track.misses = 0;
                    known.push(finders);
                    let read = found.add(&grid, finders);
                    track.unread = if read { 0 } else { track.unread + 1 };
                }
                // Not reached in the time is not missing.
                None if follow_end.passed() => {}
                None => track.misses += 1,
            }
        }
        let followed = found.codes.len();
        search(&pic, deadline, &known, &mut found);
        for &(finders, spec, read) in &found.codes[followed..] {
            self.tracks.retain(|t| !same_place(&t.finders, &finders));
            self.tracks.push(Track {
                finders,
                spec,
                misses: 0,
                unread: (!read) as u32,
            });
        }
        self.tracks
            .retain(|t| t.misses < LOST_AFTER && t.unread < UNREAD_AFTER);
        // Codes last read when seen first, those missed fewest pictures
        // first; then codes seen but not read. So two codes that cannot be
        // read cannot crowd out one that can.
        self.tracks
            .sort_by_key(|t| (t.unread > 0, t.misses, t.unread));
        self.tracks.truncate(MAX_TRACKS);
        found.outcome()
    }
}

/// `image` ready to read: shrunk if it is too big, `None` if it is too
/// small.
fn prepare(image: &GrayImage) -> Option<Cow<'_, GrayImage>> {
    let side = image.width().max(image.height());
    let image = if side > MAX_SIDE {
        Cow::Owned(shrink(image, side.div_ceil(MAX_SIDE)))
    } else {
        Cow::Borrowed(image)
    };
    (image.width() >= 32 && image.height() >= 32).then_some(image)
}

/// Whether two codes' finders are in about the same place.
fn same_place(a: &[Finder; 3], b: &[Finder; 3]) -> bool {
    let centre = |f: &[Finder; 3]| {
        (
            f.iter().map(|g| g.x).sum::<f32>() / 3.0,
            f.iter().map(|g| g.y).sum::<f32>() / 3.0,
        )
    };
    let ((ax, ay), (bx, by)) = (centre(a), centre(b));
    let leg = (a[1].x - a[0].x).hypot(a[1].y - a[0].y);
    (ax - bx).hypot(ay - by) < leg / 4.0
}

/// What the reading of one picture has found so far.
#[derive(Debug, Default)]
struct Found {
    frames: Vec<Vec<u8>>,
    /// Some fit was a code in view.
    seen: bool,
    /// The search ran out of time or attempts with a fit whose format
    /// field read still untried.
    pending: bool,
    /// Every code in view: where it is, at what grid size, and whether it
    /// was read.
    codes: Vec<([Finder; 3], CodeSpec, bool)>,
}

impl Found {
    /// A code in view at `finders`, read if it can be. Whether it was.
    fn add(&mut self, grid: &Grid, finders: [Finder; 3]) -> bool {
        self.seen = true;
        let bytes = grid.decode();
        self.codes.push((finders, grid.spec, bytes.is_some()));
        if let Some(bytes) = bytes {
            if !self.frames.contains(&bytes) {
                self.frames.push(bytes);
            }
            return true;
        }
        false
    }

    fn outcome(self) -> ReadOutcome {
        if !self.frames.is_empty() {
            ReadOutcome::Frames(self.frames)
        } else if self.seen || self.pending {
            ReadOutcome::Unreadable
        } else {
            ReadOutcome::NoCode
        }
    }
}

/// Fit a followed code where it was last seen: its finders found again
/// nearby where they can be, else where they were. The finders and grid,
/// if that is a code in view.
fn follow(pic: &Picture, track: &Track, deadline: Deadline) -> Option<([Finder; 3], Grid)> {
    let n = track.spec.cells;
    let near = track
        .finders
        .map(|f| nearest_mark(pic, &f, deadline).unwrap_or(f));
    let tries = if near == track.finders {
        vec![near]
    } else {
        vec![near, track.finders]
    };
    tries.into_iter().find_map(|finders| {
        if deadline.passed() {
            return None;
        }
        let h = locate(pic, finders, n)?;
        let grid = Grid::new(track.spec, sample_cells(pic, &h, n));
        grid.fixed_cells().in_view().then_some((finders, grid))
    })
}

/// The finder mark of about the same size nearest to where `f` was, within
/// four of its cells.
fn nearest_mark(pic: &Picture, f: &Finder, deadline: Deadline) -> Option<Finder> {
    let area = pic.around(f.x, f.y, 8.0 * f.module + 4.0);
    find_finders(pic, area, deadline)
        .into_iter()
        .filter(|g| g.module < 1.5 * f.module && f.module < 1.5 * g.module)
        .map(|g| ((g.x - f.x).hypot(g.y - f.y), g))
        .filter(|&(dist, _)| dist < 4.0 * f.module)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, g)| g)
}

/// Search the whole picture for codes, leaving out the finders of codes
/// already found in it (`known`).
fn search(pic: &Picture, deadline: Deadline, known: &[[Finder; 3]], found: &mut Found) {
    if deadline.passed() {
        return;
    }
    let near_known = |m: &Finder| {
        known
            .iter()
            .flatten()
            .any(|k| (k.x - m.x).hypot(k.y - m.y) < 2.0 * k.module)
    };
    // Marks that look like a code's corners first, the rest after.
    let (mut finders, rest): (Vec<Finder>, Vec<Finder>) =
        find_finders(pic, pic.area(), deadline.of_rest(FINDERS_SHARE))
            .into_iter()
            .filter(|m| !near_known(m))
            .partition(|f| code_corner(pic, f));
    let corners = finders.len();
    finders.extend(rest);
    // Triples joined by timing lines, then the best marks' triples by
    // shape alone.
    let mut triples = linked_triples(pic, &finders, corners, deadline.of_rest(WALKS_SHARE));
    let linked: HashSet<[usize; 3]> = triples.iter().map(|t| t.finders).collect();
    triples.extend(
        shape_triples(&finders)
            .into_iter()
            .filter(|t| !linked.contains(&t.finders)),
    );

    let mut used = vec![false; finders.len()];
    let mut attempts = 0;
    let mut later = Vec::new();
    // Every fit whose format field reads is tried as soon as it is found,
    // in turns (see `in_turns`); the rest after, in the same order, while
    // attempts and time remain.
    let checks_end = deadline.of_rest(CHECKS_SHARE);
    for t in in_turns(&finders, triples) {
        if checks_end.passed() {
            break;
        }
        let three = t.finders.map(|i| finders[i]);
        if !quiet_outside(pic, three) {
            continue;
        }
        for &(_, spec) in &t.specs {
            if t.finders.iter().any(|&i| used[i]) {
                break;
            }
            let distance = rough(pic, three, spec.cells)
                .map_or(u32::MAX, |h| format_field(pic, &h, three, &spec));
            if distance > FORMAT_GATE {
                later.push((!t.linked, later.len(), t.finders, spec));
                continue;
            }
            if attempts >= MAX_ATTEMPTS || deadline.passed() {
                found.pending = true;
                return;
            }
            attempts += 1;
            if let Some(grid) = fit(pic, three, spec) {
                t.finders.iter().for_each(|&i| used[i] = true);
                found.add(&grid, three);
            }
        }
    }
    // Fits joined by timing lines first, each group in turns as above.
    later.sort_by_key(|&(unlinked, k, _, _)| (unlinked, k));
    for (_, _, idx, spec) in later {
        if attempts >= MAX_ATTEMPTS || deadline.passed() {
            return;
        }
        if idx.iter().any(|&i| used[i]) {
            continue;
        }
        attempts += 1;
        let three = idx.map(|i| finders[i]);
        if let Some(grid) = fit(pic, three, spec) {
            idx.iter().for_each(|&i| used[i] = true);
            found.add(&grid, three);
        }
    }
}

/// The grid at `spec`'s size on `three` finders, if it is a code in view.
fn fit(pic: &Picture, three: [Finder; 3], spec: CodeSpec) -> Option<Grid> {
    let h = locate(pic, three, spec.cells)?;
    let grid = Grid::new(spec, sample_cells(pic, &h, spec.cells));
    grid.fixed_cells().in_view().then_some(grid)
}

/// Whether the format field reads as one of `spec`'s, straight or
/// transposed (a mirrored picture), sampled through `h`. A code's format
/// field lies in its first two rows between the top finders, close to
/// the finders, so even the rough homography (see [`rough`]) finds it.
/// Its cells come in pairs, one dark and one light, so each pair is read
/// by comparing its two cells, with no threshold to set: a pair whose two
/// cells differ by less than a quarter of the finder's contrast is
/// unknown. The field must then be within the Reed-Muller code's reach of
/// one of the preset's sixteen fields; clutter, another kind of code, or
/// a grid fitted at the wrong size, nearly never is.
fn format_field(pic: &Picture, h: &Homography, three: [Finder; 3], spec: &CodeSpec) -> u32 {
    let Some(preset) = spec.preset() else {
        return u32::MAX;
    };
    let cells = &layout(spec).data_positions()[..FORMAT_CELLS];
    let margin = 0.25 * contrast(pic, three, spec.cells);
    [false, true]
        .into_iter()
        .map(|transposed| {
            let value = |&(r, c): &(u16, u16)| {
                let (r, c) = if transposed { (c, r) } else { (r, c) };
                let (x, y) = h.map(c as f64 + 0.5, r as f64 + 0.5);
                pic.at(x, y)
            };
            let mut field = [0u8; FORMAT_CELLS / 8];
            for (i, pair) in cells.chunks_exact(2).enumerate() {
                let (first, second) = (value(&pair[0]), value(&pair[1]));
                let bits = if first + margin < second {
                    0b10
                } else if second + margin < first {
                    0b01
                } else {
                    0
                };
                field[i / 4] |= bits << (6 - 2 * (i % 4));
            }
            format_distance(preset, &field).0
        })
        .min()
        .unwrap_or(u32::MAX)
}

/// `triples` in the order they are tried: in turns, each top left mark
/// giving its next triple (joined by timing lines first, then by shape)
/// before any gives another. Within a turn the 64-pixel squares of the
/// picture take turns, a mark each. So no number of marks elsewhere, nor
/// how much crisper or squarer they are, can put a code's own triple far
/// back: it comes in the first turn, after at most one triple from each
/// other top left mark.
fn in_turns(finders: &[Finder], mut triples: Vec<Triple>) -> Vec<Triple> {
    triples.sort_by(|a, b| {
        (a.finders[0], !a.linked)
            .cmp(&(b.finders[0], !b.linked))
            .then(a.cost.total_cmp(&b.cost))
    });
    // Each triple's turn: its place among its top left mark's.
    let mut turn = vec![0usize; triples.len()];
    for i in 1..triples.len() {
        if triples[i].finders[0] == triples[i - 1].finders[0] {
            turn[i] = turn[i - 1] + 1;
        }
    }
    let deal = dealing_order(finders);
    let mut order: Vec<usize> = (0..triples.len()).collect();
    order.sort_by_key(|&i| (turn[i], deal[triples[i].finders[0]]));
    let mut slots: Vec<Option<Triple>> = triples.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

/// Each mark's place in a fair turn over the picture: the [`TILE`]
/// squares take turns, giving their marks in the order the marks come in.
fn dealing_order(finders: &[Finder]) -> Vec<usize> {
    let tile = |f: &Finder| {
        (
            (f.y.max(0.0) / TILE) as usize,
            (f.x.max(0.0) / TILE) as usize,
        )
    };
    let mut by_tile: Vec<usize> = (0..finders.len()).collect();
    by_tile.sort_by_key(|&i| (tile(&finders[i]), i));
    let mut place = vec![0usize; finders.len()];
    for k in 1..by_tile.len() {
        if tile(&finders[by_tile[k]]) == tile(&finders[by_tile[k - 1]]) {
            place[by_tile[k]] = place[by_tile[k - 1]] + 1;
        }
    }
    let mut dealt = by_tile.clone();
    dealt.sort_by_key(|&i| place[i]);
    let mut rank = vec![0usize; finders.len()];
    for (k, &i) in dealt.iter().enumerate() {
        rank[i] = k;
    }
    rank
}

/// Whether a mark has a code corner's surroundings. Just outside a code's
/// finder, the quiet zone covers more than half a turn and the code's own
/// cells the rest. Sixteen directions are tested at points five and a
/// quarter and six of the mark's cells out (clear of its own corners,
/// which reach just under five). A direction is light when both points
/// are nearer the mark's light ring than its dark centre, and busy when
/// either falls short of three quarters of the way from dark to light.
/// At least seven light directions in a row (round the circle) and at
/// least two busy ones are needed. A mark in a lattice of others with
/// gaps under about two and a half cells has a neighbour's dark ring
/// along each of its axes at one distance or the other; one with wider
/// gaps, or alone on a blank ground, has nothing busy round it.
fn code_corner(pic: &Picture, f: &Finder) -> bool {
    const POINTS: usize = 16;
    const RUN: usize = 7;
    let at = |angle: f32, d: f32| {
        let (s, c) = angle.sin_cos();
        pic.at(f.x + c * d * f.module, f.y + s * d * f.module)
    };
    let turn = std::f32::consts::TAU;
    let dark = at(0.0, 0.0);
    // The light ring, two cells out, at whatever rotation.
    let ring = (0..8)
        .map(|k| at(turn * k as f32 / 8.0, 2.0))
        .fold(f32::MIN, f32::max);
    let mid = (dark + ring) / 2.0;
    // Quiet zone is light past halfway; a busy direction is one whose
    // nearer or further point falls short of three quarters of the way
    // from dark to light (blur leaves the code's own cells grey).
    let busy_below = dark + 0.75 * (ring - dark);
    let (mut light, mut busy) = (Vec::with_capacity(POINTS), 0);
    for k in 0..POINTS {
        let angle = turn * k as f32 / POINTS as f32;
        let (near, far) = (at(angle, 5.25), at(angle, 6.0));
        light.push(near > mid && far > mid);
        busy += (near.min(far) < busy_below) as usize;
    }
    let (mut run, mut best) = (0, 0);
    for k in 0..2 * POINTS {
        run = if light[k % POINTS] { run + 1 } else { 0 };
        best = best.max(run);
    }
    best >= RUN && busy >= 2
}

/// Whether each finder of a triple (top left, top right, bottom left) has
/// light quiet zone on its outer sides, a cell or two beyond its edge,
/// measured in its own cell size and so unaffected by perspective. Light
/// means nearer the finder's own light ring than its dark centre, so a
/// dim screen in a bright room still counts. Three finders taken from
/// different codes nearly always put one finder's outer side on another
/// code's data.
fn quiet_outside(pic: &Picture, [tl, tr, bl]: [Finder; 3]) -> bool {
    let unit = |p: &Finder| {
        let (dx, dy) = (p.x - tl.x, p.y - tl.y);
        let len = dx.hypot(dy).max(1.0);
        (dx / len, dy / len)
    };
    let (u, v) = (unit(&tr), unit(&bl));
    let mut light = 0;
    let mut total = 0;
    for (f, su, sv) in [(tl, -1.0, -1.0), (tr, 1.0, -1.0), (bl, -1.0, 1.0)] {
        let sample =
            |dx: f32, dy: f32, d: f32| pic.at(f.x + dx * d * f.module, f.y + dy * d * f.module);
        let dark = sample(0.0, 0.0, 0.0);
        let ring = [(u.0, u.1), (-u.0, -u.1), (v.0, v.1), (-v.0, -v.1)]
            .iter()
            .map(|&(dx, dy)| sample(dx, dy, 2.0))
            .fold(f32::MIN, f32::max);
        let mid = (dark + ring) / 2.0;
        // Straight out past each outer edge, and out past the corner (the
        // corner direction is one cell along each axis per step).
        let dirs = [
            (su * u.0, su * u.1),
            (sv * v.0, sv * v.1),
            (su * u.0 + sv * v.0, su * u.1 + sv * v.1),
        ];
        for (dx, dy) in dirs {
            for d in [4.75, 5.5] {
                light += (sample(dx, dy, d) > mid) as u32;
                total += 1;
            }
        }
    }
    light * 6 >= total * 5
}

/// `image` shrunk by `factor` on each side, each output pixel the mean of
/// a `factor` by `factor` square.
fn shrink(image: &GrayImage, factor: u32) -> GrayImage {
    let f = factor as usize;
    let (sw, w, h) = (
        image.width() as usize,
        image.width() as usize / f,
        image.height() as usize / f,
    );
    let src = image.as_raw();
    let mut sums = vec![0u32; w];
    let mut out = Vec::with_capacity(w * h);
    let area = (f * f) as u32;
    for y in 0..h {
        sums.fill(0);
        for row in src[y * f * sw..].chunks(sw).take(f) {
            for (x, sum) in sums.iter_mut().enumerate() {
                *sum += row[x * f..x * f + f].iter().map(|&p| p as u32).sum::<u32>();
            }
        }
        out.extend(sums.iter().map(|&s| ((s + area / 2) / area) as u8));
    }
    GrayImage::from_raw(w as u32, h as u32, out).unwrap_or_default()
}

/// Three finders that could be one code's, as top left, top right and
/// bottom left, with the grid sizes their spacing allows.
struct Triple {
    finders: [usize; 3],
    /// Each grid size the spacing allows, with how far the spacing is from
    /// it (as a log ratio), closest first.
    specs: Vec<(f32, CodeSpec)>,
    /// How far the three are from a code's geometry: a right angle, legs
    /// of equal length, finders of equal size, a preset's spacing. Lower
    /// is more plausible. How crisp the marks are plays no part.
    cost: f32,
    /// Joined by timing lines.
    linked: bool,
}

/// Every triple whose two legs each carry a timing line on the side
/// facing the third finder, as a code's top left finder's legs do. Pairs
/// of finders of nearly the same size whose distance apart suits a preset
/// are walked in turns: each mark walks to its next partner (closest to a
/// preset's spacing first) before any walks to another, the [`TILE`]
/// squares of the picture taking turns within a turn, until [`MAX_WALK`]
/// pixels have been walked or `deadline` passes.
fn linked_triples(
    pic: &Picture,
    finders: &[Finder],
    corners: usize,
    deadline: Deadline,
) -> Vec<Triple> {
    // How far a pair's spacing, in cells, is from the nearest preset's
    // (as a log ratio). A tilted code's legs differ by up to 1.8 times,
    // so the bound for walking a pair is loose.
    let fit = |spacing: f32| {
        CodeSpec::presets()
            .iter()
            .map(|s| (spacing / (s.cells - 7) as f32).ln().abs())
            .fold(f32::MAX, f32::min)
    };
    // Each mark that looks like a code's corner (the first `corners`)
    // walks to its partners: marks of nearly its size whose distance
    // suits a preset, those that look like corners too first, each group
    // closest to a preset's spacing first. A code whose quiet zone is
    // partly covered, or whose cells beside one finder happen to be
    // light, has a finder that does not look like a corner, but the other
    // two do and walk to it.
    let mut partners: Vec<Vec<(bool, f32, usize)>> = vec![Vec::new(); corners];
    for (a, mine) in partners.iter_mut().enumerate() {
        if a.is_multiple_of(64) && deadline.passed() {
            return Vec::new();
        }
        let p = &finders[a];
        for (b, q) in finders.iter().enumerate() {
            if b == a || p.module.max(q.module) > 1.8 * p.module.min(q.module) {
                continue;
            }
            let spacing = (p.x - q.x).hypot(p.y - q.y) / ((p.module + q.module) / 2.0);
            let off = fit(spacing);
            if off < 0.6 {
                mine.push((b >= corners, off, b));
            }
        }
        mine.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)).then(x.2.cmp(&y.2)));
    }
    let deal = dealing_order(&finders[..corners]);
    let mut order: Vec<usize> = (0..corners).collect();
    order.sort_by_key(|&i| deal[i]);
    let counts_fit = |cells: usize| {
        CodeSpec::presets()
            .iter()
            .any(|s| (cells as f32 / (s.cells - 14) as f32).ln().abs() < 0.25)
    };
    // Each finder's legs: the other finder, and the side its line is on.
    let mut legs: Vec<Vec<(usize, f32)>> = vec![Vec::new(); finders.len()];
    let mut walked = HashSet::new();
    let mut work = MAX_WALK;
    'turns: for turn in 0.. {
        let mut more = false;
        for &a in &order {
            let Some(&(_, _, b)) = partners[a].get(turn) else {
                continue;
            };
            more = true;
            let (p, q) = (a.min(b), a.max(b));
            if !walked.insert((p, q)) {
                continue;
            }
            if work == 0 || deadline.passed() {
                break 'turns;
            }
            let lines = timing_lines(pic, &finders[p], &finders[q], &mut work);
            for (line, side) in lines.into_iter().zip([1.0, -1.0]) {
                if line.is_some_and(counts_fit) {
                    legs[p].push((q, side));
                    legs[q].push((p, -side));
                }
            }
        }
        if !more {
            break;
        }
    }
    let mut out = Vec::new();
    for (a, mine) in legs.iter().enumerate() {
        for (i, &(b, side_b)) in mine.iter().enumerate() {
            for &(c, side_c) in &mine[i + 1..] {
                let (p, q, r) = (&finders[a], &finders[b], &finders[c]);
                let cross = (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
                if side_b * cross <= 0.0 || side_c * -cross <= 0.0 {
                    continue;
                }
                if let Some(mut t) = triple([a, b, c], finders).filter(|t| t.finders[0] == a) {
                    t.linked = true;
                    out.push(t);
                }
            }
        }
    }
    out
}

/// Every plausible triple among the first [`MAX_CANDIDATES`] finders, by
/// shape alone.
fn shape_triples(finders: &[Finder]) -> Vec<Triple> {
    let k = finders.len().min(MAX_CANDIDATES);
    let mut out = Vec::new();
    for a in 0..k {
        for b in a + 1..k {
            for c in b + 1..k {
                out.extend(triple([a, b, c], finders));
            }
        }
    }
    out
}

/// The triple three finders make, if they could be one code's.
fn triple(idx: [usize; 3], finders: &[Finder]) -> Option<Triple> {
    let f = idx.map(|i| finders[i]);
    let modules = f.map(|f| f.module);
    let (lo, hi) = (
        modules.iter().cloned().fold(f32::MAX, f32::min),
        modules.iter().cloned().fold(f32::MIN, f32::max),
    );
    if hi > 1.8 * lo {
        return None;
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
        return None;
    }
    let cos = (u.0 * v.0 + u.1 * v.1) / (lu * lv);
    if cos.abs() > 0.5 {
        return None;
    }
    // Top right is the one a quarter turn anticlockwise of bottom left as
    // drawn (y down); a mirrored picture is caught by the orientation mark.
    let cross = u.0 * v.1 - u.1 * v.0;
    let (tr, bl) = if cross > 0.0 { (pi, qi) } else { (qi, pi) };
    let module = (modules[0] + modules[1] + modules[2]) / 3.0;
    let spacing = (lu + lv) / 2.0 / module;
    let mut specs: Vec<(f32, CodeSpec)> = CodeSpec::presets()
        .into_iter()
        .map(|spec| ((spacing / (spec.cells - 7) as f32).ln().abs(), spec))
        .filter(|&(off, _)| off < 0.35)
        .collect();
    specs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let off = specs.first()?.0;
    let shape = cos.abs() + (lu / lv).ln().abs() + (hi / lo).ln();
    Some(Triple {
        finders: [idx[corner], tr, bl],
        specs,
        cost: shape + off,
        linked: false,
    })
}

/// How well a fit's fixed cells read, by kind, each as the share right.
#[derive(Debug, Clone, Copy)]
struct FixedCells {
    /// The three finders and their separators.
    finders: f32,
    /// The two timing lines.
    timing: f32,
    /// The orientation mark, whichever way round reads better.
    mark: f32,
}

impl FixedCells {
    /// Whether this is a light code in view. Each kind of fixed cell must
    /// mostly read right on its own: QR codes share the finders and the
    /// timing lines but not the orientation mark, and a grid fitted at the
    /// wrong size gets the finders but not the timing.
    fn in_view(&self) -> bool {
        self.finders >= 0.85 && self.timing >= 0.75 && self.mark >= 0.8
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

    /// How well the finder, timing and orientation mark cells read.
    fn fixed_cells(&self) -> FixedCells {
        let layout = layout(&self.spec);
        let n = self.spec.cells;
        let o = n - 5;
        let share = |(right, total): (u32, u32)| right as f32 / total.max(1) as f32;
        let (mut finders, mut timing) = ((0, 0), (0, 0));
        for row in 0..n {
            for col in 0..n {
                if row >= o && col >= o {
                    continue;
                }
                if let Cell::Fixed(dark) = layout.cell(row, col) {
                    let tally = if row < 8 && (col < 8 || col >= n - 8) || row >= n - 8 && col < 8 {
                        &mut finders
                    } else {
                        &mut timing
                    };
                    tally.0 += (self.dark(row, col) == dark) as u32;
                    tally.1 += 1;
                }
            }
        }
        // The mark as drawn, and reflected across its diagonal.
        let (mut normal, mut mirrored) = (0, 0);
        for r in 0..5 {
            for c in 0..5 {
                let seen = self.dark(o + r, o + c);
                let drawn = |r, c| matches!(layout.cell(o + r, o + c), Cell::Fixed(true));
                normal += (seen == drawn(r, c)) as u32;
                mirrored += (seen == drawn(c, r)) as u32;
            }
        }
        FixedCells {
            finders: share(finders),
            timing: share(timing),
            mark: share((normal.max(mirrored), 25)),
        }
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
