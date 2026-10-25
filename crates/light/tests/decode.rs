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
use camera::textures::{blinds, checkerboard, diagonal_stripes, finder_stripes, keyboard, qr_like};
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

/// Pictures wider than 1280 px are read at reduced size.
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
    let spec = CodeSpec::normal();
    let frame = some_frame(&spec, &mut rng);
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
    for (kind, name, img) in &pictures {
        read(img); // warm up
        let runs = 5;
        let start = Instant::now();
        for _ in 0..runs {
            std::hint::black_box(read(img));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        if ms > worst.0 {
            worst = (ms, name.clone());
        }
        match by_kind.iter_mut().find(|k| k.0 == *kind) {
            Some(k) => {
                k.1 += ms;
                k.2 += 1;
            }
            None => by_kind.push((kind, ms, 1)),
        }
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
