//! Encode lexically ordered browser screenshots into a verified GIF.

use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use seekdeep_repository_tools::browser_gif::{
    DEFAULT_MAX_BYTES, EncodeGifOptions, encode_gif, render_summary,
};

#[derive(Debug, Parser)]
#[command(about = "Encode lexically ordered browser screenshots into a verified GIF.")]
struct Args {
    /// Directory containing lexically ordered frames.
    frames: PathBuf,
    /// Output `.gif` path.
    output: PathBuf,
    /// Frame glob within the input directory.
    #[arg(long, default_value = "*.png")]
    pattern: String,
    /// One hold duration or one comma-separated value per frame.
    #[arg(long, default_value = "2", allow_hyphen_values = true)]
    durations: String,
    /// Encoded frames per second.
    #[arg(long, default_value_t = 10, allow_hyphen_values = true, value_parser = positive_integer)]
    fps: u64,
    /// Maximum output width.
    #[arg(long, default_value_t = 1200, allow_hyphen_values = true, value_parser = positive_integer)]
    max_width: u64,
    /// Palette colors, from 4 through 256.
    #[arg(long, default_value_t = 128, allow_hyphen_values = true, value_parser = positive_integer)]
    colors: u64,
    /// Maximum output size.
    #[arg(long, default_value_t = DEFAULT_MAX_BYTES, allow_hyphen_values = true, value_parser = positive_integer)]
    max_bytes: u64,
    /// Replace an existing output file.
    #[arg(long)]
    force: bool,
}

fn positive_integer(value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<i128>()
        .map_err(|_| format!("expected an integer, got {value:?}"))?;
    if parsed <= 0 {
        return Err(format!("expected a positive integer, got {value:?}"));
    }
    u64::try_from(parsed).map_err(|_| format!("expected an integer, got {value:?}"))
}

fn main() -> ExitCode {
    let args = Args::parse();
    let options = EncodeGifOptions {
        frames: args.frames,
        output: args.output,
        pattern: args.pattern,
        durations: args.durations,
        fps: args.fps,
        max_width: args.max_width,
        colors: args.colors,
        max_bytes: args.max_bytes,
        force: args.force,
    };
    match encode_gif(&options).and_then(|summary| render_summary(&summary)) {
        Ok(summary) => {
            print!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
