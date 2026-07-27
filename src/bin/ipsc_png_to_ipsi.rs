//! Command-line PNG to IPSI converter.
//!
//! The converter downscales, palette-matches, and optionally dithers source
//! artwork into the indexed sprite format used by PixelStream overlays.

use image::ImageReader;
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

const DEFAULT_DITHER_STRENGTH: f32 = 0.75;
const SOLVE_2X2_CANDIDATES: usize = 10;
const DEFAULT_SIZE: OutputSize = OutputSize {
    width: 128,
    height: 128,
};

fn main() {
    let args = match Args::parse(env::args().skip(1).collect()) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "Usage: cargo run --bin ipsc_png_to_ipsi -- <input.png> <output.ipsi> <palette.toml>"
            );
            eprintln!(
                "   or: cargo run --bin ipsc_png_to_ipsi -- <input.png> --output output.ipsi --palette palette.toml --size 128x128 --downscale solve2x2-hue --no-dither"
            );
            return;
        }
    };

    match convert_png_to_ipsi(
        &args.input,
        &args.output,
        &args.palette,
        args.size,
        args.dither_strength,
        args.downscale_mode,
    ) {
        Ok(report) => {
            eprintln!(
                "Wrote {} ({}x{}, {} palette colors, OKLab matched)",
                report.output.display(),
                report.width,
                report.height,
                report.palette_len
            );
            eprintln!("Palette: {}", report.palette.display());
            eprintln!("Dither strength: {}", report.dither_strength);
            eprintln!("Downscale mode: {}", report.downscale_mode.name());
            eprintln!("Used {} palette entries", report.used_palette_entries);
        }
        Err(err) => eprintln!("Could not convert PNG to IPSI: {err}"),
    }
}

struct Args {
    input: PathBuf,
    output: PathBuf,
    palette: PathBuf,
    size: Option<OutputSize>,
    dither_strength: f32,
    downscale_mode: DownscaleMode,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Self, String> {
        let mut input = None;
        let mut output = None;
        let mut palette = None;
        let mut size = Some(DEFAULT_SIZE);
        let mut dither_strength = DEFAULT_DITHER_STRENGTH;
        let mut downscale_mode = DownscaleMode::OklchAverage;
        let mut positional = Vec::new();
        let mut index = 0;

        while index < raw.len() {
            match raw[index].as_str() {
                "--palette" | "-p" => {
                    index += 1;
                    palette = raw.get(index).map(PathBuf::from);
                }
                "--output" | "-o" => {
                    index += 1;
                    output = raw.get(index).map(PathBuf::from);
                }
                "--size" | "-s" => {
                    index += 1;
                    size =
                        Some(parse_size(raw.get(index).ok_or_else(|| {
                            "--size needs a value like 128x128".to_owned()
                        })?)?);
                }
                "--original-size" => {
                    size = None;
                }
                "--dither-strength" => {
                    index += 1;
                    dither_strength = parse_dither_strength(raw.get(index).ok_or_else(|| {
                        "--dither-strength needs a number from 0 to 1".to_owned()
                    })?)?;
                }
                "--no-dither" => {
                    dither_strength = 0.0;
                }
                "--downscale" => {
                    index += 1;
                    downscale_mode = parse_downscale_mode(raw.get(index).ok_or_else(|| {
                        "--downscale needs average, majority, minority, solve2x2, or solve2x2-hue".to_owned()
                    })?)?;
                }
                "--help" | "-h" => return Err("Convert a PNG to an IPSI indexed image.".to_owned()),
                value if value.starts_with('-') => {
                    return Err(format!("unknown option: {value}"));
                }
                value => positional.push(PathBuf::from(value)),
            }
            index += 1;
        }

        if input.is_none() {
            input = positional.first().cloned();
        }
        if output.is_none() {
            output = positional.get(1).cloned();
        }
        if palette.is_none() {
            palette = positional.get(2).cloned();
        }

        Ok(Self {
            input: input.ok_or_else(|| "missing input PNG path".to_owned())?,
            output: output.ok_or_else(|| "missing output IPSI path".to_owned())?,
            palette: palette.ok_or_else(|| "missing palette TOML path".to_owned())?,
            size,
            dither_strength,
            downscale_mode,
        })
    }
}

