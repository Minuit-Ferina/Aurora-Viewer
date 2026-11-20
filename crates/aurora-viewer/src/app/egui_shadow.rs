//! A CPU copy of every texture egui manages (the font atlas, the icons and
//! the images of the windows), kept from the deltas it hands to the
//! renderer. egui sends a texture once and then only its changes, and keeps
//! no pixels itself (`TextureManager` holds their size and name): a renderer
//! made after a GPU device loss (gpu_recovery.rs) gets them all again from
//! here, whole, before its first frame.

use egui::epaint::{ClippedShape, ImageData, ImageDelta, Shape, TextureId, textures::TexturesDelta};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct EguiShadow {
    images: HashMap<TextureId, Image>,
    revision: u64,
}

struct Image {
    full: ImageDelta,
    revision: u64,
}

/// Textures uploaded to one desktop presentation's egui renderer.
#[derive(Default)]
pub(super) struct NativeTextures(HashMap<TextureId, u64>);

impl EguiShadow {
    /// A frame's delta, applied to the copy.
    pub fn note(&mut self, delta: &TexturesDelta) {
        self.note_updates(delta);
        self.note_frees(delta);
    }

    pub(super) fn note_updates(&mut self, delta: &TexturesDelta) {
        if delta.set.is_empty() {
            return;
        }
        self.revision += 1;
        for (id, changes) in &delta.set {
            for change in changes {
                match change.pos {
                    None => {
                        self.images.insert(
                            *id,
                            Image {
                                full: change.clone(),
                                revision: self.revision,
                            },
                        );
                    }
                    Some(pos) => {
                        if let Some(image) = self.images.get_mut(id) {
                            patch(&mut image.full, &change.image, pos);
                            image.full.options = change.options;
                            image.revision = self.revision;
                        }
                    }
                }
            }
        }
    }

    pub(super) fn note_frees(&mut self, delta: &TexturesDelta) {
        for id in &delta.free {
            self.images.remove(id);
        }
    }

    /// Only upload changed textures used by this presentation. A fresh
    /// surface can start from complete images even if it missed their patches.
    pub(super) fn for_native(&self, state: &mut NativeTextures, shapes: &[ClippedShape]) -> TexturesDelta {
        let mut delta = TexturesDelta::default();
        state.0.retain(|id, _| {
            if self.images.contains_key(id) {
                true
            } else {
                delta.free.insert(*id);
                false
            }
        });
        let mut texture = |id| {
            if let Some(image) = self.images.get(&id)
                && state.0.insert(id, image.revision) != Some(image.revision)
            {
                delta.set.insert(id, [image.full.clone()].into_iter().collect());
            }
        };
        for shape in shapes {
            visit_textures(&shape.shape, &mut texture);
        }
        delta
    }

    /// Every texture whole, as the delta of a renderer that has none (the
    /// frame's own delta is in the copy already), and what it frees.
    pub fn replay(&self, mut frame: TexturesDelta) -> TexturesDelta {
        let free = std::mem::take(&mut frame.free);
        // its changes are in the copy: dropped on purpose (TexturesDelta
        // asserts in debug builds that none is lost by mistake)
        frame.clear();
        TexturesDelta {
            set: self
                .images
                .iter()
                .map(|(id, image)| (*id, [image.full.clone()].into_iter().collect()))
                .collect(),
            free,
        }
    }

    /// Bytes held, for the log.
    pub fn bytes(&self) -> usize {
        self.images
            .values()
            .map(|d| d.full.image.resolution()[0] * d.full.image.resolution()[1] * 4)
            .sum()
    }
}

fn visit_textures(shape: &Shape, visit: &mut impl FnMut(TextureId)) {
    match shape {
        Shape::Vec(shapes) => {
            for shape in shapes {
                visit_textures(shape, visit);
            }
        }
        _ => visit(shape.texture_id()),
    }
}

