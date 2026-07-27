//! Stream audio decoding, buffering, delay, and transport.
//!
//! Downstream games submit whole clips or one-shot sounds. PixelStream converts
//! them into a compact PCM packet stream that the browser schedules to align
//! with delayed video playback.

use crate::constants::{
    CUSTOM_AUDIO_SAMPLE_RATE, STREAM_AUDIO_BUFFER_SECONDS, STREAM_AUDIO_CHANNELS,
    STREAM_AUDIO_MAX_MIX_FRAMES_PER_UPDATE, STREAM_AUDIO_SAMPLE_RATE,
};
use bevy::prelude::*;
use ffmpeg::{
    ChannelLayout, codec, format, frame, media,
    software::resampling::context::Context as ResampleContext,
    util::format::{Sample, sample::Type as SampleType},
};
use ffmpeg_next as ffmpeg;
use std::{
    f32::consts::PI,
    fs,
    path::Path,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

const CUSTOM_AUDIO_PACKET_RATE: u32 = 50;
const CUSTOM_AUDIO_LOWPASS_CUTOFF_HZ: f32 = 3_200.0;

#[derive(Asset, TypePath, Clone)]
pub struct StreamAudioClip {
    samples: Arc<Vec<f32>>,
    channels: usize,
    sample_rate: u32,
}

#[derive(Event, Message, Clone)]
pub struct PlayStreamSound {
    pub clip: Handle<StreamAudioClip>,
    pub volume: f32,
    pub repeat: bool,
}

impl PlayStreamSound {
    pub fn once(clip: Handle<StreamAudioClip>) -> Self {
        Self {
            clip,
            volume: 1.0,
            repeat: false,
        }
    }

    pub fn looping(clip: Handle<StreamAudioClip>) -> Self {
        Self {
            clip,
            volume: 1.0,
            repeat: true,
        }
    }

    pub fn with_volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }
}

#[derive(Resource)]
pub(crate) struct StreamAudioMixer {
    voices: Vec<StreamVoice>,
    pending_frames: f64,
}

impl Default for StreamAudioMixer {
    fn default() -> Self {
        Self {
            voices: Vec::new(),
            pending_frames: 0.0,
        }
    }
}

struct StreamVoice {
    clip: Handle<StreamAudioClip>,
    cursor_frames: usize,
    volume: f32,
    repeat: bool,
}

pub(crate) fn collect_stream_audio_events(
    mut events: MessageReader<PlayStreamSound>,
    mut mixer: ResMut<StreamAudioMixer>,
) {
    for event in events.read() {
        mixer.voices.push(StreamVoice {
            clip: event.clip.clone(),
            cursor_frames: 0,
            volume: event.volume.max(0.0),
            repeat: event.repeat,
        });
    }
}

pub(crate) fn mix_stream_audio(
    time: Res<Time>,
    clips: Res<Assets<StreamAudioClip>>,
    mut mixer: ResMut<StreamAudioMixer>,
    audio_target: Res<DirectStreamAudioTarget>,
) {
    if mixer.voices.is_empty() {
        mixer.pending_frames = 0.0;
        return;
    }

    mixer.pending_frames += time.delta_secs_f64() * STREAM_AUDIO_SAMPLE_RATE as f64;
    let frames_to_mix = mixer
        .pending_frames
        .floor()
        .min(STREAM_AUDIO_MAX_MIX_FRAMES_PER_UPDATE as f64) as usize;
    mixer.pending_frames -= frames_to_mix as f64;
    if frames_to_mix == 0 {
        return;
    }

    let mut mixed = vec![0.0; frames_to_mix * STREAM_AUDIO_CHANNELS];
    let mut active_voices = Vec::with_capacity(mixer.voices.len());

    for mut voice in mixer.voices.drain(..) {
        let Some(clip) = clips.get(&voice.clip) else {
            active_voices.push(voice);
            continue;
        };

        let total_frames = clip.stream_frame_count();
        if total_frames == 0 {
            continue;
        }

        for frame in 0..frames_to_mix {
            if voice.cursor_frames >= total_frames {
                if voice.repeat {
                    voice.cursor_frames = 0;
                } else {
                    break;
                }
            }

            let [left, right] = clip.stereo_frame(voice.cursor_frames);
            mixed[frame * 2] += left * voice.volume;
            mixed[frame * 2 + 1] += right * voice.volume;
            voice.cursor_frames += 1;
        }

        if voice.repeat || voice.cursor_frames < total_frames {
            active_voices.push(voice);
        }
    }

    mixer.voices = active_voices;
    audio_target.push_stereo_f32(&mixed);
}

