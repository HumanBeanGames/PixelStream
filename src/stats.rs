//! Shared runtime statistics for preview, custom-host, readback, and transport.

use bevy::prelude::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Component)]
pub(crate) struct StatsText;

/// Mutable counters and rolling timings sampled by the stats and status UIs.
#[derive(Default)]
pub(crate) struct StreamStats {
    pub(crate) frames_captured: u64,
    pub(crate) frames_read: u64,
    pub(crate) frames_encoded: u64,
    pub(crate) frames_dropped: u64,
    pub(crate) preview_frames_dropped: u64,
    pub(crate) custom_frames_dropped: u64,
    pub(crate) custom_queue_full_drops: u64,
    pub(crate) custom_frames_sent: u64,
    pub(crate) custom_bytes_sent: u64,
    pub(crate) custom_stage: &'static str,
    pub(crate) custom_last_error: String,
    pub(crate) custom_keyframes_sent: u64,
    pub(crate) custom_delta_frames_sent: u64,
    pub(crate) custom_raw_tiles_sent: u64,
    pub(crate) custom_solid_tiles_sent: u64,
    pub(crate) custom_rle_tiles_sent: u64,
    pub(crate) custom_span_tiles_sent: u64,
    pub(crate) custom_xor_tiles_sent: u64,
    pub(crate) custom_cached_tiles_sent: u64,
    pub(crate) custom_skipped_tiles: u64,
    pub(crate) custom_audio_packets_sent: u64,
    pub(crate) custom_audio_bytes_sent: u64,
    pub(crate) custom_recording_path: String,
    pub(crate) custom_readback_wait_last_ms: f64,
    pub(crate) custom_readback_wait_avg_ms: f64,
    pub(crate) custom_readback_cpu_last_ms: f64,
    pub(crate) custom_readback_cpu_avg_ms: f64,
    pub(crate) custom_encode_last_ms: f64,
    pub(crate) custom_encode_avg_ms: f64,
    pub(crate) custom_record_last_ms: f64,
    pub(crate) custom_record_avg_ms: f64,
    pub(crate) custom_publish_last_ms: f64,
    pub(crate) custom_publish_avg_ms: f64,
    pub(crate) custom_pipeline_last_ms: f64,
    pub(crate) custom_pipeline_avg_ms: f64,
    pub(crate) custom_actual_fps: f64,
    pub(crate) custom_frame_samples: VecDeque<Instant>,
    pub(crate) custom_app_fps: f64,
    pub(crate) custom_app_frame_samples: VecDeque<Instant>,
    pub(crate) custom_batch_size: usize,
    pub(crate) custom_batch_latency_ms: f64,
    pub(crate) custom_http_batch_last_frames: usize,
    pub(crate) custom_http_batch_avg_frames: f64,
    pub(crate) custom_pending_readbacks: usize,
    pub(crate) custom_batch_buffered_frames: usize,
    pub(crate) custom_sender_wait_timeouts: u64,
    pub(crate) custom_sender_wait_wakeups: u64,
    pub(crate) custom_audio_delay_ms: u32,
    pub(crate) stream_clients: u32,
    pub(crate) preview_requests: u64,
    pub(crate) latest_frame_bytes: usize,
}

/// Thread-safe stats handle shared between Bevy systems and HTTP/audio workers.
#[derive(Clone, Resource)]
pub(crate) struct SharedStats(pub(crate) Arc<Mutex<StreamStats>>);

impl SharedStats {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(StreamStats::default())))
    }

    pub(crate) fn with_mut(&self, update: impl FnOnce(&mut StreamStats)) {
        if let Ok(mut stats) = self.0.lock() {
            update(&mut stats);
        }
    }
}

impl StreamStats {
    fn update_timing(avg: &mut f64, last: &mut f64, sample_ms: f64) {
        *last = sample_ms;
        if *avg <= 0.0 {
            *avg = sample_ms;
        } else {
            *avg = *avg * 0.85 + sample_ms * 0.15;
        }
    }

    pub(crate) fn record_custom_readback_cpu(&mut self, sample_ms: f64) {
        Self::update_timing(
            &mut self.custom_readback_cpu_avg_ms,
            &mut self.custom_readback_cpu_last_ms,
            sample_ms,
        );
    }

    pub(crate) fn record_custom_encode(&mut self, sample_ms: f64) {
        Self::update_timing(
            &mut self.custom_encode_avg_ms,
            &mut self.custom_encode_last_ms,
            sample_ms,
        );
    }

    pub(crate) fn record_custom_record(&mut self, sample_ms: f64) {
        Self::update_timing(
            &mut self.custom_record_avg_ms,
            &mut self.custom_record_last_ms,
            sample_ms,
        );
    }

    pub(crate) fn record_custom_publish(&mut self, sample_ms: f64) {
        Self::update_timing(
            &mut self.custom_publish_avg_ms,
            &mut self.custom_publish_last_ms,
            sample_ms,
        );
    }

    pub(crate) fn record_custom_pipeline(&mut self, sample_ms: f64) {
        Self::update_timing(
            &mut self.custom_pipeline_avg_ms,
            &mut self.custom_pipeline_last_ms,
            sample_ms,
        );
    }

