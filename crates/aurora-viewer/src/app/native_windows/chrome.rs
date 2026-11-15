//! Compact title controls for desktop floaters.

use aurora_i18n::tr;
use egui::{CursorIcon, Pos2, Rect, RichText, Sense};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::window::{ResizeDirection, Window, WindowAttributes};

use crate::theme::Palette;
use crate::ui::{icons, widgets};

pub(super) const DETACH_ICON: &str = "picture-in-picture-on";
const RETURN_ICON: &str = "picture-in-picture-off";
const CLIENT_TITLE: bool = cfg!(windows);
const EDGE: f64 = 5.0;

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
    pointer: Option<PhysicalPosition<f64>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Close,
    Return,
}

impl Chrome {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let names: &[&str] = if CLIENT_TITLE {
            &[RETURN_ICON, "x", "minus", "maximize", "minimize"]
        } else {
            &[RETURN_ICON]
        };
        Self {
            icons: icons::Icons::load_embedded(ctx, names),
            pointer: None,
        }
    }

    pub(super) fn event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        if !CLIENT_TITLE {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => self.pointer = Some(*position),
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: ElementState::Pressed,
                ..
            } => {
                return self
                    .edge(window)
                    .is_some_and(|direction| window.drag_resize_window(direction).is_ok());
            }
            _ => {}
        }
        false
    }

    pub(super) fn cursor(&self, window: &Window) -> Option<CursorIcon> {
        use ResizeDirection::*;
        Some(match self.edge(window)? {
            North | South => CursorIcon::ResizeVertical,
            East | West => CursorIcon::ResizeHorizontal,
            NorthEast | SouthWest => CursorIcon::ResizeNeSw,
            NorthWest | SouthEast => CursorIcon::ResizeNwSe,
        })
    }

    fn edge(&self, window: &Window) -> Option<ResizeDirection> {
        if !CLIENT_TITLE || window.is_maximized() {
            return None;
        }
        resize_edge(self.pointer?, window.inner_size(), EDGE * window.scale_factor())
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, palette: &Palette, window: &Window, title: &str) -> Option<Action> {
        let mut action = None;
        let frame = widgets::overlay_frame(ui.ctx(), palette).inner_margin(egui::Margin::symmetric(8, 2));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(22.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if CLIENT_TITLE {
                        if self.button(ui, palette, "x", tr!("common-close")) {
                            action = Some(Action::Close);
                        }
                        let maximized = window.is_maximized();
                        let (icon, label) = if maximized {
                            ("minimize", tr!("native-windows-restore"))
                        } else {
                            ("maximize", tr!("native-windows-maximize"))
                        };
                        if self.button(ui, palette, icon, label) {
                            window.set_maximized(!maximized);
                        }
                        if self.button(ui, palette, "minus", tr!("native-windows-minimize")) {
                            window.set_minimized(true);
                        }
                    }
                    if self.button(ui, palette, RETURN_ICON, tr!("native-windows-return")) {
                        action = Some(Action::Return);
                    }
                    if CLIENT_TITLE {
                        self.title(ui, palette, window, title);
                    }
                });
            });
        });
        action
    }

    fn button(&self, ui: &mut egui::Ui, palette: &Palette, icon: &str, label: &str) -> bool {
        widgets::caption_button(ui, palette, self.icons.get(icon), label).clicked()
    }

    fn title(&self, ui: &mut egui::Ui, palette: &Palette, window: &Window, title: &str) {
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let response = ui.add(egui::Label::new(RichText::new(title).strong().color(palette.ink)).truncate());
            let rect = Rect::from_min_max(
                Pos2::new(response.rect.left(), response.rect.top()),
                Pos2::new(ui.max_rect().right(), response.rect.bottom()),
            );
            let drag = ui.interact(rect, ui.scope_id().with("title"), Sense::click_and_drag());
            if drag.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag.double_clicked_by(egui::PointerButton::Primary) {
                window.set_maximized(!window.is_maximized());
            }
            if drag.secondary_clicked()
                && let Some(pointer) = self.pointer
            {
                window.show_window_menu(pointer);
            }
        });
    }
}

fn resize_edge(pointer: PhysicalPosition<f64>, size: PhysicalSize<u32>, edge: f64) -> Option<ResizeDirection> {
    use ResizeDirection::*;
    let [w, h] = [f64::from(size.width), f64::from(size.height)];
    let PhysicalPosition { x, y } = pointer;
    if !(0.0..w).contains(&x) || !(0.0..h).contains(&y) {
        return None;
    }
    match (x < edge, x >= w - edge, y < edge, y >= h - edge) {
        (true, _, true, _) => Some(NorthWest),
        (_, true, true, _) => Some(NorthEast),
        (true, _, _, true) => Some(SouthWest),
        (_, true, _, true) => Some(SouthEast),
        (true, _, _, _) => Some(West),
        (_, true, _, _) => Some(East),
        (_, _, true, _) => Some(North),
        (_, _, _, true) => Some(South),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_window_resize_edges_leave_the_caption_controls_available() {
        let size = PhysicalSize::new(420, 270);
        let at = |x, y| resize_edge(PhysicalPosition::new(x, y), size, 8.0);
        assert_eq!(at(0.0, 0.0), Some(ResizeDirection::NorthWest));
        assert_eq!(at(419.0, 269.0), Some(ResizeDirection::SouthEast));
        assert_eq!(at(419.0, 12.0), Some(ResizeDirection::East));
        assert_eq!(at(393.0, 12.0), None);
        assert_eq!(at(2.0, 135.0), Some(ResizeDirection::West));
        assert_eq!(at(-1.0, 0.0), None);
        assert_eq!(at(420.0, 269.0), None);
    }
}