#[derive(Clone, Copy)]
struct OutputSize {
    width: u32,
    height: u32,
}

struct ConversionReport {
    palette: PathBuf,
    output: PathBuf,
    width: u32,
    height: u32,
    palette_len: usize,
    dither_strength: f32,
    downscale_mode: DownscaleMode,
    used_palette_entries: usize,
}

fn convert_png_to_ipsi(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    palette_path: impl AsRef<Path>,
    size: Option<OutputSize>,
    dither_strength: f32,
    downscale_mode: DownscaleMode,
) -> Result<ConversionReport, String> {
    let input = input.as_ref();
    let output = output.as_ref();
    let palette_path = palette_path.as_ref();
    let palette = load_palette(palette_path)?;
    let palette_oklab = palette
        .iter()
        .map(|[r, g, b, _]| rgb_to_oklab(*r, *g, *b))
        .collect::<Vec<_>>();

    let image = ImageReader::open(input)
        .map_err(|err| err.to_string())?
        .with_guessed_format()
        .map_err(|err| err.to_string())?
        .decode()
        .map_err(|err| err.to_string())?
        .to_rgba8();

    let prepared = if let Some(size) = size {
        let prepared = resize_to_oklch(&image, size);
        match downscale_mode {
            DownscaleMode::OklchAverage => prepared,
            DownscaleMode::Majority => {
                resize_to_palette_vote(&image, size, &palette, &palette_oklab, VoteMode::Majority)
            }
            DownscaleMode::Minority => {
                resize_to_palette_vote(&image, size, &palette, &palette_oklab, VoteMode::Minority)
            }
            DownscaleMode::Solve2x2 => solve_2x2_palette_downscale(prepared, &palette_oklab),
            DownscaleMode::Solve2x2Hue => solve_2x2_hue_downscale(prepared, &palette_oklab),
        }
    } else {
        image_to_oklab(&image)
    };

    let width = prepared.width;
    let height = prepared.height;
    if width == 0 || height == 0 {
        return Err("IPSI images cannot be empty".to_owned());
    }
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err("IPSI images are limited to 65535x65535".to_owned());
    }

    let indexed_pixels = quantize_image(&prepared, &palette, &palette_oklab, dither_strength);
    let mut usage_counts = vec![0usize; palette.len()];
    for index in &indexed_pixels {
        if let Some(count) = usage_counts.get_mut(*index as usize) {
            *count += 1;
        }
    }

    let mut ipsi = Vec::with_capacity(11 + palette.len() * 4 + indexed_pixels.len());
    ipsi.extend_from_slice(b"IPSI");
    ipsi.push(1);
    ipsi.extend_from_slice(&(width as u16).to_le_bytes());
    ipsi.extend_from_slice(&(height as u16).to_le_bytes());
    ipsi.extend_from_slice(&(palette.len() as u16).to_le_bytes());
    for color in &palette {
        ipsi.extend_from_slice(color);
    }
    ipsi.extend_from_slice(&indexed_pixels);

    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::File::create(output)
        .and_then(|mut file| file.write_all(&ipsi))
        .map_err(|err| err.to_string())?;

    Ok(ConversionReport {
        palette: palette_path.to_owned(),
        output: output.to_owned(),
        width,
        height,
        palette_len: palette.len(),
        dither_strength,
        downscale_mode,
        used_palette_entries: usage_counts.iter().filter(|count| **count > 0).count(),
    })
}

#[derive(Clone, Copy)]
enum DownscaleMode {
    OklchAverage,
    Majority,
    Minority,
    Solve2x2,
    Solve2x2Hue,
}

impl DownscaleMode {
    fn name(self) -> &'static str {
        match self {
            Self::OklchAverage => "average",
            Self::Majority => "majority",
            Self::Minority => "minority",
            Self::Solve2x2 => "solve2x2",
            Self::Solve2x2Hue => "solve2x2-hue",
        }
    }
}

struct PreparedImage {
    width: u32,
    height: u32,
    colors: Vec<Oklab>,
    alpha: Vec<u8>,
}

fn image_to_oklab(image: &image::RgbaImage) -> PreparedImage {
    PreparedImage {
        width: image.width(),
        height: image.height(),
        colors: image
            .pixels()
            .map(|pixel| rgb_to_oklab(pixel[0], pixel[1], pixel[2]))
            .collect(),
        alpha: image.pixels().map(|pixel| pixel[3]).collect(),
    }
}

