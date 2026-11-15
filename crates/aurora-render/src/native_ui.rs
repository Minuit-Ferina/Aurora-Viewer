//! UI-only swapchains on the existing device and queue.

use std::collections::HashMap;
use std::sync::mpsc::SyncSender;

use crate::EguiFrame;

/// A native presentation lifetime, assigned by the event-loop owner. A
/// re-created window gets a new ID, even when it presents the same model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NativeUiId(pub u64);

pub enum NativeUiCommand {
    Open { surface: wgpu::Surface<'static>, size: [u32; 2] },
    Draw { size: [u32; 2], frame: NativeUiFrame },
    Retire,
}

pub struct NativeUiTask {
    pub id: NativeUiId,
    pub command: NativeUiCommand,
    pub reply: SyncSender<NativeUiResult>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeUiResult {
    Opened,
    Presented,
    Occluded,
    Retry,
    Failed,
    Retired,
}

/// A frame can be invalidated on window shutdown, worker shutdown or
/// device loss. Its unused delta belongs to that presentation's context.
pub struct NativeUiFrame(Option<EguiFrame>);

impl From<EguiFrame> for NativeUiFrame {
    fn from(frame: EguiFrame) -> Self {
        Self(Some(frame))
    }
}

impl Drop for NativeUiFrame {
    fn drop(&mut self) {
        if let Some(frame) = &mut self.0 {
            frame.textures_delta.clear();
        }
    }
}

#[derive(Default)]
pub(crate) struct Surfaces(HashMap<NativeUiId, Surface>);

struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    configure: bool,
    egui: egui_wgpu::Renderer,
    previews: crate::preview::Cache,
}

impl Surfaces {
    pub(crate) fn run(&mut self, adapter: &wgpu::Adapter, device: &wgpu::Device, queue: &wgpu::Queue, task: NativeUiTask) {
        let result = match task.command {
            NativeUiCommand::Open { surface, size } => match self.0.entry(task.id) {
                std::collections::hash_map::Entry::Occupied(_) => NativeUiResult::Failed,
                std::collections::hash_map::Entry::Vacant(entry) => match Surface::open(adapter, device, surface, size) {
                    Some(surface) => {
                        entry.insert(surface);
                        NativeUiResult::Opened
                    }
                    None => NativeUiResult::Failed,
                },
            },
            NativeUiCommand::Draw { size, mut frame } => match (self.0.get_mut(&task.id), frame.0.take()) {
                (Some(surface), Some(frame)) => surface.draw(device, queue, size, frame),
                (_, mut frame) => {
                    if let Some(frame) = &mut frame {
                        frame.textures_delta.clear();
                    }
                    NativeUiResult::Failed
                }
            },
            NativeUiCommand::Retire => {
                self.0.remove(&task.id);
                NativeUiResult::Retired
            }
        };
        let _ = task.reply.try_send(result);
    }

    pub(crate) fn abandon_lost_surfaces(&mut self) {
        for (_, surface) in self.0.drain() {
            // The root surface follows the same loss path. Some drivers
            // cannot release an acquired image after device loss.
            std::mem::forget(surface.surface);
        }
    }
}

impl Surface {
    fn open(adapter: &wgpu::Adapter, device: &wgpu::Device, surface: wgpu::Surface<'static>, [width, height]: [u32; 2]) -> Option<Self> {
        let mut config = surface.get_default_config(adapter, width.max(1), height.max(1))?;
        let caps = surface.get_capabilities(adapter);
        if let Some(format) = caps.formats.iter().find(|format| !format.is_srgb()) {
            config.format = *format;
        }
        config.present_mode = crate::present::choose(&caps.present_modes, false, None);
        log::info!("native UI surface: {}", crate::present::name(config.present_mode));
        Some(Self {
            surface,
            egui: egui_wgpu::Renderer::new(device, config.format, egui_wgpu::RendererOptions::default()),
            config,
            configure: true,
            previews: Default::default(),
        })
    }

    fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: [u32; 2], mut frame: EguiFrame) -> NativeUiResult {
        for (id, deltas) in &frame.textures_delta.set {
            for delta in deltas {
                self.egui.update_texture(device, queue, *id, delta);
            }
        }
        let frees = std::mem::take(&mut frame.textures_delta.free);
        frame.textures_delta.clear();
        let result = self.present(device, queue, size, frame);
        for id in &frees {
            self.egui.free_texture(id);
        }
        result
    }

    fn present(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, [width, height]: [u32; 2], frame: EguiFrame) -> NativeUiResult {
        if width == 0 || height == 0 {
            return NativeUiResult::Occluded;
        }
        if self.configure || self.config.width != width || self.config.height != height {
            self.config.width = width;
            self.config.height = height;
            crate::present::configure(&self.surface, device, &self.config);
            self.configure = false;
        }
        use wgpu::CurrentSurfaceTexture::*;
        let surface_texture = match self.surface.get_current_texture() {
            Success(texture) => texture,
            Suboptimal(texture) => {
                self.configure = true;
                texture
            }
            Outdated => {
                self.configure = true;
                return NativeUiResult::Retry;
            }
            Timeout => return NativeUiResult::Retry,
            Occluded => return NativeUiResult::Occluded,
            Lost | Validation => return NativeUiResult::Failed,
        };
        let view = surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("native UI") });
        let mut ui = frame.tessellate();
        crate::ui_world::take_end(&mut ui.primitives);
        crate::ui_blur::take_boundary(&mut ui.primitives);
        crate::ui_glass::skip(&mut ui.primitives);
        crate::label_depth::skip(&mut ui.primitives);
        self.previews
            .render(device, queue, &mut encoder, &mut self.egui, &mut ui.primitives, ui.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: ui.pixels_per_point,
        };
        let mut commands = self.egui.update_buffers(device, queue, &mut encoder, &ui.primitives, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("native UI"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            let jobs = crate::ui_frame::paint_jobs(&ui.primitives, crate::ui_frame::meshes_uploaded(&ui.primitives));
            self.egui.render(&mut pass, &jobs, &screen);
        }
        commands.push(encoder.finish());
        queue.submit(commands);
        queue.present(surface_texture);
        NativeUiResult::Presented
    }
}
