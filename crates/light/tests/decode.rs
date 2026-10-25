//! Reading light codes out of synthetic camera pictures.
//!
//! Pictures come from `camera` (a posed screen in a cluttered room, lens
//! blur, noise, moire, gamma, JPEG and so on), drawn fresh from seeded
//! randomness, so every run sees the same pictures. The full 200-sample
//! sweep and the timing run are slow and only meaningful in release:
//!
//! ```text
//! cargo test --release -p bombechat-light --test decode -- --ignored --nocapture
//! ```

mod camera;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bombechat_light::{
    encode, read_within, CameraIo, CodeSpec, Collector, FakeCamera, Layout, LightEvent,
    ReadOutcome, Reader,
};
use camera::textures::{
    blinds, box_blur, checkerboard, diagonal_stripes, dotted_lattice, fake_corners,
    fake_corners_on_checks, finder_bars, finder_crosses, finder_lattice, finder_stripes, irregular,
    keyboard, qr_like, qr_wall, scattered_finders, IRREGULAR,
};
use camera::{in_view, shoot, Camera, Code, Noise, Pose};
use image::GrayImage;

/// A budget no test picture comes near, so what these tests see does not
/// depend on how busy the machine is (they run on every core at once, and
/// on efficiency cores a picture takes about three times as long). The
/// default budget is tested on its own, in `decode_time_per_frame`.
const UNHURRIED: Duration = Duration::from_secs(10);

/// [`bombechat_light::read`] without its time budget (see [`UNHURRIED`]).
fn read(image: &GrayImage) -> ReadOutcome {
    read_within(image, UNHURRIED)
}

fn presets() -> [(&'static str, CodeSpec); 3] {
    [
        ("slow", CodeSpec::slow()),
        ("normal", CodeSpec::normal()),
        ("fast", CodeSpec::fast()),
    ]
}

/// A drawable frame of `spec` from a random envelope.
fn some_frame(spec: &CodeSpec, rng: &mut Noise) -> Vec<u8> {
    let len = rng.range(40.0, 1200.0) as usize;
    let envelope: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
    let mut enc = encode(spec, &envelope, rng.next() as u16);
    for _ in 0..(rng.next() % 4) {
        enc.next_frame();
    }
    enc.next_frame()
}

fn code(spec: &CodeSpec, frame: &[u8], pose: Pose) -> Code {
    Code {
        n: spec.cells as usize,
        grid: Layout::new(spec).grid(frame),
        pose,
        occlude: None,
    }
}

/// A picture condition: changes the everyday baseline in one way (or, for
/// "everything", in all of them at once).
struct Condition {
    name: &'static str,
    /// The share the normal preset must read: 98 percent for the
    /// conditions the brief lists, 95 percent for the harsher camera
    /// effects at moderate strength; `None` is reported only.
    bar: Option<f64>,
    apply: fn(&mut Noise, &mut Pose, &mut Camera),
}

/// An everyday shot: the code about half the frame high near the middle,
/// a slight roll and tilt, mild lens blur and noise, mild uneven light.
fn baseline(rng: &mut Noise) -> (Pose, Camera) {
    let pose = Pose {
        scale: rng.range(0.4, 0.6),
        centre: (rng.range(0.4, 0.6), rng.range(0.45, 0.55)),
        roll: rng.range(-3.0, 3.0),
        tilt: rng.range(0.0, 8.0),
        tilt_axis: rng.range(0.0, 360.0),
    };
    let cam = Camera {
        gradient_angle: rng.range(0.0, 360.0),
        ..Camera::default()
    };
    (pose, cam)
}

fn conditions() -> Vec<Condition> {
    vec![
        Condition {
            name: "baseline",
            bar: Some(0.98),
            apply: |_, _, _| {},
        },
        Condition {
            name: "rotation ±20°",
            bar: Some(0.98),
            apply: |r, p, _| p.roll = r.range(-20.0, 20.0),
        },
        Condition {
            name: "perspective 15-30°",
            bar: Some(0.98),
            apply: |r, p, _| p.tilt = r.range(15.0, 30.0),
        },
        Condition {
            name: "blur σ1.5",
            bar: Some(0.98),
            apply: |_, _, c| c.blur = 1.5,
        },
        Condition {
            name: "noise σ8",
            bar: Some(0.98),
            apply: |_, _, c| c.noise = 8.0,
        },
        Condition {
            name: "brightness gradient",
            bar: Some(0.98),
            apply: |_, _, c| {
                c.gradient = 0.6;
                c.vignette = 0.35;
            },
        },
        Condition {
            name: "gamma 0.5-2.2",
            bar: Some(0.98),
            apply: |r, _, c| {
                c.gamma = r.range(0.5, 2.2);
                c.black = r.range(0.05, 0.25);
                c.white = r.range(0.7, 0.95);
            },
        },
        Condition {
            name: "moiré",
            bar: Some(0.98),
            apply: |r, _, c| {
                c.moire = 0.15;
                c.moire_period = r.range(2.2, 4.0);
                c.moire_angle = r.range(0.0, 180.0);
            },
        },
        Condition {
            name: "scale 25-100%",
            bar: Some(0.98),
            apply: |r, p, _| {
                // Square on, so the top of the range (the quiet zone
                // filling the frame height) fits.
                p.scale = r.range(0.25, 1.0);
                p.centre = (0.5, 0.5);
                p.roll = 0.0;
                p.tilt = 0.0;
            },
        },
        Condition {
            name: "off-centre",
            bar: Some(0.98),
            apply: |r, p, _| {
                p.scale = r.range(0.3, 0.45);
                let edge = |r: &mut Noise| {
                    if r.unit() < 0.5 {
                        r.range(0.12, 0.25)
                    } else {
                        r.range(0.75, 0.88)
                    }
                };
                p.centre = (edge(r), r.range(0.22, 0.78));
            },
        },
        Condition {
            name: "JPEG q60-80",
            bar: Some(0.98),
            apply: |r, _, c| c.jpeg = Some(r.range(60.0, 80.0) as u8),
        },
        Condition {
            name: "mirrored",
            bar: Some(0.98),
            apply: |r, p, c| {
                c.mirror = true;
                p.roll = r.range(-10.0, 10.0);
            },
        },
        Condition {
            name: "banding moiré 10-30%",
            bar: Some(0.95),
            apply: |r, _, c| {
                c.band = r.range(0.1, 0.3);
                c.band_period = r.range(20.0, 300.0);
                c.band_angle = r.range(0.0, 180.0);
            },
        },
        Condition {
            name: "overexposed 1.2-1.6",
            bar: Some(0.95),
            apply: |r, _, c| {
                c.white = r.range(1.2, 1.6);
                c.black = r.range(0.08, 0.2);
                c.bloom = 1.0;
            },
        },
        Condition {
            name: "ISP sharpening",
            bar: Some(0.95),
            apply: |r, _, c| {
                c.sharpen = r.range(0.5, 1.5);
                c.noise = 4.0;
            },
        },
        Condition {
            name: "shot noise and grain",
            bar: Some(0.95),
            apply: |r, _, c| {
                c.shot = r.range(0.03, 0.06);
                c.grain = r.range(2.0, 4.0);
            },
        },
        Condition {
            name: "hand shake",
            bar: None,
            apply: |r, _, c| {
                c.motion = r.range(2.0, 5.0);
                c.motion_angle = r.range(0.0, 180.0);
            },
        },
        Condition {
            name: "dim",
            bar: None,
            apply: |r, _, c| {
                c.black = 0.03;
                c.white = r.range(0.22, 0.3);
                c.noise = 5.0;
            },
        },
        Condition {
            name: "everything",
            bar: None,
            apply: |r, p, c| {
                p.roll = r.range(-20.0, 20.0);
                p.tilt = r.range(0.0, 30.0);
                p.scale = r.range(0.3, 0.8);
                p.centre = (r.range(0.3, 0.7), r.range(0.35, 0.65));
                c.blur = r.range(0.8, 1.5);
                c.noise = 5.0;
                c.gradient = 0.4;
                c.gamma = r.range(0.7, 1.6);
                c.moire = 0.08;
                c.moire_period = r.range(2.2, 4.0);
                c.moire_angle = r.range(0.0, 180.0);
                c.jpeg = Some(75);
                c.mirror = r.unit() < 0.5;
                c.band = r.range(0.0, 0.15);
                c.band_period = r.range(20.0, 300.0);
                c.band_angle = r.range(0.0, 180.0);
                c.shot = 0.03;
                c.grain = 2.0;
                c.sharpen = r.range(0.0, 1.0);
            },
        },
        Condition {
            name: "lens distortion",
            bar: None,
            apply: |r, p, c| {
                c.k1 = r.range(-0.15, 0.05);
                p.scale = r.range(0.5, 0.8);
                p.centre = (r.range(0.3, 0.7), r.range(0.4, 0.6));
            },
        },
        // Beyond what the presets promise, to show where reading gives
        // out; reported, not held to a bar.
        Condition {
            name: "beyond: banding 30-60%, period 10-60",
            bar: None,
            apply: |r, _, c| {
                c.band = r.range(0.3, 0.6);
                c.band_period = r.range(10.0, 60.0);
                c.band_angle = r.range(0.0, 180.0);
            },
        },
        Condition {
            name: "beyond: overexposed 2-3",
            bar: None,
            apply: |r, _, c| {
                c.white = r.range(2.0, 3.0);
                c.black = r.range(0.15, 0.3);
                c.bloom = 1.0;
            },
        },
        Condition {
            name: "beyond: sharpening 2-3",
            bar: None,
            apply: |r, _, c| {
                c.sharpen = r.range(2.0, 3.0);
                c.noise = 4.0;
            },
        },
        Condition {
            name: "beyond: shot noise 0.1, grain 6",
            bar: None,
            apply: |_, _, c| {
                c.shot = 0.1;
                c.grain = 6.0;
            },
        },
        Condition {
            name: "beyond: any rotation",
            bar: None,
            apply: |r, p, _| p.roll = r.range(0.0, 360.0),
        },
        Condition {
            name: "beyond: perspective 30-45°",
            bar: None,
            apply: |r, p, _| p.tilt = r.range(30.0, 45.0),
        },
        Condition {
            name: "beyond: blur σ2.5",
            bar: None,
            apply: |_, _, c| c.blur = 2.5,
        },
        Condition {
            name: "beyond: noise σ16",
            bar: None,
            apply: |_, _, c| c.noise = 16.0,
        },
        Condition {
            name: "beyond: 25-35% and blur σ1.5",
            bar: None,
            apply: |r, p, c| {
                p.scale = r.range(0.25, 0.35);
                c.blur = 1.5;
            },
        },
    ]
}

/// What happened to one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Result {
    Read,
    Unreadable,
    Missed,
    /// A frame other than the one shown: must never happen.
    Wrong,
}