fn resize_to_oklch(image: &image::RgbaImage, size: OutputSize) -> PreparedImage {
    let (crop_x, crop_y, crop_width, crop_height) = crop_to_aspect_bounds(image, size);
    let mut colors = Vec::with_capacity(size.width as usize * size.height as usize);
    let mut alpha = Vec::with_capacity(size.width as usize * size.height as usize);

    for out_y in 0..size.height {
        let source_y_start = crop_y + scale_floor(out_y, crop_height, size.height);
        let source_y_end = crop_y + scale_ceil(out_y + 1, crop_height, size.height);
        for out_x in 0..size.width {
            let source_x_start = crop_x + scale_floor(out_x, crop_width, size.width);
            let source_x_end = crop_x + scale_ceil(out_x + 1, crop_width, size.width);

            colors.push(average_oklch(
                image,
                source_x_start,
                source_y_start,
                source_x_end.max(source_x_start + 1),
                source_y_end.max(source_y_start + 1),
            ));
            alpha.push(average_alpha(
                image,
                source_x_start,
                source_y_start,
                source_x_end.max(source_x_start + 1),
                source_y_end.max(source_y_start + 1),
            ));
        }
    }

    PreparedImage {
        width: size.width,
        height: size.height,
        colors,
        alpha,
    }
}

#[derive(Clone, Copy)]
enum VoteMode {
    Majority,
    Minority,
}

fn resize_to_palette_vote(
    image: &image::RgbaImage,
    size: OutputSize,
    palette: &[[u8; 4]],
    palette_oklab: &[Oklab],
    vote_mode: VoteMode,
) -> PreparedImage {
    let (crop_x, crop_y, crop_width, crop_height) = crop_to_aspect_bounds(image, size);
    let mut colors = Vec::with_capacity(size.width as usize * size.height as usize);
    let mut alpha = Vec::with_capacity(size.width as usize * size.height as usize);

    for out_y in 0..size.height {
        let source_y_start = crop_y + scale_floor(out_y, crop_height, size.height);
        let source_y_end = crop_y + scale_ceil(out_y + 1, crop_height, size.height);
        for out_x in 0..size.width {
            let source_x_start = crop_x + scale_floor(out_x, crop_width, size.width);
            let source_x_end = crop_x + scale_ceil(out_x + 1, crop_width, size.width);

            colors.push(vote_or_palette_average(
                image,
                source_x_start,
                source_y_start,
                source_x_end.max(source_x_start + 1),
                source_y_end.max(source_y_start + 1),
                palette,
                palette_oklab,
                vote_mode,
            ));
            alpha.push(average_alpha(
                image,
                source_x_start,
                source_y_start,
                source_x_end.max(source_x_start + 1),
                source_y_end.max(source_y_start + 1),
            ));
        }
    }

    PreparedImage {
        width: size.width,
        height: size.height,
        colors,
        alpha,
    }
}

fn crop_to_aspect_bounds(image: &image::RgbaImage, size: OutputSize) -> (u32, u32, u32, u32) {
    let source_width = image.width();
    let source_height = image.height();
    let target_aspect = size.width as u64 * source_height as u64;
    let source_aspect = source_width as u64 * size.height as u64;

    if source_aspect == target_aspect {
        return (0, 0, source_width, source_height);
    }

    let (crop_width, crop_height) = if source_aspect > target_aspect {
        let crop_width = (source_height as u64 * size.width as u64 / size.height as u64) as u32;
        (crop_width.max(1).min(source_width), source_height)
    } else {
        let crop_height = (source_width as u64 * size.height as u64 / size.width as u64) as u32;
        (source_width, crop_height.max(1).min(source_height))
    };

    let crop_x = (source_width - crop_width) / 2;
    let crop_y = (source_height - crop_height) / 2;
    (crop_x, crop_y, crop_width, crop_height)
}

fn scale_floor(position: u32, source: u32, target: u32) -> u32 {
    (position as u64 * source as u64 / target as u64) as u32
}

fn scale_ceil(position: u32, source: u32, target: u32) -> u32 {
    ((position as u64 * source as u64).div_ceil(target as u64)) as u32
}

