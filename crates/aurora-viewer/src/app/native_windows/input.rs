//! Egui's selected memory viewport stays at the last completed outer pass.
//! Native events can arrive between passes, so root shortcuts and world
//! picking use the last completed root hit regions and keyboard state.

#[derive(Clone, Copy, Default)]
pub(in crate::app) struct Keyboard {
    pub wants: bool,
    pub popup: bool,
    pub text_edit: bool,
}

#[derive(Default)]
pub(super) struct RootInput {
    areas: Vec<egui::Rect>,
    grips: Vec<egui::Rect>,
    radius_squared: f32,
    keyboard: Keyboard,
    captured_pointer: bool,
    last_was_child: bool,
    surrender: bool,
}

impl RootInput {
    pub(super) fn begin_root_pass(&mut self, ctx: &egui::Context) {
        self.last_was_child = false;
        if std::mem::take(&mut self.surrender) {
            self.surrender_focus(ctx);
        }
    }

    pub(super) fn remember(&mut self, ctx: &egui::Context) {
        self.keyboard = Keyboard {
            wants: ctx.egui_wants_keyboard_input(),
            popup: egui::Popup::is_any_open(ctx),
            text_edit: ctx.text_edit_focused(),
        };
        self.captured_pointer = ctx.egui_is_using_pointer();
        self.radius_squared = ctx.global_style().interaction.interact_radius.powi(2);
        crate::ui::widgets::pointer_regions(ctx, &mut self.areas, &mut self.grips);
    }

    pub(super) fn child_pass_finished(&mut self) {
        self.last_was_child = true;
    }

    pub(super) fn pointer(&self, pos: egui::Pos2) -> Option<bool> {
        self.last_was_child.then(|| {
            self.captured_pointer
                || self.areas.iter().any(|area| area.contains(pos))
                || self.grips.iter().any(|rect| rect.distance_sq_to_pos(pos) <= self.radius_squared)
        })
    }

    pub(super) fn keyboard(&self) -> Option<Keyboard> {
        self.last_was_child.then_some(self.keyboard)
    }

    pub(super) fn surrender_focus(&mut self, ctx: &egui::Context) {
        if self.last_was_child {
            self.surrender = true;
        } else {
            ctx.memory_mut(|memory| {
                if let Some(id) = memory.focused() {
                    memory.surrender_focus(id);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RootInput;
    use egui::{Area, Context, Id, Pos2, RawInput, Rect, ViewportId, ViewportInfo, pos2, vec2};

    fn draw_area(ctx: &Context, viewport: ViewportId, position: Pos2) {
        let mut input = RawInput {
            viewport_id: viewport,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(480.0, 360.0))),
            ..Default::default()
        };
        if viewport != ViewportId::ROOT {
            input.viewports.insert(
                viewport,
                ViewportInfo {
                    parent: Some(ViewportId::ROOT),
                    ..Default::default()
                },
            );
        }
        let mut frame = ctx.run_ui(input, |ui| {
            Area::new(Id::unique(("window input", viewport)))
                .fixed_pos(position)
                .show(ui.ctx(), |ui| ui.set_min_size(vec2(100.0, 100.0)));
        });
        frame.textures_delta.clear();
    }

    #[test]
    fn a_child_pass_preserves_the_viewer_panel_hit_regions() {
        let ctx = Context::default();
        let mut input = RootInput::default();
        let root_pos = pos2(20.0, 20.0);
        let child_pos = pos2(240.0, 150.0);
        let child = ViewportId::from_hash_of("desktop input");

        for _ in 0..2 {
            input.begin_root_pass(&ctx);
            draw_area(&ctx, ViewportId::ROOT, root_pos);
            input.remember(&ctx);
            assert_eq!(input.pointer(root_pos + vec2(10.0, 10.0)), None);
            draw_area(&ctx, child, child_pos);
            input.child_pass_finished();
        }

        assert_eq!(input.pointer(root_pos + vec2(10.0, 10.0)), Some(true));
        assert_eq!(input.pointer(child_pos + vec2(10.0, 10.0)), Some(false));
    }
}
