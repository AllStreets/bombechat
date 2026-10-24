//! Multi-tone FSK acoustic modem: turns one on-air frame into sound and
//! finds frames again in a stream of microphone samples.
//!
//! One transmission is:
//!
//! 1. a preamble: a linear up-chirp across the whole band, 100 ms long;
//! 2. the frame's byte length, sent three times (decoded by bitwise
//!    majority, so a corrupted length cannot misframe the frame before
//!    Reed-Solomon gets a chance to correct it);
//! 3. the frame bytes from [`crate::split`], header and parity included.
//!
//! Every byte is sent as `8 / log2(tones)` symbols, high bits first (two
//! symbols of four bits each with the default 16 tones). A symbol is one
//! tone held for `symbol_ms`, shaped by 2 ms raised-cosine ramps at both
//! edges so the speaker never clicks and the energy stays inside the band.
//! Tone `k` sits at `band_low + k * (band_high - band_low) / (tones - 1)`.
//!
//! The receiver finds the preamble with an FFT matched filter against the
//! complex (analytic) chirp, normalised by the in-band energy of the input
//! so loud out-of-band sound (speech, music) cannot fake a match. Sender and
//! receiver sample clocks never quite agree, and a chirp heard at the wrong
//! clock ratio smears, so the matched filter runs against a small bank of
//! time-scaled chirps covering ±2%. It then correlates the two halves of
//! the chirp separately: the distance between their peaks gives the exact
//! clock ratio, which corrects both the tone frequencies and the symbol
//! period before the first symbol is read. Each symbol is decided by
//! Hann-windowed Goertzel energy at every tone, and an early-late gate on
//! the winning tone keeps the symbol clock locked for the rest of the
//! frame.

use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::frames::{FRAME_DATA, PARITY};

/// Frame header size (`tag u16 | seq u8 | total u8 | len u8`), mirrored from
/// `frames.rs`.
const FRAME_HEADER: usize = 5;
/// Shortest frame on the air: a header, no data, and parity.
const MIN_FRAME: usize = FRAME_HEADER + PARITY;
/// Longest frame on the air: a header, full data and parity.
const MAX_FRAME: usize = FRAME_HEADER + FRAME_DATA + PARITY;
/// How many times the frame length is repeated after the preamble.
const LENGTH_COPIES: usize = 3;

const PREAMBLE_MS: f64 = 100.0;
const RAMP_MS: f64 = 2.0;
/// Peak amplitude of the transmitted signal, leaving headroom for the
/// output stage.
const AMPLITUDE: f64 = 0.5;
/// A [`DemodEvent::Level`] is emitted after this much input.
const LEVEL_MS: f64 = 100.0;
/// Floor for reported levels, so digital silence is a number.
const LEVEL_FLOOR_DBFS: f32 = -120.0;

/// Normalised matched-filter output (1.0 for a clean chirp) above which a
/// preamble is taken to be present. White and speech-band noise peak below
/// 0.05, and the modem's own data tones (heard by a receiver that joined
/// mid-frame) below 0.11.
const DETECT_THRESHOLD: f64 = 0.2;
/// In-band mean square below which the input is treated as silence.
const SILENCE_MEAN_SQUARE: f64 = 1e-12;
/// Largest sender/receiver clock mismatch accepted (2%).
const MAX_CLOCK_ERROR: f64 = 0.02;
/// Symbol clock loop: share of the measured timing error corrected at
/// once, and the share folded into the symbol period.
const LOOP_PHASE_GAIN: f64 = 0.25;
const LOOP_PERIOD_GAIN: f64 = 0.02;

/// Which part of the spectrum the modem uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// 17.6 to 19.8 kHz: above what most adults hear, within what laptop
    /// speakers and microphones still pass. The default.
    NearSilent,
    /// 2.0 to 4.2 kHz: plainly audible, for hardware that filters off the
    /// top of the spectrum.
    Audible,
}

impl Band {
    /// Lowest and highest tone frequency in Hz.
    pub fn range_hz(self) -> (f32, f32) {
        match self {
            Band::NearSilent => (17_600.0, 19_800.0),
            Band::Audible => (2_000.0, 4_200.0),
        }
    }
}