/// Draw sample `i` of a condition for a preset and read it back.
fn trial(spec: &CodeSpec, cond: &Condition, cond_index: usize, i: u64) -> Result {
    let mut rng = Noise::new(
        (spec.cells as u64) << 48 ^ (cond_index as u64) << 32 ^ i ^ 0x6c69_6768_7400_0000,
    );
    let frame = some_frame(spec, &mut rng);
    let mut placed = None;
    for _ in 0..50 {
        let (mut pose, mut cam) = baseline(&mut rng);
        (cond.apply)(&mut rng, &mut pose, &mut cam);
        let c = code(spec, &frame, pose);
        if in_view(&c, &cam) {
            placed = Some((c, cam));
            break;
        }
    }
    let (c, cam) = placed.expect("a pose that fits the frame");
    let img = shoot(&[c], &cam, rng.next());
    match read(&img) {
        ReadOutcome::Frames(frames) if frames == vec![frame] => Result::Read,
        ReadOutcome::Frames(_) => Result::Wrong,
        ReadOutcome::Unreadable => Result::Unreadable,
        ReadOutcome::NoCode => Result::Missed,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    read: u32,
    unreadable: u32,
    missed: u32,
    wrong: u32,
}

impl Tally {
    fn add(&mut self, r: Result) {
        match r {
            Result::Read => self.read += 1,
            Result::Unreadable => self.unreadable += 1,
            Result::Missed => self.missed += 1,
            Result::Wrong => self.wrong += 1,
        }
    }

    fn total(&self) -> u32 {
        self.read + self.unreadable + self.missed + self.wrong
    }

    fn rate(&self) -> f64 {
        self.read as f64 / self.total().max(1) as f64
    }
}

/// Run `samples` trials of every condition on every preset, in parallel.
fn sweep(samples: u64) -> Vec<(usize, usize, Tally)> {
    let conds = conditions();
    // BOMBECHAT_SWEEP_ONLY=text runs only the conditions whose name has it.
    let only = std::env::var("BOMBECHAT_SWEEP_ONLY").unwrap_or_default();
    let jobs: Vec<(usize, usize)> = (0..3)
        .flat_map(|p| (0..conds.len()).map(move |c| (p, c)))
        .filter(|&(_, c)| conds[c].name.contains(only.as_str()))
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let j = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&(p, c)) = jobs.get(j) else { break };
                let spec = presets()[p].1;
                let mut tally = Tally::default();
                for i in 0..samples {
                    tally.add(trial(&spec, &conds[c], c, i));
                }
                results.lock().unwrap().push((p, c, tally));
            });
        }
    });
    let mut out = results.into_inner().unwrap();
    out.sort_by_key(|&(p, c, _)| (c, p));
    out
}

fn table(results: &[(usize, usize, Tally)]) -> String {
    let conds = conditions();
    let mut s = String::from("| condition | slow | normal | fast |\n|---|---|---|---|\n");
    for (c, cond) in conds.iter().enumerate() {
        if !results.iter().any(|r| r.1 == c) {
            continue;
        }
        s += &format!("| {} |", cond.name);
        for p in 0..3 {
            let Some(t) = results.iter().find(|r| r.0 == p && r.1 == c).map(|r| r.2) else {
                continue;
            };
            s += &format!(
                " {:.1}% ({} unreadable, {} missed{}) |",
                100.0 * t.rate(),
                t.unreadable,
                t.missed,
                if t.wrong > 0 {
                    format!(", {} WRONG", t.wrong)
                } else {
                    String::new()
                }
            );
        }
        s += "\n";
    }
    s
}

/// Hold each condition to its bar, less `slack` for small samples.
fn check_sweep(results: &[(usize, usize, Tally)], slack: f64) {
    let conds = conditions();
    for &(p, c, t) in results {
        assert_eq!(
            t.wrong,
            0,
            "{} {}: wrong frames delivered",
            presets()[p].0,
            conds[c].name
        );
        if let (1, Some(bar)) = (p, conds[c].bar) {
            let bar = bar - slack;
            assert!(
                t.rate() >= bar,
                "normal, {}: {:.1}% read, under {:.0}%",
                conds[c].name,
                100.0 * t.rate(),
                100.0 * bar
            );
        }
    }
}

/// A quick version of the sweep for every `cargo test`: 20 samples per
/// condition per preset.
#[test]
fn reads_codes_under_camera_conditions() {
    let results = sweep(20);
    println!("{}", table(&results));
    check_sweep(&results, 0.05);
}

/// The full sweep: 200 seeded samples per condition per preset, at least
/// 98 percent read on the normal preset for each listed condition.
#[test]
#[ignore = "slow; run in release with --ignored"]
fn success_rates_over_200_samples() {
    let results = sweep(200);
    println!("{}", table(&results));
    check_sweep(&results, 0.0);
}

#[test]
fn a_mirrored_picture_reads() {
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(11);
    let frame = some_frame(&spec, &mut rng);
    let cam = Camera {
        mirror: true,
        ..Camera::default()
    };
    let img = shoot(
        &[code(
            &spec,
            &frame,
            Pose {
                roll: 12.0,
                ..Pose::default()
            },
        )],
        &cam,
        1,
    );
    assert_eq!(read(&img), ReadOutcome::Frames(vec![frame]));
}

#[test]
fn two_codes_in_view_are_both_read() {
    for (name, spec) in presets() {
        let mut rng = Noise::new(21);
        let a = some_frame(&spec, &mut rng);
        let b = some_frame(&spec, &mut rng);
        let left = code(
            &spec,
            &a,
            Pose {
                scale: 0.5,
                centre: (0.27, 0.45),
                roll: 8.0,
                ..Pose::default()
            },
        );
        let right = code(
            &spec,
            &b,
            Pose {
                scale: 0.45,
                centre: (0.74, 0.55),
                roll: -5.0,
                tilt: 15.0,
                tilt_axis: 90.0,
            },
        );
        let cam = Camera::default();
        assert!(in_view(&left, &cam) && in_view(&right, &cam));
        let img = shoot(&[left, right], &cam, 2);
        let ReadOutcome::Frames(frames) = read(&img) else {
            panic!("{name}: no frames read");
        };
        assert_eq!(frames.len(), 2, "{name}");
        assert!(frames.contains(&a) && frames.contains(&b), "{name}");
    }
}

/// Pictures that are not light codes but look like parts of one, by
/// name. QR-style symbols come at sizes either side of the presets and at
/// the preset sizes themselves.
fn look_alikes() -> Vec<(String, GrayImage)> {
    let mut out: Vec<(String, GrayImage)> = Vec::new();
    let mut rng = Noise::new(91);
    for unit in [2, 3, 5] {
        out.push((
            format!("finder stripes {unit}px"),
            finder_stripes(1280, 720, unit),
        ));
        out.push((
            format!("diagonal stripes {unit}px"),
            diagonal_stripes(1280, 720, unit),
        ));
    }
    out.push(("blinds".into(), blinds(1280, 720)));
    for square in [3, 6, 12, 30] {
        out.push((
            format!("checkerboard {square}px"),
            checkerboard(1280, 720, square),
        ));
    }
    for key in [24, 40, 70] {
        out.push((
            format!("keyboard {key}px"),
            keyboard(1280, 720, key, &mut rng),
        ));
    }
    for m in [21, 25, 33, 41, 45, 49, 53, 56, 57, 61, 64, 65] {
        let pose = Pose {
            scale: rng.range(0.4, 0.8),
            roll: rng.range(-15.0, 15.0),
            tilt: rng.range(0.0, 20.0),
            tilt_axis: rng.range(0.0, 360.0),
            ..Pose::default()
        };
        let qr = Code {
            n: m,
            grid: qr_like(m, &mut rng),
            pose,
            occlude: None,
        };
        out.push((
            format!("QR-style {m}"),
            shoot(&[qr], &Camera::default(), m as u64),
        ));
    }
    // Four QR-style symbols in one picture.
    let four: Vec<Code> = [
        (0.25, 0.27, 25),
        (0.75, 0.27, 41),
        (0.25, 0.73, 57),
        (0.75, 0.73, 65),
    ]
    .into_iter()
    .map(|(x, y, m)| Code {
        n: m,
        grid: qr_like(m, &mut rng),
        pose: Pose {
            scale: 0.45,
            centre: (x, y),
            ..Pose::default()
        },
        occlude: None,
    })
    .collect();
    out.push(("four QR-style".into(), shoot(&four, &Camera::default(), 7)));
    out
}

/// Look-alikes never read as frames, and are not mistaken for a code in
/// view either.
#[test]
fn look_alikes_read_as_no_code() {
    let mut unreadable = Vec::new();
    for (name, img) in look_alikes() {
        match read(&img) {
            ReadOutcome::NoCode => {}
            ReadOutcome::Unreadable => unreadable.push(name),
            ReadOutcome::Frames(f) => panic!("{name}: read {} frames", f.len()),
        }
    }
    assert!(unreadable.is_empty(), "taken for codes: {unreadable:?}");
}

/// Many look-alikes: how often anything that is not a light code is
/// taken for one. Frames must never appear; codes in view are counted.
#[test]
#[ignore = "slow; run in release with --ignored"]
fn false_codes_over_many_look_alikes() {
    let mut rng = Noise::new(98);
    let mut rows: Vec<(&str, u32, u32, u32)> = Vec::new();
    let mut tally = |kind: &'static str, img: &GrayImage| {
        let i = match rows.iter().position(|r| r.0 == kind) {
            Some(i) => i,
            None => {
                rows.push((kind, 0, 0, 0));
                rows.len() - 1
            }
        };
        rows[i].1 += 1;
        match read(img) {
            ReadOutcome::NoCode => {}
            ReadOutcome::Unreadable => rows[i].2 += 1,
            ReadOutcome::Frames(_) => rows[i].3 += 1,
        }
    };
    for i in 0..200u64 {
        // QR-style symbols of every version from 1 to 13 and the preset
        // sizes, posed like codes.
        let sizes = [
            21, 25, 29, 33, 37, 41, 45, 48, 49, 53, 56, 57, 61, 64, 65, 69,
        ];
        let m = sizes[(rng.next() % sizes.len() as u64) as usize];
        let pose = Pose {
            scale: rng.range(0.3, 0.9),
            roll: rng.range(-30.0, 30.0),
            tilt: rng.range(0.0, 30.0),
            tilt_axis: rng.range(0.0, 360.0),
            ..Pose::default()
        };
        let qr = Code {
            n: m,
            grid: qr_like(m, &mut rng),
            pose,
            occlude: None,
        };
        let cam = Camera {
            blur: rng.range(0.5, 1.5),
            noise: rng.range(1.0, 8.0),
            mirror: rng.unit() < 0.5,
            ..Camera::default()
        };
        if in_view(&qr, &cam) {
            tally("QR-style symbol", &shoot(&[qr], &cam, i));
        }
    }
    for square in 2..42 {
        tally("checkerboard", &checkerboard(1280, 720, square));
    }
    for i in 0..50u64 {
        let key = 16 + (i as u32 * 3) % 70;
        tally("keyboard", &keyboard(1280, 720, key, &mut rng));
    }
    for unit in 1..12 {
        tally("finder stripes", &finder_stripes(1280, 720, unit));
        tally("finder stripes", &diagonal_stripes(1280, 720, unit));
    }
    for seed in 0..400u64 {
        let cam = Camera {
            noise: rng.range(1.0, 10.0),
            blur: rng.range(0.5, 1.5),
            sharpen: rng.range(0.0, 1.5),
            ..Camera::default()
        };
        tally("room", &shoot(&[], &cam, 2000 + seed));
    }
    println!("| picture | count | taken for a code in view | frames read |\n|---|---|---|---|");
    for (kind, count, unreadable, frames) in &rows {
        println!("| {kind} | {count} | {unreadable} | {frames} |");
    }
    assert!(
        rows.iter().all(|r| r.3 == 0),
        "frames read from look-alikes"
    );
}

/// A light code drawn with random bytes: every fixed cell right, so it is
/// a code in view, but nothing passes the CRC.
fn garbage_codes(count: usize, rng: &mut Noise) -> Vec<Code> {
    let spec = CodeSpec::normal();
    let layout = Layout::new(&spec);
    [(0.25, 0.27), (0.75, 0.27), (0.25, 0.73), (0.75, 0.73)]
        .into_iter()
        .take(count)
        .map(|centre| {
            let junk: Vec<u8> = (0..spec.frame_bytes()).map(|_| rng.next() as u8).collect();
            Code {
                n: spec.cells as usize,
                grid: layout.grid(&junk),
                pose: Pose {
                    scale: 0.45,
                    centre,
                    ..Pose::default()
                },
                occlude: None,
            }
        })
        .collect()
}

