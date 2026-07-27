//! Command-line IPSMAP builder for palette authoring pipelines.

use pixel_stream::palette_lut::{
    build_lookup, load_palette_config, sibling_lut_path, write_lookup,
};
use std::{env, path::PathBuf, time::Instant};

fn main() {
    let mut palette_config_path = None;
    let mut output_path = None;

    for arg in env::args().skip(1) {
        if let Some(path) = arg.strip_prefix("--palette-config=") {
            palette_config_path = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--output=") {
            output_path = Some(PathBuf::from(path));
        } else {
            eprintln!("Unknown argument: {arg}");
            eprintln!(
                "Usage: cargo run --release --bin ipsc_build_palette_lut -- --palette-config=path/to/palette.toml [--output=path/to/palette.ipsmap]"
            );
            std::process::exit(1);
        }
    }

    let Some(palette_config_path) = palette_config_path else {
        eprintln!("Missing required --palette-config=path/to/palette.toml");
        eprintln!(
            "Usage: cargo run --release --bin ipsc_build_palette_lut -- --palette-config=path/to/palette.toml [--output=path/to/palette.ipsmap]"
        );
        std::process::exit(1);
    };
    let output_path = output_path.unwrap_or_else(|| sibling_lut_path(&palette_config_path));
    let start = Instant::now();

    let config = match load_palette_config(&palette_config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!(
                "Could not load palette config {}: {err}",
                palette_config_path.display()
            );
            std::process::exit(1);
        }
    };

    println!(
        "Building LUT for {} colors from {}",
        config.colors.len(),
        palette_config_path.display()
    );

    let entries = build_lookup(&config);

    if let Err(err) = write_lookup(&output_path, &config, &entries) {
        eprintln!("Could not write LUT {}: {err}", output_path.display());
        std::process::exit(1);
    }

    println!("Wrote {} in {:.2?}", output_path.display(), start.elapsed());
}
