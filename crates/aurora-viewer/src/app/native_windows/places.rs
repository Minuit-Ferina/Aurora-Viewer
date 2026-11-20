use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::{App, Instant, UiActions};
use crate::ui;

impl App {
    pub(in crate::app) fn apply_places_actions(&mut self, actions: Vec<ui::places::PlacesAction>, a: &mut UiActions) {
        for pa in actions {
            use ui::places::PlacesAction;
            match pa {
                PlacesAction::Teleport(g) => self.teleport_saved_place(g),
                PlacesAction::TeleportLandmark(asset) => a.landmark = Some(asset),
                PlacesAction::CreateLandmark(dest) => self.open_create_landmark(dest),
                PlacesAction::ShowOnMap(g) => {
                    self.world.map.show_location(g.x, g.y, g.z as f32);
                    self.panels.world_map = true;
                    self.native_windows.reveal(Panel::WorldMap);
                }
                PlacesAction::OpenProfile(source) => {
                    self.world
                        .place_details
                        .open_panel(source, &self.world.map, &mut self.world.landmarks, Instant::now())
                }
                PlacesAction::CloseProfile => self.world.place_details.close_panel(),
                PlacesAction::RemoveHistory(i) => self.world.tp_storage.remove(i),
                PlacesAction::ClearHistory => {
                    // order matters: the back / forward list first
                    self.world.tp_history.purge();
                    self.world.tp_storage.clear();
                }
                PlacesAction::SortByDate(on) => {
                    self.settings.landmarks_by_date = on;
                    self.settings.save();
                }
                PlacesAction::StopConfirmingTeleports => self.stop_confirming_teleports(),
                PlacesAction::Edit(change) => self.apply_inventory_action(ui::inventory::InvAction::Edit(change)),
                PlacesAction::ExpectFavorite => self.world.favorites.expect_added(),
                PlacesAction::MoveFavorite(item, rank) => self.world.favorites.rearrange(&self.world.inventory, item, rank),
                PlacesAction::OpenWindow(source) => {
                    let w = &mut self.world;
                    let serial = w.place_details.open_window(source, &w.map, &mut w.landmarks, Instant::now());
                    self.place_ui.raise(serial);
                }
                PlacesAction::Sort(by) => {
                    self.settings.places_sort = by;
                    self.settings.save();
                }
                PlacesAction::HistoryZone(zone) => {
                    self.places_ui.demo_zone = None;
                    self.settings.tp_history_zone = zone;
                    self.settings.save();
                }
            }
        }
    }

    pub(super) fn reconcile_desktop_places(&mut self) {
        let visible = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx);
        self.native_windows.reconcile(Panel::Places, visible && self.panels.places);
    }

    pub(super) fn paint_desktop_places(&mut self, event_loop: &dyn ActiveEventLoop) {
        let ctx = self.egui_ctx.clone();
        let modal = self.exit_prompt.open;
        let classic = self.settings.classic_places || self.places_ui.demo_classic;
        let favorites = (!classic).then(|| self.world.favorites.list(&self.world.inventory).to_vec());
        let env = favorites.as_ref().map(|favorites| ui::places::aurora::Env {
            sort: self.settings.places_sort,
            favorites,
        });
        let Some(gfx) = &mut self.gfx else { return };
        let mut actions = Vec::new();
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
            Panel::Places,
            |ui, close, _, caption| {
                if modal {
                    ui.disable();
                    caption.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(&ctx);
                let may_close = !modal && ctx.memory(|m| m.top_modal_layer()).is_none();
                if may_close && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W))) {
                    self.panels.places = false;
                    return true;
                }
                let mut content = ui::place_details::Ctx {
                    p: &self.palette,
                    world: &self.world,
                    images: &self.ui_images,
                    emoji: &mut self.emoji,
                    want_names: &mut self.chat_ui.wanted_names,
                    wanted_images: &mut self.place_images,
                    want_groups: &mut self.place_groups,
                };
                actions.extend(self.places_ui.show(
                    &ctx,
                    &mut content,
                    &mut self.panels.places,
                    self.settings.landmarks_by_date,
                    self.settings.confirm_teleports,
                    env.as_ref(),
                    ui::widgets::PanelHost::Desktop { ui, key: () },
                ));
                ui::places::drag::ghost(&ctx, &self.palette);
                requests.extend(ui::context::take_requests(&ctx));
                !self.panels.places
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        if !actions.is_empty() || !requests.is_empty() {
            let mut result = UiActions::default();
            self.apply_places_actions(actions, &mut result);
            for action in requests {
                self.on_ctx_action(action);
            }
            self.handle_actions(result, event_loop);
        }
    }
}
