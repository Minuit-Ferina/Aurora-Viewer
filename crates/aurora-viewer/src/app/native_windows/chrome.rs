//! Compact title controls for desktop floaters.

#[cfg(windows)]
mod win32;

use aurora_i18n::tr;
use egui::RichText;
use winit::window::{Window, WindowAttributes};

use crate::theme::Palette;
use crate::ui::{icons, widgets};

pub(super) const DETACH_ICON: &str = "picture-in-picture-on";
const RETURN_ICON: &str = "picture-in-picture-off";

pub(super) fn attributes(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs.with_decorations(false).with_undecorated_shadow(true)
    }
    #[cfg(not(windows))]
    attrs
}

pub(super) struct Chrome {
    icons: icons::Icons,
    #[cfg(windows)]
    caption: Option<win32::Caption>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Close,
    Return,
}

impl Chrome {
    pub(super) fn new(ctx: &egui::Context, window: &Window) -> Self {
        #[cfg(windows)]
        let caption = win32::Caption::install(window);
        #[cfg(windows)]
        let custom = caption.is_some();
        #[cfg(not(windows))]
        let custom = {
            let _ = window;
            false
        };
        #[cfg(windows)]
        if !custom {
            log::warn!("native UI: Windows caption hit testing unavailable, using system decorations");
            window.set_decorations(true);
        }
        let names: &[&str] = if custom {
            &[RETURN_ICON, "x", "minus", "maximize", "minimize"]
        } else {
            &[RETURN_ICON]
        };
        Self {
            icons: icons::Icons::load_embedded(ctx, names),
            #[cfg(windows)]
            caption,
        }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, palette: &Palette, window: &Window, title: &str) -> Option<Action> {
        #[cfg(windows)]
        let custom = self.caption.is_some();
        #[cfg(not(windows))]
        let custom = false;
        let mut action = None;
        // The client frame owns the divider. Carry the former stroke width
        // in the inset to keep the caption controls and hit area in place.
        let frame = widgets::overlay_frame(ui.ctx(), palette)
            .inner_margin(egui::Margin::symmetric(9, 3))
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE);
        let row = frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(22.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if custom {
                        if self.button(ui, palette, "x", tr!("common-close")).clicked() {
                            action = Some(Action::Close);
                        }
                        let maximized = window.is_maximized();
                        let (icon, label) = if maximized {
                            ("minimize", tr!("native-windows-restore"))
                        } else {
                            ("maximize", tr!("native-windows-maximize"))
                        };
                        if self.button(ui, palette, icon, label).clicked() {
                            window.set_maximized(!maximized);
                        }
                        if self.button(ui, palette, "minus", tr!("native-windows-minimize")).clicked() {
                            window.set_minimized(true);
                        }
                    }
                    let back = self.button(ui, palette, RETURN_ICON, tr!("native-windows-return"));
                    if back.clicked() {
                        action = Some(Action::Return);
                    }
                    if custom {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(title).strong().color(palette.ink)).truncate());
                        });
                    }
                    back.rect.left()
                })
                .inner
            })
            .inner
        });
        // The client area starts at the caption edge. Keep the theme's
        // widget spacing inside both areas.
        ui.add_space(-ui.spacing().item_spacing.y);
        #[cfg(windows)]
        if let Some(caption) = &self.caption {
            caption.update(window, ui.ctx().pixels_per_point(), row.inner - 2.0, row.response.rect.bottom());
        }
        #[cfg(not(windows))]
        let _ = row;
        action
    }

    fn button(&self, ui: &mut egui::Ui, palette: &Palette, icon: &str, label: &str) -> egui::Response {
        widgets::caption_button(ui, palette, self.icons.get(icon), label)
    }
}
