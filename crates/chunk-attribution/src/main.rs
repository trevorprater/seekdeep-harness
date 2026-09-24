//! Command-line entry for source-map chunk attribution.

use clap::Parser;

/// Attribute minified chunk units to source packages through its source map.
#[derive(Debug, Parser)]
struct Arguments {
    /// Built JavaScript chunk; its source map must be `<chunk>.map`.
    chunk: String,
    /// Maximum rows to print, matching JavaScript Number comparison semantics.
    #[arg(long)]
    top: Option<f64>,
}

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    print!(
        "{}",
        seekdeep_chunk_attribution::run(&arguments.chunk, arguments.top.unwrap_or(f64::NAN),)?
    );
    Ok(())
}
