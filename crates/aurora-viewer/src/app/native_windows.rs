//! Offline fixture for the event-loop side of native UI presentation.
//!
//! A document stays in the root until the render owner accepts its surface.
//! Closing a presentation hides it immediately and keeps its window alive
//! until the render owner has dropped its reference to the surface.

mod chrome;

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use aurora_i18n::tr;
use aurora_render::native_ui::{NativeUiCommand, NativeUiId, NativeUiResult, NativeUiTask};
use aurora_render::{EguiHandOver, Renderer, preview};
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::theme::Palette;
use crate::ui::widgets;

const DEMO_MAX: usize = 6;
const RETRY_DELAY: Duration = Duration::from_millis(32);

struct Document {
    open: bool,
    detach: bool,
    text: String,
    editor: Option<egui::text_edit::TextEditState>,
    preview: Option<DemoPreview>,
}

struct DemoPreview {
    model: Arc<preview::Model>,
    view: preview::View,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Opening,
    Live,
    Retiring,
}

struct Host {
    id: NativeUiId,
    document: usize,
    window: Arc<Window>,
    context: egui::Context,
    input: egui_winit::State,
    handover: EguiHandOver,
    chrome: chrome::Chrome,
    stage: Stage,
    surface: Option<aurora_render::wgpu::Surface<'static>>,
    occluded: bool,
    repaint: Option<Instant>,
}

impl Host {
    fn viewport(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(self.id.0)
    }

    fn visible(&self) -> bool {
        self.stage == Stage::Live && !self.occluded && self.window.is_minimized() != Some(true)
    }

    fn set_repaint(&mut self, delay: Duration) {
        if let Some(at) = Instant::now().checked_add(delay) {
            self.repaint = Some(self.repaint.map_or(at, |existing| existing.min(at)));
        }
    }

    /// Run a panel presenter with this window's input, clip, repaint clock
    /// and platform output. The caller applies logical actions and decides
    /// whether to send its finished frame to the render worker.
    fn paint(
        &mut self,
        event_loop: &ActiveEventLoop,
        root: &Window,
        root_context: &egui::Context,
        mut presenter: impl FnMut(&mut egui::Ui, &chrome::Chrome, &Window),
    ) -> Painted {
        self.repaint = None;
        let viewport = self.viewport();
        let mut raw = self.input.take_egui_input(&self.window);
        egui_winit::update_viewport_info(raw.viewports.entry(egui::ViewportId::ROOT).or_default(), root_context, root, false);
        let info = raw.viewports.entry(viewport).or_default();
        egui_winit::update_viewport_info(info, &self.context, &self.window, false);
        info.parent = Some(egui::ViewportId::ROOT);
        let mut output = self.context.run_ui(raw, |ui| presenter(ui, &self.chrome, &self.window));
        if let Some(cursor) = self.chrome.cursor(&self.window) {
            output.platform_output.cursor_icon = cursor;
        }
        self.input
            .handle_platform_output_with_event_loop(&self.window, event_loop, output.platform_output);
        let mut close_requested = false;
        if let Some(view) = output.viewport_output.remove(&viewport) {
            self.set_repaint(view.repaint_delay);
            let mut viewport_info = egui::ViewportInfo::default();
            let mut actions = Vec::new();
            egui_winit::process_viewport_commands(&self.context, &mut viewport_info, view.commands, &self.window, &mut actions);
            close_requested = viewport_info.close_requested();
        }
        Painted {
            close_requested,
            frame: self
                .handover
                .frame(&self.context, output.shapes, output.textures_delta, output.pixels_per_point),
        }
    }
}

struct Painted {
    frame: aurora_render::EguiFrame,
    close_requested: bool,
}

#[derive(Default)]
pub(super) struct NativeWindows {
    initialized: bool,
    show_controls: bool,
    documents: Vec<Document>,
    hosts: Vec<Host>,
    pending: Option<(NativeUiId, Receiver<NativeUiResult>)>,
    next_id: u64,
    next_draw: usize,
    failed: bool,
}

