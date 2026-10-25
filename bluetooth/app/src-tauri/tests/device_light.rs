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
use bombechat_node::{NodeEvent, ReaderWatch, TILT_OR_DIM};
use bombechat_store::Channel;
use image::{GrayImage, Luma};
use serde_json::json;
use tempfile::TempDir;

/// A machine over a quiet fake sound card and a fake camera, ticked by
/// hand every 20 ms; after each tick it waits for the camera thread to
/// finish what it was given, so what the next tick takes is fixed.
struct Bench {
    s: Shared,
    watch: ReaderWatch,
    now: u64,
}

impl Bench {
    fn new(path: &Path, cam: &FakeCamera) -> Bench {
        let device = Device::with_audio_and_camera(
            path.to_path_buf(),
            Box::new(FakeAudio::new(48_000)),
            Box::new(cam.clone()),
        );
        let watch = device.light_watch().expect("a light channel");
        Bench {
            s: Shared::new(Box::new(device)),
            watch,
            now: 0,
        }
    }

    /// One tick, then the camera thread caught up.
    fn step(&mut self) -> Vec<NodeEvent> {
        self.now += 20;
        let events = commands::tick(&self.s, self.now);
        let until = Instant::now() + Duration::from_secs(10);
        while !self.watch.idle() {
            assert!(Instant::now() < until, "the camera thread never caught up");
            thread::sleep(Duration::from_millis(1));
        }
        events
    }

    /// `n` ticks, and what they reported.
    fn steps(&mut self, n: usize) -> Vec<NodeEvent> {
        (0..n).flat_map(|_| self.step()).collect()
    }

    fn light(&self) -> serde_json::Value {
        light(&self.s)
    }

    fn set(&self, id: &str, value: usize) {
        commands::set_setting(&self.s, id.into(), value).unwrap();
    }
}

fn store(dir: &TempDir) -> PathBuf {
    dir.path().join("bombechat.db")
}

/// Until the camera thread has noticed `cam` failed and closed it: it
/// checks at least every 100 ms.
fn wait_until_closed(cam: &FakeCamera) {
    let until = Instant::now() + Duration::from_secs(10);
    while cam.is_running() {
        assert!(Instant::now() < until, "the failure was never noticed");
        thread::sleep(Duration::from_millis(5));
    }
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
    let mut b = Bench::new(&store(&dir), &cam);
    b.steps(100);
    assert_eq!(cam.starts(), 0, "the camera was never opened");
    assert_eq!(b.light()["state"], "off");
}

#[test]
fn the_light_switch_opens_and_closes_the_camera_and_is_kept() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let mut b = Bench::new(&store(&dir), &cam);
    b.step();
    assert_eq!(commands::settings(&b.s)["light"], 0, "Off until turned");

    b.set("light", 1);
    assert_eq!(cam.starts(), 0, "not opened by the command itself");
    b.steps(2);
    assert!(cam.is_running());
    assert_eq!(b.light()["state"], "listening");

    b.set("light", 0);
    b.step();
    assert!(!cam.is_running());
    assert_eq!(cam.stops(), 1, "closed, so its light goes off");
    assert_eq!(b.light()["state"], "off");

    b.set("light", 2);
    drop(b);
    let mut again = Bench::new(&store(&dir), &cam);
    assert_eq!(
        commands::settings(&again.s)["light"],
        2,
        "kept for next launch"
    );
    again.steps(2);
    assert!(cam.is_running());
}

#[test]
fn a_camera_the_person_has_not_allowed_shows_as_unavailable() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::denied();
    let mut b = Bench::new(&store(&dir), &cam);
    b.set("light", 1);
    b.steps(2);
    assert_eq!(b.light()["state"], "unavailable: camera permission denied");
    b.steps(100);
    assert_eq!(
        cam.starts(),
        1,
        "not asked again until the switch is turned"
    );
}

