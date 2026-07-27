//! Palette-indexed frame encoder and transport batching.
//!
//! The encoder converts raw indexed frames into compact tile packets, choosing
//! keyframes, deltas, RLE, and cached tiles based on the previous framebuffer.

use crate::{
    frames::{RawFrame, RawFramePixels},
    gpu_lookup::build_lookup_gpu_with_progress,
    palette_lut::{
        PaletteConfig, PaletteLookup, PaletteMatching, build_lookup_with_progress,
        load_lookup_bundle, recover_lookup_config, write_lookup,
    },
    stats::SharedStats,
    stream_control::CustomStreamState,
};
use bevy::prelude::Resource;
use crossbeam_channel::Receiver;
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

const CODEC_MAGIC: &[u8; 4] = b"IPSC";
const CODEC_VERSION: u8 = 1;
const TILE_SIZE: usize = 8;
const TILE_CACHE_LIMIT: usize = 4096;
const KEYFRAME_INTERVAL: u32 = 25;
const FRAME_HISTORY_SECONDS: u64 = 60;
#[cfg(test)]
const OKLCH_HUE_COUNT: usize = 20;
#[cfg(test)]
const OKLCH_HUE_OFFSET_DEGREES: f32 = 29.233885;
#[cfg(test)]
const OKLCH_LIGHTNESS_LEVELS: [f32; 16] = [
    0.0, 0.06666667, 0.13333334, 0.2, 0.26666668, 0.33333334, 0.4, 0.46666667, 0.53333336, 0.6,
    0.6666667, 0.73333335, 0.8, 0.8666667, 0.93333334, 1.0,
];
#[cfg(test)]
const OKLCH_CHROMA_LEVELS: [f32; 3] = [0.08589443, 0.17178887, 0.2576833];

#[derive(Clone, Copy)]
pub(crate) struct PaletteBias {
    pub(crate) lightness: f32,
    pub(crate) chroma: f32,
    pub(crate) hue: f32,
    pub(crate) lightness_multiply: f32,
    pub(crate) lightness_add: f32,
    pub(crate) chroma_multiply: f32,
    pub(crate) chroma_add: f32,
    pub(crate) grey_chroma_threshold: f32,
    pub(crate) hue_add: f32,
}

impl Default for PaletteBias {
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

pub(crate) fn load_palette_runtime(path: impl AsRef<Path>) -> (Vec<[u8; 4]>, PaletteBias) {
    let palette_config = load_palette_lookup_runtime(path.as_ref()).config().clone();

    (
        palette_config.colors,
        PaletteBias::from(palette_config.matching),
    )
}

pub(crate) fn load_palette_lookup_runtime(path: &Path) -> PaletteLookup {
    match load_lookup_bundle(path) {
        Ok(lookup) => lookup,
        Err(load_err) => {
            match recover_lookup_config(path).and_then(|config| {
                eprintln!(
                    "Palette lookup {} is stale or invalid ({load_err}); regenerating IPSMAP6 from embedded palette colors.",
                    path.display()
                );
                let entries = rebuild_runtime_lookup_entries(&config);
                write_lookup(path, &config, &entries)?;
                load_lookup_bundle(path)
            }) {
                Ok(lookup) => lookup,
                Err(rebuild_err) => {
                    panic!(
                        "Could not load palette lookup {}: {load_err}; recovery failed: {rebuild_err}",
                        path.display()
                    );
                }
            }
        }
    }
}

fn rebuild_runtime_lookup_entries(config: &PaletteConfig) -> Vec<u8> {
    match build_lookup_gpu_with_progress(config, |_| {}) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("GPU palette lookup rebuild failed ({err}); using CPU fallback.");
            build_lookup_with_progress(config, |_| {})
        }
    }
}

impl From<PaletteMatching> for PaletteBias {
    fn from(matching: PaletteMatching) -> Self {
        Self {
            lightness: matching.lightness,
            chroma: matching.chroma,
            hue: matching.hue,
            lightness_multiply: matching.lightness_multiply,
            lightness_add: matching.lightness_add,
            chroma_multiply: matching.chroma_multiply,
            chroma_add: matching.chroma_add,
            grey_chroma_threshold: matching.grey_chroma_threshold,
            hue_add: matching.hue_add,
        }
    }
}

impl From<PaletteBias> for PaletteMatching {
    fn from(bias: PaletteBias) -> Self {
        Self {
            lightness: bias.lightness,
            chroma: bias.chroma,
            hue: bias.hue,
            lightness_multiply: bias.lightness_multiply,
            lightness_add: bias.lightness_add,
            chroma_multiply: bias.chroma_multiply,
            chroma_add: bias.chroma_add,
            grey_chroma_threshold: bias.grey_chroma_threshold,
            hue_add: bias.hue_add,
        }
    }
}

#[derive(Clone, bevy::prelude::Resource)]
pub(crate) struct SharedPaletteBias(Arc<Mutex<PaletteBias>>);

impl SharedPaletteBias {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(PaletteBias::default())))
    }

    pub(crate) fn get(&self) -> PaletteBias {
        self.0.lock().map(|bias| *bias).unwrap_or_default()
    }

    pub(crate) fn set(&self, bias: PaletteBias) {
        if let Ok(mut current) = self.0.lock() {
            *current = bias;
        }
    }
}

#[derive(Clone, Resource)]
pub(crate) struct PaletteFrameHub {
    inner: Arc<(Mutex<LatestPaletteFrame>, Condvar)>,
}

#[derive(Default)]
struct LatestPaletteFrame {
    sequence: u64,
    stream_header: Option<Arc<Vec<u8>>>,
    encoded_history: VecDeque<EncodedPaletteBatchEntry>,
}

#[derive(Clone)]
pub(crate) struct PaletteFramePacket {
    pub(crate) sequence: u64,
    pub(crate) framebuffer: Arc<Framebuffer>,
    pub(crate) is_keyframe: bool,
    pub(crate) frame_index: u32,
}

#[derive(Clone)]
pub(crate) struct EncodedPaletteBatch {
    pub(crate) sequence: u64,
    pub(crate) packets: Arc<Vec<Vec<u8>>>,
    pub(crate) start_packet_index: Option<usize>,
    pub(crate) bytes: usize,
}

#[derive(Clone)]
pub(crate) struct EncodedPaletteStartBatch {
    pub(crate) batch: EncodedPaletteBatch,
    pub(crate) start_packet_index: usize,
}

#[derive(Clone)]
struct EncodedPaletteBatchEntry {
    captured_at: Instant,
    batch: EncodedPaletteBatch,
}