/// Four codes in one picture are all read: finders of one code are never
/// spent on triples mixing codes.
#[test]
fn four_codes_in_view_are_all_read() {
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(96);
    let frames: Vec<Vec<u8>> = (0..4).map(|_| some_frame(&spec, &mut rng)).collect();
    let codes: Vec<Code> = [(0.25, 0.27), (0.75, 0.27), (0.25, 0.73), (0.75, 0.73)]
        .into_iter()
        .zip(&frames)
        .map(|(centre, frame)| {
            code(
                &spec,
                frame,
                Pose {
                    // Small enough that no code's grey surround covers another's
                    // quiet zone.
                    scale: 0.4,
                    centre,
                    ..Pose::default()
                },
            )
        })
        .collect();
    let ReadOutcome::Frames(read_back) = read(&shoot(&codes, &Camera::default(), 6)) else {
        panic!("nothing read");
    };
    assert_eq!(read_back.len(), 4);
    assert!(frames.iter().all(|f| read_back.contains(f)));
}

/// Codes that cannot be read do not use up the attempts a readable code
/// in the same picture needs.
#[test]
fn unreadable_codes_do_not_hide_a_readable_one() {
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(97);
    let mut codes = garbage_codes(3, &mut rng);
    let frame = some_frame(&spec, &mut rng);
    codes.push(code(
        &spec,
        &frame,
        Pose {
            scale: 0.45,
            centre: (0.75, 0.73),
            ..Pose::default()
        },
    ));
    assert_eq!(
        read(&shoot(&codes, &Camera::default(), 7)),
        ReadOutcome::Frames(vec![frame])
    );
}

#[test]
fn codes_with_garbage_bytes_are_unreadable() {
    let mut rng = Noise::new(92);
    let img = shoot(&garbage_codes(4, &mut rng), &Camera::default(), 1);
    assert_eq!(read(&img), ReadOutcome::Unreadable);
}

/// Pictures over 1280 px on their longer side are read at reduced size.
#[test]
fn a_4k_picture_reads() {
    for (name, spec) in presets() {
        let mut rng = Noise::new(93);
        let frame = some_frame(&spec, &mut rng);
        let cam = Camera {
            width: 3840,
            height: 2160,
            blur: 2.0,
            noise: 3.0,
            ..Camera::default()
        };
        let img = shoot(
            &[code(
                &spec,
                &frame,
                Pose {
                    scale: 0.4,
                    roll: 9.0,
                    ..Pose::default()
                },
            )],
            &cam,
            4,
        );
        assert_eq!(read(&img), ReadOutcome::Frames(vec![frame]), "{name}");
    }
}

#[test]
fn a_portrait_4k_picture_reads() {
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(99);
    let frame = some_frame(&spec, &mut rng);
    let cam = Camera {
        width: 2160,
        height: 3840,
        blur: 1.5,
        ..Camera::default()
    };
    let pose = Pose {
        scale: 0.3,
        ..Pose::default()
    };
    let img = shoot(&[code(&spec, &frame, pose)], &cam, 8);
    assert_eq!(read(&img), ReadOutcome::Frames(vec![frame]));
}

/// Where a texture goes beside a code, and where the code goes.
#[derive(Debug, Clone, Copy)]
enum Side {
    Left,
    Right,
    Above,
}

/// A 1280x720 picture of `frame`'s code with a texture filling the space
/// on one side of it, and the frame.
fn code_beside(spec: &CodeSpec, frame: &[u8], texture: &GrayImage, side: Side) -> GrayImage {
    type Region = fn(u32, u32) -> bool;
    let (centre, inside): ((f64, f64), Region) = match side {
        Side::Left => ((0.72, 0.5), |x, _| x < 512),
        Side::Right => ((0.28, 0.5), |x, _| x >= 768),
        Side::Above => ((0.5, 0.68), |_, y| y < 250),
    };
    let pose = Pose {
        scale: 0.5,
        centre,
        ..Pose::default()
    };
    let base = shoot(&[code(spec, frame, pose)], &Camera::default(), 9);
    GrayImage::from_fn(1280, 720, |x, y| {
        if inside(x, y) {
            *texture.get_pixel(x, y)
        } else {
            *base.get_pixel(x, y)
        }
    })
}

/// The textures that look most like finder rows, by name.
fn finder_like_textures() -> Vec<(&'static str, GrayImage)> {
    vec![
        ("stripes 2px", finder_stripes(1280, 720, 2)),
        ("stripes 3px", finder_stripes(1280, 720, 3)),
        ("diagonal stripes 2px", diagonal_stripes(1280, 720, 2)),
        ("blinds", blinds(1280, 720)),
        ("checkerboard 3px", checkerboard(1280, 720, 3)),
    ]
}

/// A finder-like texture on the same rows or columns as a code never
/// hides it.
#[test]
fn a_code_beside_finder_like_textures_reads() {
    for (name, spec) in presets() {
        let mut rng = Noise::new(100);
        let frame = some_frame(&spec, &mut rng);
        for (texture, img) in finder_like_textures() {
            for side in [Side::Left, Side::Right, Side::Above] {
                let picture = code_beside(&spec, &frame, &img, side);
                assert_eq!(
                    read(&picture),
                    ReadOutcome::Frames(vec![frame.clone()]),
                    "{name}, {texture} {side:?}"
                );
            }
        }
    }
}

/// A placement's name, the code's centre and scale, and which columns the
/// texture covers.
type Placement = (&'static str, (f64, f64, f64), fn(u32) -> bool);

/// Where an irregular texture covers the picture, and where the code
/// goes: one side, both sides, or a wide or narrow strip.
fn placements() -> Vec<Placement> {
    vec![
        ("left", (0.72, 0.5, 0.5), |x| x < 512),
        ("right", (0.28, 0.5, 0.5), |x| x >= 768),
        ("both sides", (0.5, 0.5, 0.4), |x| !(380..900).contains(&x)),
        ("left 60%", (0.72, 0.5, 0.5), |x| x < 640),
        ("left 20%", (0.72, 0.5, 0.5), |x| x < 256),
        ("left 10%", (0.72, 0.5, 0.5), |x| x < 128),
    ]
}

/// Every irregular texture in every placement beside a code of each
/// preset, with the frame shown: (description, picture, frame).
fn codes_beside_irregular_textures() -> Vec<(String, GrayImage, Vec<u8>)> {
    let textures: Vec<(&str, GrayImage)> = IRREGULAR
        .iter()
        .map(|&name| (name, irregular(name, 1280, 720)))
        .collect();
    let mut out = Vec::new();
    for (name, spec) in presets() {
        let mut rng = Noise::new(101);
        let frame = some_frame(&spec, &mut rng);
        for (place, (cx, cy, scale), inside) in placements() {
            let pose = Pose {
                scale,
                centre: (cx, cy),
                ..Pose::default()
            };
            let base = shoot(&[code(&spec, &frame, pose)], &Camera::default(), 10);
            for (texture, tex) in &textures {
                let img = GrayImage::from_fn(1280, 720, |x, y| {
                    if inside(x) {
                        *tex.get_pixel(x, y)
                    } else {
                        *base.get_pixel(x, y)
                    }
                });
                out.push((format!("{name}, {texture} {place}"), img, frame.clone()));
            }
        }
    }
    out
}

/// Finder-ratio textures with no fixed period, in any placement, never
/// hide a code beside them.
#[test]
fn a_code_beside_irregular_finder_textures_reads() {
    for (what, img, frame) in codes_beside_irregular_textures() {
        assert_eq!(read(&img), ReadOutcome::Frames(vec![frame]), "{what}");
    }
    for name in IRREGULAR {
        assert_eq!(
            read(&irregular(name, 1280, 720)),
            ReadOutcome::NoCode,
            "{name} alone"
        );
    }
}

/// Finder-like clutter, by name: bars in the finder ratio just short of
/// and just over the height a finder's cluster may have, leaning bars,
/// lattices of real finder marks at many sizes and spacings (some with a
/// dark mark beside each, so each looks like a code's corner), finder
/// marks scattered at random, and crosses of finder-ratio bars.
fn finder_clutter() -> Vec<(String, GrayImage)> {
    let mut out = Vec::new();
    for u in [2u32, 3, 4] {
        let cut = (1.5 * 7.0 * u as f64) as u32;
        for (what, height) in [("under", cut + 1), ("over", cut + 6)] {
            out.push((
                format!("bars {u}px, {height} tall ({what} the cut)"),
                finder_bars(1280, 720, u, height, (9 * u, height + 2), 0),
            ));
        }
        for shear in 1..=3 {
            out.push((
                format!("leaning bars {u}px, shear {shear}"),
                finder_bars(1280, 720, u, 720, (9 * u, 721), shear),
            ));
        }
        out.push((
            format!("short leaning bars {u}px"),
            finder_bars(1280, 720, u, cut + 1, (9 * u, cut + 3), 1),
        ));
    }
    // The most crossing work found: short bars packed tight, leaning.
    for (u, height) in [(2, 17), (3, 25), (3, 29), (4, 41)] {
        out.push((
            format!("packed leaning bars {u}px, {height} tall"),
            finder_bars(1280, 720, u, height, (8 * u, height + 1), 1),
        ));
    }
    out.push(("finder lattice 1px".into(), finder_lattice(1280, 720, 1, 1)));
    for u in 2..=6 {
        for gap in [0, 1, 2, 3, 4, 6] {
            // A gap of no cells is still a pixel, so the marks stay apart.
            let px = (gap * u).max(1);
            out.push((
                format!("finder lattice {u}px, {px}px apart"),
                finder_lattice(1280, 720, u, px),
            ));
        }
    }
    for (u, gap) in [(2, 6), (3, 9), (3, 12), (4, 12), (4, 16), (5, 15), (6, 18)] {
        out.push((
            format!("dotted lattice {u}px, {gap}px apart"),
            dotted_lattice(1280, 720, u, gap),
        ));
    }
    let mut rng = Noise::new(77);
    for (units, count) in [((3, 6), 40), ((3, 6), 150), ((4, 8), 400), ((2, 4), 1500)] {
        out.push((
            format!("{count} scattered finders {}-{}px", units.0, units.1),
            scattered_finders(1280, 720, units, count, &mut rng),
        ));
    }
    for u in [2, 3, 4] {
        out.push((format!("crosses {u}px"), finder_crosses(1280, 720, u)));
    }
    out
}

/// Where clutter goes beside a code: its name, the code's pose, and
/// which pixels the clutter covers given the code's quiet zone square
/// (left, top, right, bottom, in pixels).
type ClutterPlace = (&'static str, Pose, fn(u32, u32, [u32; 4]) -> bool);

fn clutter_places() -> Vec<ClutterPlace> {
    let pose = |x: f64, scale: f64, roll: f64| Pose {
        scale,
        centre: (x, 0.5),
        roll,
        ..Pose::default()
    };
    vec![
        ("left 40%", pose(0.72, 0.5, 0.0), |x, _, _| x < 512),
        ("right", pose(0.28, 0.5, 0.0), |x, _, _| x >= 768),
        ("both sides", pose(0.5, 0.4, 0.0), |x, _, _| {
            !(380..900).contains(&x)
        }),
        ("all round", pose(0.5, 0.4, 0.0), |x, y, [l, t, r, b]| {
            !((l..=r).contains(&x) && (t..=b).contains(&y))
        }),
        (
            "all round, small and turned",
            pose(0.4, 0.3, 12.0),
            |x, y, [l, t, r, b]| !((l..=r).contains(&x) && (t..=b).contains(&y)),
        ),
    ]
}

/// The square a posed code's quiet zone fills, turned with the code
/// (its bounding box), in a 1280x720 picture: left, top, right, bottom.
fn quiet_square(pose: &Pose) -> [u32; 4] {
    let side = pose.scale * 720.0;
    let turn = pose.roll.to_radians();
    let half = side / 2.0 * (turn.cos().abs() + turn.sin().abs());
    let (cx, cy) = (pose.centre.0 * 1280.0, pose.centre.1 * 720.0);
    [cx - half, cy - half, cx + half, cy + half].map(|v| v.clamp(0.0, 1279.0) as u32)
}

/// Read `count` pictures, built by `picture(i)` as (what, picture,
/// expected), on every core; the descriptions of those that read wrong.
fn read_all(
    count: usize,
    picture: impl Fn(usize) -> (String, GrayImage, ReadOutcome) + Sync,
) -> Vec<String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let wrong = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= count {
                    break;
                }
                let (what, img, want) = picture(i);
                let got = read(&img);
                if got != want {
                    wrong.lock().unwrap().push(format!("{what}: {got:?}"));
                }
            });
        }
    });
    let mut wrong = wrong.into_inner().unwrap();
    wrong.sort();
    wrong
}