#[test]
fn a_camera_taken_by_another_app_shows_as_unavailable_until_it_is_back() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let mut b = Bench::new(&store(&dir), &cam);
    b.set("light", 1);
    b.steps(2);
    assert_eq!(b.light()["state"], "listening");
    // Taken, and still taken at the first try a second later.
    cam.refuse_starts(1, CameraError::Interrupted);
    cam.fail(CameraError::Interrupted);
    wait_until_closed(&cam);
    b.step();
    assert_eq!(
        b.light()["state"],
        "unavailable: camera in use by another app"
    );
    b.steps(55);
    assert_eq!(cam.starts(), 2, "tried again after a second");
    assert_eq!(
        b.light()["state"],
        "unavailable: camera in use by another app"
    );
    // Let go of: back, with no switch turned.
    let events = b.steps(110);
    assert_eq!(cam.starts(), 3, "and again two seconds after that");
    assert!(cam.is_running());
    assert_eq!(b.light()["state"], "listening");
    assert!(events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelState { channel: Channel::Light, state } if state == "listening"
    )));
}

#[test]
fn the_flash_speed_sets_the_light_rate_and_is_kept() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let b = Bench::new(&store(&dir), &cam);
    assert_eq!(commands::settings(&b.s)["flash"], 1);
    assert_eq!(b.light()["rate"], json!(2290.0));
    b.set("flash", 0);
    assert_eq!(b.light()["rate"], json!(948.0));
    b.set("flash", 2);
    assert_eq!(b.light()["rate"], json!(4680.0));
    drop(b);
    let again = Bench::new(&store(&dir), &cam);
    assert_eq!(again.light()["rate"], json!(4680.0), "Fast, as kept");
}

#[test]
fn a_notice_by_light_is_handed_to_the_page_as_frames_at_the_flash_speed() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let mut b = Bench::new(&store(&dir), &cam);
    commands::unlock(&b.s, "correct horse".into(), Some("Ana".into())).unwrap();
    b.set("flash", 0);

    // At Read, refused as the switch says.
    b.set("light", 1);
    commands::broadcast(&b.s, "Water at the church.".into(), "light".into()).unwrap();
    let events: Vec<_> = b.step().iter().map(commands::bridge_event).collect();
    assert!(events.contains(&(
        "channel",
        json!({ "channel": "light", "state": "not sent: light is set to read only" })
    )));
    assert!(!events.iter().any(|(name, _)| *name == "display"));

    // At Read and flash, shown: Slow is 6 frames a second of a 48-cell
    // grid, for the page's still moment (1.2 s, 8 frames at 6 a second)
    // and then at least four seconds, then the screen clears.
    b.set("light", 2);
    commands::broadcast(&b.s, "Water at the church.".into(), "light".into()).unwrap();
    let mut shown = Vec::new();
    let mut stopped = None;
    for e in b.steps(300) {
        match commands::bridge_event(&e) {
            ("display", d) => shown.push(d),
            ("displayStopped", d) => stopped = Some(d),
            _ => {}
        }
    }
    assert_eq!(
        shown.len(),
        8 + 24,
        "the still moment, then 4 s at 6 a second"
    );
    assert!(shown.iter().all(|d| d["spec"] == 0
        && d["bits"].as_array().unwrap().len() == 48 * 48 / 8
        && d["tag"] == shown[0]["tag"]));
    assert_eq!(shown[31]["index"], 31);
    assert_eq!(stopped, Some(json!({ "tag": shown[0]["tag"] })));
}

#[test]
fn escape_stops_flashing_even_when_the_setting_cannot_be_saved() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let mut b = Bench::new(&store(&dir), &cam);
    commands::unlock(&b.s, "correct horse".into(), Some("Ana".into())).unwrap();
    b.set("light", 2);
    commands::broadcast(&b.s, "Water at the church.".into(), "light".into()).unwrap();
    let shown = |events: Vec<NodeEvent>| {
        events
            .iter()
            .map(commands::bridge_event)
            .filter(|(name, _)| *name == "display")
            .count()
    };
    assert!(shown(b.steps(20)) > 0, "flashing");

    // The settings file can't be written: a folder stands where it goes.
    let file = dir.path().join("settings.json");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();

    // Turned up, refused and left as it was.
    let why = commands::set_setting(&b.s, "flash".into(), 2).unwrap_err();
    assert!(why.starts_with("The setting was not saved"), "{why}");
    assert_eq!(commands::settings(&b.s)["flash"], 1);

    // Turned down to Read (Escape): not saved, but in effect, so the
    // screen clears and nothing more is shown.
    let why = commands::set_setting(&b.s, "light".into(), 1).unwrap_err();
    assert!(why.starts_with("The setting was not saved"), "{why}");
    assert_eq!(
        commands::settings(&b.s)["light"],
        1,
        "Read, for this session"
    );
    let after: Vec<_> = b.steps(300).iter().map(commands::bridge_event).collect();
    assert!(after.iter().any(|(name, _)| *name == "displayStopped"));
    assert!(
        !after.iter().any(|(name, _)| *name == "display"),
        "nothing flashed after Escape"
    );
    assert_eq!(b.light()["state"], "listening", "still reading");

    // Turned back up while the file still can't be written: refused, and
    // the screen stays clear.
    assert!(commands::set_setting(&b.s, "light".into(), 2).is_err());
    assert_eq!(commands::settings(&b.s)["light"], 1);
    assert_eq!(shown(b.steps(50)), 0);
}

