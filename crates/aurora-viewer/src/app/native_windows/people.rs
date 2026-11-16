use std::collections::HashMap;

use uuid::Uuid;
use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::{App, UiActions};
use crate::ui;

impl App {
    pub(in crate::app) fn ui_voice_levels(&self, ctx: &egui::Context) -> HashMap<Uuid, (f32, bool)> {
        if self.demo {
            static VOICE_MODE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
            let mut levels = crate::demo::voice_levels(
                ctx.input(|i| i.time),
                self.world.agent_id,
                self.settings.audio.voice_enabled,
                self.talking(),
                self.voice.mic_level,
                VOICE_MODE.get_or_init(|| std::env::var("AURORA_DEMO_VOICE").ok()).as_deref(),
            );
            crate::demo::tags::voice(&mut levels, ctx.input(|i| i.time));
            crate::demo::people::voice(&mut levels, ctx.input(|i| i.time));
            crate::demo::voice_controls::voice(&mut levels, ctx.input(|i| i.time));
            levels
        } else {
            self.voice.levels(self.world.agent_id)
        }
    }

    pub(in crate::app) fn apply_minimap_actions(&mut self, actions: Vec<ui::minimap::MiniMapAction>) {
        for ma in actions {
            match ma {
                ui::minimap::MiniMapAction::Teleport { x, y, z } => self.world.map.track_location(x, y, z, true),
                ui::minimap::MiniMapAction::Track { x, y, z } => self.world.map.track_location(x, y, z, false),
                ui::minimap::MiniMapAction::StopTracking => self.world.map.clear_track(),
                ui::minimap::MiniMapAction::OpenWorldMap => {
                    self.panels.world_map = true;
                    self.native_windows.reveal(Panel::WorldMap);
                }
                ui::minimap::MiniMapAction::Profile(id) => self.profile_ui.open(&mut self.world, id),
                ui::minimap::MiniMapAction::AboutLand(gx, gy) => {
                    ui::land::LandUi::select_at_global(&mut self.world, gx, gy);
                    self.panels.about_land = true;
                }
                ui::minimap::MiniMapAction::Colors(k) => self.settings.colors = k,
                ui::minimap::MiniMapAction::Close => self.panels.minimap = false,
            }
        }
    }

    pub(super) fn reconcile_desktop_people(&mut self) {
        let visible = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx);
        self.native_windows.reconcile(Panel::People, visible && self.panels.people);
    }

    pub(super) fn paint_desktop_people(&mut self, event_loop: &dyn ActiveEventLoop) {
        let Some(root) = self.gfx.as_ref().map(|gfx| gfx.window.surface_size()) else {
            return;
        };
        let camera = self.map_camera(egui::vec2(root.width as f32, root.height as f32));
        let ctx = self.egui_ctx.clone();
        let second_life = self.second_life();
        let voice = self.ui_voice_levels(&ctx);
        let modal = self.exit_prompt.open;
        let Some(gfx) = &mut self.gfx else { return };
        self.settings.radar.alert_sounds = self.settings.audio.ui.radar_sounds();
        let before = (self.settings.radar.clone(), self.settings.friends_list.clone());
        let mut actions = Vec::new();
        let mut map_actions = Vec::new();
        let mut contacts = Vec::new();
        let mut requests = Vec::new();
        let mut close_all = false;
        self.native_windows.paint(
            View {
                event_loop,
                renderer: &mut gfx.renderer,
                root: gfx.window.as_ref(),
                context: &ctx,
                palette: &self.palette,
                shadow: &mut self.egui_shadow,
            },
            Panel::People,
            |ui, close, _, caption| {
                if modal {
                    ui.disable();
                    caption.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(&ctx);
                let may_close = !modal && ctx.memory(|m| m.top_modal_layer()).is_none();
                if may_close {
                    ui::close_key::begin(&ctx);
                }
                if may_close && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W))) {
                    self.panels.people = false;
                    return true;
                }
                let mut radar = ui::people::RadarData {
                    radar: &self.radar,
                    settings: &mut self.settings.radar,
                    voice: &voice,
                    draw_distance: self.settings.draw_distance,
                    second_life,
                    now: self.radar_clock.elapsed().as_secs_f64(),
                    pics: &self.avatar_pics,
                };
                let mut map = |ui: &mut egui::Ui, size: egui::Vec2| {
                    map_actions.extend(self.people_map_ui.draw(
                        ui,
                        &self.palette,
                        &self.world,
                        &mut self.map_tiles,
                        &camera,
                        &mut self.settings.maps,
                        size,
                        true,
                    ));
                };
                self.contacts_ui.set_people_origin(true);
                actions.extend(ui::people::people_window(
                    &ctx,
                    &self.palette,
                    &self.world,
                    &mut self.panels,
                    &mut self.people_ui,
                    &mut radar,
                    &mut self.settings.friends_list,
                    ui::people::ContactsParts {
                        ui: &mut self.contacts_ui,
                        images: &self.ui_images,
                    },
                    &mut map,
                    !self.settings.classic_people && !(self.demo && crate::demo::people::legacy()),
                    ui::widgets::PanelHost::Desktop { ui, key: () },
                ));
                if !modal {
                    contacts.extend(ui::contacts::dialog_windows(
                        &ctx,
                        &self.palette,
                        &mut self.world,
                        &mut self.contacts_ui,
                        Some(true),
                    ));
                }
                self.contacts_ui.set_people_origin(false);
                requests.extend(ui::context::take_requests(&ctx));
                !self.panels.people
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        if self.settings.radar.alert_sounds != before.0.alert_sounds {
            self.settings.audio.ui.set_radar_sounds(self.settings.radar.alert_sounds);
        }
        if before != (self.settings.radar.clone(), self.settings.friends_list.clone()) {
            self.settings.save();
        }
        self.apply_minimap_actions(map_actions);
        if !actions.is_empty() || !contacts.is_empty() || !requests.is_empty() {
            self.contacts_ui.set_people_origin(true);
            let mut result = UiActions::default();
            for action in requests {
                self.on_ctx_action(action);
            }
            self.apply_people_actions(actions, &mut result);
            for action in contacts {
                self.on_contacts_action(action, &mut result);
            }
            self.handle_actions(result, event_loop);
            self.contacts_ui.set_people_origin(false);
        }
    }
}
