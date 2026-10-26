//! Measures the light channel on real hardware, without the app.
//!
//! ```text
//! cargo run --release -p bombechat-light --example measure -- --show [slow|normal|fast]
//! cargo run --release -p bombechat-light --example measure -- --read [slow|normal|fast] [--seconds N]
//! cargo run --release -p bombechat-light --example measure -- --images DIR [--fps N]
//! cargo run --release -p bombechat-light --example measure -- --selftest
//! ```
//!
//! `--show` writes a page that flashes a fixed sequence of light codes
//! and opens it in the browser: six messages a preset, each shown for as
//! long as the app would show it, then the next preset (or only the one
//! named). Every frame is drawn by the crate's own renderer. The page is
//! written to the temporary folder; `MEASURE_NO_OPEN=1` leaves it unopened.
//!
//! `--read` points the camera at that page (on a second screen, another
//! Mac, or a printout of a frame) for N seconds (30 unless given) and
//! prints one line a preset: camera pictures with a code in view, those
//! that read a frame, the symbols and bytes a second collected while a
//! message was coming in, and how long the first message took from its
//! first frame read to its last symbol.
//! The messages are known, so each one completed is checked byte for byte.
//!
//! `--images DIR` reads a folder of PNG photos (or screenshots) of the
//! page in name order, as if a camera took them N a second (30 unless
//! `--fps` says otherwise), and prints the same lines.
//!
//! `--selftest` draws the whole sequence, photographs nothing, and puts
//! each picture straight through the reader and the collector at a
//! simulated 30 pictures a second. It fails (exit 1) unless every message
//! of every preset comes back exactly. `cargo test` runs it too.
//!
//! This is an example, not part of the app: nothing here is built into
//! Bombechat, and its PNG reading and writing comes from a dev-dependency, which the
//! airgapped check (normal dependencies only) does not follow.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bombechat_light::{encode, render, CodeSpec, Collector, LightEvent, ReadOutcome, Reader};
use image::{GrayImage, ImageFormat, Luma};

/// Messages in each preset's run.
const MESSAGES: u16 = 6;
/// Bytes in each message: a mid-sized envelope.
const ENVELOPE: usize = 1500;
/// As the node shows an envelope: three times the frames a camera needs,
/// and never under four seconds (crates/node/src/light.rs `window`).
const SEND_WINDOW: u32 = 3;
const HUMAN_FLOOR_MS: u32 = 4_000;
/// The camera rate the self-test and `--images` assume.
const CAMERA_FPS: u32 = 30;
const NAMES: [&str; 3] = ["slow", "normal", "fast"];