impl NativeWindows {
    pub(super) fn start_demo(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        let Ok(value) = std::env::var("AURORA_DEMO_NATIVE_WINDOWS") else {
            return;
        };
        let (count, previews) = value
            .strip_suffix(":preview")
            .map_or((value.as_str(), false), |count| (count, true));
        let Some(auto) = count.parse::<usize>().ok().filter(|count| *count <= DEMO_MAX) else {
            log::warn!("AURORA_DEMO_NATIVE_WINDOWS requires a count from 0 to {DEMO_MAX} and accepts :preview");
            return;
        };
        self.show_controls = true;
        let count = auto.max(1);
        let model = previews.then(demo_model);
        self.documents.extend((0..count).map(|_| Document {
            open: true,
            detach: auto > 0,
            text: tr!("native-windows-demo-text").to_owned(),
            editor: None,
            preview: model.as_ref().map(|model| DemoPreview {
                model: Arc::clone(model),
                view: preview::View::default(),
            }),
        }));
    }

    pub(super) fn draw_embedded(&mut self, ctx: &egui::Context, palette: &Palette) {
        if !self.show_controls {
            return;
        }
        let mut return_all = false;
        let mut open = true;
        widgets::Floater::new(
            "native_window_demo_controls",
            tr!("native-windows-demo"),
            egui::pos2(115.0, 115.0),
            egui::vec2(360.0, 100.0),
        )
        .show(ctx, palette, &mut open, |ui| {
            return_all |= widgets::flat_button(ui, palette, tr!("native-windows-return-all")).clicked();
            if self.failed {
                ui.label(tr!("native-windows-open-failed"));
            }
            if self.documents.iter().any(|document| !document.open)
                && widgets::flat_button(ui, palette, tr!("native-windows-demo-reopen")).clicked()
            {
                for document in &mut self.documents {
                    document.open = true;
                }
            }
        });
        self.show_controls = open;
        if return_all {
            for document in &mut self.documents {
                document.detach = false;
            }
        }
        for (index, document) in self.documents.iter_mut().enumerate() {
            if !document.open || self.hosts.iter().any(|host| host.document == index && host.stage == Stage::Live) {
                continue;
            }
            let delta = (index as f32) * 22.0;
            let key = format!("native_window_demo_document_{index}");
            let mut keep_open = document.open;
            let mut detach = false;
            widgets::Floater::new(
                &key,
                document_title(index),
                egui::pos2(510.0 + delta, 160.0 + delta),
                egui::vec2(410.0, 255.0),
            )
            .action(chrome::DETACH_ICON, tr!("native-windows-detach"), &mut detach)
            .show(ctx, palette, &mut keep_open, |ui| {
                if let Some(editor) = document.editor.take() {
                    egui::TextEdit::store_state(ctx, editor_id(index), editor);
                }
                show_document(ui, index, document);
            });
            document.open = keep_open;
            if detach && keep_open {
                document.detach = true;
            }
        }
    }

    pub(super) fn event(&mut self, id: WindowId, event: &WindowEvent) -> Option<bool> {
        let host = self.hosts.iter_mut().find(|host| host.window.id() == id)?;
        if host.stage != Stage::Live {
            return Some(false);
        }
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            self.documents[host.document].open = false;
            self.documents[host.document].detach = false;
            return Some(false);
        }
        if let WindowEvent::Occluded(occluded) = event {
            host.occluded = *occluded;
        }
        if host.chrome.event(&host.window, event) {
            return Some(true);
        }
        let repaint = host.input.on_window_event(&host.window, event).repaint;
        if repaint || matches!(event, WindowEvent::RedrawRequested | WindowEvent::Occluded(false)) {
            host.set_repaint(Duration::ZERO);
        }
        Some(matches!(
            event,
            WindowEvent::KeyboardInput { is_synthetic: false, .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Ime(_)
        ))
    }

    pub(super) fn any_visible(&self) -> bool {
        self.hosts.iter().any(Host::visible)
    }

    pub(super) fn owns_focus(&self) -> bool {
        self.hosts.iter().any(|host| host.stage == Stage::Live && host.window.has_focus())
    }

    pub(super) fn retire_all(&mut self) {
        for document in &mut self.documents {
            document.open = false;
            document.detach = false;
        }
        for host in &self.hosts {
            host.window.set_visible(false);
        }
    }

    pub(super) fn renderer_lost(&mut self) {
        for mut host in self.hosts.drain(..) {
            if host.stage == Stage::Live {
                save_editor(&mut self.documents[host.document], &host);
            }
            host.stage = Stage::Retiring;
            host.window.set_visible(false);
            self.documents[host.document].detach = false;
        }
        self.pending = None;
    }

