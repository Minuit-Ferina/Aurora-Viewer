use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::{App, UiActions};
use crate::ui::{self, bars::BarAction, worldmap::WorldMapAction};

impl App {
    pub(in crate::app) fn map_camera(&self, size: egui::Vec2) -> ui::minimap::MapCamera {
        let aspect = (size.x / size.y.max(1.0)).max(0.1);
        ui::minimap::MapCamera {
            pos: self.camera.position,
            dir: self.camera.target - self.camera.position,
            hfov: 2.0 * ((self.camera.fov_y * 0.5).tan() * aspect).atan(),
            far: self.settings.draw_distance,
        }
    }

    pub(in crate::app) fn apply_world_map_actions(&mut self, actions: Vec<WorldMapAction>, ui: &mut UiActions) {
        for action in actions {
            match action {
                WorldMapAction::TrackRegion(name) => {
                    self.region_tracker_ui.track_region(&self.world, name);
                    self.panels.region_tracker = true;
                }
                WorldMapAction::TeleportHome => {
                    ui.bar = BarAction::TeleportHome;
                    self.panels.world_map = false;
                }
                WorldMapAction::TeleportLandmark(asset) => ui.landmark = Some(asset),
                WorldMapAction::TeleportHistory(global) => self.teleport_saved_place(global),
                WorldMapAction::OpenPlace(source) => self.show_place_profile(source),
            }
        }
    }

    pub(super) fn reconcile_desktop_world_map(&mut self) {
        let restricted = self.world.rlva.has("showworldmap");
        if restricted {
            self.panels.world_map = false;
        }
        let visible = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx);
        self.native_windows.reconcile(Panel::WorldMap, visible && self.panels.world_map);
    }

    pub(super) fn paint_desktop_world_map(&mut self, event_loop: &ActiveEventLoop) {
        let Some(root) = self.gfx.as_ref().map(|gfx| gfx.window.inner_size()) else {
            return;
        };
        let camera = self.map_camera(egui::vec2(root.width as f32, root.height as f32));
        let ctx = self.egui_ctx.clone();
        let modal = self.exit_prompt.open;
        let Some(gfx) = &mut self.gfx else { return };
        let pictures = ui::worldmap::MapPictures {
            images: &self.ui_images,
            avatars: &self.avatar_pics,
        };
        let mut actions = Vec::new();
        let mut close_all = false;
        self.native_windows.paint(
            View {
                event_loop,
                renderer: &mut gfx.renderer,
                root: &gfx.window,
                context: &ctx,
                palette: &self.palette,
                shadow: &mut self.egui_shadow,
            },
            Panel::WorldMap,
            |ui, close, _, _| {
                if modal {
                    ui.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                let close = !modal && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                self.panels.world_map &= !close;
                actions.extend(self.worldmap_ui.show(
                    &ctx,
                    &self.palette,
                    &mut self.world,
                    &mut self.map_tiles,
                    &camera,
                    &mut self.settings.maps,
                    self.settings.world_map_style,
                    &pictures,
                    &mut self.panels.world_map,
                    ui::widgets::PanelHost::Desktop { ui, key: () },
                ));
                !self.panels.world_map
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        if !actions.is_empty() {
            let mut result = UiActions::default();
            self.apply_world_map_actions(actions, &mut result);
            self.handle_actions(result, event_loop);
        }
    }
}
