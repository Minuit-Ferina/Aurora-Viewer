use uuid::Uuid;
use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::{App, UiActions};
use crate::ui;

impl App {
    pub(in crate::app) fn classic_inventory_window(&self) -> bool {
        (!self.demo || !ui::inventory::aurora_demo_active())
            && (self.settings.classic_inventory || (self.demo && ui::inventory::legacy_demo()))
    }

    pub(super) fn reconcile_desktop_inventory(&mut self) {
        let allowed = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx) && !self.world.rlva.has("showinv");
        self.native_windows.reconcile_inventory(|id| {
            allowed
                && id.map_or(self.panels.inventory, |id| {
                    ui::inventory::desktop_window_open(&self.inventory_ui, id)
                })
        });
    }

    pub(super) fn paint_desktop_inventory(&mut self, event_loop: &ActiveEventLoop, id: Option<Uuid>) {
        let classic = self.classic_inventory_window();
        let ctx = self.egui_ctx.clone();
        let before = self.settings.inventory.clone();
        let modal = self.exit_prompt.open;
        let Some(gfx) = &mut self.gfx else { return };
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
            Panel::Inventory(id),
            |ui, close, _| {
                if modal {
                    ui.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                let may_close = !modal && ctx.memory(|m| m.top_modal_layer()).is_none();
                let close = may_close && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                if close {
                    if let Some(id) = id {
                        ui::inventory::close_desktop_window(&mut self.inventory_ui, id);
                    } else {
                        self.panels.inventory = false;
                    }
                    return true;
                }
                actions.extend(ui::inventory::show_placed(
                    &ctx,
                    &self.palette,
                    &self.skin.icons,
                    &mut self.world,
                    &mut self.inventory_ui,
                    &mut self.settings.inventory,
                    &mut self.panels.inventory,
                    self.appearance_ui.pending.is_some(),
                    &self.ui_images,
                    &self.inventory_hover_images.ready,
                    &self.avatar_pics,
                    &self.settings.script_editor,
                    classic,
                    ui::widgets::PanelHost::Desktop { ui, key: id },
                ));
                false
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        self.inventory_ui.preferences_dirty |= self.settings.inventory != before;
        let mut landmark = None;
        for action in actions {
            match action {
                ui::inventory::InvAction::TeleportLandmark(asset) => landmark = Some(asset),
                action => self.apply_inventory_action(action),
            }
        }
        if landmark.is_some() {
            self.handle_actions(
                UiActions {
                    landmark,
                    ..Default::default()
                },
                event_loop,
            );
        }
    }
}