impl StreamAudioClip {
    pub fn from_interleaved_f32(samples: Vec<f32>, channels: usize, sample_rate: u32) -> Self {
        Self {
            samples: Arc::new(samples.into_iter().map(normalize_stream_sample).collect()),
            channels: channels.max(1),
            sample_rate,
        }
    }

    pub fn from_mono_f32(samples: Vec<f32>, sample_rate: u32) -> Self {
        Self::from_interleaved_f32(samples, 1, sample_rate)
    }

    pub fn from_wav_file(path: impl AsRef<Path>) -> Result<Self, String> {
        if let Ok(clip) = decode_audio_file_with_ffmpeg(path.as_ref()) {
            return Ok(clip);
        }

        let bytes = fs::read(path.as_ref())
            .map_err(|err| format!("could not read {}: {err}", path.as_ref().display()))?;
        decode_wav_clip(&bytes)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels
    }

    fn stream_frame_count(&self) -> usize {
        self.frame_count() * STREAM_AUDIO_SAMPLE_RATE as usize / self.sample_rate.max(1) as usize
    }

    fn stereo_frame(&self, frame: usize) -> [f32; 2] {
        let source_position =
            frame as f32 * self.sample_rate as f32 / STREAM_AUDIO_SAMPLE_RATE as f32;
        let source_frame = source_position.floor() as usize;
        let next_frame = (source_frame + 1).min(self.frame_count().saturating_sub(1));
        let t = source_position.fract();

        if source_frame >= self.frame_count() {
            return [0.0, 0.0];
        }

        let current = self.source_stereo_frame(source_frame);
        let next = self.source_stereo_frame(next_frame);
        [
            current[0] + (next[0] - current[0]) * t,
            current[1] + (next[1] - current[1]) * t,
        ]
    }

    fn source_stereo_frame(&self, frame: usize) -> [f32; 2] {
        let base = frame.saturating_mul(self.channels);
        if base >= self.samples.len() {
            return [0.0, 0.0];
        }

        if self.channels == 1 {
            let sample = self.samples[base];
            [sample, sample]
        } else {
            [
                self.samples[base],
                self.samples.get(base + 1).copied().unwrap_or(0.0),
            ]
        }
    }
}

fn decode_audio_file_with_ffmpeg(path: &Path) -> Result<StreamAudioClip, String> {
    ffmpeg::init().map_err(|err| err.to_string())?;

    let mut input = format::input(path).map_err(|err| err.to_string())?;
    let stream = input
        .streams()
        .best(media::Type::Audio)
        .ok_or_else(|| "file does not contain an audio stream".to_owned())?;
    let stream_index = stream.index();
    let context = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|err| err.to_string())?;
    let mut decoder = context.decoder().audio().map_err(|err| err.to_string())?;
    decoder
        .set_parameters(stream.parameters())
        .map_err(|err| err.to_string())?;

    let source_layout = if decoder.channel_layout().is_empty() {
        ChannelLayout::default(decoder.channels() as i32)
    } else {
        decoder.channel_layout()
    };
    let sample_rate = decoder.rate();
    let mut resampler = ResampleContext::get(
        decoder.format(),
        source_layout,
        sample_rate,
        Sample::F32(SampleType::Planar),
        ChannelLayout::STEREO,
        sample_rate,
    )
    .map_err(|err| err.to_string())?;

    let mut samples = Vec::new();
    for (stream, packet) in input.packets() {
        if stream.index() == stream_index {
            decoder
                .send_packet(&packet)
                .map_err(|err| err.to_string())?;
            receive_resampled_audio(&mut decoder, &mut resampler, &mut samples)?;
        }
    }

    decoder.send_eof().map_err(|err| err.to_string())?;
    receive_resampled_audio(&mut decoder, &mut resampler, &mut samples)?;

    if samples.is_empty() {
        return Err("decoded audio stream had no samples".to_owned());
    }

    Ok(StreamAudioClip::from_interleaved_f32(
        samples,
        STREAM_AUDIO_CHANNELS,
        sample_rate,
    ))
}

