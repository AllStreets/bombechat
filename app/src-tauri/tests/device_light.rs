//! The real machine over a fake camera: what the Light switch and Flash
//! speed settings do, how a camera that will not open shows, and what
//! the page is handed to draw.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use bombechat_app::commands::{self, Shared};
use bombechat_app::Device;
use bombechat_light::{encode, render, CameraError, CodeSpec, FakeCamera};
use bombechat_modem::FakeAudio;
use bombechat_node::{NodeEvent, TILT_OR_DIM};
use bombechat_store::Channel;
use image::{GrayImage, Luma};
use serde_json::json;
use tempfile::TempDir;

/// A machine over a quiet fake sound card and `cam`, its store at `path`.
fn machine(path: &Path, cam: &FakeCamera) -> Shared {
    Shared::new(Box::new(Device::with_audio_and_camera(
        path.to_path_buf(),
        Box::new(FakeAudio::new(48_000)),
        Box::new(cam.clone()),
    )))
}

fn store(dir: &TempDir) -> PathBuf {
    dir.path().join("bombechat.db")
}

/// Tick every 20 ms from `*now` until `done` or ten seconds of wall
/// time pass (the camera opens and reads on a thread of its own), and
/// what the ticks reported.
fn tick_until(s: &Shared, now: &mut u64, done: impl Fn(&Shared) -> bool) -> Vec<NodeEvent> {
    let until = Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    while !done(s) {
        assert!(Instant::now() < until, "never happened");
        *now += 20;
        events.extend(commands::tick(s, *now));
        thread::sleep(Duration::from_millis(2));
    }
    events
}

fn light(s: &Shared) -> serde_json::Value {
    serde_json::to_value(commands::channels(s))
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["channel"] == "light")
        .cloned()
        .expect("a light channel")
}

#[test]
fn a_persisted_light_off_never_opens_the_camera() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("settings.json"), r#"{"light":0}"#).unwrap();
    let cam = FakeCamera::new(Vec::new());
    let s = machine(&store(&dir), &cam);
    for now in (0..2_000).step_by(20) {
        commands::tick(&s, now);
    }
    thread::sleep(Duration::from_millis(50));
    assert_eq!(cam.starts(), 0, "the camera was never opened");
    assert_eq!(light(&s)["state"], "off");
}

#[test]
fn the_light_switch_opens_and_closes_the_camera_and_is_kept() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let s = machine(&store(&dir), &cam);
    let mut now = 0;
    commands::tick(&s, now);
    assert_eq!(commands::settings(&s)["light"], 0, "Off until turned");

    commands::set_setting(&s, "light".into(), 1).unwrap();
    assert_eq!(cam.starts(), 0, "not opened by the command itself");
    tick_until(&s, &mut now, |_| cam.is_running());
    tick_until(&s, &mut now, |s| light(s)["state"] == "listening");

    commands::set_setting(&s, "light".into(), 0).unwrap();
    tick_until(&s, &mut now, |_| !cam.is_running());
    assert_eq!(cam.stops(), 1, "closed, so its light goes off");
    tick_until(&s, &mut now, |s| light(s)["state"] == "off");

    commands::set_setting(&s, "light".into(), 2).unwrap();
    drop(s);
    let again = machine(&store(&dir), &cam);
    assert_eq!(
        commands::settings(&again)["light"],
        2,
        "kept for next launch"
    );
    tick_until(&again, &mut now, |_| cam.is_running());
}

#[test]
fn a_camera_the_person_has_not_allowed_shows_as_unavailable() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::denied();
    let s = machine(&store(&dir), &cam);
    commands::set_setting(&s, "light".into(), 1).unwrap();
    let mut now = 0;
    tick_until(&s, &mut now, |s| {
        light(s)["state"] == "unavailable: camera permission denied"
    });
}

#[test]
fn a_camera_taken_by_another_app_shows_as_unavailable() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let s = machine(&store(&dir), &cam);
    commands::set_setting(&s, "light".into(), 1).unwrap();
    let mut now = 0;
    tick_until(&s, &mut now, |_| cam.is_running());
    cam.fail(CameraError::Interrupted);
    tick_until(&s, &mut now, |s| {
        light(s)["state"] == "unavailable: camera in use by another app"
    });
}

