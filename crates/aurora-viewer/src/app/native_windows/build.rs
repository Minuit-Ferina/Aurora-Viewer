use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::App;
use crate::{build, ui};

impl App {
    pub(super) fn reconcile_desktop_build(&mut self) {
        let allowed = self.in_world() && self.build.open && !ui::widgets::interface_hidden(&self.egui_ctx) && !self.world.rlva.has("edit");
        self.native_windows.reconcile(Panel::Build, allowed);
    }

    pub(super) fn paint_desktop_build(&mut self, event_loop: &dyn ActiveEventLoop) {
        let ctx = self.egui_ctx.clone();
        let pipette = self.color_pipette() || self.texture_pipette();
        let modal = self.exit_prompt.open;
        let Some(gfx) = &mut self.gfx else { return };
        let mut changed = false;
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
            Panel::Build,
            |ui, close, _, _| {
                if modal {
                    ui.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                let may_close = !modal && ctx.memory(|m| m.top_modal_layer()).is_none();
                let close = may_close && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                if close {
                    self.build.close(&mut self.world, &mut self.settings.build);
                    return true;
                }
                let mods = ui.input(|i| build::Mods {
                    shift: i.modifiers.shift,
                    ctrl: i.modifiers.command || i.modifiers.ctrl,
                    alt: i.modifiers.alt,
                });
                if !modal {
                    build::ui::shortcuts(&ctx, &mut self.build, &mut self.world, &mut self.settings.build);
                }
                self.settings.build.show_parcel_owners = self.settings.world_overlays.land_owners;
                let mut env = build::ui::Env {
                    images: &self.ui_images,
                    media: &mut self.media,
                    scene: &mut self.scene,
                    zoom_fraction: self.camera.zoom_fraction(),
                    property_lines: &mut self.settings.world_overlays.property_lines,
                    demo: self.demo,
                };
                changed |= build::ui::show(
                    &ctx,
                    &self.palette,
                    &mut self.build,
                    &mut self.world,
                    &mut self.settings.build,
                    mods,
                    &mut env,
                    ui::widgets::PanelHost::Desktop { ui, key: () },
                );
                self.settings.world_overlays.land_owners = self.settings.build.show_parcel_owners;
                !self.build.open
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        if changed {
            self.settings.save();
        }
        self.flush_build();
        if !pipette
            && (self.color_pipette() || self.texture_pipette())
            && let Some(gfx) = &self.gfx
        {
            gfx.window.focus_window();
        }
    }
}