/// The message for `tag`, the same every run, so a reader can check it.
fn envelope(tag: u16) -> Vec<u8> {
    let mut x = 0x2545_f491_u32 ^ (tag as u32).wrapping_mul(0x9e37_79b9);
    (0..ENVELOPE)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Tags of preset `p`'s messages: 1 to 6 slow, 17 to 22 normal, 33 to 38 fast.
fn tags(p: usize) -> impl Iterator<Item = u16> {
    (1..=MESSAGES).map(move |k| p as u16 * 16 + k)
}

/// Frames to show one message for, as the node reckons it.
fn window(spec: &CodeSpec) -> u32 {
    let per_frame = spec.packets_per_frame().max(1) as u32;
    let needed = spec.source_symbols(ENVELOPE).div_ceil(per_frame).max(1);
    let floor = (HUMAN_FLOOR_MS * spec.frames_per_second()).div_ceil(1000);
    (needed * SEND_WINDOW).max(floor)
}

/// Preset `p`'s run: each message's frames, in the order shown.
fn sequence(p: usize) -> Vec<(u16, Vec<u8>)> {
    let spec = CodeSpec::presets()[p];
    tags(p)
        .flat_map(|tag| {
            let mut encoder = encode(&spec, &envelope(tag), tag);
            (0..window(&spec)).map(move |_| (tag, encoder.next_frame()))
        })
        .collect()
}

/// The bytes a second a camera reading every frame of preset `p` takes
/// in: what the app's "Bytes a second" readout shows for it.
fn full_rate(p: usize) -> f64 {
    let spec = CodeSpec::presets()[p];
    (spec.frames_per_second() as usize * spec.packets_per_frame() * spec.symbol_size as usize)
        as f64
}

#[derive(Default, Clone)]
struct PresetTally {
    /// Pictures with a code of this preset in view.
    seen: u32,
    /// Pictures that read a frame of it that passed its check.
    ok: u32,
    messages: u32,
    /// From a message's first frame read to its last byte.
    first_message_ms: Option<u64>,
}

/// One message as it comes in.
struct Taking {
    preset: usize,
    /// When its first frame read.
    started: u64,
    /// When its latest symbol came, or when it was all in.
    last_ms: u64,
    /// Distinct symbols in hand.
    got: u32,
    done: bool,
}

/// What a run of pictures came to, by preset.
struct Tally {
    per: [PresetTally; 3],
    /// The preset every code is counted against (`--read normal`), if one.
    only: Option<usize>,
    /// The last preset read: unreadable codes count against it.
    last: Option<usize>,
    /// Unreadable codes before any preset was read.
    unplaced: u32,
    collector: Collector,
    taking: HashMap<u16, Taking>,
    /// Messages that completed with the wrong bytes: never, unless broken.
    wrong: u32,
    pictures: u32,
}

impl Tally {
    fn new(only: Option<usize>) -> Self {
        Tally {
            per: Default::default(),
            only,
            last: None,
            unplaced: 0,
            collector: Collector::new(),
            taking: HashMap::new(),
            wrong: 0,
            pictures: 0,
        }
    }

    fn done(&self, tag: u16) -> bool {
        self.taking.get(&tag).is_some_and(|t| t.done)
    }

    fn picture(&mut self, outcome: &ReadOutcome, now_ms: u64) {
        self.pictures += 1;
        match outcome {
            ReadOutcome::NoCode => self.collector.expire(now_ms),
            ReadOutcome::Unreadable => match self.only.or(self.last) {
                Some(p) => self.per[p].seen += 1,
                None => self.unplaced += 1,
            },
            ReadOutcome::Frames(frames) => {
                let mut seen = [false; 3];
                let mut ok = [false; 3];
                for bytes in frames {
                    let Some(p) = CodeSpec::presets()
                        .iter()
                        .position(|s| s.frame_bytes() == bytes.len())
                    else {
                        continue;
                    };
                    seen[p] = true;
                    self.last = Some(p);
                    for event in self.collector.push(bytes, now_ms) {
                        match event {
                            LightEvent::FrameOk { tag, got, .. } => {
                                ok[p] = true;
                                let t = self.taking.entry(tag).or_insert(Taking {
                                    preset: p,
                                    started: now_ms,
                                    last_ms: now_ms,
                                    got: 0,
                                    done: false,
                                });
                                if !t.done && got > t.got {
                                    t.got = got;
                                    t.last_ms = now_ms;
                                }
                            }
                            LightEvent::Complete { tag, bytes } => {
                                // collected afresh from here on, so the frames that keep
                                // coming still show whether they read
                                self.collector.reject(tag);
                                let Some(t) = self.taking.get_mut(&tag).filter(|t| !t.done) else {
                                    continue;
                                };
                                t.done = true;
                                t.last_ms = now_ms;
                                let took = now_ms - t.started;
                                if bytes != envelope(tag) {
                                    self.wrong += 1;
                                }
                                let t = &mut self.per[p];
                                t.messages += 1;
                                t.first_message_ms.get_or_insert(took);
                            }
                            LightEvent::FrameBad => {}
                        }
                    }
                }
                for p in 0..3 {
                    self.per[p].seen += seen[p] as u32;
                    self.per[p].ok += ok[p] as u32;
                }
            }
        }
    }

    /// One line for preset `p`. Symbols a second count only while a
    /// message is coming in: from its first frame read to the end of the
    /// frame its last symbol came in.
    fn line(&self, p: usize) -> String {
        let t = &self.per[p];
        let spec = CodeSpec::presets()[p];
        let frame_ms = 1000.0 / spec.frames_per_second() as f64;
        let (symbols, ms) =
            self.taking
                .values()
                .filter(|m| m.preset == p)
                .fold((0.0, 0.0), |(s, ms), m| {
                    (
                        s + m.got as f64,
                        ms + (m.last_ms - m.started) as f64 + frame_ms,
                    )
                });
        let sps = if ms > 0.0 { symbols * 1000.0 / ms } else { 0.0 };
        let share = (100 * t.ok).checked_div(t.seen).unwrap_or(0);
        let first = t
            .first_message_ms
            .map(|ms| format!("{:.1} s", ms as f64 / 1000.0))
            .unwrap_or_else(|| "none".into());
        format!(
            "{:<6}  frames seen {:>5}  frames ok {:>5} ({:>3}%)  symbols/s {:>5.1}  bytes/s {:>5.0} of {:.0}  first message {:>7}  messages {}",
            NAMES[p],
            t.seen,
            t.ok,
            share,
            sps,
            sps * spec.symbol_size as f64,
            full_rate(p),
            first,
            t.messages
        )
    }

    fn print(&self, presets: &[usize]) {
        for &p in presets {
            println!("{}", self.line(p));
        }
        if self.unplaced > 0 {
            println!("codes seen but never read: {}", self.unplaced);
        }
        let seen: u32 = presets.iter().map(|&p| self.per[p].seen).sum::<u32>() + self.unplaced;
        let ok: u32 = presets.iter().map(|&p| self.per[p].ok).sum();
        if self.pictures > 0 && seen == 0 {
            println!(
                "no code in view in {} pictures: aim the camera at the code",
                self.pictures
            );
        } else if seen >= 10 && ok * 2 < seen {
            println!("most codes seen were not read: tilt or dim the screen, or come closer");
        }
        if self.wrong > 0 {
            println!(
                "WRONG: {} messages came back with the wrong bytes",
                self.wrong
            );
        }
    }
}

fn preset_arg(arg: &str) -> Option<usize> {
    NAMES.iter().position(|n| n.eq_ignore_ascii_case(arg))
}

fn usage() -> ! {
    eprintln!(
        "measure --show [slow|normal|fast] | --read [slow|normal|fast] [--seconds N] | --images DIR [--fps N] | --selftest"
    );
    std::process::exit(2)
}

fn flag(args: &[String], name: &str) -> Option<u64> {
    let i = args.iter().position(|a| a == name)?;
    Some(
        args.get(i + 1)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| usage()),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let presets = |i: usize| -> Vec<usize> {
        match args.get(i).filter(|a| !a.starts_with("--")) {
            None => vec![0, 1, 2],
            Some(a) => vec![preset_arg(a).unwrap_or_else(|| usage())],
        }
    };
    match args.first().map(String::as_str) {
        Some("--show") => show(&presets(1)),
        Some("--read") => read_camera(&presets(1), flag(&args, "--seconds").unwrap_or(30)),
        Some("--images") => {
            let dir = args.get(1).unwrap_or_else(|| usage());
            read_images(
                Path::new(dir),
                flag(&args, "--fps").unwrap_or(CAMERA_FPS as u64),
            );
        }
        Some("--selftest") => {
            let tally = selftest();
            tally.print(&[0, 1, 2]);
            if let Err(why) = selftest_passed(&tally) {
                println!("selftest failed: {why}");
                std::process::exit(1);
            }
            println!("selftest passed");
        }
        _ => usage(),
    }
}

/// A clean picture of preset `p`'s `frame`, drawn by the renderer at
/// `cell` pixels a cell in the middle of a mid-grey picture `side` big.
fn photograph(p: usize, frame: &[u8], cell: u32, side: (u32, u32)) -> GrayImage {
    let spec = CodeSpec::presets()[p];
    let px = (spec.cells as u32 + 6) * cell;
    let code = render(&spec, frame, px);
    let mut picture = GrayImage::from_pixel(side.0, side.1, Luma([128]));
    let (x0, y0) = ((side.0 - px) / 2, (side.1 - px) / 2);
    for (x, y, v) in code.enumerate_pixels() {
        picture.put_pixel(x0 + x, y0 + y, *v);
    }
    picture
}

/// Every preset's run, each code frame photographed for as many pictures
/// as a 30 a second camera takes of it, straight through the reader. A
/// message stops showing once it is in, as when the receiver says so.
fn selftest() -> Tally {
    let mut tally = Tally::new(None);
    // an unoptimised build reads slowly; the clock here is simulated
    let mut reader = Reader::with_budget(Duration::from_secs(5));
    let mut n = 0u64;
    for p in 0..3 {
        let fps = CodeSpec::presets()[p].frames_per_second();
        let per_frame = CAMERA_FPS.div_ceil(fps);
        for (tag, frame) in sequence(p) {
            if tally.done(tag) {
                continue;
            }
            let picture = photograph(p, &frame, 4, (640, 400));
            for _ in 0..per_frame {
                let outcome = reader.read(&picture);
                tally.picture(&outcome, n * 1000 / CAMERA_FPS as u64);
                n += 1;
            }
        }
    }
    tally
}

fn selftest_passed(tally: &Tally) -> Result<(), String> {
    if tally.wrong > 0 {
        return Err(format!("{} messages came back wrong", tally.wrong));
    }
    for (name, t) in NAMES.iter().zip(&tally.per) {
        if t.messages != MESSAGES as u32 {
            return Err(format!("{name}: {} of {MESSAGES} messages", t.messages));
        }
        if t.ok != t.seen {
            return Err(format!("{name}: {} of {} pictures read", t.ok, t.seen));
        }
    }
    Ok(())
}

fn read_images(dir: &Path, fps: u64) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| {
            eprintln!("{}: {e}", dir.display());
            std::process::exit(1)
        })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    let (pngs, others): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")));
    if !others.is_empty() {
        println!(
            "skipped {} files that are not PNG (convert them with: sips -s format png FILE --out DIR)",
            others.len()
        );
    }
    let fps = fps.max(1);
    let mut tally = Tally::new(None);
    let mut reader = Reader::with_budget(Duration::from_millis(500));
    for (i, path) in pngs.iter().enumerate() {
        let picture = match image::open(path) {
            Ok(p) => p.into_luma8(),
            Err(e) => {
                println!("{}: {e}", path.display());
                continue;
            }
        };
        let outcome = reader.read(&picture);
        let said = match &outcome {
            ReadOutcome::NoCode => "no code".to_string(),
            ReadOutcome::Unreadable => "a code, not read".to_string(),
            ReadOutcome::Frames(f) => format!("{} frame(s) read", f.len()),
        };
        println!(
            "{}: {said}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        tally.picture(&outcome, i as u64 * 1000 / fps);
    }
    println!("{} pictures, timed as {fps} a second", pngs.len());
    tally.print(&[0, 1, 2]);
}

