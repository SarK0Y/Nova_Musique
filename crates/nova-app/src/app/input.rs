//! Keyboard shortcuts and applying the `Action`s collected during a frame.

use super::*;

impl NovaApp {
    pub(super) fn handle_input(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (toggle, next, prev, info, del, escape, window, pgup, pgdn, c, shift, command, alt) =
            ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::Space),
                    i.modifiers.command && i.key_pressed(egui::Key::ArrowRight),
                    i.modifiers.command && i.key_pressed(egui::Key::ArrowLeft),
                    i.key_pressed(egui::Key::I),
                    i.key_pressed(egui::Key::Delete),
                    i.key_pressed(egui::Key::Escape),
                    i.key_pressed(egui::Key::F) && !i.modifiers.command && !i.modifiers.alt,
                    i.key_pressed(egui::Key::PageUp),
                    i.key_pressed(egui::Key::PageDown),
                    i.key_pressed(egui::Key::C),
                    i.modifiers.shift,
                    i.modifiers.command,
                    i.modifiers.alt,
                )
            });
        // Plain arrows scrub the progress bar by 5 s; Ctrl+arrows stay
        // bound to prev/next above. 9/0 nudge the volume by 1 %.
        let (seek_back, seek_fwd, vol_down, vol_up) = ctx.input(|i| {
            let plain = !i.modifiers.command && !i.modifiers.alt;
            (
                plain && i.key_pressed(egui::Key::ArrowLeft),
                plain && i.key_pressed(egui::Key::ArrowRight),
                plain && i.key_pressed(egui::Key::Num9),
                plain && i.key_pressed(egui::Key::Num0),
            )
        });
        if (seek_back || seek_fwd) && self.engine.state() != PlaybackState::Stopped {
            let position = self.engine.position().as_secs_f32();
            actions.push(Action::Seek(step_seek(position, seek_fwd)));
        }
        if vol_down || vol_up {
            let volume = self.engine.volume();
            actions.push(Action::SetVolume(step_volume(volume, vol_up)));
        }
        if toggle {
            self.engine.toggle();
        }
        if next || pgdn {
            self.engine.next(&self.library);
        }
        if prev || pgup {
            self.engine.previous(&self.library);
        }
        // Shift-C puts the window away with playback carrying on; C pauses
        // first, so the music stops right there too.
        //let (shift, c) = Self::c_key(c, shift, command, alt);
        if c && !shift && !command && !alt {
            self.engine.pause();
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if c && shift {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
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

    /// <kbd>C</kbd> and <kbd>Shift</kbd>+<kbd>C</kbd> as one decision:
    /// `(pause, minimize)`.
    ///
    /// SHIFT+C pauses first and puts the window away; C puts the window
    /// away with playback carrying on. C with Ctrl or Alt belongs to someone
    /// else (copy, whatever the desktop bound it to), so it does nothing here.
    pub(super) fn c_key(c: bool, shift: bool, command: bool, alt: bool) -> (bool, bool) {
        if !c || command || alt {
            return (false, false);
        }
        (!shift, true)
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
                    // SMPlayer's `Core::setVolume` returns early when the
                    // value did not change, and keeps the volume in memory —
                    // its prefs are written once at the end, not per step.
                    // Writing config.json here would stall every drag frame.
                    if volume != self.engine.volume() {
                        self.engine.set_volume(volume);
                        self.config.volume = volume;
                        self.config.allow_video_movement = allow_video_movement();
                        self.volume_save = Some(Instant::now() + Duration::from_millis(400));
                    }
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

/// Where the ←/→ hotkeys put the playhead: a 5 s step, never before zero.
/// The top end is left to the backend, which already clamps to the duration.
pub(super) fn step_seek(position: f32, forward: bool) -> f32 {
    const STEP: f32 = 5.0;
    (position + if forward { STEP } else { -STEP }).max(0.0)
}

/// Where the 9/0 hotkeys put the volume: a 1 % step inside the slider's
/// 0.0..=1.0. Rounded to whole percents first so repeated presses stay on
/// exact values instead of drifting through float error.
pub(super) fn step_volume(volume: f32, up: bool) -> f32 {
    let cents = (volume * 100.0).round() + if up { 1.0 } else { -1.0 };
    (cents / 100.0).clamp(0.0, 1.0)
}

/// SMPlayer's `TimeSlider` state machine, one frame at a time
/// (`timeslider.cpp`).
///
/// `drag` is `None` while playback drives the handle and `Some((start,
/// current))` while the pointer holds it. `before` is the value the slider
/// was seeded with this frame, `after` the value it gave back. Returns the
/// new state plus a position to seek to, if this frame should seek:
///
/// - never while the handle is held (`dont_update`), and nothing is sent
///   mid-drag at all — SMPlayer's `update_while_seeking` defaults to off,
///   so the seek happens once, on release;
/// - on release only if the drag actually moved the handle
///   (`slider_has_moved`), which is what keeps a click that lands where
///   playback already is from seeking and juddering;
/// - a change with no pointer holding the slider — an arrow key on a
///   focused one — seeks straight away, like `valueChanged_slot` outside a
///   drag.
pub(super) fn seek_drag(
    drag: Option<(f32, f32)>,
    before: f32,
    after: f32,
    grabbed: bool,
    changed: bool,
    released: bool,
) -> (Option<(f32, f32)>, Option<f32>) {
    let mut drag = drag;
    if drag.is_none() && grabbed {
        drag = Some((before, before));
    }
    if let Some((start, current)) = drag.as_mut() {
        if changed {
            *current = after;
        }
        if released {
            let (start, end) = (*start, *current);
            return (None, (end != start).then_some(end));
        }
        return (drag, None);
    }
    (None, changed.then_some(after))
}
