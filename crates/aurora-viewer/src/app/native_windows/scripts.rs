use aurora_net::scripts::ScriptTarget;
use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::App;
use crate::ui;

impl App {
    pub(super) fn reconcile_desktop_scripts(&mut self) {
        let allowed = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx) && !self.world.rlva.has("viewscript");
        self.native_windows.reconcile_scripts(|target| {
            (
                allowed && self.script_editors.documents.iter().any(|editor| editor.target == target),
                self.script_editors.file_picker_open(target),
            )
        });
    }

    pub(super) fn paint_desktop_script(&mut self, event_loop: &ActiveEventLoop, target: ScriptTarget) {
        self.refresh_script_presenters();
        let arm_before = self.color_pipette();
        let ctx = self.egui_ctx.clone();
        let modal = self.exit_prompt.open;
        let Some(gfx) = &mut self.gfx else { return };
        let mut actions = Vec::new();
        let mut close_all = false;
        let panel = Panel::Script(target.object, target.item);
        self.native_windows.paint(
            View {
                event_loop,
                renderer: &mut gfx.renderer,
                root: &gfx.window,
                context: &ctx,
                palette: &self.palette,
                shadow: &mut self.egui_shadow,
            },
            panel,
            |ui, close, owner, _| {
                if modal {
                    ui.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                let may_close = !modal && !self.script_editors.file_picker_open(target);
                let close = may_close && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                actions.extend(self.script_editors.show_placed(
                    &ctx,
                    &self.palette,
                    &mut self.settings.script_editor,
                    &mut self.settings.build.color_picker,
                    ui::scripts::EditorHost {
                        placement: ui::widgets::PanelHost::Desktop { ui, key: target },
                        parent: Some(owner),
                        close: close.then_some(target),
                    },
                ));
                !self.script_editors.documents.iter().any(|editor| editor.target == target)
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        for action in actions {
            if let ui::scripts::Action::Paste(target) = action {
                if let Some(text) = self.native_windows.clipboard_text(panel) {
                    self.script_editors.paste(&ctx, target, &text);
                }
            } else {
                self.apply_script_action(action);
            }
        }
        if !arm_before
            && self.color_pipette()
            && let Some(gfx) = &self.gfx
        {
            gfx.window.focus_window();
        }
    }
}