#[cfg(target_os = "macos")]
fn read_camera(presets: &[usize], seconds: u64) {
    use std::sync::mpsc;
    use std::time::Instant;

    use bombechat_light::{AvCamera, CameraAccess, CameraIo};

    match AvCamera::authorization_status() {
        Ok(CameraAccess::Denied) => {
            println!("camera access denied: allow Terminal in System Settings, Privacy and Security, Camera, then run this again");
            std::process::exit(1)
        }
        Ok(CameraAccess::NotAsked) => println!("macOS will ask for camera access: allow it"),
        Ok(CameraAccess::Allowed) => {}
        Err(e) => {
            println!("camera: {e}");
            std::process::exit(1)
        }
    }
    let (tx, rx) = mpsc::channel::<GrayImage>();
    let mut camera = AvCamera::new();
    if let Err(e) = camera.start(Box::new(move |picture| {
        let _ = tx.send(picture);
    })) {
        println!("camera: {e}");
        std::process::exit(1)
    }
    println!("reading for {seconds} s: aim the camera at the code");
    let only = (presets.len() == 1).then(|| presets[0]);
    let mut tally = Tally::new(only);
    let mut reader = Reader::new();
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    let mut skipped = 0u32;
    let mut next_note = 5;
    while Instant::now() < end {
        let Ok(mut picture) = rx.recv_timeout(Duration::from_millis(200)) else {
            if let Some(e) = camera.take_error() {
                println!("camera: {e}");
                break;
            }
            continue;
        };
        // a reader that falls behind reads the newest picture
        while let Ok(newer) = rx.try_recv() {
            picture = newer;
            skipped += 1;
        }
        let outcome = reader.read(&picture);
        let now_ms = start.elapsed().as_millis() as u64;
        tally.picture(&outcome, now_ms);
        if now_ms / 1000 >= next_note {
            next_note += 5;
            let ok: u32 = tally.per.iter().map(|t| t.ok).sum();
            let messages: u32 = tally.per.iter().map(|t| t.messages).sum();
            println!(
                "{} s: {} pictures, {ok} read, {messages} messages",
                now_ms / 1000,
                tally.pictures
            );
        }
    }
    camera.stop();
    println!(
        "{} pictures in {} s ({} skipped while reading)",
        tally.pictures,
        start.elapsed().as_secs(),
        skipped
    );
    tally.print(presets);
}