/// Modem parameters. Sender and receiver must agree on all of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModemConfig {
    pub sample_rate: u32,
    pub band: Band,
    /// Number of tones: 2, 4 or 16 (1, 2 or 4 bits per symbol).
    pub tones: u8,
    pub symbol_ms: f32,
}

impl Default for ModemConfig {
    fn default() -> Self {
        ModemConfig {
            sample_rate: 48_000,
            band: Band::NearSilent,
            tones: 16,
            symbol_ms: 20.0,
        }
    }
}

/// What the demodulator heard.
#[derive(Clone, Debug, PartialEq)]
pub enum DemodEvent {
    /// The bytes of one frame exactly as demodulated: header, data and
    /// parity, not yet Reed-Solomon corrected. Feed it to
    /// [`crate::Reassembler::push`].
    Frame(Vec<u8>),
    /// RMS level of the last 100 ms of input, all frequencies, in dBFS.
    Level { dbfs: f32 },
}

/// Sample-domain layout derived from a [`ModemConfig`], shared by the
/// modulator and the demodulator so they cannot disagree.
struct Layout {
    fs: f64,
    lo: f64,
    hi: f64,
    tones: usize,
    bits_per_symbol: usize,
    /// Samples per symbol.
    symbol: usize,
    /// Samples per edge ramp.
    ramp: usize,
    /// Samples in the preamble chirp.
    chirp: usize,
}

impl Layout {
    fn new(cfg: &ModemConfig) -> Self {
        let bits_per_symbol = match cfg.tones {
            2 => 1,
            4 => 2,
            16 => 4,
            t => panic!("tones must be 2, 4 or 16 so symbols pack whole bytes, got {t}"),
        };
        let fs = cfg.sample_rate as f64;
        let (lo, hi) = cfg.band.range_hz();
        assert!(
            (hi as f64) < fs / 2.0,
            "band top {hi} Hz is above the Nyquist frequency of {fs} Hz sampling"
        );
        let symbol = (cfg.symbol_ms as f64 * fs / 1000.0).round() as usize;
        let ramp = (RAMP_MS * fs / 1000.0).round() as usize;
        assert!(
            symbol >= 4 * ramp,
            "symbol of {} ms is too short for its {RAMP_MS} ms edge ramps",
            cfg.symbol_ms
        );
        Layout {
            fs,
            lo: lo as f64,
            hi: hi as f64,
            tones: cfg.tones as usize,
            bits_per_symbol,
            symbol,
            ramp,
            chirp: (PREAMBLE_MS * fs / 1000.0).round() as usize,
        }
    }

    fn symbols_per_byte(&self) -> usize {
        8 / self.bits_per_symbol
    }

    fn tone_hz(&self, k: usize) -> f64 {
        self.lo + k as f64 * (self.hi - self.lo) / (self.tones - 1) as f64
    }

    /// Raised-cosine edge ramps over a burst of `len` samples, at position
    /// `t` (fractional positions are used for time-scaled references).
    fn envelope(&self, t: f64, len: usize) -> f64 {
        let from_edge = t.min(len as f64 - 1.0 - t);
        if from_edge < -0.5 {
            0.0
        } else if from_edge >= self.ramp as f64 {
            1.0
        } else {
            0.5 * (1.0 - (PI * (from_edge + 0.5) / self.ramp as f64).cos())
        }
    }

    /// Phase of the preamble chirp at sample `m`: frequency rises linearly
    /// from `lo` at the first sample to `hi` at the last.
    fn chirp_phase(&self, m: f64) -> f64 {
        let t = m / self.fs;
        let duration = self.chirp as f64 / self.fs;
        TAU * (self.lo * t + (self.hi - self.lo) * t * t / (2.0 * duration))
    }

    /// The analytic (complex) preamble chirp at sample position `m`.
    fn chirp_at(&self, m: f64) -> Complex<f64> {
        Complex::from_polar(self.envelope(m, self.chirp), self.chirp_phase(m))
    }

