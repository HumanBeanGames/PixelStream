//! Swapchain safety pass for Vulkan startup and resize edges.
//!
//! Some Vulkan drivers validate aggressively when a newly acquired swapchain
//! image is presented before Bevy's first window camera path has produced GPU
//! work for that image. PixelStream renders most game content into offscreen
//! stream targets, so the native window can be especially vulnerable during
//! startup while the UI/upscaling pipelines are still warming. This render-graph
//! pass performs a cheap clear on acquired window textures before the camera
//! driver runs, ensuring the image has a valid presentation layout even if the
//! normal window camera skips that frame.

use bevy::{
    camera::ClearColor,
    prelude::*,
    render::{
        RenderApp,
        render_resource::{
            CommandEncoderDescriptor, LoadOp, Operations, RenderPassColorAttachment,
            RenderPassDescriptor, StoreOp,
        },
        renderer::{PendingCommandBuffers, RenderDevice, RenderGraph, RenderGraphSystems},
        view::ExtractedWindows,
    },
};

const STARTUP_PRIMER_FRAMES: u32 = 60;

pub(crate) struct SurfacePrimerPlugin;

impl Plugin for SurfacePrimerPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(
            RenderGraph,
            prime_window_surfaces.in_set(RenderGraphSystems::Begin),
        );
    }
}

fn prime_window_surfaces(
    windows: Res<ExtractedWindows>,
    clear_color: Res<ClearColor>,
    render_device: Res<RenderDevice>,
    mut pending: ResMut<PendingCommandBuffers>,
    mut startup_frames_remaining: Local<u32>,
    mut initialized: Local<bool>,
) {
    if !*initialized {
        *startup_frames_remaining = STARTUP_PRIMER_FRAMES;
        *initialized = true;
    }

    let should_prime = *startup_frames_remaining > 0
        || windows.windows.values().any(|window| {
            window.needs_initial_present || window.size_changed || window.present_mode_changed
        });
    if !should_prime {
        return;
    }

    let targets = windows
        .windows
        .values()
        .filter(|window| window.physical_width > 0 && window.physical_height > 0)
        .filter_map(|window| window.swap_chain_texture_view.as_ref())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return;
    }

    let clear = clear_color.0.to_linear();
    let mut encoder = render_device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("pixel_stream_surface_primer"),
    });
    for target in targets {
        let descriptor = RenderPassDescriptor {
            label: Some("pixel_stream_surface_primer"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: Operations {
                    load: LoadOp::Clear(clear.into()),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        };
        encoder.begin_render_pass(&descriptor);
    }
    pending.push_encoder(encoder);

    if *startup_frames_remaining > 0 {
        *startup_frames_remaining -= 1;
    }
}
