//! `.m3u8` playlists: reading, writing, the Load and Save menus and the playlist window.

use super::*;

/// The playlist as UTF-8 M3U8 — the one format mpv, VLC and friends all read.
///
/// An `#EXTINF` line names each track so other players show titles rather
/// than file names; `-1` is the format's "duration unknown".
/// The default contents of a playlist file: the header of a standard `.m3u8`,
/// with no tracks in it.
pub(super) const DEFAULT_PLAYLIST: &str = "#EXTM3U\n";

pub(super) fn write_playlist(path: &Path, tracks: &[&Track]) -> std::io::Result<()> {
    let mut body = String::from(DEFAULT_PLAYLIST);
    for track in tracks {
        let name = if track.artist.is_empty() {
            track.title.clone()
        } else {
            format!("{} - {}", track.artist, track.title)
        };
        let seconds = track.duration.map(|d| d.round() as i64).unwrap_or(-1);
        body.push_str(&format!("#EXTINF:{seconds},{name}\n"));
        body.push_str(&track.path.to_string_lossy());
        body.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)
}

/// The file paths an M3U8 playlist lists, in order. Comment lines (`#EXTM3U`,
/// `#EXTINF`, …) are skipped; a relative line is read against the playlist's
/// own folder, the way mpv and VLC do.
pub(super) fn read_playlist(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let body = std::fs::read_to_string(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    Ok(body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let file = PathBuf::from(line);
            if file.is_relative() {
                dir.join(file)
            } else {
                file
            }
        })
        .collect())
}

impl NovaApp {
    /// Keep `path` at the head of the Load menu's list, newest first, and
    /// write the list back to the config.
    pub(super) fn remember_playlist(&mut self, path: &Path) {
        self.config.playlists.retain(|p| p != path);
        self.config.playlists.insert(0, path.to_path_buf());
        self.config.playlists.truncate(10); // shortcut: fixed menu length; raise the cap if the list feels short
        self.config.save();
    }

    /// Reloads the current playlist with its default data — the header of a
    /// standard `.m3u8` file, with no tracks in it.
    ///
    /// No file is read: the file on disk is written with the same default
    /// data and the in-memory list is reset, so the playlist stays the active
    /// one but is empty.
    ///
    /// No-op when no playlist is current, so callers do not need to guard.
    pub(super) fn fake_reload_playlist(&mut self) {
        let Some(path) = self.config.current_playlist.clone() else {
            return;
        };
        let _ = std::fs::write(&path, DEFAULT_PLAYLIST);
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        self.loaded_playlist = Some((name, Vec::new()));
    }

    /// Show `path`'s tracks in place of the whole library and make it the
    /// current playlist. Shared by the Load menu and by startup, which
    /// replays the config's current playlist.
    pub(super) fn open_playlist(&mut self, path: &Path) {
        let Ok(listed) = read_playlist(path) else {
            self.toast(format!("Load failed: {}", path.display()));
            return;
        };
        // The library hashed canonical paths at scan time, so resolve the
        // same way a scan would — and read tags for files it never indexed:
        // a playlist is a list of files, not of library entries.
        let listed: Vec<PathBuf> = listed
            .into_iter()
            .map(|file| std::fs::canonicalize(&file).unwrap_or(file))
            .collect();
        self.import_missing(&listed);
        let mut ids = Vec::with_capacity(listed.len());
        let mut missing = 0usize;
        for file in &listed {
            match self.library.track(track_id(file)) {
                Some(track) => ids.push(track.id),
                None => missing += 1,
            }
        }
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let found = ids.len();
        self.loaded_playlist = Some((name, ids));
        // The loaded file becomes the one "as is" writes.
        self.config.current_playlist = Some(path.to_path_buf());
        self.remember_playlist(path);
        let skipped = if missing > 0 {
            format!(", {missing} not in the library")
        } else {
            String::new()
        };
        self.toast(format!("Loaded {found} track(s){skipped}"));
    }

    /// The Save menu over the list as it is on screen: `as is` overwrites the
    /// current playlist file, `as…` picks a new one and makes it current.
    pub(super) fn save_menu(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.menu_button("Save", |ui| {
            self.dbg_menu.set("save");
            if ui.button("as is").clicked() {
                actions.push(Action::SavePlaylist(None));
                ui.close();
            }
            if ui.button("as…").clicked() {
                let mut dialog = rfd::FileDialog::new().set_file_name("playlist.m3u8");
                if let Some(dir) = self.music_dir.as_deref() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.save_file() {
                    actions.push(Action::SavePlaylist(Some(path)));
                }
                ui.close();
            }
        });
    }