    /// Symbol values for one byte, high bits first.
    fn byte_symbols(&self, byte: u8) -> impl Iterator<Item = usize> + '_ {
        let b = self.bits_per_symbol;
        (0..self.symbols_per_byte())
            .map(move |s| ((byte >> (8 - b * (s + 1))) as usize) & (self.tones - 1))
    }

    /// Total samples of one transmission carrying a `frame_len`-byte frame.
    fn transmission_len(&self, frame_len: usize) -> usize {
        self.chirp + (LENGTH_COPIES + frame_len) * self.symbols_per_byte() * self.symbol
    }
}

/// Turn one on-air frame (from [`crate::split`]) into samples in `[-0.5,
/// 0.5]`: the preamble chirp, the frame length three times, then the frame.
///
/// The receiver only accepts frames of 21 to 69 bytes, the sizes
/// [`crate::split`] produces.
///
/// # Panics
///
/// Panics if `frame` is longer than 255 bytes, or if `cfg` is not a valid
/// configuration (see [`ModemConfig`]).
pub fn modulate(cfg: &ModemConfig, frame: &[u8]) -> Vec<f32> {
    let l = Layout::new(cfg);
    let len = u8::try_from(frame.len()).expect("a frame is at most 255 bytes");
    let mut out = Vec::with_capacity(l.transmission_len(frame.len()));

    out.extend((0..l.chirp).map(|m| (AMPLITUDE * l.chirp_at(m as f64).re) as f32));

    let bytes = std::iter::repeat_n(len, LENGTH_COPIES).chain(frame.iter().copied());
    for byte in bytes {
        for k in l.byte_symbols(byte) {
            let w = TAU * l.tone_hz(k) / l.fs;
            out.extend((0..l.symbol).map(|i| {
                (AMPLITUDE * l.envelope(i as f64, l.symbol) * (w * i as f64).cos()) as f32
            }));
        }
    }
    out
}

/// Payload bytes per second for full frames, counting the preamble, the
/// length bytes, the frame header and the Reed-Solomon parity as overhead.
/// This is the rate the interface shows.
pub fn measured_rate(cfg: &ModemConfig) -> f32 {
    let l = Layout::new(cfg);
    let seconds = l.transmission_len(MAX_FRAME) as f64 / l.fs;
    (FRAME_DATA as f64 / seconds) as f32
}

/// Streaming receiver. Feed it microphone samples in chunks of any size;
/// the events it returns do not depend on how the input was chunked.
pub struct Demodulator {
    l: Layout,

    // Preamble search.
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    fft_len: usize,
    /// Lags covered by one FFT block.
    block: usize,
    /// Lags after a threshold crossing searched for the true peak.
    peak_span: usize,
    /// One matched filter per assumed clock ratio.
    hypotheses: Vec<Hypothesis>,
    /// Lags either side of the full-chirp peak searched by each half.
    half_radius: usize,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    metrics: Vec<Vec<f64>>,

    // Symbol decisions.
    window: Vec<f64>,
    gate: usize,
    /// Timing error in samples per unit of normalised early-late output.
    gate_scale: f64,

    // Input stream.
    /// Raw samples; `buf[0]` is absolute sample `origin`.
    buf: Vec<f32>,
    /// Running sum of squared band-passed input, one entry longer than
    /// `buf`: `energy[i] - energy[j]` is the in-band energy of absolute
    /// samples `origin + j .. origin + i`.
    energy: Vec<f64>,
    origin: usize,
    band_filter: [Biquad; 4],
    level_sum: f64,
    level_count: usize,
    level_len: usize,

    state: State,
}

/// The preamble as it would sound to a receiver whose clock ratio is
/// `rho`: received sample `n` holds sent sample `n * rho`.
///
/// A linear chirp heard at the wrong clock ratio smears: the chirp rate
/// changes by `2 * (rho - 1)`, and at 1% that is several radians of
/// quadratic phase error across 100 ms, enough to lose the preamble in
/// noise. A small bank of time-scaled references keeps every ratio within
/// [`MAX_CLOCK_ERROR`] close to one of them.
struct Hypothesis {
    rho: f64,
    /// Conjugated spectrum of the scaled chirp, zero-padded to `fft_len`.
    spectrum: Vec<Complex<f32>>,
    /// Length of the scaled chirp in received samples.
    len: usize,
    energy: f64,
    /// The scaled chirp's two halves, for fine clock-ratio estimation.
    halves: [Vec<Complex<f64>>; 2],
}

