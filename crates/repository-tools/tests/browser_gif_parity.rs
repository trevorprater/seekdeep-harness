//! Browser GIF argument, manifest, media-boundary, and compatibility parity.

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use seekdeep_repository_tools::browser_gif::{
    EncodeGifOptions, EncodeGifSummary, GifMediaBackend, MediaEncodeRequest,
    encode_gif_with_backend, ffconcat_quote, parse_durations, positive_float,
    render_concat_manifest, render_summary,
};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct FakeMedia {
    probes: VecDeque<Map<String, Value>>,
    output_bytes: usize,
    request: Option<MediaEncodeRequest>,
    manifest: Option<String>,
}

impl FakeMedia {
    fn new(probes: Vec<Map<String, Value>>, output_bytes: usize) -> Self {
        Self {
            probes: probes.into(),
            output_bytes,
            request: None,
            manifest: None,
        }
    }
}

impl GifMediaBackend for FakeMedia {
    fn probe(&mut self, path: &Path) -> Result<Map<String, Value>> {
        self.probes
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("unexpected probe for {}", path.display()))
    }

    fn encode(&mut self, request: &MediaEncodeRequest) -> Result<()> {
        self.manifest = Some(fs::read_to_string(&request.manifest)?);
        self.request = Some(request.clone());
        fs::write(&request.output, vec![b'g'; self.output_bytes])?;
        Ok(())
    }
}

fn stream(width: u64, height: u64, frames: u64, duration: f64) -> Map<String, Value> {
    json!({
        "width": width.to_string(),
        "height": height,
        "nb_frames": frames.to_string(),
        "duration": duration.to_string(),
        "r_frame_rate": "10/1"
    })
    .as_object()
    .unwrap()
    .clone()
}

fn fixture() -> (tempfile::TempDir, EncodeGifOptions) {
    let root = tempfile::tempdir().unwrap();
    let frames = root.path().join("frames");
    fs::create_dir(&frames).unwrap();
    fs::write(frames.join("01-second.png"), b"second").unwrap();
    fs::write(frames.join("00-first.png"), b"first").unwrap();
    fs::create_dir(frames.join("nested")).unwrap();
    fs::write(frames.join("nested/02-not-direct.png"), b"nested").unwrap();
    (
        root,
        EncodeGifOptions {
            frames,
            output: PathBuf::from("unused"),
            pattern: "*.png".to_owned(),
            durations: "1,2".to_owned(),
            fps: 10,
            max_width: 600,
            colors: 128,
            max_bytes: 5 * 1024 * 1024,
            force: false,
        },
    )
}

#[test]
fn numeric_duration_and_ffconcat_contract_is_exact() {
    assert_eq!(
        positive_float("1.25").unwrap().to_bits(),
        1.25_f64.to_bits()
    );
    for invalid in ["zero", "0", "-1", "NaN", "inf"] {
        assert!(positive_float(invalid).is_err(), "{invalid}");
    }
    assert_eq!(parse_durations(" 1.5 ", 3).unwrap(), [1.5, 1.5, 1.5]);
    assert_eq!(parse_durations("1, 2,3", 3).unwrap(), [1.0, 2.0, 3.0]);
    assert_eq!(
        parse_durations("1,2", 3).unwrap_err().to_string(),
        "--durations supplied 2 values for 3 frames"
    );
    assert!(parse_durations("1,,2", 3).is_err());
    assert_eq!(
        ffconcat_quote(Path::new("a'b.png")).unwrap(),
        "'a'\\''b.png'"
    );
    assert!(ffconcat_quote(Path::new("bad\nname.png")).is_err());

    let manifest = render_concat_manifest(
        &[PathBuf::from("00.png"), PathBuf::from("01.png")],
        &[1.0, 2.5],
    )
    .unwrap();
    assert_eq!(
        manifest,
        "ffconcat version 1.0\nfile '00.png'\nduration 1.000000\nfile '01.png'\nduration 2.500000\nfile '01.png'\n"
    );
}