/// `part` written into `whole` at `pos`, clipped to `whole`.
fn patch(whole: &mut ImageDelta, part: &ImageData, pos: [usize; 2]) {
    let (ImageData::Color(dst), ImageData::Color(src)) = (&mut whole.image, part);
    let dst = std::sync::Arc::make_mut(dst);
    let [w, h] = dst.resolution();
    let [pw, ph] = src.resolution();
    for row in 0..ph.min(h.saturating_sub(pos[1])) {
        let width = pw.min(w.saturating_sub(pos[0]));
        let at = (pos[1] + row) * w + pos[0];
        dst.pixels_mut()[at..at + width].copy_from_slice(&src.pixels()[row * pw..row * pw + width]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, ColorImage, TextureOptions};

    fn image(w: usize, h: usize, c: Color32) -> ImageData {
        ImageData::Color(std::sync::Arc::new(ColorImage::filled([w, h], c)))
    }

    fn delta(set: Vec<(TextureId, ImageDelta)>, free: Vec<TextureId>) -> TexturesDelta {
        TexturesDelta {
            set: set.into_iter().map(|(id, d)| (id, [d].into_iter().collect())).collect(),
            free: free.into_iter().collect(),
        }
    }

    fn pixels(d: &ImageDelta) -> Vec<Color32> {
        let ImageData::Color(c) = &d.image;
        c.pixels().to_vec()
    }

    #[test]
    fn desktop_textures_follow_shared_images_without_replaying_unseen_textures() {
        let icon = TextureId::Managed(4);
        let hidden = TextureId::Managed(7);
        let mut shadow = EguiShadow::default();
        let mut first = delta(
            vec![
                (icon, ImageDelta::full(image(2, 1, Color32::RED), TextureOptions::LINEAR)),
                (hidden, ImageDelta::full(image(1, 1, Color32::BLACK), TextureOptions::LINEAR)),
            ],
            vec![],
        );
        shadow.note(&first);
        first.clear();

        let mesh = egui::Mesh::with_texture(icon);
        let shapes = [ClippedShape::new(egui::Rect::EVERYTHING, Shape::Vec(vec![Shape::mesh(mesh)]))];
        let mut child = NativeTextures::default();
        let mut initial = shadow.for_native(&mut child, &shapes);
        assert_eq!(initial.set.len(), 1);
        assert!(initial.set.contains_key(&icon));
        initial.clear();
        assert!(shadow.for_native(&mut child, &shapes).is_empty());

        let mut patch = delta(
            vec![(
                icon,
                ImageDelta::partial([1, 0], image(1, 1, Color32::BLUE), TextureOptions::LINEAR),
            )],
            vec![],
        );
        shadow.note(&patch);
        patch.clear();
        let mut refreshed = shadow.for_native(&mut child, &shapes);
        assert_eq!(pixels(&refreshed.set[&icon][0]), [Color32::RED, Color32::BLUE]);
        refreshed.clear();

        let mut freed = delta(vec![], vec![icon]);
        shadow.note(&freed);
        freed.clear();
        let mut retire = shadow.for_native(&mut child, &[]);
        assert!(retire.free.contains(&icon));
        retire.clear();
    }

    #[test]
    fn a_new_renderer_gets_every_texture_whole_with_its_changes() {
        let (font, icon, gone) = (TextureId::Managed(0), TextureId::Managed(7), TextureId::Managed(9));
        let mut shadow = EguiShadow::default();
        let mut first = delta(
            vec![
                (font, ImageDelta::full(image(4, 2, Color32::BLACK), TextureOptions::LINEAR)),
                (icon, ImageDelta::full(image(1, 1, Color32::RED), TextureOptions::NEAREST)),
                (gone, ImageDelta::full(image(1, 1, Color32::BLUE), TextureOptions::LINEAR)),
            ],
            vec![],
        );
        shadow.note(&first);
        first.clear();
        // the atlas grows a glyph in its second row, a patch past the edge
        // is clipped, and a texture is freed
        let glyph = ImageDelta::partial([2, 1], image(3, 1, Color32::WHITE), TextureOptions::LINEAR);
        let mut second = delta(vec![(font, glyph)], vec![gone]);
        shadow.note(&second);
        second.clear();
        let mut replay = shadow.replay(delta(vec![], vec![TextureId::Managed(5)]));
        assert_eq!(replay.set.len(), 2);
        let font_now = &replay.set[&font][0];
        assert_eq!(font_now.pos, None, "whole, for a renderer that has none");
        let (b, w) = (Color32::BLACK, Color32::WHITE);
        assert_eq!(pixels(font_now), vec![b, b, b, b, b, b, w, w]);
        assert_eq!(pixels(&replay.set[&icon][0]), vec![Color32::RED]);
        assert!(!replay.set.contains_key(&gone));
        assert!(replay.free.contains(&TextureId::Managed(5)));
        assert_eq!(shadow.bytes(), (4 * 2 + 1) * 4);
        replay.clear();
    }
}
