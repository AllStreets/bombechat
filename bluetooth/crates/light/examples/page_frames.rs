//! The front page's flashing view, checked against the core: the page
//! draws the grid bits the node hands it, and a camera on another laptop
//! must read back exactly the frame those bits carry. `tools/check.cjs`
//! runs this example twice:
//!
//! ```text
//! cargo run -q -p bombechat-light --example page_frames -- frames <preset> <count> <tag>
//! cargo run -q -p bombechat-light --example page_frames -- read <picture.png>...
//! ```
//!
//! `frames` codes a seeded envelope as the light transport does and
//! prints `count` frames as JSON: for each, the grid bits the node sends
//! the page (`display`'s `bits`) and the frame bytes they carry, in hex.
//! `read` reads each picture (a screenshot of the page) with the
//! camera's decoder and prints what it found, one JSON line a picture:
//! the frame bytes of each code read, in hex, or `"no code"` /
//! `"unreadable"`.

use std::time::Duration;

use bombechat_light::{encode, grid_bits, read_within, CodeSpec, ReadOutcome};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("frames") => {
            let num = |i: usize| -> u64 {
                args.get(i)
                    .and_then(|a| a.parse().ok())
                    .unwrap_or_else(|| panic!("frames <preset> <count> <tag>"))
            };
            let spec = CodeSpec::from_preset(num(1) as u8).expect("a preset: 0, 1 or 2");
            let (count, tag) = (num(2), num(3) as u16);
            // an envelope-sized run of bytes, the same every time
            let mut x = 0x2545_f491_u32;
            let envelope: Vec<u8> = (0..1500)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let mut encoder = encode(&spec, &envelope, tag);
            let frames: Vec<String> = (0..count)
                .map(|_| {
                    let frame = encoder.next_frame();
                    let bits = grid_bits(&spec, &frame);
                    format!("{{\"bits\":{bits:?},\"frame\":\"{}\"}}", hex(&frame))
                })
                .collect();
            println!(
                "{{\"spec\":{},\"cells\":{},\"frames\":[{}]}}",
                spec.preset().unwrap_or(0),
                spec.cells,
                frames.join(",")
            );
        }
        Some("read") => {
            for path in &args[1..] {
                let picture = image::open(path)
                    .unwrap_or_else(|e| panic!("{path}: {e}"))
                    .into_luma8();
                // an unoptimised build reads slowly: give it all the time it needs
                let found = match read_within(&picture, Duration::from_secs(20)) {
                    ReadOutcome::NoCode => "\"no code\"".to_string(),
                    ReadOutcome::Unreadable => "\"unreadable\"".to_string(),
                    ReadOutcome::Frames(frames) => format!(
                        "[{}]",
                        frames
                            .iter()
                            .map(|f| format!("\"{}\"", hex(f)))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                };
                println!("{found}");
            }
        }
        _ => {
            eprintln!("page_frames frames <preset> <count> <tag> | page_frames read <png>...");
            std::process::exit(2);
        }
    }
}
