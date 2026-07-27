//! Local custom-host HTTP server and generated browser player.
//!
//! The server is intentionally small and loopback-first. It serves frame/audio
//! streams, chat, panels, overlays, pointer events, and status for the static
//! browser page generated below.

use crate::{
    audio::CustomAudioPacketHub,
    chat::{LocalChatHub, LocalChatSubmitResult},
    config::AppConfig,
    constants::{
        AUDIO_STREAM_PATH, CUSTOM_AUDIO_CHANNELS, CUSTOM_AUDIO_SAMPLE_RATE, CUSTOM_OVERLAYS_PATH,
        CUSTOM_PANEL_ACTION_PATH, CUSTOM_PANELS_PATH, LOCAL_CHAT_FEED_PATH, LOCAL_CHAT_PATH,
        PALETTE_STREAM_PATH, STREAM_CLICK_PATH, STREAM_HEIGHT, STREAM_PATH, STREAM_STATUS_PATH,
        STREAM_WIDTH, WEB_ADDR,
    },
    custom_host::{
        CustomHostBranding, CustomHostChatPanelHub, CustomHostLayout, CustomHostOverlayHub,
        CustomHostPanelAction, CustomHostPanelActionHub, CustomHostPanelElement,
        CustomHostPanelElementStyle, CustomHostPanelHub, CustomHostPanelSize, CustomHostPanelStyle,
        OverlayElementKind, OverlayElementStyle, PanelWhiteSpace, StreamPointerClick,
        StreamPointerClickHub,
    },
    frames::EncodedFrameHub,
    palette::PaletteFrameHub,
    stats::SharedStats,
    stream_control::CustomStreamState,
};
use bevy::prelude::*;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CUSTOM_STREAM_SERVER_DELAY: Duration = Duration::ZERO;
const CUSTOM_STREAM_PLAYBACK_BUFFER_SECONDS: f64 = 1.0;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024;
const PIXEL_STREAM_SESSION_HEADER: &str = "X-PixelStream-Session";

#[derive(Clone)]
pub(crate) enum LocalStreamSource {
    Mjpeg {
        frames: EncodedFrameHub,
        audio: CustomAudioPacketHub,
        active: CustomStreamState,
    },
    Palette {
        frames: PaletteFrameHub,
        audio: CustomAudioPacketHub,
        chat: LocalChatHub,
        chat_panel: CustomHostChatPanelHub,
        panels: CustomHostPanelHub,
        panel_actions: CustomHostPanelActionHub,
        overlays: CustomHostOverlayHub,
        clicks: StreamPointerClickHub,
        active: CustomStreamState,
    },
}

pub(crate) fn start_local_web_server_from_resources(
    mut started: Local<bool>,
    config: Res<AppConfig>,
    frame_hub: Res<EncodedFrameHub>,
    palette_frame_hub: Res<PaletteFrameHub>,
    audio: Res<CustomAudioPacketHub>,
    chat: Res<LocalChatHub>,
    chat_panel: Res<CustomHostChatPanelHub>,
    panels: Res<CustomHostPanelHub>,
    panel_actions: Res<CustomHostPanelActionHub>,
    overlays: Res<CustomHostOverlayHub>,
    clicks: Res<StreamPointerClickHub>,
    active: Res<CustomStreamState>,
    stats: Res<SharedStats>,
    branding: Res<CustomHostBranding>,
    layout: Res<CustomHostLayout>,
) {
    if *started {
        return;
    }
    *started = true;

    let source = if config.custom_host {
        LocalStreamSource::Palette {
            frames: palette_frame_hub.clone(),
            audio: audio.clone(),
            chat: chat.clone(),
            chat_panel: chat_panel.clone(),
            panels: panels.clone(),
            panel_actions: panel_actions.clone(),
            overlays: overlays.clone(),
            clicks: clicks.clone(),
            active: active.clone(),
        }
    } else {
        LocalStreamSource::Mjpeg {
            frames: frame_hub.clone(),
            audio: audio.clone(),
            active: active.clone(),
        }
    };
    start_local_web_server(source, stats.clone(), branding.clone(), layout.clone());
}

pub(crate) fn start_local_web_server(
    source: LocalStreamSource,
    stats: SharedStats,
    branding: CustomHostBranding,
    layout: CustomHostLayout,
) {
    thread::spawn(move || {
        let listener = match TcpListener::bind(WEB_ADDR) {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("Could not bind local stream page at http://{WEB_ADDR}: {err}");
                return;
            }
        };

        eprintln!("Local stream page: http://{WEB_ADDR}");

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let source = source.clone();
                    let stats = stats.clone();
                    let branding = branding.clone();
                    let layout = layout.clone();
                    thread::spawn(move || {
                        handle_web_request(stream, source, stats, branding, layout)
                    });
                }
                Err(err) => eprintln!("Local web server connection failed: {err}"),
            }
        }
    });
}