/// The code beside clutter case `i` of [`finder_clutter`] by
/// [`clutter_places`] by preset, given the textures, the codes shot in
/// each place, and their frames.
fn clutter_case(
    i: usize,
    textures: &[(String, GrayImage)],
    shots: &[Vec<GrayImage>],
    frames: &[Vec<u8>],
) -> (String, GrayImage, ReadOutcome) {
    let places = clutter_places();
    let (t, rest) = (i % textures.len(), i / textures.len());
    let (p, preset) = (rest % places.len(), rest / places.len());
    let (place, pose, covered) = &places[p];
    let square = quiet_square(pose);
    let (tex, base) = (&textures[t].1, &shots[preset][p]);
    let img = GrayImage::from_fn(1280, 720, |x, y| {
        if covered(x, y, square) {
            *tex.get_pixel(x, y)
        } else {
            *base.get_pixel(x, y)
        }
    });
    let what = format!("{}, {} {place}", presets()[preset].0, textures[t].0);
    (what, img, ReadOutcome::Frames(vec![frames[preset].clone()]))
}

/// Each preset's frame and its code shot in each clutter place.
fn clutter_shots() -> (Vec<Vec<GrayImage>>, Vec<Vec<u8>>) {
    let (mut shots, mut frames) = (Vec::new(), Vec::new());
    for (_, spec) in presets() {
        let mut rng = Noise::new(102);
        let frame = some_frame(&spec, &mut rng);
        shots.push(
            clutter_places()
                .into_iter()
                .map(|(_, pose, _)| shoot(&[code(&spec, &frame, pose)], &Camera::default(), 11))
                .collect(),
        );
        frames.push(frame);
    }
    (shots, frames)
}