#[cfg(not(target_os = "macos"))]
fn read_camera(_: &[usize], _: u64) {
    eprintln!("--read needs the macOS camera; use --images on this machine");
    std::process::exit(1)
}

/// Standard base64, for the frames in the page.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The page for `--show`: each frame as the renderer draws it at one
/// pixel a cell (a PNG), scaled up by a whole number of screen pixels.
fn show_page(presets: &[usize]) -> String {
    let runs: Vec<String> = presets
        .iter()
        .map(|&p| {
            let spec = CodeSpec::presets()[p];
            let px = spec.cells as u32 + 6;
            let frames: Vec<String> = sequence(p)
                .iter()
                .map(|(_, frame)| {
                    let mut png = Cursor::new(Vec::new());
                    render(&spec, frame, px)
                        .write_to(&mut png, ImageFormat::Png)
                        .expect("a PNG in memory");
                    format!("\"{}\"", base64(png.get_ref()))
                })
                .collect();
            format!(
                "{{name:\"{}\",label:\"{}\",cells:{},fps:{},per:{},px:{px},frames:[{}]}}",
                NAMES[p],
                ["Slow", "Normal", "Fast"][p],
                spec.cells,
                spec.frames_per_second(),
                window(&spec),
                frames.join(",")
            )
        })
        .collect();
    SHOW_PAGE.replace("/*RUNS*/", &runs.join(","))
}