fn average_oklch(
    image: &image::RgbaImage,
    x_start: u32,
    y_start: u32,
    x_end: u32,
    y_end: u32,
) -> Oklab {
    let mut lightness_total = 0.0;
    let mut chroma_total = 0.0;
    let mut hue_x = 0.0;
    let mut hue_y = 0.0;
    let mut weight = 0.0;

    for y in y_start..y_end.min(image.height()) {
        for x in x_start..x_end.min(image.width()) {
            let pixel = image.get_pixel(x, y);
            let alpha = pixel[3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let color = rgb_to_oklab(pixel[0], pixel[1], pixel[2]);
            let chroma = color.chroma();
            let hue = color.hue();

            lightness_total += color.l * alpha;
            chroma_total += chroma * alpha;
            hue_x += hue.cos() * chroma * alpha;
            hue_y += hue.sin() * chroma * alpha;
            weight += alpha;
        }
    }

    if weight <= 0.0 {
        return Oklab::ZERO;
    }

    let lightness = lightness_total / weight;
    let chroma = chroma_total / weight;
    if hue_x.abs() + hue_y.abs() > 0.000001 {
        let hue = hue_y.atan2(hue_x);
        Oklab {
            l: lightness,
            a: hue.cos() * chroma,
            b: hue.sin() * chroma,
        }
    } else {
        Oklab {
            l: lightness,
            a: 0.0,
            b: 0.0,
        }
    }
}

fn average_alpha(
    image: &image::RgbaImage,
    x_start: u32,
    y_start: u32,
    x_end: u32,
    y_end: u32,
) -> u8 {
    let mut total = 0u32;
    let mut count = 0u32;

    for y in y_start..y_end.min(image.height()) {
        for x in x_start..x_end.min(image.width()) {
            total += image.get_pixel(x, y)[3] as u32;
            count += 1;
        }
    }

    total.checked_div(count).unwrap_or(0) as u8
}

#[allow(clippy::too_many_arguments)]
fn vote_or_palette_average(
    image: &image::RgbaImage,
    x_start: u32,
    y_start: u32,
    x_end: u32,
    y_end: u32,
    palette: &[[u8; 4]],
    palette_oklab: &[Oklab],
    vote_mode: VoteMode,
) -> Oklab {
    let mut counts = vec![0.0f32; palette_oklab.len().min(256)];
    let mut samples = Vec::new();
    let mut total_weight = 0.0;

    for y in y_start..y_end.min(image.height()) {
        for x in x_start..x_end.min(image.width()) {
            let pixel = image.get_pixel(x, y);
            let alpha = pixel[3] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let source = rgb_to_oklab(pixel[0], pixel[1], pixel[2]);
            let index = nearest_palette_index(source, pixel[3], palette, palette_oklab) as usize;
            if let Some(count) = counts.get_mut(index) {
                *count += alpha;
                total_weight += alpha;
                if let Some(color) = palette_oklab.get(index).copied() {
                    samples.push((color, alpha));
                }
            }
        }
    }

    if total_weight <= 0.0 {
        return Oklab::ZERO;
    }

    let winner = match vote_mode {
        VoteMode::Majority => counts
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, count)| *count > 0.0)
            .max_by(|(_, left), (_, right)| left.total_cmp(right)),
        VoteMode::Minority => counts
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, count)| *count > 0.0)
            .min_by(|(_, left), (_, right)| left.total_cmp(right)),
    };

    if let Some((index, _)) = winner
        && let Some(color) = palette_oklab.get(index)
    {
        return *color;
    }

    weighted_average_oklch(samples)
}

fn weighted_average_oklch(values: impl IntoIterator<Item = (Oklab, f32)>) -> Oklab {
    let mut lightness = 0.0;
    let mut chroma = 0.0;
    let mut hue_x = 0.0;
    let mut hue_y = 0.0;
    let mut weight = 0.0;

    for (color, sample_weight) in values {
        let color_chroma = color.chroma();
        let hue = color.hue();
        lightness += color.l * sample_weight;
        chroma += color_chroma * sample_weight;
        hue_x += hue.cos() * color_chroma * sample_weight;
        hue_y += hue.sin() * color_chroma * sample_weight;
        weight += sample_weight;
    }

    if weight <= 0.0 {
        return Oklab::ZERO;
    }

    let lightness = lightness / weight;
    let chroma = chroma / weight;
    if hue_x.abs() + hue_y.abs() > 0.000001 {
        let hue = hue_y.atan2(hue_x);
        Oklab {
            l: lightness,
            a: hue.cos() * chroma,
            b: hue.sin() * chroma,
        }
    } else {
        Oklab {
            l: lightness,
            a: 0.0,
            b: 0.0,
        }
    }
}