    /// The Load menu over the playlists this install has opened or saved,
    /// plus `new…` to pick any other file. Loading shows that playlist in
    /// place of the whole library; `all songs` goes back.
    pub(super) fn load_menu(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.menu_button("Load", |ui| {
            self.dbg_menu.set("load");
            // The menu is as wide as its longest entry: names stay on one
            // line and the hovered highlight cannot poke out of the panel.
            let mut labels: Vec<String> = Vec::new();
            if self.loaded_playlist.is_some() {
                labels.push("all songs".to_owned());
            }
            for path in &self.config.playlists {
                labels.push(file_label(path));
            }
            if labels.is_empty() {
                labels.push("No playlists yet.".to_owned());
            }
            labels.push("new…".to_owned());
            let font = egui::TextStyle::Body.resolve(ui.style());
            let widest = labels
                .iter()
                .map(|label| {
                    ui.ctx().fonts_mut(|f| {
                        f.layout_no_wrap(label.clone(), font.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    })
                })
                .fold(0.0, f32::max);
            ui.set_min_width(widest + 24.0);

            if self.loaded_playlist.is_some() && ui.button("all songs").clicked() {
                actions.push(Action::UnloadPlaylist);
                ui.close();
            }
            if self.config.playlists.is_empty() {
                ui.label(RichText::new("No playlists yet.").weak());
            }
            for path in &self.config.playlists {
                let label = file_label(path);
                if ui
                    .button(&label)
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    actions.push(Action::LoadPlaylist(path.clone()));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("new…").clicked() {
                let mut dialog = rfd::FileDialog::new().add_filter("Playlist", &["m3u8", "m3u"]);
                if let Some(dir) = self.music_dir.as_deref() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    actions.push(Action::LoadPlaylist(path));
                }
                ui.close();
            }
        })
        .response
        .on_hover_text("playlist");
    }

    pub(super) fn playlist_window(&mut self, ctx: &egui::Context) {
        let viewport_id = egui::ViewportId::from_hash_of("nova-playlist-window");
        let builder = egui::ViewportBuilder::default()
            .with_title("Nova Musique — Playlist")
            .with_inner_size([380.0, 300.0])
            .with_min_inner_size([280.0, 180.0]);

        ctx.show_viewport_immediate(viewport_id, builder, |viewport_ctx, _class| {
            if viewport_ctx.input(|input| input.viewport().close_requested()) {
                self.playlist_open = false;
                return;
            }

            // Native file-manager drops are delivered to the viewport that is
            // under the pointer. Import them here (rather than in the main
            // viewport) so files can be dropped directly onto this window.
            let external_drops: Vec<PathBuf> = viewport_ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect()
            });
            if !external_drops.is_empty() {
                self.enqueue_dropped_files(external_drops);
            }

            let mut actions = Vec::new();
            let panel = egui::CentralPanel::default().show(viewport_ctx, |ui| {
                // The queue can also be rendered in the main window. Give this
                // viewport its own widget-ID namespace so ScrollArea state,
                // row interactions, and focus cannot collide between windows.
                ui.push_id("external-playlist-window", |ui| {
                    ui.heading("Playlist");
                    ui.label(
                        RichText::new(
                            "Drop audio files from your file manager, or drag tracks here",
                        )
                        .weak()
                        .size(12.0),
                    );
                    ui.separator();
                    self.queue_view(ui, &mut actions);
                });
            });

            // egui's drag payload is shared by the viewports. Consume it only when
            // the pointer is released over the playlist window, then append the
            // track to the existing queue without interrupting current playback.
            let (released, pointer_pos) = viewport_ctx
                .input(|input| (input.pointer.any_released(), input.pointer.interact_pos()));
            if released && pointer_pos.is_some_and(|pos| panel.response.rect.contains(pos)) {
                if let Some(track_id) = egui::DragAndDrop::take_payload::<TrackId>(viewport_ctx) {
                    actions.push(Action::Enqueue(*track_id));
                }
            }
            self.apply(actions);
        });
    }

    /// Adds dropped or picked files to the open playlist, walking folders for
    /// audio. Tracks already listed are not duplicated; new files get their
    /// tags read first so they show up even if never scanned.
    pub(super) fn add_to_playlist(&mut self, paths: Vec<PathBuf>) {
        if self.loaded_playlist.is_none() {
            return;
        }
        let (files, skipped) = expand_to_audio(paths);
        if files.is_empty() {
            self.toast(if skipped > 0 {
                format!("Skipped {skipped} unsupported file(s)")
            } else {
                "Nothing to add to the playlist".to_string()
            });
            return;
        }

        self.import_missing(&files);

        let ids: Vec<TrackId> = files
            .iter()
            .filter_map(|path| self.find_track_by_path(path))
            .collect();
        let mut added = 0usize;
        if let Some((_, list)) = self.loaded_playlist.as_mut() {
            for id in ids {
                if !list.contains(&id) {
                    list.push(id);
                    added += 1;
                }
            }
        }
        let name = self
            .loaded_playlist
            .as_ref()
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        if added == 0 {
            self.toast(format!("Nothing new to add to {name}"));
            return;
        }
        let skipped_note = if skipped > 0 {
            format!(", skipped {skipped} unsupported")
        } else {
            String::new()
        };
        self.toast(format!("Added {added} track(s) to {name}{skipped_note}"));
    }
}