fn receive_resampled_audio(
    decoder: &mut codec::decoder::Audio,
    resampler: &mut ResampleContext,
    samples: &mut Vec<f32>,
) -> Result<(), String> {
    let mut decoded = frame::Audio::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let mut converted = frame::Audio::empty();
        resampler
            .run(&decoded, &mut converted)
            .map_err(|err| err.to_string())?;
        append_planar_stereo_f32(&converted, samples)?;
    }

    Ok(())
}

fn append_planar_stereo_f32(frame: &frame::Audio, samples: &mut Vec<f32>) -> Result<(), String> {
    if frame.format() != Sample::F32(SampleType::Planar) || frame.planes() < 2 {
        return Err("resampler did not produce planar stereo f32 audio".to_owned());
    }

    let left = frame.plane::<f32>(0);
    let right = frame.plane::<f32>(1);
    for index in 0..frame.samples().min(left.len()).min(right.len()) {
        samples.push(left[index]);
        samples.push(right[index]);
    }

    Ok(())
}

fn decode_wav_clip(bytes: &[u8]) -> Result<StreamAudioClip, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_owned());
    }

    let mut cursor = 12;
    let mut format = None;
    let mut data = None;

    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_size = read_u32_le(bytes, cursor + 4)? as usize;
        let chunk_start = cursor + 8;
        let chunk_end = chunk_start
            .checked_add(chunk_size)
            .ok_or_else(|| "WAV chunk size overflow".to_owned())?;
        if chunk_end > bytes.len() {
            return Err("WAV chunk extends past end of file".to_owned());
        }

        match chunk_id {
            b"fmt " => {
                if chunk_size < 16 {
                    return Err("WAV fmt chunk is too small".to_owned());
                }
                format = Some(WavFormat {
                    audio_format: read_u16_le(bytes, chunk_start)?,
                    channels: read_u16_le(bytes, chunk_start + 2)? as usize,
                    sample_rate: read_u32_le(bytes, chunk_start + 4)?,
                    bits_per_sample: read_u16_le(bytes, chunk_start + 14)?,
                });
            }
            b"data" => data = Some(&bytes[chunk_start..chunk_end]),
            _ => {}
        }

        cursor = chunk_end + (chunk_size % 2);
    }

    let format = format.ok_or_else(|| "WAV file is missing fmt chunk".to_owned())?;
    let data = data.ok_or_else(|| "WAV file is missing data chunk".to_owned())?;
    if format.channels == 0 {
        return Err("WAV file has zero channels".to_owned());
    }

    let samples = decode_wav_samples(data, format)?;
    Ok(StreamAudioClip::from_interleaved_f32(
        samples,
        format.channels,
        format.sample_rate,
    ))
}

#[derive(Clone, Copy)]
struct WavFormat {
    audio_format: u16,
    channels: usize,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn decode_wav_samples(data: &[u8], format: WavFormat) -> Result<Vec<f32>, String> {
    let bytes_per_sample = (format.bits_per_sample as usize)
        .checked_div(8)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "WAV bits per sample must be byte-aligned".to_owned())?;
    if !data.len().is_multiple_of(bytes_per_sample) {
        return Err("WAV data chunk is not sample-aligned".to_owned());
    }

