//! UI-only swapchains on the existing device and queue.

use std::{collections::HashMap, time::Instant};

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
    /// Whether the host is inside a native interactive resize gesture.
    pub resizing: bool,
    pub reply: Box<dyn FnOnce(NativeUiResult) + Send>,
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
    resize_scaling: bool,
    /// Last client extent and when it changed while stretching this surface.
    resize_sample: Option<([u32; 2], Instant)>,
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
                (Some(surface), Some(frame)) => surface.draw(device, queue, size, task.resizing, frame),
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
        (task.reply)(result);
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
        crate::present::request_resize_scaling(&surface);
        Some(Self {
            surface,
            egui: egui_wgpu::Renderer::new(device, config.format, egui_wgpu::RendererOptions::default()),
            config,
            configure: true,
            resize_scaling: false,
            resize_sample: None,
            previews: Default::default(),
        })
    }

    fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, size: [u32; 2], resizing: bool, mut frame: EguiFrame) -> NativeUiResult {
        for (id, deltas) in &frame.textures_delta.set {
            for delta in deltas {
                self.egui.update_texture(device, queue, *id, delta);
            }
        }
        let frees = std::mem::take(&mut frame.textures_delta.free);
        frame.textures_delta.clear();
        let start = log::log_enabled!(log::Level::Trace).then(Instant::now);
        let result = self.present(device, queue, size, resizing, frame);
        if let Some(start) = start {
            log::trace!("native UI {}x{}: {result:?} in {:?}", size[0], size[1], start.elapsed());
        }
        for id in &frees {
            self.egui.free_texture(id);
        }
        result
    }

    fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        [width, height]: [u32; 2],
        resizing: bool,
        frame: EguiFrame,
    ) -> NativeUiResult {
        if width == 0 || height == 0 {
            self.resize_sample = None;
            return NativeUiResult::Occluded;
        }
        let size = [width, height];
        let size_differs = size != [self.config.width, self.config.height];
        // A held resize stretches through changing sizes, then restores exact
        // pixels on a pause or release. Each new size starts its own delay.
        let can_retain = resizing && self.resize_scaling && !self.configure && size_differs;
        let retaining = if can_retain {
            let changed = self
                .resize_sample
                .filter(|(last, _)| *last == size)
                .map_or_else(Instant::now, |(_, changed)| changed);
            self.resize_sample = Some((size, changed));
            changed.elapsed() < crate::present::RESIZE_SETTLE_DELAY
        } else {
            false
        };
        if !retaining {
            self.resize_sample = None;
        }
        if self.configure || !retaining && size_differs {
            if can_retain {
                log::trace!("native UI {width}x{height}: settled while held");
            }
            self.config.width = width;
            self.config.height = height;
            let start = log::log_enabled!(log::Level::Trace).then(Instant::now);
            crate::present::configure(&self.surface, device, &self.config);
            if let Some(start) = start {
                log::trace!("native UI {width}x{height}: configured in {:?}", start.elapsed());
            }
            self.configure = false;
            self.resize_scaling = crate::present::resize_scaling_enabled(&self.surface);
            log::trace!("native UI resize scaling enabled={}", self.resize_scaling);
        }
        use wgpu::CurrentSurfaceTexture::*;
        let surface_texture = match self.surface.get_current_texture() {
            Success(texture) => texture,
            Suboptimal(texture) => {
                self.configure = !retaining;
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
        let surface_size = [self.config.width, self.config.height];
        if surface_size != [width, height] {
            log::trace!(
                "native UI {width}x{height}: retaining {}x{} during resize",
                surface_size[0],
                surface_size[1]
            );
            scale_ui(&mut ui.primitives, [width, height], surface_size);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: surface_size,
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

/// Fit the latest logical layout into the retained swapchain. The compositor
/// stretches this image back to the live client rectangle until exact-size
/// presentation resumes. Scale clips and callback rectangles with the meshes.
fn scale_ui(primitives: &mut [egui::epaint::ClippedPrimitive], from: [u32; 2], to: [u32; 2]) {
    debug_assert!(from.into_iter().chain(to).all(|side| side > 0));
    let scale = egui::vec2(to[0] as f32 / from[0] as f32, to[1] as f32 / from[1] as f32);
    for primitive in primitives {
        primitive.clip_rect.min = (primitive.clip_rect.min.to_vec2() * scale).to_pos2();
        primitive.clip_rect.max = (primitive.clip_rect.max.to_vec2() * scale).to_pos2();
        match &mut primitive.primitive {
            egui::epaint::Primitive::Mesh(mesh) => {
                for vertex in &mut mesh.vertices {
                    vertex.pos = (vertex.pos.to_vec2() * scale).to_pos2();
                }
            }
            egui::epaint::Primitive::Callback(callback) => {
                callback.rect.min = (callback.rect.min.to_vec2() * scale).to_pos2();
                callback.rect.max = (callback.rect.max.to_vec2() * scale).to_pos2();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{ClippedPrimitive, Mesh, Primitive, Vertex};

    #[test]
    fn retained_surface_scales_layout_clips_and_callbacks_without_changing_texture_coordinates() {
        let rect = egui::Rect::from_min_max(egui::pos2(20.0, 10.0), egui::pos2(100.0, 50.0));
        let vertex = Vertex {
            pos: egui::pos2(40.0, 30.0),
            uv: egui::pos2(0.25, 0.75),
            color: Default::default(),
        };
        let callback = std::sync::Arc::new(());
        for (from, to, expected_vertex, expected_rect) in [
            (
                [400, 300],
                [200, 600],
                egui::pos2(20.0, 60.0),
                egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(50.0, 100.0)),
            ),
            (
                [200, 600],
                [400, 300],
                egui::pos2(80.0, 15.0),
                egui::Rect::from_min_max(egui::pos2(40.0, 5.0), egui::pos2(200.0, 25.0)),
            ),
            ([400, 300], [400, 300], vertex.pos, rect),
        ] {
            let mut primitives = vec![
                ClippedPrimitive {
                    clip_rect: rect,
                    primitive: Primitive::Mesh(Mesh {
                        vertices: vec![vertex],
                        indices: vec![0],
                        texture_id: egui::TextureId::Managed(17),
                    }),
                },
                ClippedPrimitive {
                    clip_rect: egui::Rect::EVERYTHING,
                    primitive: Primitive::Callback(egui::PaintCallback {
                        rect,
                        callback: callback.clone(),
                    }),
                },
            ];
            scale_ui(&mut primitives, from, to);
            assert_eq!(primitives[0].clip_rect, expected_rect);
            let Primitive::Mesh(mesh) = &primitives[0].primitive else {
                panic!("expected mesh")
            };
            assert_eq!(mesh.vertices[0].pos, expected_vertex);
            assert_eq!(mesh.vertices[0].uv, vertex.uv);
            assert_eq!(mesh.vertices[0].color, vertex.color);
            assert_eq!(mesh.texture_id, egui::TextureId::Managed(17));
            assert_eq!(mesh.indices, [0]);
            assert_eq!(primitives[1].clip_rect, egui::Rect::EVERYTHING);
            let Primitive::Callback(actual) = &primitives[1].primitive else {
                panic!("expected callback")
            };
            assert_eq!(actual.rect, expected_rect);
            assert!(actual.callback.downcast_ref::<()>().is_some());
        }
    }
}
