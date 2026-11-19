//! Inventory drop targets and inline feedback for desktop text import.
use super::{InvAction, Inventory, rules};
use crate::{theme::Palette, ui::widgets};
use aurora_i18n::tr;
use egui::{Context, Id, LayerId, Pos2, Rect, Ui};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    window::WindowId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Folder(Uuid),
    Choose,
    Reject,
}

#[derive(Clone, Copy)]
struct Zone {
    rect: Rect,
    layer: LayerId,
    row: bool,
    target: Target,
}

#[derive(Clone)]
struct Layout {
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    zones: Vec<Zone>,
}

fn layout_id() -> Id {
    Id::unique("desktop_text_inventory_targets")
}

/// Start fresh on every egui pass, including a sizing pass that gets discarded.
pub(crate) fn begin(ctx: &Context, window: WindowId, size: PhysicalSize<u32>, native_scale: f64) {
    ctx.data_mut(|data| {
        data.insert_temp(
            layout_id(),
            Some(Arc::new(Layout {
                window,
                size,
                native_scale,
                zones: Vec::new(),
            })),
        )
    });
}

pub(super) fn record_row(ui: &Ui, rect: Rect, inv: &Inventory, folder: Option<Uuid>) {
    let list = ui.clip_rect();
    record(ui, Rect::from_x_y_ranges(list.x_range(), rect.y_range()), inv, folder, true);
}

pub(super) fn record(ui: &Ui, rect: Rect, inv: &Inventory, folder: Option<Uuid>, row: bool) {
    let Some(rect) = rect
        .is_positive()
        .then(|| rect.intersect(ui.clip_rect()))
        .filter(|rect| rect.is_positive())
    else {
        return;
    };
    let target = match folder {
        Some(id) if rules::text_import_destination(inv, id) => Target::Folder(id),
        Some(_) => Target::Reject,
        None if row => Target::Reject,
        None => Target::Choose,
    };
    ui.ctx().data_mut(|data| {
        if let Some(layout) = data.get_temp_mut_or_insert_with(layout_id(), || None::<Arc<Layout>>).as_mut() {
            Arc::make_mut(layout).zones.push(Zone {
                rect,
                layer: ui.layer_id(),
                row,
                target,
            });
        }
    });
}

pub(crate) fn hit(
    ctx: &Context,
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    position: Option<PhysicalPosition<f64>>,
) -> Target {
    resolve(ctx, window, size, native_scale, position).map_or_else(|target| target, |(zone, _)| zone.target)
}

pub(crate) fn draw_hover(
    ctx: &Context,
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    position: PhysicalPosition<f64>,
    p: &Palette,
    inv: &Inventory,
) {
    let Ok((zone, pos)) = resolve(ctx, window, size, native_scale, Some(position)) else {
        return;
    };
    let (allowed, label) = match zone.target {
        Target::Folder(folder) => (true, tr!("desktop-text-import-into", folder = super::folder_name(inv, folder))),
        Target::Reject => (false, tr!("desktop-text-import-error-destination").to_owned()),
        Target::Choose => return,
    };
    super::tree::paint_drop_outline(
        &ctx.layer_painter(zone.layer),
        p,
        ctx.global_style().visuals.window_corner_radius,
        zone.rect,
        allowed,
    );
    super::drag::paint_badge(ctx, p, pos, label);
}

fn resolve(
    ctx: &Context,
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    position: Option<PhysicalPosition<f64>>,
) -> Result<(Zone, Pos2), Target> {
    let layout = ctx
        .data(|data| data.get_temp::<Option<Arc<Layout>>>(layout_id()).flatten())
        .ok_or(Target::Reject)?;
    if layout.window != window || layout.zones.is_empty() {
        return Err(Target::Reject);
    }
    let position = position.filter(|p| p.x.is_finite() && p.y.is_finite()).ok_or(Target::Choose)?;
    if position.x < 0.0 || position.y < 0.0 || position.x >= f64::from(size.width) || position.y >= f64::from(size.height) {
        return Err(Target::Reject);
    }
    if layout.size != size || layout.native_scale != native_scale {
        return Err(Target::Choose);
    }
    let scale = f64::from(ctx.pixels_per_point());
    let pos = Pos2::new((position.x / scale) as f32, (position.y / scale) as f32);
    let layer = ctx.layer_id_at(pos).ok_or(Target::Reject)?;
    let local = ctx.layer_transform_from_global(layer).map_or(pos, |transform| transform * pos);
    zone_at(&layout.zones, layer, local).map(|zone| (zone, pos)).ok_or(Target::Reject)
}

fn zone_at(zones: &[Zone], layer: LayerId, local: Pos2) -> Option<Zone> {
    zones
        .iter()
        .filter(|zone| zone.layer == layer && zone.rect.contains(local))
        .max_by_key(|zone| zone.row)
        .copied()
}

#[derive(Clone)]
pub(crate) struct Status {
    pub label: String,
    pub detail: String,
    pub error: bool,
}