    pub(crate) fn reset_custom_session(&mut self) {
        self.frames_captured = 0;
        self.frames_read = 0;
        self.frames_encoded = 0;
        self.frames_dropped = 0;
        self.custom_frames_dropped = 0;
        self.custom_queue_full_drops = 0;
        self.custom_frames_sent = 0;
        self.custom_bytes_sent = 0;
        self.latest_frame_bytes = 0;
        self.custom_stage = "starting";
        self.custom_last_error.clear();
        self.custom_keyframes_sent = 0;
        self.custom_delta_frames_sent = 0;
        self.custom_raw_tiles_sent = 0;
        self.custom_solid_tiles_sent = 0;
        self.custom_rle_tiles_sent = 0;
        self.custom_span_tiles_sent = 0;
        self.custom_xor_tiles_sent = 0;
        self.custom_cached_tiles_sent = 0;
        self.custom_skipped_tiles = 0;
        self.custom_audio_packets_sent = 0;
        self.custom_audio_bytes_sent = 0;
        self.custom_readback_wait_last_ms = 0.0;
        self.custom_readback_wait_avg_ms = 0.0;
        self.custom_readback_cpu_last_ms = 0.0;
        self.custom_readback_cpu_avg_ms = 0.0;
        self.custom_encode_last_ms = 0.0;
        self.custom_encode_avg_ms = 0.0;
        self.custom_record_last_ms = 0.0;
        self.custom_record_avg_ms = 0.0;
        self.custom_publish_last_ms = 0.0;
        self.custom_publish_avg_ms = 0.0;
        self.custom_pipeline_last_ms = 0.0;
        self.custom_pipeline_avg_ms = 0.0;
        self.custom_actual_fps = 0.0;
        self.custom_frame_samples.clear();
        self.custom_app_fps = 0.0;
        self.custom_app_frame_samples.clear();
        self.custom_batch_size = 0;
        self.custom_batch_latency_ms = 0.0;
        self.custom_http_batch_last_frames = 0;
        self.custom_http_batch_avg_frames = 0.0;
        self.custom_pending_readbacks = 0;
        self.custom_batch_buffered_frames = 0;
        self.custom_sender_wait_timeouts = 0;
        self.custom_sender_wait_wakeups = 0;
        self.custom_audio_delay_ms = 0;
    }

    pub(crate) fn record_custom_frame_batch_sent(
        &mut self,
        frames: usize,
        frame_interval: Duration,
    ) {
        if frames == 0 {
            return;
        }

        let now = Instant::now();
        let batch_duration = frame_interval.mul_f64(frames.saturating_sub(1) as f64);
        let first_frame_time = now.checked_sub(batch_duration).unwrap_or(now);
        for index in 0..frames {
            self.custom_frame_samples
                .push_back(first_frame_time + frame_interval.mul_f64(index as f64));
        }
        self.refresh_custom_fps(now);
    }

    pub(crate) fn record_custom_app_frame(&mut self) {
        let now = Instant::now();
        self.custom_app_frame_samples.push_back(now);
        let window = Duration::from_secs(10);
        while self
            .custom_app_frame_samples
            .front()
            .is_some_and(|sample_time| now.duration_since(*sample_time) > window)
        {
            self.custom_app_frame_samples.pop_front();
        }

        if self.custom_app_frame_samples.len() < 2 {
            self.custom_app_fps = 0.0;
            return;
        }

        let oldest = self
            .custom_app_frame_samples
            .front()
            .copied()
            .expect("length checked");

        let elapsed = now.duration_since(oldest).as_secs_f64();
        self.custom_app_fps = if elapsed > 0.0 {
            (self.custom_app_frame_samples.len() - 1) as f64 / elapsed
        } else {
            0.0
        };
    }

    pub(crate) fn record_custom_http_batch(&mut self, frames: usize) {
        self.custom_http_batch_last_frames = frames;
        if self.custom_http_batch_avg_frames <= 0.0 {
            self.custom_http_batch_avg_frames = frames as f64;
        } else {
            self.custom_http_batch_avg_frames =
                self.custom_http_batch_avg_frames * 0.85 + frames as f64 * 0.15;
        }
    }

    pub(crate) fn refresh_custom_fps(&mut self, now: Instant) {
        let window = Duration::from_secs(10);
        while self
            .custom_frame_samples
            .front()
            .is_some_and(|sample_time| now.duration_since(*sample_time) > window)
        {
            self.custom_frame_samples.pop_front();
        }

        if self.custom_frame_samples.len() < 2 {
            self.custom_actual_fps = 0.0;
            return;
        }

        let oldest = self
            .custom_frame_samples
            .front()
            .copied()
            .expect("length checked");

        let elapsed = now.duration_since(oldest).as_secs_f64();
        if elapsed <= 0.0 {
            self.custom_actual_fps = 0.0;
            return;
        }

        let frame_count = self.custom_frame_samples.len() - 1;
        self.custom_actual_fps = frame_count as f64 / elapsed;
    }
}