fn solve_2x2_palette_downscale(image: PreparedImage, palette_oklab: &[Oklab]) -> PreparedImage {
    let width = image.width as usize;
    let height = image.height as usize;
    if width < 2 || height < 2 || palette_oklab.is_empty() {
        return image;
    }

    let mut candidate_sets = vec![Vec::new(); width * height];
    for y in 0..height - 1 {
        for x in 0..width - 1 {
            let block_indices = [
                y * width + x,
                y * width + x + 1,
                (y + 1) * width + x,
                (y + 1) * width + x + 1,
            ];
            let target =
                average_oklch_values(block_indices.iter().map(|index| image.colors[*index]));
            let solved = best_palette_quad(target, palette_oklab);
            for index in block_indices {
                candidate_sets[index].extend_from_slice(&solved);
            }
        }
    }

    let colors = candidate_sets
        .into_iter()
        .enumerate()
        .map(|(index, candidates)| {
            if candidates.is_empty() {
                image.colors[index]
            } else {
                average_oklch_values(candidates)
            }
        })
        .collect();

    PreparedImage { colors, ..image }
}

fn solve_2x2_hue_downscale(image: PreparedImage, palette_oklab: &[Oklab]) -> PreparedImage {
    let width = image.width as usize;
    let height = image.height as usize;
    if width < 2 || height < 2 || palette_oklab.is_empty() {
        return image;
    }

    let mut hue_sets = vec![Vec::new(); width * height];
    for y in 0..height - 1 {
        for x in 0..width - 1 {
            let block_indices = [
                y * width + x,
                y * width + x + 1,
                (y + 1) * width + x,
                (y + 1) * width + x + 1,
            ];
            let target =
                average_oklch_values(block_indices.iter().map(|index| image.colors[*index]));
            let solved = best_palette_quad(target, palette_oklab);
            for index in block_indices {
                hue_sets[index].extend_from_slice(&solved);
            }
        }
    }

    let colors = hue_sets
        .into_iter()
        .enumerate()
        .map(|(index, candidates)| {
            let base = image.colors[index];
            let chroma = base.chroma();
            if chroma <= 0.000001 {
                return base;
            }

            if let Some(hue) = average_hue(candidates) {
                Oklab {
                    l: base.l,
                    a: hue.cos() * chroma,
                    b: hue.sin() * chroma,
                }
            } else {
                base
            }
        })
        .collect();

    PreparedImage { colors, ..image }
}

fn best_palette_quad(target: Oklab, palette_oklab: &[Oklab]) -> [Oklab; 4] {
    let candidates = nearest_palette_candidates(target, palette_oklab);
    let mut best = [candidates[0]; 4];
    let mut best_distance = f32::MAX;

    for a in 0..candidates.len() {
        for b in a..candidates.len() {
            for c in b..candidates.len() {
                for d in c..candidates.len() {
                    let average = average_oklch_values([
                        candidates[a],
                        candidates[b],
                        candidates[c],
                        candidates[d],
                    ]);
                    let distance = average.distance_squared(target);
                    if distance < best_distance {
                        best_distance = distance;
                        best = [candidates[a], candidates[b], candidates[c], candidates[d]];
                    }
                }
            }
        }
    }

    best
}

fn nearest_palette_candidates(target: Oklab, palette_oklab: &[Oklab]) -> Vec<Oklab> {
    let mut candidates = palette_oklab.iter().copied().take(256).collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.distance_squared(target)
            .total_cmp(&right.distance_squared(target))
    });
    candidates.truncate(SOLVE_2X2_CANDIDATES.min(candidates.len()));
    candidates
}