/// Number of clock-ratio hypotheses, spread evenly over
/// `±MAX_CLOCK_ERROR`. Seven leaves at most 0.33% residual, which costs
/// about 1 radian of quadratic phase at the chirp's ends.
const CLOCK_HYPOTHESES: usize = 7;

enum State {
    /// Looking for a preamble whose start is at or after absolute sample `at`.
    Search {
        at: usize,
    },
    Receive(Reception),
}

/// A frame being read after its preamble was found.
struct Reception {
    /// Absolute position of the matched-filter peak, to resume the search
    /// from if the frame turns out not to be one.
    peak: usize,
    /// Goertzel angular frequency of each tone, corrected for clock ratio.
    omegas: Vec<f64>,
    /// Centre of the next symbol, absolute sample position.
    center: f64,
    /// Symbol period in received samples.
    period: f64,
    symbols: Vec<usize>,
    /// Total symbols in this transmission, once the length is known.
    total: Option<usize>,
}

impl Demodulator {
    pub fn new(cfg: ModemConfig) -> Self {
        let l = Layout::new(&cfg);
        let m = l.chirp;
        let half = m / 2;

        let longest = (m as f64 / (1.0 - MAX_CLOCK_ERROR)).ceil() as usize;
        let fft_len = (2 * longest).next_power_of_two();
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_len);
        let ifft = planner.plan_fft_inverse(fft_len);

        let hypotheses = (0..CLOCK_HYPOTHESES)
            .map(|i| {
                let step = 2.0 * MAX_CLOCK_ERROR / (CLOCK_HYPOTHESES - 1) as f64;
                let rho = 1.0 - MAX_CLOCK_ERROR + i as f64 * step;
                let scaled = |from: usize, sent: usize| -> Vec<Complex<f64>> {
                    let n = (sent as f64 / rho).round() as usize;
                    (0..n)
                        .map(|j| l.chirp_at(from as f64 + j as f64 * rho))
                        .collect()
                };
                let chirp = scaled(0, m);
                let mut spectrum: Vec<Complex<f32>> = chirp
                    .iter()
                    .map(|c| Complex::new(c.re as f32, c.im as f32))
                    .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
                    .take(fft_len)
                    .collect();
                fft.process(&mut spectrum);
                spectrum.iter_mut().for_each(|c| *c = c.conj());
                Hypothesis {
                    rho,
                    spectrum,
                    len: chirp.len(),
                    energy: chirp.iter().map(|c| c.norm_sqr()).sum(),
                    halves: [scaled(0, half), scaled(half, half)],
                }
            })
            .collect();

        let window_len = l.symbol - 2 * l.ramp;
        let window: Vec<f64> = (0..window_len)
            .map(|i| 0.5 * (1.0 - (TAU * i as f64 / (window_len - 1) as f64).cos()))
            .collect();
        let gate = l.symbol / 4;
        let gate_scale = gate_scale(&l, &window, gate);

        let (lo, hi, fs) = (l.lo, l.hi, l.fs);
        let band_filter = [
            Biquad::high_pass(lo * 0.8, 0.5412, fs),
            Biquad::high_pass(lo * 0.8, 1.3066, fs),
            Biquad::low_pass((hi * 1.15).min(fs * 0.45), 0.5412, fs),
            Biquad::low_pass((hi * 1.15).min(fs * 0.45), 1.3066, fs),
        ];

        // A reference at the wrong ratio still matches, but its peak moves
        // by `ratio error * (chirp + lo / chirp_rate)` samples. The search
        // for the best peak after a threshold crossing must reach across
        // the widest such move, between the outermost hypotheses.
        let coupling = m as f64 + lo * m as f64 / (hi - lo);
        let peak_span = (2.0 * MAX_CLOCK_ERROR * coupling).ceil() as usize + m / 16;
        let half_radius = m / 32;
        let block = fft_len - longest + 1;
        assert!(block > 2 * peak_span, "preamble search block too short");

