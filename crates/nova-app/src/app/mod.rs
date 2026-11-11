//! The egui application: library browser plus transport bar.
//!
//! `NovaApp` and its frame loop live here; everything else is split by concern:
//!
//! - [`config`] — Persisted configuration: the JSON config file, the XDG paths, and `allow_video_movement`.
//! - [`types`] — Views, actions and the messages that cross thread and process boundaries.
//! - [`remote`] — Commands read from stdin, one per line, so a script can drive the app.
//! - [`covers`] — Cover art: the texture cache and the per-frame decode budget.
//! - [`text`] — Small formatting helpers for clocks, durations, file formats and labels.
//! - [`library`] — Scanning, indexing and the persisted library cache and hidden list.
//! - [`drops`] — Drag and drop, command-line targets and "play these files".
//! - [`playlist`] — `.m3u8` playlists: reading, writing, the Load and Save menus and the playlist window.
//! - [`nfm`] — Songs → To NFM: the conversion window and the background conversion.
//! - [`browse`] — The library views: songs, albums, artists, queue, sidebar and the track-info panel.
//! - [`chrome`] — The top bar and the bottom transport bar.
//! - [`video`] — In-window video: showing it, sizing it, and the commands from the video-stream picker.
//! - [`audio`] — The Audio window (SMPlayer's audio controls).
//! - [`input`] — Keyboard shortcuts and applying the `Action`s collected during a frame.
//! - `tests` — unit tests (compiled for `cargo test` only).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText};
use serde::{Deserialize, Serialize};

use crate::mpv::{deinterlace_filter, Mpv, VideoBackend};
use crate::videowin;
use nova_core357_11::metadata;
use nova_core357_11::{
    track_id, AlbumKey, Library, PlaybackEngine, PlaybackState, ScanReport, Track, TrackId,
};

/// How long a transient message stays on screen.
const TOAST_TTL: Duration = Duration::from_secs(6);
/// How many cover images to decode per frame, to avoid stalling the UI.
const COVER_BUDGET_PER_FRAME: usize = 6;
/// Height of one track row in the list views.
const ROW_HEIGHT: f32 = 24.0;

mod audio;
mod browse;
mod chrome;
mod config;
mod covers;
pub(crate) mod drops;
mod input;
mod library;
mod nfm;
mod playlist;
mod remote;
mod sub_timeline;
#[cfg(test)]
mod tests;
mod text;
mod types;
mod video;