    data.chunks_exact(bytes_per_sample)
        .map(|sample| decode_wav_sample(sample, format))
        .collect()
}

fn decode_wav_sample(sample: &[u8], format: WavFormat) -> Result<f32, String> {
    match (format.audio_format, format.bits_per_sample) {
        (1, 8) => Ok((sample[0] as f32 - 128.0) / 128.0),
        (1, 16) => Ok(i16::from_le_bytes([sample[0], sample[1]]) as f32 / 32768.0),
        (1, 24) => {
            let sign = if sample[2] & 0x80 == 0 { 0x00 } else { 0xff };
            let value = i32::from_le_bytes([sample[0], sample[1], sample[2], sign]);
            Ok(value as f32 / 8_388_608.0)
        }
        (1, 32) => Ok(
            i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) as f32
                / 2_147_483_648.0,
        ),
        (3, 32) => Ok(f32::from_le_bytes([
            sample[0], sample[1], sample[2], sample[3],
        ])),
        _ => Err(format!(
            "unsupported WAV format: format {}, {} bits",
            format.audio_format, format.bits_per_sample
        )),
    }
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "unexpected end of WAV data".to_owned())?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "unexpected end of WAV data".to_owned())?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

#[derive(Clone, Resource)]
pub struct DirectStreamAudioTarget {
    inner: Arc<Mutex<DirectStreamAudioBuffer>>,
}

struct DirectStreamAudioBuffer {
    samples: Vec<f32>,
    read_index: usize,
    len: usize,
}

impl DirectStreamAudioTarget {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DirectStreamAudioBuffer {
                samples: vec![
                    0.0;
                    STREAM_AUDIO_SAMPLE_RATE as usize
                        * STREAM_AUDIO_CHANNELS
                        * STREAM_AUDIO_BUFFER_SECONDS
                ],
                read_index: 0,
                len: 0,
            })),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        STREAM_AUDIO_SAMPLE_RATE
    }

    pub fn channels(&self) -> usize {
        STREAM_AUDIO_CHANNELS
    }

    pub fn push_stereo_f32(&self, interleaved_stereo: &[f32]) {
        if let Ok(mut buffer) = self.inner.lock() {
            for sample in interleaved_stereo {
                buffer.push(normalize_stream_sample(*sample));
            }
        }
    }

    pub fn push_mono_f32(&self, mono: &[f32]) {
        if let Ok(mut buffer) = self.inner.lock() {
            for sample in mono {
                let sample = normalize_stream_sample(*sample);
                for channel_sample in [sample, sample] {
                    buffer.push(channel_sample);
                }
            }
        }
    }

    pub(crate) fn take_delayed_stereo_f32_into(
        &self,
        frames: usize,
        delay_frames: usize,
        output: &mut Vec<f32>,
    ) -> bool {
        let sample_count = frames * STREAM_AUDIO_CHANNELS;
        let delay_sample_count = delay_frames * STREAM_AUDIO_CHANNELS;
        if let Ok(mut buffer) = self.inner.lock() {
            if buffer.len < sample_count + delay_sample_count {
                return false;
            }

            output.clear();
            if output.capacity() < sample_count {
                output.reserve(sample_count - output.capacity());
            }
            for _ in 0..sample_count {
                output.push(buffer.pop().unwrap_or(0.0));
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut buffer) = self.inner.lock() {
            buffer.clear();
        }
    }
}