        // The stream is treated as starting after a short stretch of
        // silence, so a preamble at the very first sample still has room
        // either side of its peak for the half-chirp search.
        let lead_in = 2 * half_radius;
        Demodulator {
            fft,
            ifft,
            fft_len,
            block,
            peak_span,
            hypotheses,
            half_radius,
            spectrum: vec![Complex::new(0.0, 0.0); fft_len],
            scratch: vec![Complex::new(0.0, 0.0); fft_len],
            metrics: vec![vec![0.0; block]; CLOCK_HYPOTHESES],
            window,
            gate,
            gate_scale,
            buf: vec![0.0; lead_in],
            energy: vec![0.0; lead_in + 1],
            origin: 0,
            band_filter,
            level_sum: 0.0,
            level_count: 0,
            level_len: (LEVEL_MS * fs / 1000.0).round() as usize,
            state: State::Search { at: 0 },
            l,
        }
    }

    /// Feed the next samples of input. Returns frames heard and level
    /// reports, in the order they occurred in the input.
    pub fn push(&mut self, samples: &[f32]) -> Vec<DemodEvent> {
        let mut events = Vec::new();
        for &x in samples {
            self.level_sum += (x as f64) * (x as f64);
            self.level_count += 1;
            if self.level_count == self.level_len {
                let mean_square = self.level_sum / self.level_len as f64;
                let dbfs = (10.0 * mean_square.log10()) as f32;
                events.push(DemodEvent::Level {
                    dbfs: dbfs.max(LEVEL_FLOOR_DBFS),
                });
                self.level_sum = 0.0;
                self.level_count = 0;
            }

            let in_band = self
                .band_filter
                .iter_mut()
                .fold(x as f64, |v, f| f.process(v));
            let last = *self.energy.last().expect("energy starts with one entry");
            self.energy.push(last + in_band * in_band);
            self.buf.push(x);

            self.advance(&mut events);
        }
        events
    }

    /// Absolute index one past the newest sample.
    fn end(&self) -> usize {
        self.origin + self.buf.len()
    }

    /// Do whatever work the samples received so far allow. Work is only
    /// ever triggered by the absolute sample count, never by chunk
    /// boundaries, which is what makes the output chunking-independent.
    fn advance(&mut self, events: &mut Vec<DemodEvent>) {
        loop {
            match &self.state {
                State::Search { at } => {
                    let at = *at;
                    if self.end() < at + self.fft_len {
                        return;
                    }
                    self.state = self.search(at);
                    if let State::Search { at } = self.state {
                        self.trim(at);
                    }
                }
                State::Receive(rx) => {
                    let last = rx.total == Some(rx.symbols.len() + 1);
                    let reach = self.window.len() / 2 + if last { 0 } else { self.gate };
                    let need = (rx.center + reach as f64).ceil() as usize + 2;
                    if self.end() < need {
                        return;
                    }
                    if let Some(frame) = self.read_symbol() {
                        events.push(DemodEvent::Frame(frame));
                    }
                }
            }
        }
    }

    /// Drop samples well behind `at` once enough have piled up.
    fn trim(&mut self, at: usize) {
        let keep_from = at.saturating_sub(self.fft_len);
        if keep_from > self.origin + 4 * self.fft_len {
            let n = keep_from - self.origin;
            self.buf.drain(..n);
            self.energy.drain(..n);
            // Rebase the running sum so it never grows large enough to
            // lose the precision of quiet spans.
            let base = self.energy[0];
            self.energy.iter_mut().for_each(|e| *e -= base);
            self.origin = keep_from;
        }
    }

    /// Matched-filter one block of lags starting at `at`, against every
    /// clock-ratio hypothesis.
    fn search(&mut self, at: usize) -> State {
        let start = at - self.origin;
        for (s, &x) in self
            .spectrum
            .iter_mut()
            .zip(&self.buf[start..start + self.fft_len])
        {
            *s = Complex::new(x, 0.0);
        }
        self.fft.process(&mut self.spectrum);

        let scale = 1.0 / (self.fft_len as f64 * self.fft_len as f64);
        for (hyp, metric) in self.hypotheses.iter().zip(self.metrics.iter_mut()) {
            for ((s, x), r) in self
                .scratch
                .iter_mut()
                .zip(&self.spectrum)
                .zip(&hyp.spectrum)
            {
                *s = x * r;
            }
            self.ifft.process(&mut self.scratch);
            // Squared normalised correlation: 1.0 for a clean chirp at this
            // ratio, whatever its level; near 0 for anything else.
            for (lag, out) in metric.iter_mut().enumerate() {
                let in_band = self.energy[start + lag + hyp.len] - self.energy[start + lag];
                *out = if in_band <= SILENCE_MEAN_SQUARE * hyp.len as f64 {
                    0.0
                } else {
                    let power = self.scratch[lag].norm_sqr() as f64 * scale;
                    2.0 * power / (hyp.energy * in_band)
                };
            }
        }

        let usable = self.block - self.peak_span;
        let best_at = |lag: usize| -> (usize, f64) {
            self.metrics
                .iter()
                .enumerate()
                .map(|(h, m)| (h, m[lag]))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .expect("at least one hypothesis")
        };
        let Some(cross) = (0..usable).find(|&lag| best_at(lag).1 >= DETECT_THRESHOLD) else {
            return State::Search { at: at + usable };
        };
        let (peak, (h, _)) = (cross..cross + self.peak_span)
            .map(|lag| (lag, best_at(lag)))
            .max_by(|a, b| a.1 .1.total_cmp(&b.1 .1))
            .expect("peak span is not empty");

        match self.acquire(at + peak, h) {
            Some(rx) => State::Receive(rx),
            None => State::Search {
                at: at + peak + self.peak_span,
            },
        }
    }

    /// Estimate the exact clock ratio and start of a preamble whose
    /// matched-filter peak is at absolute sample `peak` under hypothesis
    /// `h`.
    ///
    /// A time-scaled linear chirp still matches a reference at a nearby
    /// ratio, just shifted in time by an amount proportional to its
    /// frequency, so the full-chirp peak alone can't separate "early" from
    /// "fast clock". Correlating the two halves separately can. With the
    /// residual ratio `1 + eps` between the signal and the hypothesis,
    /// half `k` (centre `c_k`, in sent samples) peaks at
    /// `n0 - eps * (2 * c_k + lo / chirp_rate) / rho_h`, so the halves'
    /// peaks sit `eps * chirp_len / rho_h` apart, and the first half's peak
    /// then gives the start `n0`.
    fn acquire(&self, peak: usize, h: usize) -> Option<Reception> {
        let hyp = &self.hypotheses[h];
        let half = self.l.chirp / 2;
        let r = self.half_radius;
        let mut found = [0.0f64; 2];
        for (k, reference) in hyp.halves.iter().enumerate() {
            // Where this half starts relative to the whole chirp, in
            // received samples at this hypothesis.
            let offset = k as f64 * half as f64 / hyp.rho;
            let centre = peak + offset.round() as usize;
            let lo = centre.checked_sub(r)?;
            if lo < self.origin {
                return None;
            }
            let mags: Vec<f64> = (lo..=centre + r)
                .map(|p| {
                    let s = p - self.origin;
                    self.buf[s..s + reference.len()]
                        .iter()
                        .zip(reference)
                        .map(|(&x, c)| c.conj() * x as f64)
                        .sum::<Complex<f64>>()
                        .norm()
                })
                .collect();
            let (best, _) = mags
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .expect("search range is not empty");
            if best == 0 || best == mags.len() - 1 {
                return None;
            }
            let (a, b, c) = (mags[best - 1], mags[best], mags[best + 1]);
            let denom = a - 2.0 * b + c;
            let frac = if denom.abs() > f64::EPSILON {
                0.5 * (a - c) / denom
            } else {
                0.0
            };
            found[k] = (lo + best) as f64 + frac - offset;
        }

        let eps = (found[0] - found[1]) * hyp.rho / (2 * half) as f64;
        let rho = hyp.rho * (1.0 + eps);
        if (rho - 1.0).abs() > MAX_CLOCK_ERROR * 1.1 {
            return None;
        }
        let first_half_centre = (half as f64 - 1.0) / 2.0;
        let lo_over_rate = self.l.lo * self.l.chirp as f64 / (self.l.hi - self.l.lo);
        let n0 = found[0] + eps * (2.0 * first_half_centre + lo_over_rate) / hyp.rho;

        let n = self.l.symbol as f64;
        let omegas = (0..self.l.tones)
            .map(|k| TAU * self.l.tone_hz(k) * rho / self.l.fs)
            .collect();
        Some(Reception {
            peak,
            omegas,
            center: n0 + (self.l.chirp as f64 + (n - 1.0) / 2.0) / rho,
            period: n / rho,
            symbols: Vec::new(),
            total: None,
        })
    }

    /// Decide the next symbol, update the symbol clock, and finish the
    /// frame if that was its last symbol.
    fn read_symbol(&mut self) -> Option<Vec<u8>> {
        let State::Receive(rx) = &self.state else {
            unreachable!("read_symbol runs only while receiving");
        };
        let w = self.window.len();
        let start = (rx.center - (w as f64 - 1.0) / 2.0).round() as usize;

        let (k, _) = rx
            .omegas
            .iter()
            .map(|&o| self.tone_power(start, o))
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("at least two tones");

        // Early-late gate on the winning tone: if the symbol sits later
        // than we think, the late window catches more of it.
        let is_last = rx.total == Some(rx.symbols.len() + 1);
        let correction = if is_last {
            0.0
        } else {
            let early = self.tone_power(start - self.gate, rx.omegas[k]);
            let late = self.tone_power(start + self.gate, rx.omegas[k]);
            let e = (late - early) / (late + early + f64::MIN_POSITIVE);
            (e * self.gate_scale).clamp(-(self.gate as f64), self.gate as f64)
        };

        let spb = self.l.symbols_per_byte();
        let bits = self.l.bits_per_symbol;
        let peak_span = self.peak_span;
        let nominal = self.l.symbol as f64;
        let State::Receive(rx) = &mut self.state else {
            unreachable!("still receiving");
        };
        rx.symbols.push(k);
        // Noise can push the loop, but never past the clock ratios the
        // preamble search accepts.
        rx.period = (rx.period + LOOP_PERIOD_GAIN * correction).clamp(
            nominal / (1.0 + MAX_CLOCK_ERROR),
            nominal / (1.0 - MAX_CLOCK_ERROR),
        );
        rx.center += rx.period + LOOP_PHASE_GAIN * correction;

        if rx.total.is_none() && rx.symbols.len() == LENGTH_COPIES * spb {
            let copies = pack(&rx.symbols, bits);
            let len = majority(copies[0], copies[1], copies[2]) as usize;
            if !(MIN_FRAME..=MAX_FRAME).contains(&len) {
                let at = rx.peak + peak_span;
                self.state = State::Search { at };
                return None;
            }
            rx.total = Some((LENGTH_COPIES + len) * spb);
        }

        if rx.total == Some(rx.symbols.len()) {
            let frame = pack(&rx.symbols[LENGTH_COPIES * spb..], bits);
            // `center` is now where a next symbol would be, so the last one
            // ended half a period before it, and the next preamble can start
            // right there. Resume from the last symbol's centre, half a
            // symbol early, so neither timing error nor the small peak shift
            // of a slightly mismatched clock hypothesis can skip it.
            let at = (rx.center - rx.period).max(0.0) as usize;
            self.state = State::Search { at };
            return Some(frame);
        }
        None
    }

    /// Hann-windowed Goertzel power at angular frequency `omega` (radians
    /// per sample) over the window starting at absolute sample `start`.
    fn tone_power(&self, start: usize, omega: f64) -> f64 {
        let s = start - self.origin;
        goertzel(&self.buf[s..s + self.window.len()], &self.window, omega)
    }
}

