//! Compact title controls for desktop floaters.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod client;
#[cfg(windows)]
mod win32;

use aurora_i18n::tr;
use egui::RichText;
use winit::window::{Window, WindowAttributes};

use crate::theme::Palette;
use crate::ui::{icons, widgets};

pub(super) const DETACH_ICON: &str = "picture-in-picture-on";
const RETURN_ICON: &str = "picture-in-picture-off";
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
const EDGE: f64 = 5.0;

pub(super) fn attributes(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs.with_decorations(false).with_undecorated_shadow(true)
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attrs.with_titlebar_hidden(true)
    }
    #[cfg(target_os = "linux")]
    {
        attrs.with_decorations(false)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    attrs
}

pub(super) struct Chrome {
    icons: icons::Icons,
    custom: bool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    client: client::Caption,
    #[cfg(windows)]
    caption: Option<win32::Caption>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Close,
    Return,
}

pub(super) struct Caption {
    pub(super) action: Option<Action>,
    pub(super) actions: egui::Ui,
    bottom: f32,
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
            cfg!(any(target_os = "linux", target_os = "macos"))
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
            custom,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            client: client::Caption::default(),
            #[cfg(windows)]
            caption,
        }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, palette: &Palette, window: &Window) -> Caption {
        let custom = self.custom;
        let mut action = None;
        // The panel's layout decides whether to draw a divider. Keep the
        // caption controls and hit area inset by the former stroke width.
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
                    let mut rect = ui.available_rect_before_wrap();
                    rect.min.y = back.rect.top();
                    rect.max.y = back.rect.bottom();
                    ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt("desktop_caption_actions")
                            .max_rect(rect)
                            .layout(*ui.layout()),
                    )
                })
                .inner
            })
            .inner
        });
        // The client area starts at the caption edge. Keep the theme's
        // widget spacing inside both areas.
        ui.add_space(-ui.spacing().item_spacing.y);
        Caption {
            action,
            actions: row.inner,
            bottom: row.response.rect.bottom(),
        }
    }

    pub(super) fn finish(&self, mut row: Caption, palette: &Palette, window: &Window, title: &str) {
        if !self.custom {
            return;
        }
        let controls_left = row.actions.available_rect_before_wrap().right();
        let pixels_per_point = row.actions.ctx().pixels_per_point();
        #[cfg(windows)]
        if let Some(caption) = &self.caption {
            caption.update(window, pixels_per_point, controls_left, row.bottom);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.client.update(pixels_per_point, controls_left, row.bottom);
        #[cfg(not(windows))]
        let _ = window;
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        let _ = (pixels_per_point, controls_left, row.bottom);
        row.actions.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(title).strong().color(palette.ink)).truncate());
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.client.event(window, event)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn resize_cursor(&self, window: &Window) -> Option<egui::CursorIcon> {
        self.client.resize_cursor(window)
    }

    fn button(&self, ui: &mut egui::Ui, palette: &Palette, icon: &str, label: &str) -> egui::Response {
        widgets::caption_button(ui, palette, self.icons.get(icon), label)
    }
}