fn handle_web_request(
    mut stream: TcpStream,
    source: LocalStreamSource,
    stats: SharedStats,
    branding: CustomHostBranding,
    layout: CustomHostLayout,
) {
    let peer_addr = stream.peer_addr().ok();
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            serve_http_error(stream, error);
            return;
        }
    };

    if request.method.eq_ignore_ascii_case("OPTIONS") {
        serve_options(stream, &request);
        return;
    }

    if request.path == STREAM_PATH {
        if let LocalStreamSource::Mjpeg { frames, .. } = source {
            stream_mjpeg(stream, &request, frames, stats);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == PALETTE_STREAM_PATH {
        if let LocalStreamSource::Palette { frames, active, .. } = source {
            stream_palette(stream, &request, frames, stats, active);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == AUDIO_STREAM_PATH {
        match source {
            LocalStreamSource::Mjpeg { audio, active, .. }
            | LocalStreamSource::Palette { audio, active, .. } => {
                stream_pcm_audio(stream, &request, audio, stats, active);
            }
        }
    } else if request.path == LOCAL_CHAT_FEED_PATH {
        if let LocalStreamSource::Palette { chat, .. } = source {
            serve_local_chat_feed(stream, &request, peer_addr, chat);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == LOCAL_CHAT_PATH {
        if let LocalStreamSource::Palette { chat, active, .. } = source {
            submit_local_chat(stream, &request, peer_addr, chat, active);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == CUSTOM_PANELS_PATH {
        if let LocalStreamSource::Palette { panels, chat, .. } = source {
            serve_custom_panels(stream, &request, peer_addr, panels, chat);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == CUSTOM_PANEL_ACTION_PATH {
        if let LocalStreamSource::Palette {
            chat,
            panel_actions,
            active,
            ..
        } = source
        {
            submit_custom_panel_action(stream, &request, peer_addr, chat, panel_actions, active);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == CUSTOM_OVERLAYS_PATH {
        if let LocalStreamSource::Palette { overlays, chat, .. } = source {
            serve_custom_overlays(stream, &request, peer_addr, overlays, chat);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == STREAM_CLICK_PATH {
        if let LocalStreamSource::Palette {
            chat,
            clicks,
            active,
            ..
        } = source
        {
            submit_stream_click(stream, &request, peer_addr, chat, clicks, active);
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == STREAM_STATUS_PATH {
        if let LocalStreamSource::Palette {
            frames,
            active,
            chat_panel,
            ..
        } = source
        {
            serve_stream_status(
                stream, &request, frames, active, stats, &branding, &layout, chat_panel,
            );
        } else {
            serve_not_found(stream, &request);
        }
    } else if request.path == "/" || request.path == "/index.html" {
        stats.with_mut(|stats| stats.preview_requests += 1);
        serve_preview_page(stream, &request, &source, &branding, &layout);
    } else {
        serve_not_found(stream, &request);
    }
}

#[derive(Clone, Debug)]
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
enum HttpRequestError {
    BadRequest,
    PayloadTooLarge,
    MethodNotAllowed,
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpRequestError> {
    let mut request = Vec::with_capacity(4096);
    let mut buffer = [0; 4096];
    let mut header_end = None;
    let mut content_length = 0usize;

    while let Ok(bytes_read) = stream.read(&mut buffer) {
        if bytes_read == 0 {
            break;
        }

        request.extend_from_slice(&buffer[..bytes_read]);
        if header_end.is_none() {
            header_end = find_header_end(&request);
            if let Some(end) = header_end {
                content_length = parse_content_length(&request[..end]);
                if end > MAX_REQUEST_HEADER_BYTES || content_length > MAX_REQUEST_BODY_BYTES {
                    return Err(HttpRequestError::PayloadTooLarge);
                }
            }
        }

        if let Some(end) = header_end
            && request.len() >= end + content_length
        {
            break;
        }

        if request.len() > MAX_REQUEST_HEADER_BYTES + MAX_REQUEST_BODY_BYTES {
            return Err(HttpRequestError::PayloadTooLarge);
        }
    }

    parse_http_request(&request)
}

fn parse_http_request(request: &[u8]) -> Result<HttpRequest, HttpRequestError> {
    let header_end = find_header_end(request).ok_or(HttpRequestError::BadRequest)?;
    let headers =
        std::str::from_utf8(&request[..header_end]).map_err(|_| HttpRequestError::BadRequest)?;
    let mut lines = headers.lines();
    let request_line = lines.next().ok_or(HttpRequestError::BadRequest)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or(HttpRequestError::BadRequest)?;
    let target = parts.next().ok_or(HttpRequestError::BadRequest)?;
    let version = parts.next().ok_or(HttpRequestError::BadRequest)?;
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(HttpRequestError::BadRequest);
    }
    if !matches!(method, "GET" | "POST" | "OPTIONS") {
        return Err(HttpRequestError::MethodNotAllowed);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if !path.starts_with('/') || path.contains("..") || path.contains('\\') {
        return Err(HttpRequestError::BadRequest);
    }

    let mut parsed_headers = BTreeMap::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(HttpRequestError::BadRequest);
        };
        parsed_headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    let content_length = parse_content_length(&request[..header_end]);
    let body_start = header_end;
    let body_end = body_start
        .checked_add(content_length)
        .ok_or(HttpRequestError::PayloadTooLarge)?;
    if body_end > request.len() {
        return Err(HttpRequestError::BadRequest);
    }
    Ok(HttpRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        headers: parsed_headers,
        body: request[body_start..body_end].to_vec(),
    })
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn parse_content_length(headers: &[u8]) -> usize {
    let headers = String::from_utf8_lossy(headers);
    headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn serve_preview_page(
    mut stream: TcpStream,
    request: &HttpRequest,
    source: &LocalStreamSource,
    branding: &CustomHostBranding,
    layout: &CustomHostLayout,
) {
    let body = match source {
        LocalStreamSource::Mjpeg { .. } => mjpeg_stream_page_html(),
        LocalStreamSource::Palette { .. } => palette_stream_page_html(branding, layout),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );

    let _ = stream.write_all(response.as_bytes());
}

fn serve_not_found(mut stream: TcpStream, request: &HttpRequest) {
    let body = "Not found";
    let response = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_stream_status(
    mut stream: TcpStream,
    request: &HttpRequest,
    frame_hub: PaletteFrameHub,
    active: CustomStreamState,
    stats: SharedStats,
    branding: &CustomHostBranding,
    layout: &CustomHostLayout,
    chat_panel: CustomHostChatPanelHub,
) {
    let stats_snapshot = stats.0.lock().ok();
    let (
        batch_duration_ms,
        readback_latency_ms,
        http_batch_latency_ms,
        video_latency_ms_estimate,
        audio_delay_ms,
    ) = if let Some(stats) = stats_snapshot.as_deref() {
        let batch_duration_ms = if active.fps() > 0 && stats.custom_http_batch_last_frames > 0 {
            stats.custom_http_batch_last_frames.saturating_sub(1) as f64 * 1000.0
                / active.fps() as f64
        } else {
            0.0
        };
        let readback_latency_ms =
            stats.custom_readback_wait_avg_ms + stats.custom_readback_cpu_avg_ms;
        let http_batch_latency_ms = stats.custom_batch_latency_ms;
        (
            batch_duration_ms,
            readback_latency_ms,
            http_batch_latency_ms,
            batch_duration_ms + readback_latency_ms + http_batch_latency_ms,
            stats.custom_audio_delay_ms,
        )
    } else {
        (0.0, 0.0, 0.0, 0.0, active.audio_delay_ms())
    };
    let chat_panel = chat_panel.snapshot();
    let stream_ready = active.is_active()
        && frame_hub
            .has_delayed_encoded_start_batch(CUSTOM_STREAM_SERVER_DELAY, active.batch_size());
    let body = format!(
        r#"{{"online":{},"stream_ready":{},"version":"{}","session_token":"{}","branding":{{"page_title":"{}","header_title":"{}"}},"layout":{{"max_player_width_px":{},"prefer_larger_player":{},"minimizable_player":{},"start_player_minimized":{}}},"chat_panel":{{"requested":{},"title":"{}"}},"width":{},"height":{},"fps":{},"audio_delay_ms":{},"video_latency_ms_estimate":{},"batch_duration_ms":{},"readback_latency_ms":{},"http_batch_latency_ms":{}}}"#,
        active.is_active(),
        stream_ready,
        env!("CARGO_PKG_VERSION"),
        active.session_token(),
        json_escape(&branding.page_title),
        json_escape(&branding.header_title),
        layout
            .max_player_width_px
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        layout.prefer_larger_player,
        layout.minimizable_player,
        layout.start_player_minimized,
        chat_panel.requested,
        json_escape(&chat_panel.title),
        active.width(),
        active.height(),
        active.fps(),
        audio_delay_ms,
        rounded_json_number(video_latency_ms_estimate),
        rounded_json_number(batch_duration_ms),
        rounded_json_number(readback_latency_ms),
        rounded_json_number(http_batch_latency_ms),
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store, no-cache, must-revalidate, max-age=0\r\nPragma: no-cache\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn rounded_json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.2}")
    } else {
        "0".to_owned()
    }
}

fn submit_local_chat(
    mut stream: TcpStream,
    request: &HttpRequest,
    peer_addr: Option<SocketAddr>,
    chat: LocalChatHub,
    active: CustomStreamState,
) {
    if !authorized_mutation(request, &active) {
        serve_forbidden(stream, request);
        return;
    }
    let body = request.body_str();
    let message = body.trim();
    if message.is_empty() || message.len() > 500 {
        serve_bad_request(stream, request);
        return;
    }
    let identity = local_chat_identity(request, peer_addr);
    let body = match chat.submit(identity, message.to_owned()) {
        LocalChatSubmitResult::Accepted { display_name } => {
            format!(r#"{{"ok":true,"name":"{}"}}"#, json_escape(&display_name))
        }
        LocalChatSubmitResult::Rejected {
            display_name,
            reason,
        } => format!(
            r#"{{"ok":false,"name":"{}","reason":"{}"}}"#,
            json_escape(&display_name),
            reason.code()
        ),
        LocalChatSubmitResult::BlockedIdentity => {
            serve_forbidden(stream, request);
            return;
        }
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store, no-cache, must-revalidate, max-age=0\r\nPragma: no-cache\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_local_chat_feed(
    mut stream: TcpStream,
    request: &HttpRequest,
    peer_addr: Option<SocketAddr>,
    chat: LocalChatHub,
) {
    let after = request
        .query_param("after")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let identity_source = local_chat_identity(request, peer_addr);
    let (viewer_identity, viewer_name) = chat.viewer_for_identity(&identity_source);
    chat.purge_expired(current_time_millis());
    let generation = chat.generation();
    let latest_id = chat.latest_id();
    let entries = chat.entries_after(after, Some(&viewer_identity), Some(&viewer_name));
    let mut body = format!(
        r#"{{"generation":{generation},"latest":{latest_id},"viewer":{{"identity":"{}","name":"{}"}},"messages":["#,
        json_escape(&viewer_identity),
        json_escape(&viewer_name)
    );
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        let expires_at_ms = entry
            .ttl_ms
            .map(|ttl| entry.created_at_ms.saturating_add(ttl));
        let mentions = entry
            .mentions
            .iter()
            .map(|mention| format!(r#""{}""#, json_escape(mention)))
            .collect::<Vec<_>>()
            .join(",");
        body.push_str(&format!(
            r#"{{"id":{},"name":"{}","user":"{}","text":"{}","created_at_ms":{},"expires_at_ms":{},"mentions":[{}],"display_name_color":{},"message_color":{},"css_class":{}}}"#,
            entry.id,
            json_escape(&entry.display_name),
            json_escape(&entry.user),
            json_escape(&entry.text),
            entry.created_at_ms,
            expires_at_ms
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_owned()),
            mentions,
            json_optional_string(entry.display_name_color.as_deref()),
            json_optional_string(entry.message_color.as_deref()),
            json_optional_string(entry.css_class.as_deref())
        ));
    }
    body.push_str("]}");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_custom_panels(
    mut stream: TcpStream,
    request: &HttpRequest,
    peer_addr: Option<SocketAddr>,
    panels: CustomHostPanelHub,
    chat: LocalChatHub,
) {
    let identity_source = local_chat_identity(request, peer_addr);
    let (viewer_identity, viewer_name) = chat.viewer_for_identity(&identity_source);
    let panels = panels.snapshot_for_viewer(Some(&viewer_identity), Some(&viewer_name));
    let mut body = format!(
        r#"{{"viewer":{{"identity":"{}","name":"{}"}},"panels":["#,
        json_escape(&viewer_identity),
        json_escape(&viewer_name)
    );
    for (index, panel) in panels.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        let anchor = panel.anchor.as_json_str();
        body.push_str(&format!(
            r#"{{"id":"{}","title":"{}","body":"{}","elements":{},"revision":{},"anchor":"{}","region":"{}","order":{},"size_hint":{},"style_hint":{}}}"#,
            json_escape(&panel.id),
            json_escape(&panel.title),
            json_escape(&panel.body),
            panel_elements_json(&panel.text_elements()),
            panel.revision,
            json_escape(&anchor),
            json_escape(&anchor),
            panel.order,
            panel_size_hint_json(panel.size_hint.as_ref()),
            panel_style_hint_json(panel.style_hint.as_ref())
        ));
    }
    body.push_str("]}");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_custom_overlays(
    mut stream: TcpStream,
    request: &HttpRequest,
    peer_addr: Option<SocketAddr>,
    overlays: CustomHostOverlayHub,
    chat: LocalChatHub,
) {
    let identity_source = local_chat_identity(request, peer_addr);
    let (viewer_identity, viewer_name) = chat.viewer_for_identity(&identity_source);
    let overlays = overlays.snapshot_for_viewer(Some(&viewer_identity), Some(&viewer_name));
    let mut body = format!(
        r#"{{"viewer":{{"identity":"{}","name":"{}"}},"overlays":["#,
        json_escape(&viewer_identity),
        json_escape(&viewer_name)
    );
    for (index, overlay) in overlays.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        body.push_str(&format!(
            r#"{{"id":"{}","x":{},"y":{},"coordinate_space":"{}","kind":{},"order":{},"style":{}}}"#,
            json_escape(&overlay.id),
            json_f32(overlay.x),
            json_f32(overlay.y),
            overlay.coordinate_space.as_json_str(),
            overlay_kind_json(&overlay.kind),
            overlay.order,
            overlay_style_json(&overlay.style)
        ));
    }
    body.push_str("]}");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn submit_custom_panel_action(
    mut stream: TcpStream,
    request: &HttpRequest,
    peer_addr: Option<SocketAddr>,
    chat: LocalChatHub,
    panel_actions: CustomHostPanelActionHub,
    active: CustomStreamState,
) {
    if !authorized_mutation(request, &active) {
        serve_forbidden(stream, request);
        return;
    }
    let body = request.body_str();
    let Some(panel_id) = json_string_field(body, "panel_id") else {
        serve_bad_request(stream, request);
        return;
    };
    let Some(action_id) = json_string_field(body, "action_id") else {
        serve_bad_request(stream, request);
        return;
    };
    if panel_id.trim().is_empty()
        || action_id.trim().is_empty()
        || panel_id.len() > 256
        || action_id.len() > 256
    {
        serve_bad_request(stream, request);
        return;
    }

    let identity_source = local_chat_identity(request, peer_addr);
    let (viewer_identity, viewer_name) = chat.viewer_for_identity(&identity_source);
    panel_actions.submit(CustomHostPanelAction {
        viewer_identity,
        viewer_name,
        panel_id,
        action_id,
    });
    let body = r#"{"ok":true}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn submit_stream_click(
    mut stream: TcpStream,
    request: &HttpRequest,
    peer_addr: Option<SocketAddr>,
    chat: LocalChatHub,
    clicks: StreamPointerClickHub,
    active: CustomStreamState,
) {
    if !authorized_mutation(request, &active) {
        serve_forbidden(stream, request);
        return;
    }
    let body = request.body_str();
    let Some(x) = json_u32_field(body, "x") else {
        serve_bad_request(stream, request);
        return;
    };
    let Some(y) = json_u32_field(body, "y") else {
        serve_bad_request(stream, request);
        return;
    };
    let normalized_x = json_f32_field(body, "normalized_x").unwrap_or(0.0);
    let normalized_y = json_f32_field(body, "normalized_y").unwrap_or(0.0);
    let client_x = json_f32_field(body, "client_x").unwrap_or(0.0);
    let client_y = json_f32_field(body, "client_y").unwrap_or(0.0);
    let identity_source = local_chat_identity(request, peer_addr);
    let (identity, display_name) = chat.viewer_for_identity(&identity_source);
    clicks.submit(StreamPointerClick {
        identity,
        display_name,
        client_x,
        client_y,
        x,
        y,
        normalized_x,
        normalized_y,
    });
    let body = r#"{"ok":true}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn query_param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key == name).then(|| value.to_owned())
        })
    }

    fn body_str(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("")
    }
}

fn local_chat_identity(request: &HttpRequest, peer_addr: Option<SocketAddr>) -> String {
    request
        .header("x-directstream-device-id")
        .map(ToOwned::to_owned)
        .and_then(validate_device_id)
        .map(|id| format!("device:{id}"))
        .or_else(|| {
            request
                .header("cf-connecting-ip")
                .map(|ip| format!("ip:{ip}"))
        })
        .or_else(|| {
            request
                .header("x-forwarded-for")
                .map(ToOwned::to_owned)
                .and_then(first_forwarded_ip)
                .map(|ip| format!("ip:{ip}"))
        })
        .or_else(|| peer_addr.map(|addr| format!("ip:{}", addr.ip())))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn validate_device_id(id: String) -> Option<String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 128 {
        return None;
    }

    id.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        .then(|| id.to_owned())
}

fn first_forwarded_ip(value: String) -> Option<String> {
    value
        .split(',')
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn current_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn json_u32_field(body: &str, name: &str) -> Option<u32> {
    json_number_field(body, name).and_then(|value| value.parse::<u32>().ok())
}

fn json_f32_field(body: &str, name: &str) -> Option<f32> {
    json_number_field(body, name).and_then(|value| value.parse::<f32>().ok())
}

fn json_string_field(body: &str, name: &str) -> Option<String> {
    let key = format!(r#""{name}""#);
    let (_, rest) = body.split_once(&key)?;
    let (_, rest) = rest.split_once(':')?;
    let mut chars = rest.trim_start().chars();
    (chars.next()? == '"').then_some(())?;

    let mut value = String::new();
    let mut escaped = false;
    for ch in chars {
        if escaped {
            match ch {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'b' => value.push('\u{0008}'),
                'f' => value.push('\u{000c}'),
                _ => return None,
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(value);
        } else {
            value.push(ch);
        }
    }
    None
}

fn json_number_field<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(r#""{name}""#);
    let (_, rest) = body.split_once(&key)?;
    let (_, rest) = rest.split_once(':')?;
    let rest = rest.trim_start();
    let end = rest
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_owned())
}

fn panel_size_hint_json(size: Option<&CustomHostPanelSize>) -> String {
    let Some(size) = size else {
        return "null".to_owned();
    };
    format!(
        r#"{{"min_width_px":{},"max_width_px":{},"min_height_px":{},"max_height_px":{}}}"#,
        json_optional_u32(size.min_width_px),
        json_optional_u32(size.max_width_px),
        json_optional_u32(size.min_height_px),
        json_optional_u32(size.max_height_px)
    )
}

fn panel_style_hint_json(style: Option<&CustomHostPanelStyle>) -> String {
    let Some(style) = style else {
        return "null".to_owned();
    };
    format!(
        r#"{{"css_class":{},"hide_header":{},"body_white_space":{},"overflow_mode":{},"region_css_class":{}}}"#,
        json_optional_string(style.css_class.as_deref()),
        style.hide_header,
        json_optional_string(style.body_white_space.map(PanelWhiteSpace::as_json_str)),
        json_optional_string(style.overflow_mode.map(|mode| mode.as_json_str())),
        json_optional_string(style.region_css_class.as_deref())
    )
}

fn panel_elements_json(elements: &[CustomHostPanelElement]) -> String {
    let mut json = String::from("[");
    for (index, element) in elements.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        match element {
            CustomHostPanelElement::Text(text) => {
                json.push_str(&format!(
                    r#"{{"type":"Text","text":"{}"}}"#,
                    json_escape(text)
                ));
            }
            CustomHostPanelElement::StyledText { text, style } => {
                json.push_str(&format!(
                    r#"{{"type":"StyledText","text":"{}","style":{}}}"#,
                    json_escape(text),
                    panel_element_style_json(style)
                ));
            }
            CustomHostPanelElement::Button {
                label,
                action_id,
                disabled,
            } => {
                json.push_str(&format!(
                    r#"{{"type":"Button","label":"{}","action_id":"{}","disabled":{}}}"#,
                    json_escape(label),
                    json_escape(action_id),
                    disabled
                ));
            }
            CustomHostPanelElement::PagedText {
                id,
                pages,
                initial_page,
                controls,
            } => {
                json.push_str(&format!(
                    r#"{{"type":"PagedText","id":"{}","initial_page":{},"controls":{{"previous_label":"{}","next_label":"{}","show_page_indicator":{},"wrap":{},"position":"{}"}},"pages":{}}}"#,
                    json_escape(id),
                    initial_page,
                    json_escape(&controls.previous_label),
                    json_escape(&controls.next_label),
                    controls.show_page_indicator,
                    controls.wrap,
                    controls.position.as_json_str(),
                    panel_pages_json(pages)
                ));
            }
        }
    }
    json.push(']');
    json
}

fn panel_element_style_json(style: &CustomHostPanelElementStyle) -> String {
    format!(
        r#"{{"text_color":{},"css_class":{},"font_weight":{}}}"#,
        json_optional_string(style.text_color.as_deref()),
        json_optional_string(style.css_class.as_deref()),
        json_optional_string(style.font_weight.as_deref())
    )
}

fn panel_pages_json(pages: &[crate::custom_host::CustomHostPanelPage]) -> String {
    let mut json = String::from("[");
    for (index, page) in pages.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"title":{},"body":"{}"}}"#,
            json_optional_string(page.title.as_deref()),
            json_escape(&page.body)
        ));
    }
    json.push(']');
    json
}

fn json_optional_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned())
}

fn overlay_kind_json(kind: &OverlayElementKind) -> String {
    match kind {
        OverlayElementKind::Circle { radius } => {
            format!(r#"{{"type":"Circle","radius":{}}}"#, json_f32(*radius))
        }
        OverlayElementKind::Flag { width, height } => format!(
            r#"{{"type":"Flag","width":{},"height":{}}}"#,
            json_f32(*width),
            json_f32(*height)
        ),
        OverlayElementKind::Text { text } => {
            format!(r#"{{"type":"Text","text":"{}"}}"#, json_escape(text))
        }
        OverlayElementKind::Sprite {
            image_id,
            width,
            height,
        } => format!(
            r#"{{"type":"Sprite","image_id":"{}","width":{},"height":{}}}"#,
            json_escape(image_id),
            json_f32(*width),
            json_f32(*height)
        ),
    }
}

fn overlay_style_json(style: &OverlayElementStyle) -> String {
    format!(
        r#"{{"stroke_color":{},"fill_color":{},"text_color":{},"line_width":{},"font_px":{},"css_class":{}}}"#,
        json_optional_string(style.stroke_color.as_deref()),
        json_optional_string(style.fill_color.as_deref()),
        json_optional_string(style.text_color.as_deref()),
        json_f32(style.line_width),
        json_f32(style.font_px),
        json_optional_string(style.css_class.as_deref())
    )
}

fn json_f32(value: f32) -> String {
    if value.is_finite() {
        value.to_string()
    } else {
        "0".to_owned()
    }
}

fn authorized_mutation(request: &HttpRequest, active: &CustomStreamState) -> bool {
    // Device identity is for audience scoping, not authorization. Mutating
    // browser calls also need a trusted Origin and the per-process session token
    // that the player learns from /status.json.
    origin_allowed(request)
        && request
            .header(PIXEL_STREAM_SESSION_HEADER)
            .is_some_and(|token| token == active.session_token())
}

fn origin_allowed(request: &HttpRequest) -> bool {
    let Some(origin) = request.header("origin") else {
        return true;
    };
    allowed_browser_origin(origin)
}

fn allowed_browser_origin(origin: &str) -> bool {
    matches!(
        origin,
        "https://stream.humanbeangames.com"
            | "https://game.humanbeangames.com"
            | "https://humanbeangames.com"
            | "https://www.humanbeangames.com"
            | "http://127.0.0.1:8080"
            | "http://localhost:8080"
    ) || origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("http://localhost:")
}

fn cors_headers(request: &HttpRequest) -> String {
    let allow_origin = match request.header("origin") {
        Some(origin) if allowed_browser_origin(origin) => origin,
        Some(_) => "null",
        None => "*",
    };
    format!(
        "Access-Control-Allow-Origin: {allow_origin}\r\nAccess-Control-Allow-Headers: Content-Type, X-DirectStream-Device-Id, {PIXEL_STREAM_SESSION_HEADER}\r\nAccess-Control-Expose-Headers: X-Stream-Fps, X-Playback-Buffer-Seconds"
    )
}

fn serve_http_error(mut stream: TcpStream, error: HttpRequestError) {
    let (status, body) = match error {
        HttpRequestError::BadRequest => ("400 Bad Request", "bad request"),
        HttpRequestError::PayloadTooLarge => ("413 Payload Too Large", "payload too large"),
        HttpRequestError::MethodNotAllowed => ("405 Method Not Allowed", "method not allowed"),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_bad_request(mut stream: TcpStream, request: &HttpRequest) {
    let body = "bad request";
    let response = format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_forbidden(mut stream: TcpStream, request: &HttpRequest) {
    let body = "forbidden";
    let response = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        cors_headers(request),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn serve_options(mut stream: TcpStream, request: &HttpRequest) {
    let response = format!(
        "HTTP/1.1 204 No Content\r\n{}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        cors_headers(request)
    );
    let _ = stream.write_all(response.as_bytes());
}

fn stream_mjpeg(
    mut stream: TcpStream,
    request: &HttpRequest,
    frame_hub: EncodedFrameHub,
    stats: SharedStats,
) {
    stats.with_mut(|stats| stats.stream_clients += 1);
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n",
        cors_headers(request)
    );
    if stream.write_all(header.as_bytes()).is_err() {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    }

    let mut last_sequence = 0;
    while let Some((sequence, jpeg)) = frame_hub.wait_for_frame_after(last_sequence) {
        last_sequence = sequence;
        let part_header = format!(
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            jpeg.len()
        );

        if stream.write_all(part_header.as_bytes()).is_err()
            || stream.write_all(&jpeg).is_err()
            || stream.write_all(b"\r\n").is_err()
        {
            break;
        }
    }
    stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
}

fn stream_palette(
    mut stream: TcpStream,
    request: &HttpRequest,
    frame_hub: PaletteFrameHub,
    stats: SharedStats,
    active: CustomStreamState,
) {
    if !active.is_active() {
        let body = "stream offline";
        let response = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            cors_headers(request),
            body
        );
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    stats.with_mut(|stats| stats.stream_clients += 1);

    stats.with_mut(|stats| stats.custom_stage = "waiting for stream frame");
    let Some(start_batch) = frame_hub.wait_for_delayed_encoded_start_batch(
        CUSTOM_STREAM_SERVER_DELAY,
        active.batch_size(),
        &stats,
        || active.is_active(),
    ) else {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    };
    if !active.is_active() {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    }
    let Some(stream_header) = frame_hub.stream_header() else {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    };
    let start_packets = &start_batch.batch.packets[start_batch.start_packet_index..];
    let Some(start_packet_bytes) = start_packets.first() else {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    };
    let stream_fps = active.fps();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\nX-Stream-Fps: {stream_fps}\r\nX-Playback-Buffer-Seconds: {CUSTOM_STREAM_PLAYBACK_BUFFER_SECONDS:.3}\r\n\r\n",
        cors_headers(request)
    );
    let mut last_sequence = start_batch.batch.sequence;
    if stream.write_all(header.as_bytes()).is_err()
        || stream.write_all(&stream_header).is_err()
        || write_palette_cache_reset(&mut stream).is_err()
        || write_palette_packets(&mut stream, start_packets).is_err()
    {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    }

    stats.with_mut(|stats| {
        stats.latest_frame_bytes = start_packet_bytes.len();
        stats.custom_stage = "streaming";
    });

    while let Some(batch) = frame_hub.wait_for_delayed_encoded_batch_after(
        last_sequence,
        CUSTOM_STREAM_SERVER_DELAY,
        &stats,
        || active.is_active(),
    ) {
        if !active.is_active() || write_palette_batch(&mut stream, &batch.packets).is_err() {
            break;
        }
        last_sequence = batch.sequence;
        stats.with_mut(|stats| {
            stats.latest_frame_bytes = batch.bytes / batch.packets.len().max(1);
        });
    }
    stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
}

fn write_palette_batch(stream: &mut TcpStream, batch: &[Vec<u8>]) -> std::io::Result<()> {
    let batch_len: usize = 4 + batch.iter().map(|packet| 4 + packet.len()).sum::<usize>();
    let mut bytes = Vec::with_capacity(batch_len);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for packet in batch {
        bytes.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        bytes.extend_from_slice(packet);
    }
    stream.write_all(&bytes)
}

fn write_palette_cache_reset(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(&0u32.to_le_bytes())
}

fn write_palette_packets(stream: &mut TcpStream, packets: &[Vec<u8>]) -> std::io::Result<()> {
    for packet in packets {
        let packet_len = packet.len() as u32;
        stream.write_all(&packet_len.to_le_bytes())?;
        stream.write_all(packet)?;
    }
    Ok(())
}

fn stream_pcm_audio(
    mut stream: TcpStream,
    request: &HttpRequest,
    audio: CustomAudioPacketHub,
    stats: SharedStats,
    active: CustomStreamState,
) {
    stats.with_mut(|stats| stats.stream_clients += 1);
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\n{}\r\nConnection: close\r\nX-Audio-Format: mulaw8\r\nX-Audio-Sample-Rate: {CUSTOM_AUDIO_SAMPLE_RATE}\r\nX-Audio-Channels: {CUSTOM_AUDIO_CHANNELS}\r\n\r\n",
        cors_headers(request)
    );
    if stream.write_all(header.as_bytes()).is_err() {
        stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
        return;
    }

    let mut last_sequence = 0;
    loop {
        if !active.is_active() {
            break;
        }

        let Some((sequence, packet)) =
            audio.wait_for_packet_after_timeout(last_sequence, Duration::from_millis(100))
        else {
            continue;
        };
        last_sequence = sequence;

        if stream.write_all(&packet).is_err() {
            break;
        }
    }

    stats.with_mut(|stats| stats.stream_clients = stats.stream_clients.saturating_sub(1));
}

fn mjpeg_stream_page_html() -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>PixelStream</title>
  <style>
    :root {{ color-scheme: dark; font-family: Arial, sans-serif; background: #111318; color: #eef3f8; }}
    body {{ margin: 0; min-height: 100vh; display: grid; grid-template-rows: auto 1fr; }}
    body.stream-minimized {{ min-height: 0; grid-template-rows: auto; }}
    body.stream-minimized main {{ display: none; }}
    header {{ padding: 12px 16px; background: #1b2029; border-bottom: 1px solid #303847; }}
    header {{ display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }}
    button {{ appearance: none; border: 1px solid #4a5668; border-radius: 5px; background: #263142; color: #f8fafc; padding: 7px 10px; font: inherit; cursor: pointer; }}
    button:disabled {{ opacity: 0.55; cursor: default; }}
    main {{ display: grid; gap: 12px; place-items: center; padding: 16px; }}
    img {{ width: min(100%, 960px); aspect-ratio: 4 / 3; object-fit: contain; image-rendering: pixelated; background: #050608; border: 1px solid #303847; }}
  </style>
</head>
<body>
  <header>PixelStream local preview</header>
  <main>
    <img id="stream" alt="Bevy GPU readback stream" src="{STREAM_PATH}">
  </main>
  <script>
    const stream = document.getElementById("stream");
    stream.onerror = () => setTimeout(() => {{
      stream.src = "{STREAM_PATH}?retry=" + Date.now();
    }}, 1000);
  </script>
</body>
</html>"#
    )
}

fn palette_stream_page_html(branding: &CustomHostBranding, layout: &CustomHostLayout) -> String {
    palette_stream_page_html_with_options("", branding, layout)
}

pub fn static_palette_stream_page_html(backend_origin: &str) -> String {
    static_palette_stream_page_html_with_options(
        backend_origin,
        &CustomHostBranding::default(),
        &CustomHostLayout::default(),
    )
}

pub fn static_palette_stream_page_html_with_options(
    backend_origin: &str,
    branding: &CustomHostBranding,
    layout: &CustomHostLayout,
) -> String {
    palette_stream_page_html_with_options(backend_origin, branding, layout)
}

pub fn export_static_palette_stream_page(
    out_dir: impl AsRef<Path>,
    backend_origin: &str,
    branding: &CustomHostBranding,
    layout: &CustomHostLayout,
) -> Result<(), String> {
    let out_dir = out_dir.as_ref();
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    fs::write(
        out_dir.join("index.html"),
        static_palette_stream_page_html_with_options(backend_origin, branding, layout),
    )
    .map_err(|err| err.to_string())
}

fn palette_stream_page_html_with_options(
    backend_origin: &str,
    branding: &CustomHostBranding,
    layout: &CustomHostLayout,
) -> String {
    let backend = backend_origin.trim_end_matches('/');
    let palette_stream_url = format!("{backend}{PALETTE_STREAM_PATH}");
    let audio_stream_url = format!("{backend}{AUDIO_STREAM_PATH}");
    let local_chat_url = format!("{backend}{LOCAL_CHAT_PATH}");
    let stream_status_url = format!("{backend}{STREAM_STATUS_PATH}");
    let local_chat_feed_url = format!("{backend}{LOCAL_CHAT_FEED_PATH}");
    let custom_panels_url = format!("{backend}{CUSTOM_PANELS_PATH}");
    let custom_panel_action_url = format!("{backend}{CUSTOM_PANEL_ACTION_PATH}");
    let custom_overlays_url = format!("{backend}{CUSTOM_OVERLAYS_PATH}");
    let stream_click_url = format!("{backend}{STREAM_CLICK_PATH}");
    let page_title = html_escape(&branding.page_title);
    let header_title = html_escape(&branding.header_title);
    let page_title_js = json_escape(&branding.page_title);
    let header_title_js = json_escape(&branding.header_title);
    let version = env!("CARGO_PKG_VERSION");
    let exported_at_ms = current_time_millis();
    let max_player_width = layout
        .max_player_width_px
        .unwrap_or(if layout.prefer_larger_player {
            1280
        } else {
            960
        })
        .clamp(240, 4096);
    let minimizable_player = layout.minimizable_player;
    let start_player_minimized = layout.start_player_minimized;

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{page_title}</title>
  <meta name="direct-stream-version" content="{version}">
  <meta name="direct-stream-exported-at-ms" content="{exported_at_ms}">
  <style>
    :root {{ color-scheme: dark; font-family: Arial, sans-serif; background: #111318; color: #eef3f8; --direct-stream-max-player-width: {max_player_width}px; }}
    * {{ box-sizing: border-box; }}
    body {{ margin: 0; min-height: 100vh; display: grid; grid-template-rows: auto 1fr; }}
    header {{ display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 12px 16px; background: #1b2029; border-bottom: 1px solid #303847; }}
    header button {{ appearance: none; border: 1px solid #4a5668; border-radius: 4px; background: #263142; color: #f8fafc; padding: 6px 10px; font: inherit; cursor: pointer; }}
    header button[hidden] {{ display: none; }}
    main {{ display: grid; place-items: center; padding: 16px; min-height: 0; overflow: auto; }}
    .stage {{ --stage-left-column: 0px; --stage-right-column: 0px; --direct-stream-aspect-ratio: 1 / 1; --direct-stream-player-size: 240px; width: min(100%, var(--direct-stream-max-player-width)); display: grid; grid-template-columns: min(100%, var(--direct-stream-max-player-width)); grid-template-areas: "player"; gap: 8px; align-items: start; justify-content: center; min-height: 0; }}
    .stage.app-ui-active {{ width: max-content; max-width: none; grid-template-columns: var(--stage-left-column) var(--direct-stream-max-player-width) var(--stage-right-column); grid-template-areas: "above above above" "left player right" "below below below"; }}
    .stage.app-ui-active.has-left {{ --stage-left-column: 320px; }}
    .stage.app-ui-active.has-right {{ --stage-right-column: 320px; }}
    .stage.player-minimized {{ width: 0; height: 0; gap: 0; overflow: hidden; }}
    .stage.player-minimized .player,
    .stage.player-minimized .left-region,
    .stage.player-minimized .right-region,
    .stage.player-minimized .above-region,
    .stage.player-minimized .below-region {{ display: none; }}
    .player {{ grid-area: player; position: relative; width: min(100%, var(--direct-stream-max-player-width)); aspect-ratio: var(--direct-stream-aspect-ratio); min-width: 240px; }}
    canvas {{ display: block; width: 100%; height: 100%; object-fit: contain; image-rendering: pixelated; image-rendering: crisp-edges; background: #050608; border: 1px solid #303847; }}
    .stream-overlay {{ position: absolute; inset: 0; width: 100%; height: 100%; border: 0; pointer-events: none; z-index: 3; background: transparent; }}
    .stream-loading {{ position: absolute; inset: 0; z-index: 2; display: grid; place-items: center; background: #050608; color: #dbe7f3; font: 700 clamp(16px, 3vw, 28px) Arial, sans-serif; letter-spacing: 0; text-align: center; pointer-events: none; }}
    .stream-loading[hidden] {{ display: none; }}
    .unmute {{ appearance: none; position: absolute; inset: -1px; z-index: 4; display: grid; place-items: center; box-sizing: border-box; margin: 0; padding: 0; border: 0; border-radius: 0; background: rgba(5, 6, 8, 0.54); color: #f8fafc; font: 700 clamp(18px, 4vw, 34px) Arial, sans-serif; cursor: pointer; text-shadow: 0 2px 8px #000; }}
    .unmute[hidden] {{ display: none; }}
    .player-controls {{ position: absolute; z-index: 7; left: 10px; right: 10px; bottom: 10px; display: flex; gap: 8px; align-items: center; padding: 6px 8px; border-radius: 5px; background: rgba(5, 6, 8, 0.62); opacity: 0; pointer-events: none; transition: opacity 120ms ease; }}
    .player:hover .player-controls, .unmute:hover ~ .player-controls, .player-controls:focus-within {{ opacity: 1; pointer-events: auto; }}
    .player-controls[hidden] {{ display: none; }}
    .player-controls button {{ appearance: none; border: 1px solid #4a5668; border-radius: 4px; background: #263142; color: #f8fafc; padding: 4px 8px; font: inherit; cursor: pointer; }}
    .player-controls input {{ flex: 1; min-width: 0; }}
    .chat {{ min-height: 0; max-height: 100%; display: flex; flex-direction: column; border: 1px solid #303847; background: #0b0d12; }}
    .chat[hidden] {{ display: none; }}
    .chat h2 {{ margin: 0; padding: 10px 12px; border-bottom: 1px solid #303847; font-size: 14px; }}
    .chat-log {{ flex: 1 1 auto; min-height: 0; padding: 10px 12px; overflow-y: auto; color: #cbd5e1; font: 13px Consolas, monospace; }}
    .chat-log p {{ margin: 0 0 8px; white-space: pre-wrap; overflow-wrap: anywhere; }}
    .chat-input {{ flex: none; display: flex; gap: 6px; padding: 10px; border-top: 1px solid #303847; }}
    .chat-input input {{ flex: 1; min-width: 0; background: #111722; color: #eef3f8; border: 1px solid #3a4353; border-radius: 4px; padding: 7px 8px; }}
    .chat-input button {{ border: 1px solid #4a5668; border-radius: 4px; background: #263142; color: #f8fafc; padding: 7px 10px; }}
    .chat-log p.mentioned-me {{ color: #fff7d6; background: rgba(247, 197, 72, 0.18); margin-inline: -4px; padding: 2px 4px; border-left: 2px solid #f7c548; }}
    .left-region {{ grid-area: left; width: var(--stage-left-column); min-width: 0; min-height: 0; display: none; flex-direction: column; align-content: stretch; overflow-x: hidden; overflow-y: hidden; }}
    .right-region {{ grid-area: right; width: var(--stage-right-column); min-width: 0; min-height: 0; display: none; flex-direction: column; gap: 8px; overflow-x: hidden; }}
    .stage.app-ui-active.has-left .left-region {{ display: flex; height: var(--direct-stream-player-size); max-height: var(--direct-stream-player-size); }}
    .stage.app-ui-active.has-right .right-region {{ display: flex; height: var(--direct-stream-player-size); max-height: var(--direct-stream-player-size); }}
    .left-region > .panel {{ width: 100%; max-width: 100%; min-width: 0; min-height: 0; flex: 0 0 auto; display: flex; flex-direction: column; }}
    .left-region > .panel:last-child {{ flex: 1 1 auto; }}
    .left-region > .panel > pre,
    .left-region > .panel > .panel-content {{ flex: 1 1 auto; min-height: 0; }}
    .above-region {{ grid-area: above; display: none; min-width: 0; }}
    .below-region {{ grid-area: below; display: none; min-width: 0; }}
    .stage.app-ui-active.has-above .above-region,
    .stage.app-ui-active.has-below .below-region {{ display: grid; }}
    .panel-region.region-wrap-no-scroll,
    .right-panels.region-wrap-no-scroll {{ overflow: visible; }}
    .panel-region {{ display: grid; gap: 8px; align-content: start; }}
    .panel-region:empty {{ display: none; }}
    .right-panels {{ display: grid; gap: 8px; align-content: start; }}
    .right-panels:empty {{ display: none; }}
    .right-region > .chat {{ flex: 1 1 auto; min-height: 0; height: 100%; }}
    .right-region > .right-panels {{ flex: 0 0 auto; }}
    .panel {{ border: 1px solid #303847; background: #0b0d12; min-width: 0; max-width: 100%; overflow: hidden; }}
    .panel.panel-headerless {{ width: auto; max-width: min(100%, 720px); }}
    .panel h2 {{ margin: 0; padding: 9px 12px; border-bottom: 1px solid #303847; font-size: 14px; }}
    .panel pre {{ margin: 0; padding: 10px 12px; min-width: 0; max-width: 100%; overflow-x: hidden; white-space: pre-wrap; overflow-wrap: anywhere; color: #dbe4ef; font: 13px Consolas, monospace; }}
    .panel-content {{ margin: 0; padding: 10px 12px; min-width: 0; max-width: 100%; overflow-x: hidden; color: #dbe4ef; font: 13px Consolas, monospace; }}
    .panel-text {{ white-space: pre-wrap; overflow-wrap: anywhere; }}
    .panel-button {{ margin: 0 6px 6px 0; border: 1px solid #4a5668; border-radius: 4px; background: #263142; color: #f8fafc; padding: 4px 8px; font: inherit; cursor: pointer; }}
    .panel-button:disabled {{ opacity: 0.5; cursor: default; }}
    .panel-paged-text {{ display: grid; gap: 6px; margin: 0 0 8px; }}
    .panel-paged-title {{ color: #f8fafc; font-weight: 700; }}
    .panel-paged-body {{ display: block; }}
    .panel-page-controls {{ display: inline-flex; gap: 6px; align-items: center; flex-wrap: wrap; }}
    .panel-page-indicator {{ color: #9fb0c4; font-size: 12px; }}
    .panel.panel-headerless pre {{ min-width: 0; max-width: 100%; }}
    .panel.panel-pre-wrap pre {{ white-space: pre-wrap; overflow-wrap: anywhere; }}
    .panel.panel-nowrap pre {{ white-space: pre; overflow-wrap: normal; }}
    .panel.panel-nowrap .panel-text {{ white-space: pre; overflow-wrap: normal; }}
    .panel.panel-wrap-no-scroll {{ width: auto; max-width: 100%; overflow: hidden; }}
    .panel.panel-wrap-no-scroll pre,
    .panel.panel-wrap-no-scroll .panel-content {{ min-width: 0; overflow: hidden; white-space: pre-wrap; overflow-wrap: anywhere; }}
    .panel.panel-wrap-no-scroll .panel-text,
    .panel.panel-wrap-no-scroll .panel-paged-body {{ white-space: pre-wrap; overflow-wrap: anywhere; }}
    .panel-styled-text {{ display: inline; }}
    .overlay-panels {{ position: absolute; z-index: 4; display: grid; gap: 6px; max-width: min(44%, 280px); pointer-events: none; }}
    .overlay-panels:empty {{ display: none; }}
    .overlay-panels .panel {{ pointer-events: auto; background: rgba(11, 13, 18, 0.84); box-shadow: 0 6px 18px rgba(0, 0, 0, 0.28); }}
    .overlay-panels .panel h2 {{ padding: 6px 8px; font-size: 12px; }}
    .overlay-panels .panel pre {{ padding: 6px 8px; font-size: 12px; }}
    .overlay-top-left {{ top: 10px; left: 10px; }}
    .overlay-top-right {{ top: 10px; right: 10px; }}
    .overlay-bottom-left {{ bottom: 54px; left: 10px; }}
    .overlay-bottom-right {{ bottom: 54px; right: 10px; }}
    .named-panel-region {{ border-top: 1px solid #303847; padding-top: 8px; }}
    .named-panel-region h2 {{ margin: 0 0 8px; color: #b9c7d7; font-size: 12px; text-transform: uppercase; letter-spacing: 0.06em; }}
    @media (max-width: 900px) {{ .stage.app-ui-active, .stage.app-ui-active.has-left, .stage.app-ui-active.has-right {{ width: min(100%, var(--direct-stream-max-player-width)); grid-template-columns: 1fr; grid-template-areas: "above" "player" "left" "right" "below"; }} .player, .stage.app-ui-active .player, .stage.app-ui-active.has-left.has-right .player {{ width: min(calc(100vw - 32px), calc(100vh - 360px), 960px); margin-inline: auto; }} .stage.app-ui-active.has-left .left-region, .stage.app-ui-active.has-right .right-region {{ height: auto; max-height: 420px; }} .chat {{ min-height: 220px; max-height: 420px; }} }}
  </style>
</head>
<body>
  <header><span id="pageHeaderTitle">{header_title}</span><button id="togglePlayerButton" type="button" hidden>Minimize stream</button></header>
  <main>
    <div class="stage" id="stage">
      <section class="panel-region above-region" id="abovePanels"></section>
      <section class="panel-region left-region" id="leftPanels"></section>
      <div class="player">
        <canvas id="screen" width="{STREAM_WIDTH}" height="{STREAM_HEIGHT}"></canvas>
        <canvas class="stream-overlay" id="streamOverlay" width="{STREAM_WIDTH}" height="{STREAM_HEIGHT}"></canvas>
        <div class="stream-loading" id="streamLoading">Loading stream...</div>
        <section class="overlay-panels overlay-top-left" id="overlayTopLeftPanels"></section>
        <section class="overlay-panels overlay-top-right" id="overlayTopRightPanels"></section>
        <section class="overlay-panels overlay-bottom-left" id="overlayBottomLeftPanels"></section>
        <section class="overlay-panels overlay-bottom-right" id="overlayBottomRightPanels"></section>
        <button class="unmute" id="unmuteButton" type="button">Click to unmute</button>
        <div class="player-controls" id="playerControls" hidden>
          <button id="muteButton" type="button">Mute</button>
          <input id="volumeSlider" type="range" min="0" max="1" step="0.01" value="0.8" aria-label="Volume">
        </div>
      </div>
      <div class="right-region">
        <section class="right-panels" id="rightPanels"></section>
      </div>
      <section class="panel-region below-region" id="belowPanels"></section>
    </div>
  </main>
  <script>
    const stage = document.getElementById("stage");
    const canvas = document.getElementById("screen");
    const overlayCanvas = document.getElementById("streamOverlay");
    const streamLoading = document.getElementById("streamLoading");
    const player = document.querySelector(".player");
    const togglePlayerButton = document.getElementById("togglePlayerButton");
    const pageHeaderTitle = document.getElementById("pageHeaderTitle");
    const ctx = canvas.getContext("2d");
    const overlayCtx = overlayCanvas.getContext("2d");
    const unmuteButton = document.getElementById("unmuteButton");
    const playerControls = document.getElementById("playerControls");
    const muteButton = document.getElementById("muteButton");
    const volumeSlider = document.getElementById("volumeSlider");
    let chatForm = null;
    let chatInput = null;
    let chatPanel = null;
    let chatLog = null;
    const rightRegion = document.querySelector(".right-region");
    const leftPanels = document.getElementById("leftPanels");
    const rightPanels = document.getElementById("rightPanels");
    const abovePanels = document.getElementById("abovePanels");
    const belowPanels = document.getElementById("belowPanels");
    const overlayTopLeftPanels = document.getElementById("overlayTopLeftPanels");
    const overlayTopRightPanels = document.getElementById("overlayTopRightPanels");
    const overlayBottomLeftPanels = document.getElementById("overlayBottomLeftPanels");
    const overlayBottomRightPanels = document.getElementById("overlayBottomRightPanels");
    const namedPanelContainers = new Map();
    const appliedPanelRegionClasses = new Set();
    const panelPageState = new Map();
    ctx.imageSmoothingEnabled = false;

    let width = 0;
    let height = 0;
    let tileSize = 8;
    let palette = [];
    let tileCache = [];
    let framebuffer = new Uint8Array(0);
    let image = ctx.createImageData(1, 1);
    let tileScratch = new Uint8Array(64);
    let dirtyTileIndices = [];
    let pending = new Uint8Array(0);
    let frameQueue = [];
    let streamReady = false;
    let streamFps = 5;
    let frameIntervalMs = 1000 / streamFps;
    let playbackBufferSeconds = 1;
    let playbackRunning = false;
    let playbackStarted = false;
    let nextPlaybackAt = 0;
    let audioContext = null;
    let audioGain = null;
    let audioNode = null;
    let fallbackProcessor = null;
    let audioQueue = [];
    let audioQueueOffset = 0;
    let audioMuted = true;
    let audioStarted = false;
    let audioPrimed = false;
    let audioVolume = Number(volumeSlider.value);
    let audioShouldReconnect = false;
    let audioLoopRunning = false;
    let streamOnline = false;
    let serverStreamReady = false;
    let statusPollDelayMs = 500;
    let requestedUiVisible = false;
    let lastChatId = 0;
    let chatGeneration = null;
    let shownChatIds = new Set();
    let currentViewer = null;
    let serverSessionToken = "";
    let currentOverlays = [];
    const overlayImageCache = new Map();
    let lastAudioLeft = 0;
    let lastAudioRight = 0;
    let lastTransportSample = null;
    const audioTransportSampleRate = 8000;
    const audioPlaybackSampleRate = 48000;
    const audioUpsampleFactor = audioPlaybackSampleRate / audioTransportSampleRate;
    const deviceIdHeaderName = "X-DirectStream-Device-Id";
    const deviceIdStorageKey = "pixelstream_device_id";
    const legacyDeviceIdStorageKey = "directstream_device_id";
    const playerMinimizable = {minimizable_player};
    const startPlayerMinimized = {start_player_minimized};
    const staticBranding = {{
      page_title: "{page_title_js}",
      header_title: "{header_title_js}",
      version: "{version}",
      exported_at_ms: {exported_at_ms},
    }};
    const playerMinimizedStorageKey = "directstream_player_minimized";
    let volatileDeviceId = null;

    function loadPlayerMinimized() {{
      if (!playerMinimizable) return false;
      try {{
        const stored = localStorage.getItem(playerMinimizedStorageKey);
        if (stored === "true") return true;
        if (stored === "false") return false;
      }} catch (error) {{
        console.error(error);
      }}
      return startPlayerMinimized;
    }}

    function setPlayerMinimized(minimized) {{
      if (!playerMinimizable) return;
      if (minimized && !audioMuted) {{
        muteAudio();
      }}
      document.body.classList.toggle("stream-minimized", minimized);
      stage.classList.toggle("player-minimized", minimized);
      togglePlayerButton.textContent = minimized ? "Restore stream" : "Minimize stream";
      updateStageUiClasses();
      try {{
        localStorage.setItem(playerMinimizedStorageKey, minimized ? "true" : "false");
      }} catch (error) {{
        console.error(error);
      }}
    }}

    function updatePlayerSizeVariable() {{
      const rect = player.getBoundingClientRect();
      const size = Math.max(0, Math.round(rect.height || rect.width || 0));
      if (size > 0) {{
        stage.style.setProperty("--direct-stream-player-size", `${{size}}px`);
      }}
    }}

    function updateStageUiClasses() {{
      const hasLeft = leftPanels.childElementCount > 0;
      const hasRight = !!chatPanel || rightPanels.childElementCount > 0;
      const hasAbove = abovePanels.childElementCount > 0;
      const hasBelow = belowPanels.childElementCount > 0;
      const appUiActive = hasLeft || hasRight || hasAbove || hasBelow;
      stage.classList.toggle("has-left", hasLeft);
      stage.classList.toggle("has-right", hasRight);
      stage.classList.toggle("has-above", hasAbove);
      stage.classList.toggle("has-below", hasBelow);
      stage.classList.toggle("app-ui-active", appUiActive);
      updatePlayerSizeVariable();
    }}

    if (typeof ResizeObserver !== "undefined") {{
      new ResizeObserver(updatePlayerSizeVariable).observe(player);
    }} else {{
      window.addEventListener("resize", updatePlayerSizeVariable);
    }}

    if (playerMinimizable) {{
      togglePlayerButton.hidden = false;
      setPlayerMinimized(loadPlayerMinimized());
      togglePlayerButton.addEventListener("click", () => {{
        setPlayerMinimized(!stage.classList.contains("player-minimized"));
      }});
    }}
    updateStageUiClasses();
    syncPlayerControls();

    function getDeviceId() {{
      try {{
        let id = localStorage.getItem(deviceIdStorageKey);
        if (!isValidDeviceId(id)) {{
          id = localStorage.getItem(legacyDeviceIdStorageKey);
        }}
        if (!isValidDeviceId(id)) {{
          id = makeDeviceId();
        }}
        if (localStorage.getItem(deviceIdStorageKey) !== id) {{
          localStorage.setItem(deviceIdStorageKey, id);
        }}
        return id;
      }} catch (error) {{
        console.error(error);
        if (!isValidDeviceId(volatileDeviceId)) {{
          volatileDeviceId = makeDeviceId();
        }}
        return volatileDeviceId;
      }}
    }}

    function makeDeviceId() {{
      if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {{
        return crypto.randomUUID();
      }}
      const bytes = new Uint8Array(16);
      if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {{
        crypto.getRandomValues(bytes);
      }} else {{
        for (let i = 0; i < bytes.length; i++) {{
          bytes[i] = Math.floor(Math.random() * 256);
        }}
      }}
      bytes[6] = (bytes[6] & 0x0f) | 0x40;
      bytes[8] = (bytes[8] & 0x3f) | 0x80;
      const hex = [...bytes].map(value => value.toString(16).padStart(2, "0"));
      return `${{hex.slice(0, 4).join("")}}-${{hex.slice(4, 6).join("")}}-${{hex.slice(6, 8).join("")}}-${{hex.slice(8, 10).join("")}}-${{hex.slice(10).join("")}}`;
    }}

    function isValidDeviceId(id) {{
      return typeof id === "string" && /^[A-Za-z0-9_-]{{1,128}}$/.test(id);
    }}

    function identityFetchOptions(options = {{}}) {{
      const headers = new Headers(options.headers || {{}});
      headers.set(deviceIdHeaderName, getDeviceId());
      return {{ ...options, headers }};
    }}

    function sessionFetchOptions(options = {{}}) {{
      const next = identityFetchOptions(options);
      if (serverSessionToken) {{
        next.headers.set("{PIXEL_STREAM_SESSION_HEADER}", serverSessionToken);
      }}
      return next;
    }}

    window.PixelStreamResetLocalIdentity = () => {{
      localStorage.removeItem(deviceIdStorageKey);
      localStorage.removeItem(legacyDeviceIdStorageKey);
      console.info("PixelStream local identity reset. Reload the page to use a new identity.");
    }};
    window.DirectStreamResetLocalIdentity = window.PixelStreamResetLocalIdentity;

    function resetStreamState() {{
      width = 0;
      height = 0;
      tileSize = 8;
      palette = [];
      tileCache = [];
      framebuffer = new Uint8Array(0);
      image = ctx.createImageData(1, 1);
      dirtyTileIndices.length = 0;
      pending = new Uint8Array(0);
      frameQueue = [];
      streamReady = false;
      streamLoading.hidden = false;
      playbackRunning = false;
      playbackStarted = false;
      nextPlaybackAt = 0;
    }}

    function applyStreamDimensions(streamWidth, streamHeight) {{
      const nextWidth = Number(streamWidth);
      const nextHeight = Number(streamHeight);
      if (!Number.isFinite(nextWidth) || !Number.isFinite(nextHeight) || nextWidth <= 0 || nextHeight <= 0) {{
        return;
      }}
      const aspectRatio = `${{nextWidth}} / ${{nextHeight}}`;
      canvas.style.aspectRatio = aspectRatio;
      overlayCanvas.style.aspectRatio = aspectRatio;
      player.style.aspectRatio = aspectRatio;
      stage.style.setProperty("--direct-stream-aspect-ratio", aspectRatio);
      updatePlayerSizeVariable();
    }}

    function setStreamTiming(fps, bufferSeconds) {{
      if (Number.isFinite(fps) && fps > 0) {{
        streamFps = Math.min(120, Math.max(1, fps));
        frameIntervalMs = 1000 / streamFps;
      }}
      if (Number.isFinite(bufferSeconds) && bufferSeconds >= 0) {{
        playbackBufferSeconds = Math.min(5, bufferSeconds);
      }}
    }}

    function appendBytes(a, b) {{
      const merged = new Uint8Array(a.length + b.length);
      merged.set(a, 0);
      merged.set(b, a.length);
      return merged;
    }}

    function readU32LE(bytes, offset) {{
      return bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16) | (bytes[offset + 3] << 24);
    }}

    function readU16LE(bytes, offset) {{
      return bytes[offset] | (bytes[offset + 1] << 8);
    }}

    function readStreamHeader() {{
      if (streamReady) return true;
      if (pending.length < 12) return false;
      if (pending[0] !== 0x49 || pending[1] !== 0x50 || pending[2] !== 0x53 || pending[3] !== 0x43) {{
        pending = new Uint8Array(0);
        return false;
      }}

      width = readU16LE(pending, 5);
      height = readU16LE(pending, 7);
      tileSize = pending[9];
      const paletteLength = readU16LE(pending, 10);
      const headerLength = 12 + paletteLength * 4;
      if (pending.length < headerLength) return false;

      palette.length = 0;
      for (let i = 0; i < paletteLength; i++) {{
        const src = 12 + i * 4;
        palette.push([pending[src], pending[src + 1], pending[src + 2], pending[src + 3]]);
      }}

      canvas.width = width;
      canvas.height = height;
      overlayCanvas.width = width;
      overlayCanvas.height = height;
      applyStreamDimensions(width, height);
      framebuffer = new Uint8Array(width * height);
      image = ctx.createImageData(width, height);
      tileCache.length = 0;
      streamReady = true;
      streamLoading.hidden = true;
      pending = pending.slice(headerLength);
      drawCustomOverlays();
      return true;
    }}

    function consumeFrames() {{
      if (!readStreamHeader()) return;
      let offset = 0;
      while (pending.length - offset >= 4) {{
        const frameLength = readU32LE(pending, offset);
        if (frameLength === 0) {{
          frameQueue.push(null);
          offset += 4;
          continue;
        }}
        if (pending.length - offset - 4 < frameLength) {{
          break;
        }}

        const frame = pending.slice(offset + 4, offset + 4 + frameLength);
        frameQueue.push(frame);
        offset += 4 + frameLength;
      }}

      pending = pending.slice(offset);
      startPlaybackLoop();
    }}

    function startPlaybackLoop() {{
      if (playbackRunning) return;
      playbackRunning = true;
      nextPlaybackAt = performance.now();
      requestAnimationFrame(playbackTick);
    }}

    function playbackTick(now) {{
      if (!playbackRunning) return;
      if (!streamOnline || !streamReady) {{
        playbackRunning = false;
        playbackStarted = false;
        return;
      }}

      const bufferFrames = Math.max(1, Math.ceil(streamFps * playbackBufferSeconds));
      if (!playbackStarted) {{
        if (frameQueue.length < bufferFrames) {{
          requestAnimationFrame(playbackTick);
          return;
        }}
        playbackStarted = true;
        nextPlaybackAt = now;
      }}

      if (frameQueue.length === 0) {{
        playbackStarted = false;
        nextPlaybackAt = now + frameIntervalMs;
        requestAnimationFrame(playbackTick);
        return;
      }}

      while (frameQueue[0] === null) {{
        tileCache.length = 0;
        frameQueue.shift();
        if (frameQueue.length === 0) {{
          requestAnimationFrame(playbackTick);
          return;
        }}
      }}

      if (now >= nextPlaybackAt) {{
        drawFrame(frameQueue.shift());
        nextPlaybackAt += frameIntervalMs;
        if (now - nextPlaybackAt > frameIntervalMs * 4) {{
          nextPlaybackAt = now + frameIntervalMs;
        }}
      }}

      requestAnimationFrame(playbackTick);
    }}

    function drawFrame(frame) {{
      if (frame.length < 9) {{
        return;
      }}

      const frameType = frame[0];
      const payloadLength = readU32LE(frame, 5);
      const payload = frame.slice(9, 9 + payloadLength);

      if (frameType === 0) {{
        framebuffer.set(payload.slice(0, framebuffer.length));
        seedTileCacheFromFramebuffer();
        renderFramebuffer();
      }} else if (frameType === 1) {{
        applyDelta(payload);
        renderDirtyTiles();
      }} else {{
        return;
      }}
    }}

    function applyDelta(payload) {{
      const tilesX = width / tileSize;
      const tilesY = height / tileSize;
      const tileCount = tilesX * tilesY;
      const maskLength = Math.ceil(tileCount / 8);
      let cursor = maskLength;
      dirtyTileIndices.length = 0;

      for (let tileIndex = 0; tileIndex < tileCount; tileIndex++) {{
        if ((payload[Math.floor(tileIndex / 8)] & (1 << (tileIndex % 8))) === 0) {{
          continue;
        }}

        const tileX = tileIndex % tilesX;
        const tileY = Math.floor(tileIndex / tilesX);
        cursor = decodeTile(payload, cursor, tileX, tileY, tileScratch);
        writeTile(tileX, tileY, tileScratch);
        rememberTile(tileScratch);
        dirtyTileIndices.push(tileIndex);
      }}
    }}

    function decodeTile(bytes, cursor, tileX, tileY, tile) {{
      const mode = bytes[cursor++];
      readTile(tileX, tileY, tile);

      if (mode === 0) {{
        tile.set(bytes.subarray(cursor, cursor + 64));
        cursor += 64;
      }} else if (mode === 1) {{
        tile.fill(bytes[cursor++]);
      }} else if (mode === 2) {{
        cursor = decodeRle(tile, bytes, cursor, false);
      }} else if (mode === 3) {{
        const spanCount = bytes[cursor++];
        let out = 0;
        for (let span = 0; span < spanCount; span++) {{
          out += bytes[cursor++];
          const len = bytes[cursor++];
          tile.set(bytes.subarray(cursor, cursor + len), out);
          cursor += len;
          out += len;
        }}
      }} else if (mode === 4) {{
        cursor = decodeRle(tile, bytes, cursor, true);
      }} else if (mode === 5) {{
        const index = readU16LE(bytes, cursor);
        cursor += 2;
        if (tileCache[index]) {{
          tile.set(tileCache[index]);
        }}
      }}

      return cursor;
    }}

    function rememberTile(tile) {{
      if (tileCache.length >= 4096) return;
      tileCache.push(new Uint8Array(tile));
    }}

    function seedTileCacheFromFramebuffer() {{
      tileCache.length = 0;
      const tilesX = width / tileSize;
      const tilesY = height / tileSize;
      for (let tileY = 0; tileY < tilesY; tileY++) {{
        for (let tileX = 0; tileX < tilesX; tileX++) {{
          readTile(tileX, tileY, tileScratch);
          rememberTile(tileScratch);
        }}
      }}
    }}

    function decodeRle(tile, bytes, cursor, xor) {{
      const runCount = bytes[cursor++];
      let out = 0;
      for (let run = 0; run < runCount; run++) {{
        const value = bytes[cursor++];
        const len = bytes[cursor++];
        for (let i = 0; i < len; i++) {{
          tile[out] = xor ? tile[out] ^ value : value;
          out++;
        }}
      }}
      return cursor;
    }}

    function readTile(tileX, tileY, tile = new Uint8Array(64)) {{
      for (let y = 0; y < tileSize; y++) {{
        for (let x = 0; x < tileSize; x++) {{
          tile[y * tileSize + x] = framebuffer[(tileY * tileSize + y) * width + tileX * tileSize + x];
        }}
      }}
      return tile;
    }}

    function writeTile(tileX, tileY, tile) {{
      for (let y = 0; y < tileSize; y++) {{
        for (let x = 0; x < tileSize; x++) {{
          const pixelIndex = (tileY * tileSize + y) * width + tileX * tileSize + x;
          const paletteIndex = tile[y * tileSize + x];
          framebuffer[pixelIndex] = paletteIndex;
          const color = palette[paletteIndex] || palette[0] || [0, 0, 0, 255];
          const out = pixelIndex * 4;
          image.data[out] = color[0];
          image.data[out + 1] = color[1];
          image.data[out + 2] = color[2];
          image.data[out + 3] = color[3];
        }}
      }}
    }}

    function renderFramebuffer() {{
      for (let i = 0; i < framebuffer.length; i++) {{
        const color = palette[framebuffer[i]] || palette[0] || [0, 0, 0, 255];
        const out = i * 4;
        image.data[out] = color[0];
        image.data[out + 1] = color[1];
        image.data[out + 2] = color[2];
        image.data[out + 3] = color[3];
      }}
      ctx.putImageData(image, 0, 0);
    }}

    function renderDirtyTiles() {{
      for (let i = 0; i < dirtyTileIndices.length; i++) {{
        const tileIndex = dirtyTileIndices[i];
        const tileX = tileIndex % (width / tileSize);
        const tileY = Math.floor(tileIndex / (width / tileSize));
        ctx.putImageData(
          image,
          0,
          0,
          tileX * tileSize,
          tileY * tileSize,
          tileSize,
          tileSize
        );
      }}
    }}

    async function connect() {{
      while (true) {{
        try {{
          while (!streamOnline) {{
            await new Promise(resolve => setTimeout(resolve, 250));
          }}
          while (streamOnline && !serverStreamReady) {{
            streamLoading.hidden = false;
            await new Promise(resolve => setTimeout(resolve, 100));
          }}
          if (!streamOnline) {{
            continue;
          }}
          resetStreamState();
          const response = await fetch("{palette_stream_url}?t=" + Date.now(), {{ cache: "no-store" }});
          if (!response.ok) {{
            throw new Error(`stream failed: ${{response.status}}`);
          }}
          setStreamTiming(
            Number(response.headers.get("X-Stream-Fps")),
            Number(response.headers.get("X-Playback-Buffer-Seconds")),
          );
          const reader = response.body.getReader();
          while (true) {{
            const {{ value, done }} = await reader.read();
            if (done) break;
            pending = appendBytes(pending, value);
            consumeFrames();
            if (!streamOnline) break;
          }}
        }} catch (error) {{
          console.error(error);
        }}
        await new Promise(resolve => setTimeout(resolve, 500));
      }}
    }}

    function queuedAudioFrames() {{
      let frames = 0;
      for (let i = 0; i < audioQueue.length; i++) {{
        frames += audioQueue[i].length / 2;
      }}
      return Math.max(0, frames - audioQueueOffset);
    }}

    function dequeueAudioFrame() {{
      while (audioQueue.length > 0) {{
        const chunk = audioQueue[0];
        if (audioQueueOffset * 2 + 1 < chunk.length) {{
          const left = chunk[audioQueueOffset * 2];
          const right = chunk[audioQueueOffset * 2 + 1];
          audioQueueOffset++;
          if (audioQueueOffset * 2 >= chunk.length) {{
            audioQueue.shift();
            audioQueueOffset = 0;
          }}
          return [left, right];
        }}
        audioQueue.shift();
        audioQueueOffset = 0;
      }}
      return null;
    }}

    function enqueueAudioBytes(bytes) {{
      const samples = new Float32Array(bytes.byteLength * audioUpsampleFactor * 2);
      let out = 0;
      for (let i = 0; i < bytes.byteLength; i++) {{
        const current = mulawToLinear(bytes[i]);
        const previous = lastTransportSample === null ? current : lastTransportSample;
        for (let step = 0; step < audioUpsampleFactor; step++) {{
          const t = (step + 1) / audioUpsampleFactor;
          const sample = previous + (current - previous) * t;
          samples[out++] = sample;
          samples[out++] = sample;
        }}
        lastTransportSample = current;
      }}
      if (audioNode) {{
        audioNode.port.postMessage(samples, [samples.buffer]);
        return;
      }}
      audioQueue.push(samples);

      const maxBufferedFrames = 48000 * 2;
      while (queuedAudioFrames() > maxBufferedFrames && audioQueue.length > 1) {{
        audioQueue.shift();
        audioQueueOffset = 0;
      }}
    }}

    function mulawToLinear(byte) {{
      const magnitude = byte & 0x7f;
      const sign = (byte & 0x80) ? -1 : 1;
      const normalized = (Math.pow(256, magnitude / 127) - 1) / 255;
      return sign * normalized;
    }}

    async function createAudioNode() {{
      const workletSource = `
        class PcmStreamProcessor extends AudioWorkletProcessor {{
          constructor() {{
            super();
            this.queue = [];
            this.offset = 0;
            this.primed = false;
            this.lastLeft = 0;
            this.lastRight = 0;
            this.port.onmessage = event => this.queue.push(event.data);
          }}

          queuedFrames() {{
            let frames = 0;
            for (let i = 0; i < this.queue.length; i++) frames += this.queue[i].length / 2;
            return Math.max(0, frames - this.offset);
          }}

          nextFrame() {{
            while (this.queue.length > 0) {{
              const chunk = this.queue[0];
              if (this.offset * 2 + 1 < chunk.length) {{
                const left = chunk[this.offset * 2];
                const right = chunk[this.offset * 2 + 1];
                this.offset++;
                if (this.offset * 2 >= chunk.length) {{
                  this.queue.shift();
                  this.offset = 0;
                }}
                return [left, right];
              }}
              this.queue.shift();
              this.offset = 0;
            }}
            return null;
          }}

          process(inputs, outputs) {{
            const output = outputs[0];
            const left = output[0];
            const right = output[1] || output[0];
            if (!this.primed) {{
              if (this.queuedFrames() < 24000) {{
                left.fill(0);
                right.fill(0);
                return true;
              }}
              this.primed = true;
            }}

            for (let i = 0; i < left.length; i++) {{
              const frame = this.nextFrame();
              if (frame) {{
                this.lastLeft = frame[0];
                this.lastRight = frame[1];
                left[i] = frame[0];
                right[i] = frame[1];
              }} else {{
                const fade = 1 - i / left.length;
                left[i] = this.lastLeft * fade;
                right[i] = this.lastRight * fade;
                if (i === left.length - 1) {{
                  this.lastLeft = 0;
                  this.lastRight = 0;
                  this.primed = false;
                }}
              }}
            }}
            return true;
          }}
        }}
        registerProcessor("pcm-stream-processor", PcmStreamProcessor);
      `;
      const url = URL.createObjectURL(new Blob([workletSource], {{ type: "text/javascript" }}));
      try {{
        await audioContext.audioWorklet.addModule(url);
        const node = new AudioWorkletNode(audioContext, "pcm-stream-processor", {{
          numberOfOutputs: 1,
          outputChannelCount: [2],
        }});
        node.connect(audioGain);
        return node;
      }} finally {{
        URL.revokeObjectURL(url);
      }}
    }}

    function createFallbackAudioNode() {{
      const processor = audioContext.createScriptProcessor(4096, 0, 2);
      processor.onaudioprocess = event => {{
        const left = event.outputBuffer.getChannelData(0);
        const right = event.outputBuffer.getChannelData(1);
        if (!audioPrimed) {{
          if (queuedAudioFrames() < 48000 * 0.75) {{
            left.fill(0);
            right.fill(0);
            return;
          }}
          audioPrimed = true;
        }}
        for (let i = 0; i < left.length; i++) {{
          const frame = dequeueAudioFrame();
          if (frame) {{
            lastAudioLeft = frame[0];
            lastAudioRight = frame[1];
            left[i] = frame[0];
            right[i] = frame[1];
          }} else {{
            const fade = 1 - i / left.length;
            left[i] = lastAudioLeft * fade;
            right[i] = lastAudioRight * fade;
            if (i === left.length - 1) {{
              lastAudioLeft = 0;
              lastAudioRight = 0;
              audioPrimed = false;
            }}
          }}
        }}
      }};
      processor.connect(audioGain);
      return processor;
    }}

    function setMuted(muted) {{
      audioMuted = muted;
      if (audioGain && audioContext) {{
        const target = muted ? 0 : audioVolume;
        audioGain.gain.cancelScheduledValues(audioContext.currentTime);
        audioGain.gain.linearRampToValueAtTime(target, audioContext.currentTime + 0.025);
      }}
      unmuteButton.hidden = !muted;
      syncPlayerControls();
      updateUnmuteOverlay();
    }}

    function updateUnmuteOverlay() {{
      if (!audioMuted) return;
      unmuteButton.textContent = streamOnline ? "Click to unmute" : "Not Online";
      unmuteButton.disabled = !streamOnline;
      syncPlayerControls();
    }}

    function syncPlayerControls() {{
      playerControls.hidden = false;
      muteButton.textContent = audioMuted ? "Unmute" : "Mute";
      muteButton.disabled = audioMuted && !streamOnline;
      volumeSlider.disabled = audioMuted && !streamOnline;
    }}

    async function pollStreamStatus() {{
      while (true) {{
        try {{
          const response = await fetch("{stream_status_url}?t=" + Date.now(), {{ cache: "no-store" }});
          const status = await response.json();
          statusPollDelayMs = 500;
          streamOnline = !!status.online;
          serverStreamReady = !!status.stream_ready;
          serverSessionToken = typeof status.session_token === "string" ? status.session_token : "";
          streamLoading.hidden = !streamOnline || (serverStreamReady && streamReady);
          setRequestedUiVisible(streamOnline && !!(status.chat_panel && status.chat_panel.requested), status.chat_panel);
          applyRuntimeBranding(status);
          if (streamOnline) {{
            applyStreamDimensions(status.width, status.height);
          }}
          updateUnmuteOverlay();
        }} catch (error) {{
          streamOnline = false;
          serverStreamReady = false;
          serverSessionToken = "";
          streamLoading.hidden = true;
          setRequestedUiVisible(false, null);
          updateUnmuteOverlay();
          statusPollDelayMs = Math.min(10000, Math.max(1000, Math.round(statusPollDelayMs * 1.7)));
        }}
        await new Promise(resolve => setTimeout(resolve, statusPollDelayMs));
      }}
    }}

    function applyRuntimeBranding(status) {{
      const branding = status && status.branding;
      if (!branding) return;
      if (typeof branding.page_title === "string" && branding.page_title && document.title !== branding.page_title) {{
        document.title = branding.page_title;
      }}
      if (typeof branding.header_title === "string" && branding.header_title && pageHeaderTitle.textContent !== branding.header_title) {{
        pageHeaderTitle.textContent = branding.header_title;
      }}
      if (status.version && status.version !== staticBranding.version) {{
        document.documentElement.dataset.directStreamVersionMismatch = "true";
      }}
    }}

    async function runAudioStreamLoop() {{
      if (audioLoopRunning) return;
      audioLoopRunning = true;
      while (audioShouldReconnect) {{
        try {{
          const response = await fetch("{audio_stream_url}?t=" + Date.now(), {{ cache: "no-store" }});
          const reader = response.body.getReader();
          let receivedAudio = false;
          while (true) {{
            const {{ value, done }} = await reader.read();
            if (done) break;
            receivedAudio = true;
            enqueueAudioBytes(value);
          }}
          if (!receivedAudio) {{
            audioShouldReconnect = false;
            audioQueue = [];
            audioQueueOffset = 0;
            audioPrimed = false;
            lastTransportSample = null;
            setMuted(true);
            break;
          }}
        }} catch (error) {{
          console.error(error);
          audioPrimed = false;
        }}
        await new Promise(resolve => setTimeout(resolve, 500));
      }}
      audioLoopRunning = false;
    }}

    async function startAudio() {{
      if (audioContext) {{
        await audioContext.resume();
      }} else {{
        audioContext = new AudioContext({{ sampleRate: 48000 }});
        audioGain = audioContext.createGain();
        audioGain.gain.value = 0;
        try {{
          audioNode = await createAudioNode();
          console.info("Direct Stream audio: using AudioWorklet");
        }} catch (error) {{
          console.error(error);
          fallbackProcessor = createFallbackAudioNode();
          console.info("Direct Stream audio: using ScriptProcessor fallback");
        }}
        audioGain.connect(audioContext.destination);
        audioStarted = true;
      }}

      audioQueue = [];
      audioQueueOffset = 0;
      audioPrimed = false;
      lastTransportSample = null;
      audioShouldReconnect = true;
      setMuted(false);
      runAudioStreamLoop();
    }}

    unmuteButton.addEventListener("click", () => {{
      startAudio().catch(error => {{
        console.error(error);
        unmuteButton.hidden = false;
      }});
    }});

    muteButton.addEventListener("click", () => {{
      if (audioMuted) {{
        startAudio().catch(error => {{
          console.error(error);
          unmuteButton.hidden = false;
        }});
        return;
      }}
      muteAudio();
    }});

    function muteAudio() {{
      audioQueue = [];
      audioQueueOffset = 0;
      audioPrimed = false;
      lastTransportSample = null;
      setMuted(true);
    }}

    volumeSlider.addEventListener("input", () => {{
      audioVolume = Number(volumeSlider.value);
      if (audioStarted && !audioMuted) {{
        setMuted(false);
      }}
    }});

    canvas.addEventListener("click", event => {{
      if (!width || !height) return;
      const renderedRect = getRenderedStreamRect(canvas, width, height);
      const streamPoint = mapClientPointToStreamPixel(
        event.clientX,
        event.clientY,
        renderedRect,
        width,
        height
      );
      if (!streamPoint) return;
      fetch("{stream_click_url}", sessionFetchOptions({{
        method: "POST",
        headers: {{
          "Content-Type": "application/json",
        }},
        body: JSON.stringify({{
          client_x: event.clientX,
          client_y: event.clientY,
          x: streamPoint.x,
          y: streamPoint.y,
          normalized_x: streamPoint.normalizedX,
          normalized_y: streamPoint.normalizedY,
        }}),
        cache: "no-store",
      }})).catch(error => console.error(error));
    }});

    function getRenderedStreamRect(canvasElement, streamWidth, streamHeight) {{
      const rect = canvasElement.getBoundingClientRect();
      const style = getComputedStyle(canvasElement);
      const borderLeft = cssPixels(style.borderLeftWidth);
      const borderRight = cssPixels(style.borderRightWidth);
      const borderTop = cssPixels(style.borderTopWidth);
      const borderBottom = cssPixels(style.borderBottomWidth);
      const paddingLeft = cssPixels(style.paddingLeft);
      const paddingRight = cssPixels(style.paddingRight);
      const paddingTop = cssPixels(style.paddingTop);
      const paddingBottom = cssPixels(style.paddingBottom);
      const contentLeft = rect.left + borderLeft + paddingLeft;
      const contentTop = rect.top + borderTop + paddingTop;
      const contentWidth = Math.max(0, rect.width - borderLeft - borderRight - paddingLeft - paddingRight);
      const contentHeight = Math.max(0, rect.height - borderTop - borderBottom - paddingTop - paddingBottom);
      if (!streamWidth || !streamHeight || !contentWidth || !contentHeight) {{
        return {{ left: contentLeft, top: contentTop, width: 0, height: 0 }};
      }}

      const streamAspect = streamWidth / streamHeight;
      const contentAspect = contentWidth / contentHeight;
      let renderedWidth = contentWidth;
      let renderedHeight = contentHeight;
      let left = contentLeft;
      let top = contentTop;
      if (contentAspect > streamAspect) {{
        renderedHeight = contentHeight;
        renderedWidth = renderedHeight * streamAspect;
        left += (contentWidth - renderedWidth) * 0.5;
      }} else if (contentAspect < streamAspect) {{
        renderedWidth = contentWidth;
        renderedHeight = renderedWidth / streamAspect;
        top += (contentHeight - renderedHeight) * 0.5;
      }}
      return {{ left, top, width: renderedWidth, height: renderedHeight }};
    }}

    function mapClientPointToStreamPixel(clientX, clientY, renderedRect, streamWidth, streamHeight) {{
      if (!renderedRect.width || !renderedRect.height || !streamWidth || !streamHeight) return null;
      const localX = clientX - renderedRect.left;
      const localY = clientY - renderedRect.top;
      if (localX < 0 || localY < 0 || localX > renderedRect.width || localY > renderedRect.height) {{
        return null;
      }}
      const u = Math.min(1, Math.max(0, localX / renderedRect.width));
      const v = Math.min(1, Math.max(0, localY / renderedRect.height));
      const x = Math.min(streamWidth - 1, Math.max(0, Math.floor(u * streamWidth)));
      const y = Math.min(streamHeight - 1, Math.max(0, Math.floor(v * streamHeight)));
      return {{
        x,
        y,
        normalizedX: streamWidth > 1 ? x / (streamWidth - 1) : 0,
        normalizedY: streamHeight > 1 ? y / (streamHeight - 1) : 0,
      }};
    }}

    function cssPixels(value) {{
      const parsed = Number.parseFloat(value);
      return Number.isFinite(parsed) ? parsed : 0;
    }}

    function submitLocalChat(event) {{
      event.preventDefault();
      if (!chatInput) return;
      const message = chatInput.value.trim();
      if (!message) return;
      chatInput.value = "";
      fetch("{local_chat_url}", sessionFetchOptions({{
        method: "POST",
        headers: {{
          "Content-Type": "text/plain;charset=utf-8",
        }},
        body: message,
        cache: "no-store",
      }}))
        .then(response => response.ok ? response.json() : Promise.reject(new Error(`chat failed: ${{response.status}}`)))
        .then(() => fetchChatFeed())
        .catch(error => {{
          console.error(error);
          appendSystemLine("chat send failed");
        }});
    }}

    async function fetchChatFeed() {{
      if (!streamOnline || !requestedUiVisible) return;
      const response = await fetch("{local_chat_feed_url}?after=" + lastChatId + "&t=" + Date.now(), identityFetchOptions({{ cache: "no-store" }}));
      if (!response.ok) {{
        throw new Error(`chat feed failed: ${{response.status}}`);
      }}
      const feed = await response.json();
      if (Number.isFinite(feed.latest) && feed.latest < lastChatId) {{
        lastChatId = 0;
        shownChatIds.clear();
        clearChatLog();
      }}
      if (feed.viewer) {{
        currentViewer = feed.viewer;
      }}
      if (Number.isFinite(feed.generation) && chatGeneration !== feed.generation) {{
        chatGeneration = feed.generation;
        lastChatId = 0;
        shownChatIds.clear();
        clearChatLog();
      }}
      if (Array.isArray(feed.messages)) {{
        for (const message of feed.messages) {{
          if (shownChatIds.has(message.id)) continue;
          shownChatIds.add(message.id);
          appendChatLine(message);
          lastChatId = Math.max(lastChatId, Number(message.id) || lastChatId);
        }}
      }}
      if (Number.isFinite(feed.latest)) {{
        lastChatId = Math.max(lastChatId, feed.latest);
      }}
    }}

    async function pollChatFeed() {{
      while (true) {{
        if (streamOnline && requestedUiVisible) {{
          try {{
            await fetchChatFeed();
          }} catch (error) {{
            console.error(error);
            appendSystemLine("chat feed offline");
          }}
        }}
        await new Promise(resolve => setTimeout(resolve, 650));
      }}
    }}

    async function fetchCustomPanels() {{
      if (!streamOnline) return;
      const response = await fetch("{custom_panels_url}?t=" + Date.now(), identityFetchOptions({{ cache: "no-store" }}));
      if (!response.ok) {{
        throw new Error(`panel fetch failed: ${{response.status}}`);
      }}
      const data = await response.json();
      renderPanels(Array.isArray(data.panels) ? data.panels : []);
    }}

    async function fetchCustomOverlays() {{
      if (!streamOnline) return;
      const response = await fetch("{custom_overlays_url}?t=" + Date.now(), identityFetchOptions({{ cache: "no-store" }}));
      if (!response.ok) {{
        throw new Error(`overlay fetch failed: ${{response.status}}`);
      }}
      const data = await response.json();
      currentOverlays = Array.isArray(data.overlays) ? data.overlays : [];
      drawCustomOverlays();
    }}

    async function pollCustomPanels() {{
      while (true) {{
        if (streamOnline) {{
          try {{
            await fetchCustomPanels();
          }} catch (error) {{
            console.error(error);
            clearCustomPanels();
          }}
        }}
        await new Promise(resolve => setTimeout(resolve, 1000));
      }}
    }}

    async function pollCustomOverlays() {{
      while (true) {{
        if (streamOnline) {{
          try {{
            await fetchCustomOverlays();
          }} catch (error) {{
            console.error(error);
            clearCustomOverlays();
          }}
        }}
        await new Promise(resolve => setTimeout(resolve, 250));
      }}
    }}

    function setRequestedUiVisible(visible, chatPanelStatus) {{
      if (visible) {{
        ensureChatPanel(chatPanelStatus);
      }}
      if (requestedUiVisible === visible) {{
        if (visible) updateChatPanelTitle(chatPanelStatus);
        return;
      }}
      requestedUiVisible = visible;
      if (!visible) {{
        clearCustomHostUi();
        removeChatPanel();
      }} else {{
        updateChatPanelTitle(chatPanelStatus);
        clearChatLog();
      }}
    }}

    function clearCustomHostUi() {{
      clearChatLog();
      clearCustomPanels();
      clearCustomOverlays();
    }}

    function clearCustomPanels() {{
      panelPageState.clear();
      renderPanels([]);
    }}

    function clearCustomOverlays() {{
      currentOverlays = [];
      drawCustomOverlays();
    }}

    function ensureChatPanel(chatPanelStatus) {{
      if (chatPanel) {{
        updateChatPanelTitle(chatPanelStatus);
        return;
      }}
      chatPanel = document.createElement("aside");
      chatPanel.className = "chat";
      const title = document.createElement("h2");
      title.dataset.role = "chat-title";
      title.textContent = chatPanelTitle(chatPanelStatus);
      chatLog = document.createElement("div");
      chatLog.className = "chat-log";
      chatForm = document.createElement("form");
      chatForm.className = "chat-input";
      chatInput = document.createElement("input");
      chatInput.autocomplete = "off";
      chatInput.placeholder = "chat message";
      const sendButton = document.createElement("button");
      sendButton.type = "submit";
      sendButton.textContent = "Send";
      chatForm.appendChild(chatInput);
      chatForm.appendChild(sendButton);
      chatForm.addEventListener("submit", submitLocalChat);
      chatPanel.appendChild(title);
      chatPanel.appendChild(chatLog);
      chatPanel.appendChild(chatForm);
      rightRegion.insertBefore(chatPanel, rightPanels);
      updateStageUiClasses();
    }}

    function removeChatPanel() {{
      if (chatForm) chatForm.removeEventListener("submit", submitLocalChat);
      if (chatPanel) chatPanel.remove();
      chatPanel = null;
      chatForm = null;
      chatInput = null;
      chatLog = null;
      lastChatId = 0;
      chatGeneration = null;
      shownChatIds.clear();
      updateStageUiClasses();
    }}

    function updateChatPanelTitle(chatPanelStatus) {{
      if (!chatPanel) return;
      const title = chatPanel.querySelector("[data-role='chat-title']");
      if (title) title.textContent = chatPanelTitle(chatPanelStatus);
    }}

    function chatPanelTitle(chatPanelStatus) {{
      const title = chatPanelStatus && typeof chatPanelStatus.title === "string" ? chatPanelStatus.title.trim() : "";
      return title || "Chat";
    }}

    function appendSystemLine(message) {{
      if (!chatLog) return;
      const last = chatLog.lastElementChild;
      if (last && last.dataset.system === message) return;
      const stickToBottom = chatIsNearBottom();
      const row = document.createElement("p");
      row.dataset.system = message;
      const name = document.createElement("strong");
      name.textContent = "system";
      row.appendChild(name);
      row.append(" " + message);
      chatLog.appendChild(row);
      if (stickToBottom) scrollChatToBottom();
    }}

    function clearChatLog() {{
      if (!chatLog) return;
      chatLog.textContent = "";
      appendSystemLine("chat panel ready");
    }}

    function appendChatLine(message) {{
      if (!chatLog) return;
      const stickToBottom = chatIsNearBottom();
      const row = document.createElement("p");
      row.dataset.chatId = String(message.id || "");
      for (const className of safeCssClasses(message.css_class)) {{
        row.classList.add(className);
      }}
      const name = document.createElement("strong");
      name.textContent = message.name || "unknown";
      const nameColor = safeCssColor(message.display_name_color);
      if (nameColor) {{
        name.style.color = nameColor;
      }}
      const text = document.createElement("span");
      text.textContent = " " + (message.text || "");
      const messageColor = safeCssColor(message.message_color);
      if (messageColor) {{
        text.style.color = messageColor;
      }}
      row.appendChild(name);
      row.appendChild(text);
      if (isMentionedMe(message)) {{
        row.classList.add("mentioned-me");
      }}
      chatLog.appendChild(row);
      if (stickToBottom) scrollChatToBottom();
      if (Number.isFinite(message.expires_at_ms)) {{
        const delay = Math.max(0, message.expires_at_ms - Date.now());
        setTimeout(() => row.remove(), delay);
      }}
    }}

    function chatIsNearBottom() {{
      if (!chatLog) return true;
      return chatLog.scrollHeight - chatLog.scrollTop - chatLog.clientHeight < 32;
    }}

    function scrollChatToBottom() {{
      if (!chatLog) return;
      chatLog.scrollTop = chatLog.scrollHeight;
    }}

    function isMentionedMe(message) {{
      if (!currentViewer || !Array.isArray(message.mentions)) return false;
      const names = [
        currentViewer.name || "",
        currentViewer.identity || "",
      ].map(value => value.toLowerCase()).filter(Boolean);
      return message.mentions.some(mention => names.includes(String(mention).toLowerCase()));
    }}

    function renderPanels(panels) {{
      leftPanels.textContent = "";
      rightPanels.textContent = "";
      abovePanels.textContent = "";
      belowPanels.textContent = "";
      overlayTopLeftPanels.textContent = "";
      overlayTopRightPanels.textContent = "";
      overlayBottomLeftPanels.textContent = "";
      overlayBottomRightPanels.textContent = "";
      clearPanelRegionClasses();
      namedPanelContainers.clear();
      updateSideColumnWidths(panels);
      panels
        .slice()
        .sort((a, b) => {{
          const anchorCompare = panelAnchorSortKey(a).localeCompare(panelAnchorSortKey(b));
          return anchorCompare || (Number(a.order) || 0) - (Number(b.order) || 0) || String(a.id || "").localeCompare(String(b.id || ""));
        }})
        .forEach(panel => {{
        const section = document.createElement("article");
        section.className = "panel";
        const styleHint = panel.style_hint || {{}};
        for (const className of safeCssClasses(panel.style_hint && panel.style_hint.css_class)) {{
          section.classList.add(className);
        }}
        if (styleHint.hide_header) {{
          section.classList.add("panel-headerless");
        }}
        if (styleHint.overflow_mode === "WrapNoScroll") {{
          section.classList.add("panel-wrap-no-scroll");
        }}
        if (styleHint.body_white_space === "NoWrap") {{
          section.classList.add("panel-nowrap");
        }} else {{
          section.classList.add("panel-pre-wrap");
        }}
        applyPanelSizeHint(section, panel.size_hint);
        const title = document.createElement("h2");
        title.textContent = panel.title || panel.id || "Panel";
        if (!styleHint.hide_header) {{
          section.appendChild(title);
        }}
        section.appendChild(renderPanelBody(panel));
        const container = panelContainer(panel.anchor || panel.region);
        applyPanelRegionStyle(container, styleHint);
        container.appendChild(section);
      }});
      updateStageUiClasses();
    }}

    function updateSideColumnWidths(panels) {{
      stage.style.setProperty("--stage-left-column", `${{panelColumnWidth(panels, "LeftOfStream", 320, false)}}px`);
      stage.style.setProperty("--stage-right-column", `${{panelColumnWidth(panels, "RightOfStream", 320, !!chatPanel)}}px`);
    }}

    function panelColumnWidth(panels, anchorName, fallbackWidth, hasImplicitPanel) {{
      let hasPanel = !!hasImplicitPanel;
      let width = hasImplicitPanel ? fallbackWidth : 0;
      for (const panel of Array.isArray(panels) ? panels : []) {{
        if (panelAnchorName(panel) !== anchorName) continue;
        hasPanel = true;
        width = Math.max(width || fallbackWidth, panelPreferredWidth(panel.size_hint, fallbackWidth));
      }}
      return hasPanel ? Math.max(1, Math.round(width || fallbackWidth)) : 0;
    }}

    function panelAnchorName(panel) {{
      return String((panel && (panel.anchor || panel.region)) || "RightOfStream");
    }}

    function panelPreferredWidth(sizeHint, fallbackWidth) {{
      if (!sizeHint || typeof sizeHint !== "object") return fallbackWidth;
      const minWidth = Number(sizeHint.min_width_px);
      const maxWidth = Number(sizeHint.max_width_px);
      let width = fallbackWidth;
      if (Number.isFinite(minWidth) && minWidth > 0) {{
        width = Math.max(width, minWidth);
      }}
      if (Number.isFinite(maxWidth) && maxWidth > 0 && (!Number.isFinite(minWidth) || minWidth <= 0)) {{
        width = Math.max(width, Math.min(maxWidth, fallbackWidth));
      }}
      if (Number.isFinite(maxWidth) && maxWidth > 0) {{
        width = Math.min(width, maxWidth);
      }}
      return Math.max(1, Math.min(2000, width));
    }}

    function renderPanelBody(panel) {{
      const elements = Array.isArray(panel.elements) ? panel.elements : [];
      if (elements.length === 0) {{
        const body = document.createElement("pre");
        body.textContent = panel.body || "";
        return body;
      }}

      const body = document.createElement("div");
      body.className = "panel-content";
      for (const element of elements) {{
        if (!element || typeof element !== "object") continue;
        if (element.type === "Button") {{
          const button = document.createElement("button");
          button.className = "panel-button";
          button.type = "button";
          button.textContent = String(element.label || element.action_id || "Action");
          button.disabled = Boolean(element.disabled);
          button.addEventListener("click", () => {{
            submitPanelAction(panel.id || "", element.action_id || "");
          }});
          body.appendChild(button);
        }} else if (element.type === "PagedText") {{
          body.appendChild(renderPagedText(panel, element));
        }} else if (element.type === "StyledText") {{
          body.appendChild(renderStyledPanelText(element));
        }} else {{
          const text = document.createElement("span");
          text.className = "panel-text";
          text.textContent = String(element.text || "");
          body.appendChild(text);
        }}
      }}
      return body;
    }}

    function renderStyledPanelText(element) {{
      const text = document.createElement("span");
      text.className = "panel-text panel-styled-text";
      text.textContent = String(element.text || "");
      const style = element.style || {{}};
      for (const className of safeCssClasses(style.css_class)) {{
        text.classList.add(className);
      }}
      const color = safeCssColor(style.text_color);
      if (color) {{
        text.style.color = color;
      }}
      const fontWeight = safeCssFontWeight(style.font_weight);
      if (fontWeight) {{
        text.style.fontWeight = fontWeight;
      }}
      return text;
    }}

    function renderPagedText(panel, element) {{
      const pages = Array.isArray(element.pages) ? element.pages : [];
      const wrapper = document.createElement("div");
      wrapper.className = "panel-paged-text";
      if (pages.length === 0) return wrapper;

      const key = `${{panel.id || ""}}\u001f${{element.id || ""}}`;
      const initialPage = clampInteger(element.initial_page, 0, pages.length - 1, 0);
      const storedPage = panelPageState.has(key) ? panelPageState.get(key) : initialPage;
      let pageIndex = clampInteger(storedPage, 0, pages.length - 1, initialPage);
      panelPageState.set(key, pageIndex);

      const title = document.createElement("div");
      title.className = "panel-paged-title";
      const body = document.createElement("span");
      body.className = "panel-text panel-paged-body";
      const controls = document.createElement("div");
      controls.className = "panel-page-controls";
      const previous = document.createElement("button");
      previous.className = "panel-button panel-page-button";
      previous.type = "button";
      previous.textContent = String(element.controls && element.controls.previous_label || "<");
      const indicator = document.createElement("span");
      indicator.className = "panel-page-indicator";
      const next = document.createElement("button");
      next.className = "panel-button panel-page-button";
      next.type = "button";
      next.textContent = String(element.controls && element.controls.next_label || ">");
      const wrap = Boolean(element.controls && element.controls.wrap);
      const showIndicator = !element.controls || element.controls.show_page_indicator !== false;
      const controlsBeforePage = element.controls && element.controls.position === "BeforePage";

      function setPage(nextIndex) {{
        if (wrap) {{
          pageIndex = ((nextIndex % pages.length) + pages.length) % pages.length;
        }} else {{
          pageIndex = clampInteger(nextIndex, 0, pages.length - 1, pageIndex);
        }}
        panelPageState.set(key, pageIndex);
        renderPage();
      }}

      function renderPage() {{
        const page = pages[pageIndex] || {{}};
        const pageTitle = String(page.title || "");
        title.textContent = pageTitle;
        title.hidden = !pageTitle;
        body.textContent = String(page.body || "");
        previous.disabled = !wrap && pageIndex <= 0;
        next.disabled = !wrap && pageIndex >= pages.length - 1;
        indicator.textContent = `${{pageIndex + 1}} / ${{pages.length}}`;
        indicator.hidden = !showIndicator;
      }}

      previous.addEventListener("click", () => setPage(pageIndex - 1));
      next.addEventListener("click", () => setPage(pageIndex + 1));
      controls.appendChild(previous);
      if (showIndicator) controls.appendChild(indicator);
      controls.appendChild(next);
      if (pages.length > 1 && controlsBeforePage) wrapper.appendChild(controls);
      wrapper.appendChild(title);
      wrapper.appendChild(body);
      if (pages.length > 1 && !controlsBeforePage) wrapper.appendChild(controls);
      renderPage();
      return wrapper;
    }}

    function clampInteger(value, min, max, fallback) {{
      const number = Number(value);
      if (!Number.isFinite(number)) return fallback;
      return Math.min(max, Math.max(min, Math.trunc(number)));
    }}

    function submitPanelAction(panelId, actionId) {{
      if (!panelId || !actionId) return;
      fetch("{custom_panel_action_url}", sessionFetchOptions({{
        method: "POST",
        headers: {{
          "Content-Type": "application/json",
        }},
        body: JSON.stringify({{
          panel_id: panelId,
          action_id: actionId,
        }}),
        cache: "no-store",
      }})).catch(error => console.error(error));
    }}

    function drawCustomOverlays() {{
      overlayCtx.clearRect(0, 0, overlayCanvas.width, overlayCanvas.height);
      if (!overlayCanvas.width || !overlayCanvas.height) return;
      const overlays = currentOverlays
        .slice()
        .sort((a, b) => (Number(a.order) || 0) - (Number(b.order) || 0) || String(a.id || "").localeCompare(String(b.id || "")));
      for (const overlay of overlays) {{
        drawCustomOverlay(overlay);
      }}
    }}

    function drawCustomOverlay(overlay) {{
      const point = overlayPoint(overlay);
      const kind = overlay.kind || {{}};
      const style = overlay.style || {{}};
      const stroke = safeCssColor(style.stroke_color) || '#ff3b30';
      const fill = safeCssColor(style.fill_color);
      const textColor = safeCssColor(style.text_color) || '#ffffff';
      const lineWidth = clampNumber(style.line_width, 1, 32, 2);
      overlayCtx.save();
      overlayCtx.lineWidth = lineWidth;
      overlayCtx.strokeStyle = stroke;
      overlayCtx.fillStyle = fill || stroke;
      overlayCtx.font = `${{clampNumber(style.font_px, 6, 96, 12)}}px Consolas, monospace`;
      overlayCtx.textBaseline = "middle";
      overlayCtx.textAlign = "center";

      if (kind.type === "Circle") {{
        overlayCtx.beginPath();
        overlayCtx.arc(point.x, point.y, clampNumber(kind.radius, 1, 4096, 8), 0, Math.PI * 2);
        if (fill) overlayCtx.fill();
        overlayCtx.stroke();
      }} else if (kind.type === "Flag") {{
        drawFlag(point.x, point.y, clampNumber(kind.width, 1, 4096, 10), clampNumber(kind.height, 1, 4096, 14), fill);
      }} else if (kind.type === "Text") {{
        overlayCtx.fillStyle = textColor;
        overlayCtx.strokeStyle = stroke;
        overlayCtx.lineWidth = Math.max(1, lineWidth);
        const text = String(kind.text || "");
        overlayCtx.strokeText(text, point.x, point.y);
        overlayCtx.fillText(text, point.x, point.y);
      }} else if (kind.type === "Sprite") {{
        drawOverlaySprite(kind, point.x, point.y);
      }}
      overlayCtx.restore();
    }}

    function overlayPoint(overlay) {{
      const x = Number(overlay.x) || 0;
      const y = Number(overlay.y) || 0;
      if (overlay.coordinate_space === "StreamPixels") {{
        return {{ x, y }};
      }}
      return {{
        x: x * overlayCanvas.width,
        y: y * overlayCanvas.height,
      }};
    }}

    function drawFlag(x, y, flagWidth, flagHeight, fill) {{
      const poleHeight = flagHeight;
      overlayCtx.beginPath();
      overlayCtx.moveTo(x, y);
      overlayCtx.lineTo(x, y - poleHeight);
      overlayCtx.stroke();
      overlayCtx.beginPath();
      overlayCtx.moveTo(x, y - poleHeight);
      overlayCtx.lineTo(x + flagWidth, y - poleHeight + flagHeight * 0.25);
      overlayCtx.lineTo(x, y - poleHeight + flagHeight * 0.5);
      overlayCtx.closePath();
      if (fill) overlayCtx.fill();
      overlayCtx.stroke();
    }}

    function drawOverlaySprite(kind, x, y) {{
      const imageId = String(kind.image_id || "");
      const spriteWidth = clampNumber(kind.width, 1, 4096, 16);
      const spriteHeight = clampNumber(kind.height, 1, 4096, 16);
      const image = overlayImage(imageId);
      if (image && image.complete && image.naturalWidth > 0) {{
        overlayCtx.drawImage(image, x - spriteWidth * 0.5, y - spriteHeight * 0.5, spriteWidth, spriteHeight);
      }} else {{
        overlayCtx.strokeRect(x - spriteWidth * 0.5, y - spriteHeight * 0.5, spriteWidth, spriteHeight);
      }}
    }}

    function overlayImage(imageId) {{
      if (!imageId) return null;
      if (overlayImageCache.has(imageId)) return overlayImageCache.get(imageId);
      const image = new Image();
      image.crossOrigin = "anonymous";
      image.onload = () => drawCustomOverlays();
      image.src = imageId;
      overlayImageCache.set(imageId, image);
      return image;
    }}

    function clampNumber(value, min, max, fallback) {{
      const number = Number(value);
      if (!Number.isFinite(number)) return fallback;
      return Math.min(max, Math.max(min, number));
    }}

    function panelAnchorSortKey(panel) {{
      const anchor = String(panel.anchor || panel.region || "RightOfStream");
      const order = {{
        LeftOfStream: "0",
        RightOfStream: "1",
        SidePanelDefault: "1",
        AboveStream: "2",
        BelowStream: "3",
        OverlayTopLeft: "4",
        OverlayTopRight: "5",
        OverlayBottomLeft: "6",
        OverlayBottomRight: "7",
      }}[anchor];
      return order || `8:${{anchor}}`;
    }}

    function panelContainer(anchor) {{
      if (anchor === "LeftOfStream") return leftPanels;
      if (anchor === "AboveStream") return abovePanels;
      if (anchor === "BelowStream") return belowPanels;
      if (anchor === "OverlayTopLeft") return overlayTopLeftPanels;
      if (anchor === "OverlayTopRight") return overlayTopRightPanels;
      if (anchor === "OverlayBottomLeft") return overlayBottomLeftPanels;
      if (anchor === "OverlayBottomRight") return overlayBottomRightPanels;
      if (String(anchor || "").startsWith("NamedRegion:")) {{
        return namedPanelContainer(String(anchor).slice("NamedRegion:".length));
      }}
      return rightPanels;
    }}

    function namedPanelContainer(name) {{
      const key = name || "default";
      if (namedPanelContainers.has(key)) {{
        return namedPanelContainers.get(key);
      }}
      const section = document.createElement("section");
      section.className = "panel-region named-panel-region";
      const title = document.createElement("h2");
      title.textContent = key;
      const panels = document.createElement("div");
      panels.className = "panel-region";
      section.appendChild(title);
      section.appendChild(panels);
      belowPanels.appendChild(section);
      namedPanelContainers.set(key, panels);
      return panels;
    }}

    function clearPanelRegionClasses() {{
      const regions = [
        leftPanels,
        rightPanels,
        abovePanels,
        belowPanels,
        overlayTopLeftPanels,
        overlayTopRightPanels,
        overlayBottomLeftPanels,
        overlayBottomRightPanels,
      ];
      for (const region of regions) {{
        region.classList.remove("region-wrap-no-scroll");
        for (const className of appliedPanelRegionClasses) {{
          region.classList.remove(className);
        }}
      }}
      appliedPanelRegionClasses.clear();
    }}

    function applyPanelRegionStyle(region, styleHint) {{
      if (!region || !styleHint) return;
      if (styleHint.overflow_mode === "WrapNoScroll") {{
        region.classList.add("region-wrap-no-scroll");
      }}
      for (const className of safeCssClasses(styleHint.region_css_class)) {{
        region.classList.add(className);
        appliedPanelRegionClasses.add(className);
      }}
    }}

    function applyPanelSizeHint(element, hint) {{
      if (!hint || typeof hint !== "object") return;
      const pairs = [
        ["min_width_px", "minWidth"],
        ["max_width_px", "maxWidth"],
        ["min_height_px", "minHeight"],
        ["max_height_px", "maxHeight"],
      ];
      for (const [key, styleName] of pairs) {{
        if (hint[key] === null || hint[key] === undefined) continue;
        const value = Number(hint[key]);
        if (Number.isFinite(value) && value >= 0 && value <= 2000) {{
          element.style[styleName] = `${{value}}px`;
        }}
      }}
    }}

    function safeCssColor(value) {{
      if (typeof value !== "string") return null;
      const color = value.trim();
      if (color.length > 64) return null;
      if (/^#(?:[0-9a-fA-F]{{3}}|[0-9a-fA-F]{{4}}|[0-9a-fA-F]{{6}}|[0-9a-fA-F]{{8}})$/.test(color)) {{
        return color;
      }}
      if (isSafeRgbColor(color)) {{
        return color;
      }}
      if (isSafeHslColor(color)) {{
        return color;
      }}
      if (/^(?:white|black|red|green|blue|yellow|cyan|magenta|orange|purple|pink|lime|teal|gold|silver|gray|grey)$/.test(color)) {{
        return color;
      }}
      return null;
    }}

    function safeCssFontWeight(value) {{
      if (typeof value !== "string") return null;
      const weight = value.trim();
      if (/^(?:normal|bold|bolder|lighter)$/.test(weight)) {{
        return weight;
      }}
      if (/^[1-9]00$/.test(weight)) {{
        const numeric = Number(weight);
        return numeric >= 100 && numeric <= 900 ? weight : null;
      }}
      return null;
    }}

    function isSafeRgbColor(color) {{
      const match = color.match(/^rgb\(\s*(\d{{1,3}})\s*,\s*(\d{{1,3}})\s*,\s*(\d{{1,3}})\s*\)$/);
      return !!match && match.slice(1).every(part => Number(part) >= 0 && Number(part) <= 255);
    }}

    function isSafeHslColor(color) {{
      const match = color.match(/^hsl\(\s*(\d{{1,3}})\s+(\d{{1,3}})%\s+(\d{{1,3}})%\s*\)$/);
      return !!match
        && Number(match[1]) <= 360
        && Number(match[2]) <= 100
        && Number(match[3]) <= 100;
    }}

    function safeCssClasses(value) {{
      if (typeof value !== "string") return [];
      return value
        .split(/\s+/)
        .filter(token => /^[A-Za-z0-9_-]{{1,48}}$/.test(token))
        .slice(0, 4);
    }}

    connect();
    pollStreamStatus();
    pollChatFeed();
    pollCustomPanels();
    pollCustomOverlays();
  </script>
</body>
</html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_identity_prefers_valid_device_id() {
        let request = parse_http_request(
            b"GET /custom-panels HTTP/1.1\r\n\
            X-DirectStream-Device-Id: 550e8400-e29b-41d4-a716-446655440000\r\n\
            CF-Connecting-IP: 203.0.113.10\r\n\r\n",
        )
        .unwrap();

        assert_eq!(
            local_chat_identity(&request, None),
            "device:550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn local_identity_rejects_unsafe_device_id_and_falls_back_to_ip() {
        let request = parse_http_request(
            b"GET /custom-panels HTTP/1.1\r\n\
            X-DirectStream-Device-Id: ../../nope\r\n\
            X-Forwarded-For: 198.51.100.8, 198.51.100.9\r\n\r\n",
        )
        .unwrap();

        assert_eq!(local_chat_identity(&request, None), "ip:198.51.100.8");
    }

    #[test]
    fn local_identity_uses_peer_ip_without_headers() {
        let request = parse_http_request(b"GET /custom-panels HTTP/1.1\r\n\r\n").unwrap();
        let peer = "192.0.2.44:8080".parse().ok();

        assert_eq!(local_chat_identity(&request, peer), "ip:192.0.2.44");
    }

    #[test]
    fn parser_rejects_path_traversal_targets() {
        assert!(matches!(
            parse_http_request(b"GET /../secret HTTP/1.1\r\n\r\n"),
            Err(HttpRequestError::BadRequest)
        ));
    }

    #[test]
    fn cors_rejects_unknown_browser_origins() {
        let request = parse_http_request(
            b"GET /status.json HTTP/1.1\r\nOrigin: https://example.invalid\r\n\r\n",
        )
        .unwrap();

        assert!(cors_headers(&request).contains("Access-Control-Allow-Origin: null"));
    }
}