fn average_oklch_values(values: impl IntoIterator<Item = Oklab>) -> Oklab {
    let mut lightness = 0.0;
    let mut chroma = 0.0;
    let mut hue_x = 0.0;
    let mut hue_y = 0.0;
    let mut count = 0.0;

    for color in values {
        let color_chroma = color.chroma();
        let hue = color.hue();
        lightness += color.l;
        chroma += color_chroma;
        hue_x += hue.cos() * color_chroma;
        hue_y += hue.sin() * color_chroma;
        count += 1.0;
    }

    if count <= 0.0 {
        return Oklab::ZERO;
    }

    let lightness = lightness / count;
    let chroma = chroma / count;
    if hue_x.abs() + hue_y.abs() > 0.000001 {
        let hue = hue_y.atan2(hue_x);
        Oklab {
            l: lightness,
            a: hue.cos() * chroma,
            b: hue.sin() * chroma,
        }
    } else {
        Oklab {
            l: lightness,
            a: 0.0,
            b: 0.0,
        }
    }
}

fn average_hue(values: impl IntoIterator<Item = Oklab>) -> Option<f32> {
    let mut hue_x = 0.0;
    let mut hue_y = 0.0;

    for color in values {
        let chroma = color.chroma();
        let hue = color.hue();
        hue_x += hue.cos() * chroma;
        hue_y += hue.sin() * chroma;
    }

    if hue_x.abs() + hue_y.abs() > 0.000001 {
        Some(hue_y.atan2(hue_x))
    } else {
        None
    }
}

fn quantize_image(
    image: &PreparedImage,
    palette: &[[u8; 4]],
    palette_oklab: &[Oklab],
    dither_strength: f32,
) -> Vec<u8> {
    let width = image.width as usize;
    let height = image.height as usize;
    let mut colors = image.colors.clone();
    let alpha = &image.alpha;
    let mut indexed = vec![0; width * height];

    for y in 0..height {
        for x in 0..width {
            let pixel_index = y * width + x;
            let color = colors[pixel_index].clamped();
            let palette_index =
                nearest_palette_index(color, alpha[pixel_index], palette, palette_oklab);
            indexed[pixel_index] = palette_index;

            if dither_strength <= 0.0 || alpha[pixel_index] == 0 {
                continue;
            }

            let quantized = palette_oklab
                .get(palette_index as usize)
                .copied()
                .unwrap_or(color);
            let error = color - quantized;
            diffuse_error(
                &mut colors,
                width,
                height,
                x + 1,
                y,
                error,
                7.0 / 16.0 * dither_strength,
            );
            if x > 0 {
                diffuse_error(
                    &mut colors,
                    width,
                    height,
                    x - 1,
                    y + 1,
                    error,
                    3.0 / 16.0 * dither_strength,
                );
            }
            diffuse_error(
                &mut colors,
                width,
                height,
                x,
                y + 1,
                error,
                5.0 / 16.0 * dither_strength,
            );
            diffuse_error(
                &mut colors,
                width,
                height,
                x + 1,
                y + 1,
                error,
                1.0 / 16.0 * dither_strength,
            );
        }
    }

    indexed
}

fn diffuse_error(
    colors: &mut [Oklab],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    error: Oklab,
    factor: f32,
) {
    if x >= width || y >= height {
        return;
    }

    let index = y * width + x;
    colors[index] = colors[index] + error * factor;
}

fn nearest_palette_index(color: Oklab, a: u8, palette: &[[u8; 4]], palette_oklab: &[Oklab]) -> u8 {
    if a == 0 {
        return palette.iter().position(|color| color[3] == 0).unwrap_or(0) as u8;
    }

    let mut best_index = 0;
    let mut best_distance = f32::MAX;
    for (index, palette_color) in palette_oklab.iter().copied().take(256).enumerate() {
        let alpha_distance = ((a as f32 - palette[index][3] as f32) / 255.0).powi(2);
        let distance = color.distance_squared(palette_color) + alpha_distance;
        if distance < best_distance {
            best_distance = distance;
            best_index = index as u8;
        }
    }
    best_index
}

fn parse_dither_strength(value: &str) -> Result<f32, String> {
    let strength = value
        .parse::<f32>()
        .map_err(|_| "dither strength must be a number".to_owned())?;
    if !strength.is_finite() || !(0.0..=1.0).contains(&strength) {
        return Err("dither strength must be a finite number from 0 to 1".to_owned());
    }
    Ok(strength)
}