impl PaletteFrameHub {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(LatestPaletteFrame::default()), Condvar::new())),
        }
    }

    pub(crate) fn clear(&self) {
        let (lock, ready) = &*self.inner;
        if let Ok(mut latest) = lock.lock() {
            latest.sequence = 0;
            latest.stream_header = None;
            latest.encoded_history.clear();
            ready.notify_all();
        }
    }

    fn publish_encoded_batch(
        &self,
        stream_header: Vec<u8>,
        sequence: u64,
        captured_at: Instant,
        encoded: PaletteBatchEncoding,
    ) {
        let (lock, ready) = &*self.inner;
        if let Ok(mut latest) = lock.lock() {
            latest.sequence = latest.sequence.max(sequence);
            let start_packet_index = if encoded.packets.is_empty() {
                None
            } else {
                Some(0)
            };
            let batch = EncodedPaletteBatch {
                sequence,
                packets: Arc::new(encoded.packets),
                start_packet_index,
                bytes: encoded.bytes,
            };
            latest.stream_header = Some(Arc::new(stream_header));
            latest
                .encoded_history
                .push_back(EncodedPaletteBatchEntry { captured_at, batch });
            prune_encoded_palette_history(&mut latest.encoded_history);
            ready.notify_all();
        }
    }

    pub(crate) fn stream_header(&self) -> Option<Arc<Vec<u8>>> {
        let (lock, _) = &*self.inner;
        let latest = lock.lock().ok()?;
        latest.stream_header.clone()
    }

    pub(crate) fn wait_for_delayed_encoded_start_batch(
        &self,
        delay: Duration,
        min_packets: usize,
        stats: &SharedStats,
        should_continue: impl Fn() -> bool,
    ) -> Option<EncodedPaletteStartBatch> {
        let (lock, ready) = &*self.inner;
        let mut latest = lock.lock().ok()?;

        loop {
            if !should_continue() {
                return None;
            }

            let now = Instant::now();
            let cutoff = now.checked_sub(delay).unwrap_or(now);
            if let Some(entry) = latest.encoded_history.iter().rev().find(|entry| {
                entry.captured_at <= cutoff
                    && entry.batch.start_packet_index.is_some()
                    && entry.batch.packets.len() >= min_packets.max(1)
            }) {
                return Some(EncodedPaletteStartBatch {
                    batch: entry.batch.clone(),
                    start_packet_index: entry.batch.start_packet_index.expect("checked above"),
                });
            }

            let deadline = next_any_encoded_batch_deadline(&latest.encoded_history, delay, now);
            latest = wait_for_next_palette_deadline(ready, latest, deadline, Some(stats))?;
        }
    }

    pub(crate) fn has_delayed_encoded_start_batch(
        &self,
        delay: Duration,
        min_packets: usize,
    ) -> bool {
        let (lock, _) = &*self.inner;
        let Ok(latest) = lock.lock() else {
            return false;
        };
        let now = Instant::now();
        let cutoff = now.checked_sub(delay).unwrap_or(now);
        latest.encoded_history.iter().rev().any(|entry| {
            entry.captured_at <= cutoff
                && entry.batch.start_packet_index.is_some()
                && entry.batch.packets.len() >= min_packets.max(1)
        })
    }

    pub(crate) fn wait_for_delayed_encoded_batch_after(
        &self,
        last_sequence: u64,
        delay: Duration,
        stats: &SharedStats,
        should_continue: impl Fn() -> bool,
    ) -> Option<EncodedPaletteBatch> {
        let (lock, ready) = &*self.inner;
        let mut latest = lock.lock().ok()?;

        loop {
            if !should_continue() {
                return None;
            }

            let now = Instant::now();
            let cutoff = now.checked_sub(delay).unwrap_or(now);
            if let Some(entry) = latest
                .encoded_history
                .iter()
                .find(|entry| entry.batch.sequence > last_sequence && entry.captured_at <= cutoff)
            {
                return Some(entry.batch.clone());
            }

            let deadline = next_encoded_batch_deadline_after(
                &latest.encoded_history,
                last_sequence,
                delay,
                now,
            );
            latest = wait_for_next_palette_deadline(ready, latest, deadline, Some(stats))?;
        }
    }
}

fn wait_for_next_palette_deadline<'a>(
    ready: &Condvar,
    latest: std::sync::MutexGuard<'a, LatestPaletteFrame>,
    deadline_wait: Option<Duration>,
    stats: Option<&SharedStats>,
) -> Option<std::sync::MutexGuard<'a, LatestPaletteFrame>> {
    let Some(wait_duration) = deadline_wait else {
        let (latest, result) = ready
            .wait_timeout(latest, Duration::from_millis(100))
            .ok()?;
        record_palette_wait(stats, result.timed_out());
        return Some(latest);
    };

    let (latest, result) = ready
        .wait_timeout(
            latest,
            wait_duration
                .max(Duration::from_millis(1))
                .min(Duration::from_millis(100)),
        )
        .ok()?;
    record_palette_wait(stats, result.timed_out());
    Some(latest)
}

fn record_palette_wait(stats: Option<&SharedStats>, timed_out: bool) {
    if let Some(stats) = stats {
        stats.with_mut(|stats| {
            if timed_out {
                stats.custom_sender_wait_timeouts += 1;
            } else {
                stats.custom_sender_wait_wakeups += 1;
            }
        });
    }
}

fn next_any_encoded_batch_deadline(
    history: &VecDeque<EncodedPaletteBatchEntry>,
    delay: Duration,
    now: Instant,
) -> Option<Duration> {
    history
        .iter()
        .map(|entry| encoded_batch_deadline_wait(entry, delay, now))
        .min()
}

fn next_encoded_batch_deadline_after(
    history: &VecDeque<EncodedPaletteBatchEntry>,
    last_sequence: u64,
    delay: Duration,
    now: Instant,
) -> Option<Duration> {
    history
        .iter()
        .filter(|entry| entry.batch.sequence > last_sequence)
        .map(|entry| encoded_batch_deadline_wait(entry, delay, now))
        .min()
}

fn encoded_batch_deadline_wait(
    entry: &EncodedPaletteBatchEntry,
    delay: Duration,
    now: Instant,
) -> Duration {
    (entry.captured_at + delay).saturating_duration_since(now)
}

fn prune_encoded_palette_history(history: &mut VecDeque<EncodedPaletteBatchEntry>) {
    if let Some(latest_keyframe_position) = history
        .iter()
        .rposition(|entry| entry.batch.start_packet_index.is_some())
    {
        for _ in 0..latest_keyframe_position {
            history.pop_front();
        }
        return;
    }

    let cutoff = Instant::now()
        .checked_sub(Duration::from_secs(FRAME_HISTORY_SECONDS))
        .unwrap_or_else(Instant::now);
    while history
        .front()
        .is_some_and(|entry| entry.captured_at < cutoff && history.len() > 1)
    {
        history.pop_front();
    }
}