/// The still state and the surround follow the app's flashing view: a
/// mid-grey surround that never changes, and a grey field the code's own
/// mean before frames start, so nothing flashes full-field. The code
/// takes about 76 percent of the window's height, as the app's does
/// (682 of 900 CSS pixels on a 1440x900 screen), so distances measured
/// here hold for the app.
const SHOW_PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Bombechat light measure</title>
<style>
html,body{margin:0;height:100%;background:#8d887a;color:#1d1b17;font:15px/1.4 -apple-system,system-ui,sans-serif;overflow:hidden}
canvas{position:absolute;left:0;top:0}
#say{position:fixed;left:16px;right:16px;bottom:12px}
</style></head><body>
<canvas id="c"></canvas><div id="say"></div>
<script>
const RUNS=[/*RUNS*/];
const READY_MS=1200,cv=document.getElementById('c'),k=cv.getContext('2d',{alpha:false}),say=document.getElementById('say');
let run=0,from=null,playing=true,imgs=RUNS.map(r=>r.frames.map(b=>{const i=new Image();i.src='data:image/png;base64,'+b;return i}));
function fit(){const r=RUNS[run],dpr=devicePixelRatio||1,room=Math.min(innerWidth-32,innerHeight*.76)*dpr;
  const side=Math.max(1,Math.floor(room/r.px))*r.px;cv.width=cv.height=side;cv.style.width=cv.style.height=side/dpr+'px';
  cv.style.left=Math.round((innerWidth*dpr-side)/2)/dpr+'px';cv.style.top=Math.round((innerHeight*dpr-40*dpr-side)/2)/dpr+'px'}
function still(){const r=RUNS[run],c=cv.width/r.px;k.fillStyle='#fff';k.fillRect(0,0,cv.width,cv.width);k.fillStyle='#bcbcbc';k.fillRect(3*c,3*c,r.cells*c,r.cells*c)}
function tick(t){const r=RUNS[run];
  if(!playing){still();say.textContent=`${r.label}, stopped. Space starts it again, 1 2 3 choose slow, normal, fast.`;return requestAnimationFrame(tick)}
  if(from===null)from=t;
  const since=t-from-READY_MS;
  if(since<0){still();say.textContent=`${r.label}, ${r.cells} cells at ${r.fps} frames a second: ready. Escape stops it.`;return requestAnimationFrame(tick)}
  let i=Math.floor(since*r.fps/1000);
  if(i>=r.frames.length){run=(run+1)%RUNS.length;from=t;fit();still();return requestAnimationFrame(tick)}
  k.imageSmoothingEnabled=false;k.drawImage(imgs[run][i],0,0,cv.width,cv.width);
  say.textContent=`${r.label}, ${r.cells} cells at ${r.fps} frames a second. Message ${Math.floor(i/r.per)+1} of ${r.frames.length/r.per}, frame ${i%r.per+1} of ${r.per}. Escape stops it.`;
  requestAnimationFrame(tick)}
addEventListener('resize',fit);
addEventListener('keydown',e=>{if(e.key==='Escape')playing=false;else if(e.key===' '){playing=true;from=null}
  else if('123'.includes(e.key)){const j=RUNS.findIndex(r=>r.name===['slow','normal','fast'][+e.key-1]);if(j>=0){run=j;from=null;playing=true;fit()}}});
fit();requestAnimationFrame(tick);
</script></body></html>
"##;

fn show(presets: &[usize]) {
    let path = std::env::temp_dir().join("bombechat-light-measure.html");
    std::fs::write(&path, show_page(presets)).unwrap_or_else(|e| {
        eprintln!("{}: {e}", path.display());
        std::process::exit(1)
    });
    println!("wrote {}", path.display());
    for &p in presets {
        let spec = CodeSpec::presets()[p];
        println!(
            "{:<6}  {} cells at {} frames a second, {} messages of {ENVELOPE} bytes, {} frames each",
            NAMES[p],
            spec.cells,
            spec.frames_per_second(),
            MESSAGES,
            window(&spec)
        );
    }
    if std::env::var_os("MEASURE_NO_OPEN").is_none() {
        let _ = std::process::Command::new("open").arg(&path).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selftest_reads_every_message_of_every_preset() {
        let tally = selftest();
        tally.print(&[0, 1, 2]);
        assert_eq!(selftest_passed(&tally), Ok(()));
        for p in 0..3 {
            assert!(tally.per[p].first_message_ms.is_some());
        }
    }

    #[test]
    fn the_show_page_carries_every_frame_as_the_renderer_draws_it() {
        let page = show_page(&[1]);
        let frames = sequence(1);
        assert_eq!(
            frames.len(),
            MESSAGES as usize * window(&CodeSpec::normal()) as usize
        );
        assert_eq!(page.matches("\",\"").count() + 1, frames.len());
        let spec = CodeSpec::normal();
        let mut png = Cursor::new(Vec::new());
        render(&spec, &frames[0].1, spec.cells as u32 + 6)
            .write_to(&mut png, ImageFormat::Png)
            .unwrap();
        assert!(page.contains(&base64(png.get_ref())));
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
