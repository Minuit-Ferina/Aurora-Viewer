//! Offline fixture for the event-loop side of native UI presentation.
//!
//! A document stays in the root until the render owner accepts its surface.
//! Closing a presentation hides it immediately and keeps its window alive
//! until the render owner has dropped its reference to the surface.

mod build;
mod chat;
mod chrome;
mod drag;
mod info;
mod input;
mod inventory;
mod notecards;
mod people;
mod placement;
mod places;
mod scripts;
mod worldmap;

use std::collections::BTreeMap;
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

use super::egui_shadow::{EguiShadow, NativeTextures};
use crate::theme::Palette;
use crate::ui::widgets;

const DEMO_MAX: usize = 6;
const RETRY_DELAY: Duration = Duration::from_millis(32);

struct Document {
    open: bool,
    detach: bool,
    text: String,

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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum Panel {
    Demo(usize),
    Conversations,
    Conversation(Option<uuid::Uuid>),
    Inventory(Option<uuid::Uuid>),
    Build,
    Script(uuid::Uuid, uuid::Uuid),
    Notecard(crate::ui::inventory::NotecardKey),
    About,
    GridStatus,
    WorldMap,
    People,
    Places,
    Bumps,
    GroupTitles,
    RegionTracker,
    AvatarRenderSettings,
}

struct Host {
    id: NativeUiId,
    panel: Panel,
    close_requested: bool,
    rehost_blocked: bool,
    return_when_unblocked: bool,
    window: Arc<Window>,
    textures: NativeTextures,
    input: egui_winit::State,
    #[cfg(windows)]
    drag_modifiers: egui::Modifiers,
    handover: EguiHandOver,
    chrome: chrome::Chrome,
    stage: Stage,
    surface: Option<aurora_render::wgpu::Surface<'static>>,
    occluded: bool,
    repaint: Option<Instant>,
    geometry_changed: Option<Instant>,
    dismiss_menus: bool,
}

impl Host {
    fn viewport(&self) -> egui::ViewportId {
        viewport(self.panel)
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
        textures: &mut TextureHandoff<'_>,
        drop: Option<drag::Input>,
        mut presenter: impl FnMut(&mut egui::Ui, &chrome::Chrome, &Window),
    ) -> Painted {
        self.repaint = None;
        let viewport = self.viewport();
        let mut raw = self.input.take_egui_input(&self.window);
        egui_winit::update_viewport_info(raw.viewports.entry(egui::ViewportId::ROOT).or_default(), root_context, root, false);
        let info = raw.viewports.entry(viewport).or_default();
        egui_winit::update_viewport_info(info, root_context, &self.window, false);
        info.parent = Some(egui::ViewportId::ROOT);
        if let Some(drop) = &drop {
            drop.add_events(root_context, &self.window, &mut raw);
        }
        let dismiss_menus = std::mem::take(&mut self.dismiss_menus);
        let mut output = root_context.run_ui(raw, |ui| {
            if dismiss_menus {
                crate::ui::menu::window_geometry_changed(ui.ctx());
            }
            presenter(ui, &self.chrome, &self.window);
        });
        if drop.is_some() {
            egui::DragAndDrop::clear_payload(root_context);
        }

        #[cfg(target_os = "linux")]
        if let Some(cursor) = self.chrome.resize_cursor(&self.window) {
            output.platform_output.cursor_icon = cursor;
        }
        self.input
            .handle_platform_output_with_event_loop(&self.window, event_loop, output.platform_output);
        let mut close_requested = false;
        if let Some(view) = output.viewport_output.remove(&viewport) {
            self.set_repaint(view.repaint_delay);
            let mut viewport_info = egui::ViewportInfo::default();
            let mut actions = Vec::new();
            egui_winit::process_viewport_commands(root_context, &mut viewport_info, view.commands, &self.window, &mut actions);
            close_requested = viewport_info.close_requested();
        }
        let delta = output.textures_delta;
        textures.shadow.note_updates(&delta);
        let child = textures.shadow.for_native(&mut self.textures, &output.shapes);
        textures.shadow.note_frees(&delta);
        textures.root.append(delta);
        Painted {
            close_requested,
            frame: self.handover.frame(root_context, output.shapes, child, output.pixels_per_point),
        }
    }
}

struct TextureHandoff<'a> {
    shadow: &'a mut EguiShadow,
    root: &'a mut egui::TexturesDelta,
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
    root_textures: egui::TexturesDelta,
    detached: BTreeMap<Panel, String>,
    open_failed: bool,
    cross_window_drag: drag::Session,
    placement: BTreeMap<Panel, placement::Geometry>,
    root_input: input::RootInput,
}