pub(crate) fn start_palette_preview_encoder(
    receiver: Receiver<RawFrame>,
    frame_hub: PaletteFrameHub,
    stats: SharedStats,
    palette_bias: SharedPaletteBias,
    active: CustomStreamState,
    palette_lookup_path: impl AsRef<Path>,
    batch_size: usize,
) {
    let palette_lookup_path = palette_lookup_path.as_ref().to_owned();
    let palette_lookup = load_palette_lookup_runtime(&palette_lookup_path);
    let palette_config = palette_lookup.config().clone();
    let palette_colors = palette_config.colors.clone();
    let palette_matching = PaletteBias::from(palette_config.matching);
    palette_bias.set(palette_matching);

    thread::spawn(move || {
        let mut publisher =
            IndexedPixelEncoder::new(palette_colors, palette_matching, Some(palette_lookup));
        let mut pending_batch = Vec::with_capacity(batch_size.max(1));
        let mut previous_framebuffer = None;
        let mut sequence = 0u64;
        let mut was_active = false;

        for raw_frame in receiver {
            let is_active = active.is_active();
            if !is_active {
                if was_active {
                    pending_batch.clear();
                    previous_framebuffer = None;
                    sequence = 0;
                }
                was_active = false;
                continue;
            }
            if !was_active {
                pending_batch.clear();
                previous_framebuffer = None;
                sequence = 0;
                publisher.reset();
                was_active = true;
            }
            let captured_at = raw_frame.captured_at;
            let pipeline_started = Instant::now();
            stats.with_mut(|stats| stats.frames_read += 1);
            let encode_started = Instant::now();
            match publisher.encode(&raw_frame, palette_bias.get()) {
                Ok(encoded) => {
                    let encode_ms = encode_started.elapsed().as_secs_f64() * 1000.0;
                    let stream_header = encoded.stream_header.clone();
                    let is_keyframe = encoded.is_keyframe;
                    let frame_index = encoded.frame_index;
                    let framebuffer = encoded.framebuffer;
                    sequence += 1;

                    if !active.is_active() {
                        continue;
                    }

                    let force_batch_start_keyframe = pending_batch.is_empty();
                    pending_batch.push(PaletteFramePacket {
                        sequence,
                        framebuffer: Arc::new(framebuffer),
                        is_keyframe: is_keyframe || force_batch_start_keyframe,
                        frame_index,
                    });

                    stats.with_mut(|stats| {
                        stats.frames_encoded += 1;
                        stats.custom_stage = "streaming";
                        stats.record_custom_encode(encode_ms);
                    });

                    if pending_batch.len() >= batch_size.max(1) {
                        let publish_started = Instant::now();
                        let encoded_batch = encode_palette_batch_packets(
                            previous_framebuffer.clone(),
                            &pending_batch,
                        );
                        previous_framebuffer = encoded_batch.last_framebuffer.clone();
                        let encoded_frame_count = encoded_batch.packets.len();
                        let encoded_bytes = encoded_batch.bytes;
                        let encoded_keyframes = encoded_batch.keyframes;
                        let encoded_delta_frames = encoded_batch.delta_frames;
                        let encoded_tile_counts = encoded_batch.tile_counts;
                        let latest_frame_bytes = encoded_bytes / encoded_frame_count.max(1);
                        stats.with_mut(|stats| {
                            stats.record_custom_http_batch(encoded_frame_count);
                            stats.custom_bytes_sent += (encoded_bytes + 4) as u64;
                            stats.custom_keyframes_sent += encoded_keyframes;
                            stats.custom_delta_frames_sent += encoded_delta_frames;
                            stats.custom_raw_tiles_sent += encoded_tile_counts.raw;
                            stats.custom_solid_tiles_sent += encoded_tile_counts.solid;
                            stats.custom_rle_tiles_sent += encoded_tile_counts.rle;
                            stats.custom_span_tiles_sent += encoded_tile_counts.span_delta;
                            stats.custom_xor_tiles_sent += encoded_tile_counts.xor_rle;
                            stats.custom_cached_tiles_sent += encoded_tile_counts.cached;
                            stats.custom_skipped_tiles += encoded_tile_counts.skipped;
                            stats.latest_frame_bytes = latest_frame_bytes;
                            stats.custom_frames_sent += encoded_frame_count as u64;
                            stats.record_custom_frame_batch_sent(
                                encoded_frame_count,
                                Duration::from_secs_f64(1.0 / active.fps().max(1) as f64),
                            );
                        });
                        frame_hub.publish_encoded_batch(
                            stream_header,
                            pending_batch
                                .last()
                                .map(|packet| packet.sequence)
                                .unwrap_or(sequence),
                            captured_at,
                            encoded_batch,
                        );
                        pending_batch.clear();
                        let publish_ms = publish_started.elapsed().as_secs_f64() * 1000.0;
                        let total_ms = pipeline_started.elapsed().as_secs_f64() * 1000.0;
                        stats.with_mut(|stats| {
                            stats.record_custom_record(0.0);
                            stats.record_custom_publish(publish_ms);
                            stats.record_custom_pipeline(total_ms);
                        });
                    }
                }
                Err(err) => {
                    eprintln!("Indexed pixel frame encode failed: {err}");
                    stats.with_mut(|stats| {
                        stats.custom_stage = "encode error";
                        stats.custom_last_error = err;
                    });
                }
            }
        }
    });
}

struct IndexedPixelEncoder {
    palette: Vec<[u8; 4]>,
    palette_oklab: Vec<Oklab>,
    exact_palette_indices: Box<[u16]>,
    lookup: Option<PaletteLookup>,
    lookup_matching: PaletteBias,
    previous: Option<Framebuffer>,
    frame_index: u32,
    header: Option<Vec<u8>>,
    header_width: u32,
    header_height: u32,
}

struct EncodedFrame {
    stream_header: Vec<u8>,
    framebuffer: Framebuffer,
    is_keyframe: bool,
    frame_index: u32,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct TileModeCounts {
    pub(crate) raw: u64,
    pub(crate) solid: u64,
    pub(crate) rle: u64,
    pub(crate) span_delta: u64,
    pub(crate) xor_rle: u64,
    pub(crate) cached: u64,
    pub(crate) skipped: u64,
}

#[derive(Default)]
struct TileCache {
    tiles: Vec<[u8; 64]>,
    lookup: HashMap<[u8; 64], u16>,
}

#[allow(dead_code)]
impl IndexedPixelEncoder {
    fn new(
        palette: Vec<[u8; 4]>,
        lookup_matching: PaletteBias,
        lookup: Option<PaletteLookup>,
    ) -> Self {
        let mut exact_palette_indices = vec![u16::MAX; 1 << 24].into_boxed_slice();
        for (index, [r, g, b, _]) in palette.iter().enumerate() {
            exact_palette_indices
                [((usize::from(*r)) << 16) | ((usize::from(*g)) << 8) | usize::from(*b)] =
                index as u16;
        }
        let palette_oklab = palette
            .iter()
            .map(|[r, g, b, _]| rgb_to_oklab(*r, *g, *b))
            .collect();
        Self {
            palette,
            palette_oklab,
            exact_palette_indices,
            lookup,
            lookup_matching,
            previous: None,
            frame_index: 0,
            header: None,
            header_width: 0,
            header_height: 0,
        }
    }

    fn reset(&mut self) {
        self.previous = None;
        self.frame_index = 0;
        self.header = None;
        self.header_width = 0;
        self.header_height = 0;
    }

    fn encode(&mut self, raw: &RawFrame, bias: PaletteBias) -> Result<EncodedFrame, String> {
        let current = match &raw.pixels {
            RawFramePixels::Bgra(_) => self.quantize(raw, bias)?,
            RawFramePixels::Indexed(pixels) => self.framebuffer_from_indexed(raw, pixels)?,
        };
        self.encode_framebuffer(raw.width, raw.height, current)
    }

