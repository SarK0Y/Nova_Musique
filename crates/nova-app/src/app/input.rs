//! Keyboard shortcuts and applying the `Action`s collected during a frame.

use super::*;

impl NovaApp {
    pub(super) fn handle_input(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (toggle, next, prev, info, del, escape, window) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowRight),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::I),
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::F) && !i.modifiers.command && !i.modifiers.alt,
            )
        });
        if toggle {
            self.engine.toggle();
        }
        if next {
            self.engine.next(&self.library);
        }
        if prev {
            self.engine.previous(&self.library);
        }
        if info {
            self.info_open = !self.info_open;
        }
        if del {
            self.delete_current();
        }
        // The video hides the sidebar, so this is the only way back to the
        // library once a clip has finished and nothing is queued behind it.
        // Esc backs out of full screen first; only from a normal or maximised
        // window does it stop the video.
        if escape {
            if self.window_mode == WindowMode::Fullscreen {
                self.set_window_mode(ctx, WindowMode::Normal);
            } else if self.video_showing() {
                self.engine.stop();
            }
        }
        // F: normal → full screen → maximised → normal, for as long as a video
        // is up. If the video ended while the window was still full screen,
        // F is the way back out.
        if window {
            if self.video_showing() {
                self.cycle_window_mode(ctx);
            } else if self.window_mode == WindowMode::Fullscreen {
                self.set_window_mode(ctx, WindowMode::Normal);
            }
        }
    }

    /// <kbd>Del</kbd>. In Songs the highlighted track is hidden from the
    /// library; anywhere else it is dropped from the queue.
    pub(super) fn delete_current(&mut self) {
        let Some(track) = self.engine.current_track(&self.library) else {
            return;
        };
        let id = track.id;

        if self.view == View::Songs {
            self.hidden.insert(id);
            save_hidden(&self.hidden);
            // Only this track leaves the library: the rest of the hidden list
            // is untouched, or tracks a playlist re-imported would vanish
            // with it.
            self.library.hide(&HashSet::from([id]));
        } else {
            self.engine.queue_mut().remove(id);
        }
        // Either way the file we were playing is gone from where it just was.
        self.engine.stop();
    }

    pub(super) fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::SetView(view) => {
                    self.view = view;
                    if view != View::Albums {
                        self.opened_album = None;
                    }
                    if view != View::Artists {
                        self.artist_filter = None;
                    }
                }
                Action::OpenAlbum(key) => self.opened_album = Some(key),
                Action::CloseAlbum => self.opened_album = None,
                Action::OpenArtist(name) => {
                    self.artist_filter = Some(name);
                    self.view = View::Artists;
                }
                Action::CloseArtist => self.artist_filter = None,
                Action::SelectPath(path) => self.select_path(PathBuf::from(path)),
                Action::PlayList(ids, start) => {
                    self.selected = ids.get(start).copied();
                    if let Err(err) = self.engine.set_queue(&self.library, ids, start) {
                        self.toast(err.to_string());
                    }
                }
                Action::PlayQueueIndex(index) => {
                    self.selected = self.engine.queue().items().get(index).copied();
                    if let Err(err) = self.engine.play_index(&self.library, index) {
                        self.toast(err.to_string());
                    }
                }
                Action::Enqueue(id) => self.engine.enqueue(id),
                Action::RemoveFromQueue(id) => self.engine.queue_mut().remove(id),
                Action::ClearQueue => {
                    self.engine.stop();
                    self.engine.queue_mut().clear();
                }
                Action::Toggle => self.engine.toggle(),
                Action::Next => self.engine.next(&self.library),
                Action::Prev => self.engine.previous(&self.library),
                Action::Seek(seconds) => {
                    let pos = Duration::from_secs_f32(seconds.max(0.0));
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.engine.seek(pos)
                    }));
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => self.toast(err.to_string()),
                        Err(_) => {
                            // rodio may panic on seek (e.g. underflow in
                            // SamplesBuffer::try_seek).  Reset the transport
                            // so the UI stays consistent.
                            self.engine.stop();
                            self.toast("Seek failed — reset to start".to_string());
                        }
                    }
                }
                Action::SetVolume(volume) => {
                    self.engine.set_volume(volume);
                    self.config.volume = volume;
                    self.config.allow_video_movement = allow_video_movement();
                    self.config.save();
                }
                Action::ToggleShuffle => self.engine.queue_mut().toggle_shuffle(),
                Action::CycleRepeat => self.engine.queue_mut().cycle_repeat(),
                Action::SavePlaylist(path) => {
                    // "as is" keeps the current playlist (or the default one);
                    // "as…" hands over the path the dialog returned.
                    let target = path
                        .or_else(|| self.config.current_playlist.clone())
                        .or_else(default_playlist_file);
                    let Some(target) = target else {
                        self.toast("Nowhere to save the playlist".to_string());
                        continue;
                    };
                    let saved = {
                        let tracks = self.visible_songs();
                        let count = tracks.len();
                        write_playlist(&target, &tracks).map(|()| count)
                    };
                    match saved {
                        Ok(count) => {
                            self.config.current_playlist = Some(target.clone());
                            // Persists the config, current playlist included.
                            self.remember_playlist(&target);
                            self.toast(format!("Saved {count} track(s) to {}", target.display()));
                        }
                        Err(err) => self.toast(format!("Save failed: {err}")),
                    }
                }
                Action::LoadPlaylist(path) => self.open_playlist(&path),
                Action::UnloadPlaylist => self.loaded_playlist = None,
                Action::ClearPlaylist => {
                    // First drop the playlist's tracks from the library, then
                    // clear the playlist itself (file on disk, list on screen).
                    self.remove_stuff_from_library();
                    self.fake_reload_playlist();
                }
                Action::ClearCache => self.clear_cache(),
                Action::OpenNfm => self.nfm_open = true,
            }
        }
    }
}