impl Drop for NativeWindows {
    fn drop(&mut self) {
        self.root_textures.clear();
    }
}

impl NativeWindows {
    pub(super) fn begin_root_pass(&mut self, ctx: &egui::Context) {
        self.root_input.begin_root_pass(ctx);
    }

    pub(super) fn finish_root_pass(&mut self, ctx: &egui::Context) {
        if !self.hosts.is_empty() || !self.detached.is_empty() {
            self.root_input.remember(ctx);
        }
    }

    pub(super) fn root_pointer(&self, pos: egui::Pos2) -> Option<bool> {
        self.root_input.pointer(pos)
    }

    pub(super) fn root_keyboard(&self) -> Option<input::Keyboard> {
        self.root_input.keyboard()
    }

    pub(super) fn surrender_root_focus(&mut self, ctx: &egui::Context) {
        self.root_input.surrender_focus(ctx);
    }
    pub(super) fn detach(&mut self, panel: Panel, title: String) {
        if let Some(host) = self.hosts.iter().find(|host| host.panel == panel && host.stage == Stage::Live) {
            host.window.focus_window();
        } else {
            self.detached.insert(panel, title);
        }
    }

    pub(super) fn is_native(&self, panel: Panel) -> bool {
        self.hosts.iter().any(|host| host.panel == panel && host.stage == Stage::Live)
    }

    pub(super) fn reveal(&self, panel: Panel) {
        if let Some(host) = self.hosts.iter().find(|host| host.panel == panel && host.stage == Stage::Live) {
            host.window.set_minimized(false);
            host.window.focus_window();
        }
    }

    pub(super) fn reconcile(&mut self, panel: Panel, allowed: bool) {
        if !allowed {
            self.detached.remove(&panel);
        }
    }

    pub(super) fn popped_windows(&self) -> crate::ui::chat::DesktopPopouts {
        crate::ui::chat::DesktopPopouts {
            live: self
                .hosts
                .iter()
                .filter_map(|host| match host.panel {
                    Panel::Conversation(id) if host.stage == Stage::Live => Some(id),
                    _ => None,
                })
                .collect(),
            ..Default::default()
        }
    }

    pub(super) fn reconcile_popouts(&mut self, mut allowed: impl FnMut(Option<uuid::Uuid>) -> bool) {
        self.detached.retain(|panel, _| match *panel {
            Panel::Conversation(id) => allowed(id),
            _ => true,
        });
    }

    pub(super) fn update_chat_presence(&self, ctx: &egui::Context) {
        let mut presence = crate::ui::chat::DesktopPresence::default();
        for host in self.hosts.iter().filter(|host| host.stage == Stage::Live) {
            let seen = host.visible() && host.window.has_focus();
            match host.panel {
                Panel::Conversations => presence.container = Some(seen),
                Panel::Conversation(id) => presence.popouts.push((id, seen)),
                _ => {}
            }
        }
        crate::ui::chat::set_desktop_presence(ctx, presence);
    }

    pub(super) fn request_close_all(&mut self) {
        for host in self.hosts.iter_mut().filter(|host| host.stage == Stage::Live) {
            host.close_requested = true;
        }
    }

    pub(super) fn inventory_windows(&self) -> crate::ui::widgets::DesktopPanels<Option<uuid::Uuid>> {
        crate::ui::widgets::DesktopPanels {
            live: self
                .hosts
                .iter()
                .filter_map(|host| match host.panel {
                    Panel::Inventory(id) if host.stage == Stage::Live => Some(id),
                    _ => None,
                })
                .collect(),
            requested: Vec::new(),
        }
    }

    pub(super) fn reconcile_inventory(&mut self, mut allowed: impl FnMut(Option<uuid::Uuid>) -> bool) {
        self.detached.retain(|panel, _| match *panel {
            Panel::Inventory(id) => allowed(id),
            _ => true,
        });
    }

    pub(super) fn script_windows(&self) -> crate::ui::widgets::DesktopPanels<aurora_net::scripts::ScriptTarget> {
        crate::ui::widgets::DesktopPanels {
            live: self
                .hosts
                .iter()
                .filter_map(|host| match host.panel {
                    Panel::Script(object, item) if host.stage == Stage::Live => Some(aurora_net::scripts::ScriptTarget { object, item }),
                    _ => None,
                })
                .collect(),
            requested: Vec::new(),
        }
    }

