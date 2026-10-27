//! Drag and drop, command-line targets and "play these files".

use super::*;

/// Overlay shown while files are being dragged over the window.
pub(super) fn drop_overlay(ctx: &egui::Context) {
    let hovering = ctx.input(|i| i.raw.hovered_files.len());
    if hovering == 0 {
        return;
    }
    egui::Area::new(egui::Id::new("drop-overlay"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(24))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("Drop to play · {hovering} item(s)"))
                            .size(20.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("Audio files start playing · folders are scanned").weak(),
                    );
                });
        });
}

/// Splits a drop into folders, supported audio files, and a count of ignored
/// entries. Directories win nothing on their own; the caller decides what to do.
pub(super) fn classify_drop(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>, usize) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for path in paths {
        if path.is_dir() {
            dirs.push(path);
        } else if metadata::is_supported(&path) {
            files.push(path);
        } else {
            skipped += 1;
        }
    }
    (dirs, files, skipped)
}

/// The audio files behind a drop or a folder pick: files matched by
/// extension, folders walked recursively. Returns them plus the count of
/// unsupported files that were skipped.
pub(super) fn expand_to_audio(paths: Vec<PathBuf>) -> (Vec<PathBuf>, usize) {
    let (dirs, files, skipped) = classify_drop(paths);
    let mut files = files;
    let mut skipped = skipped;
    for dir in dirs {
        for entry in walkdir::WalkDir::new(&dir)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            if metadata::is_supported(entry.path()) {
                files.push(entry.path().to_path_buf());
            } else {
                skipped += 1;
            }
        }
    }
    (files, skipped)
}

/// Accepts either a plain filesystem path or a `file://` URL and turns it into
/// a path. For URLs the authority component (`file://host/path`) is dropped and
/// `%XX` escapes are decoded; plain paths are used verbatim.
pub(super) fn parse_target(argument: &str) -> PathBuf {
    let Some(rest) = argument.strip_prefix("file://") else {
        return PathBuf::from(argument);
    };
    let rest = if rest.starts_with('/') {
        rest
    } else if let Some(slash) = rest.find('/') {
        &rest[slash..]
    } else {
        rest
    };
    PathBuf::from(percent_decode(rest))
}

/// Decodes `%XX` escapes; anything malformed is passed through unchanged.
pub(super) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl NovaApp {
    /// Reads files dropped onto the window and acts on them.
    pub(super) fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.accept_drop(dropped);
        }
    }

    /// A dropped folder is scanned; dropped audio files start playing. A mix
    /// prefers the files (the common case is dragging an album's tracks).
    /// With a playlist open the drop adds to it instead of starting playback.
    pub(super) fn accept_drop(&mut self, paths: Vec<PathBuf>) {
        if self.loaded_playlist.is_some() {
            self.add_to_playlist(paths);
            return;
        }

        let (dirs, files, skipped) = classify_drop(paths);

        if !files.is_empty() {
            if skipped > 0 {
                self.toast(format!("Skipped {skipped} unsupported file(s)"));
            }
            self.play_files(files);
        } else if let Some(dir) = dirs.into_iter().next() {
            self.folder_input = dir.display().to_string();
            self.start_scan(dir);
        } else if skipped > 0 {
            self.toast(format!("Skipped {skipped} unsupported file(s)"));
        } else {
            self.toast("Nothing playable in the drop");
        }
    }

    /// Imports externally dropped audio files and appends them to the queue
    /// without interrupting the track that is currently playing.
    pub(super) fn enqueue_dropped_files(&mut self, paths: Vec<PathBuf>) {
        let (dirs, files, skipped) = classify_drop(paths);
        if !dirs.is_empty() {
            self.toast("Drop audio files here (folders are not added to the playlist)");
        }
        if files.is_empty() {
            if skipped > 0 {
                self.toast(format!("Skipped {skipped} unsupported file(s)"));
            }
            return;
        }

        self.import_missing(&files);

        let ids: Vec<TrackId> = files
            .iter()
            .filter_map(|path| self.find_track_by_path(path))
            .collect();
        if ids.is_empty() {
            self.toast("No playable audio found in the dropped files");
            return;
        }

        for id in &ids {
            self.engine.enqueue(*id);
        }
        let count = ids.len();
        if skipped > 0 {
            self.toast(format!(
                "Added {count} track(s); skipped {skipped} unsupported file(s)"
            ));
        } else {
            self.toast(format!("Added {count} track(s) to the playlist"));
        }
    }

    /// Queues and plays a set of files, reading tags for any the library has
    /// not seen yet (so a drop works without rescanning the folder).
    pub(super) fn play_files(&mut self, paths: Vec<PathBuf>) {
        let missing: Vec<PathBuf> = paths
            .iter()
            .filter(|path| self.find_track_by_path(path).is_none())
            .cloned()
            .collect();

        let mut added = 0usize;
        if !missing.is_empty() {
            let mut tracks = self.library.tracks().to_vec();
            for path in &missing {
                match metadata::read_track(path) {
                    Ok(track) => {
                        if !tracks.iter().any(|known| known.path == track.path) {
                            tracks.push(track);
                            added += 1;
                        }
                    }
                    Err(err) => {
                        self.toast(format!("Could not read {}: {err}", path.display()));
                    }
                }
            }
            if added > 0 {
                let root = self.library.root().map(Path::to_path_buf);
                self.library = Library::from_tracks(root, tracks);
            }
        }

        // Resolve in drop order, now that everything is indexed.
        let ids: Vec<TrackId> = paths
            .iter()
            .filter_map(|path| self.find_track_by_path(path))
            .collect();
        if ids.is_empty() {
            self.toast("No playable audio found in the drop");
            return;
        }

        let count = ids.len();
        match self.engine.set_queue(&self.library, ids, 0) {
            Ok(()) => {
                self.view = View::Songs;
                self.toast(format!("Playing {count} dropped track(s)"));
            }
            Err(err) => self.toast(err.to_string()),
        }
    }

    /// Replaces the queue with the file's album (falling back to just the file)
    /// and starts playback. Returns `false` if the file is not in the library.
    pub(super) fn play_path(&mut self, path: &Path) -> bool {
        let Some(id) = self.find_track_by_path(path) else {
            return false;
        };

        let album_key = self.library.track(id).map(|track| track.album_key());
        let mut ids: Vec<TrackId> = album_key
            .as_ref()
            .map(|key| {
                self.library
                    .album_tracks(key)
                    .iter()
                    .map(|track| track.id)
                    .collect()
            })
            .unwrap_or_default();
        if !ids.contains(&id) {
            ids = vec![id];
        }
        let start = ids.iter().position(|item| *item == id).unwrap_or(0);

        match self.engine.set_queue(&self.library, ids, start) {
            Ok(()) => true,
            Err(err) => {
                self.toast(err.to_string());
                false
            }
        }
    }
}
