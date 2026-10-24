//! The library views: songs, albums, artists, queue, sidebar and the track-info panel.

use super::*;

/// Renders the cover, the primary text fields and the technical details of
/// `track` inside the info panel. Split out from [`NovaApp::track_info_panel`]
/// so it can be exercised without a full application.
pub(super) fn track_info_body(ui: &mut egui::Ui, track: &Track, cover: Option<&egui::TextureHandle>) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        // Embedded cover art (NFM containers and tagged audio alike).
        let cover_size = ui.available_width().min(320.0);
        cover_thumb(ui, cover, cover_size);
        ui.add_space(10.0);

        // Primary text fields.
        ui.label(RichText::new(&track.title).strong().size(17.0));
        ui.label(RichText::new(track.display_artist()).size(13.0));
        ui.label(RichText::new(track.display_album()).weak().size(13.0));

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        egui::Grid::new("track-info-grid")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for (label, value) in track_info_rows(track) {
                    info_row(ui, label, &value);
                }
            });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(RichText::new("FILE").weak().size(11.0));
        ui.add(
            egui::Label::new(
                RichText::new(track.path.display().to_string())
                    .monospace()
                    .weak()
                    .size(11.0),
            )
            .wrap(),
        );
    });
}

/// The `label → value` rows of the info panel's details grid.
///
/// Optional fields the file does not carry are left out entirely rather than
/// rendered as placeholders, so a bare container does not sprout empty rows.
pub(super) fn track_info_rows(track: &Track) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Format", format_label(&track.path))];

    if !track.album_artist.is_empty() {
        rows.push(("Album artist", track.album_artist.clone()));
    }
    if let Some(genre) = track.genre.as_deref().filter(|g| !g.is_empty()) {
        rows.push(("Genre", genre.to_string()));
    }
    if let Some(year) = track.year {
        rows.push(("Year", year.to_string()));
    }
    match (track.track_no, track.disc_no) {
        (Some(track_no), Some(disc_no)) => {
            rows.push(("Track", format!("Disc {disc_no} · Track {track_no}")));
        }
        (Some(track_no), None) => rows.push(("Track", track_no.to_string())),
        (None, Some(disc_no)) => rows.push(("Disc", disc_no.to_string())),
        (None, None) => {}
    }

    rows.push(("Duration", format_duration(track.duration())));
    if let Some(resolution) = track.resolution() {
        rows.push(("Resolution", resolution));
    }
    if let Some(rate) = track.sample_rate.filter(|r| *r > 0) {
        rows.push(("Sample rate", format_sample_rate(rate)));
    }
    if let Some(channels) = track.channels.filter(|c| *c > 0) {
        rows.push(("Channels", format_channels(channels)));
    }
    rows.push((
        "Embedded art",
        if track.has_cover { "Yes" } else { "No" }.to_string(),
    ));

    rows
}

/// One list row: number, title, artist, duration. The playing track swaps its
/// number for a marker, so the cursor highlight stays the only filled row.
pub(super) fn track_row_text(track: &Track, playing: bool) -> String {
    let number = if track.is_video() {
        // Films and clips share the music list, so mark the odd ones out.
        "V".to_string()
    } else if playing {
        "\u{25b6}".to_string()
    } else {
        track
            .track_no
            .map(|n| n.to_string())
            .unwrap_or_else(|| "\u{b7}".into())
    };
    format!(
        "{:>3}   {:<46}  {:<22}  {}",
        number,
        truncate(&track.title, 46),
        truncate(track.display_artist(), 22),
        format_duration(track.duration()),
    )
}

/// One `label → value` row of the track-info grid.
pub(super) fn info_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).weak().size(12.0));
    ui.label(RichText::new(value).size(12.0));
    ui.end_row();
}

impl NovaApp {
    pub(super) fn center(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let query = self.search.trim().to_lowercase();
        if !query.is_empty() {
            let results = self.visible_songs();
            self.songs_header(ui, actions);
            ui.add_space(4.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.track_list(ui, &results, actions);
            });
            return;
        }

