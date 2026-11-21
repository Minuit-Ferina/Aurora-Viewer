//! Inventory drop targets and inline feedback for desktop text import.
use super::{InvAction, Inventory, rules};
use crate::{theme::Palette, ui::widgets};
use aurora_i18n::tr;
use egui::{Context, Id, LayerId, Pos2, Rect, Ui, ViewportId};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;
use winit::{
    data_transfer::DataTransferId,
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
struct HitLayer {
    id: LayerId,
    bounds: Rect,
    from_global: Option<egui::emath::TSTransform>,
    above_modal: bool,
}

#[derive(Clone)]
struct Layout {
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    pixels_per_point: f64,
    layers: Vec<HitLayer>,
    zones: Vec<Zone>,
    pointer: Option<(DataTransferId, Pos2)>,
}

fn layout_id(viewport: ViewportId) -> Id {
    Id::unique(("desktop_text_inventory_targets", viewport))
}

pub(crate) fn forget(ctx: &Context, viewport: ViewportId) {
    ctx.data_mut(|data| data.remove::<Option<Arc<Layout>>>(layout_id(viewport)));
}

/// Start fresh on every egui pass, including a sizing pass that gets discarded.
pub(crate) fn begin(
    ctx: &Context,
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    drag: Option<(DataTransferId, PhysicalPosition<f64>)>,
) {
    let viewport = ctx.viewport_id();
    let pixels_per_point = f64::from(ctx.pixels_per_point());
    let pointer = drag.and_then(|(id, position)| surface_point(size, pixels_per_point, position).map(|pos| (id, pos)));
    ctx.data_mut(|data| {
        data.insert_temp(
            layout_id(viewport),
            Some(Arc::new(Layout {
                window,
                size,
                native_scale,
                pixels_per_point,
                layers: Vec::new(),
                zones: Vec::new(),
                pointer,
            })),
        )
    });
}

pub(super) fn pointer(ctx: &Context) -> Option<(DataTransferId, Pos2)> {
    layout(ctx, ctx.viewport_id())?.pointer
}

pub(super) fn record_row(ui: &Ui, rect: Rect, folder: Uuid) {
    let list = ui.clip_rect();
    record(ui, Rect::from_x_y_ranges(list.x_range(), rect.y_range()), Some(folder), true);
}

pub(super) fn record(ui: &Ui, rect: Rect, folder: Option<Uuid>, row: bool) {
    let Some(rect) = rect
        .is_positive()
        .then(|| rect.intersect(ui.clip_rect()))
        .filter(|rect| rect.is_positive())
    else {
        return;
    };
    // Store geometry and folder ids only. Check permissions for the actual
    // hover/release target, not every visible row on every frame.
    let target = folder.map_or(Target::Choose, Target::Folder);
    let viewport = ui.ctx().viewport_id();
    ui.ctx().data_mut(|data| {
        if let Some(layout) = data
            .get_temp_mut_or_insert_with(layout_id(viewport), || None::<Arc<Layout>>)
            .as_mut()
        {
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
    viewport: ViewportId,
    window: WindowId,
    size: PhysicalSize<u32>,
    native_scale: f64,
    position: Option<PhysicalPosition<f64>>,
    inv: &Inventory,
) -> Target {
    let Some(layout) = layout(ctx, viewport).filter(|layout| layout.window == window && !layout.zones.is_empty()) else {
        return Target::Reject;
    };
    let Some(position) = position.filter(|p| p.x.is_finite() && p.y.is_finite()) else {
        return Target::Choose;
    };
    let Some(pos) = surface_point(size, layout.pixels_per_point, position) else {
        return Target::Reject;
    };
    if layout.size != size || layout.native_scale != native_scale {
        return Target::Choose;
    }
    resolve(&layout, pos).map_or(Target::Reject, |zone| checked(zone.target, inv))
}

/// Remember the final layer order of the viewport we just drew. Egui's
/// memory still points at that viewport, but its current pass has ended.
pub(crate) fn finish(ctx: &Context, viewport: ViewportId) {
    if layout(ctx, viewport).is_none_or(|layout| layout.zones.is_empty()) {
        return;
    }
    let order = ctx.memory(|memory| {
        memory
            .layer_ids()
            .filter(|layer| memory.areas().is_visible(layer))
            .map(|layer| (layer, memory.is_above_modal_layer(layer)))
            .collect::<Vec<_>>()
    });
    let layers = order
        .into_iter()
        .rev()
        .filter_map(|(id, above_modal)| {
            let area = egui::AreaState::load(ctx, id.id).filter(|area| area.interactable)?;
            let to_global = ctx.layer_transform_to_global(id);
            Some(HitLayer {
                id,
                bounds: to_global.map_or(area.rect(), |transform| transform * area.rect()),
                from_global: to_global.map(|transform| transform.inverse()),
                above_modal,
            })
        })
        .collect();
    ctx.data_mut(|data| {
        if let Some(saved) = data
            .get_temp_mut_or_insert_with(layout_id(viewport), || None::<Arc<Layout>>)
            .as_mut()
        {
            Arc::make_mut(saved).layers = layers;
        }
    });
}

pub(crate) fn draw_hover(ctx: &Context, p: &Palette, inv: &Inventory) {
    let Some(layout) = layout(ctx, ctx.viewport_id()) else { return };
    let Some((_, pos)) = layout.pointer else { return };
    let Some(layer) = ctx.layer_id_at(pos) else { return };
    let local = ctx.layer_transform_from_global(layer).map_or(pos, |transform| transform * pos);
    let Some(zone) = zone_at(&layout.zones, layer, local) else { return };
    let (allowed, label) = match checked(zone.target, inv) {
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

fn checked(target: Target, inv: &Inventory) -> Target {
    match target {
        Target::Folder(id) if !rules::text_import_destination(inv, id) => Target::Reject,
        _ => target,
    }
}

fn layout(ctx: &Context, viewport: ViewportId) -> Option<Arc<Layout>> {
    ctx.data(|data| data.get_temp::<Option<Arc<Layout>>>(layout_id(viewport)).flatten())
}

fn surface_point(size: PhysicalSize<u32>, scale: f64, position: PhysicalPosition<f64>) -> Option<Pos2> {
    if !(0.0..f64::from(size.width)).contains(&position.x) || !(0.0..f64::from(size.height)).contains(&position.y) {
        return None;
    }
    Some(Pos2::new((position.x / scale) as f32, (position.y / scale) as f32))
}

fn resolve(layout: &Layout, pos: Pos2) -> Option<Zone> {
    let layer = layout.layers.iter().find(|layer| layer.bounds.contains(pos))?;
    if !layer.above_modal {
        return None;
    }
    let local = layer.from_global.map_or(pos, |transform| transform * pos);
    zone_at(&layout.zones, layer.id, local)
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
    fn desktop_text_targets_follow_the_native_surface_between_viewport_passes() {
        let ctx = Context::default();
        let root = ViewportId::ROOT;
        let child = ViewportId(Id::unique("detached inventory"));
        let root_window = WindowId::from_raw(1);
        let child_window = WindowId::from_raw(2);
        let size = PhysicalSize::new(480, 360);
        let pos = PhysicalPosition::new(25.0, 25.0);
        let inv = Inventory::default();
        let draw = |viewport, window, inventory| {
            let mut input = egui::RawInput {
                viewport_id: viewport,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(480.0, 360.0))),
                ..Default::default()
            };
            input.viewports.entry(child).or_default().parent = Some(root);
            let _ = ctx.run_ui(input, |ui| {
                begin(ui.ctx(), window, size, 1.0, None);
                egui::Area::new(Id::unique(("desktop text inventory", viewport)))
                    .fixed_pos(Pos2::ZERO)
                    .show(ui.ctx(), |ui| {
                        let area = ui.allocate_space(egui::vec2(100.0, 90.0)).1;
                        if inventory {
                            record(ui, area, None, false);
                        }
                    });
            });
            finish(&ctx, viewport);
        };
        for _ in 0..3 {
            draw(root, root_window, false);
            draw(child, child_window, true);
        }
        draw(root, root_window, false);
        assert_eq!(hit(&ctx, child, child_window, size, 1.0, Some(pos), &inv), Target::Choose);
        assert_eq!(hit(&ctx, root, root_window, size, 1.0, Some(pos), &inv), Target::Reject);
        assert_eq!(hit(&ctx, child, root_window, size, 1.0, Some(pos), &inv), Target::Reject);
        forget(&ctx, child);
        assert_eq!(hit(&ctx, child, child_window, size, 1.0, Some(pos), &inv), Target::Reject);
    }

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