use self::browse::*;
use self::config::*;
use self::covers::*;
use self::drops::*;
use self::input::*;
use self::library::*;
use self::nfm::*;
use self::playlist::*;
use self::remote::*;
use self::sub_timeline::*;
use self::text::*;
use self::types::*;
use self::video::*;

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// The whole application state.
pub struct NovaApp {
    library: Library,
    engine: PlaybackEngine,
    covers: CoverCache,
    view: View,
    opened_album: Option<AlbumKey>,
    artist_filter: Option<String>,
    search: String,
    folder_input: String,
    music_dir: Option<PathBuf>,
    /// Set when the app was launched pointing at a specific file, played as
    /// soon as startup indexing settles. Never touches the Folder setting.
    pending_play: Option<PathBuf>,
    scan_rx: Option<Receiver<ScanMsg>>,
    scan_status: Option<String>,
    /// The cached library and the current playlist, read on their own thread
    /// at startup so a big index does not hold up the first frame.
    init_rx: Option<Receiver<InitMsg>>,
    /// Async metadata loading results.
    meta_tx: Option<mpsc::Sender<MetaMsg>>,
    meta_rx: Option<Receiver<MetaMsg>>,
    config: Config,
    toast: Option<(String, Instant)>,
    /// `Some((start, target))` while a ←/→ hotkey run is in progress: the
    /// first press freezes the counter and each later press only moves the
    /// target, and the stream is set to it once when the key lifts — the
    /// press/release model of SMPlayer's `TimeSlider`.
    hotkey_seek: Option<(f32, f32)>,
    /// `Some((start, current))` while the seek handle is held: the progress
    /// bar stops following playback until the drag ends (SMPlayer's
    /// `TimeSlider` `dont_update`). The file keeps playing so the stream
    /// stays in sync; the seek itself goes out on release.
    seek_drag: Option<(f32, f32)>,
    /// When the pending 9/0 hotkey volume step may be applied (SMPlayer's
    /// 200 ms drag timer), so a rapid run of presses settles instead of
    /// firing one step per key.
    volume_step: Option<Instant>,
    /// PgUp/PgDn presses since the last playback-machinery reload. Both
    /// keys share this counter: every 8th press rebuilds the engine, which
    /// is the way out of a file wedged at its end.
    switch_presses: u32,
    /// When the in-memory volume reaches config.json; `None` when nothing
    /// is pending. A drag writes once it settles, not on every frame.
    volume_save: Option<Instant>,
    /// The active `.ctrl` sub-timeline of the file under playback, if one
    /// sits beside it. `Some` when the engine loads a file that has a
    /// `.ctrl`, `None` otherwise.
    active_ctrl: Option<Ctrl>,
    /// The media path `active_ctrl` governs, so a track switch drops a
    /// stale sub-timeline.
    active_ctrl_for: Option<PathBuf>,
    /// Ctrl is only active when explicitly enabled (Shift+Esc toggles).
    ctrl_enabled: bool,
    /// Whether the track-info panel is shown.
    info_open: bool,
    /// Last track the user explicitly played; the info panel falls back to it
    /// when nothing is currently playing.
    selected: Option<TrackId>,
    /// Track ids the user hid from the library, persisted in `hidden.json`.
    hidden: HashSet<TrackId>,
    /// Keyboard cursor row in the list on screen. `Cell` so the list
    /// renderers can move it without taking `&mut self`.
    cursor: std::cell::Cell<usize>,
    /// libmpv, when it could be started. `None` means video plays through the
    /// audio backend only, which works for songs and not much else.
    mpv: Option<Arc<Mpv>>,
    /// How many tracks the last Enter in the Search box reported; the count
    /// stays up as an in-window message until OK clears it.
    search_report: Option<usize>,
    /// Take keyboard focus for the message's OK button, once, on the frame
    /// the window opens — so Enter dismisses it without the mouse.
    search_report_fresh: bool,
    /// The playlist shown in place of the whole library: its file name plus
    /// the track ids it lists, in playlist order. `None` shows everything.
    loaded_playlist: Option<(String, Vec<TrackId>)>,
    /// "Songs → To NFM": the conversion window, the `-set-rate` value typed in
    /// it, the running conversion's (done, total) progress and failure count,
    /// and the per-file messages coming back from the conversion thread.
    nfm_open: bool,
    nfm_rate: String,
    /// The "Save to" folder as typed, so the field stays editable; written to
    /// the config when Go is pressed.
    nfm_dir: String,
    /// How many groups to split the playlist into, as typed. Defaults to one
    /// per core.
    nfm_threads: String,
    nfm_progress: Option<(usize, usize)>,
    nfm_errors: usize,
    /// When the running conversion started, for the elapsed clock; the run's
    /// duration is kept here (instead of cleared with the rest) so the window
    /// can show it after the run ends.
    nfm_started: Option<Instant>,
    nfm_elapsed: Option<Duration>,
    /// Which files the threads have started, so the window can name what is
    /// running right now. Cleared per file on completion.
    nfm_active: Vec<String>,
    /// Set by the Cancel button and read by every group thread, so the whole
    /// run stops at the next file boundary.
    nfm_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    nfm_rx: Option<Receiver<NfmMsg>>,
    /// Which menu is open this frame, for the /dev/shm/nova state dump.
    dbg_menu: std::cell::Cell<&'static str>,
    /// Floating playlist window, opened from the Nova menu in the top bar.
    playlist_open: bool,
    /// External video window: picks which video stream mpv should show.
    video_open: bool,
    /// External Audio control window, opened from Nova → Audio.
    audio_open: bool,
    audio_mode: String,
    audio_channels: String,
    audio_delay_ms: i32,
    audio_device: String,
    audio_karaoke: bool,
    audio_tempo: bool,
    audio_normalize: bool,
    external_audio_input: String,
    /// SMPlayer's *Mirror image* (`hflip`) and *Flip image* (`vflip`), asked of
    /// mpv as video filters rather than drawn flipped here.
    flip_x: bool,
    flip_y: bool,
    /// SMPlayer's *Zoom*: how far past the size that fits the picture is drawn.
    /// It scales the video widget and re-centres it, and nothing reaches mpv;
    /// `video_view` does the same by drawing the image that much larger.
    zoom: f32,
    /// Optional display aspect ratio override selected from Nova → Video.
    aspect_ratio_override: Option<f32>,
    /// Clockwise rotation selected from Nova → Video → Rotate.
    video_rotation: i32,
    /// SMPlayer-style optional noise-reduction and cleanup filters.
    noise_hqdn3d: bool,
    noise_deband: bool,
    noise_nlmeans: bool,
    /// SMPlayer's *Move left/right/up/down*: how far the picture is nudged off
    /// centre, in points, with +y running down like every other coordinate.
    /// The buttons in the video window nudge it; dragging the picture does the
    /// same, which is the only way to move it that feels direct.
    pan: egui::Vec2,
    /// Which of SMPlayer's *Deinterlace* choices is on the filter chain. The
    /// picker's combo box shows this one and changes it.
    deinterlace: usize,
    /// That window's process, started on demand and killed when dropped.
    video_window: Option<videowin::VideoWindow>,
    /// Lines read from stdin, drained once per frame.
    remote: Option<Receiver<String>>,
    /// The engine state this frame, so a change is detected and timestamped
    /// for the `running` dump.
    last_engine_state: PlaybackState,
    /// When the current engine state began; the `running` file carries this
    /// and is cleared once it is 20 s old.
    state_started_at: Instant,
    /// How the main window is shown — normal, full screen or maximised.
    /// <kbd>F</kbd> cycles it while a video plays; see [`WindowMode`].
    window_mode: WindowMode,
}

