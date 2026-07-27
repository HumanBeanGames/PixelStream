//! IPSMAP palette lookup format and baking math.
//!
//! IPSMAP stores self-contained palette metadata plus precomputed altered and
//! direct RGB-to-palette-index tables, so runtime quantization is a single
//! lookup instead of a nearest-color search.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub const LUT_ENTRY_COUNT: usize = 256 * 256 * 256;
const LUT_MAGIC_V1: &[u8; 8] = b"IPSMAP1\0";
const LUT_MAGIC_V2: &[u8; 8] = b"IPSMAP2\0";
const LUT_MAGIC_V3: &[u8; 8] = b"IPSMAP3\0";
const LUT_MAGIC_V4: &[u8; 8] = b"IPSMAP4\0";
const LUT_MAGIC_V5: &[u8; 8] = b"IPSMAP5\0";
const LUT_MAGIC_V6: &[u8; 8] = b"IPSMAP6\0";
const LUT_V1_HEADER_LEN: usize = 30;
const LUT_V4_HEADER_LEN: usize = 24;
const LUT_V6_MATCHING_LEN: usize = 9 * std::mem::size_of::<f32>();
const PALETTE_MATCHING_ALGORITHM_VERSION: &[u8] = b"delta-e-ok-v8";
const CHROMA_OFFSET_TRANSITION: f32 = 0.04;

#[derive(Clone, Copy, Debug)]
pub struct PaletteMatching {
    pub lightness: f32,
    pub chroma: f32,
    pub hue: f32,
    pub lightness_multiply: f32,
    pub lightness_add: f32,
    pub chroma_multiply: f32,
    pub chroma_add: f32,
    pub grey_chroma_threshold: f32,
    pub hue_add: f32,
}

impl Default for PaletteMatching {
    fn default() -> Self {
        Self {
            lightness: 0.333,
            chroma: 0.333,
            hue: 0.334,
            lightness_multiply: 0.0,
            lightness_add: 0.0,
            chroma_multiply: 0.0,
            chroma_add: 0.0,
            grey_chroma_threshold: 0.001,
            hue_add: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PaletteConfig {
    pub colors: Vec<[u8; 4]>,
    pub matching: PaletteMatching,
}

pub struct PaletteLookup {
    hash: u64,
    entries: Vec<u8>,
    config: PaletteConfig,
}

impl PaletteLookup {
    pub fn entries(&self) -> &[u8] {
        &self.entries
    }

    pub fn altered_entries(&self) -> &[u8] {
        &self.entries[..LUT_ENTRY_COUNT.min(self.entries.len())]
    }

    pub fn direct_entries(&self) -> Option<&[u8]> {
        self.entries
            .get(LUT_ENTRY_COUNT..LUT_ENTRY_COUNT.saturating_mul(2))
    }

    pub fn hash(&self) -> u64 {
        self.hash
    }

    pub fn config(&self) -> &PaletteConfig {
        &self.config
    }
}

pub fn load_palette_config(path: impl AsRef<Path>) -> Result<PaletteConfig, String> {
    let contents = fs::read_to_string(path).map_err(|err| err.to_string())?;
    parse_palette_config(&contents)
}

pub fn parse_palette_config(contents: &str) -> Result<PaletteConfig, String> {
    let mut colors = Vec::new();
    let mut matching = PaletteMatching::default();
    let mut section = "";

    for raw_line in contents.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        let raw_trimmed = raw_line.trim();
        if raw_trimmed.is_empty() {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(['[', ']']).trim();
            continue;
        }

        if section == "matching"
            && let Some((key, value)) = line.split_once('=')
        {
            match key.trim() {
                "oklab_l" | "oklab_l_weight" | "l" | "l_weight" | "lightness"
                | "lightness_weight" | "value" | "value_weight" => {
                    let value = parse_f32_value(value)?;
                    matching.lightness = value
                }
                "oklab_a" | "oklab_a_weight" | "a" | "a_weight" | "chroma" | "chroma_weight" => {
                    matching.chroma = parse_f32_value(value)?
                }
                "oklab_b" | "oklab_b_weight" | "b" | "b_weight" | "hue" | "hue_weight" => {
                    matching.hue = parse_f32_value(value)?
                }
                "lightness_multiply" | "value_multiply" => {
                    matching.lightness_multiply = parse_f32_value(value)?
                }
                "lightness_add" | "value_add" => matching.lightness_add = parse_f32_value(value)?,
                "chroma_multiply" => matching.chroma_multiply = parse_f32_value(value)?,
                "chroma_add" => matching.chroma_add = parse_f32_value(value)?,
                "grey_chroma_threshold"
                | "gray_chroma_threshold"
                | "grey_threshold"
                | "gray_threshold" => matching.grey_chroma_threshold = parse_f32_value(value)?,
                "hue_add" => matching.hue_add = parse_f32_value(value)?,
                _ => {}
            }
        }

        for quoted in raw_line.split('"').skip(1).step_by(2) {
            if let Some(color) = parse_hex_color(quoted) {
                colors.push(color?);
            }
        }
    }

    if colors.is_empty() {
        Err("palette contains no quoted #RRGGBB colors".to_owned())
    } else if colors.len() > 256 {
        Err("palette contains more than 256 colors".to_owned())
    } else {
        Ok(PaletteConfig { colors, matching })
    }
}

pub fn serialize_palette_config(config: &PaletteConfig) -> String {
    let mut output = String::new();
    output.push_str("colors = [\n");
    for [r, g, b, a] in &config.colors {
        if *a == 0xff {
            output.push_str(&format!("    \"#{r:02x}{g:02x}{b:02x}\",\n"));
        } else {
            output.push_str(&format!("    \"#{r:02x}{g:02x}{b:02x}{a:02x}\",\n"));
        }
    }
    output.push_str("]\n\n[matching]\n");
    output.push_str(&format!("oklab_l = {}\n", config.matching.lightness));
    output.push_str(&format!("oklab_a = {}\n", config.matching.chroma));
    output.push_str(&format!("oklab_b = {}\n", config.matching.hue));
    output.push_str(&format!(
        "lightness_multiply = {}\n",
        config.matching.lightness_multiply
    ));
    output.push_str(&format!(
        "lightness_add = {}\n",
        config.matching.lightness_add
    ));
    output.push_str(&format!(
        "chroma_multiply = {}\n",
        config.matching.chroma_multiply
    ));
    output.push_str(&format!("chroma_add = {}\n", config.matching.chroma_add));
    output.push_str(&format!(
        "grey_chroma_threshold = {}\n",
        config.matching.grey_chroma_threshold
    ));
    output.push_str(&format!("hue_add = {}\n", config.matching.hue_add));
    output
}

fn parse_f32_value(value: &str) -> Result<f32, String> {
    value
        .trim()
        .trim_matches(',')
        .parse::<f32>()
        .map_err(|err| err.to_string())
}

pub fn sibling_lut_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref().with_extension("ipsmap")
}

pub fn build_lookup(config: &PaletteConfig) -> Vec<u8> {
    build_lookup_with_progress(config, |_| {})
}

pub fn build_lookup_with_progress(
    config: &PaletteConfig,
    mut progress: impl FnMut(usize) + Send,
) -> Vec<u8> {
    let palette_oklch = config
        .colors
        .iter()
        .map(|[r, g, b, _]| Oklch::from(rgb_to_oklab(*r, *g, *b)))
        .collect::<Vec<_>>();
    let mut altered_entries = Vec::with_capacity(LUT_ENTRY_COUNT);
    let mut direct_entries = Vec::with_capacity(LUT_ENTRY_COUNT);

    for r in 0..=255u8 {
        for g in 0..=255u8 {
            for b in 0..=255u8 {
                altered_entries.push(nearest_palette_index(
                    Oklch::from(rgb_to_oklab(r, g, b)),
                    &palette_oklch,
                    config.matching,
                ));
            }
        }
        progress((r as usize + 1) * 50 / 256);
    }

    for r in 0..=255u8 {
        for g in 0..=255u8 {
            for b in 0..=255u8 {
                direct_entries.push(nearest_palette_index_unaltered(
                    Oklch::from(rgb_to_oklab(r, g, b)),
                    &palette_oklch,
                ));
            }
        }
        progress(50 + (r as usize + 1) * 50 / 256);
    }

    let mut entries = altered_entries;
    entries.extend_from_slice(&direct_entries);
    entries
}

pub fn write_lookup(
    path: impl AsRef<Path>,
    config: &PaletteConfig,
    entries: &[u8],
) -> Result<(), String> {
    let bytes = encode_lookup(config, entries)?;
    fs::File::create(path)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|err| err.to_string())
}