    pub(super) fn reconcile_scripts(&mut self, mut state: impl FnMut(aurora_net::scripts::ScriptTarget) -> (bool, bool)) {
        self.detached.retain(|panel, _| {
            let Panel::Script(object, item) = *panel else { return true };
            let (valid, file_picker_open) = state(aurora_net::scripts::ScriptTarget { object, item });
            let Some(host) = self.hosts.iter_mut().find(|host| host.panel == *panel) else {
                return valid;
            };
            host.rehost_blocked = file_picker_open;
            valid && !(host.return_when_unblocked && !file_picker_open)
        });
    }

    pub(super) fn clipboard_text(&mut self, panel: Panel) -> Option<String> {
        self.hosts
            .iter_mut()
            .find(|host| host.panel == panel && host.stage == Stage::Live)?
            .input
            .clipboard_text()
    }

    pub(super) fn reconcile_notecards(&mut self, mut allowed: impl FnMut(crate::ui::inventory::NotecardKey) -> bool) {
        self.detached.retain(|panel, _| match *panel {
            Panel::Notecard(key) => allowed(key),
            _ => true,
        });
    }

    pub(super) fn update_notecard_presence(&self, ctx: &egui::Context) {
        crate::ui::inventory::set_desktop_notecards(
            ctx,
            self.hosts
                .iter()
                .filter_map(|host| match host.panel {
                    Panel::Notecard(key) if host.stage == Stage::Live => Some((key, host.viewport())),
                    _ => None,
                })
                .collect(),
        );
    }

    pub(super) fn has_detached_panels(&self) -> bool {
        !self.detached.is_empty()
    }

    pub(super) fn return_all(&mut self) {
        self.detached.retain(|panel, _| {
            if let Some(host) = self.hosts.iter_mut().find(|host| host.panel == *panel && host.rehost_blocked) {
                host.return_when_unblocked = true;
                true
            } else {
                false
            }
        });
        for document in &mut self.documents {
            document.detach = false;
        }
    }

    pub(super) fn take_open_failure(&mut self) -> bool {
        std::mem::take(&mut self.open_failed)
    }
    pub(super) fn declare_viewports(&self, ctx: &egui::Context) {
        for host in self.hosts.iter().filter(|host| host.stage == Stage::Live) {
            // The application invokes the borrowed presenter on its own repaint clock.
            ctx.show_viewport_deferred(host.viewport(), egui::ViewportBuilder::default(), |_, _| {});
        }
    }

