//! Scanning, indexing and the persisted library cache and hidden list.

use super::*;

pub(super) fn load_cached_library(expected_root: Option<&Path>) -> Option<Library> {
    let path = cache_file()?;
    let library = Library::load_cache(&path).ok()?;

    // The cache is only valid for the folder it was built from.
    if let (Some(expected), Some(cached)) = (expected_root, library.root()) {
        let expected = std::fs::canonicalize(expected).unwrap_or_else(|_| expected.to_path_buf());
        let cached = std::fs::canonicalize(cached).unwrap_or_else(|_| cached.to_path_buf());
        if expected != cached {
            return None;
        }
    }

    if library.is_empty() {
        None
    } else {
        Some(library)
    }
}

/// Ids of tracks the user hid. A missing or corrupt file just means "nothing
/// hidden", never a failed startup.
pub(super) fn load_hidden() -> HashSet<TrackId> {
    hidden_file()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn save_hidden(hidden: &HashSet<TrackId>) {
    let Some(path) = hidden_file() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(hidden) {
        let _ = std::fs::write(path, json);
    }
}

impl NovaApp {
    /// Locates a track in the library by file path (exact first, then
    /// canonicalised so symlinks and redundant components still match).
    pub(super) fn find_track_by_path(&self, path: &Path) -> Option<TrackId> {
        if let Some(track) = self.library.tracks().iter().find(|t| t.path == path) {
            return Some(track.id);
        }
        let target = std::fs::canonicalize(path).ok()?;
        self.library.tracks().iter().find_map(|track| {
            let candidate = std::fs::canonicalize(&track.path).ok()?;
            (candidate == target).then_some(track.id)
        })
    }

    /// Reads tags for files the library has not seen yet and adds them, so a
    /// drop or an add works without rescanning the folder.
    pub(super) fn import_missing(&mut self, files: &[PathBuf]) {
        let missing: Vec<PathBuf> = files
            .iter()
            .filter(|path| self.find_track_by_path(path).is_none())
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        let mut tracks = self.library.tracks().to_vec();
        let mut added = 0usize;
        for path in &missing {
            match metadata::read_track(path) {
                Ok(track) => {
                    if !tracks.iter().any(|known| known.path == track.path) {
                        tracks.push(track);
                        added += 1;
                    }
                }
                Err(err) => self.toast(format!("Could not read {}: {err}", path.display())),
            }
        }
        if added > 0 {
            let root = self.library.root().map(Path::to_path_buf);
            self.library = Library::from_tracks(root, tracks);
        }
    }

    /// Acts on whatever path the folder selector was given.
    ///
    /// A folder is indexed as before. A file is not a request to walk its parent —
    /// pointing the selector at one track should add that track and leave the rest of
    /// the folder alone, which is the difference between adding a video and
    /// indexing whatever else happens to sit beside it.
    pub(super) fn select_path(&mut self, path: PathBuf) {
        // With a playlist open, the Folder row adds to it instead of
        // scanning — that is the whole point of keeping the list on screen.
        if self.loaded_playlist.is_some() {
            self.add_to_playlist(vec![path]);
            return;
        }
        if path.is_dir() {
            self.start_scan(path);
        } else if path.is_file() {
            self.add_file(path);
        } else {
            self.toast(format!(
                "{} is not a folder or a media file",
                path.display()
            ));
        }
    }

    /// Adds a single media file to the library without indexing its folder.
    pub(super) fn add_file(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if self.find_track_by_path(&path).is_some() {
            self.toast(format!("{name} is already in the library"));
            return;
        }

        let track = match metadata::read_track(&path) {
            Ok(track) => track,
            Err(err) => {
                self.toast(format!("Could not read {name}: {err}"));
                return;
            }
        };
        let title = track.title.clone();
        let root = self.library.root().map(Path::to_path_buf);
        let mut tracks = self.library.tracks().to_vec();
        tracks.push(track);
        self.library = Library::from_tracks(root, tracks);
        self.covers.clear();
        self.toast(format!("Added {title} to the library"));
    }

    pub(super) fn start_scan(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            self.toast(format!("{} is not a directory", dir.display()));
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        self.scan_status = Some("Scanning…".to_string());
        self.music_dir = Some(dir.clone());

        std::thread::spawn(move || {
            let sender = tx.clone();
            let result = Library::scan(&dir, &mut |progress| {
                if progress.indexed % 32 == 0 {
                    let _ = sender.send(ScanMsg::Progress(progress.indexed));
                }
            });
            match result {
                Ok((library, report)) => {
                    let _ = tx.send(ScanMsg::Done(Box::new((library, report))));
                }
                Err(err) => {
                    let _ = tx.send(ScanMsg::Failed(err.to_string()));
                }
            }
        });
    }

    pub(super) fn poll_scan(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(rx) = &self.scan_rx {
            loop {
                match rx.try_recv() {
                    Ok(msg) => messages.push(msg),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
        }

        for msg in messages {
            match msg {
                ScanMsg::Progress(indexed) => {
                    self.scan_status = Some(format!("Scanning… {indexed} tracks"));
                    ctx.request_repaint();
                }
                ScanMsg::Done(payload) => {
                    let (mut library, report) = *payload;
                    library.hide(&self.hidden);
                    let tracks = library.len();
                    self.library = library;
                    self.covers.clear();
                    self.scan_rx = None;
                    self.scan_status = None;
                    self.config.music_dir = self.music_dir.clone();
                    // Loaded at startup, so it would write back the old value
                    // and undo any toggle since.
                    self.config.allow_video_movement = allow_video_movement();
                    self.config.save();
                    if let Some(path) = cache_file() {
                        if let Err(err) = self.library.save_cache(&path) {
                            log::warn!("could not write the library cache: {err}");
                        }
                    }
                    if report.failed > 0 {
                        self.toast(format!(
                            "Indexed {tracks} tracks ({} skipped)",
                            report.failed
                        ));
                    } else {
                        self.toast(format!("Indexed {tracks} tracks in {:.2?}", report.elapsed));
                    }
                    // A file was requested on the command line: start it now.
                    if let Some(path) = self.pending_play.take() {
                        if self.play_path(&path) {
                            self.view = View::Songs;
                        } else {
                            self.toast(format!("Not found in the library: {}", path.display()));
                        }
                    }
                }
                ScanMsg::Failed(err) => {
                    self.scan_rx = None;
                    self.scan_status = None;
                    self.toast(format!("Scan failed: {err}"));
                }
            }
        }
    }

    /// "Songs → Clear cache": drop the cached library index and the hidden
    /// list, then re-index the folder, so the library is back to what is on
    /// disk.
    pub(super) fn clear_cache(&mut self) {
        self.hidden.clear();
        for path in [hidden_file(), cache_file()].into_iter().flatten() {
            let _ = std::fs::remove_file(path);
        }
        match self.music_dir.clone() {
            Some(dir) => self.start_scan(dir),
            None => self.toast("Cache cleared".to_string()),
        }
    }

    /// Removes the current playlist's tracks from the library.
    ///
    /// The files stay on disk: the tracks are hidden the same way
    /// <kbd>Del</kbd> hides them, so a rescan does not bring them back.
    pub(super) fn remove_stuff_from_library(&mut self) {
        let ids: Vec<TrackId> = match &self.loaded_playlist {
            Some((_, ids)) => ids.clone(),
            None => return,
        };
        if ids.is_empty() {
            return;
        }
        // Only the playlist's own tracks leave the library: the rest of the
        // hidden list is untouched, or tracks a playlist re-imported would
        // vanish with it.
        self.library.hide(&ids.iter().copied().collect());
        self.hidden.extend(ids);
        save_hidden(&self.hidden);
    }

    /// The song list as it is on screen: the loaded playlist (or the whole
    /// library) narrowed by the search box. What `Save` writes, so a saved
    /// file always matches the view.
    pub(super) fn visible_songs(&self) -> Vec<&Track> {
        let query = self.search.trim().to_lowercase();
        match &self.loaded_playlist {
            None if query.is_empty() => self.library.tracks().iter().collect(),
            None => self.library.search(&query, 500),
            // A loaded playlist keeps its own order; the search box only
            // narrows within it, reusing the library's matcher.
            Some((_, ids)) => {
                let hits: HashSet<TrackId> = if query.is_empty() {
                    HashSet::new()
                } else {
                    self.library
                        .search(&query, usize::MAX)
                        .iter()
                        .map(|t| t.id)
                        .collect()
                };
                ids.iter()
                    .filter_map(|id| self.library.track(*id))
                    .filter(|t| query.is_empty() || hits.contains(&t.id))
                    .collect()
            }
        }
    }
}