/// However much finder-like clutter surrounds a code, and however much
/// cleaner its marks are than the code's own, the code reads; the clutter
/// alone reads as no code.
#[test]
fn a_code_among_finder_clutter_reads() {
    let textures = finder_clutter();
    let (shots, frames) = clutter_shots();
    let count = textures.len() * clutter_places().len() * 3;
    let wrong = read_all(count, |i| clutter_case(i, &textures, &shots, &frames));
    assert!(wrong.is_empty(), "{} of {count}: {wrong:#?}", wrong.len());
    let wrong = read_all(textures.len(), |i| {
        let (name, img) = &textures[i];
        (format!("{name} alone"), img.clone(), ReadOutcome::NoCode)
    });
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Clutter built to get ahead of a code in the search: fake code corners
/// (finder marks with a code corner's surroundings, so they pass the
/// corner test), dense or sparse, on a light ground or on a checkerboard
/// (where every pair as far apart as a code's finders reads a timing line
/// between them), and walls of printed QR codes, which have finders,
/// corners and timing lines just as a light code does.
fn corner_clutter() -> Vec<(String, GrayImage)> {
    let mut out = Vec::new();
    for u in [2, 3] {
        for pitch in [14, 16, 20] {
            out.push((
                format!("fake corners {u}px, every {pitch} cells"),
                fake_corners(1280, 720, u, pitch, 0, 1),
            ));
            out.push((
                format!("fake corners {u}px, every {} cells, jittered", pitch + 3),
                fake_corners(1280, 720, u, pitch + 3, 3, 2),
            ));
        }
    }
    out.push((
        "fake corners 4px, every 19 cells, jittered".into(),
        fake_corners(1280, 720, 4, 19, 3, 3),
    ));
    out.push((
        "sparse fake corners 3px".into(),
        fake_corners(1280, 720, 3, 40, 20, 4),
    ));
    out.push((
        "sparse fake corners 4px".into(),
        fake_corners(1280, 720, 4, 30, 12, 5),
    ));
    for (u, pitch, jitter) in [(2, 16, 2), (3, 16, 2), (2, 19, 3)] {
        out.push((
            format!("fake corners on checks {u}px, every {pitch} cells"),
            fake_corners_on_checks(1280, 720, u, pitch, jitter, 7),
        ));
    }
    let mut rng = Noise::new(9);
    for (u, m) in [(2, 41), (2, 49), (3, 41), (2, 57)] {
        for count in [4, 8, 16, 40] {
            let (img, placed) = qr_wall(1280, 720, u, m, count, &mut rng);
            out.push((format!("{placed} QR codes of {m} cells, {u}px"), img));
        }
    }
    out
}

/// Codes to put beside [`corner_clutter`]: (name, camera, pose), the code
/// on the right of the picture, plain, blurred, small, under moire, and
/// tilted from 10 to 30 degrees about two axes.
fn beside_clutter_shots() -> Vec<(String, Camera, Pose)> {
    let at = |scale: f64, tilt: f64, tilt_axis: f64, roll: f64| Pose {
        scale,
        centre: (0.74, 0.5),
        roll,
        tilt,
        tilt_axis,
    };
    let mut out = vec![
        (
            "plain".to_string(),
            Camera::default(),
            at(0.45, 0.0, 0.0, 0.0),
        ),
        (
            "blur 1.5".into(),
            Camera {
                blur: 1.5,
                ..Camera::default()
            },
            at(0.45, 0.0, 0.0, 0.0),
        ),
        (
            "tilt 30, roll 15, blur 1.5, noise 6".into(),
            Camera {
                blur: 1.5,
                noise: 6.0,
                ..Camera::default()
            },
            at(0.42, 30.0, 70.0, 15.0),
        ),
        (
            "small, blur 1.2".into(),
            Camera {
                blur: 1.2,
                ..Camera::default()
            },
            Pose {
                scale: 0.3,
                centre: (0.8, 0.5),
                ..Pose::default()
            },
        ),
        (
            "moire".into(),
            Camera {
                moire: 0.15,
                moire_period: 3.0,
                moire_angle: 20.0,
                ..Camera::default()
            },
            at(0.45, 0.0, 0.0, -10.0),
        ),
    ];
    for tilt in [10.0, 15.0, 20.0, 25.0, 30.0] {
        for axis in [30.0, 70.0] {
            out.push((
                format!("tilt {tilt} about {axis}"),
                Camera::default(),
                at(0.45, tilt, axis, 0.0),
            ));
        }
    }
    out
}

/// Each preset's code shot every way [`beside_clutter_shots`] gives, three
/// seeds each: (what, picture, frame, where the clutter may come up to).
fn codes_for_clutter() -> Vec<(String, GrayImage, Vec<u8>, u32)> {
    let mut out = Vec::new();
    for (name, spec) in presets() {
        let frame = some_frame(&spec, &mut Noise::new(105));
        for (shot, cam, pose) in beside_clutter_shots() {
            // Clear of the code's quiet zone however it is tilted.
            let edge = (1280.0 * pose.centre.0 - 0.5 * pose.scale * 720.0 * 1.4) as u32;
            for seed in 0..3 {
                let img = shoot(&[code(&spec, &frame, pose.clone())], &cam, 50 + seed);
                out.push((format!("{name} {shot} s{seed}"), img, frame.clone(), edge));
            }
        }
    }
    out
}

/// A code beside clutter case `i`: the clutter left of the code's edge.
fn corner_clutter_case(
    i: usize,
    textures: &[(String, GrayImage)],
    codes: &[(String, GrayImage, Vec<u8>, u32)],
) -> (String, GrayImage, ReadOutcome) {
    let (t, c) = (i % textures.len(), i / textures.len());
    let (shot, base, frame, edge) = &codes[c];
    let tex = &textures[t].1;
    let img = GrayImage::from_fn(1280, 720, |x, y| {
        if x < *edge {
            *tex.get_pixel(x, y)
        } else {
            *base.get_pixel(x, y)
        }
    });
    let what = format!("{shot}, {} (x < {edge})", textures[t].0);
    (what, img, ReadOutcome::Frames(vec![frame.clone()]))
}

/// However many fake code corners or printed QR codes are beside a code,
/// however much crisper and squarer they are, and however the code is
/// tilted up to 30 degrees, the code reads on its own picture, with no
/// help from earlier ones; the clutter alone reads as no code.
#[test]
fn a_code_among_fake_code_corners_reads() {
    let textures = corner_clutter();
    let codes = codes_for_clutter();
    let wrong = read_all(codes.len(), |i| {
        let (what, img, frame, _) = &codes[i];
        (
            what.clone(),
            img.clone(),
            ReadOutcome::Frames(vec![frame.clone()]),
        )
    });
    assert!(wrong.is_empty(), "codes alone: {wrong:#?}");
    let count = textures.len() * codes.len();
    let wrong = read_all(count, |i| corner_clutter_case(i, &textures, &codes));
    assert!(wrong.is_empty(), "{} of {count}: {wrong:#?}", wrong.len());
    let wrong = read_all(textures.len(), |i| {
        let (name, img) = &textures[i];
        (format!("{name} alone"), img.clone(), ReadOutcome::NoCode)
    });
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `count` pictures of one code showing successive frames of an
/// envelope, the camera's noise different in each; `beside(i)` is laid
/// over picture `i` wherever it is not `None`.
fn sequence(
    count: usize,
    spec: &CodeSpec,
    pose: &Pose,
    cam: &Camera,
    beside: impl Fn(usize, u32, u32) -> Option<image::Luma<u8>>,
) -> Vec<(GrayImage, Vec<u8>)> {
    let mut enc = encode(spec, &[0x5a; 1500], 21);
    (0..count)
        .map(|i| {
            let frame = enc.next_frame();
            let mut img = shoot(&[code(spec, &frame, pose.clone())], cam, 700 + i as u64);
            for (x, y, px) in img.enumerate_pixels_mut() {
                if let Some(over) = beside(i, x, y) {
                    *px = over;
                }
            }
            (img, frame)
        })
        .collect()
}

/// A reader follows a code from picture to picture: once it has read one,
/// it reads every later picture of it, tilted or not, among clutter that
/// defeats the stateless search's first tries.
#[test]
fn a_reader_keeps_reading_a_code_it_has_found() {
    let clutter = fake_corners_on_checks(1280, 720, 2, 16, 2, 7);
    let (wall, _) = qr_wall(1280, 720, 2, 49, 16, &mut Noise::new(9));
    for (_, spec) in presets() {
        for tilt in [0.0, 25.0] {
            let pose = Pose {
                scale: 0.45,
                centre: (0.74, 0.5),
                tilt,
                tilt_axis: 30.0,
                ..Pose::default()
            };
            let pictures = sequence(10, &spec, &pose, &Camera::default(), |i, x, y| {
                let tex = if i % 2 == 0 { &clutter } else { &wall };
                (x < 720).then(|| *tex.get_pixel(x, y))
            });
            let mut reader = Reader::with_budget(UNHURRIED);
            for (i, (img, frame)) in pictures.iter().enumerate() {
                assert_eq!(
                    reader.read(img),
                    ReadOutcome::Frames(vec![frame.clone()]),
                    "{} cells, tilt {tilt}, picture {i}",
                    spec.cells
                );
            }
        }
    }
}

/// Texture right up to (and over) a code's quiet zone hides its finders'
/// corners from the stateless search, which then often misses it; a
/// reader that has seen the code once looks where it was and reads it
/// anyway.
#[test]
fn a_reader_reads_a_found_code_whose_quiet_zone_is_covered() {
    let textures = [
        ("stripes 2px", finder_stripes(1280, 720, 2)),
        ("stripes 3px", finder_stripes(1280, 720, 3)),
        ("stripes 4px", finder_stripes(1280, 720, 4)),
        ("stripes 5px", finder_stripes(1280, 720, 5)),
        ("jitter", irregular("jitter", 1280, 720)),
        ("alt 2/4", irregular("alt 2/4", 1280, 720)),
    ];
    let pose = Pose {
        scale: 0.5,
        centre: (0.6, 0.5),
        ..Pose::default()
    };
    let (mut stateless, mut total) = (0, 0);
    for (_, spec) in presets() {
        let n = spec.cells as f64;
        // The grid's own square, inside its three-cell quiet zone.
        let half = 0.5 * 0.5 * 720.0 * n / (n + 6.0);
        let (left, top, right) = (0.6 * 1280.0 - half, 360.0 - half, 0.6 * 1280.0 + half);
        let clean = sequence(1, &spec, &pose, &Camera::default(), |_, _, _| None);
        let mut locked = Reader::with_budget(UNHURRIED);
        assert_eq!(
            locked.read(&clean[0].0),
            ReadOutcome::Frames(vec![clean[0].1.clone()])
        );
        for (name, tex) in &textures {
            // Texture stops this far short of the grid: none, into the
            // quiet zone, or up to it.
            for short in [0.0, 3.0, 6.0, 10.0, 16.0, 30.0] {
                for above in [false, true] {
                    let covered = |x: f64, y: f64| {
                        if above {
                            y < top - short && x + 20.0 >= left && x <= right + 20.0
                        } else {
                            x < left - short
                        }
                    };
                    let pictures = sequence(2, &spec, &pose, &Camera::default(), |_, x, y| {
                        covered(x as f64, y as f64).then(|| *tex.get_pixel(x, y))
                    });
                    let (img, frame) = &pictures[1];
                    let want = ReadOutcome::Frames(vec![frame.clone()]);
                    total += 1;
                    stateless += (read(img) == want) as u32;
                    let mut reader = locked.clone();
                    assert_eq!(
                        reader.read(img),
                        want,
                        "{} cells, {name}, {short}px short, above {above}",
                        spec.cells
                    );
                }
            }
        }
    }
    println!("stateless search read {stateless} of {total}; the reader all");
}

/// A code that drifts across the picture a few pixels at a time, as in a
/// hand-held shot, stays followed; one that has gone is soon forgotten,
/// and a change of picture size forgets everything at once.
#[test]
fn a_reader_follows_a_drifting_code_and_forgets_a_gone_one() {
    let spec = CodeSpec::normal();
    let mut enc = encode(&spec, &[0x33; 900], 5);
    let mut reader = Reader::with_budget(UNHURRIED);
    for i in 0..10 {
        let frame = enc.next_frame();
        let pose = Pose {
            scale: 0.4,
            centre: (0.4 + 0.004 * i as f64, 0.5 - 0.003 * i as f64),
            roll: 0.3 * i as f64,
            ..Pose::default()
        };
        let img = shoot(&[code(&spec, &frame, pose)], &Camera::default(), 800 + i);
        assert_eq!(
            reader.read(&img),
            ReadOutcome::Frames(vec![frame]),
            "picture {i}"
        );
    }
    let blank = GrayImage::from_pixel(1280, 720, image::Luma([128]));
    for _ in 0..20 {
        assert_eq!(reader.read(&blank), ReadOutcome::NoCode);
    }
    let small = GrayImage::from_pixel(640, 360, image::Luma([128]));
    assert_eq!(reader.read(&small), ReadOutcome::NoCode);
}

/// With no time at all, a picture is not searched: the reading stops at
/// once with what it has, which is nothing.
#[test]
fn a_spent_budget_stops_the_search() {
    let spec = CodeSpec::normal();
    let frame = some_frame(&spec, &mut Noise::new(106));
    let img = shoot(
        &[code(&spec, &frame, Pose::default())],
        &Camera::default(),
        14,
    );
    assert_eq!(read(&img), ReadOutcome::Frames(vec![frame]));
    assert_eq!(read_within(&img, Duration::ZERO), ReadOutcome::NoCode);
    assert_eq!(
        Reader::with_budget(Duration::ZERO).read(&img),
        ReadOutcome::NoCode
    );
}

/// A reader gives more time only when time ran out. Blank pictures are
/// searched to the end and found empty, so the budget stays put however
/// many there are. Pictures of dense fake code corners use up a 2 ms
/// budget and a 4 ms one: after 3 in a row the budget doubles, and again
/// after each 3 more. A picture searched to the end with nothing in it,
/// or one it reads, puts the budget back. A reader whose own budget is
/// over 40 ms, the most it raises a budget to, keeps it.
#[test]
fn a_reader_gives_more_time_only_when_time_runs_out() {
    let ms = Duration::from_millis;
    let blank = GrayImage::from_pixel(1280, 720, image::Luma([128]));
    let walky = fake_corners_on_checks(1280, 720, 2, 16, 2, 7);
    let mut reader = Reader::with_budget(ms(2));
    for i in 0..15 {
        assert_eq!(reader.read(&blank), ReadOutcome::NoCode);
        assert_eq!(reader.budget(), ms(2), "blank picture {i}");
    }
    let mut budgets = Vec::new();
    for _ in 0..6 {
        budgets.push(reader.budget().as_millis());
        reader.read(&walky);
    }
    budgets.push(reader.budget().as_millis());
    assert_eq!(budgets, [2, 2, 2, 4, 4, 4, 8]);
    assert_eq!(reader.read(&blank), ReadOutcome::NoCode);
    assert_eq!(reader.budget(), ms(2), "a finished search puts it back");

    for _ in 0..3 {
        reader.read(&walky);
    }
    assert_eq!(reader.budget(), ms(4));
    let spec = CodeSpec::normal();
    let frame = some_frame(&spec, &mut Noise::new(107));
    let img = shoot(
        &[code(&spec, &frame, Pose::default())],
        &Camera::default(),
        15,
    );
    assert_eq!(reader.read(&img), ReadOutcome::Frames(vec![frame]));
    assert_eq!(reader.budget(), ms(2), "a read puts it back");

    let mut patient = Reader::with_budget(UNHURRIED);
    for _ in 0..4 {
        patient.read(&blank);
    }
    assert_eq!(patient.budget(), UNHURRIED);
}

/// A reader follows a code it cannot read for 8 pictures, then lets it
/// go, and does not take it up again for 30 pictures: it still says it
/// sees a code it cannot read, but no longer spends its time on it first.
#[test]
fn a_reader_lets_go_of_a_code_it_cannot_read() {
    let img = shoot(
        &garbage_codes(1, &mut Noise::new(108)),
        &Camera::default(),
        16,
    );
    let mut reader = Reader::with_budget(UNHURRIED);
    let followed: Vec<usize> = (0..40)
        .map(|i| {
            assert_eq!(reader.read(&img), ReadOutcome::Unreadable, "picture {i}");
            reader.followed_codes()
        })
        .collect();
    let mut want = vec![1; 7];
    want.extend([0; 30]);
    want.extend([1; 3]);
    assert_eq!(followed, want);
}

/// A reader on a small budget (5 ms) following two codes it cannot read
/// still finds a readable code that comes into view, within 12 pictures
/// however busy the machine (by then the budget has had time to rise to
/// 40 ms); `following_is_capped`, run alone in
/// release, asserts the first picture.
#[test]
fn followed_unreadable_codes_leave_the_search_its_time() {
    for (_, spec) in presets() {
        let (mut reader, target, frame) = following_two_garbage_codes(&spec, 5);
        let n = pictures_to_read(&mut reader, &target, &frame, 12);
        assert!(n <= 12, "{} cells: not read in 12 pictures", spec.cells);
    }
}

/// Run alone in release: two codes a reader follows but cannot read take
/// at most 30 percent of a 5 ms budget, so the search finds a readable
/// code that comes into view at the first picture of it, while both are
/// still followed. Following without the cap, the search runs out of time
/// and the code is read only once the budget has been raised.
#[test]
#[ignore = "timing; run alone in release with --ignored"]
fn following_is_capped() {
    for (_, spec) in presets() {
        let (mut reader, target, frame) = following_two_garbage_codes(&spec, 5);
        assert_eq!(
            reader.read(&target),
            ReadOutcome::Frames(vec![frame]),
            "{} cells",
            spec.cells
        );
    }
}

/// A reader of `ms` milliseconds following two unreadable codes, at its
/// own budget; then a picture with a readable code of `spec` beside them,
/// and its frame.
fn following_two_garbage_codes(spec: &CodeSpec, ms: u64) -> (Reader, GrayImage, Vec<u8>) {
    let budget = Duration::from_millis(ms);
    let (garbage, target, frame) = two_garbage_and_a_target(spec);
    let mut reader = Reader::with_budget(budget);
    // Until both are followed, and any budget raised meanwhile is back.
    for _ in 0..12 {
        if reader.followed_codes() == 2 && reader.budget() == budget {
            break;
        }
        reader.read(&garbage);
    }
    assert_eq!(reader.followed_codes(), 2, "{} cells", spec.cells);
    assert_eq!(reader.budget(), budget, "{} cells", spec.cells);
    (reader, target, frame)
}

/// Two unreadable codes on the left of a 1280x720 picture, and the same
/// with a readable code of `spec` on the right: (garbage only, garbage
/// and target, the target's frame).
fn two_garbage_and_a_target(spec: &CodeSpec) -> (GrayImage, GrayImage, Vec<u8>) {
    let layout = Layout::new(spec);
    let mut rng = Noise::new(109);
    let mut codes: Vec<Code> = [(0.15, 0.25), (0.15, 0.75)]
        .into_iter()
        .map(|centre| {
            let junk: Vec<u8> = (0..spec.frame_bytes()).map(|_| rng.next() as u8).collect();
            Code {
                n: spec.cells as usize,
                grid: layout.grid(&junk),
                pose: Pose {
                    scale: 0.3,
                    centre,
                    ..Pose::default()
                },
                occlude: None,
            }
        })
        .collect();
    let garbage = shoot(&codes, &Camera::default(), 17);
    let frame = some_frame(spec, &mut rng);
    codes.push(code(
        spec,
        &frame,
        Pose {
            scale: 0.45,
            centre: (0.72, 0.5),
            ..Pose::default()
        },
    ));
    (garbage, shoot(&codes, &Camera::default(), 18), frame)
}

/// Pictures of a clean code, 1280x720 or 4K: (size, pictures, frames).
fn clean_codes(width: usize, height: usize) -> Vec<(GrayImage, Vec<u8>)> {
    let cam = Camera {
        width,
        height,
        ..Camera::default()
    };
    let mut out = Vec::new();
    for (_, spec) in presets() {
        let mut enc = encode(&spec, &[0x6b; 900], 4);
        for seed in 0..2u64 {
            let frame = enc.next_frame();
            let pose = Pose {
                scale: 0.45,
                ..Pose::default()
            };
            out.push((shoot(&[code(&spec, &frame, pose)], &cam, 900 + seed), frame));
        }
    }
    out
}

/// How many pictures `reader` takes to read `frame` from `img` shown
/// over and over, up to `most`.
fn pictures_to_read(reader: &mut Reader, img: &GrayImage, frame: &[u8], most: usize) -> usize {
    (1..=most)
        .find(|_| matches!(reader.read(img), ReadOutcome::Frames(ref f) if f.contains(&frame.to_vec())))
        .unwrap_or(usize::MAX)
}

/// The budget is time to search, not counting the fixed cost of
/// preparing the picture, so a budget well under the default still reads
/// a clean code: 4.5 ms at 1280x720 and 10.5 ms at 4K (where preparing
/// alone takes about 7 ms). Through a reader, which gives more time after
/// pictures it cannot read, it is read within 10 pictures however busy
/// the machine (these tests run on every core at once); alone, in
/// release, `small_budgets_read_at_once` asserts the first picture.
#[test]
fn a_reader_on_a_small_budget_reads_a_clean_code() {
    for ((width, height), budget) in [((1280, 720), 4.5), ((3840, 2160), 10.5)] {
        for (i, (img, frame)) in clean_codes(width, height).iter().enumerate() {
            let mut reader = Reader::with_budget(Duration::from_secs_f64(budget / 1e3));
            let n = pictures_to_read(&mut reader, img, frame, 10);
            assert!(
                n <= 10,
                "{width}x{height} at {budget} ms, code {i}: not read in 10"
            );
        }
    }
}

/// Keeps every core busy until dropped.
struct Load(
    Vec<std::thread::JoinHandle<()>>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
);

impl Load {
    /// `per_core` spinning threads for each core.
    fn new(per_core: usize) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cores = std::thread::available_parallelism().map_or(8, |n| n.get());
        let threads = (0..per_core * cores)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 1u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                })
            })
            .collect();
        Load(threads, stop)
    }
}