/// Windowed Goertzel: the power of `x * window` at `omega`.
fn goertzel(x: &[f32], window: &[f64], omega: f64) -> f64 {
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for (&v, &w) in x.iter().zip(window) {
        let s = v as f64 * w + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

/// Timing error in samples per unit of normalised early-late output
/// `(late - early) / (late + early)`, measured on an ideal symbol so the
/// loop gains mean the same thing for every configuration.
fn gate_scale(l: &Layout, window: &[f64], gate: usize) -> f64 {
    // The envelope-weighted window sum for a window starting `offset`
    // samples into an isolated symbol. Tone energy scales with its square.
    let n = l.symbol as isize;
    let amplitude = |offset: isize| -> f64 {
        window
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let t = offset + i as isize;
                if (0..n).contains(&t) {
                    w * l.envelope(t as f64, l.symbol)
                } else {
                    0.0
                }
            })
            .sum()
    };
    let aligned = l.ramp as isize;
    let probe = (l.symbol / 40).max(1) as isize;
    // The symbol starts `probe` samples later than expected.
    let early = amplitude(aligned - gate as isize - probe).powi(2);
    let late = amplitude(aligned + gate as isize - probe).powi(2);
    probe as f64 * (late + early) / (late - early)
}

/// Pack symbols of `bits` bits each, high bits first, into bytes.
fn pack(symbols: &[usize], bits: usize) -> Vec<u8> {
    symbols
        .chunks(8 / bits)
        .map(|c| c.iter().fold(0u8, |acc, &s| (acc << bits) | s as u8))
        .collect()
}