    pub(super) fn pump(
        &mut self,
        event_loop: &ActiveEventLoop,
        renderer: &mut Renderer,
        root: &Window,
        root_context: &egui::Context,
        palette: &Palette,
    ) {
        self.take_reply();
        for host in &mut self.hosts {
            let document = &mut self.documents[host.document];
            if host.stage != Stage::Retiring && (!document.open || !document.detach) {
                if host.stage == Stage::Live {
                    save_editor(document, host);
                }
                host.stage = Stage::Retiring;
                host.window.set_visible(false);
            }
        }
        if self.pending.is_some() {
            return;
        }
        if let Some(host) = self.hosts.iter().find(|host| host.stage == Stage::Retiring) {
            self.submit(renderer, host.id, NativeUiCommand::Retire);
            return;
        }
        if let Some(host) = self.hosts.iter_mut().find(|host| host.stage == Stage::Opening) {
            if let Some(surface) = host.surface.take() {
                let id = host.id;
                let size = host.window.inner_size().into();
                self.submit(renderer, id, NativeUiCommand::Open { surface, size });
            }
            return;
        }
        if let Some(document) = self
            .documents
            .iter()
            .enumerate()
            .position(|(index, document)| document.open && document.detach && !self.hosts.iter().any(|host| host.document == index))
        {
            self.open(event_loop, renderer, root_context, document);
            return;
        }

        let now = Instant::now();
        let Some(index) = (0..self.hosts.len())
            .map(|offset| (self.next_draw + offset) % self.hosts.len())
            .find(|index| {
                let host = &self.hosts[*index];
                host.repaint.is_some_and(|at| at <= now) && host.visible()
            })
        else {
            return;
        };
        self.next_draw = index + 1;
        let host = &mut self.hosts[index];
        let id = host.id;
        let size = host.window.inner_size().into();
        let index = host.document;
        let document = &mut self.documents[index];
        let mut painted = host.paint(event_loop, root, root_context, |ui, chrome, window| {
            present_document(ui, palette, chrome, window, index, document);
        });
        if painted.close_requested {
            document.open = false;
            document.detach = false;
        }
        if document.open && document.detach {
            host.window.pre_present_notify();
            self.submit(
                renderer,
                id,
                NativeUiCommand::Draw {
                    size,
                    frame: painted.frame.into(),
                },
            );
        } else {
            painted.frame.textures_delta.clear();
        }
    }

    fn open(&mut self, event_loop: &ActiveEventLoop, renderer: &Renderer, root_context: &egui::Context, document: usize) {
        let attrs = chrome::attributes(Window::default_attributes())
            .with_title(format!("{} — {}", document_title(document), crate::cli::window_title()))
            .with_inner_size(LogicalSize::new(420.0, 270.0))
            .with_min_inner_size(LogicalSize::new(260.0, 160.0))
            .with_active(false)
            .with_visible(false);
        let result = event_loop
            .create_window(attrs)
            .map_err(|error| error.to_string())
            .and_then(|window| {
                let window = Arc::new(window);
                let surface = renderer
                    .create_native_ui_surface(window.clone())
                    .map_err(|error| error.to_string())?;
                Ok((window, surface))
            });
        let (window, surface) = match result {
            Ok(pair) => pair,
            Err(error) => {
                log::warn!("native UI window creation failed: {error}");
                self.failed = true;
                self.documents[document].detach = false;
                return;
            }
        };
        self.next_id += 1;
        let id = NativeUiId(self.next_id);
        let context = egui::Context::default();
        context.set_global_style(root_context.global_style());
        let _ = crate::ui::fonts::apply(&context, "");
        context.set_zoom_factor(root_context.zoom_factor());
        let input = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::from_hash_of(id.0),
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(renderer.device.limits().max_texture_dimension_2d as usize),
        );
        let chrome = chrome::Chrome::new(&context);
        self.hosts.push(Host {
            id,
            document,
            window,
            context,
            input,
            handover: EguiHandOver::default(),
            chrome,
            stage: Stage::Opening,
            surface: Some(surface),
            occluded: false,
            repaint: None,
        });
        if let Some(editor) = egui::TextEdit::load_state(root_context, editor_id(document)) {
            egui::TextEdit::store_state(
                &self.hosts.last().expect("host was just added").context,
                editor_id(document),
                editor,
            );
        }
    }

    fn take_reply(&mut self) {
        let Some((id, receiver)) = &self.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => NativeUiResult::Failed,
        };
        let Some(index) = self.hosts.iter().position(|host| host.id == *id) else {
            self.pending = None;
            return;
        };
        self.pending = None;
        let host = &mut self.hosts[index];
        let document = &mut self.documents[host.document];
        match result {
            NativeUiResult::Opened if host.stage == Stage::Opening && document.open && document.detach => {
                host.stage = Stage::Live;
                host.window.set_visible(true);
                host.set_repaint(Duration::ZERO);
                log::info!("native UI opened demo document {}", host.document + 1);
            }
            NativeUiResult::Retry => host.set_repaint(RETRY_DELAY),
            NativeUiResult::Occluded => host.set_repaint(Duration::from_millis(200)),
            NativeUiResult::Failed if host.stage != Stage::Retiring => {
                self.failed = true;
                document.detach = false;
            }
            NativeUiResult::Retired | NativeUiResult::Failed => {
                self.hosts.swap_remove(index);
            }
            NativeUiResult::Opened | NativeUiResult::Presented => {}
        }
    }

    fn submit(&mut self, renderer: &mut Renderer, id: NativeUiId, command: NativeUiCommand) {
        let (reply, receiver) = sync_channel(1);
        // One in-flight operation across these windows. No new egui pass
        // runs until its predecessor was consumed, including its deltas.
        if renderer.queue_native_ui(NativeUiTask { id, command, reply }) {
            self.pending = Some((id, receiver));
        } else {
            self.failed = true;
            self.renderer_lost();
        }
    }
}

