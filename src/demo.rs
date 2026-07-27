//! Small built-in demo scene for exercising PixelStream without a downstream game.

use crate::{
    DirectStreamSet, DirectText, PlayStreamSound, StreamAudioClip, StreamChatCommand,
    StreamChatSender, StreamCommandAppExt, app::direct_stream_app,
    public_types::DirectStreamTarget,
};
use bevy::{
    asset::RenderAssetUsages,
    ecs::system::In,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    window::FileDragAndDrop,
};
use crossbeam_channel::Receiver;
use ffmpeg_next as ffmpeg;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const DEMO_MUSIC_PATH: &str = "music/Elijah_K - Iron.wav";
const DEMO_SFX_PATH: &str = "sfx/boing_x.wav";

#[derive(Component)]
pub struct HelloWorldText;

#[derive(Resource)]
pub struct DemoMusicClip(Handle<StreamAudioClip>);

#[derive(Resource)]
pub struct DemoSfxClip(Handle<StreamAudioClip>);

#[derive(Resource, Default)]
pub struct DemoMusicStarted(bool);

#[derive(Default)]
pub struct DemoVideoBackground {
    image: Handle<Image>,
    worker: Option<DemoVideoWorker>,
}

pub struct DemoVideoWorker {
    receiver: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
}

impl Drop for DemoVideoWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub fn run_demo() {
    let mut app = direct_stream_app();
    app.insert_non_send(DemoVideoBackground::default())
        .add_stream_command("boing", handle_demo_boing_command)
        .add_systems(Startup, setup_demo_scene.after(DirectStreamSet::Setup))
        .add_systems(
            Update,
            (
                pulse_hello_world_text,
                start_demo_music,
                handle_demo_video_drop,
                update_demo_video_background,
            ),
        );
    app.run();
}

pub fn setup_demo_scene(
    mut commands: Commands,
    target: Res<DirectStreamTarget>,
    mut video_background: Option<NonSendMut<DemoVideoBackground>>,
    mut images: ResMut<Assets<Image>>,
    mut clips: ResMut<Assets<StreamAudioClip>>,
) {
    let background = images.add(srgb_hue_gradient_image(target.width, target.height));
    let text_size = (target.width.min(target.height) as f32 * 0.11).clamp(8.0, 20.0);
    commands.spawn((
        Sprite {
            image: background.clone(),
            custom_size: Some(Vec2::new(target.width as f32, target.height as f32)),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -10.0),
    ));
    if let Some(video_background) = video_background.as_mut() {
        video_background.image = background;
        video_background.worker = None;
    }

    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::NONE),
            UiTargetCamera(target.camera),
        ))
        .with_child((
            Text::new("HelloWorld"),
            TextFont {
                font_size: FontSize::Px(text_size),
                ..default()
            },
            TextColor(Color::srgb(0.92, 0.96, 1.0)),
            TextShadow {
                offset: Vec2::new(2.0, 2.0),
                color: Color::srgba(0.0, 0.0, 0.0, 0.7),
            },
            HelloWorldText,
        ));

    commands.spawn(
        DirectText::new("DIRECT TEXT", 4, 4)
            .with_scale(2)
            .with_color(Srgba::WHITE),
    );

    match StreamAudioClip::from_wav_file(DEMO_MUSIC_PATH) {
        Ok(clip) => {
            commands.insert_resource(DemoMusicClip(clips.add(clip)));
            commands.insert_resource(DemoMusicStarted::default());
        }
        Err(err) => eprintln!("Could not load demo music {DEMO_MUSIC_PATH}: {err}"),
    }

    match StreamAudioClip::from_wav_file(DEMO_SFX_PATH) {
        Ok(clip) => {
            commands.insert_resource(DemoSfxClip(clips.add(clip)));
        }
        Err(err) => eprintln!("Could not load demo SFX {DEMO_SFX_PATH}: {err}"),
    }
}

pub fn handle_demo_video_drop(
    mut drops: MessageReader<FileDragAndDrop>,
    mut background: Option<NonSendMut<DemoVideoBackground>>,
    target: Res<DirectStreamTarget>,
) {
    let Some(background) = background.as_mut() else {
        return;
    };

    for drop in drops.read() {
        let FileDragAndDrop::DroppedFile { path_buf, .. } = drop else {
            continue;
        };
        if !is_supported_demo_video(path_buf) {
            eprintln!("Demo video ignored: {}", path_buf.display());
            continue;
        }

        match DemoVideoWorker::spawn(path_buf.clone(), target.width, target.height) {
            Ok(worker) => {
                background.worker = Some(worker);
                eprintln!("Demo video loaded: {}", path_buf.display());
            }
            Err(err) => eprintln!("Could not load demo video {}: {err}", path_buf.display()),
        }
    }
}