impl NovaApp {
    /// Builds the app, restoring the cache and settings when available.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(8.0, 4.0);
            style
                .text_styles
                .insert(egui::TextStyle::Body, FontId::proportional(15.0));
        });

        let config = Config::load();
        // Seed the global from the stored setting, so the value the UI acts on
        // is the one the last session left behind.
        set_allow_video_movement(config.allow_video_movement);

        // An optional command-line target: a file, a folder, or a `file://` URL.
        // A folder is indexed and becomes the library folder; a file only
        // plays, leaving the Folder setting as configured. `-proxy-run` and
        // the player it names are not targets, so `parse_args` skips them.
        let mut pending_play = None;
        let mut requested_root = None;
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if let Some(arg) = crate::parse_args(&args).target {
            let target = parse_target(&arg.to_string_lossy());
            if target.is_file() {
                pending_play = Some(target);
            } else if target.is_dir() {
                requested_root = Some(target);
            } else {
                log::warn!(
                    "nova: ignoring argument that is not a file or folder: {}",
                    target.display()
                );
            }
        }

        let music_dir = requested_root.clone().or_else(|| config.music_dir.clone());
        // The hidden list is tiny, so it stays on this thread; the library
        // cache and the current playlist go to their own thread, because a
        // big index must not hold up the first frame. `poll_init` applies
        // them and runs the work that has to wait for them.
        let hidden = load_hidden();
        let (init_tx, init_rx) = mpsc::channel();
        let init_music_dir = music_dir.clone();
        let init_hidden = hidden.clone();
        let init_playlist = config.current_playlist.clone();
        std::thread::spawn(move || {
            let mut library = Library::default();
            if let Some(root) = init_music_dir.as_deref() {
                library = Library::from_tracks(Some(root.to_path_buf()), Vec::new());
            }
            library.hide(&init_hidden);

            // 1stly, load active playlist
            if let Some(ref playlist_path) = init_playlist {
                if playlist_path.exists() {
                    let load = load_playlist_into(&mut library, &playlist_path).clone();
                    // Send newly added tracks from playlist
                    if let Ok(ref load_res) = load {
                        let mut new_tracks = Vec::new();
                        for &id in &load_res.ids {
                            if let Some(t) = library.track(id) {
                                new_tracks.push(t.clone());
                            }
                        }
                        if !new_tracks.is_empty() {
                            let _ = init_tx.send(InitMsg::Tracks(new_tracks));
                        }
                    }
                    let _ = init_tx.send(InitMsg::Playlist(
                        playlist_path.clone(),
                        load.ok(),
                    ));
                }
            }

            // Then discover other files incrementally
            if let Some(root) = init_music_dir.as_deref() {
                let walker = walkdir::WalkDir::new(root).follow_links(true).into_iter();
                let mut batch = Vec::new();
                for entry in walker.flatten() {
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let path = entry.path();
                    if !nova_core357_11::metadata::is_supported(path) {
                        continue;
                    }
                    // Skip if already in library (e.g. from playlist)
                    if find_track_in(&library, path).is_some() {
                        continue;
                    }
                    let track = nova_core357_11::track_from_path(path);
                    // Add to library
                    let mut tracks = library.tracks().to_vec();
                    tracks.push(track.clone());
                    library = Library::from_tracks(Some(root.to_path_buf()), tracks);
                    batch.push(track);
                    if batch.len() >= 32 {
                        let _ = init_tx.send(InitMsg::Tracks(batch.drain(..).collect()));
                    }
                }
                if !batch.is_empty() {
                    let _ = init_tx.send(InitMsg::Tracks(batch));
                }
            }
            let _ = init_tx.send(InitMsg::Done);
            // Also send Ready for compatibility
            let _ = init_tx.send(InitMsg::Ready(Box::new(InitResult {
                library,
                playlist: None,
            })));
        });
        let mpv = Mpv::new(cc).map(Arc::new);
        let mut engine = PlaybackEngine::with_backend(Box::new(VideoBackend::new(mpv.clone())));
        engine.set_volume(config.volume);

        let folder_input = music_dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default();

        // Read the saved conversion folder before `config` is moved into the app.
        let nfm_dir = config
            .path_to_save_converted_files
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();

        let mut app = NovaApp {
            library: Library::default(),
            engine,
            covers: CoverCache::default(),
            view: View::Songs,
            opened_album: None,
            artist_filter: None,
            search: String::new(),
            folder_input,
            music_dir,
            pending_play,
            scan_rx: None,
            scan_status: None,
            init_rx: Some(init_rx),
            meta_tx: None,
            meta_rx: None,
            config,
            toast: None,
            hotkey_seek: None,
            seek_drag: None,
            volume_step: None,
            switch_presses: 0,
            volume_save: None,
            active_ctrl: None,
            active_ctrl_for: None,
            ctrl_enabled: false,
            info_open: false,
            selected: None,
            hidden,
            cursor: std::cell::Cell::new(0),
            mpv,
            search_report: None,
            search_report_fresh: false,
            loaded_playlist: None,
            nfm_open: false,
            nfm_rate: "4".to_string(),
            nfm_dir,
            nfm_threads: default_nfm_threads(),
            nfm_progress: None,
            nfm_errors: 0,
            nfm_started: None,
            nfm_elapsed: None,
            nfm_active: Vec::new(),
            nfm_cancel: None,
            nfm_rx: None,
            dbg_menu: std::cell::Cell::new(""),
            playlist_open: false,
            video_open: std::env::var_os("NOVA_TEST_VIDEO").is_some(),
            audio_open: false,
            audio_mode: "Stereo".to_string(),
            audio_channels: "auto".to_string(),
            audio_delay_ms: 0,
            audio_device: "auto".to_string(),
            audio_karaoke: false,
            audio_tempo: false,
            audio_normalize: false,
            external_audio_input: String::new(),
            flip_x: false,
            flip_y: false,
            zoom: 1.0,
            aspect_ratio_override: None,
            video_rotation: 0,
            noise_hqdn3d: false,
            noise_deband: false,
            noise_nlmeans: false,
            pan: egui::Vec2::ZERO,
            deinterlace: 0,
            video_window: None,
            remote: remote_reader(cc.egui_ctx.clone()),
            last_engine_state: PlaybackState::Stopped,
            state_started_at: Instant::now(),
            window_mode: WindowMode::Normal,
        };

        // The current playlist and the requested file are replayed by
        // `poll_init`, once the startup thread has handed the index over.
        if requested_root.is_some() {
            app.config.music_dir = app.music_dir.clone();
            app.config.save();
        }
        app
    }

    /// Apply what the startup thread finished, then the work that had to
    /// wait for it: playing a requested file, replaying the current
    /// playlist and indexing a folder the cache does not know.
    pub(super) fn poll_init(&mut self) {
        let Some(rx) = self.init_rx.take() else {
            return;
        };
        let mut ready = None;
        let mut alive = true;
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(InitMsg::Tracks(tracks)) => {
                    if self.scan_rx.is_none() {
                        let mut current = self.library.tracks().to_vec();
                        for t in tracks {
                            if !current.iter().any(|existing| existing.id == t.id || existing.path == t.path) {
                                current.push(t);
                            }
                        }
                        let root = self
                            .library
                            .root()
                            .map(|p| p.to_path_buf())
                            .or_else(|| self.music_dir.clone());
                        self.library = Library::from_tracks(root, current);
                    }
                }
                Ok(InitMsg::Playlist(path, load)) => {
                    match load {
                        Some(load) => self.apply_playlist(&path, load),
                        None => self.toast(format!("Load failed: {}", path.display())),
                    }
                }
                Ok(InitMsg::Done) => {
                    done = true;
                }
                Ok(InitMsg::Ready(payload)) => {
                    ready = Some(*payload);
                    done = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    alive = false;
                    break;
                }
            }
        }
        if !done && alive {
            self.init_rx = Some(rx); // still reading
            return;
        }
        if let Some(result) = ready {
            // A scan already run or under way owns a fresher index than the
            // cache, so it wins; a thread that died just leaves the empty list.
            if self.scan_rx.is_none() && self.library.is_empty() {
                self.library = result.library;
            }
            if let Some((path, load)) = result.playlist {
                match load {
                    Some(load) => self.apply_playlist(&path, load),
                    None => self.toast(format!("Load failed: {}", path.display())),
                }
            }
        }

        // A file was requested on the command line: its album when the
        // library knows it, the file alone — imported like a drop — when not.
        // Either way the Folder setting is left as configured.
        if let Some(path) = self.pending_play.take() {
            if self.play_path(&path) {
                self.view = View::Songs;
            } else {
                self.play_files(vec![path]);
            }
        }

        // Index the folder on first launch.
        if self.scan_rx.is_none() && self.library.is_empty() && self.music_dir.is_some() {
            if let Some(dir) = self.music_dir.clone() {
                self.start_scan(dir);
            }
        }
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    /// Request async metadata loading for tracks not yet fully loaded.
    /// Called when an item is selected/running/activated.
    pub(super) fn request_metadata_load(&mut self, paths: &[PathBuf]) {
        if self.meta_rx.is_none() {
            let (tx, rx) = mpsc::channel();
            self.meta_tx = Some(tx);
            self.meta_rx = Some(rx);
        }
        let Some(tx) = self.meta_tx.clone() else {
            return;
        };
        for path in paths {
            let tx = tx.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                match metadata::read_track(&path) {
                    Ok(track) => {
                        let _ = tx.send(MetaMsg::Loaded(track));
                    }
                    Err(err) => {
                        let _ = tx.send(MetaMsg::Failed(path, err.to_string()));
                    }
                }
            });
        }
    }

    pub(super) fn poll_meta(&mut self) {
        let mut messages = Vec::new();
        if let Some(rx) = &self.meta_rx {
            loop {
                match rx.try_recv() {
                    Ok(msg) => messages.push(msg),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
        }
        if messages.is_empty() {
            return;
        }
        let mut updated = false;
        for msg in messages {
            match msg {
                MetaMsg::Loaded(track) => {
                    // Update if track exists or add if missing
                    let mut tracks = self.library.tracks().to_vec();
                    let id = track.id;
                    if let Some(existing) = tracks.iter_mut().find(|t| t.id == id) {
                        *existing = track;
                    } else if !tracks.iter().any(|t| t.path == track.path) {
                        tracks.push(track);
                    } else {
                        continue;
                    }
                    let root = self.library.root().map(Path::to_path_buf);
                    self.library = Library::from_tracks(root, tracks);
                    updated = true;
                }
                MetaMsg::Failed(path, err) => {
                    // Keep minimal track if it exists
                    log::debug!("metadata load failed for {}: {}", path.display(), err);
                }
            }
        }
        if updated {
            self.covers.clear();
        }
    }

    fn tick_engine(&mut self) {
        self.engine.tick(&self.library);
        if let Some(error) = self.engine.take_error() {
            self.toast(error);
        }
        self.tick_ctrl();
    }
}

/// States mirrored into `/dev/shm/nova` as tiny files, so a script can watch
/// what the app is doing without screenshots. Only written on change; an
/// idle app does no I/O.
fn dbg_dump(states: &[(&str, String)]) {
    let dir = std::path::Path::new("/dev/shm/nova");
    let _ = std::fs::create_dir_all(dir);
    for (name, body) in states {
        let path = dir.join(format!("{name}.out"));
        let same = std::fs::read_to_string(&path).is_ok_and(|old| old == *body);
        if !same {
            let _ = std::fs::write(path, body);
        }
    }
}

impl eframe::App for NovaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.drain_nfm();

        if let Some((_, at)) = &self.toast {
            if at.elapsed() > TOAST_TTL {
                self.toast = None;
            }
        }

        self.poll_init();
        self.poll_scan(&ctx);
        self.poll_meta();
        self.tick_engine();

        // SMPlayer writes its whole prefs file once at exit; Nova keeps the
        // same spirit by writing the volume a beat after the last change
        // instead of on every drag frame.
        if self.volume_save.is_some_and(|when| Instant::now() >= when) {
            self.config.save();
            self.volume_save = None;
        }

        let mut actions = Vec::new();
        self.handle_input(&ctx, &mut actions);
        self.handle_drops(&ctx);

        self.remote_commands(&mut actions);

        // While a video plays, the sidebar gives up its width to the picture;
        // in full screen every bar does, and the picture gets the whole display.
        let showing_video = self.video_showing();
        let bare = self.fullscreen_video();

        if !bare {
            egui::Panel::top("top-bar").show(ui, |ui| {
                ui.add_space(4.0);
                self.top_bar(ui, &mut actions);
                ui.add_space(4.0);
            });
        }
        if !showing_video {
            egui::Panel::left("nav")
                .resizable(false)
                .default_size(220.0)
                .show(ui, |ui| {
                    self.side_bar(ui, &mut actions);
                });
        }

        if self.info_open && !bare {
            egui::Panel::right("track-info")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| {
                    self.track_info_panel(ui);
                });
        }

        if !bare {
            egui::Panel::bottom("player")
                .exact_size(104.0)
                .show(ui, |ui| {
                    self.player_bar(ui, &mut actions);
                });
        }

        if showing_video {
            let panel = if bare {
                // Edge to edge on black: no margin, no letterbox colour but black.
                egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::BLACK))
            } else {
                egui::CentralPanel::default()
            };
            panel.show(ui, |ui| {
                self.video_view(ui, frame);
            });
            // A video needs a fresh frame far more often than the audio clock ticks.
            if self.engine.state() == PlaybackState::Playing {
                ctx.request_repaint();
            }
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                self.center(ui, &mut actions);
            });
        }

        self.apply(actions);
        self.ensure_covers(&ctx);

        if self.search_report.is_some() {
            self.search_report_window(&ctx);
        }
        if self.nfm_open {
            self.nfm_window(&ctx);
        }
        if self.playlist_open {
            self.playlist_window(&ctx);
        }
        if self.audio_open {
            self.audio_window(&ctx);
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.video_open {
            self.video_window();
            // The picker is a separate process, so its clicks only reach us
            // through the pipe read above, and only inside ui(). While the
            // picture plays, the 100 ms clock below keeps ui() coming; paused
            // with no pointer over this window nothing repaints and every
            // click made in the picker waits there unread, so ask for frames
            // ourselves for as long as it is open.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        drop_overlay(&ctx);

        if self.scan_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.init_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.meta_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.nfm_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        // The "To NFM" window shows an elapsed clock while it is open.
        if self.nfm_open {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.engine.state() == PlaybackState::Playing {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            ctx.request_repaint();
        }

        let playing = match self.engine.state() {
            PlaybackState::Playing => "playing",
            PlaybackState::Paused => "paused",
            PlaybackState::Stopped => "stopped",
        };
        let state_now = self.engine.state();
        if state_now != self.last_engine_state {
            self.last_engine_state = state_now;
            self.state_started_at = Instant::now();
        }
        let now_playing = match self.engine.current_track(&self.library) {
            Some(track) => format!("{playing}: {}", track.title),
            None => playing.to_string(),
        };
        dbg_dump(&[
            (
                "playlist",
                match &self.loaded_playlist {
                    Some((name, ids)) => format!("{name}: {} track(s)", ids.len()),
                    None => "none".to_string(),
                },
            ),
            ("menu_state", self.dbg_menu.replace("").to_string()),
            (
                "toast",
                self.toast
                    .as_ref()
                    .map(|(message, _)| message.clone())
                    .unwrap_or_default(),
            ),
            ("search", self.search.clone()),
            ("library", format!("{} track(s)", self.library.len())),
            ("queue", format!("{} track(s)", self.engine.queue().len())),
            (
                "nfm",
                if let Some((done, total)) = self.nfm_progress {
                    format!("converting {done}/{total}")
                } else if self.nfm_open {
                    "open".to_string()
                } else {
                    String::new()
                },
            ),
            ("now_playing", now_playing),
            (
                "running",
                running_stamp(state_now, self.state_started_at.elapsed()),
            ),
        ]);
    }
}