    fn encode_framebuffer(
        &mut self,
        width: u32,
        height: u32,
        current: Framebuffer,
    ) -> Result<EncodedFrame, String> {
        if width != height
            || width == 0
            || width > u8::MAX as u32 + 1
            || !(width as usize).is_multiple_of(TILE_SIZE)
            || !(height as usize).is_multiple_of(TILE_SIZE)
        {
            return Err(
                "IPSC frames must be square, 8-aligned, and no larger than 256x256".to_owned(),
            );
        }

        if self.palette.is_empty() || self.palette.len() > 256 {
            return Err("IPSC palette must contain 1-256 colors".to_owned());
        }

        if self.header_width != width || self.header_height != height {
            self.previous = None;
            self.frame_index = 0;
            self.header = Some(stream_header(width as u16, height as u16, &self.palette));
            self.header_width = width;
            self.header_height = height;
        }
        let header = self
            .header
            .as_ref()
            .expect("header initialized for current resolution")
            .clone();
        let is_keyframe =
            self.previous.is_none() || self.frame_index.is_multiple_of(KEYFRAME_INTERVAL);
        let frame_index = self.frame_index;

        let framebuffer = current.clone();
        self.previous = Some(current);
        self.frame_index = self.frame_index.wrapping_add(1);

        Ok(EncodedFrame {
            stream_header: header,
            framebuffer,
            is_keyframe,
            frame_index,
        })
    }

    fn quantize(&self, raw: &RawFrame, bias: PaletteBias) -> Result<Framebuffer, String> {
        let pixel_count = raw.width as usize * raw.height as usize;
        let RawFramePixels::Bgra(bgra) = &raw.pixels else {
            return Err("indexed frame cannot be quantized as BGRA".to_owned());
        };
        if bgra.len() < pixel_count * 4 {
            return Err("raw frame is shorter than expected".to_owned());
        }

        let mut pixels = Vec::with_capacity(pixel_count);
        for pixel in bgra.chunks_exact(4).take(pixel_count) {
            let b = pixel[0];
            let g = pixel[1];
            let r = pixel[2];
            let exact_index = self.exact_palette_indices
                [((usize::from(r)) << 16) | ((usize::from(g)) << 8) | usize::from(b)];
            pixels.push(if exact_index != u16::MAX {
                exact_index as u8
            } else {
                self.palette_index(r, g, b, bias)
            });
        }

        Ok(Framebuffer {
            pixels,
            width: raw.width as usize,
            height: raw.height as usize,
        })
    }

    fn framebuffer_from_indexed(
        &self,
        raw: &RawFrame,
        indexed: &[u8],
    ) -> Result<Framebuffer, String> {
        let pixel_count = raw.width as usize * raw.height as usize;
        if indexed.len() < pixel_count {
            return Err("indexed frame is shorter than expected".to_owned());
        }

        Ok(Framebuffer {
            pixels: indexed[..pixel_count].to_vec(),
            width: raw.width as usize,
            height: raw.height as usize,
        })
    }

    fn palette_index(&self, r: u8, g: u8, b: u8, bias: PaletteBias) -> u8 {
        if self.lookup_can_match(bias)
            && let Some(lookup) = &self.lookup
        {
            let index = (r as usize) << 16 | (g as usize) << 8 | b as usize;
            return lookup.altered_entries()[index];
        }

        self.nearest_palette_index(r, g, b, bias)
    }

    fn lookup_can_match(&self, bias: PaletteBias) -> bool {
        self.lookup.is_some()
            && (bias.lightness - self.lookup_matching.lightness).abs() <= 0.000_5
            && (bias.chroma - self.lookup_matching.chroma).abs() <= 0.000_5
            && (bias.hue - self.lookup_matching.hue).abs() <= 0.000_5
            && (bias.lightness_multiply - self.lookup_matching.lightness_multiply).abs() <= 0.000_5
            && (bias.lightness_add - self.lookup_matching.lightness_add).abs() <= 0.000_5
            && (bias.chroma_multiply - self.lookup_matching.chroma_multiply).abs() <= 0.000_5
            && (bias.chroma_add - self.lookup_matching.chroma_add).abs() <= 0.000_5
            && (bias.grey_chroma_threshold - self.lookup_matching.grey_chroma_threshold).abs()
                <= 0.000_5
            && (bias.hue_add - self.lookup_matching.hue_add).abs() <= 0.000_5
    }

    fn nearest_palette_index(&self, r: u8, g: u8, b: u8, bias: PaletteBias) -> u8 {
        let color = Oklch::from(rgb_to_oklab(r, g, b)).with_input_offset(bias);
        let mut best_index = 0;
        let mut best_distance = f32::MAX;

        for (index, palette_color) in self.palette_oklab.iter().copied().take(256).enumerate() {
            let distance = color.delta_e_ok_distance_squared(Oklch::from(palette_color));
            if distance < best_distance {
                best_distance = distance;
                best_index = index as u8;
            }
        }

        best_index
    }
}

fn stream_header(width: u16, height: u16, palette: &[[u8; 4]]) -> Vec<u8> {
    let mut header = Vec::with_capacity(4 + 1 + 2 + 2 + 1 + 2 + palette.len() * 4);
    header.extend_from_slice(CODEC_MAGIC);
    header.push(CODEC_VERSION);
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&height.to_le_bytes());
    header.push(TILE_SIZE as u8);
    header.extend_from_slice(&(palette.len() as u16).to_le_bytes());
    for color in palette {
        header.extend_from_slice(color);
    }
    header
}