/// Bitwise majority of three copies of a byte.
fn majority(a: u8, b: u8, c: u8) -> u8 {
    (a & b) | (a & c) | (b & c)
}

/// One RBJ second-order section, direct form I.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn high_pass(hz: f64, q: f64, fs: f64) -> Self {
        let (cos, alpha) = Self::prewarp(hz, q, fs);
        Self::normalised(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            cos,
            alpha,
        )
    }

    fn low_pass(hz: f64, q: f64, fs: f64) -> Self {
        let (cos, alpha) = Self::prewarp(hz, q, fs);
        Self::normalised(
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            cos,
            alpha,
        )
    }

    fn prewarp(hz: f64, q: f64, fs: f64) -> (f64, f64) {
        let w0 = TAU * hz / fs;
        (w0.cos(), w0.sin() / (2.0 * q))
    }

    fn normalised(b: [f64; 3], cos: f64, alpha: f64) -> Self {
        let a0 = 1.0 + alpha;
        Biquad {
            b: [b[0] / a0, b[1] / a0, b[2] / a0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majority_outvotes_one_bad_copy() {
        assert_eq!(majority(69, 69, 69), 69);
        assert_eq!(majority(69, 0xff, 69), 69);
        assert_eq!(majority(0x00, 69, 69), 69);
        // Different bits wrong in each copy still recover the value.
        assert_eq!(majority(69 ^ 0x01, 69 ^ 0x80, 69 ^ 0x10), 69);
    }

    #[test]
    fn symbols_round_trip_high_bits_first() {
        let l = Layout::new(&ModemConfig::default());
        let s: Vec<usize> = l.byte_symbols(0xa7).collect();
        assert_eq!(s, vec![0xa, 0x7]);
        assert_eq!(pack(&s, 4), vec![0xa7]);

        let l = Layout::new(&ModemConfig {
            tones: 4,
            ..Default::default()
        });
        let s: Vec<usize> = l.byte_symbols(0b10_01_11_00).collect();
        assert_eq!(s, vec![2, 1, 3, 0]);
        assert_eq!(pack(&s, 2), vec![0b10_01_11_00]);
    }

    #[test]
    fn tones_span_the_band() {
        let l = Layout::new(&ModemConfig::default());
        assert_eq!(l.tone_hz(0), 17_600.0);
        assert_eq!(l.tone_hz(15), 19_800.0);
    }
}
