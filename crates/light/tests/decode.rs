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
    /// Counted against the 98 percent bar on the normal preset.
    held_to_98: bool,
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
            held_to_98: true,
            apply: |_, _, _| {},
        },
        Condition {
            name: "rotation ±20°",
            held_to_98: true,
            apply: |r, p, _| p.roll = r.range(-20.0, 20.0),
        },
        Condition {
            name: "perspective 15-30°",
            held_to_98: true,
            apply: |r, p, _| p.tilt = r.range(15.0, 30.0),
        },
        Condition {
            name: "blur σ1.5",
            held_to_98: true,
            apply: |_, _, c| c.blur = 1.5,
        },
        Condition {
            name: "noise σ8",
            held_to_98: true,
            apply: |_, _, c| c.noise = 8.0,
        },
        Condition {
            name: "brightness gradient",
            held_to_98: true,
            apply: |_, _, c| {
                c.gradient = 0.6;
                c.vignette = 0.35;
            },
        },
        Condition {
            name: "gamma 0.5-2.2",
            held_to_98: true,
            apply: |r, _, c| {
                c.gamma = r.range(0.5, 2.2);
                c.black = r.range(0.05, 0.25);
                c.white = r.range(0.7, 0.95);
            },
        },
        Condition {
            name: "moiré",
            held_to_98: true,
            apply: |r, _, c| {
                c.moire = 0.15;
                c.moire_period = r.range(2.2, 4.0);
                c.moire_angle = r.range(0.0, 180.0);
            },
        },
        Condition {
            name: "scale 25-100%",
            held_to_98: true,
            apply: |r, p, _| {
                p.scale = r.range(0.25, 1.0);
                p.centre = (0.5, 0.5);
                p.tilt = r.range(0.0, 4.0);
            },
        },
        Condition {
            name: "off-centre",
            held_to_98: true,
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
            held_to_98: true,
            apply: |r, _, c| c.jpeg = Some(r.range(60.0, 80.0) as u8),
        },
        Condition {
            name: "mirrored",
            held_to_98: true,
            apply: |r, p, c| {
                c.mirror = true;
                p.roll = r.range(-10.0, 10.0);
            },
        },
        Condition {
            name: "hand shake",
            held_to_98: false,
            apply: |r, _, c| {
                c.motion = r.range(2.0, 5.0);
                c.motion_angle = r.range(0.0, 180.0);
            },
        },
        Condition {
            name: "dim",
            held_to_98: false,
            apply: |r, _, c| {
                c.black = 0.03;
                c.white = r.range(0.22, 0.3);
                c.noise = 5.0;
            },
        },
        Condition {
            name: "everything",
            held_to_98: false,
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
            },
        },
        Condition {
            name: "lens distortion",
            held_to_98: false,
            apply: |r, p, c| {
                c.k1 = r.range(-0.15, 0.05);
                p.scale = r.range(0.5, 0.8);
                p.centre = (r.range(0.3, 0.7), r.range(0.4, 0.6));
            },
        },
        // Beyond what the presets promise, to show where reading gives
        // out; reported, not held to a bar.
        Condition {
            name: "beyond: any rotation",
            held_to_98: false,
            apply: |r, p, _| p.roll = r.range(0.0, 360.0),
        },
        Condition {
            name: "beyond: perspective 30-45°",
            held_to_98: false,
            apply: |r, p, _| p.tilt = r.range(30.0, 45.0),
        },
        Condition {
            name: "beyond: blur σ2.5",
            held_to_98: false,
            apply: |_, _, c| c.blur = 2.5,
        },
        Condition {
            name: "beyond: noise σ16",
            held_to_98: false,
            apply: |_, _, c| c.noise = 16.0,
        },
        Condition {
            name: "beyond: 25-35% and blur σ1.5",
            held_to_98: false,
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
    let jobs: Vec<(usize, usize)> = (0..3)
        .flat_map(|p| (0..conds.len()).map(move |c| (p, c)))
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
            let t = results.iter().find(|r| r.0 == p && r.1 == c).unwrap().2;
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

fn check_sweep(results: &[(usize, usize, Tally)], bar: f64) {
    let conds = conditions();
    for &(p, c, t) in results {
        assert_eq!(
            t.wrong,
            0,
            "{} {}: wrong frames delivered",
            presets()[p].0,
            conds[c].name
        );
        if p == 1 && conds[c].held_to_98 {
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
    check_sweep(&results, 0.95);
}

/// The full sweep: 200 seeded samples per condition per preset, at least
/// 98 percent read on the normal preset for each listed condition.
#[test]
#[ignore = "slow; run in release with --ignored"]
fn success_rates_over_200_samples() {
    let results = sweep(200);
    println!("{}", table(&results));
    check_sweep(&results, 0.98);
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

#[test]
fn noise_blank_and_clutter_read_as_no_code() {
    let blank = GrayImage::from_pixel(1280, 720, image::Luma([128]));
    assert_eq!(read(&blank), ReadOutcome::NoCode);
    let black = GrayImage::from_pixel(1280, 720, image::Luma([0]));
    assert_eq!(read(&black), ReadOutcome::NoCode);
    let mut rng = Noise::new(5);
    let noise = GrayImage::from_fn(1280, 720, |_, _| image::Luma([rng.next() as u8]));
    assert_eq!(read(&noise), ReadOutcome::NoCode);
    // Rooms with no screen in them.
    for seed in 0..20 {
        assert_eq!(
            read(&shoot(&[], &Camera::default(), seed)),
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
    let mut pictures: Vec<(&str, GrayImage)> = Vec::new();
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
                pictures.push((name, shoot(&[c], &Camera::default(), i)));
            }
        }
    }
    for i in 0..10 {
        pictures.push(("room", shoot(&[], &Camera::default(), 100 + i)));
    }
    // Codes in view that cannot be read try every fit, the slowest case.
    for (i, (_, spec)) in presets().into_iter().enumerate() {
        let frame = some_frame(&spec, &mut rng);
        let mut hidden = code(&spec, &frame, Pose::default());
        let n = spec.cells as f64;
        hidden.occlude = Some((0.25 * n, 0.25 * n, 0.8 * n, 0.8 * n));
        pictures.push((
            "unreadable",
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
        pictures.push(("unreadable", shoot(&[blurred], &cam, 300 + i as u64)));
    }
    let mut n = Noise::new(62);
    pictures.push((
        "noise",
        GrayImage::from_fn(1280, 720, |_, _| image::Luma([n.next() as u8])),
    ));

    let mut worst = 0f64;
    let mut by_kind: Vec<(&str, f64, u32)> = Vec::new();
    for (kind, img) in &pictures {
        read(img); // warm up
        let runs = 5;
        let start = Instant::now();
        for _ in 0..runs {
            std::hint::black_box(read(img));
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        worst = worst.max(ms);
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
    println!("worst: {worst:.2} ms");
    if !cfg!(debug_assertions) {
        assert!(worst < 15.0, "worst picture took {worst:.2} ms");
    }
}
