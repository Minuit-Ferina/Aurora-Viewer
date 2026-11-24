//! Offline editable windows and GPU previews for exercising the generic floater host.
use crate::{theme::Palette, ui::widgets};
use aurora_i18n::tr;
use aurora_render::preview;
use std::sync::Arc;

const DEMO_MAX: usize = 6;

#[derive(Default)]
pub(crate) struct Documents {
    show_controls: bool,
    documents: Vec<Document>,
}

struct Document {
    open: bool,
    initial_detached: Option<bool>,
    text: String,
    preview: Option<DemoPreview>,
}

struct DemoPreview {
    model: Arc<preview::Model>,
    view: preview::View,
}

impl Documents {
    pub fn from_env() -> Self {
        let mut fixture = Self::default();
        let Ok(value) = std::env::var("AURORA_DEMO_NATIVE_WINDOWS") else {
            return fixture;
        };
        let (count, previews) = value
            .strip_suffix(":preview")
            .map_or((value.as_str(), false), |count| (count, true));
        let Some(auto) = count.parse::<usize>().ok().filter(|count| *count <= DEMO_MAX) else {
            log::warn!("AURORA_DEMO_NATIVE_WINDOWS requires a count from 0 to {DEMO_MAX} and accepts :preview");
            return fixture;
        };
        fixture.show_controls = true;
        let count = auto.max(1);
        let model = previews.then(demo_model);
        fixture.documents.extend((0..count).map(|_| Document {
            open: true,
            initial_detached: Some(auto > 0),
            text: tr!("native-windows-demo-text").to_owned(),
            preview: model.as_ref().map(|model| DemoPreview {
                model: Arc::clone(model),
                view: preview::View::default(),
            }),
        }));
        fixture
    }

    pub fn show(&mut self, ctx: &egui::Context, palette: &Palette) {
        if self.documents.is_empty() {
            return;
        }
        let mut return_all = false;
        let mut open = self.show_controls;
        widgets::Floater::new(
            "native_window_demo_controls",
            tr!("native-windows-demo"),
            egui::pos2(115.0, 115.0),
            egui::vec2(360.0, 100.0),
        )
        .show(ctx, palette, &mut open, |ui| {
            return_all |= widgets::flat_button(ui, palette, tr!("native-windows-return-all")).clicked();
            if self.documents.iter().any(|document| !document.open)
                && widgets::flat_button(ui, palette, tr!("native-windows-demo-reopen")).clicked()
            {
                for document in &mut self.documents {
                    document.open = true;
                }
            }
        });
        self.show_controls = open;
        if return_all {
            super::request_return_all(ctx);
        }
        for (index, document) in self.documents.iter_mut().enumerate() {
            if !document.open {
                continue;
            }
            let delta = (index as f32) * 22.0;
            let key = format!("native_window_demo_document_{index}");
            let mut keep_open = document.open;
            if let Some(detached) = document.initial_detached.take() {
                super::want(ctx, egui::Id::unique(&key), detached);
            }
            widgets::Floater::new(
                &key,
                document_title(index),
                egui::pos2(510.0 + delta, 160.0 + delta),
                egui::vec2(410.0, 255.0),
            )
            .show(ctx, palette, &mut keep_open, |ui| {
                show_document(ui, index, document);
            });
            document.open = keep_open;
        }
    }
}

fn document_title(index: usize) -> String {
    tr!("native-windows-demo-document", number = index + 1)
}

fn editor_id(index: usize) -> egui::Id {
    egui::Id::unique(("native_window_demo_editor", index))
}

fn show_document(ui: &mut egui::Ui, index: usize, document: &mut Document) {
    let height = ui.available_height().max(60.0);
    let edit = |ui: &mut egui::Ui, text: &mut String| {
        let editor = egui::TextEdit::multiline(text).id(editor_id(index)).desired_width(f32::INFINITY);
        ui.add_sized([ui.available_width(), height], editor);
    };
    if let Some(model) = &mut document.preview {
        ui.horizontal_top(|ui| {
            let side = (ui.available_width() * 0.35).min(height).min(140.0);
            preview::paint(
                ui,
                editor_id(index).with("preview"),
                Arc::clone(&model.model),
                &mut model.view,
                egui::Vec2::splat(side),
            );
            edit(ui, &mut document.text);
        });
    } else {
        edit(ui, &mut document.text);
    }
}

fn demo_model() -> Arc<preview::Model> {
    // A synthetic textured surface exercises the same paint callback and
    // per-window user texture registration as the viewer's asset previews.
    let normal = [0.0, 0.0, 1.0];
    let vertices = [
        preview::Vertex::new([-1.0, -1.0, 0.0], normal, [0.0, 1.0]),
        preview::Vertex::new([1.0, -1.0, 0.0], normal, [1.0, 1.0]),
        preview::Vertex::new([1.0, 1.0, 0.0], normal, [1.0, 0.0]),
        preview::Vertex::new([-1.0, 1.0, 0.0], normal, [0.0, 0.0]),
    ];
    let image = Arc::new(preview::Image {
        size: [2, 2],
        rgba: Arc::from([238, 128, 60, 255, 78, 202, 216, 255, 77, 106, 208, 255, 208, 99, 198, 255]),
    });
    Arc::new(preview::Model {
        parts: vec![preview::Part {
            diagnostic: false,
            vertices: Arc::from(vertices),
            indices: Arc::from([0, 1, 2, 0, 2, 3]),
            textures: [Some(image), None, None, None],
            material: preview::Material::default(),
            skin: None,
        }],
        center: glam::Vec3::ZERO,
        radius: std::f32::consts::SQRT_2,
    })
}
