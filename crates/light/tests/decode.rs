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
use std::time::Instant;

use bombechat_light::{encode, read, CodeSpec, Collector, Layout, LightEvent, ReadOutcome};
use camera::textures::{
    blinds, box_blur, checkerboard, diagonal_stripes, dotted_lattice, finder_bars, finder_crosses,
    finder_lattice, finder_stripes, irregular, keyboard, qr_like, scattered_finders, IRREGULAR,
};
use camera::{in_view, shoot, Camera, Code, Noise, Pose};
use image::GrayImage;

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

/// Decoding time per 1280x720 picture, over pictures with a code, with
/// nothing, with noise, and with a code that cannot be read. Only
/// meaningful in release, run on its own (not alongside the sweep, which
/// takes every core).
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

    let mut worst = (0f64, String::new());
    let mut by_kind: Vec<(&str, f64, u32)> = Vec::new();
    let mut time = |kind: &'static str, name: &str, img: &GrayImage| {
        read(img); // warm up
        let runs = 5;
        let start = Instant::now();
        for _ in 0..runs {
            std::hint::black_box(read(img));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        if ms > worst.0 {
            worst = (ms, name.to_string());
        }
        match by_kind.iter_mut().find(|k| k.0 == kind) {
            Some(k) => {
                k.1 += ms;
                k.2 += 1;
            }
            None => by_kind.push((kind, ms, 1)),
        }
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
    for (kind, total, count) in &by_kind {
        println!(
            "{kind}: mean {:.2} ms over {count} pictures",
            total / *count as f64
        );
    }
    println!("worst: {:.2} ms ({})", worst.0, worst.1);
    if !cfg!(debug_assertions) {
        assert!(
            worst.0 < 15.0,
            "worst picture took {:.2} ms ({})",
            worst.0,
            worst.1
        );
    }
}