impl Status {
    pub(super) fn color(&self, p: &Palette) -> egui::Color32 {
        if self.error { p.warn } else { p.muted }
    }
}

pub(crate) struct PendingRow {
    pub name: String,
    pub status: Status,
}

pub(crate) struct PendingBatch {
    pub id: Uuid,
    pub folder: Uuid,
    pub rows: Vec<PendingRow>,
    pub cancellable: bool,
    pub finished: bool,
}

#[derive(Default)]
pub(crate) struct Feedback {
    pub items: HashMap<Uuid, Status>,
    pub pending: Vec<PendingBatch>,
    pub choice: Option<Uuid>,
}

fn feedback_id() -> Id {
    Id::unique("desktop_text_inventory_feedback")
}

pub(crate) fn publish(ctx: &Context, feedback: Feedback) {
    ctx.data_mut(|data| data.insert_temp(feedback_id(), Arc::new(feedback)));
}

fn feedback(ctx: &Context) -> Option<Arc<Feedback>> {
    ctx.data(|data| data.get_temp(feedback_id()))
}

pub(super) fn choice(ctx: &Context) -> Option<Uuid> {
    feedback(ctx)?.choice
}

pub(super) fn item(ctx: &Context, id: Uuid) -> Option<Status> {
    feedback(ctx)?.items.get(&id).cloned()
}

pub(super) fn append(ui: &Ui, p: &Palette, job: &mut egui::text::LayoutJob, status: &Status, selected: bool) {
    let color = if selected { p.on_accent } else { status.color(p) };
    job.append(
        &status.label,
        8.0,
        egui::TextFormat::simple(egui::TextStyle::Small.resolve(ui.style()), color),
    );
}

/// Use the inventory itself to resolve drops without an exact folder.
pub(super) fn inline(ui: &mut Ui, p: &Palette, folder: Option<Uuid>, actions: &mut Vec<InvAction>) {
    let Some(feedback) = feedback(ui.ctx()) else { return };
    if let Some(id) = feedback.choice {
        ui.horizontal_wrapped(|ui| {
            ui.small(tr!("desktop-text-import-choose"));
            if widgets::action_button(
                ui,
                p,
                "x",
                tr!("desktop-text-import-cancel-choice"),
                widgets::ActionKind::Flat,
                egui::vec2(0.0, 24.0),
            )
            .clicked()
            {
                actions.push(InvAction::DiscardDesktopText(id));
            }
        });
    }
    let mut pending = feedback
        .pending
        .iter()
        .filter(|batch| folder.is_none_or(|folder| folder == batch.folder))
        .peekable();
    if pending.peek().is_none() {
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("desktop_text_pending_rows")
        .max_height(4.0 * ui.spacing().interact_size.y)
        .show(ui, |ui| {
            for batch in pending {
                ui.push_id(batch.id, |ui| {
                    for row in &batch.rows {
                        ui.horizontal_wrapped(|ui| {
                            ui.small(&row.name);
                            ui.small(egui::RichText::new(&row.status.label).color(row.status.color(p)))
                                .on_hover_text(&row.status.detail);
                        });
                    }
                    let (label, action) = if batch.cancellable {
                        (tr!("desktop-text-import-cancel"), InvAction::CancelDesktopText(batch.id))
                    } else if batch.finished {
                        (tr!("desktop-text-import-dismiss"), InvAction::DismissDesktopText(batch.id))
                    } else {
                        return;
                    };
                    if widgets::action_button(ui, p, "x", label, widgets::ActionKind::Flat, egui::vec2(0.0, 24.0)).clicked() {
                        actions.push(action);
                    }
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_top_layer_at_the_drop_can_receive_it_and_rows_take_precedence() {
        let main = LayerId::new(egui::Order::Middle, Id::unique("inventory"));
        let overlaid = LayerId::new(egui::Order::Foreground, Id::unique("menu"));
        let folder = Uuid::from_u128(1);
        let parent = Uuid::from_u128(2);
        let area = Rect::from_min_max(Pos2::ZERO, Pos2::new(40.0, 40.0));
        let row = Rect::from_min_max(Pos2::ZERO, Pos2::new(40.0, 12.0));
        let zones = [
            Zone {
                rect: row,
                layer: main,
                row: true,
                target: Target::Folder(folder),
            },
            Zone {
                rect: area,
                layer: main,
                row: false,
                target: Target::Folder(parent),
            },
        ];
        let target_at = |zones: &[Zone], layer, pos| zone_at(zones, layer, pos).map_or(Target::Reject, |zone| zone.target);
        assert_eq!(target_at(&zones, main, Pos2::new(10.0, 5.0)), Target::Folder(folder));
        assert_eq!(target_at(&zones, main, Pos2::new(10.0, 20.0)), Target::Folder(parent));
        assert_eq!(target_at(&zones, overlaid, Pos2::new(10.0, 5.0)), Target::Reject);
        assert_eq!(target_at(&zones, main, Pos2::new(99.0, 99.0)), Target::Reject);
    }
}
