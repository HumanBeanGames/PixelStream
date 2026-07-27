//! Builds a preview PNG from an IPSMAP palette.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

const HUE_COUNT: usize = 20;
const GRAYSCALE_COUNT: usize = 16;
const CHROMA_LEVELS: usize = 3;
const LIGHTNESS_LEVELS: usize = 16;
const HUE_OFFSET_DEGREES: f32 = 29.233885;
const LIGHTNESS_VALUES: [f32; LIGHTNESS_LEVELS] = [
    0.0, 0.06666667, 0.13333334, 0.2, 0.26666668, 0.33333334, 0.4, 0.46666667, 0.53333336, 0.6,
    0.6666667, 0.73333335, 0.8, 0.8666667, 0.93333334, 1.0,
];
const CHROMA_VALUES: [f32; CHROMA_LEVELS] = [0.08589443, 0.17178887, 0.2576833];
const WIDTH: u16 = (HUE_COUNT * CHROMA_LEVELS + 1) as u16;
const HEIGHT: u16 = GRAYSCALE_COUNT as u16;

fn main() {
    let mut args = env::args().skip(1);
    let Some(palette_path) = args.next().map(PathBuf::from) else {
        eprintln!("Usage: cargo run --bin ipsc_make_palette_image -- <palette.toml> <output.ipsi>");
        return;
    };
    let Some(output_path) = args.next().map(PathBuf::from) else {
        eprintln!("Usage: cargo run --bin ipsc_make_palette_image -- <palette.toml> <output.ipsi>");
        return;
    };

    let palette = match load_palette(&palette_path) {
        Ok(palette) => palette,
        Err(err) => {
            eprintln!("Could not load {}: {err}", palette_path.display());
            return;
        }
    };

    if palette.len() != 256 {
        eprintln!("palette.toml must contain exactly 256 colors for the palette image");
        return;
    }

    let mut image = Vec::with_capacity(4 + 1 + 2 + 2 + 2 + palette.len() * 4 + 256);
    image.extend_from_slice(b"IPSI");
    image.push(1);
    image.extend_from_slice(&WIDTH.to_le_bytes());
    image.extend_from_slice(&HEIGHT.to_le_bytes());
    image.extend_from_slice(&(palette.len() as u16).to_le_bytes());
    for color in &palette {
        image.extend_from_slice(color);
    }
    let pixels = palette_swatch_pixels();
    image.extend_from_slice(&pixels);

    match fs::File::create(&output_path).and_then(|mut file| file.write_all(&image)) {
        Ok(()) => eprintln!("Wrote {}", output_path.display()),
        Err(err) => eprintln!("Could not write {}: {err}", output_path.display()),
    }
}

fn palette_swatch_pixels() -> Vec<u8> {
    let mut pixels = vec![0; WIDTH as usize * HEIGHT as usize];

    for hue in 0..HUE_COUNT {
        let flip_chroma = hue % 2 == 1;
        let hue_base = GRAYSCALE_COUNT + (0..hue).map(colors_in_hue).sum::<usize>();

        for visual_chroma_column in 0..CHROMA_LEVELS {
            let chroma = if flip_chroma {
                visual_chroma_column
            } else {
                CHROMA_LEVELS - 1 - visual_chroma_column
            };
            for lightness in 0..LIGHTNESS_LEVELS {
                let x = hue * CHROMA_LEVELS + visual_chroma_column;
                let y = GRAYSCALE_COUNT - 1 - lightness;
                let pixel = &mut pixels[y * WIDTH as usize + x];
                if let Some(index) = compact_palette_index(hue, hue_base, chroma, lightness)
                    && *pixel == 0
                {
                    *pixel = index as u8;
                }
            }
        }
    }

    let final_column = WIDTH as usize - 1;
    for y in 0..HEIGHT as usize {
        pixels[y * WIDTH as usize + final_column] = greyscale_index_for_row(y);
    }

    pixels
}

fn greyscale_index_for_row(y: usize) -> u8 {
    match y {
        0 => (GRAYSCALE_COUNT - 1) as u8,
        y if y == GRAYSCALE_COUNT - 1 => 0,
        _ => (GRAYSCALE_COUNT - 1 - y) as u8,
    }
}

fn compact_palette_index(
    hue: usize,
    hue_base: usize,
    chroma: usize,
    lightness: usize,
) -> Option<usize> {
    if !is_valid_palette_slot(hue, chroma, lightness) {
        return None;
    }

    let offset = (0..CHROMA_LEVELS)
        .flat_map(|cell_chroma| {
            (0..LIGHTNESS_LEVELS).map(move |cell_lightness| (cell_chroma, cell_lightness))
        })
        .take_while(|&(cell_chroma, cell_lightness)| {
            cell_chroma < chroma || (cell_chroma == chroma && cell_lightness < lightness)
        })
        .filter(|&(cell_chroma, cell_lightness)| {
            is_valid_palette_slot(hue, cell_chroma, cell_lightness)
        })
        .count();

    Some(hue_base + offset)
}

fn colors_in_hue(hue: usize) -> usize {
    (0..CHROMA_LEVELS)
        .flat_map(|chroma| (0..LIGHTNESS_LEVELS).map(move |lightness| (chroma, lightness)))
        .filter(|&(chroma, lightness)| is_valid_palette_slot(hue, chroma, lightness))
        .count()
}

fn is_valid_palette_slot(hue: usize, chroma: usize, lightness: usize) -> bool {
    if LIGHTNESS_VALUES[lightness] <= 0.0 || LIGHTNESS_VALUES[lightness] >= 1.0 {
        return false;
    }

    let hue_degrees = HUE_OFFSET_DEGREES + hue as f32 * 360.0 / HUE_COUNT as f32;
    let (r, g, b) = oklch_to_linear_srgb(
        LIGHTNESS_VALUES[lightness],
        CHROMA_VALUES[chroma],
        hue_degrees,
    );
    in_srgb_gamut(r, g, b)
}

fn oklch_to_linear_srgb(lightness: f32, chroma: f32, hue_degrees: f32) -> (f32, f32, f32) {
    let hue = hue_degrees.to_radians();
    let a = hue.cos() * chroma;
    let b = hue.sin() * chroma;

    let l_ = lightness + 0.39633778 * a + 0.21580376 * b;
    let m_ = lightness - 0.105561346 * a - 0.06385417 * b;
    let s_ = lightness - 0.08948418 * a - 1.2914855 * b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    (
        4.0767417 * l - 3.3077116 * m + 0.23096994 * s,
        -1.268438 * l + 2.6097574 * m - 0.34131938 * s,
        -0.0041960863 * l - 0.7034186 * m + 1.7076147 * s,
    )
}

fn in_srgb_gamut(r: f32, g: f32, b: f32) -> bool {
    r.is_finite()
        && g.is_finite()
        && b.is_finite()
        && (0.0..=1.0).contains(&r)
        && (0.0..=1.0).contains(&g)
        && (0.0..=1.0).contains(&b)
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