pub fn encode_lookup(config: &PaletteConfig, entries: &[u8]) -> Result<Vec<u8>, String> {
    if entries.len() != LUT_ENTRY_COUNT && entries.len() != LUT_ENTRY_COUNT * 2 {
        return Err(format!(
            "LUT must contain {LUT_ENTRY_COUNT} altered entries or {LUT_ENTRY_COUNT} altered plus {LUT_ENTRY_COUNT} direct entries, got {}",
            entries.len()
        ));
    }
    if config.colors.is_empty() || config.colors.len() > 256 {
        return Err("palette must contain 1-256 colors".to_owned());
    }

    let colors_len = u16::try_from(config.colors.len())
        .map_err(|_| "palette contains more than 65535 colors".to_owned())?;
    let mut bytes = Vec::with_capacity(
        LUT_V4_HEADER_LEN
            + config.colors.len() * std::mem::size_of::<[u8; 4]>()
            + LUT_V6_MATCHING_LEN
            + entries.len(),
    );
    bytes.extend_from_slice(LUT_MAGIC_V6);
    bytes.extend_from_slice(&lookup_hash(config, entries).to_le_bytes());
    bytes.extend_from_slice(&colors_len.to_le_bytes());
    bytes.extend_from_slice(&(LUT_V6_MATCHING_LEN as u16).to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for color in &config.colors {
        bytes.extend_from_slice(color);
    }
    bytes.extend_from_slice(&palette_matching_bytes(config.matching));
    bytes.extend_from_slice(entries);
    Ok(bytes)
}

pub fn load_lookup(
    path: impl AsRef<Path>,
    config: &PaletteConfig,
) -> Result<PaletteLookup, String> {
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    decode_lookup(&bytes, config)
}

pub fn load_lookup_bundle(path: impl AsRef<Path>) -> Result<PaletteLookup, String> {
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    decode_lookup_bundle(&bytes)
}

pub fn recover_lookup_config(path: impl AsRef<Path>) -> Result<PaletteConfig, String> {
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    decode_lookup_config_unchecked(&bytes)
}

pub fn decode_lookup_config_unchecked(bytes: &[u8]) -> Result<PaletteConfig, String> {
    if bytes.len() >= LUT_MAGIC_V6.len() && &bytes[0..8] == LUT_MAGIC_V6 {
        return decode_lookup_config_v6(bytes);
    }
    if bytes.len() >= LUT_MAGIC_V5.len() && &bytes[0..8] == LUT_MAGIC_V5 {
        return decode_lookup_config_v4_or_v5(bytes, LUT_ENTRY_COUNT * 2);
    }
    if bytes.len() >= LUT_MAGIC_V4.len() && &bytes[0..8] == LUT_MAGIC_V4 {
        return decode_lookup_config_v4_or_v5(bytes, LUT_ENTRY_COUNT);
    }
    Err("LUT is not a recoverable self-contained IPSMAP4/IPSMAP5/IPSMAP6 lookup".to_owned())
}

pub fn decode_lookup(bytes: &[u8], config: &PaletteConfig) -> Result<PaletteLookup, String> {
    if bytes.len() >= LUT_MAGIC_V6.len() && &bytes[0..8] == LUT_MAGIC_V6 {
        let lookup = decode_lookup_bundle(bytes)?;
        if lookup.config.colors != config.colors {
            return Err("LUT palette colors do not match palette config".to_owned());
        }
        if !palette_matching_eq(lookup.config.matching, config.matching) {
            return Err("LUT matching metadata does not match palette config".to_owned());
        }
        return Ok(lookup);
    }

    if bytes.len() >= LUT_MAGIC_V5.len() && &bytes[0..8] == LUT_MAGIC_V5 {
        let lookup = decode_lookup_bundle(bytes)?;
        if lookup.config.colors != config.colors {
            return Err("LUT palette colors do not match palette config".to_owned());
        }
        return Ok(lookup);
    }

    if bytes.len() >= LUT_MAGIC_V4.len() && &bytes[0..8] == LUT_MAGIC_V4 {
        let lookup = decode_lookup_bundle(bytes)?;
        if lookup.config.colors != config.colors {
            return Err("LUT palette colors do not match palette config".to_owned());
        }
        return Ok(lookup);
    }

    if bytes.len() >= LUT_MAGIC_V3.len() && &bytes[0..8] == LUT_MAGIC_V3 {
        return Err(
            "IPSMAP3 matching-metadata lookups are no longer supported; regenerate an IPSMAP6 lookup"
                .to_owned(),
        );
    }

    if bytes.len() >= LUT_MAGIC_V2.len() && &bytes[0..8] == LUT_MAGIC_V2 {
        return Err(
            "IPSMAP2 embedded-TOML lookups are no longer supported; regenerate an IPSMAP6 lookup"
                .to_owned(),
        );
    }

    if bytes.len() < LUT_V1_HEADER_LEN {
        return Err(format!(
            "LUT has {} bytes, expected at least {LUT_V1_HEADER_LEN}",
            bytes.len()
        ));
    }

    let header = &bytes[0..LUT_V1_HEADER_LEN];

    if &header[0..8] != LUT_MAGIC_V1 {
        return Err("LUT magic/version mismatch".to_owned());
    }

    let hash = u64::from_le_bytes(header[8..16].try_into().expect("header slice length"));
    let expected_hash = palette_hash(config);
    if hash != expected_hash {
        return Err("LUT does not match palette colors and matching settings".to_owned());
    }

    let color_count = u16::from_le_bytes(header[16..18].try_into().expect("header slice length"));
    if color_count as usize != config.colors.len() {
        return Err("LUT color count does not match palette".to_owned());
    }

    let entries = bytes[LUT_V1_HEADER_LEN..].to_vec();
    if entries.len() != LUT_ENTRY_COUNT {
        return Err(format!(
            "LUT has {} entries, expected {LUT_ENTRY_COUNT}",
            entries.len()
        ));
    }

    Ok(PaletteLookup {
        hash,
        entries,
        config: config.clone(),
    })
}

pub fn decode_lookup_bundle(bytes: &[u8]) -> Result<PaletteLookup, String> {
    if bytes.len() >= LUT_MAGIC_V6.len() && &bytes[0..8] == LUT_MAGIC_V6 {
        return decode_lookup_bundle_v6(bytes);
    }
    if bytes.len() >= LUT_MAGIC_V5.len() && &bytes[0..8] == LUT_MAGIC_V5 {
        return decode_lookup_bundle_v4_or_v5(bytes, LUT_ENTRY_COUNT * 2);
    }
    if bytes.len() >= LUT_MAGIC_V4.len() && &bytes[0..8] == LUT_MAGIC_V4 {
        return decode_lookup_bundle_v4_or_v5(bytes, LUT_ENTRY_COUNT);
    }
    if bytes.len() >= LUT_MAGIC_V3.len() && &bytes[0..8] == LUT_MAGIC_V3 {
        return Err(
            "IPSMAP3 matching-metadata lookups are no longer supported; regenerate an IPSMAP6 lookup"
                .to_owned(),
        );
    }
    if bytes.len() >= LUT_MAGIC_V2.len() && &bytes[0..8] == LUT_MAGIC_V2 {
        return Err(
            "IPSMAP2 embedded-TOML lookups are no longer supported; regenerate an IPSMAP6 lookup"
                .to_owned(),
        );
    }
    Err("LUT is not a self-contained IPSMAP4/IPSMAP5/IPSMAP6 lookup".to_owned())
}

fn decode_lookup_bundle_v6(bytes: &[u8]) -> Result<PaletteLookup, String> {
    let (hash, config, entries) = decode_lookup_v6_parts(bytes)?;
    if lookup_hash(&config, &entries) != hash {
        return Err(
            "binary palette, matching metadata, and entries do not match LUT hash".to_owned(),
        );
    }
    Ok(PaletteLookup {
        hash,
        entries,
        config,
    })
}

fn decode_lookup_bundle_v4_or_v5(
    bytes: &[u8],
    expected_entry_count: usize,
) -> Result<PaletteLookup, String> {
    if bytes.len() < LUT_V4_HEADER_LEN {
        return Err(format!(
            "LUT has {} bytes, expected at least {LUT_V4_HEADER_LEN}",
            bytes.len()
        ));
    }
    let hash = u64::from_le_bytes(bytes[8..16].try_into().expect("header slice length"));
    let color_count =
        u16::from_le_bytes(bytes[16..18].try_into().expect("header slice length")) as usize;
    if color_count == 0 || color_count > 256 {
        return Err(format!(
            "LUT header declares {color_count} colors, expected 1-256"
        ));
    }
    let entry_count =
        u32::from_le_bytes(bytes[20..24].try_into().expect("header slice length")) as usize;
    if entry_count != expected_entry_count {
        return Err(format!(
            "LUT header declares {entry_count} entries, expected {expected_entry_count}"
        ));
    }

    let entries_start = LUT_V4_HEADER_LEN
        .checked_add(color_count * std::mem::size_of::<[u8; 4]>())
        .ok_or_else(|| "LUT palette length overflowed".to_owned())?;
    let expected_len = entries_start
        .checked_add(expected_entry_count)
        .ok_or_else(|| "LUT entry length overflowed".to_owned())?;
    if bytes.len() != expected_len {
        return Err(format!(
            "LUT has {} bytes, expected {expected_len} for binary palette and entries",
            bytes.len()
        ));
    }

    let colors = bytes[LUT_V4_HEADER_LEN..entries_start]
        .chunks_exact(4)
        .map(|color| [color[0], color[1], color[2], color[3]])
        .collect::<Vec<_>>();
    let entries = bytes[entries_start..].to_vec();
    if legacy_lookup_hash(&colors, &entries) != hash {
        return Err("binary palette and entries do not match LUT hash".to_owned());
    }
    let config = PaletteConfig {
        colors,
        matching: PaletteMatching::default(),
    };
    Ok(PaletteLookup {
        hash,
        entries,
        config,
    })
}

fn decode_lookup_config_v4_or_v5(
    bytes: &[u8],
    expected_entry_count: usize,
) -> Result<PaletteConfig, String> {
    if bytes.len() < LUT_V4_HEADER_LEN {
        return Err(format!(
            "LUT has {} bytes, expected at least {LUT_V4_HEADER_LEN}",
            bytes.len()
        ));
    }
    let color_count =
        u16::from_le_bytes(bytes[16..18].try_into().expect("header slice length")) as usize;
    if color_count == 0 || color_count > 256 {
        return Err(format!(
            "LUT header declares {color_count} colors, expected 1-256"
        ));
    }
    let entry_count =
        u32::from_le_bytes(bytes[20..24].try_into().expect("header slice length")) as usize;
    if entry_count != expected_entry_count {
        return Err(format!(
            "LUT header declares {entry_count} entries, expected {expected_entry_count}"
        ));
    }

    let entries_start = LUT_V4_HEADER_LEN
        .checked_add(color_count * std::mem::size_of::<[u8; 4]>())
        .ok_or_else(|| "LUT palette length overflowed".to_owned())?;
    let expected_len = entries_start
        .checked_add(expected_entry_count)
        .ok_or_else(|| "LUT entry length overflowed".to_owned())?;
    if bytes.len() != expected_len {
        return Err(format!(
            "LUT has {} bytes, expected {expected_len} for binary palette and entries",
            bytes.len()
        ));
    }

    let colors = bytes[LUT_V4_HEADER_LEN..entries_start]
        .chunks_exact(4)
        .map(|color| [color[0], color[1], color[2], color[3]])
        .collect::<Vec<_>>();
    Ok(PaletteConfig {
        colors,
        matching: PaletteMatching::default(),
    })
}

fn decode_lookup_config_v6(bytes: &[u8]) -> Result<PaletteConfig, String> {
    let (_, config, _) = decode_lookup_v6_parts(bytes)?;
    Ok(config)
}

fn decode_lookup_v6_parts(bytes: &[u8]) -> Result<(u64, PaletteConfig, Vec<u8>), String> {
    if bytes.len() < LUT_V4_HEADER_LEN {
        return Err(format!(
            "LUT has {} bytes, expected at least {LUT_V4_HEADER_LEN}",
            bytes.len()
        ));
    }
    let hash = u64::from_le_bytes(bytes[8..16].try_into().expect("header slice length"));
    let color_count =
        u16::from_le_bytes(bytes[16..18].try_into().expect("header slice length")) as usize;
    if color_count == 0 || color_count > 256 {
        return Err(format!(
            "LUT header declares {color_count} colors, expected 1-256"
        ));
    }
    let matching_len =
        u16::from_le_bytes(bytes[18..20].try_into().expect("header slice length")) as usize;
    if matching_len != LUT_V6_MATCHING_LEN {
        return Err(format!(
            "IPSMAP6 header declares {matching_len} matching bytes, expected {LUT_V6_MATCHING_LEN}"
        ));
    }
    let entry_count =
        u32::from_le_bytes(bytes[20..24].try_into().expect("header slice length")) as usize;
    if entry_count != LUT_ENTRY_COUNT && entry_count != LUT_ENTRY_COUNT * 2 {
        return Err(format!(
            "LUT header declares {entry_count} entries, expected {LUT_ENTRY_COUNT} or {}",
            LUT_ENTRY_COUNT * 2
        ));
    }

    let matching_start = LUT_V4_HEADER_LEN
        .checked_add(color_count * std::mem::size_of::<[u8; 4]>())
        .ok_or_else(|| "LUT palette length overflowed".to_owned())?;
    let entries_start = matching_start
        .checked_add(matching_len)
        .ok_or_else(|| "LUT matching metadata length overflowed".to_owned())?;
    let expected_len = entries_start
        .checked_add(entry_count)
        .ok_or_else(|| "LUT entry length overflowed".to_owned())?;
    if bytes.len() != expected_len {
        return Err(format!(
            "LUT has {} bytes, expected {expected_len} for binary palette, matching metadata, and entries",
            bytes.len()
        ));
    }

    let colors = bytes[LUT_V4_HEADER_LEN..matching_start]
        .chunks_exact(4)
        .map(|color| [color[0], color[1], color[2], color[3]])
        .collect::<Vec<_>>();
    let matching = palette_matching_from_bytes(&bytes[matching_start..entries_start])?;
    let entries = bytes[entries_start..].to_vec();
    Ok((hash, PaletteConfig { colors, matching }, entries))
}

pub fn palette_hash(config: &PaletteConfig) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;

    for byte in PALETTE_MATCHING_ALGORITHM_VERSION {
        feed_hash(&mut hash, *byte);
    }
    for color in &config.colors {
        for byte in color {
            feed_hash(&mut hash, *byte);
        }
    }
    for value in [
        config.matching.lightness,
        config.matching.chroma,
        config.matching.hue,
    ] {
        for byte in value.to_le_bytes() {
            feed_hash(&mut hash, byte);
        }
    }
    if config.matching.has_input_offset() {
        for value in [
            config.matching.lightness_multiply,
            config.matching.lightness_add,
            config.matching.chroma_multiply,
            config.matching.chroma_add,
            config.matching.grey_chroma_threshold,
            config.matching.hue_add,
        ] {
            for byte in value.to_le_bytes() {
                feed_hash(&mut hash, byte);
            }
        }
    }
    hash
}