fn encode_frame_packet(is_keyframe: bool, frame_index: u32, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(1 + 4 + 4 + payload.len());
    frame.push(if is_keyframe { 0 } else { 1 });
    frame.extend_from_slice(&frame_index.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub(crate) struct PaletteBatchEncoding {
    pub(crate) packets: Vec<Vec<u8>>,
    pub(crate) tile_counts: TileModeCounts,
    pub(crate) bytes: usize,
    pub(crate) keyframes: u64,
    pub(crate) delta_frames: u64,
    pub(crate) last_framebuffer: Option<Arc<Framebuffer>>,
}

pub(crate) fn encode_palette_batch_packets(
    previous_framebuffer: Option<Arc<Framebuffer>>,
    batch: &[PaletteFramePacket],
) -> PaletteBatchEncoding {
    let mut previous = previous_framebuffer;
    let mut tile_cache = TileCache::default();
    let mut packets = Vec::with_capacity(batch.len());
    let mut tile_counts = TileModeCounts::default();
    let mut bytes = 0usize;
    let mut keyframes = 0u64;
    let mut delta_frames = 0u64;

    for packet in batch {
        let current = packet.framebuffer.as_ref();
        let encode_as_keyframe = packet.is_keyframe || previous.is_none();
        let (payload, counts) = if encode_as_keyframe {
            keyframes += 1;
            let payload = encode_keyframe_raw(current);
            tile_cache = TileCache::default();
            seed_tile_cache_from_frame(current, &mut tile_cache);
            (payload, TileModeCounts::default())
        } else {
            delta_frames += 1;
            let (payload, counts) = encode_delta_frame_with_tile_cache(
                current,
                previous
                    .as_ref()
                    .expect("previous framebuffer is available")
                    .as_ref(),
                &mut tile_cache,
            );
            (payload, counts)
        };

        tile_counts.add(counts);
        let frame = encode_frame_packet(encode_as_keyframe, packet.frame_index, &payload);
        bytes += 4 + frame.len();
        packets.push(frame);
        previous = Some(packet.framebuffer.clone());
    }

    PaletteBatchEncoding {
        packets,
        tile_counts,
        bytes,
        keyframes,
        delta_frames,
        last_framebuffer: previous,
    }
}

#[cfg(test)]
fn generated_test_palette() -> Vec<[u8; 4]> {
    let mut palette = Vec::with_capacity(256);
    for lightness in OKLCH_LIGHTNESS_LEVELS {
        palette.push(greyscale_color(lightness));
    }

    for hue_index in 0..OKLCH_HUE_COUNT {
        let hue_degrees =
            OKLCH_HUE_OFFSET_DEGREES + hue_index as f32 * 360.0 / OKLCH_HUE_COUNT as f32;
        for chroma in OKLCH_CHROMA_LEVELS.iter().copied() {
            for lightness in OKLCH_LIGHTNESS_LEVELS.iter().copied() {
                if lightness > 0.0
                    && lightness < 1.0
                    && let Some(color) = checked_oklch_to_srgb(lightness, chroma, hue_degrees)
                {
                    palette.push(color);
                }
            }
        }
    }
    while palette.len() < 256 {
        palette.push([0x00, 0x00, 0x00, 0xff]);
    }
    palette
}

#[cfg(test)]
fn greyscale_color(lightness: f32) -> [u8; 4] {
    if lightness <= 0.0 {
        [0x00, 0x00, 0x00, 0xff]
    } else if lightness >= 1.0 {
        [0xff, 0xff, 0xff, 0xff]
    } else {
        oklch_to_srgb(lightness, 0.0, 0.0)
    }
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

impl Oklch {
    fn with_input_offset(self, bias: PaletteBias) -> Self {
        let grey_chroma_threshold = bias.grey_chroma_threshold.clamp(0.0, 1.0);
        let chroma_offset_weight =
            smoothstep(grey_chroma_threshold, grey_chroma_threshold + 0.04, self.c);
        let adjusted_chroma = (self.c
            + (self.c * bias.chroma_multiply + bias.chroma_add) * chroma_offset_weight)
            .max(0.0);
        let mut adjusted = Self {
            l: (self.l * (1.0 + bias.lightness_multiply) + bias.lightness_add).clamp(0.0, 1.0),
            c: adjusted_chroma,
            h: self.h + bias.hue_add * std::f32::consts::TAU,
        };
        if PaletteMatching::from(bias).has_input_offset() {
            adjusted.c = clamp_chroma_to_srgb_gamut(adjusted);
        }
        adjusted
    }

    fn delta_e_ok_distance_squared(self, other: Self) -> f32 {
        let self_lab = oklch_to_oklab(self);
        let other_lab = oklch_to_oklab(other);
        let dl = self_lab.l - other_lab.l;
        let da = self_lab.a - other_lab.a;
        let db = self_lab.b - other_lab.b;
        dl * dl + da * da + db * db
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn oklch_to_oklab(color: Oklch) -> Oklab {
    Oklab {
        l: color.l,
        a: color.h.cos() * color.c,
        b: color.h.sin() * color.c,
    }
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
    let (r, g, b) = oklch_to_linear_srgb_radians(color.l, color.c, color.h);
    in_srgb_gamut(r, g, b)
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

#[cfg(test)]
fn oklch_to_srgb(lightness: f32, chroma: f32, hue_degrees: f32) -> [u8; 4] {
    let (r, g, blue) = oklch_to_linear_srgb(lightness, chroma, hue_degrees);
    [
        linear_to_u8(r.clamp(0.0, 1.0)),
        linear_to_u8(g.clamp(0.0, 1.0)),
        linear_to_u8(blue.clamp(0.0, 1.0)),
        0xff,
    ]
}

#[cfg(test)]
fn checked_oklch_to_srgb(lightness: f32, chroma: f32, hue_degrees: f32) -> Option<[u8; 4]> {
    let (r, g, b) = oklch_to_linear_srgb(lightness, chroma, hue_degrees);
    in_srgb_gamut(r, g, b).then(|| oklch_to_srgb(lightness, chroma, hue_degrees))
}

#[cfg(test)]
fn oklch_to_linear_srgb(lightness: f32, chroma: f32, hue_degrees: f32) -> (f32, f32, f32) {
    oklch_to_linear_srgb_radians(lightness, chroma, hue_degrees.to_radians())
}

fn oklch_to_linear_srgb_radians(lightness: f32, chroma: f32, hue_radians: f32) -> (f32, f32, f32) {
    let a = hue_radians.cos() * chroma;
    let b = hue_radians.sin() * chroma;

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

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
fn linear_to_u8(value: f32) -> u8 {
    let srgb = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

fn in_srgb_gamut(r: f32, g: f32, b: f32) -> bool {
    const EPSILON: f32 = 0.000_001;
    r.is_finite()
        && g.is_finite()
        && b.is_finite()
        && (-EPSILON..=1.0 + EPSILON).contains(&r)
        && (-EPSILON..=1.0 + EPSILON).contains(&g)
        && (-EPSILON..=1.0 + EPSILON).contains(&b)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Framebuffer {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}

impl Framebuffer {
    #[cfg(test)]
    fn new(width: usize, height: usize) -> Self {
        Self {
            pixels: vec![0; width * height],
            width,
            height,
        }
    }

    #[cfg(test)]
    fn set_pixel(&mut self, x: usize, y: usize, color: u8) {
        self.pixels[y * self.width + x] = color;
    }
}

fn encode_keyframe_raw(frame: &Framebuffer) -> Vec<u8> {
    frame.pixels.clone()
}

#[cfg(any(test, feature = "cpu-palette-encoder"))]
#[allow(dead_code)]
fn encode_delta_frame(current: &Framebuffer, previous: &Framebuffer) -> (Vec<u8>, TileModeCounts) {
    let mut tile_cache = TileCache::default();
    encode_delta_frame_impl(current, previous, &mut tile_cache, false)
}

fn encode_delta_frame_with_tile_cache(
    current: &Framebuffer,
    previous: &Framebuffer,
    tile_cache: &mut TileCache,
) -> (Vec<u8>, TileModeCounts) {
    encode_delta_frame_impl(current, previous, tile_cache, true)
}

fn encode_delta_frame_impl(
    current: &Framebuffer,
    previous: &Framebuffer,
    tile_cache: &mut TileCache,
    allow_cached_tiles: bool,
) -> (Vec<u8>, TileModeCounts) {
    let tiles_x = current.width / TILE_SIZE;
    let tiles_y = current.height / TILE_SIZE;
    let tile_count = tiles_x * tiles_y;
    let mask_len = tile_count.div_ceil(8);
    let mut mask = vec![0u8; mask_len];
    let mut payload = Vec::new();
    let mut counts = TileModeCounts::default();

    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            let tile_index = tile_y * tiles_x + tile_x;
            let current_tile = extract_tile(current, tile_x, tile_y);
            let previous_tile = extract_tile(previous, tile_x, tile_y);
            if current_tile == previous_tile {
                counts.skipped += 1;
                continue;
            }

            set_bit(&mut mask, tile_index);
            let cached = allow_cached_tiles
                .then(|| tile_cache.lookup(&current_tile))
                .flatten();
            let mode = write_best_tile(&current_tile, &previous_tile, cached, &mut payload);
            counts.record(mode);
            if allow_cached_tiles {
                tile_cache.remember(current_tile);
            }
        }
    }

    mask.extend_from_slice(&payload);
    (mask, counts)
}

impl TileModeCounts {
    fn add(&mut self, other: Self) {
        self.raw += other.raw;
        self.solid += other.solid;
        self.rle += other.rle;
        self.span_delta += other.span_delta;
        self.xor_rle += other.xor_rle;
        self.cached += other.cached;
        self.skipped += other.skipped;
    }

    fn record(&mut self, mode: TileMode) {
        match mode {
            TileMode::Raw => self.raw += 1,
            TileMode::Solid => self.solid += 1,
            TileMode::Rle => self.rle += 1,
            TileMode::SpanDelta => self.span_delta += 1,
            TileMode::XorRle => self.xor_rle += 1,
            TileMode::Cached => self.cached += 1,
        }
    }
}

impl TileCache {
    fn lookup(&self, tile: &[u8; 64]) -> Option<u16> {
        self.lookup.get(tile).copied()
    }

    fn remember(&mut self, tile: [u8; 64]) {
        if self.tiles.len() >= TILE_CACHE_LIMIT {
            return;
        }

        let index = self.tiles.len() as u16;
        self.tiles.push(tile);
        self.lookup.entry(tile).or_insert(index);
    }
}

fn seed_tile_cache_from_frame(frame: &Framebuffer, tile_cache: &mut TileCache) {
    let tiles_x = frame.width / TILE_SIZE;
    let tiles_y = frame.height / TILE_SIZE;
    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            tile_cache.remember(extract_tile(frame, tile_x, tile_y));
        }
    }
}

fn extract_tile(frame: &Framebuffer, tile_x: usize, tile_y: usize) -> [u8; 64] {
    let mut out = [0u8; 64];
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let src_x = tile_x * TILE_SIZE + x;
            let src_y = tile_y * TILE_SIZE + y;
            out[y * TILE_SIZE + x] = frame.pixels[src_y * frame.width + src_x];
        }
    }
    out
}

fn set_bit(mask: &mut [u8], index: usize) {
    mask[index / 8] |= 1 << (index % 8);
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum TileMode {
    Raw = 0,
    Solid = 1,
    Rle = 2,
    SpanDelta = 3,
    XorRle = 4,
    Cached = 5,
}

#[cfg(test)]
fn encode_best_tile(current: &[u8; 64], previous: &[u8; 64], cached: Option<u16>) -> Vec<u8> {
    let mut out = Vec::with_capacity(best_tile_len(current, previous, cached).1);
    write_best_tile(current, previous, cached, &mut out);
    out
}

fn write_best_tile(
    current: &[u8; 64],
    previous: &[u8; 64],
    cached: Option<u16>,
    out: &mut Vec<u8>,
) -> TileMode {
    let (mode, _) = best_tile_len(current, previous, cached);
    match mode {
        TileMode::Raw => write_raw_tile(current, out),
        TileMode::Solid => write_solid_tile(current, out),
        TileMode::Rle => write_rle_tile(current, out),
        TileMode::SpanDelta => write_span_delta_tile(current, previous, out),
        TileMode::XorRle => write_xor_rle_tile(current, previous, out),
        TileMode::Cached => write_cached_tile(cached.expect("cached tile index is available"), out),
    }
    mode
}

fn best_tile_len(
    current: &[u8; 64],
    previous: &[u8; 64],
    cached: Option<u16>,
) -> (TileMode, usize) {
    let mut best_mode = TileMode::Raw;
    let mut best_len = 65;

    if is_solid_tile(current) && 2 < best_len {
        best_mode = TileMode::Solid;
        best_len = 2;
    }

    let rle_len = rle_tile_len(current);
    if rle_len < best_len {
        best_mode = TileMode::Rle;
        best_len = rle_len;
    }

    let span_len = span_delta_tile_len(current, previous);
    if span_len < best_len {
        best_mode = TileMode::SpanDelta;
        best_len = span_len;
    }

    let xor_len = xor_rle_tile_len(current, previous);
    if xor_len < best_len {
        best_mode = TileMode::XorRle;
        best_len = xor_len;
    }

    if cached.is_some() && 3 < best_len {
        best_mode = TileMode::Cached;
        best_len = 3;
    }

    (best_mode, best_len)
}

fn write_cached_tile(index: u16, out: &mut Vec<u8>) {
    out.push(TileMode::Cached as u8);
    out.extend_from_slice(&index.to_le_bytes());
}

fn write_raw_tile(tile: &[u8; 64], out: &mut Vec<u8>) {
    out.push(TileMode::Raw as u8);
    out.extend_from_slice(tile);
}

fn is_solid_tile(tile: &[u8; 64]) -> bool {
    let first = tile[0];
    tile.iter().all(|&pixel| pixel == first)
}

fn write_solid_tile(tile: &[u8; 64], out: &mut Vec<u8>) {
    out.push(TileMode::Solid as u8);
    out.push(tile[0]);
}

fn rle_tile_len(tile: &[u8; 64]) -> usize {
    2 + count_runs(tile.iter().copied()) * 2
}

fn write_rle_tile(tile: &[u8; 64], out: &mut Vec<u8>) {
    out.push(TileMode::Rle as u8);
    let run_count_index = out.len();
    out.push(0);
    let mut run_count = 0u8;
    write_runs(tile.iter().copied(), out, &mut run_count);
    out[run_count_index] = run_count;
}

fn span_delta_tile_len(current: &[u8; 64], previous: &[u8; 64]) -> usize {
    let mut len = 2usize;
    let mut i = 0usize;

    while i < current.len() {
        while i < current.len() && current[i] == previous[i] {
            i += 1;
        }
        if i >= current.len() {
            break;
        }

        let mut pixel_count = 0usize;
        while i < current.len() && current[i] != previous[i] {
            pixel_count += 1;
            i += 1;
        }
        len += 2 + pixel_count;
    }

    len
}

fn write_span_delta_tile(current: &[u8; 64], previous: &[u8; 64], out: &mut Vec<u8>) {
    out.push(TileMode::SpanDelta as u8);
    let span_count_index = out.len();
    out.push(0);
    let mut span_count = 0u8;
    let mut i = 0usize;

    while i < current.len() {
        let mut skip = 0u8;
        while i < current.len() && current[i] == previous[i] {
            skip += 1;
            i += 1;
        }

        if i >= current.len() {
            break;
        }

        let len_index = out.len() + 1;
        out.push(skip);
        out.push(0);
        let start = i;
        while i < current.len() && current[i] != previous[i] {
            out.push(current[i]);
            i += 1;
        }
        out[len_index] = (i - start) as u8;
        span_count += 1;
    }

    out[span_count_index] = span_count;
}

fn xor_rle_tile_len(current: &[u8; 64], previous: &[u8; 64]) -> usize {
    2 + count_runs(current.iter().zip(previous.iter()).map(|(a, b)| a ^ b)) * 2
}

fn write_xor_rle_tile(current: &[u8; 64], previous: &[u8; 64], out: &mut Vec<u8>) {
    out.push(TileMode::XorRle as u8);
    let run_count_index = out.len();
    out.push(0);
    let mut run_count = 0u8;
    write_runs(
        current.iter().zip(previous.iter()).map(|(a, b)| a ^ b),
        out,
        &mut run_count,
    );
    out[run_count_index] = run_count;
}

fn count_runs(values: impl Iterator<Item = u8>) -> usize {
    let mut run_count = 0usize;
    let mut previous = None;

    for value in values {
        if previous != Some(value) {
            run_count += 1;
            previous = Some(value);
        }
    }

    run_count
}

fn write_runs(values: impl Iterator<Item = u8>, out: &mut Vec<u8>, run_count: &mut u8) {
    let mut previous = None;
    let mut len = 0u8;

    for value in values {
        if previous == Some(value) && len < u8::MAX {
            len += 1;
            continue;
        }

        if let Some(previous) = previous {
            out.push(previous);
            out.push(len);
            *run_count += 1;
        }

        previous = Some(value);
        len = 1;
    }

    if let Some(previous) = previous {
        out.push(previous);
        out.push(len);
        *run_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_roundtrip() {
        let mut frame = Framebuffer::new(128, 128);
        frame.set_pixel(12, 34, 99);
        let encoded = encode_keyframe_raw(&frame);
        assert_eq!(encoded, frame.pixels);
    }

    #[test]
    #[ignore = "rebuilds the full IPSMAP6 lookup"]
    fn stale_runtime_lookup_rebuilds_in_place() {
        let config = PaletteConfig {
            colors: vec![[0, 0, 0, 255], [255, 0, 0, 255], [255, 255, 255, 255]],
            matching: PaletteMatching::default(),
        };
        let entries = vec![0u8; crate::palette_lut::LUT_ENTRY_COUNT * 2];
        let mut bytes = crate::palette_lut::encode_lookup(&config, &entries).expect("encodes");
        bytes[8] ^= 0xff;
        let path = std::env::temp_dir().join(format!(
            "directstreamgame-stale-runtime-{}.ipsmap",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("writes stale lookup");

        let lookup = load_palette_lookup_runtime(&path);
        let rewritten = std::fs::read(&path).expect("reads rebuilt lookup");
        let decoded =
            crate::palette_lut::decode_lookup_bundle(&rewritten).expect("rebuilt lookup decodes");
        let _ = std::fs::remove_file(path);

        assert_eq!(lookup.config().colors, config.colors);
        assert_eq!(decoded.config().colors, config.colors);
        assert_eq!(
            lookup.entries().len(),
            crate::palette_lut::LUT_ENTRY_COUNT * 2
        );
    }

    #[test]
    fn delta_span_roundtrip() {
        let mut previous = Framebuffer::new(128, 128);
        let mut current = previous.clone();
        current.set_pixel(10, 10, 5);
        current.set_pixel(11, 10, 5);
        current.set_pixel(12, 10, 5);

        let mut tile_cache = TileCache::default();
        seed_tile_cache_from_frame(&previous, &mut tile_cache);
        let (encoded, counts) =
            encode_delta_frame_with_tile_cache(&current, &previous, &mut tile_cache);
        assert_eq!(counts.span_delta, 1);
        let decoded = decode_delta_frame_for_test(&previous, &encoded);
        assert_eq!(current, decoded);

        previous = current;
        let (encoded, counts) =
            encode_delta_frame_with_tile_cache(&previous, &previous, &mut tile_cache);
        assert_eq!(
            counts.raw
                + counts.solid
                + counts.rle
                + counts.span_delta
                + counts.xor_rle
                + counts.cached,
            0
        );
        assert_eq!(encoded.len(), 32);
    }

    #[test]
    fn best_tile_prefers_solid() {
        let previous = [0u8; 64];
        let current = [7u8; 64];
        assert_eq!(encode_best_tile(&current, &previous, None), vec![1, 7]);
    }

    #[test]
    fn delta_can_reference_cached_tiles() {
        let previous = Framebuffer::new(128, 128);
        let mut current = previous.clone();
        let mut cached_tile = [0u8; 64];
        cached_tile[0] = 3;
        current.set_pixel(0, 0, 3);

        let mut tile_cache = TileCache::default();
        tile_cache.remember(cached_tile);
        let (encoded, counts) =
            encode_delta_frame_with_tile_cache(&current, &previous, &mut tile_cache);

        assert_eq!(counts.cached, 1);
        assert!(encoded.ends_with(&[TileMode::Cached as u8, 0, 0]));
    }

    #[test]
    fn batch_keyframe_resets_cached_tile_indices() {
        let previous = Arc::new(Framebuffer::new(128, 128));
        let mut keyframe = Framebuffer::new(128, 128);
        keyframe.set_pixel(0, 0, 4);

        let mut delta = keyframe.clone();
        delta.set_pixel(64, 64, 4);

        let batch = vec![
            PaletteFramePacket {
                sequence: 1,
                framebuffer: Arc::new(keyframe),
                is_keyframe: true,
                frame_index: 1,
            },
            PaletteFramePacket {
                sequence: 2,
                framebuffer: Arc::new(delta),
                is_keyframe: false,
                frame_index: 2,
            },
        ];

        let encoded = encode_palette_batch_packets(Some(previous), &batch);
        assert_eq!(encoded.keyframes, 1);
        assert_eq!(encoded.delta_frames, 1);
        assert_eq!(encoded.tile_counts.cached, 1);

        let delta_packet = &encoded.packets[1];
        let payload_len = u32::from_le_bytes([
            delta_packet[5],
            delta_packet[6],
            delta_packet[7],
            delta_packet[8],
        ]) as usize;
        let payload = &delta_packet[9..9 + payload_len];
        assert!(payload.ends_with(&[TileMode::Cached as u8, 0, 0]));
    }

    #[test]
    fn encoder_accepts_256_square_frames() {
        let mut encoder =
            IndexedPixelEncoder::new(generated_test_palette(), PaletteBias::default(), None);
        let raw = RawFrame {
            pixels: RawFramePixels::Bgra(vec![0; 256 * 256 * 4]),
            width: 256,
            height: 256,
            captured_at: Instant::now(),
        };

        let encoded = encoder.encode(&raw, PaletteBias::default()).unwrap();
        assert!(encoded.is_keyframe);
        assert_eq!(&encoded.stream_header[0..4], b"IPSC");
        let packet = PaletteFramePacket {
            sequence: 1,
            framebuffer: Arc::new(encoded.framebuffer),
            is_keyframe: encoded.is_keyframe,
            frame_index: encoded.frame_index,
        };
        let batch = encode_palette_batch_packets(None, &[packet]);
        assert_eq!(batch.packets[0].len(), 1 + 4 + 4 + 256 * 256);
    }

    #[test]
    fn exact_palette_fast_path_uses_rgb_not_bgr_indexing() {
        let mut encoder = IndexedPixelEncoder::new(
            vec![[255, 0, 0, 255], [0, 0, 255, 255]],
            PaletteBias::default(),
            None,
        );
        let raw = RawFrame {
            pixels: RawFramePixels::Bgra([0, 0, 255, 255].repeat(8 * 8)),
            width: 8,
            height: 8,
            captured_at: Instant::now(),
        };

        let encoded = encoder.encode(&raw, PaletteBias::default()).unwrap();

        assert!(encoded.framebuffer.pixels.iter().all(|index| *index == 0));
    }

    #[test]
    fn exact_palette_fast_path_ignores_input_offsets() {
        let mut encoder = IndexedPixelEncoder::new(
            vec![[255, 0, 0, 255], [0, 255, 255, 255]],
            PaletteBias::default(),
            None,
        );
        let raw = RawFrame {
            pixels: RawFramePixels::Bgra([0, 0, 255, 255].repeat(8 * 8)),
            width: 8,
            height: 8,
            captured_at: Instant::now(),
        };
        let bias = PaletteBias {
            lightness_multiply: -0.85,
            lightness_add: -0.35,
            chroma_multiply: 2.5,
            chroma_add: 0.25,
            hue_add: 0.5,
            ..PaletteBias::default()
        };

        let encoded = encoder.encode(&raw, bias).unwrap();

        assert!(encoded.framebuffer.pixels.iter().all(|index| *index == 0));
    }

    #[test]
    fn indexed_raw_frame_bypasses_cpu_quantization() {
        let mut encoder =
            IndexedPixelEncoder::new(generated_test_palette(), PaletteBias::default(), None);
        let pixels = (0..64).map(|index| index as u8).collect::<Vec<_>>();
        let raw = RawFrame {
            pixels: RawFramePixels::Indexed(pixels.clone()),
            width: 8,
            height: 8,
            captured_at: Instant::now(),
        };

        let encoded = encoder.encode(&raw, PaletteBias::default()).unwrap();

        assert_eq!(encoded.framebuffer.pixels, pixels);
    }

    #[test]
    fn generated_test_palette_has_256_entries() {
        assert_eq!(generated_test_palette().len(), 256);
    }

    #[test]
    fn delayed_frame_becomes_available_without_new_publish() {
        let hub = PaletteFrameHub::new();
        let stats = SharedStats::new();
        let delay = Duration::from_millis(25);
        let packet = PaletteFramePacket {
            sequence: 1,
            framebuffer: Arc::new(Framebuffer::new(128, 128)),
            is_keyframe: true,
            frame_index: 0,
        };
        let encoded = encode_palette_batch_packets(None, &[packet]);
        hub.publish_encoded_batch(vec![0], 1, Instant::now(), encoded);

        let started = Instant::now();
        let batch = hub
            .wait_for_delayed_encoded_batch_after(0, delay, &stats, || true)
            .expect("frame should become eligible after its delay");

        assert_eq!(batch.sequence, 1);
        assert_eq!(batch.packets.len(), 1);
        assert!(started.elapsed() >= delay);
    }

    fn decode_delta_frame_for_test(previous: &Framebuffer, bytes: &[u8]) -> Framebuffer {
        let tiles_x = previous.width / TILE_SIZE;
        let tiles_y = previous.height / TILE_SIZE;
        let tile_count = tiles_x * tiles_y;
        let mask_len = tile_count.div_ceil(8);
        let mask = &bytes[..mask_len];
        let mut cursor = mask_len;
        let mut out = previous.clone();

        for tile_y in 0..tiles_y {
            for tile_x in 0..tiles_x {
                let tile_index = tile_y * tiles_x + tile_x;
                if mask[tile_index / 8] & (1 << (tile_index % 8)) == 0 {
                    continue;
                }

                let previous_tile = extract_tile(previous, tile_x, tile_y);
                let (tile, consumed) = decode_tile_for_test(&previous_tile, &bytes[cursor..]);
                cursor += consumed;
                write_tile_for_test(&mut out, tile_x, tile_y, &tile);
            }
        }

        out
    }

    fn decode_tile_for_test(previous: &[u8; 64], bytes: &[u8]) -> ([u8; 64], usize) {
        match bytes[0] {
            0 => {
                let mut tile = [0u8; 64];
                tile.copy_from_slice(&bytes[1..65]);
                (tile, 65)
            }
            1 => ([bytes[1]; 64], 2),
            2 => decode_rle_for_test(bytes),
            3 => decode_span_for_test(previous, bytes),
            4 => decode_xor_rle_for_test(previous, bytes),
            mode => panic!("unknown mode {mode}"),
        }
    }

    fn decode_rle_for_test(bytes: &[u8]) -> ([u8; 64], usize) {
        let run_count = bytes[1] as usize;
        let mut tile = [0u8; 64];
        let mut out_i = 0;
        let mut cursor = 2;
        for _ in 0..run_count {
            let color = bytes[cursor];
            let len = bytes[cursor + 1] as usize;
            cursor += 2;
            for _ in 0..len {
                tile[out_i] = color;
                out_i += 1;
            }
        }
        (tile, cursor)
    }

    fn decode_span_for_test(previous: &[u8; 64], bytes: &[u8]) -> ([u8; 64], usize) {
        let span_count = bytes[1] as usize;
        let mut tile = *previous;
        let mut tile_i = 0usize;
        let mut cursor = 2usize;
        for _ in 0..span_count {
            let skip = bytes[cursor] as usize;
            let len = bytes[cursor + 1] as usize;
            cursor += 2;
            tile_i += skip;
            tile[tile_i..tile_i + len].copy_from_slice(&bytes[cursor..cursor + len]);
            cursor += len;
            tile_i += len;
        }
        (tile, cursor)
    }

    fn decode_xor_rle_for_test(previous: &[u8; 64], bytes: &[u8]) -> ([u8; 64], usize) {
        let run_count = bytes[1] as usize;
        let mut tile = [0u8; 64];
        let mut out_i = 0;
        let mut cursor = 2;
        for _ in 0..run_count {
            let value = bytes[cursor];
            let len = bytes[cursor + 1] as usize;
            cursor += 2;
            for _ in 0..len {
                tile[out_i] = previous[out_i] ^ value;
                out_i += 1;
            }
        }
        (tile, cursor)
    }

    fn write_tile_for_test(frame: &mut Framebuffer, tile_x: usize, tile_y: usize, tile: &[u8; 64]) {
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                let dst_x = tile_x * TILE_SIZE + x;
                let dst_y = tile_y * TILE_SIZE + y;
                frame.pixels[dst_y * frame.width + dst_x] = tile[y * TILE_SIZE + x];
            }
        }
    }
}