pub fn update_demo_video_background(
    mut background: Option<NonSendMut<DemoVideoBackground>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(background) = background.as_mut() else {
        return;
    };
    let image_handle = background.image.clone();
    let Some(worker) = background.worker.as_mut() else {
        return;
    };

    let mut latest_frame = None;
    while let Ok(frame) = worker.receiver.try_recv() {
        latest_frame = Some(frame);
    }

    let Some(frame) = latest_frame else {
        return;
    };
    if let Some(mut image) = images.get_mut(&image_handle) {
        image.data = Some(frame);
    }
}

impl DemoVideoWorker {
    fn spawn(path: PathBuf, output_width: u32, output_height: u32) -> Result<Self, String> {
        let frame_seconds =
            DemoVideoDecoder::open(&path, output_width, output_height)?.frame_seconds;
        let (sender, receiver) = crossbeam_channel::bounded(4);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();

        thread::spawn(move || {
            let mut decoder = match DemoVideoDecoder::open(&path, output_width, output_height) {
                Ok(decoder) => decoder,
                Err(err) => {
                    eprintln!(
                        "Could not start demo video worker {}: {err}",
                        path.display()
                    );
                    return;
                }
            };
            let frame_interval = Duration::from_secs_f32(frame_seconds.max(1.0 / 120.0));
            let mut next_frame_at = Instant::now();

            while !thread_stop.load(Ordering::Relaxed) {
                match decoder.next_frame() {
                    Some(frame) => {
                        let _ = sender.try_send(frame);
                    }
                    None => thread::sleep(Duration::from_millis(2)),
                }

                next_frame_at += frame_interval;
                let now = Instant::now();
                if next_frame_at > now {
                    thread::sleep(next_frame_at - now);
                } else if now.duration_since(next_frame_at) > frame_interval {
                    next_frame_at = now + frame_interval;
                }
            }
        });

        Ok(Self { receiver, stop })
    }
}

fn is_supported_demo_video(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi"
            )
        })
        .unwrap_or(false)
}

pub struct DemoVideoDecoder {
    path: PathBuf,
    output_width: u32,
    output_height: u32,
    frame_seconds: f32,
    input: ffmpeg::format::context::Input,
    stream_index: usize,
    decoder: ffmpeg::codec::decoder::Video,
    scaler: ffmpeg::software::scaling::context::Context,
}

impl DemoVideoDecoder {
    fn open(path: &Path, output_width: u32, output_height: u32) -> Result<Self, String> {
        ffmpeg::init().map_err(|err| err.to_string())?;
        let input = ffmpeg::format::input(path).map_err(|err| err.to_string())?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| "file has no video stream".to_owned())?;
        let stream_index = stream.index();
        let frame_seconds = frame_seconds_from_rate(stream.avg_frame_rate());
        let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .map_err(|err| err.to_string())?;
        let decoder = context.decoder().video().map_err(|err| err.to_string())?;
        let scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::util::format::Pixel::RGBA,
            output_width,
            output_height,
            ffmpeg::software::scaling::flag::Flags::BILINEAR,
        )
        .map_err(|err| err.to_string())?;

        Ok(Self {
            path: path.to_owned(),
            output_width,
            output_height,
            frame_seconds,
            input,
            stream_index,
            decoder,
            scaler,
        })
    }

    fn next_frame(&mut self) -> Option<Vec<u8>> {
        for _ in 0..2 {
            if let Some(frame) = self.decode_next_frame() {
                return Some(frame);
            }
            if let Err(err) = self.loop_video() {
                eprintln!("Could not loop demo video {}: {err}", self.path.display());
                return None;
            }
        }
        None
    }

    fn decode_next_frame(&mut self) -> Option<Vec<u8>> {
        let mut decoded = ffmpeg::frame::Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if let Some(frame) = convert_frame(
                &mut self.scaler,
                self.output_width,
                self.output_height,
                &decoded,
            ) {
                return Some(frame);
            }
        }

        for (stream, packet) in self.input.packets() {
            if stream.index() != self.stream_index {
                continue;
            }
            if self.decoder.send_packet(&packet).is_err() {
                continue;
            }
            while self.decoder.receive_frame(&mut decoded).is_ok() {
                if let Some(frame) = convert_frame(
                    &mut self.scaler,
                    self.output_width,
                    self.output_height,
                    &decoded,
                ) {
                    return Some(frame);
                }
            }
        }

        let _ = self.decoder.send_eof();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if let Some(frame) = convert_frame(
                &mut self.scaler,
                self.output_width,
                self.output_height,
                &decoded,
            ) {
                return Some(frame);
            }
        }
        None
    }

    fn loop_video(&mut self) -> Result<(), String> {
        let reopened = Self::open(&self.path, self.output_width, self.output_height)?;
        *self = reopened;
        Ok(())
    }
}