fn present_document(ui: &mut egui::Ui, palette: &Palette, chrome: &chrome::Chrome, window: &Window, index: usize, document: &mut Document) {
    if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
        document.open = false;
        document.detach = false;
    }
    match chrome.show(ui, palette, window, &document_title(index)) {
        Some(chrome::Action::Close) => {
            document.open = false;
            document.detach = false;
        }
        Some(chrome::Action::Return) => document.detach = false,
        None => {}
    }
    let frame = widgets::overlay_frame(ui.ctx(), palette).inner_margin(egui::Margin::same(10));
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        show_document(ui, index, document);
    });
}

fn document_title(index: usize) -> String {
    tr!("native-windows-demo-document", number = index + 1)
}

fn editor_id(index: usize) -> egui::Id {
    egui::Id::unique(("native_window_demo_editor", index))
}

fn save_editor(document: &mut Document, host: &Host) {
    document.editor = egui::TextEdit::load_state(&host.context, editor_id(host.document));
}

fn show_document(ui: &mut egui::Ui, index: usize, document: &mut Document) {
    let height = ui.available_height().max(60.0);
    let edit = |ui: &mut egui::Ui, text: &mut String| {
        let editor = egui::TextEdit::multiline(text).id(editor_id(index)).desired_width(f32::INFINITY);
        ui.add_sized([ui.available_width(), height], editor);
    };
    if let Some(model) = &mut document.preview {
        ui.horizontal_top(|ui| {
            let side = (ui.available_width() * 0.35).min(height).min(140.0);
            preview::paint(
                ui,
                editor_id(index).with("preview"),
                Arc::clone(&model.model),
                &mut model.view,
                egui::Vec2::splat(side),
            );
            edit(ui, &mut document.text);
        });
    } else {
        edit(ui, &mut document.text);
    }
}

fn demo_model() -> Arc<preview::Model> {
    // A synthetic textured surface exercises the same paint callback and
    // per-window user texture registration as the viewer's asset previews.
    let normal = [0.0, 0.0, 1.0];
    let vertices = [
        preview::Vertex::new([-1.0, -1.0, 0.0], normal, [0.0, 1.0]),
        preview::Vertex::new([1.0, -1.0, 0.0], normal, [1.0, 1.0]),
        preview::Vertex::new([1.0, 1.0, 0.0], normal, [1.0, 0.0]),
        preview::Vertex::new([-1.0, 1.0, 0.0], normal, [0.0, 0.0]),
    ];
    let image = Arc::new(preview::Image {
        size: [2, 2],
        rgba: Arc::from([238, 128, 60, 255, 78, 202, 216, 255, 77, 106, 208, 255, 208, 99, 198, 255]),
    });
    Arc::new(preview::Model {
        parts: vec![preview::Part {
            diagnostic: false,
            vertices: Arc::from(vertices),
            indices: Arc::from([0, 1, 2, 0, 2, 3]),
            textures: [Some(image), None, None, None],
            material: preview::Material::default(),
            skin: None,
        }],
        center: glam::Vec3::ZERO,
        radius: std::f32::consts::SQRT_2,
    })
}