#[test]
fn full_plan_orders_frames_encodes_and_verifies_the_summary() {
    let (root, mut options) = fixture();
    options.output = root.path().join("result/demo.gif");
    let mut media = FakeMedia::new(
        vec![
            stream(800, 600, 1, 0.0),
            stream(800, 600, 1, 0.0),
            stream(600, 450, 30, 3.0),
        ],
        42,
    );

    let summary = encode_gif_with_backend(&options, &mut media).unwrap();
    let expected_output = fs::canonicalize(root.path())
        .unwrap()
        .join("result/demo.gif");
    assert_eq!(summary.bytes, 42);
    assert_eq!(summary.duration_seconds.to_bits(), 3.0_f64.to_bits());
    assert_eq!(summary.encoded_frames, 30);
    assert_eq!(summary.fps, 10);
    assert_eq!(summary.width, 600);
    assert_eq!(summary.height, 450);
    assert_eq!(summary.source_frames, 2);
    assert_eq!(summary.output, expected_output.display().to_string());

    let request = media.request.unwrap();
    assert_eq!(request.expected_duration.to_bits(), 3.0_f64.to_bits());
    assert!(!request.force);
    assert_eq!(request.output, expected_output);
    assert_eq!(
        request.filters,
        "fps=10,scale='min(600,iw)':-2:flags=lanczos,split[base][palette_input];[palette_input]palettegen=max_colors=128:stats_mode=full[palette];[base][palette]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle"
    );
    let manifest = media.manifest.unwrap();
    assert!(manifest.find("00-first.png").unwrap() < manifest.find("01-second.png").unwrap());
    assert!(manifest.ends_with("01-second.png'\n"));

    let rendered = render_summary(&summary).unwrap();
    assert!(rendered.starts_with("{\n  \"bytes\": 42,"));
    assert!(rendered.ends_with("\n}\n"));
    let unicode = render_summary(&EncodeGifSummary {
        output: "演示😀.gif".to_owned(),
        ..summary
    })
    .unwrap();
    assert!(unicode.contains("\\u6f14\\u793a\\ud83d\\ude00.gif"));
}

#[test]
fn validation_rejects_input_shape_and_every_output_bound() {
    let (root, mut options) = fixture();
    options.output = root.path().join("demo.txt");
    let mut unused = FakeMedia::new(Vec::new(), 0);
    assert!(
        encode_gif_with_backend(&options, &mut unused)
            .unwrap_err()
            .to_string()
            .contains("output must end in .gif")
    );

    options.output = root.path().join("demo.gif");
    options.colors = 3;
    assert_eq!(
        encode_gif_with_backend(&options, &mut unused)
            .unwrap_err()
            .to_string(),
        "--colors must be between 4 and 256"
    );
    options.colors = 128;
    options.fps = 31;
    assert_eq!(
        encode_gif_with_backend(&options, &mut unused)
            .unwrap_err()
            .to_string(),
        "--fps must not exceed 30"
    );

    options.fps = 10;
    let mut mismatched =
        FakeMedia::new(vec![stream(800, 600, 1, 0.0), stream(801, 600, 1, 0.0)], 0);
    assert!(
        encode_gif_with_backend(&options, &mut mismatched)
            .unwrap_err()
            .to_string()
            .contains("all frames must have identical dimensions")
    );

    let variants = [
        (stream(601, 450, 30, 3.0), 42, "expected width at most 600"),
        (stream(600, 450, 1, 3.0), 42, "expected an animated GIF"),
        (stream(600, 450, 30, 9.0), 42, "expected about 3.000s"),
        (stream(600, 450, 30, 3.0), 43, "above --max-bytes 42"),
    ];
    for (output_stream, bytes, expected) in variants {
        if options.output.exists() {
            fs::remove_file(&options.output).unwrap();
        }
        options.max_bytes = 42;
        let mut media = FakeMedia::new(
            vec![
                stream(800, 600, 1, 0.0),
                stream(800, 600, 1, 0.0),
                output_stream,
            ],
            bytes,
        );
        assert!(
            encode_gif_with_backend(&options, &mut media)
                .unwrap_err()
                .to_string()
                .contains(expected),
            "{expected}"
        );
    }
}

#[test]
fn command_exposes_the_rust_owned_entry_path() {
    let help = Command::new(env!("CARGO_BIN_EXE_encode-browser-gif"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Encode lexically ordered browser screenshots"));
    assert!(help.contains("--durations"));
    assert!(help.contains("--max-bytes"));

    let invalid = Command::new(env!("CARGO_BIN_EXE_encode-browser-gif"))
        .args(["frames", "demo.gif", "--fps", "0"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("expected a positive integer")
    );
}