fn convert_frame(
    scaler: &mut ffmpeg::software::scaling::context::Context,
    output_width: u32,
    output_height: u32,
    decoded: &ffmpeg::frame::Video,
) -> Option<Vec<u8>> {
    let mut rgba = ffmpeg::frame::Video::new(
        ffmpeg::util::format::Pixel::RGBA,
        output_width,
        output_height,
    );
    scaler.run(decoded, &mut rgba).ok()?;
    Some(tightly_packed_rgba(&rgba, output_width, output_height))
}

fn frame_seconds_from_rate(rate: ffmpeg::Rational) -> f32 {
    let numerator = rate.numerator();
    let denominator = rate.denominator();
    if numerator <= 0 || denominator <= 0 {
        1.0 / 30.0
    } else {
        (denominator as f32 / numerator as f32).clamp(1.0 / 120.0, 1.0)
    }
}

fn tightly_packed_rgba(frame: &ffmpeg::frame::Video, width: u32, height: u32) -> Vec<u8> {
    let stride = frame.stride(0);
    let row_bytes = width as usize * 4;
    let data = frame.data(0);
    let mut output = Vec::with_capacity(row_bytes * height as usize);
    for y in 0..height as usize {
        let start = y * stride;
        let end = start + row_bytes;
        output.extend_from_slice(&data[start..end]);
    }
    output
}

fn srgb_hue_gradient_image(width: u32, height: u32) -> Image {
    let width = width.max(1);
    let height = height.max(1);
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    let max_x = (width - 1).max(1) as f32;
    let max_y = (height - 1).max(1) as f32;

    for y in 0..height {
        for x in 0..width {
            let corner_progress = (x as f32 / max_x + y as f32 / max_y) * 0.5;
            let [r, g, b] = hsv_to_srgb(corner_progress * 360.0, 1.0, 1.0);
            data.extend_from_slice(&[r, g, b, 255]);
        }
    }

    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

fn hsv_to_srgb(hue_degrees: f32, saturation: f32, value: f32) -> [u8; 3] {
    let hue = hue_degrees.rem_euclid(360.0);
    let c = value * saturation;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = value - c;
    let (r, g, b) = if hue < 60.0 {
        (c, x, 0.0)
    } else if hue < 120.0 {
        (x, c, 0.0)
    } else if hue < 180.0 {
        (0.0, c, x)
    } else if hue < 240.0 {
        (0.0, x, c)
    } else if hue < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    [
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    ]
}

pub fn pulse_hello_world_text(
    time: Res<Time>,
    mut text_query: Query<&mut TextColor, With<HelloWorldText>>,
) {
    let pulse = ((time.elapsed_secs() * 4.0).sin() + 1.0) * 0.5;
    let color = Color::srgb(0.55 + pulse * 0.4, 0.85, 1.0);

    for mut text_color in &mut text_query {
        text_color.0 = color;
    }
}

pub fn start_demo_music(
    mut started: Option<ResMut<DemoMusicStarted>>,
    clip: Option<Res<DemoMusicClip>>,
    mut events: MessageWriter<PlayStreamSound>,
) {
    let (Some(started), Some(clip)) = (started.as_mut(), clip) else {
        return;
    };

    if !started.0 {
        events.write(PlayStreamSound::looping(clip.0.clone()).with_volume(0.20));
        started.0 = true;
    }
}

pub fn handle_demo_boing_command(
    In(command): In<StreamChatCommand>,
    clip: Option<Res<DemoSfxClip>>,
    chat: Option<Res<StreamChatSender>>,
    mut events: MessageWriter<PlayStreamSound>,
) {
    let Some(clip) = clip else {
        return;
    };

    events.write(PlayStreamSound::once(clip.0.clone()).with_volume(0.80));
    if let Some(chat) = chat {
        let role = if command.roles.broadcaster {
            "broadcaster"
        } else if command.roles.moderator {
            "moderator"
        } else if command.roles.vip {
            "vip"
        } else if command.roles.subscriber {
            "subscriber"
        } else {
            "viewer"
        };
        chat.send(format!("Boing, {} ({role})!", command.display_name));
    }
}