        match self.view {
            View::Songs => {
                self.songs_header(ui, actions);
                ui.label(RichText::new("Click a track to play it.").weak().size(12.0));
                ui.add_space(4.0);
                let tracks = self.visible_songs();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.track_list(ui, &tracks, actions);
                });
            }
            View::Albums => match self.opened_album.clone() {
                Some(key) => self.album_detail(ui, &key, actions),
                None => self.album_grid(ui, actions),
            },
            View::Artists => match self.artist_filter.clone() {
                Some(name) => self.artist_detail(ui, &name, actions),
                None => self.artist_list(ui, actions),
            },
            View::Queue => self.queue_view(ui, actions),
        }
    }

    /// The row above the song list. Drawn identically with and without a
    /// search filter, so `Clear all` never disappears behind a typed query.
    ///
    /// `Clear all` is a no-op when the whole library is on screen; it appears
    /// only while a playlist is loaded, where it clears it.
    pub(super) fn songs_header(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.heading("Songs");
            if let Some((name, _)) = &self.loaded_playlist {
                ui.label(RichText::new(name).weak());
                // Fires on release: a click that ends on the button (not a
                // click-drag that started here and left).
                if ui
                    .button("Clear all")
                    .on_hover_text("current playlist")
                    .clicked()
                {
                    actions.push(Action::ClearPlaylist);
                }
            }
            self.save_menu(ui, actions);
            self.load_menu(ui, actions);
            if ui.button("To NFM").clicked() {
                actions.push(Action::OpenNfm);
            }
            if ui.button("Clear cache").clicked() {
                actions.push(Action::ClearCache);
            }
        });
    }

    /// Renders a clickable list of tracks. Clicking one replaces the queue with
    /// this very list, starting at the clicked track.
    pub(super) fn track_list(&self, ui: &mut egui::Ui, tracks: &[&Track], actions: &mut Vec<Action>) {
        if tracks.is_empty() {
            ui.label(RichText::new("No tracks.").weak());
            return;
        }

        let ids: Vec<TrackId> = tracks.iter().map(|t| t.id).collect();
        let current = self.engine.queue().current();

        // Arrow keys walk the list, Enter plays whatever the cursor is on.
        // Enter stays with a focused text field so typing is not hijacked.
        let typing = ui.ctx().egui_wants_keyboard_input();
        let (down, up, enter) = ui.ctx().input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter) && !typing,
            )
        });
        let last = tracks.len() - 1;
        let cursor = if down {
            (self.cursor.get() + 1).min(last)
        } else if up {
            self.cursor.get().saturating_sub(1)
        } else {
            self.cursor.get().min(last)
        };
        self.cursor.set(cursor);

        for (index, track) in tracks.iter().enumerate() {
            let playing = current == Some(track.id);
            let response = ui.add_sized(
                [ui.available_width(), ROW_HEIGHT],
                egui::Button::selectable(
                    index == cursor,
                    RichText::new(track_row_text(track, playing)).size(14.0),
                ),
            );

            // Same rule as queue_view: follow the cursor only on the frames
            // the arrow keys move it, or the wheel scroll gets yanked back.
            if index == cursor && (down || up) && !ui.clip_rect().contains_rect(response.rect) {
                response.scroll_to_me(Some(Align::Center));
            }
            if response.clicked() {
                self.cursor.set(index);
                actions.push(Action::PlayList(ids.clone(), index));
            }
            // Tracks can be dragged from any library list into the external playlist window.
            if response.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), track.id);
            }
            response.context_menu(|ui| {
                if ui.button("Play now").clicked() {
                    actions.push(Action::PlayList(ids.clone(), index));
                    ui.close();
                }
                if ui.button("Play next").clicked() {
                    actions.push(Action::Enqueue(track.id));
                    ui.close();
                }
            });
        }

        if enter {
            actions.push(Action::PlayList(ids.clone(), cursor));
        }
    }

    pub(super) fn album_grid(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.heading("Albums");
        ui.add_space(4.0);

        const CARD: f32 = 150.0;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let available = ui.available_width().max(CARD);
            let columns = ((available + 16.0) / (CARD + 16.0)).floor().max(1.0) as usize;

            egui::Grid::new("album-grid")
                .num_columns(columns)
                .spacing([16.0, 16.0])
                .show(ui, |ui| {
                    for (index, album) in self.library.albums().iter().enumerate() {
                        if index > 0 && index % columns == 0 {
                            ui.end_row();
                        }

                        ui.vertical(|ui| {
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(CARD, CARD), egui::Sense::click());
                            match self
                                .album_cover_source(&album.key)
                                .and_then(|id| self.covers.get(id))
                            {
                                Some(texture) => {
                                    egui::Image::new((texture.id(), egui::vec2(CARD, CARD)))
                                        .paint_at(ui, rect);
                                }
                                None => {
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "♪",
                                        FontId::proportional(40.0),
                                        ui.visuals().weak_text_color(),
                                    );
                                }
                            }
                            if response.clicked() {
                                actions.push(Action::OpenAlbum(album.key.clone()));
                            }
                            ui.label(RichText::new(truncate(&album.title, 22)).strong());
                            ui.label(
                                RichText::new(truncate(&album.album_artist, 22))
                                    .weak()
                                    .size(12.0),
                            );
                        });
                    }
                    ui.end_row();
                });
        });
    }

    pub(super) fn album_detail(&self, ui: &mut egui::Ui, key: &AlbumKey, actions: &mut Vec<Action>) {
        let tracks = self.library.album_tracks(key);

        ui.horizontal(|ui| {
            if ui.button("← Albums").clicked() {
                actions.push(Action::CloseAlbum);
            }
            if ui.button("▶ Play album").clicked() && !tracks.is_empty() {
                actions.push(Action::PlayList(tracks.iter().map(|t| t.id).collect(), 0));
            }
        });
        ui.add_space(4.0);
        ui.heading(&key.album);
        ui.label(RichText::new(&key.album_artist).weak());
        ui.add_space(6.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.track_list(ui, &tracks, actions);
        });
    }

    pub(super) fn artist_list(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.heading("Artists");
        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            if self.library.artists().is_empty() {
                ui.label(RichText::new("No artists yet — scan a folder.").weak());
            }
            for artist in self.library.artists() {
                let text = format!(
                    "{}   ·   {} tracks · {} albums",
                    artist.name, artist.track_count, artist.album_count
                );
                if ui
                    .add_sized(
                        [ui.available_width(), 26.0],
                        egui::Button::selectable(false, text),
                    )
                    .clicked()
                {
                    actions.push(Action::OpenArtist(artist.name.clone()));
                }
            }
        });
    }

    pub(super) fn artist_detail(&self, ui: &mut egui::Ui, name: &str, actions: &mut Vec<Action>) {
        let tracks = self.library.artist_tracks(name);

        ui.horizontal(|ui| {
            if ui.button("← Artists").clicked() {
                actions.push(Action::CloseArtist);
            }
            if ui.button("▶ Play all").clicked() && !tracks.is_empty() {
                actions.push(Action::PlayList(tracks.iter().map(|t| t.id).collect(), 0));
            }
        });
        ui.add_space(4.0);
        ui.heading(name);
        ui.add_space(6.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.track_list(ui, &tracks, actions);
        });
    }

    pub(super) fn queue_view(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.heading("Queue");
            if ui.button("Clear all").clicked() {
                actions.push(Action::ClearQueue);
            }
        });
        ui.add_space(4.0);

        let current_index = self.engine.queue().current_item_index();
        let items: Vec<TrackId> = self.engine.queue().items().to_vec();

        let typing = ui.ctx().egui_wants_keyboard_input();
        let (down, up, enter) = ui.ctx().input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter) && !typing,
            )
        });
        let last = items.len().saturating_sub(1);
        let cursor = if down {
            (self.cursor.get() + 1).min(last)
        } else if up {
            self.cursor.get().saturating_sub(1)
        } else {
            self.cursor.get().min(last)
        };
        self.cursor.set(cursor);

        egui::ScrollArea::vertical().show(ui, |ui| {
            if items.is_empty() {
                ui.label(RichText::new("The queue is empty.").weak());
                return;
            }
            for (index, id) in items.iter().enumerate() {
                let Some(track) = self.library.track(*id) else {
                    continue;
                };
                let response = ui.add_sized(
                    [ui.available_width(), ROW_HEIGHT],
                    egui::Button::selectable(
                        index == cursor,
                        RichText::new(track_row_text(track, current_index == Some(index)))
                            .size(14.0),
                    ),
                );
                // Follow the cursor only on the frames the arrow keys move
                // it. Doing this whenever the row merely sits partly outside
                // the view yanks the list back over any wheel or scrollbar
                // scroll, which looks like a list that will not move.
                if index == cursor && (down || up) && !ui.clip_rect().contains_rect(response.rect) {
                    response.scroll_to_me(Some(Align::Center));
                }
                if response.clicked() {
                    self.cursor.set(index);
                    actions.push(Action::PlayQueueIndex(index));
                }
                response.context_menu(|ui| {
                    if ui.button("Remove from queue").clicked() {
                        actions.push(Action::RemoveFromQueue(*id));
                        ui.close();
                    }
                });
            }
        });

        if enter && !items.is_empty() {
            actions.push(Action::PlayQueueIndex(cursor));
        }
    }

    pub(super) fn side_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.add_space(6.0);
        ui.label(RichText::new("LIBRARY").weak().size(11.0));
        ui.add_space(2.0);

        for view in View::ALL {
            let selected = self.view == view;
            let label = match view {
                View::Songs => format!("Songs   ({})", self.library.len()),
                View::Albums => format!("Albums  ({})", self.library.albums().len()),
                View::Artists => format!("Artists ({})", self.library.artists().len()),
                View::Queue => format!("Queue   ({})", self.engine.queue().len()),
            };
            if ui
                .add_sized(
                    [ui.available_width(), 26.0],
                    egui::Button::new(label).selected(selected),
                )
                .clicked()
            {
                actions.push(Action::SetView(view));
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("NOW PLAYING").weak().size(11.0));
        ui.add_space(4.0);

        match self.engine.current_track(&self.library) {
            Some(track) => {
                cover_thumb(ui, self.covers.get(track.id), 150.0);
                ui.add_space(4.0);
                ui.label(RichText::new(&track.title).strong());
                ui.label(RichText::new(track.display_artist()).weak());
                ui.label(RichText::new(track.display_album()).weak().size(12.0));
            }
            None => {
                ui.label(RichText::new("Nothing playing").weak());
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("Audio backend: {}", self.engine.backend_name()))
                .weak()
                .size(11.0),
        );
    }

    /// Track shown in the info panel: whatever is playing, otherwise the last
    /// track the user explicitly started.
    pub(super) fn info_track(&self) -> Option<&Track> {
        self.engine
            .current_track(&self.library)
            .or_else(|| self.selected.and_then(|id| self.library.track(id)))
    }

    /// Right-hand panel with the embedded art plus the text fields and the
    /// technical details of the current track.
    pub(super) fn track_info_panel(&self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("TRACK INFO").weak().size(11.0));
        ui.add_space(6.0);

        let Some(track) = self.info_track() else {
            ui.label(RichText::new("Nothing selected.").weak());
            return;
        };

        track_info_body(ui, track, self.covers.get(track.id));
    }

    /// Render the playlist in its own native OS viewport, separate from the main window.
    /// The "N item(s) found" message, drawn as a window inside the main
    /// viewport so it shares this window's GL context instead of blocking on
    /// a native dialog.
    pub(super) fn search_report_window(&mut self, ctx: &egui::Context) {
        let Some(found) = self.search_report else {
            return;
        };
        let fresh = std::mem::take(&mut self.search_report_fresh);
        let mut open = true;
        let mut ok = false;
        egui::Window::new("Search")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("{found} item(s) found"));
                let button = ui.button("OK");
                // Only the opening frame: egui treats Enter/Space as a click
                // on the focused button, so this alone makes Enter dismiss
                // the message; re-taking focus every frame would trap the
                // keyboard here until the window closes.
                if fresh {
                    button.request_focus();
                }
                ok = button.clicked();
            });
        if !open || ok {
            self.search_report = None;
        }
    }
}
