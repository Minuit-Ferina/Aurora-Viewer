use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::App;
use crate::ui::{
    self,
    inventory::{DesktopNotecard, InvAction, NotecardKey},
};

impl App {
    pub(super) fn reconcile_desktop_notecards(&mut self) {
        let allowed = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx) && !self.world.rlva.has("viewnote");
        for key in ui::inventory::take_desktop_notecard_requests(&self.egui_ctx) {
            if allowed {
                self.native_windows
                    .detach(Panel::Notecard(key), aurora_i18n::tr!("inventory-kind-notecard").to_owned());
            }
        }
        let matches = |state: &ui::inventory::InventoryUi, key: NotecardKey| state.ui_id == key.editor && state.preview == Some(key.item);
        self.native_windows.reconcile_notecards(|key| {
            allowed
                && (matches(&self.inventory_ui, key)
                    || self.inventory_ui.windows.iter().any(|w| matches(&w.state, key))
                    || self.task_notecards.values().any(|d| matches(&d.state, key)))
        });
    }

    pub(super) fn paint_desktop_notecard(&mut self, event_loop: &ActiveEventLoop, key: NotecardKey) {
        let ctx = self.egui_ctx.clone();
        let modal = self.exit_prompt.open;
        let view_before = self.settings.inventory.notecard_view;
        let task = self
            .task_notecards
            .iter()
            .find(|(_, d)| d.state.ui_id == key.editor)
            .map(|(k, _)| *k);
        let metadata = task.and_then(|(object, item)| self.task_notecard_item(object, item));
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
            Panel::Notecard(key),
            |ui, close, _| {
                if modal {
                    ui.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                let may_close = !modal && ctx.memory(|m| m.top_modal_layer()).is_none();
                let close = may_close && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                let god_level = self.world.god_level;
                let world_inventory = &self.world.inventory;
                let (state, inventory, god_level) = if let Some(task) = task {
                    let Some(d) = self.task_notecards.get_mut(&task) else { return true };
                    d.refresh_permissions(key.item, metadata.as_ref());
                    (&mut d.state, &d.inventory, 0)
                } else if self.inventory_ui.ui_id == key.editor {
                    (&mut self.inventory_ui, world_inventory, god_level)
                } else if let Some(w) = self.inventory_ui.windows.iter_mut().find(|w| w.state.ui_id == key.editor) {
                    (w.state.as_mut(), world_inventory, god_level)
                } else {
                    return true;
                };
                actions.extend(ui::inventory::show_document_placed(
                    &ctx,
                    &self.palette,
                    &self.skin.icons,
                    inventory,
                    state,
                    world_inventory,
                    &self.ui_images,
                    &mut self.settings.inventory,
                    &self.settings.script_editor,
                    god_level,
                    Some(DesktopNotecard { ui, key, close }),
                ));
                state.preview != Some(key.item)
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        if view_before != self.settings.inventory.notecard_view {
            self.settings.save();
        }
        for action in actions {
            match (task, action) {
                (Some((object, item)), InvAction::SaveContent { text, close, .. }) => self.save_task_notecard(object, item, text, close),
                (
                    Some((object, _)),
                    InvAction::Embedded {
                        editor,
                        notecard,
                        marker,
                        copy,
                    },
                ) => self.open_notecard_attachment(editor, notecard, marker, copy, object),
                (Some((object, item)), InvAction::Edit(change)) => self.apply_task_note_description(object, item, change),
                (None, action) => self.apply_inventory_action(action),
                _ => {}
            }
        }
    }
}
