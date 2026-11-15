use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::{app::App, ui};

impl App {
    pub(in crate::app) fn bumps_window(&mut self, ctx: &egui::Context, palette: &crate::theme::Palette) {
        let panel = Panel::Bumps;
        if self.native_windows.is_native(panel) {
            return;
        }
        let mut desktop = ui::widgets::DesktopPanels::default();
        self.bumps_ui.show(
            ctx,
            palette,
            &self.world,
            &mut self.panels.bumps,
            ui::widgets::PanelHost::Viewer(&mut desktop),
        );
        if !desktop.requested.is_empty() {
            self.native_windows.detach(panel, aurora_i18n::tr!("bumps-title").to_owned());
        }
    }

    pub(super) fn reconcile_desktop_info(&mut self) {
        let visible = self.in_world() && !ui::widgets::interface_hidden(&self.egui_ctx);
        self.native_windows.reconcile(Panel::About, visible && self.panels.about);
        self.native_windows.reconcile(Panel::GridStatus, visible && self.panels.grid_status);
        self.native_windows.reconcile(Panel::Bumps, visible && self.panels.bumps);
        self.native_windows
            .reconcile(Panel::GroupTitles, visible && self.panels.group_titles);
    }

    pub(super) fn paint_desktop_info(&mut self, event_loop: &ActiveEventLoop, panel: Panel) {
        let ctx = self.egui_ctx.clone();
        let modal = self.exit_prompt.open;
        if panel == Panel::About {
            self.refresh_about(&ctx);
        }
        let sections = self
            .gfx
            .as_ref()
            .filter(|_| panel == Panel::About)
            .map(|gfx| self.about_sections(&gfx.renderer));
        let Some(gfx) = &mut self.gfx else { return };
        let mut close_all = false;
        let mut refresh = false;
        let mut titles = Vec::new();
        let region = (panel == Panel::GroupTitles)
            .then(|| Self::group_titles_region(&self.world))
            .flatten();
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
            |ui, close, _, _| {
                if modal {
                    ui.disable();
                }
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                let close = !modal && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                let host = ui::widgets::PanelHost::Desktop { ui, key: () };
                match panel {
                    Panel::About => {
                        self.panels.about &= !close;
                        if let Some(sections) = &sections {
                            let view = ui::about::AboutView {
                                sections: sections.clone(),
                                logo: self.logo.as_ref(),
                            };
                            self.about.ui.show_placed(&ctx, &self.palette, &view, &mut self.panels.about, host);
                        }
                        !self.panels.about
                    }
                    Panel::GridStatus => {
                        self.panels.grid_status &= !close;
                        match ui::grid_status::show(&ctx, &self.palette, &self.grid_status, &mut self.panels.grid_status, host) {
                            Some(ui::grid_status::Action::Refresh) => refresh = true,
                            Some(ui::grid_status::Action::Open(url)) => ctx.open_url(egui::OpenUrl::new_tab(url)),
                            None => {}
                        }
                        !self.panels.grid_status
                    }
                    Panel::Bumps => {
                        self.panels.bumps &= !close;
                        self.bumps_ui.show(&ctx, &self.palette, &self.world, &mut self.panels.bumps, host);
                        !self.panels.bumps
                    }
                    Panel::GroupTitles => {
                        if close {
                            self.panels.group_titles = false;
                            self.group_titles_ui.closed();
                        }
                        let view = Self::group_titles_view(&self.world, region.as_deref());
                        titles.extend(
                            self.group_titles_ui
                                .show(&ctx, &self.palette, &view, &mut self.panels.group_titles, host),
                        );
                        !self.panels.group_titles
                    }
                    _ => true,
                }
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        for action in titles {
            self.on_group_titles_action(action, region.as_deref());
        }
        if refresh {
            self.grid_status.refresh();
        }
    }
}
