//! Cover art: the texture cache and the per-frame decode budget.

use super::*;

#[derive(Default)]
pub(super) struct CoverCache {
    pub(super) entries: HashMap<TrackId, Option<egui::TextureHandle>>,
}

impl CoverCache {
    pub(super) fn get(&self, id: TrackId) -> Option<&egui::TextureHandle> {
        self.entries.get(&id).and_then(Option::as_ref)
    }

    pub(super) fn contains(&self, id: TrackId) -> bool {
        self.entries.contains_key(&id)
    }

    pub(super) fn load(&mut self, ctx: &egui::Context, id: TrackId, path: &Path) {
        if self.entries.contains_key(&id) {
            return;
        }
        let texture = metadata::read_cover(path)
            .ok()
            .flatten()
            .and_then(|cover| decode_texture(ctx, format!("cover-{id}"), &cover.data));
        self.entries.insert(id, texture);
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

pub(super) fn decode_texture(ctx: &egui::Context, name: String, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &image.into_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// Draws a square cover (or a music-note placeholder) in a single allocation,
/// so the widget id stays stable whether or not art has loaded yet.
pub(super) fn cover_thumb(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    match texture {
        Some(texture) => {
            egui::Image::new((texture.id(), egui::vec2(size, size))).paint_at(ui, rect);
        }
        None => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "♪",
                FontId::proportional(size * 0.35),
                ui.visuals().weak_text_color(),
            );
        }
    }
}

impl NovaApp {
    /// Representative track used to render an album's cover.
    pub(super) fn album_cover_source(&self, key: &AlbumKey) -> Option<TrackId> {
        let album = self.library.albums().iter().find(|a| &a.key == key)?;
        let mut fallback = None;
        for id in &album.track_ids {
            let Some(track) = self.library.track(*id) else {
                continue;
            };
            if fallback.is_none() {
                fallback = Some(*id);
            }
            if track.has_cover {
                return Some(*id);
            }
        }
        fallback
    }

    pub(super) fn ensure_covers(&mut self, ctx: &egui::Context) {
        let mut wanted: Vec<(TrackId, PathBuf)> = Vec::new();

        if self.view == View::Albums && self.opened_album.is_none() {
            for album in self.library.albums() {
                if let Some(id) = self.album_cover_source(&album.key) {
                    if !self.covers.contains(id) {
                        if let Some(track) = self.library.track(id) {
                            wanted.push((id, track.path.clone()));
                        }
                    }
                }
            }
        }

        if let Some(track) = self.engine.current_track(&self.library) {
            if !self.covers.contains(track.id) {
                wanted.push((track.id, track.path.clone()));
            }
        }

        if self.info_open {
            if let Some(track) = self.info_track() {
                if !self.covers.contains(track.id) {
                    wanted.push((track.id, track.path.clone()));
                }
            }
        }

        let mut budget = COVER_BUDGET_PER_FRAME;
        for (id, path) in wanted {
            if budget == 0 {
                ctx.request_repaint();
                break;
            }
            self.covers.load(ctx, id, &path);
            budget -= 1;
        }
    }
}