impl Drop for Load {
    fn drop(&mut self) {
        self.1.store(true, std::sync::atomic::Ordering::Relaxed);
        for t in self.0.drain(..) {
            let _ = t.join();
        }
    }
}

/// With two spinning threads per core competing for the machine, a
/// reader on the default budget still reads a code within 10 pictures,
/// clean or beside fake code corners, at 1280x720 and 4K: it gives each
/// picture more time until one reads.
#[test]
fn a_reader_reads_on_a_loaded_machine() {
    let walky = fake_corners_on_checks(1280, 720, 2, 16, 2, 7);
    let mut cases: Vec<(String, GrayImage, Vec<u8>)> = Vec::new();
    for (width, height) in [(1280, 720), (3840, 2160)] {
        for (i, (img, frame)) in clean_codes(width, height).into_iter().enumerate() {
            cases.push((format!("{width}x{height} clean {i}"), img, frame));
        }
    }
    for (name, spec) in presets() {
        let pose = Pose {
            scale: 0.45,
            centre: (0.74, 0.5),
            tilt: 20.0,
            tilt_axis: 30.0,
            ..Pose::default()
        };
        let (img, frame) = sequence(1, &spec, &pose, &Camera::default(), |_, x, y| {
            (x < 720).then(|| *walky.get_pixel(x, y))
        })
        .remove(0);
        cases.push((format!("{name} beside fake corners"), img, frame));
    }
    let load = Load::new(2);
    let mut most = 0;
    for (what, img, frame) in &cases {
        let mut reader = Reader::new();
        let n = pictures_to_read(&mut reader, img, frame, 10);
        assert!(n <= 10, "{what}: not read in 10 pictures under load");
        most = most.max(n);
    }
    drop(load);
    println!("under load, every code read within {most} pictures");
}

/// A reader that has been following two codes it cannot read (on a
/// screen of noise, say) still takes up a readable code that comes into
/// view, and keeps it: the codes that read go first among those followed,
/// so the unreadable two cannot crowd it out, and then it reads every
/// picture of it even with its quiet zone covered, which the stateless
/// search cannot.
#[test]
fn unreadable_codes_followed_do_not_crowd_out_a_readable_one() {
    let stripes = finder_stripes(1280, 720, 3);
    let mut rng = Noise::new(5);
    for (_, spec) in presets() {
        let layout = Layout::new(&spec);
        let n = spec.cells as usize;
        let garbage: Vec<Code> = [(0.15, 0.25), (0.15, 0.75)]
            .into_iter()
            .map(|centre| {
                let junk: Vec<u8> = (0..spec.frame_bytes()).map(|_| rng.next() as u8).collect();
                Code {
                    n,
                    grid: layout.grid(&junk),
                    pose: Pose {
                        scale: 0.3,
                        centre,
                        ..Pose::default()
                    },
                    occlude: None,
                }
            })
            .collect();
        let pose = Pose {
            scale: 0.45,
            centre: (0.72, 0.5),
            ..Pose::default()
        };
        let half = 0.5 * 0.45 * 720.0 * n as f64 / (n as f64 + 6.0);
        let left = 0.72 * 1280.0 - half;
        let mut enc = encode(&spec, &[9; 900], 3);
        let mut reader = Reader::with_budget(UNHURRIED);
        let only_garbage = shoot(&garbage, &Camera::default(), 10);
        for _ in 0..3 {
            assert_eq!(reader.read(&only_garbage), ReadOutcome::Unreadable);
        }
        let mut stateless = 0;
        for i in 0..6u64 {
            let frame = enc.next_frame();
            let mut codes = garbage.clone();
            codes.push(code(&spec, &frame, pose.clone()));
            let mut img = shoot(&codes, &Camera::default(), 20 + i);
            // The first picture is clean; the rest have stripes right up
            // to the code's left edge.
            if i > 0 {
                for (x, y, px) in img.enumerate_pixels_mut() {
                    if (x as f64) < left - 3.0 && x > 560 {
                        *px = *stripes.get_pixel(x, y);
                    }
                }
                stateless += (read(&img) == ReadOutcome::Frames(vec![frame.clone()])) as u32;
            }
            assert_eq!(
                reader.read(&img),
                ReadOutcome::Frames(vec![frame]),
                "{} cells, picture {i}",
                spec.cells
            );
        }
        println!("{} cells: stateless read {stateless} of 5", spec.cells);
    }
}

/// Bars in the finder ratio over a growing share of the picture, sharp
/// or blurred, beside a code: (what, picture, expected).
fn blurred_bar_pictures() -> Vec<(String, GrayImage, ReadOutcome)> {
    let spec = CodeSpec::normal();
    let frame = some_frame(&spec, &mut Noise::new(103));
    let pose = Pose {
        scale: 0.5,
        centre: (0.72, 0.5),
        ..Pose::default()
    };
    let base = shoot(&[code(&spec, &frame, pose)], &Camera::default(), 12);
    let sharp = finder_bars(1280, 720, 3, 32, (27, 34), 0);
    let bars = [
        ("sharp", sharp.clone()),
        ("blurred 1px", box_blur(&sharp, 1)),
        ("blurred 2px", box_blur(&sharp, 2)),
    ];
    let mut out = Vec::new();
    for (blur, tex) in &bars {
        for share in [0.05, 0.10, 0.15, 0.20, 0.25, 0.30, 0.40] {
            let edge = (1280.0 * share) as u32;
            let img = GrayImage::from_fn(1280, 720, |x, y| {
                *if x < edge { tex } else { &base }.get_pixel(x, y)
            });
            let what = format!("bars {blur} over {:.0}%", share * 100.0);
            out.push((what, img, ReadOutcome::Frames(vec![frame.clone()])));
        }
    }
    out
}