fn parse_downscale_mode(value: &str) -> Result<DownscaleMode, String> {
    match value {
        "average" | "oklch" => Ok(DownscaleMode::OklchAverage),
        "majority" | "majority-rules" => Ok(DownscaleMode::Majority),
        "minority" | "smallest-minority" => Ok(DownscaleMode::Minority),
        "solve2x2" | "2x2" => Ok(DownscaleMode::Solve2x2),
        "solve2x2-hue" | "2x2-hue" | "hue2x2" => Ok(DownscaleMode::Solve2x2Hue),
        _ => Err(
            "downscale mode must be average, majority, minority, solve2x2, or solve2x2-hue"
                .to_owned(),
        ),
    }
}

fn parse_size(value: &str) -> Result<OutputSize, String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| "size must look like 128x128".to_owned())?;
    let width = width
        .parse::<u32>()
        .map_err(|_| "size width must be a whole number".to_owned())?;
    let height = height
        .parse::<u32>()
        .map_err(|_| "size height must be a whole number".to_owned())?;
    if width == 0 || height == 0 {
        return Err("size dimensions must be greater than zero".to_owned());
    }
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err("IPSI images are limited to 65535x65535".to_owned());
    }
    Ok(OutputSize { width, height })
}

fn load_palette(path: impl AsRef<Path>) -> Result<Vec<[u8; 4]>, String> {
    let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut colors = Vec::new();

    for quoted in contents.split('"').skip(1).step_by(2) {
        let color = quoted.trim().trim_start_matches('#');
        if color.len() != 6 && color.len() != 8 {
            continue;
        }

        let r = u8::from_str_radix(&color[0..2], 16).map_err(|err| err.to_string())?;
        let g = u8::from_str_radix(&color[2..4], 16).map_err(|err| err.to_string())?;
        let b = u8::from_str_radix(&color[4..6], 16).map_err(|err| err.to_string())?;
        let a = if color.len() == 8 {
            u8::from_str_radix(&color[6..8], 16).map_err(|err| err.to_string())?
        } else {
            0xff
        };
        colors.push([r, g, b, a]);
    }

    if colors.is_empty() {
        Err("palette contains no quoted #RRGGBB colors".to_owned())
    } else if colors.len() > 256 {
        Err("palette contains more than 256 colors".to_owned())
    } else {
        Ok(colors)
    }
}

#[derive(Clone, Copy)]
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

impl Oklab {
    const ZERO: Self = Self {
        l: 0.0,
        a: 0.0,
        b: 0.0,
    };

    fn clamped(self) -> Self {
        Self {
            l: self.l.clamp(0.0, 1.0),
            a: self.a.clamp(-0.5, 0.5),
            b: self.b.clamp(-0.5, 0.5),
        }
    }

    fn distance_squared(self, other: Self) -> f32 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        dl * dl + da * da + db * db
    }

    fn chroma(self) -> f32 {
        self.a.hypot(self.b)
    }

    fn hue(self) -> f32 {
        self.b.atan2(self.a)
    }
}

impl std::ops::Add for Oklab {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            l: self.l + rhs.l,
            a: self.a + rhs.a,
            b: self.b + rhs.b,
        }
    }
}

impl std::ops::Sub for Oklab {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            l: self.l - rhs.l,
            a: self.a - rhs.a,
            b: self.b - rhs.b,
        }
    }
}

impl std::ops::Mul<f32> for Oklab {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            l: self.l * rhs,
            a: self.a * rhs,
            b: self.b * rhs,
        }
    }
}

fn rgb_to_oklab(r: u8, g: u8, b: u8) -> Oklab {
    let r = srgb_to_linear(r as f32 / 255.0);
    let g = srgb_to_linear(g as f32 / 255.0);
    let b = srgb_to_linear(b as f32 / 255.0);

    let l = 0.41222146 * r + 0.53633255 * g + 0.051445995 * b;
    let m = 0.2119035 * r + 0.6806995 * g + 0.10739696 * b;
    let s = 0.08830246 * r + 0.28171884 * g + 0.6299787 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    Oklab {
        l: 0.21045426 * l_ + 0.7936178 * m_ - 0.004072047 * s_,
        a: 1.9779985 * l_ - 2.4285922 * m_ + 0.4505937 * s_,
        b: 0.025904037 * l_ + 0.78277177 * m_ - 0.80867577 * s_,
    }
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