impl DirectStreamAudioBuffer {
    fn push(&mut self, sample: f32) {
        let capacity = self.samples.len();
        if capacity == 0 {
            return;
        }

        let write_index = (self.read_index + self.len) % capacity;
        self.samples[write_index] = sample;
        if self.len == capacity {
            self.read_index = (self.read_index + 1) % capacity;
        } else {
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<f32> {
        if self.len == 0 {
            return None;
        }

        let sample = self.samples[self.read_index];
        self.read_index = (self.read_index + 1) % self.samples.len();
        self.len -= 1;
        Some(sample)
    }

    fn clear(&mut self) {
        self.read_index = 0;
        self.len = 0;
    }
}

pub(crate) fn normalize_stream_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

impl Default for DirectStreamAudioTarget {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Resource)]
pub(crate) struct CustomAudioPacketHub {
    inner: Arc<(Mutex<LatestAudioPacket>, Condvar)>,
}

#[derive(Default)]
struct LatestAudioPacket {
    sequence: u64,
    packet: Option<Arc<Vec<u8>>>,
}

impl CustomAudioPacketHub {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(LatestAudioPacket::default()), Condvar::new())),
        }
    }

    fn publish(&self, packet: Vec<u8>) {
        let (lock, ready) = &*self.inner;
        if let Ok(mut latest) = lock.lock() {
            latest.sequence += 1;
            latest.packet = Some(Arc::new(packet));
            ready.notify_all();
        }
    }

    pub(crate) fn wait_for_packet_after_timeout(
        &self,
        last_sequence: u64,
        timeout: Duration,
    ) -> Option<(u64, Arc<Vec<u8>>)> {
        let (lock, ready) = &*self.inner;
        let mut latest = lock.lock().ok()?;

        while latest.sequence <= last_sequence || latest.packet.is_none() {
            let (next_latest, wait_result) = ready.wait_timeout(latest, timeout).ok()?;
            latest = next_latest;
            if wait_result.timed_out() {
                return None;
            }
        }

        Some((latest.sequence, latest.packet.as_ref()?.clone()))
    }
}

pub(crate) fn start_custom_audio_packet_pump(
    audio: DirectStreamAudioTarget,
    hub: CustomAudioPacketHub,
    stats: crate::stats::SharedStats,
    active: crate::stream_control::CustomStreamState,
) {
    thread::spawn(move || {
        let frames_per_packet = (STREAM_AUDIO_SAMPLE_RATE / CUSTOM_AUDIO_PACKET_RATE) as usize;
        let packet_duration = Duration::from_millis(1000 / CUSTOM_AUDIO_PACKET_RATE as u64);
        let mut next_tick = Instant::now();
        let mut started = false;
        let mut last_left = 0.0f32;
        let mut last_right = 0.0f32;
        let mut synthesized_previous_packet = false;
        let mut downsample_state = AudioDownsampleState::new();
        let mut samples = Vec::with_capacity(frames_per_packet * STREAM_AUDIO_CHANNELS);
        let mut packet = Vec::with_capacity(frames_per_packet);

        loop {
            if !active.is_active() {
                started = false;
                last_left = 0.0;
                last_right = 0.0;
                synthesized_previous_packet = false;
                downsample_state.reset();
                thread::sleep(Duration::from_millis(20));
                next_tick = Instant::now();
                continue;
            }

            let delay_frames = (STREAM_AUDIO_SAMPLE_RATE as u128 * active.audio_delay_ms() as u128
                / 1000) as usize;
            if audio.take_delayed_stereo_f32_into(frames_per_packet, delay_frames, &mut samples) {
                if synthesized_previous_packet {
                    smooth_packet_start(&mut samples, last_left, last_right);
                }
                if let Some([left, right]) = last_stereo_frame(&samples) {
                    last_left = left;
                    last_right = right;
                }
                started = true;
                synthesized_previous_packet = false;
            } else if started {
                synthesized_previous_packet = true;
                fade_to_silence_packet_into(
                    frames_per_packet,
                    &mut last_left,
                    &mut last_right,
                    &mut samples,
                );
            } else {
                thread::sleep(Duration::from_millis(5));
                continue;
            }

            pcm_mulaw_8khz_mono_packet_into(&samples, &mut downsample_state, &mut packet);
            let packet_len = packet.len() as u64;
            hub.publish(packet.clone());
            stats.with_mut(|stats| {
                stats.custom_audio_packets_sent += 1;
                stats.custom_audio_bytes_sent += packet_len;
            });

            next_tick += packet_duration;
            let now = Instant::now();
            if next_tick > now {
                thread::sleep(next_tick - now);
            } else {
                next_tick = now;
            }
        }
    });
}