#[test]
fn the_node_allows_for_the_pages_still_moment() {
    // The page holds a still field for READY_MS before the first code
    // frame; the node lengthens the window by STILL_MS for it. They agree.
    let page =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/bombechat.html"))
            .unwrap();
    let ready = page
        .split("const READY_MS=")
        .nth(1)
        .and_then(|rest| rest.split([',', ';']).next())
        .and_then(|value| value.rsplit(':').next())
        .and_then(|ms| ms.trim().parse::<u32>().ok())
        .expect("READY_MS in bombechat.html");
    assert_eq!(ready, bombechat_node::STILL_MS);
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
    let mut b = Bench::new(&store(&dir), &cam);
    b.set("light", 1);
    b.steps(2);
    assert!(cam.show_next());
    // the picture is read on the camera thread, taken on the next tick
    while !b.watch.idle() {
        thread::sleep(Duration::from_millis(1));
    }
    let events = b.step();
    assert_eq!(b.light()["state"], TILT_OR_DIM);
    assert!(events.iter().any(|e| matches!(
        e,
        NodeEvent::ChannelState { channel: Channel::Light, state } if state == TILT_OR_DIM
    )));
    // With no unreadable code in view for three seconds, it gives way.
    b.steps(140);
    assert_eq!(b.light()["state"], TILT_OR_DIM, "not yet");
    b.steps(20);
    assert_eq!(b.light()["state"], "listening");
}

#[test]
fn pairing_by_light_shows_the_hello_and_never_turns_the_switch() {
    let dir = TempDir::new().unwrap();
    let cam = FakeCamera::new(Vec::new());
    let mut b = Bench::new(&store(&dir), &cam);
    commands::unlock(&b.s, "correct horse".into(), Some("Ana".into())).unwrap();

    // At Off (and at Read), refused as the switch says: the camera stays
    // closed and the switch where it was.
    b.step();
    assert_eq!(
        commands::pair(&b.s, "light".into()),
        Err("not sent: light is off".to_string())
    );
    b.steps(5);
    assert_eq!(cam.starts(), 0, "pairing never opens the camera");
    assert_eq!(commands::settings(&b.s)["light"], 0);
    b.set("light", 1);
    assert_eq!(
        commands::pair(&b.s, "light".into()),
        Err("not sent: light is set to read only".to_string())
    );
    assert_eq!(commands::settings(&b.s)["light"], 1);

    // At Read and flash, the hello goes up and keeps showing, past one
    // window, while the camera reads.
    b.set("light", 2);
    commands::pair(&b.s, "light".into()).unwrap();
    let shown: Vec<_> = b
        .steps(300)
        .iter()
        .map(commands::bridge_event)
        .filter(|(name, _)| *name == "display" || *name == "displayStopped")
        .collect();
    assert!(shown.len() > 50, "shown throughout: {}", shown.len());
    assert!(shown.iter().all(|(name, _)| *name == "display"));
    assert!(cam.is_running());

    // Stopped, the screen clears at once; the camera reads on, as the
    // switch has it.
    commands::stop_pairing(&b.s);
    let after: Vec<_> = b
        .steps(100)
        .iter()
        .map(commands::bridge_event)
        .filter(|(name, _)| *name == "display" || *name == "displayStopped")
        .map(|(name, _)| name)
        .collect();
    assert_eq!(after, vec!["displayStopped"]);
    assert!(cam.is_running());
    assert_eq!(commands::settings(&b.s)["light"], 2);
}