fn lookup_hash(config: &PaletteConfig, entries: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in PALETTE_MATCHING_ALGORITHM_VERSION {
        feed_hash(&mut hash, *byte);
    }
    for color in &config.colors {
        for byte in color {
            feed_hash(&mut hash, *byte);
        }
    }
    for byte in palette_matching_bytes(config.matching) {
        feed_hash(&mut hash, byte);
    }
    for byte in entries {
        feed_hash(&mut hash, *byte);
    }
    hash
}

fn legacy_lookup_hash(colors: &[[u8; 4]], entries: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in PALETTE_MATCHING_ALGORITHM_VERSION {
        feed_hash(&mut hash, *byte);
    }
    for color in colors {
        for byte in color {
            feed_hash(&mut hash, *byte);
        }
    }
    for byte in entries {
        feed_hash(&mut hash, *byte);
    }
    hash
}

fn feed_hash(hash: &mut u64, byte: u8) {
    *hash ^= byte as u64;
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}

fn palette_matching_bytes(matching: PaletteMatching) -> [u8; LUT_V6_MATCHING_LEN] {
    let values = [
        matching.lightness,
        matching.chroma,
        matching.hue,
        matching.lightness_multiply,
        matching.lightness_add,
        matching.chroma_multiply,
        matching.chroma_add,
        matching.grey_chroma_threshold,
        matching.hue_add,
    ];
    let mut bytes = [0u8; LUT_V6_MATCHING_LEN];
    for (index, value) in values.iter().copied().enumerate() {
        let start = index * std::mem::size_of::<f32>();
        bytes[start..start + std::mem::size_of::<f32>()].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn palette_matching_from_bytes(bytes: &[u8]) -> Result<PaletteMatching, String> {
    if bytes.len() != LUT_V6_MATCHING_LEN {
        return Err(format!(
            "matching metadata has {} bytes, expected {LUT_V6_MATCHING_LEN}",
            bytes.len()
        ));
    }
    let mut values = [0.0f32; 9];
    for (index, chunk) in bytes.chunks_exact(std::mem::size_of::<f32>()).enumerate() {
        values[index] = f32::from_le_bytes(chunk.try_into().expect("f32 chunk length"));
    }
    Ok(PaletteMatching {
        lightness: values[0],
        chroma: values[1],
        hue: values[2],
        lightness_multiply: values[3],
        lightness_add: values[4],
        chroma_multiply: values[5],
        chroma_add: values[6],
        grey_chroma_threshold: values[7],
        hue_add: values[8],
    })
}

fn palette_matching_eq(a: PaletteMatching, b: PaletteMatching) -> bool {
    let a = palette_matching_bytes(a);
    let b = palette_matching_bytes(b);
    a == b
}

impl PaletteMatching {
    pub fn has_input_offset(&self) -> bool {
        [
            self.lightness_multiply,
            self.lightness_add,
            self.chroma_multiply,
            self.chroma_add,
            self.hue_add,
        ]
        .iter()
        .any(|value| value.abs() > 0.000_001)
            || (self.grey_chroma_threshold - Self::default().grey_chroma_threshold).abs()
                > 0.000_001
    }
}

fn parse_hex_color(value: &str) -> Option<Result<[u8; 4], String>> {
    let color = value.trim().trim_start_matches('#');
    if color.len() != 6 && color.len() != 8 {
        return None;
    }

    Some((|| {
        let r = u8::from_str_radix(&color[0..2], 16).map_err(|err| err.to_string())?;
        let g = u8::from_str_radix(&color[2..4], 16).map_err(|err| err.to_string())?;
        let b = u8::from_str_radix(&color[4..6], 16).map_err(|err| err.to_string())?;
        let a = if color.len() == 8 {
            u8::from_str_radix(&color[6..8], 16).map_err(|err| err.to_string())?
        } else {
            0xff
        };
        Ok([r, g, b, a])
    })())
}

#[derive(Clone, Copy)]
struct Oklab {
    l: f32,
    a: f32,
    b: f32,
}

#[derive(Clone, Copy)]
struct Oklch {
    l: f32,
    c: f32,
    h: f32,
}

impl From<Oklab> for Oklch {
    fn from(color: Oklab) -> Self {
        let c = color.a.hypot(color.b);
        let h = if c <= 0.000_001 {
            0.0
        } else {
            color.b.atan2(color.a)
        };
        Self { l: color.l, c, h }
    }
}

fn nearest_palette_index(color: Oklch, palette: &[Oklch], matching: PaletteMatching) -> u8 {
    let color = apply_input_offset(color, matching);
    nearest_palette_index_unaltered(color, palette)
}

fn nearest_palette_index_unaltered(color: Oklch, palette: &[Oklch]) -> u8 {
    let mut best_index = 0;
    let mut best_distance = f32::MAX;

    for (index, palette_color) in palette.iter().copied().take(256).enumerate() {
        let distance = delta_e_ok_distance_squared(color, palette_color);
        if distance < best_distance {
            best_distance = distance;
            best_index = index as u8;
        }
    }

    best_index
}

fn apply_input_offset(color: Oklch, matching: PaletteMatching) -> Oklch {
    let grey_chroma_threshold = matching.grey_chroma_threshold.clamp(0.0, 1.0);
    let chroma_offset_weight = smoothstep(
        grey_chroma_threshold,
        grey_chroma_threshold + CHROMA_OFFSET_TRANSITION,
        color.c,
    );
    let adjusted_chroma = (color.c
        + (color.c * matching.chroma_multiply + matching.chroma_add) * chroma_offset_weight)
        .max(0.0);
    let mut adjusted = Oklch {
        l: (color.l * (1.0 + matching.lightness_multiply) + matching.lightness_add).clamp(0.0, 1.0),
        c: adjusted_chroma,
        h: color.h + matching.hue_add * std::f32::consts::TAU,
    };
    if matching.has_input_offset() {
        adjusted.c = clamp_chroma_to_srgb_gamut(adjusted);
    }
    adjusted
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn clamp_chroma_to_srgb_gamut(color: Oklch) -> f32 {
    if color.c <= 0.0 || !color.l.is_finite() || !color.c.is_finite() || !color.h.is_finite() {
        return 0.0;
    }

    if oklch_in_srgb_gamut(color) {
        return color.c;
    }

    let mut low = 0.0;
    let mut high = color.c;
    for _ in 0..16 {
        let mid = (low + high) * 0.5;
        let candidate = Oklch { c: mid, ..color };
        if oklch_in_srgb_gamut(candidate) {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

fn oklch_in_srgb_gamut(color: Oklch) -> bool {
    let (r, g, b) = oklch_to_linear_srgb(color);
    in_srgb_gamut(r, g, b)
}

fn oklch_to_linear_srgb(color: Oklch) -> (f32, f32, f32) {
    let a = color.h.cos() * color.c;
    let b = color.h.sin() * color.c;

    let l_ = color.l + 0.39633778 * a + 0.21580376 * b;
    let m_ = color.l - 0.105561346 * a - 0.06385417 * b;
    let s_ = color.l - 0.08948418 * a - 1.2914855 * b;

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
    const EPSILON: f32 = 0.000_001;
    let range = -EPSILON..=1.0 + EPSILON;
    r.is_finite()
        && g.is_finite()
        && b.is_finite()
        && range.contains(&r)
        && range.contains(&g)
        && range.contains(&b)
}

fn delta_e_ok_distance_squared(a: Oklch, b: Oklch) -> f32 {
    let a = oklch_to_oklab(a);
    let b = oklch_to_oklab(b);
    let dl = a.l - b.l;
    let da = a.a - b.a;
    let db = a.b - b.b;
    dl * dl + da * da + db * db
}

fn oklch_to_oklab(color: Oklch) -> Oklab {
    Oklab {
        l: color.l,
        a: color.h.cos() * color.c,
        b: color.h.sin() * color.c,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_config_parses() {
        let config = parse_palette_config(
            r##"
colors = [
    "#000000",
    "#ffffff",
]
"##,
        )
        .expect("palette parses");

        assert!(!config.colors.is_empty());
        assert!(config.colors.len() <= 256);
    }

    #[test]
    fn input_offsets_affect_lookup_entries_and_hash() {
        let palette = [
            Oklch {
                l: 0.0,
                c: 0.0,
                h: 0.0,
            },
            Oklch {
                l: 1.0,
                c: 0.0,
                h: 0.0,
            },
        ];
        let source = Oklch {
            l: 0.4,
            c: 0.0,
            h: 0.0,
        };
        let plain = PaletteMatching {
            lightness: 1.0,
            chroma: 0.0,
            hue: 0.0,
            ..Default::default()
        };
        let shifted = PaletteMatching {
            lightness_add: 0.4,
            ..plain
        };
        let plain_config = PaletteConfig {
            colors: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
            matching: plain,
        };
        let shifted_config = PaletteConfig {
            matching: shifted,
            ..plain_config.clone()
        };

        assert_eq!(nearest_palette_index(source, &palette, plain), 0);
        assert_eq!(nearest_palette_index(source, &palette, shifted), 1);
        assert_ne!(palette_hash(&plain_config), palette_hash(&shifted_config));
    }

    #[test]
    fn self_contained_lookup_stores_binary_palette_and_cooked_entries() {
        let toml = r##"
colors = [
    "#000000",
    "#ffffff",
]

[matching]
oklab_l = 1.0
oklab_a = 0.0
oklab_b = 0.0
"##;
        let config = parse_palette_config(toml).expect("test palette parses");
        let entries = vec![0u8; LUT_ENTRY_COUNT];
        let bytes = encode_lookup(&config, &entries).expect("lookup encodes");
        let lookup = decode_lookup_bundle(&bytes).expect("lookup decodes");

        assert_eq!(&bytes[0..8], LUT_MAGIC_V6);
        assert!(
            !bytes
                .windows(b"[matching]".len())
                .any(|window| window == b"[matching]")
        );
        assert_eq!(lookup.entries().len(), LUT_ENTRY_COUNT);
        assert_eq!(lookup.hash(), lookup_hash(&config, &entries));
        assert_eq!(lookup.config().colors, config.colors);
        assert!(palette_matching_eq(
            lookup.config().matching,
            config.matching
        ));
    }

    #[test]
    fn self_contained_lookup_hash_changes_when_cooked_entries_change() {
        let config = PaletteConfig {
            colors: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
            matching: PaletteMatching::default(),
        };
        let entries = vec![0u8; LUT_ENTRY_COUNT];
        let first = encode_lookup(&config, &entries).expect("first lookup encodes");
        let mut shifted_entries = entries.clone();
        shifted_entries[12345] = 1;
        let second = encode_lookup(&config, &shifted_entries).expect("second lookup encodes");

        assert_ne!(&first[8..16], &second[8..16]);
    }

    #[test]
    fn ipsmap6_stores_matching_altered_and_direct_lookup_entries() {
        let config = PaletteConfig {
            colors: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
            matching: PaletteMatching {
                lightness: 0.2,
                chroma: 0.3,
                hue: 0.5,
                lightness_add: 0.1,
                ..PaletteMatching::default()
            },
        };
        let mut entries = vec![0u8; LUT_ENTRY_COUNT * 2];
        entries[LUT_ENTRY_COUNT + 12345] = 1;

        let bytes = encode_lookup(&config, &entries).expect("lookup encodes");
        let lookup = decode_lookup_bundle(&bytes).expect("lookup decodes");

        assert_eq!(&bytes[0..8], LUT_MAGIC_V6);
        assert_eq!(lookup.altered_entries().len(), LUT_ENTRY_COUNT);
        assert_eq!(
            lookup.direct_entries().expect("direct entries").len(),
            LUT_ENTRY_COUNT
        );
        assert_eq!(lookup.altered_entries()[12345], 0);
        assert_eq!(lookup.direct_entries().expect("direct entries")[12345], 1);
        assert!(palette_matching_eq(
            lookup.config().matching,
            config.matching
        ));
        assert_eq!(lookup.hash(), lookup_hash(&config, &entries));
    }

    #[test]
    fn stale_self_contained_lookup_can_recover_palette_config() {
        let config = PaletteConfig {
            colors: vec![[3, 5, 7, 255], [255, 240, 220, 255]],
            matching: PaletteMatching {
                lightness: 0.1,
                chroma: 0.7,
                hue: 0.2,
                chroma_add: 0.03,
                ..PaletteMatching::default()
            },
        };
        let entries = vec![0u8; LUT_ENTRY_COUNT * 2];
        let mut bytes = encode_lookup(&config, &entries).expect("lookup encodes");
        bytes[8] ^= 0xff;

        assert!(decode_lookup_bundle(&bytes).is_err());
        let recovered =
            decode_lookup_config_unchecked(&bytes).expect("embedded palette can be recovered");

        assert_eq!(recovered.colors, config.colors);
        assert!(palette_matching_eq(recovered.matching, config.matching));
    }

    #[test]
    fn hue_distance_does_not_penalize_neutral_palette_colors() {
        let palette = [
            Oklch::from(rgb_to_oklab(72, 72, 72)),
            Oklch::from(rgb_to_oklab(60, 66, 117)),
        ];
        let source = Oklch::from(rgb_to_oklab(62, 63, 66));
        let matching = PaletteMatching {
            lightness: 0.251,
            chroma: 0.233,
            hue: 0.516,
            chroma_add: 0.031,
            ..Default::default()
        };

        assert_eq!(nearest_palette_index(source, &palette, matching), 0);
    }

    #[test]
    fn delta_e_ok_uses_rectangular_oklab_distance() {
        let redish = Oklch {
            l: 0.5,
            c: 0.1,
            h: 0.0,
        };
        let same = redish;
        let opposite_hue = Oklch {
            h: std::f32::consts::PI,
            ..redish
        };
        assert_eq!(delta_e_ok_distance_squared(redish, same), 0.0);
        assert!(delta_e_ok_distance_squared(redish, opposite_hue) > 0.0);
    }

    #[test]
    fn delta_e_ok_default_weights_are_balanced_oklab_axes() {
        let source = Oklch {
            l: 0.5,
            c: 0.0,
            h: 0.0,
        };
        let lightness_shift = Oklch { l: 0.6, ..source };
        let chroma_shift = Oklch { c: 0.1, ..source };
        let lightness_distance = delta_e_ok_distance_squared(source, lightness_shift);
        let chroma_distance = delta_e_ok_distance_squared(source, chroma_shift);

        assert!((lightness_distance - chroma_distance).abs() < 0.000_01);
    }

    #[test]
    fn chroma_offset_is_clamped_to_reachable_srgb_gamut() {
        let source = Oklch::from(rgb_to_oklab(0, 17, 17));
        let matching = PaletteMatching {
            chroma_add: 0.031,
            ..Default::default()
        };
        let unclamped_chroma = source.c + matching.chroma_add;
        let adjusted = apply_input_offset(source, matching);

        assert!(adjusted.c < unclamped_chroma);
        assert!(oklch_in_srgb_gamut(adjusted));
    }

    #[test]
    fn near_neutral_chroma_offset_ramps_in_instead_of_jumping() {
        let source = Oklch {
            l: 0.5,
            c: 0.00666,
            h: -std::f32::consts::FRAC_PI_2,
        };
        let matching = PaletteMatching {
            chroma_add: 0.05,
            ..Default::default()
        };
        let adjusted = apply_input_offset(source, matching);

        assert!(adjusted.c > source.c);
        assert!(adjusted.c < 0.012);
    }

    #[test]
    fn near_black_blue_noise_does_not_jump_to_saturated_purple() {
        let palette = [
            Oklch::from(rgb_to_oklab(8, 8, 8)),
            Oklch::from(rgb_to_oklab(14, 13, 60)),
        ];
        let source = Oklch::from(rgb_to_oklab(10, 11, 14));
        let matching = PaletteMatching {
            chroma_add: 0.05,
            ..Default::default()
        };

        assert_eq!(nearest_palette_index(source, &palette, matching), 0);
    }

    #[test]
    fn chroma_multiply_affects_lookup_choice() {
        let palette = [
            Oklch {
                l: 0.5,
                c: 0.08,
                h: 0.0,
            },
            Oklch {
                l: 0.5,
                c: 0.16,
                h: 0.0,
            },
        ];
        let source = Oklch {
            l: 0.5,
            c: 0.09,
            h: 0.0,
        };
        let plain = PaletteMatching {
            lightness: 0.0,
            chroma: 1.0,
            hue: 0.0,
            grey_chroma_threshold: 0.001,
            ..Default::default()
        };
        let multiplied = PaletteMatching {
            chroma_multiply: 1.0,
            ..plain
        };

        assert_eq!(nearest_palette_index(source, &palette, plain), 0);
        assert_eq!(nearest_palette_index(source, &palette, multiplied), 1);
    }

    #[test]
    fn grey_chroma_threshold_blocks_degenerate_chroma_offsets() {
        let palette = [
            Oklch {
                l: 0.5,
                c: 0.0,
                h: 0.0,
            },
            Oklch {
                l: 0.5,
                c: 0.17,
                h: 0.0,
            },
        ];
        let source = Oklch {
            l: 0.5,
            c: 0.05,
            h: 0.0,
        };
        let matching = PaletteMatching {
            lightness: 0.0,
            chroma: 1.0,
            hue: 0.0,
            chroma_add: 0.12,
            grey_chroma_threshold: 0.1,
            ..Default::default()
        };
        let disabled_threshold = PaletteMatching {
            grey_chroma_threshold: 0.0,
            ..matching
        };

        assert_eq!(nearest_palette_index(source, &palette, matching), 0);
        assert_eq!(
            nearest_palette_index(source, &palette, disabled_threshold),
            1
        );
    }

    #[test]
    fn near_identical_dark_blue_beats_pale_cyan_without_anchoring() {
        let palette = [
            Oklch::from(rgb_to_oklab(12, 24, 82)),
            Oklch::from(rgb_to_oklab(172, 236, 244)),
        ];
        let source = Oklch::from(rgb_to_oklab(8, 20, 70));
        let matching = PaletteMatching {
            lightness: 0.333,
            chroma: 0.333,
            hue: 0.334,
            ..Default::default()
        };

        assert_eq!(nearest_palette_index(source, &palette, matching), 0);
    }
}