#[test]
fn the_flash_speed_sets_the_light_rate_and_is_kept() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let s = machine(&store(&dir), &cam);
    assert_eq!(commands::settings(&s)["flash"], 1);
    assert_eq!(light(&s)["rate"], json!(2290.0));
    commands::set_setting(&s, "flash".into(), 0).unwrap();
    assert_eq!(light(&s)["rate"], json!(948.0));
    commands::set_setting(&s, "flash".into(), 2).unwrap();
    assert_eq!(light(&s)["rate"], json!(4680.0));
    drop(s);
    let again = machine(&store(&dir), &cam);
    assert_eq!(light(&again)["rate"], json!(4680.0), "Fast, as kept");
}

#[test]
fn a_notice_by_light_is_handed_to_the_page_as_frames_at_the_flash_speed() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let s = machine(&store(&dir), &cam);
    commands::unlock(&s, "correct horse".into(), Some("Ana".into())).unwrap();
    commands::set_setting(&s, "flash".into(), 0).unwrap();

    // At Read, refused as the switch says.
    commands::set_setting(&s, "light".into(), 1).unwrap();
    commands::broadcast(&s, "Water at the church.".into(), "light".into()).unwrap();
    let events: Vec<_> = commands::tick(&s, 0)
        .iter()
        .map(commands::bridge_event)
        .collect();
    assert!(events.contains(&(
        "channel",
        json!({ "channel": "light", "state": "not sent: light is set to read only" })
    )));
    assert!(!events.iter().any(|(name, _)| *name == "display"));

    // At Read and flash, shown: Slow is 6 frames a second of a 48-cell
    // grid, for at least four seconds, then the screen clears.
    commands::set_setting(&s, "light".into(), 2).unwrap();
    commands::broadcast(&s, "Water at the church.".into(), "light".into()).unwrap();
    let mut shown = Vec::new();
    let mut stopped = None;
    for now in (20..6_000).step_by(20) {
        for e in commands::tick(&s, now) {
            match commands::bridge_event(&e) {
                ("display", d) => shown.push(d),
                ("displayStopped", d) => stopped = Some(d),
                _ => {}
            }
        }
    }
    assert_eq!(shown.len(), 24, "4 s at 6 a second");
    assert!(shown.iter().all(|d| d["spec"] == 0
        && d["bits"].as_array().unwrap().len() == 48 * 48 / 8
        && d["tag"] == shown[0]["tag"]));
    assert_eq!(shown[23]["index"], 23);
    assert_eq!(stopped, Some(json!({ "tag": shown[0]["tag"] })));
}

/// A picture of a light code with a finger over its middle: in view, but
/// not readable.
fn covered_code() -> GrayImage {
    let spec = CodeSpec::normal();
    let frame = encode(&spec, b"hi", 3).next_frame();
    let code = render(&spec, &frame, 480);
    let mut picture = GrayImage::from_pixel(800, 600, Luma([128]));
    image::imageops::overlay(&mut picture, &code, 160, 60);
    for (x, y, p) in picture.enumerate_pixels_mut() {
        if (300..500).contains(&x) && (200..400).contains(&y) {
            *p = Luma([90]);
        }
    }
    picture
}

#[test]
fn a_code_that_cannot_be_read_asks_to_tilt_or_dim_the_screen() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(vec![covered_code()]);
    let s = machine(&store(&dir), &cam);
    commands::set_setting(&s, "light".into(), 1).unwrap();
    let mut now = 0;
    tick_until(&s, &mut now, |_| cam.is_running());
    assert!(cam.show_next());
    let events = tick_until(&s, &mut now, |s| light(s)["state"] == TILT_OR_DIM);
    assert!(events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelState { channel: Channel::Light, state } if state == TILT_OR_DIM
    )));
    // With no unreadable code in view for a while, it gives way.
    tick_until(&s, &mut now, |s| light(s)["state"] == "listening");
}