    pub(super) fn root_textures(&mut self, newer: egui::TexturesDelta) -> egui::TexturesDelta {
        let mut delta = std::mem::take(&mut self.root_textures);
        delta.append(newer);
        delta
    }
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
            self.return_all();
        }
        for (index, document) in self.documents.iter_mut().enumerate() {
            if !document.open
                || self
                    .hosts
                    .iter()
                    .any(|host| host.panel == Panel::Demo(index) && host.stage == Stage::Live)
            {
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
                show_document(ui, index, document);
            });
            document.open = keep_open;
            if detach && keep_open {
                document.detach = true;
            }
        }
    }

    pub(super) fn event(&mut self, id: WindowId, event: &WindowEvent, app_modal: bool) -> Option<bool> {
        let host = self.hosts.iter_mut().find(|host| host.window.id() == id)?;
        if host.stage != Stage::Live {
            return Some(false);
        }
        if app_modal
            && matches!(
                event,
                WindowEvent::CloseRequested
                    | WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::Touch(_)
                    | WindowEvent::DroppedFile(_)
            )
        {
            host.input.egui_input_mut().events.clear();
            return Some(false);
        }
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            host.close_requested = true;
            host.set_repaint(Duration::ZERO);
            return Some(false);
        }
        if matches!(
            event,
            WindowEvent::Moved(_) | WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
        ) {
            host.geometry_changed = Some(Instant::now());
            host.dismiss_menus = true;
        }
        if let WindowEvent::Occluded(occluded) = event {
            host.occluded = *occluded;
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if host.chrome.event(&host.window, event) {
            host.set_repaint(Duration::ZERO);
            return Some(true);
        }
        let repaint = host.input.on_window_event(&host.window, event).repaint;
        #[cfg(windows)]
        if matches!(event, WindowEvent::ModifiersChanged(_))
            && let Some(egui::Event::ModifiersChanged(modifiers)) = host.input.egui_input().events.last()
        {
            host.drag_modifiers = *modifiers;
        }
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

    pub(super) fn take_root_drop(&mut self, window: WindowId) -> Option<drag::Input> {
        self.cross_window_drag.frame(window)
    }

    pub(super) fn retire_all(&mut self) {
        self.cross_window_drag.cancel();
        self.placement.clear();
        for document in &mut self.documents {
            document.open = false;
            document.detach = false;
        }
        self.detached.clear();
        for host in &self.hosts {
            host.window.set_visible(false);
        }
    }

    pub(super) fn renderer_lost(&mut self) {
        self.cross_window_drag.cancel();
        self.detached.clear();
        for document in &mut self.documents {
            document.detach = false;
        }
        for host in self.hosts.drain(..) {
            host.window.set_visible(false);
        }
        self.pending = None;
    }

    /// Advance the bounded surface queue and choose the next panel to paint.
    pub(super) fn next(&mut self, event_loop: &ActiveEventLoop, renderer: &mut Renderer, root_context: &egui::Context) -> Option<Panel> {
        self.take_reply();
        for host in &mut self.hosts {
            let desired = match host.panel {
                Panel::Demo(index) => self.documents[index].open && self.documents[index].detach,
                panel => self.detached.contains_key(&panel),
            };
            if host.stage == Stage::Live
                && host
                    .geometry_changed
                    .is_some_and(|at| !desired || at.elapsed() >= Duration::from_millis(300))
            {
                remember_placement(&mut self.placement, root_context, host);
            }
            if host.stage != Stage::Retiring && !desired {
                self.cross_window_drag.forget_window(host.window.id());
                host.stage = Stage::Retiring;
                host.window.set_visible(false);
            }
        }
        if self.pending.is_some() {
            return None;
        }
        if let Some(host) = self.hosts.iter().find(|host| host.stage == Stage::Retiring) {
            self.submit(renderer, host.id, NativeUiCommand::Retire);
            return None;
        }
        if let Some(host) = self.hosts.iter_mut().find(|host| host.stage == Stage::Opening) {
            if let Some(surface) = host.surface.take() {
                let id = host.id;
                let size = host.window.inner_size().into();
                self.submit(renderer, id, NativeUiCommand::Open { surface, size });
            }
            return None;
        }
        let has_host = |panel| self.hosts.iter().any(|host| host.panel == panel);
        let demo = self
            .documents
            .iter()
            .enumerate()
            .position(|(index, document)| document.open && document.detach && !has_host(Panel::Demo(index)))
            .map(Panel::Demo);
        if let Some(panel) = demo.or_else(|| self.detached.keys().copied().find(|panel| !has_host(*panel))) {
            self.open(event_loop, renderer, root_context, panel);
            return None;
        }

        let now = Instant::now();
        let index = (0..self.hosts.len())
            .map(|offset| (self.next_draw + offset) % self.hosts.len())
            .find(|index| {
                let host = &self.hosts[*index];
                host.close_requested || (host.repaint.is_some_and(|at| at <= now) && host.visible())
            })?;
        self.next_draw = index + 1;
        Some(self.hosts[index].panel)
    }

    pub(super) fn paint(
        &mut self,
        view: View<'_>,
        panel: Panel,
        mut presenter: impl FnMut(&mut egui::Ui, bool, &Arc<Window>, &mut egui::Ui) -> bool,
    ) {
        let View {
            event_loop,
            renderer,
            root,
            context,
            palette,
            shadow,
        } = view;
        let Some(host) = self.hosts.iter_mut().find(|host| host.panel == panel && host.stage == Stage::Live) else {
            return;
        };
        let id = host.id;
        let size = host.window.inner_size().into();
        let mut close = std::mem::take(&mut host.close_requested);
        let mut rehost = false;
        let mut deferred = false;
        let blocked = host.rehost_blocked;
        let owner = Arc::clone(&host.window);
        let title = match panel {
            Panel::Demo(index) => document_title(index),
            _ => self.detached[&panel].clone(),
        };
        let mut textures = TextureHandoff {
            shadow,
            root: &mut self.root_textures,
        };
        let drop = self.cross_window_drag.frame(host.window.id());
        let mut painted = host.paint(event_loop, root, context, &mut textures, drop, |ui, chrome, window| {
            let mut caption = chrome.show(ui, palette, window);
            rehost |= caption.action.is_some_and(|action| {
                close |= action == chrome::Action::Close;
                deferred |= action == chrome::Action::Return && blocked;
                action == chrome::Action::Return && !blocked
            });
            if !rehost {
                match panel {
                    Panel::Demo(index) => {
                        let document = &mut self.documents[index];
                        close |= ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::W));
                        if !close {
                            present_document(ui, palette, index, document);
                        }
                    }
                    _ => close = presenter(ui, close, &owner, &mut caption.actions),
                }
            }
            chrome.finish(caption, palette, window, &title);
        });
        #[cfg(windows)]
        self.cross_window_drag.restore_capture_cursor(host.window.id());
        self.root_input.child_pass_finished();
        host.return_when_unblocked |= deferred;
        if !matches!(panel, Panel::Demo(_)) {
            // Model updates can come from the network with no local window event.
            host.set_repaint(Duration::from_millis(125));
        }
        if painted.close_requested {
            host.close_requested = true;
            host.set_repaint(Duration::ZERO);
        }
        if close || rehost {
            self.cross_window_drag.forget_window(host.window.id());
            remember_placement(&mut self.placement, context, host);
            painted.frame.textures_delta.clear();
            host.stage = Stage::Retiring;
            host.window.set_visible(false);
            match panel {
                Panel::Demo(index) => {
                    self.documents[index].open &= !close;
                    self.documents[index].detach = false;
                }
                _ => {
                    self.detached.remove(&panel);
                }
            }
            return;
        }
        host.window.pre_present_notify();
        self.submit(
            renderer,
            id,
            NativeUiCommand::Draw {
                size,
                frame: painted.frame.into(),
            },
        );
    }

    fn open(&mut self, event_loop: &ActiveEventLoop, renderer: &Renderer, root_context: &egui::Context, panel: Panel) {
        let (title, size, minimum) = match panel {
            Panel::Demo(index) => (
                format!("{} — {}", document_title(index), crate::cli::window_title()),
                LogicalSize::new(420.0, 270.0),
                LogicalSize::new(260.0, 160.0),
            ),
            Panel::Inventory(_) | Panel::Build | Panel::About | Panel::GridStatus => (
                self.detached[&panel].clone(),
                LogicalSize::new(520.0, 680.0),
                LogicalSize::new(320.0, 320.0),
            ),
            Panel::Bumps => (
                self.detached[&panel].clone(),
                LogicalSize::new(480.0, 250.0),
                LogicalSize::new(300.0, 160.0),
            ),
            Panel::GroupTitles => (
                self.detached[&panel].clone(),
                LogicalSize::new(640.0, 540.0),
                LogicalSize::new(520.0, 340.0),
            ),
            Panel::RegionTracker | Panel::AvatarRenderSettings => (
                self.detached[&panel].clone(),
                LogicalSize::new(620.0, 420.0),
                LogicalSize::new(460.0, 260.0),
            ),
            Panel::Places => (
                self.detached[&panel].clone(),
                LogicalSize::new(620.0, 620.0),
                LogicalSize::new(320.0, 360.0),
            ),
            Panel::People => (
                self.detached[&panel].clone(),
                LogicalSize::new(580.0, 620.0),
                LogicalSize::new(320.0, 280.0),
            ),
            Panel::WorldMap => (
                self.detached[&panel].clone(),
                LogicalSize::new(1040.0, 640.0),
                LogicalSize::new(450.0, 390.0),
            ),
            Panel::Notecard(_) | Panel::Script(..) | Panel::Conversations | Panel::Conversation(_) => (
                self.detached[&panel].clone(),
                LogicalSize::new(760.0, 480.0),
                LogicalSize::new(320.0, 270.0),
            ),
        };
        let saved =
            self.placement.get(&panel).copied().or_else(|| {
                placement_key(panel).and_then(|key| root_context.data_mut(|data| data.get_persisted::<placement::Geometry>(key)))
            });
        let os_title = if matches!(panel, Panel::Demo(_)) {
            title
        } else if let Some(marker) = crate::cli::get()
            .title
            .as_deref()
            .filter(|title| title.contains("[AUTOMATION - DO NOT TOUCH]"))
        {
            format!("{title} — {marker}")
        } else {
            title
        };
        let mut attrs = chrome::attributes(Window::default_attributes())
            .with_title(os_title)
            .with_inner_size(size)
            .with_min_inner_size(minimum)
            .with_active(false)
            .with_visible(false);
        if let Some(saved) = saved
            && let Some((position, size)) = saved.restore(event_loop)
        {
            attrs = attrs.with_position(position).with_inner_size(size);
        }
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
                self.reject(panel);
                return;
            }
        };
        self.next_id += 1;
        let id = NativeUiId(self.next_id);

        let input = egui_winit::State::new(
            root_context.clone(),
            viewport(panel),
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(renderer.device.limits().max_texture_dimension_2d as usize),
        );
        let chrome = chrome::Chrome::new(root_context, &window);
        self.hosts.push(Host {
            id,
            panel,
            close_requested: false,
            rehost_blocked: false,
            return_when_unblocked: false,
            window,
            textures: NativeTextures::default(),
            input,
            #[cfg(windows)]
            drag_modifiers: egui::Modifiers::NONE,
            handover: EguiHandOver::default(),
            chrome,
            stage: Stage::Opening,
            surface: Some(surface),
            occluded: false,
            repaint: None,
            geometry_changed: None,
            dismiss_menus: false,
        });
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
        let panel = host.panel;
        let desired = match panel {
            Panel::Demo(index) => self.documents[index].open && self.documents[index].detach,
            _ => self.detached.contains_key(&panel),
        };
        match result {
            NativeUiResult::Opened if host.stage == Stage::Opening && desired => {
                host.stage = Stage::Live;
                host.window.set_visible(true);
                host.set_repaint(Duration::ZERO);
                if !matches!(panel, Panel::Demo(_)) {
                    host.window.focus_window();
                }
                log::info!("native UI opened a desktop panel");
            }
            NativeUiResult::Retry => host.set_repaint(RETRY_DELAY),
            NativeUiResult::Occluded => host.set_repaint(Duration::from_millis(200)),
            NativeUiResult::Failed if host.stage != Stage::Retiring => self.reject(panel),
            NativeUiResult::Retired | NativeUiResult::Failed => {
                self.hosts.swap_remove(index);
            }
            NativeUiResult::Opened | NativeUiResult::Presented => {}
        }
    }

    fn reject(&mut self, panel: Panel) {
        match panel {
            Panel::Demo(index) => {
                self.failed = true;
                self.documents[index].detach = false;
            }
            _ => {
                self.open_failed = true;
                self.detached.remove(&panel);
            }
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

pub(super) struct View<'a> {
    pub event_loop: &'a ActiveEventLoop,
    pub renderer: &'a mut Renderer,
    pub root: &'a Window,
    pub context: &'a egui::Context,
    pub palette: &'a Palette,
    pub shadow: &'a mut EguiShadow,
}

fn placement_key(panel: Panel) -> Option<egui::Id> {
    let panel_type = match panel {
        Panel::Conversations => "conversations",
        Panel::Conversation(None) => "nearby-chat",
        Panel::Inventory(None) => "inventory",
        Panel::Build => "build",
        Panel::About => "about",
        Panel::GridStatus => "grid-status",
        Panel::WorldMap => "world-map",
        Panel::People => "people",
        Panel::Places => "places",
        Panel::Bumps => "bumps",
        Panel::GroupTitles => "group-titles",
        Panel::RegionTracker => "region-tracker",
        Panel::AvatarRenderSettings => "avatar-render-settings",
        _ => return None,
    };
    Some(egui::Id::unique(("native_window_placement", panel_type)))
}

fn remember_placement(placements: &mut BTreeMap<Panel, placement::Geometry>, ctx: &egui::Context, host: &mut Host) {
    host.geometry_changed = None;
    if let Some(geometry) = placement::Geometry::from_window(&host.window, host.chrome.is_maximized(&host.window)) {
        placements.insert(host.panel, geometry);
        if let Some(key) = placement_key(host.panel) {
            ctx.data_mut(|data| data.insert_persisted(key, geometry));
        }
    }
}

fn viewport(panel: Panel) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("aurora_desktop_panel", panel))
}

fn present_document(ui: &mut egui::Ui, palette: &Palette, index: usize, document: &mut Document) {
    widgets::desktop_client(ui, palette, egui::Margin::same(10), true, |ui| {
        show_document(ui, index, document);
    });
}

fn document_title(index: usize) -> String {
    tr!("native-windows-demo-document", number = index + 1)
}

fn editor_id(index: usize) -> egui::Id {
    egui::Id::unique(("native_window_demo_editor", index))
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