struct AudioDownsampleState {
    lowpass_sample: f32,
    lowpass_alpha: f32,
}

impl AudioDownsampleState {
    fn new() -> Self {
        Self {
            lowpass_sample: 0.0,
            lowpass_alpha: 1.0
                - (-2.0 * PI * CUSTOM_AUDIO_LOWPASS_CUTOFF_HZ / STREAM_AUDIO_SAMPLE_RATE as f32)
                    .exp(),
        }
    }

    fn reset(&mut self) {
        self.lowpass_sample = 0.0;
    }

    fn lowpass(&mut self, sample: f32) -> f32 {
        self.lowpass_sample += self.lowpass_alpha * (sample - self.lowpass_sample);
        self.lowpass_sample
    }
}

fn pcm_mulaw_8khz_mono_packet_into(
    samples: &[f32],
    state: &mut AudioDownsampleState,
    packet: &mut Vec<u8>,
) {
    let downsample_factor = (STREAM_AUDIO_SAMPLE_RATE / CUSTOM_AUDIO_SAMPLE_RATE).max(1) as usize;
    let source_frames = samples.len() / STREAM_AUDIO_CHANNELS;
    let output_frames = source_frames / downsample_factor;
    packet.clear();
    if packet.capacity() < output_frames {
        packet.reserve(output_frames - packet.capacity());
    }

    for output_frame in 0..output_frames {
        let mut sum = 0.0;
        let mut count = 0;
        for source_offset in 0..downsample_factor {
            let source_frame = output_frame * downsample_factor + source_offset;
            let base = source_frame * STREAM_AUDIO_CHANNELS;
            if base + 1 < samples.len() {
                let mono = (samples[base] + samples[base + 1]) * 0.5;
                sum += state.lowpass(mono);
                count += 1;
            }
        }

        let mono = if count == 0 { 0.0 } else { sum / count as f32 };
        packet.push(linear_to_mulaw(mono));
    }
}

fn linear_to_mulaw(sample: f32) -> u8 {
    const MU: f32 = 255.0;
    let sample = sample.clamp(-1.0, 1.0);
    let sign_bit = if sample < 0.0 { 0x80 } else { 0x00 };
    let magnitude = sample.abs();
    let encoded = ((magnitude.mul_add(MU, 1.0).ln() / (1.0 + MU).ln()) * 127.0)
        .round()
        .clamp(0.0, 127.0) as u8;
    sign_bit | encoded
}

fn smooth_packet_start(samples: &mut [f32], last_left: f32, last_right: f32) {
    let fade_frames = 128.min(samples.len() / STREAM_AUDIO_CHANNELS);
    for frame in 0..fade_frames {
        let t = frame as f32 / fade_frames as f32;
        let left_index = frame * STREAM_AUDIO_CHANNELS;
        let right_index = left_index + 1;
        samples[left_index] = last_left * (1.0 - t) + samples[left_index] * t;
        samples[right_index] = last_right * (1.0 - t) + samples[right_index] * t;
    }
}

fn fade_to_silence_packet_into(
    frames: usize,
    last_left: &mut f32,
    last_right: &mut f32,
    samples: &mut Vec<f32>,
) {
    samples.clear();
    samples.resize(frames * STREAM_AUDIO_CHANNELS, 0.0);
    for frame in 0..frames {
        let t = 1.0 - frame as f32 / frames as f32;
        samples[frame * STREAM_AUDIO_CHANNELS] = *last_left * t;
        samples[frame * STREAM_AUDIO_CHANNELS + 1] = *last_right * t;
    }
    *last_left = 0.0;
    *last_right = 0.0;
}

fn last_stereo_frame(samples: &[f32]) -> Option<[f32; 2]> {
    let frame_start = samples.len().checked_sub(STREAM_AUDIO_CHANNELS)?;
    Some([samples[frame_start], samples[frame_start + 1]])
}