/// Sharp or blurred finder-ratio bars over a growing share of the
/// picture never hide a code beside them.
#[test]
fn a_code_beside_blurred_finder_bars_reads() {
    let pictures = blurred_bar_pictures();
    let wrong = read_all(pictures.len(), |i| pictures[i].clone());
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Finder-ratio texture right up to a code, above it or to its left,
/// leaving at least most of the quiet zone: (what, picture, expected).
fn quiet_zone_pictures() -> Vec<(String, GrayImage, ReadOutcome)> {
    let textures = [
        ("stripes 2px", finder_stripes(1280, 720, 2)),
        ("stripes 3px", finder_stripes(1280, 720, 3)),
        ("stripes 4px", finder_stripes(1280, 720, 4)),
        ("stripes 5px", finder_stripes(1280, 720, 5)),
        ("jitter", irregular("jitter", 1280, 720)),
        ("alt 2/4", irregular("alt 2/4", 1280, 720)),
    ];
    let (shots, frames): (Vec<_>, Vec<_>) = presets()
        .iter()
        .map(|(_, spec)| {
            let frame = some_frame(spec, &mut Noise::new(104));
            let pose = Pose {
                scale: 0.5,
                centre: (0.6, 0.5),
                ..Pose::default()
            };
            let shot = shoot(&[code(spec, &frame, pose)], &Camera::default(), 13);
            (shot, frame)
        })
        .unzip();
    // Texture stops this many pixels short of the code's grid, which
    // starts a quiet zone (three cells, 15 to 20 pixels here) in from the
    // edge of the square the pose gives.
    let gaps = [16, 30];
    let count = 3 * textures.len() * gaps.len() * 2;
    (0..count)
        .map(|i| {
            let (p, rest) = (i % 3, i / 3);
            let (t, rest) = (rest % textures.len(), rest / textures.len());
            let (g, above) = (rest % gaps.len(), rest / gaps.len() == 1);
            let spec = presets()[p].1;
            let n = spec.cells as f64;
            let half = 0.5 * 0.5 * 720.0 * n / (n + 6.0);
            let (left, top, right) = (0.6 * 1280.0 - half, 360.0 - half, 0.6 * 1280.0 + half);
            let gap = gaps[g] as f64;
            let tex = &textures[t].1;
            let img = GrayImage::from_fn(1280, 720, |x, y| {
                let (x, y) = (x as f64, y as f64);
                let covered = if above {
                    y < top - gap && x + 20.0 >= left && x <= right + 20.0
                } else {
                    x < left - gap
                };
                let src = if covered { tex } else { &shots[p] };
                *src.get_pixel(x as u32, y as u32)
            });
            let side = if above { "above" } else { "left" };
            let what = format!(
                "{}, {} {side}, {gap}px short",
                presets()[p].0,
                textures[t].0
            );
            (what, img, ReadOutcome::Frames(vec![frames[p].clone()]))
        })
        .collect()
}

/// Texture up to a code's quiet zone, leaving most of it, does not hide
/// the code. (Texture further in, over the quiet zone itself, is a
/// damaged code and may not read.)
#[test]
fn texture_up_to_the_quiet_zone_does_not_hide_a_code() {
    let pictures = quiet_zone_pictures();
    let wrong = read_all(pictures.len(), |i| pictures[i].clone());
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The top of the scale range: the quiet zone fills the frame height.
#[test]
fn a_code_filling_the_frame_reads() {
    for (name, spec) in presets() {
        let mut rng = Noise::new(94);
        let frame = some_frame(&spec, &mut rng);
        let c = code(
            &spec,
            &frame,
            Pose {
                scale: 1.0,
                ..Pose::default()
            },
        );
        assert!(in_view(&c, &Camera::default()), "{name}");
        let img = shoot(&[c], &Camera::default(), 5);
        assert_eq!(read(&img), ReadOutcome::Frames(vec![frame]), "{name}");
    }
}

/// A camera exposure that straddles a display frame change sees a blend
/// of two consecutive frames. It never reads as anything but one of them.
#[test]
fn blended_frames_never_misread() {
    let mut tally = (0, 0, 0);
    for (name, spec) in presets() {
        let mut rng = Noise::new(95);
        let envelope: Vec<u8> = (0..1500).map(|_| rng.next() as u8).collect();
        let mut enc = encode(&spec, &envelope, 5);
        let layout = Layout::new(&spec);
        for i in 0..40u64 {
            let (a, b) = (enc.next_frame(), enc.next_frame());
            let pose = Pose {
                roll: rng.range(-10.0, 10.0),
                tilt: rng.range(0.0, 20.0),
                tilt_axis: rng.range(0.0, 360.0),
                scale: rng.range(0.4, 0.6),
                ..Pose::default()
            };
            let n = spec.cells as usize;
            let shot = |grid: Vec<bool>| {
                let c = Code {
                    n,
                    grid,
                    pose: pose.clone(),
                    occlude: None,
                };
                shoot(&[c], &Camera::default(), i)
            };
            let (ia, ib) = (shot(layout.grid(&a)), shot(layout.grid(&b)));
            let alpha = rng.range(0.2, 0.8);
            let img = GrayImage::from_fn(ia.width(), ia.height(), |x, y| {
                let (pa, pb) = (
                    ia.get_pixel(x, y).0[0] as f64,
                    ib.get_pixel(x, y).0[0] as f64,
                );
                image::Luma([(pa * alpha + pb * (1.0 - alpha)).round() as u8])
            });
            match read(&img) {
                ReadOutcome::Frames(frames) => {
                    assert!(
                        frames.iter().all(|f| *f == a || *f == b),
                        "{name}: blend {i} misread"
                    );
                    tally.0 += 1;
                }
                ReadOutcome::Unreadable => tally.1 += 1,
                ReadOutcome::NoCode => tally.2 += 1,
            }
        }
    }
    println!(
        "blends: {} read one of the two, {} unreadable, {} no code",
        tally.0, tally.1, tally.2
    );
}

#[test]
fn noise_blank_and_clutter_read_as_no_code() {
    let blank = GrayImage::from_pixel(1280, 720, image::Luma([128]));
    assert_eq!(read(&blank), ReadOutcome::NoCode);
    let black = GrayImage::from_pixel(1280, 720, image::Luma([0]));
    assert_eq!(read(&black), ReadOutcome::NoCode);
    let mut rng = Noise::new(5);
    let noise = GrayImage::from_fn(1280, 720, |_, _| image::Luma([rng.next() as u8]));
    assert_eq!(read(&noise), ReadOutcome::NoCode);
    // Rooms with no screen in them, under varied cameras.
    for seed in 0..100 {
        let cam = Camera {
            noise: rng.range(1.0, 10.0),
            blur: rng.range(0.5, 1.5),
            sharpen: rng.range(0.0, 1.0),
            ..Camera::default()
        };
        assert_eq!(
            read(&shoot(&[], &cam, 1000 + seed)),
            ReadOutcome::NoCode,
            "room {seed}"
        );
    }
    // Tiny pictures.
    assert_eq!(read(&GrayImage::new(1, 1)), ReadOutcome::NoCode);
    assert_eq!(read(&GrayImage::new(0, 0)), ReadOutcome::NoCode);
}

/// Codes partly out of the frame, and crops of all sizes, never panic
/// the reader and never yield a frame that was not shown.
#[test]
fn partial_views_and_odd_sizes_never_misread() {
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(71);
    let frame = some_frame(&spec, &mut rng);
    for i in 0..40u64 {
        let pose = Pose {
            scale: rng.range(0.3, 1.2),
            centre: (rng.range(-0.1, 1.1), rng.range(-0.1, 1.1)),
            roll: rng.range(0.0, 360.0),
            tilt: rng.range(0.0, 50.0),
            tilt_axis: rng.range(0.0, 360.0),
        };
        let cam = Camera {
            width: 32 + (rng.next() % 1300) as usize,
            height: 32 + (rng.next() % 800) as usize,
            ..Camera::default()
        };
        let img = shoot(&[code(&spec, &frame, pose)], &cam, i);
        if let ReadOutcome::Frames(frames) = read(&img) {
            assert_eq!(frames, vec![frame.clone()], "picture {i}");
        }
    }
}

/// A picture caught while the screen changed frames (top rows from one
/// frame, the rest from the next) never reads as anything but one of the
/// two.
#[test]
fn a_frame_caught_mid_change_never_misreads() {
    for (name, spec) in presets() {
        let mut rng = Noise::new(81);
        let layout = Layout::new(&spec);
        let n = spec.cells as usize;
        for i in 0..20u64 {
            let a = some_frame(&spec, &mut rng);
            let b = some_frame(&spec, &mut rng);
            let split = (rng.next() as usize % (n - 8)) + 4;
            let (ga, gb) = (layout.grid(&a), layout.grid(&b));
            let grid: Vec<bool> = (0..n * n)
                .map(|k| if k / n < split { ga[k] } else { gb[k] })
                .collect();
            let c = Code {
                n,
                grid,
                pose: Pose::default(),
                occlude: None,
            };
            if let ReadOutcome::Frames(frames) = read(&shoot(&[c], &Camera::default(), i)) {
                assert!(
                    frames == vec![a] || frames == vec![b],
                    "{name}: picture {i} misread"
                );
            }
        }
    }
}

#[test]
fn a_code_in_view_that_cannot_be_read_says_so() {
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(31);
    let frame = some_frame(&spec, &mut rng);
    // A finger over the middle of the code: finders and orientation mark
    // still in view, a third of the data hidden.
    let mut c = code(&spec, &frame, Pose::default());
    c.occlude = Some((14.0, 14.0, 44.0, 44.0));
    let img = shoot(&[c], &Camera::default(), 3);
    assert_eq!(read(&img), ReadOutcome::Unreadable);
    let mut collector = Collector::new();
    assert_eq!(
        collector.push_read(&read(&img), 0),
        vec![LightEvent::FrameBad]
    );
}

/// Every frame of an envelope shot under mixed conditions, through
/// `read` and the collector, rebuilds the envelope.
#[test]
fn an_envelope_arrives_through_the_camera() {
    for (name, spec) in presets() {
        let mut rng = Noise::new(41);
        let envelope: Vec<u8> = (0..2000).map(|_| rng.next() as u8).collect();
        let mut enc = encode(&spec, &envelope, 77);
        let mut collector = Collector::new();
        let mut done = None;
        for i in 0..60u64 {
            let frame = enc.next_frame();
            let pose = Pose {
                scale: rng.range(0.45, 0.6),
                roll: rng.range(-10.0, 10.0),
                tilt: rng.range(0.0, 20.0),
                tilt_axis: rng.range(0.0, 360.0),
                ..Pose::default()
            };
            let cam = Camera {
                blur: 1.0,
                noise: 4.0,
                jpeg: Some(80),
                ..Camera::default()
            };
            let img = shoot(&[code(&spec, &frame, pose)], &cam, i);
            let events = collector.push_read(&read(&img), i * 100);
            if let Some(bytes) = events.iter().find_map(|e| match e {
                LightEvent::Complete { tag: 77, bytes } => Some(bytes.clone()),
                _ => None,
            }) {
                done = Some(bytes);
                break;
            }
        }
        assert_eq!(done.as_deref(), Some(&envelope[..]), "{name}");
    }
}

/// The whole way in, as the app will run it: a camera (the fake, playing
/// pictures of an envelope's frames) hands each picture to its sink, a
/// reader reads it, and the collector puts the envelope back together.
/// Stopping the camera ends the pictures.
#[test]
fn an_envelope_arrives_through_a_camera_and_a_reader() {
    let spec = CodeSpec::normal();
    let envelope: Vec<u8> = (0..1500).map(|i| (i * 7 % 251) as u8).collect();
    let mut enc = encode(&spec, &envelope, 78);
    let pictures = (0..40u64)
        .map(|i| {
            let pose = Pose {
                scale: 0.5,
                roll: 3.0,
                ..Pose::default()
            };
            shoot(
                &[code(&spec, &enc.next_frame(), pose)],
                &Camera::default(),
                600 + i,
            )
        })
        .collect();
    let mut camera = FakeCamera::new(pictures);
    let handle = camera.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    camera
        .start(Box::new(move |img| {
            let _ = tx.send(img);
        }))
        .unwrap();
    let (mut reader, mut collector) = (Reader::with_budget(UNHURRIED), Collector::new());
    let mut done = None;
    let mut t = 0;
    while done.is_none() && handle.show_next() {
        let img = rx.recv().unwrap();
        t += 33;
        done = collector
            .push_read(&reader.read(&img), t)
            .into_iter()
            .find_map(|e| match e {
                LightEvent::Complete { tag: 78, bytes } => Some(bytes),
                _ => None,
            });
    }
    camera.stop();
    assert_eq!(done.as_deref(), Some(&envelope[..]));
    assert!(
        handle.remaining() > 0,
        "it arrived before the pictures ran out"
    );
    assert!(!handle.show_next(), "no pictures after stop");
    assert_eq!((handle.starts(), handle.stops()), (1, 1));
}

/// Checked-in pictures: a handful of shots, kept as PNGs so a change to
/// the camera model cannot quietly change what the reader is held to.
/// Set `BOMBECHAT_WRITE_FIXTURES=1` to redraw them.
#[test]
fn fixtures_read() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let write = std::env::var_os("BOMBECHAT_WRITE_FIXTURES").is_some();
    for (file, expected) in fixtures() {
        let path = dir.join(file.name);
        if write {
            std::fs::create_dir_all(&dir).unwrap();
            file.image.save(&path).unwrap();
        }
        let img = image::open(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .into_luma8();
        assert_eq!(read(&img), expected, "{}", file.name);
    }
}

struct Fixture {
    name: &'static str,
    image: GrayImage,
}

fn fixtures() -> Vec<(Fixture, ReadOutcome)> {
    let small = |cam: Camera| Camera {
        width: 480,
        height: 270,
        ..cam
    };
    let mut out = Vec::new();
    let spec = CodeSpec::normal();
    let mut rng = Noise::new(51);

    let frame = some_frame(&spec, &mut rng);
    let pose = Pose {
        scale: 0.7,
        roll: -14.0,
        tilt: 25.0,
        tilt_axis: 30.0,
        ..Pose::default()
    };
    let cam = small(Camera {
        blur: 0.8,
        noise: 4.0,
        gradient: 0.4,
        moire: 0.08,
        jpeg: Some(75),
        ..Camera::default()
    });
    out.push((
        Fixture {
            name: "normal-tilted.png",
            image: shoot(&[code(&spec, &frame, pose)], &cam, 1),
        },
        ReadOutcome::Frames(vec![frame]),
    ));

    let frame = some_frame(&spec, &mut rng);
    let cam = small(Camera {
        mirror: true,
        ..Camera::default()
    });
    out.push((
        Fixture {
            name: "normal-mirrored.png",
            image: shoot(
                &[code(
                    &spec,
                    &frame,
                    Pose {
                        scale: 0.8,
                        roll: 5.0,
                        ..Pose::default()
                    },
                )],
                &cam,
                2,
            ),
        },
        ReadOutcome::Frames(vec![frame]),
    ));

    let fast = CodeSpec::fast();
    let frame = some_frame(&fast, &mut rng);
    out.push((
        Fixture {
            name: "fast-straight.png",
            image: shoot(
                &[code(
                    &fast,
                    &frame,
                    Pose {
                        scale: 0.9,
                        ..Pose::default()
                    },
                )],
                &small(Camera::default()),
                3,
            ),
        },
        ReadOutcome::Frames(vec![frame]),
    ));

    let frame = some_frame(&spec, &mut rng);
    let mut hidden = code(
        &spec,
        &frame,
        Pose {
            scale: 0.8,
            ..Pose::default()
        },
    );
    hidden.occlude = Some((14.0, 14.0, 44.0, 44.0));
    out.push((
        Fixture {
            name: "normal-hidden.png",
            image: shoot(&[hidden], &small(Camera::default()), 4),
        },
        ReadOutcome::Unreadable,
    ));

    out.push((
        Fixture {
            name: "room.png",
            image: shoot(&[], &small(Camera::default()), 5),
        },
        ReadOutcome::NoCode,
    ));
    out
}

/// Run alone in release, a budget well under the default reads a clean
/// code at the first picture, with no reader to give it more time: 4.5 ms
/// at 1280x720 and 10.5 ms at 4K (the budget does not count preparing the
/// picture).
#[test]
#[ignore = "timing; run alone in release with --ignored"]
fn small_budgets_read_at_once() {
    for ((width, height), budget) in [((1280, 720), 4.5), ((3840, 2160), 10.5)] {
        let budget = Duration::from_secs_f64(budget / 1e3);
        for (i, (img, frame)) in clean_codes(width, height).iter().enumerate() {
            assert_eq!(
                read_within(img, budget),
                ReadOutcome::Frames(vec![frame.clone()]),
                "{width}x{height} at {budget:?}, code {i}"
            );
        }
    }
}

/// Decoding time per 1280x720 picture with the default budget, over
/// pictures with a code, with nothing, with noise, with a code that
/// cannot be read, and with every kind of clutter the other tests build,
/// and how many of them read as they do with no deadline; then a reader
/// with the default budget through cluttered sequences. Only meaningful
/// in release, run on its own (not alongside the sweep, which takes every
/// core): the budget is wall-clock time.
#[test]
#[ignore = "timing; run alone in release with --ignored"]
fn decode_time_per_frame() {
    let mut pictures: Vec<(&str, String, GrayImage)> = Vec::new();
    let mut rng = Noise::new(61);
    for (name, spec) in presets() {
        for i in 0..10 {
            let frame = some_frame(&spec, &mut rng);
            let pose = Pose {
                scale: rng.range(0.3, 0.9),
                roll: rng.range(-20.0, 20.0),
                tilt: rng.range(0.0, 30.0),
                tilt_axis: rng.range(0.0, 360.0),
                ..Pose::default()
            };
            let c = code(&spec, &frame, pose);
            if in_view(&c, &Camera::default()) {
                pictures.push((name, name.to_string(), shoot(&[c], &Camera::default(), i)));
            }
        }
    }
    for i in 0..10 {
        pictures.push((
            "room",
            "room".into(),
            shoot(&[], &Camera::default(), 100 + i),
        ));
    }
    // Codes in view that cannot be read try every fit, the slowest case.
    for (i, (_, spec)) in presets().into_iter().enumerate() {
        let frame = some_frame(&spec, &mut rng);
        let mut hidden = code(&spec, &frame, Pose::default());
        let n = spec.cells as f64;
        hidden.occlude = Some((0.25 * n, 0.25 * n, 0.8 * n, 0.8 * n));
        pictures.push((
            "unreadable",
            "unreadable".into(),
            shoot(&[hidden], &Camera::default(), 200 + i as u64),
        ));
        let blurred = code(
            &spec,
            &frame,
            Pose {
                scale: 0.3,
                ..Pose::default()
            },
        );
        let cam = Camera {
            blur: 2.5,
            ..Camera::default()
        };
        pictures.push((
            "unreadable",
            "unreadable".into(),
            shoot(&[blurred], &cam, 300 + i as u64),
        ));
    }
    let mut n = Noise::new(62);
    pictures.push((
        "noise",
        "noise".into(),
        GrayImage::from_fn(1280, 720, |_, _| image::Luma([n.next() as u8])),
    ));
    for (name, img) in look_alikes() {
        pictures.push(("look-alike", name, img));
    }
    pictures.push((
        "garbage codes",
        "four garbage codes".into(),
        shoot(&garbage_codes(4, &mut rng), &Camera::default(), 400),
    ));
    for (_, spec) in presets() {
        let frame = some_frame(&spec, &mut rng);
        for (texture, img) in finder_like_textures() {
            for side in [Side::Left, Side::Right, Side::Above] {
                pictures.push((
                    "code beside texture",
                    format!("{} code, {texture} {side:?}", spec.cells),
                    code_beside(&spec, &frame, &img, side),
                ));
            }
        }
    }
    for (what, img, _) in codes_beside_irregular_textures() {
        pictures.push(("code beside irregular texture", what, img));
    }
    for name in IRREGULAR {
        pictures.push((
            "irregular texture",
            name.to_string(),
            irregular(name, 1280, 720),
        ));
    }
    let portrait = Camera {
        width: 2160,
        height: 3840,
        blur: 1.5,
        ..Camera::default()
    };
    pictures.push((
        "4K portrait",
        "4K portrait".into(),
        finder_stripes(2160, 3840, 2),
    ));
    let spec = CodeSpec::normal();
    let frame = some_frame(&spec, &mut rng);
    pictures.push((
        "4K portrait",
        "4K portrait code".into(),
        shoot(
            &[code(
                &spec,
                &frame,
                Pose {
                    scale: 0.3,
                    ..Pose::default()
                },
            )],
            &portrait,
            402,
        ),
    ));
    let cam4k = Camera {
        width: 3840,
        height: 2160,
        blur: 2.0,
        ..Camera::default()
    };
    pictures.push((
        "4K code",
        "4K code".into(),
        shoot(&[code(&spec, &frame, Pose::default())], &cam4k, 401),
    ));

    // Each picture is read once without a deadline, for what it holds,
    // then four times as the camera would read it, with the default
    // budget: the first for its outcome (and to warm up), the rest timed.
    // A reading may take its preparation (timed as a reading with no
    // budget at all) and its budget, and run over by at most one fit:
    // the most any reading of any picture runs over that is what must
    // stay under `OVER`.
    const OVER: f64 = 2.0;
    let budget_ms = bombechat_light::DEFAULT_BUDGET.as_secs_f64() * 1000.0;
    let mut worst = (0f64, String::new());
    let mut over = (f64::MIN, String::new());
    // (kind, total of means, slowest single reading, pictures, read as
    // with no deadline)
    let mut by_kind: Vec<(&str, f64, f64, u32, u32)> = Vec::new();
    let mut time = |kind: &'static str, name: &str, img: &GrayImage| {
        let unhurried = read(img);
        let same = bombechat_light::read(img) == unhurried;
        let prep = preparation(img);
        let (mut total, mut slowest) = (0.0, 0f64);
        for _ in 0..3 {
            let start = Instant::now();
            std::hint::black_box(bombechat_light::read(img));
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            total += ms;
            slowest = slowest.max(ms);
        }
        if slowest > worst.0 {
            worst = (slowest, name.to_string());
        }
        if slowest - prep - budget_ms > over.0 {
            over = (slowest - prep - budget_ms, name.to_string());
        }
        let i = match by_kind.iter().position(|k| k.0 == kind) {
            Some(i) => i,
            None => {
                by_kind.push((kind, 0.0, 0.0, 0, 0));
                by_kind.len() - 1
            }
        };
        let k = &mut by_kind[i];
        k.1 += total / 3.0;
        k.2 = k.2.max(slowest);
        k.3 += 1;
        k.4 += same as u32;
    };
    for (kind, name, img) in &pictures {
        time(kind, name, img);
    }
    drop(pictures);
    // Finder clutter: built one picture at a time (there are about a
    // thousand).
    let textures = finder_clutter();
    let (shots, frames) = clutter_shots();
    for i in 0..textures.len() * clutter_places().len() * 3 {
        let (what, img, _) = clutter_case(i, &textures, &shots, &frames);
        time("code among finder clutter", &what, &img);
    }
    for (name, img) in &textures {
        time("finder clutter alone", name, img);
    }
    for (what, img, _) in blurred_bar_pictures() {
        time("code beside blurred bars", &what, &img);
    }
    for (what, img, _) in quiet_zone_pictures() {
        time("texture up to the quiet zone", &what, &img);
    }
    // Fake code corners and QR walls beside codes, the first seed of each
    // shot (a third of what `a_code_among_fake_code_corners_reads` reads).
    let corners = corner_clutter();
    let codes: Vec<_> = codes_for_clutter()
        .into_iter()
        .filter(|c| c.0.ends_with("s0"))
        .collect();
    for i in 0..corners.len() * codes.len() {
        let (what, img, _) = corner_clutter_case(i, &corners, &codes);
        time("code among fake code corners", &what, &img);
    }
    for (name, img) in &corners {
        time("fake code corners alone", name, img);
    }
    for (kind, total, slowest, count, same) in &by_kind {
        println!(
            "{kind}: mean {:.2} ms, slowest {slowest:.2} ms, over {count} pictures; \
             {same} read as with no deadline",
            total / *count as f64
        );
    }

    // A reader with the default budget through ten pictures of a code
    // beside the worst clutter: it must read every picture after the
    // first it reads, each within the budget.
    let walky = fake_corners_on_checks(1280, 720, 2, 16, 2, 7);
    let (wall, _) = qr_wall(1280, 720, 2, 49, 40, &mut Noise::new(9));
    let (mut slowest, mut first_missed) = (0f64, 0);
    for (_, spec) in presets() {
        for tilt in [0.0, 30.0] {
            for (name, tex) in [("fake corners on checks", &walky), ("40 QR codes", &wall)] {
                let pose = Pose {
                    scale: 0.45,
                    centre: (0.74, 0.5),
                    tilt,
                    tilt_axis: 30.0,
                    ..Pose::default()
                };
                let pictures = sequence(10, &spec, &pose, &Camera::default(), |_, x, y| {
                    (x < 720).then(|| *tex.get_pixel(x, y))
                });
                let mut reader = Reader::new();
                let mut locked = false;
                for (i, (img, frame)) in pictures.iter().enumerate() {
                    let allowed = preparation(img) + reader.budget().as_secs_f64() * 1000.0;
                    let start = Instant::now();
                    let got = reader.read(img);
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    slowest = slowest.max(ms);
                    let what = format!(
                        "reader, {} cells, tilt {tilt}, {name}, picture {i}",
                        spec.cells
                    );
                    if ms > worst.0 {
                        worst = (ms, what.clone());
                    }
                    if ms - allowed > over.0 {
                        over = (ms - allowed, what.clone());
                    }
                    let read = got == ReadOutcome::Frames(vec![frame.clone()]);
                    assert!(read || !locked, "{what}: {got:?}");
                    first_missed += (!locked && !read) as u32;
                    locked |= read;
                }
                assert!(
                    locked,
                    "reader, {} cells, tilt {tilt}, {name}: never read",
                    spec.cells
                );
            }
        }
    }
    println!(
        "reader through 12 cluttered sequences of 10: every picture after the first \
         read, {first_missed} pictures before it; slowest {slowest:.2} ms"
    );
    println!("worst: {:.2} ms ({})", worst.0, worst.1);
    println!(
        "most over preparation and budget: {:.2} ms ({})",
        over.0, over.1
    );
    if !cfg!(debug_assertions) {
        assert!(
            over.0 < OVER,
            "a reading ran {:.2} ms over its preparation and budget ({})",
            over.0,
            over.1
        );
    }
}

/// How long preparing `img` for reading takes, in milliseconds: the
/// fastest of three readings with no budget, which stop as soon as the
/// picture is ready.
fn preparation(img: &GrayImage) -> f64 {
    (0..3)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(read_within(img, Duration::ZERO));
            start.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::MAX, f64::min)
}
