//! The egui application: library browser plus transport bar.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, FontId, Layout, RichText};
use serde::{Deserialize, Serialize};

use crate::mpv::{Mpv, VideoBackend};
use crate::videowin;
use nova_core::metadata;
use nova_core::{AlbumKey, Library, PlaybackEngine, PlaybackState, ScanReport, Track, TrackId};

/// How long a transient message stays on screen.
const TOAST_TTL: Duration = Duration::from_secs(6);
/// How many cover images to decode per frame, to avoid stalling the UI.
const COVER_BUDGET_PER_FRAME: usize = 6;
/// Height of one track row in the list views.
const ROW_HEIGHT: f32 = 24.0;

// ---------------------------------------------------------------------------
// Persisted configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    music_dir: Option<PathBuf>,
    #[serde(default = "default_volume")]
    volume: f32,
    #[serde(default = "default_allow_video_movement")]
    allow_video_movement: bool,
}

fn default_volume() -> f32 {
    0.8
}

fn default_allow_video_movement() -> bool {
    true
}

/// SMPlayer's `allow_video_movement`, held as a process-wide variable.
///
/// A field on `NovaApp` would be rebuilt on every launch, so the checkbox always
/// came back on. Behind a global that is seeded from [`Config`] on startup and
/// written back when it changes, it outlives the window and the child process
/// that asks about it — a setting, not a bit of UI state.
static ALLOW_VIDEO_MOVEMENT: RwLock<bool> = RwLock::new(true);

pub fn allow_video_movement() -> bool {
    *ALLOW_VIDEO_MOVEMENT
        .read()
        .unwrap_or_else(|err| err.into_inner())
}

pub fn set_allow_video_movement(allowed: bool) {
    *ALLOW_VIDEO_MOVEMENT
        .write()
        .unwrap_or_else(|err| err.into_inner()) = allowed;
    let mut config = Config::load();
    config.allow_video_movement = allowed;
    config.save();
}

impl Default for Config {
    fn default() -> Self {
        Config {
            music_dir: None,
            volume: default_volume(),
            allow_video_movement: default_allow_video_movement(),
        }
    }
}

impl Config {
    fn load() -> Self {
        config_file()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = config_file() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

fn xdg_dir(env: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
}

fn config_file() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("nova").join("config.json"))
}

fn cache_file() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache").map(|dir| dir.join("nova").join("library.json"))
}

/// Tracks the user hid from the library, kept beside the library cache so both
/// live in the same place. Edit or delete this file to bring songs back.
fn hidden_file() -> Option<PathBuf> {
    cache_file().map(|path| path.with_file_name("hidden.json"))
}

// ---------------------------------------------------------------------------
// Views and actions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Songs,
    Albums,
    Artists,
    Queue,
}

impl View {
    const ALL: [View; 4] = [View::Songs, View::Albums, View::Artists, View::Queue];
}

/// Deferred UI intent, applied after the frame has been drawn.
#[derive(Debug, Clone)]
enum Action {
    SetView(View),
    OpenAlbum(AlbumKey),
    CloseAlbum,
    OpenArtist(String),
    CloseArtist,
    SelectPath(String),
    HideAll,
    PlayList(Vec<TrackId>, usize),
    PlayQueueIndex(usize),
    Enqueue(TrackId),
    RemoveFromQueue(TrackId),
    ClearQueue,
    Toggle,
    Next,
    Prev,
    Seek(f32),
    SetVolume(f32),
    ToggleShuffle,
    CycleRepeat,
}

/// Messages sent by the scanner thread.
enum ScanMsg {
    Progress(usize),
    Done(Box<(Library, ScanReport)>),
    Failed(String),
}

// ---------------------------------------------------------------------------
// Cover cache
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CoverCache {
    entries: HashMap<TrackId, Option<egui::TextureHandle>>,
}

impl CoverCache {
    fn get(&self, id: TrackId) -> Option<&egui::TextureHandle> {
        self.entries.get(&id).and_then(Option::as_ref)
    }

    fn contains(&self, id: TrackId) -> bool {
        self.entries.contains_key(&id)
    }

    fn load(&mut self, ctx: &egui::Context, id: TrackId, path: &Path) {
        if self.entries.contains_key(&id) {
            return;
        }
        let texture = metadata::read_cover(path)
            .ok()
            .flatten()
            .and_then(|cover| decode_texture(ctx, format!("cover-{id}"), &cover.data));
        self.entries.insert(id, texture);
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

fn decode_texture(ctx: &egui::Context, name: String, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &image.into_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

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
    /// Set when the app was launched pointing at a specific file, to be played
    /// as soon as its folder has been indexed.
    pending_play: Option<PathBuf>,
    scan_rx: Option<Receiver<ScanMsg>>,
    scan_status: Option<String>,
    config: Config,
    toast: Option<(String, Instant)>,
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
    /// Floating playlist window, opened from the Nova menu in the top bar.
    playlist_open: bool,
    /// External video window: picks which video stream mpv should show.
    video_open: bool,
    /// SMPlayer's *Mirror image* (`hflip`) and *Flip image* (`vflip`), asked of
    /// mpv as video filters rather than drawn flipped here.
    flip_x: bool,
    flip_y: bool,
    /// SMPlayer's *Zoom*: how far past the size that fits the picture is drawn.
    /// It scales the video widget and re-centres it, and nothing reaches mpv;
    /// `video_view` does the same by drawing the image that much larger.
    zoom: f32,
    /// SMPlayer's *Move left/right/up/down*: how far the picture is nudged off
    /// centre, in points, with +y running down like every other coordinate.
    /// The buttons in the video window nudge it; dragging the picture does the
    /// same, which is the only way to move it that feels direct.
    pan: egui::Vec2,
    /// That window's process, started on demand and killed when dropped.
    video_window: Option<videowin::VideoWindow>,
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
        *ALLOW_VIDEO_MOVEMENT
            .write()
            .unwrap_or_else(|err| err.into_inner()) = config.allow_video_movement;

        // An optional command-line target: a file, a folder, or a `file://` URL.
        // Pointing at a file scans its folder and starts playing that track.
        let mut pending_play = None;
        let mut requested_root = None;
        if let Some(arg) = std::env::args_os().nth(1) {
            let target = parse_target(&arg.to_string_lossy());
            if target.is_file() {
                pending_play = Some(target.clone());
                requested_root = target.parent().map(Path::to_path_buf);
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
        let hidden = load_hidden();
        let mut library = load_cached_library(music_dir.as_deref()).unwrap_or_default();
        library.hide(&hidden);
        let mpv = Mpv::new(cc).map(Arc::new);
        let mut engine = PlaybackEngine::with_backend(Box::new(VideoBackend::new(mpv.clone())));
        engine.set_volume(config.volume);

        let folder_input = music_dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default();

        let mut app = NovaApp {
            library,
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
            config,
            toast: None,
            info_open: false,
            selected: None,
            hidden,
            cursor: std::cell::Cell::new(0),
            mpv,
            playlist_open: false,
            video_open: std::env::var_os("NOVA_TEST_VIDEO").is_some(),
            flip_x: false,
            flip_y: false,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            video_window: None,
        };

        // If a file was requested and the cache already knows it, play right away.
        if let Some(path) = app.pending_play.clone() {
            if app.play_path(&path) {
                app.pending_play = None;
                app.view = View::Songs;
            }
        }

        if requested_root.is_some() {
            app.config.music_dir = app.music_dir.clone();
            app.config.save();
        }

        // Otherwise index the folder (first launch, or to reach the requested file).
        if app.pending_play.is_some() || (app.library.is_empty() && app.music_dir.is_some()) {
            if let Some(dir) = app.music_dir.clone() {
                app.start_scan(dir);
            }
        }
        app
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    /// Locates a track in the library by file path (exact first, then
    /// canonicalised so symlinks and redundant components still match).
    fn find_track_by_path(&self, path: &Path) -> Option<TrackId> {
        if let Some(track) = self.library.tracks().iter().find(|t| t.path == path) {
            return Some(track.id);
        }
        let target = std::fs::canonicalize(path).ok()?;
        self.library.tracks().iter().find_map(|track| {
            let candidate = std::fs::canonicalize(&track.path).ok()?;
            (candidate == target).then_some(track.id)
        })
    }

    /// Replaces the queue with the file's album (falling back to just the file)
    /// and starts playback. Returns `false` if the file is not in the library.
    fn play_path(&mut self, path: &Path) -> bool {
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

    // -- drag and drop ------------------------------------------------------

    /// Reads files dropped onto the window and acts on them.
    fn handle_drops(&mut self, ctx: &egui::Context) {
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
    fn accept_drop(&mut self, paths: Vec<PathBuf>) {
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
    fn enqueue_dropped_files(&mut self, paths: Vec<PathBuf>) {
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

        let missing: Vec<PathBuf> = files
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
                    Err(err) => self.toast(format!("Could not read {}: {err}", path.display())),
                }
            }
            if added > 0 {
                let root = self.library.root().map(Path::to_path_buf);
                self.library = Library::from_tracks(root, tracks);
            }
        }

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
    fn play_files(&mut self, paths: Vec<PathBuf>) {
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

    // -- scanning -----------------------------------------------------------

    /// Acts on whatever path the folder selector was given.
    ///
    /// A folder is indexed as before. A file is not a request to walk its parent —
    /// pointing the selector at one track should add that track and leave the rest of
    /// the folder alone, which is the difference between adding a video and
    /// indexing whatever else happens to sit beside it.
    fn select_path(&mut self, path: PathBuf) {
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
    fn add_file(&mut self, path: PathBuf) {
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

    fn start_scan(&mut self, dir: PathBuf) {
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

    fn poll_scan(&mut self, ctx: &egui::Context) {
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

    // -- per-frame maintenance ---------------------------------------------

    fn tick_engine(&mut self) {
        self.engine.tick(&self.library);
        if let Some(error) = self.engine.take_error() {
            self.toast(error);
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (toggle, next, prev, info, del, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowRight),
                i.modifiers.command && i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::I),
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::Escape),
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
        if escape && self.video_showing() {
            self.engine.stop();
        }
    }

    /// <kbd>Del</kbd>. In Songs the highlighted track is hidden from the
    /// library; anywhere else it is dropped from the queue.
    fn delete_current(&mut self) {
        let Some(track) = self.engine.current_track(&self.library) else {
            return;
        };
        let id = track.id;

        if self.view == View::Songs {
            self.hidden.insert(id);
            save_hidden(&self.hidden);
            self.library.hide(&self.hidden);
        } else {
            self.engine.queue_mut().remove(id);
        }
        // Either way the file we were playing is gone from where it just was.
        self.engine.stop();
    }

    /// Representative track used to render an album's cover.
    fn album_cover_source(&self, key: &AlbumKey) -> Option<TrackId> {
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

    fn ensure_covers(&mut self, ctx: &egui::Context) {
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

    // -- actions ------------------------------------------------------------

    fn apply(&mut self, actions: Vec<Action>) {
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
                Action::HideAll => {
                    self.hidden
                        .extend(self.library.tracks().iter().map(|t| t.id));
                    save_hidden(&self.hidden);
                    self.library.hide(&self.hidden);
                    self.engine.stop();
                }
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
            }
        }
    }

    // -- panels -------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.menu_button(RichText::new("Nova").strong().size(18.0), |ui| {
                if ui.button("Playlist").clicked() {
                    self.playlist_open = true;
                    ui.close();
                }
                if ui.button("Video").clicked() {
                    self.video_open = true;
                    ui.close();
                }
                ui.separator();
                // Not wired up yet, so shown disabled rather than pretending.
                ui.add_enabled(false, egui::Label::new("Audio"));
            });
            ui.separator();

            ui.label("Folder");
            let folder_field = ui.add(
                egui::TextEdit::singleline(&mut self.folder_input)
                    .desired_width(300.0)
                    .hint_text("/home/you/Music"),
            );
            // Enter does what the Scan button does, so a typed path needs no
            // mouse. A one-line field drops focus on Enter, so `has_focus` is
            // already false by the time the frame closes; `lost_focus` plus the
            // key is how egui itself spells this.
            let entered =
                folder_field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if entered || ui.button("Scan").clicked() {
                actions.push(Action::SelectPath(self.folder_input.trim().to_string()));
            }
            if ui.button("Browse…").clicked() {
                let mut dialog = rfd::FileDialog::new();
                if let Some(dir) = self.music_dir.as_deref() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.folder_input = dir.display().to_string();
                    actions.push(Action::SelectPath(dir.display().to_string()));
                }
            }

            ui.separator();
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(220.0)
                    .hint_text("title, artist, album"),
            );
            if ui.button("✕").on_hover_text("Clear search").clicked() {
                self.search.clear();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .selectable_label(self.info_open, "ℹ Info")
                    .on_hover_text("Track info panel (I)")
                    .clicked()
                {
                    self.info_open = !self.info_open;
                }
                ui.separator();
                if let Some((message, _)) = &self.toast {
                    ui.label(RichText::new(message).color(Color32::from_rgb(255, 200, 120)));
                } else if let Some(status) = &self.scan_status {
                    ui.label(RichText::new(status).weak());
                } else if !self.library.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "{} tracks · {} albums",
                            self.library.len(),
                            self.library.albums().len()
                        ))
                        .weak(),
                    );
                }
            });
        });
    }

    fn side_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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
    fn info_track(&self) -> Option<&Track> {
        self.engine
            .current_track(&self.library)
            .or_else(|| self.selected.and_then(|id| self.library.track(id)))
    }

    /// Right-hand panel with the embedded art plus the text fields and the
    /// technical details of the current track.
    fn track_info_panel(&self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("TRACK INFO").weak().size(11.0));
        ui.add_space(6.0);

        let Some(track) = self.info_track() else {
            ui.label(RichText::new("Nothing selected.").weak());
            return;
        };

        track_info_body(ui, track, self.covers.get(track.id));
    }

    fn center(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let query = self.search.trim().to_lowercase();
        if !query.is_empty() {
            let results = self.library.search(&query, 500);
            ui.heading(format!("Search · {} result(s)", results.len()));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.track_list(ui, &results, actions);
            });
            return;
        }

        match self.view {
            View::Songs => {
                ui.horizontal(|ui| {
                    ui.heading("Songs");
                    if ui.button("Clear all").clicked() {
                        actions.push(Action::HideAll);
                    }
                });
                ui.label(RichText::new("Click a track to play it.").weak().size(12.0));
                ui.add_space(4.0);
                let tracks: Vec<&Track> = self.library.tracks().iter().collect();
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

    /// Renders a clickable list of tracks. Clicking one replaces the queue with
    /// this very list, starting at the clicked track.
    fn track_list(&self, ui: &mut egui::Ui, tracks: &[&Track], actions: &mut Vec<Action>) {
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

            if index == cursor && !ui.clip_rect().contains_rect(response.rect) {
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

    fn album_grid(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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

    fn album_detail(&self, ui: &mut egui::Ui, key: &AlbumKey, actions: &mut Vec<Action>) {
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

    fn artist_list(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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

    fn artist_detail(&self, ui: &mut egui::Ui, name: &str, actions: &mut Vec<Action>) {
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

    fn queue_view(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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
                if index == cursor && !ui.clip_rect().contains_rect(response.rect) {
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

    fn player_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let track = self.engine.current_track(&self.library);
        let position = self.engine.position();
        let total = self.engine.duration(&self.library);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // Cover thumbnail.
            cover_thumb(ui, track.and_then(|t| self.covers.get(t.id)), 72.0);

            ui.vertical(|ui| {
                ui.add_space(4.0);
                match track {
                    Some(track) => {
                        ui.label(
                            RichText::new(truncate(&track.title, 46))
                                .strong()
                                .size(16.0),
                        );
                        ui.label(RichText::new(track.display_artist()).weak());
                    }
                    None => {
                        ui.label(RichText::new("Nothing playing").weak());
                    }
                }
            });

            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("⏮").on_hover_text("Previous").clicked() {
                        actions.push(Action::Prev);
                    }
                    let play_label = if self.engine.state() == PlaybackState::Playing {
                        "⏸"
                    } else {
                        "▶"
                    };
                    if ui
                        .button(play_label)
                        .on_hover_text("Play / pause (space)")
                        .clicked()
                    {
                        actions.push(Action::Toggle);
                    }
                    if ui.button("⏭").on_hover_text("Next").clicked() {
                        actions.push(Action::Next);
                    }
                    if ui
                        .selectable_label(self.engine.queue().shuffle(), "🔀")
                        .on_hover_text("Shuffle")
                        .clicked()
                    {
                        actions.push(Action::ToggleShuffle);
                    }
                    if ui
                        .selectable_label(
                            self.engine.queue().repeat() != nova_core::RepeatMode::Off,
                            "🔁",
                        )
                        .on_hover_text(self.engine.queue().repeat().label())
                        .clicked()
                    {
                        actions.push(Action::CycleRepeat);
                    }
                });

                let seconds_total = total.map(|d| d.as_secs_f32()).unwrap_or(0.0).max(0.001);
                let mut seconds_position = position.as_secs_f32().min(seconds_total);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format_clock(position)).size(12.0).weak());
                    let slider = egui::Slider::new(&mut seconds_position, 0.0..=seconds_total)
                        .show_value(false);
                    if ui
                        .add_sized([ui.available_width() - 52.0, 18.0], slider)
                        .changed()
                    {
                        actions.push(Action::Seek(seconds_position));
                    }
                    ui.label(
                        RichText::new(format_clock(total.unwrap_or_default()))
                            .size(12.0)
                            .weak(),
                    );
                });
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut volume = self.engine.volume();
                if ui
                    .add_sized(
                        [120.0, 18.0],
                        egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false),
                    )
                    .changed()
                {
                    actions.push(Action::SetVolume(volume));
                }
                ui.label(RichText::new("🔈").weak());
            });
        });
        ui.add_space(6.0);
    }
    /// Whether the video surface should take over the content area.
    ///
    /// True only while a video is actually loaded: `current_track` keeps
    /// answering with the queue's selection after a stop, so the state is what
    /// decides, or the library would never come back.
    fn video_showing(&self) -> bool {
        self.mpv.is_some()
            && self.engine.state() != PlaybackState::Stopped
            && self
                .engine
                .current_track(&self.library)
                .is_some_and(Track::is_video)
    }

    /// Aspect ratio of the video on screen, from its declared frame size.
    fn video_aspect(&self) -> f32 {
        self.engine
            .current_track(&self.library)
            .and_then(|track| track.video)
            .filter(|v| v.width > 0 && v.height > 0)
            .map_or(16.0 / 9.0, |v| v.width as f32 / v.height as f32)
    }

    /// Draws the video, centred in whatever space the panel offers.
    ///
    /// The sidebar is hidden while this runs, so the surface spans the full
    /// width under the top bar.
    fn video_view(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let Some(mpv) = self.mpv.clone() else {
            return;
        };
        let aspect = self.video_aspect();
        let panel = ui.max_rect();
        // Zoom scales the picture past the panel, which clips it — the same
        // effect SMPlayer gets by resizing its video widget and re-centring it.
        let size = crate::mpv::fit(panel.size(), aspect) * self.zoom;

        // How far the picture's centre may travel. SMPlayer gates this on
        // `allow_video_movement`: with it off, the picture can only be moved
        // while it reaches past the window, and with it on it can be pushed
        // around freely. The picture never leaves the panel either way, so it
        // cannot be lost.
        let limit = if allow_video_movement() {
            panel.size() / 2.0
        } else {
            egui::vec2(
                (size.x - panel.width()).max(0.0) / 2.0,
                (size.y - panel.height()).max(0.0) / 2.0,
            )
        };
        let pan = egui::vec2(
            self.pan.x.clamp(-limit.x, limit.x),
            self.pan.y.clamp(-limit.y, limit.y),
        );
        let rect = egui::Rect::from_center_size(panel.center() + pan, size);

        // Dragging the picture moves it with the cursor, the same way moving the
        // video widget by hand does in SMPlayer. It answers to the checkbox:
        // unchecked, the picture stays where the buttons left it and a drag
        // writes nothing, so there is no way past the block by hand.
        if allow_video_movement() {
            let drag = ui.interact(rect, ui.id().with("video-pan"), egui::Sense::drag());
            if drag.dragged() {
                self.pan += drag.drag_delta();
            }
        }

        // Render at the on-screen pixel size so the frame is decoded and scaled
        // exactly once, at the size it is displayed.
        let pixels = size * ui.ctx().pixels_per_point();
        match mpv.texture(frame, pixels, aspect) {
            Some(texture) => {
                let sized = egui::load::SizedTexture::new(texture, size);
                egui::Image::new(sized).paint_at(ui, rect);
            }
            None => {
                ui.colored_label(Color32::RED, "video could not be started");
            }
        }
    }

    /// Render the playlist in its own native OS viewport, separate from the main window.
    fn playlist_window(&mut self, ctx: &egui::Context) {
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

    /// Keeps the external video-stream picker in step with mpv.
    ///
    /// This is what SMPlayer calls *Video → Track*: a container carrying more
    /// than one video stream (multi-angle discs, dual-cam footage) lets you
    /// pick which one mpv shows. The picker runs as its own process, so the
    /// picture stays here and nothing about mpv's GL context is disturbed.
    fn video_window(&mut self) {
        if !self.video_open {
            return;
        }
        if self.video_window.is_none() {
            self.video_window = videowin::VideoWindow::spawn();
            if self.video_window.is_none() {
                // Spawning failed; leave the flag off so this is not retried on
                // every frame. Opening it again from the menu tries afresh.
                self.video_open = false;
                return;
            }
        }
        let Some(window) = self.video_window.as_mut() else {
            return;
        };

        // Closing the picker window, or its process dying, closes it here too.
        if window.closed() {
            log::debug!("the video window closed");
            self.video_window = None;
            self.video_open = false;
            return;
        }

        if let Some(mpv) = self.mpv.clone() {
            window.send_tracks(&mpv.video_tracks());
        }
        window.send_state(
            self.flip_x,
            self.flip_y,
            self.zoom,
            self.pan,
            allow_video_movement(),
        );
        let commands = window.commands();

        // The window's borrow ends with its last use above, so the app can be
        // changed here.
        for command in commands {
            match command {
                videowin::VideoCommand::Pick(id) => {
                    log::info!("video stream {id} selected");
                    if let Some(mpv) = self.mpv.clone() {
                        mpv.select_video_track(&id);
                    }
                }
                videowin::VideoCommand::Zoom(zoom) => self.zoom = zoom,
                videowin::VideoCommand::Pan(x, y) => self.pan = egui::vec2(x, y),
                videowin::VideoCommand::MoveAllowed(allowed) => set_allow_video_movement(allowed),
                videowin::VideoCommand::Flip { x, y } => {
                    self.flip_x = x;
                    self.flip_y = y;
                    if let Some(mpv) = self.mpv.clone() {
                        mpv.set_video_filter("hflip", x);
                        mpv.set_video_filter("vflip", y);
                    }
                }
            }
        }
    }
}

impl eframe::App for NovaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if let Some((_, at)) = &self.toast {
            if at.elapsed() > TOAST_TTL {
                self.toast = None;
            }
        }

        self.poll_scan(&ctx);
        self.tick_engine();
        self.handle_input(&ctx);
        self.handle_drops(&ctx);

        let mut actions = Vec::new();

        egui::Panel::top("top-bar").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui, &mut actions);
            ui.add_space(4.0);
        });
        // While a video plays, the sidebar gives up its width to the picture.
        let showing_video = self.video_showing();
        if !showing_video {
            egui::Panel::left("nav")
                .resizable(false)
                .default_size(220.0)
                .show(ui, |ui| {
                    self.side_bar(ui, &mut actions);
                });
        }

        if self.info_open {
            egui::Panel::right("track-info")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| {
                    self.track_info_panel(ui);
                });
        }

        egui::Panel::bottom("player")
            .exact_size(104.0)
            .show(ui, |ui| {
                self.player_bar(ui, &mut actions);
            });

        if showing_video {
            egui::CentralPanel::default().show(ui, |ui| {
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

        if self.playlist_open {
            self.playlist_window(&ctx);
        }
        if self.video_open {
            self.video_window();
        }

        drop_overlay(&ctx);

        if self.scan_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if self.engine.state() == PlaybackState::Playing {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            ctx.request_repaint();
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_cached_library(expected_root: Option<&Path>) -> Option<Library> {
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
fn load_hidden() -> HashSet<TrackId> {
    hidden_file()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_hidden(hidden: &HashSet<TrackId>) {
    let Some(path) = hidden_file() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(hidden) {
        let _ = std::fs::write(path, json);
    }
}

fn format_clock(duration: Duration) -> String {
    let total = duration.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn format_duration(duration: Option<Duration>) -> String {
    duration
        .map(format_clock)
        .unwrap_or_else(|| "--:--".to_string())
}

/// Renders the cover, the primary text fields and the technical details of
/// `track` inside the info panel. Split out from [`NovaApp::track_info_panel`]
/// so it can be exercised without a full application.
fn track_info_body(ui: &mut egui::Ui, track: &Track, cover: Option<&egui::TextureHandle>) {
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
fn track_info_rows(track: &Track) -> Vec<(&'static str, String)> {
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
fn track_row_text(track: &Track, playing: bool) -> String {
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
fn info_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).weak().size(12.0));
    ui.label(RichText::new(value).size(12.0));
    ui.end_row();
}

/// Short, human-readable label for the container/codec behind a file path.
fn format_label(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if nova_core::video::VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        return format!("{} video", ext.to_ascii_uppercase());
    }
    match ext.as_str() {
        "nfm" => "NFM container".to_string(),
        "no_fft" => "NoFFT".to_string(),
        "nadc" => "NADC adaptive".to_string(),
        "mp3" => "MP3".to_string(),
        "flac" => "FLAC".to_string(),
        "ogg" | "oga" => "Ogg".to_string(),
        "opus" => "Opus".to_string(),
        "wav" => "WAV".to_string(),
        "m4a" | "mp4" | "aac" => "AAC / MPEG-4".to_string(),
        "wv" => "WavPack".to_string(),
        "aiff" | "aif" => "AIFF".to_string(),
        "" => "Unknown".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

fn format_sample_rate(hz: u32) -> String {
    if hz >= 1000 {
        format!("{:.1} kHz", hz as f64 / 1000.0)
    } else {
        format!("{hz} Hz")
    }
}

fn format_channels(count: u8) -> String {
    match count {
        1 => "1 (mono)".to_string(),
        2 => "2 (stereo)".to_string(),
        other => format!("{other} ch"),
    }
}

/// Draws a square cover (or a music-note placeholder) in a single allocation,
/// so the widget id stays stable whether or not art has loaded yet.
fn cover_thumb(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, size: f32) {
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

/// Overlay shown while files are being dragged over the window.
fn drop_overlay(ctx: &egui::Context) {
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
fn classify_drop(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>, usize) {
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

fn truncate(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        text.to_string()
    } else {
        let mut out: String = chars[..width.saturating_sub(1)].iter().collect();
        out.push('…');
        out
    }
}

/// Accepts either a plain filesystem path or a `file://` URL and turns it into
/// a path. For URLs the authority component (`file://host/path`) is dropped and
/// `%XX` escapes are decoded; plain paths are used verbatim.
fn parse_target(argument: &str) -> PathBuf {
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
fn percent_decode(input: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_are_used_verbatim() {
        assert_eq!(parse_target("/music/a.mp3"), PathBuf::from("/music/a.mp3"));
        // A relative path must keep its first segment.
        assert_eq!(
            parse_target("Album/track.mp3"),
            PathBuf::from("Album/track.mp3")
        );
        // Literal percent signs in a real path are not treated as escapes.
        assert_eq!(
            parse_target("/music/100%25.mp3"),
            PathBuf::from("/music/100%25.mp3")
        );
    }

    #[test]
    fn file_urls_strip_the_scheme() {
        assert_eq!(
            parse_target("file:///music/a.mp3"),
            PathBuf::from("/music/a.mp3")
        );
        // Spaces may appear literally (as in a shell-quoted URL).
        assert_eq!(
            parse_target("file:///music/10. What's It Gonna Be.mp3"),
            PathBuf::from("/music/10. What's It Gonna Be.mp3")
        );
        // ...or percent-encoded.
        assert_eq!(
            parse_target("file:///music/a%20b.mp3"),
            PathBuf::from("/music/a b.mp3")
        );
        // A host/authority component is dropped.
        assert_eq!(
            parse_target("file://localhost/music/a.mp3"),
            PathBuf::from("/music/a.mp3")
        );
    }

    #[test]
    fn percent_decode_handles_malformed_input() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
    }

    #[test]
    fn classify_drop_separates_folders_audio_and_junk() {
        let base = std::env::temp_dir().join(format!("nova-drop-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();

        let song = base.join("song.mp3");
        std::fs::write(&song, b"not really audio").unwrap();
        std::fs::write(base.join("cover.jpg"), b"x").unwrap();
        std::fs::write(base.join("notes.txt"), b"x").unwrap();
        let missing = base.join("missing.flac");

        let (dirs, files, skipped) = classify_drop(vec![
            base.join("sub"),
            song.clone(),
            base.join("cover.jpg"),
            base.join("notes.txt"),
            missing.clone(),
        ]);

        assert_eq!(dirs, vec![base.join("sub")]);
        // Audio is matched by extension, whether or not the file exists yet.
        assert_eq!(files, vec![song, missing]);
        assert_eq!(skipped, 2);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_playing_row_shows_a_marker_instead_of_its_number() {
        let mut track = sample_track("/music/a.flac");
        track.track_no = Some(7);

        let numbered = track_row_text(&track, false);
        assert!(numbered.starts_with("  7   "), "got {numbered:?}");
        assert!(numbered.contains("Test NFM Track"));
        assert!(numbered.contains("Test Artist"));

        // Playing: the marker takes the number column so the cursor stays the
        // only filled row.
        let playing = track_row_text(&track, true);
        assert!(playing.starts_with("  \u{25b6}   "), "got {playing:?}");

        // No track number and not playing: the middle dot.
        track.track_no = None;
        assert!(track_row_text(&track, false).starts_with("  \u{b7}   "));
    }

    #[test]
    fn format_labels_name_known_containers() {
        assert_eq!(format_label(Path::new("/a/b.nfm")), "NFM container");
        assert_eq!(format_label(Path::new("/a/b.no_fft")), "NoFFT");
        assert_eq!(format_label(Path::new("/a/b.nadc")), "NADC adaptive");
        assert_eq!(format_label(Path::new("/a/b.flac")), "FLAC");
        assert_eq!(format_label(Path::new("/a/b.MP3")), "MP3");
        // Unknown extensions are upper-cased; missing ones are Unknown.
        assert_eq!(format_label(Path::new("/a/b.xyz")), "XYZ");
        assert_eq!(format_label(Path::new("/a/b")), "Unknown");
    }

    #[test]
    fn sample_rates_and_channels_are_human_readable() {
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(48_000), "48.0 kHz");
        assert_eq!(format_sample_rate(8_000), "8.0 kHz");
        assert_eq!(format_sample_rate(320), "320 Hz");

        assert_eq!(format_channels(1), "1 (mono)");
        assert_eq!(format_channels(2), "2 (stereo)");
        assert_eq!(format_channels(6), "6 ch");
    }

    fn sample_track(path: &str) -> Track {
        Track {
            id: 1,
            path: PathBuf::from(path),
            title: "Test NFM Track".to_string(),
            artist: "Test Artist".to_string(),
            album_artist: String::new(),
            album: "Test Album".to_string(),
            genre: None,
            year: None,
            track_no: None,
            disc_no: None,
            duration: Some(236.0),
            sample_rate: Some(44_100),
            channels: Some(2),
            has_cover: true,
            video: None,
        }
    }

    /// Draws the info panel headlessly. This cannot assert on content, but it
    /// proves the grid, scroll area and label wrapping all lay out cleanly.
    fn layout_info_panel(track: &Track) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                track_info_body(ui, track, None);
            });
        });
    }

    fn row<'a>(rows: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        rows.iter()
            .find(|(label, _)| *label == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn info_rows_describe_an_nfm_track() {
        let rows = track_info_rows(&sample_track("/music/test.nfm"));

        // The container is named, and the stream properties are those of the
        // inner payload that gets decoded for playback.
        assert_eq!(row(&rows, "Format"), Some("NFM container"));
        assert_eq!(row(&rows, "Duration"), Some("03:56"));
        assert_eq!(row(&rows, "Sample rate"), Some("44.1 kHz"));
        assert_eq!(row(&rows, "Channels"), Some("2 (stereo)"));
        assert_eq!(row(&rows, "Embedded art"), Some("Yes"));

        layout_info_panel(&sample_track("/music/test.nfm"));
    }

    #[test]
    fn info_rows_omit_fields_the_file_does_not_carry() {
        let rows = track_info_rows(&sample_track("/music/tst.nfm"));

        assert_eq!(row(&rows, "Format"), Some("NFM container"));
        // This container has no genre, year or numbering of any kind.
        assert_eq!(row(&rows, "Genre"), None);
        assert_eq!(row(&rows, "Year"), None);
        assert_eq!(row(&rows, "Track"), None);
        assert_eq!(row(&rows, "Disc"), None);
    }

    #[test]
    fn info_rows_report_full_tagging_when_present() {
        let mut track = sample_track("/music/tagged.flac");
        track.album_artist = "Album Artist".to_string();
        track.genre = Some("Ambient".to_string());
        track.year = Some(2024);
        track.track_no = Some(3);
        track.disc_no = Some(2);
        track.has_cover = false;

        let rows = track_info_rows(&track);

        assert_eq!(row(&rows, "Format"), Some("FLAC"));
        assert_eq!(row(&rows, "Album artist"), Some("Album Artist"));
        assert_eq!(row(&rows, "Genre"), Some("Ambient"));
        assert_eq!(row(&rows, "Year"), Some("2024"));
        assert_eq!(row(&rows, "Track"), Some("Disc 2 · Track 3"));
        assert_eq!(row(&rows, "Embedded art"), Some("No"));
    }

    #[test]
    fn info_panel_falls_back_when_text_fields_are_missing() {
        let mut track = sample_track("/music/tst.nfm");
        track.title = "tst".to_string();
        track.artist = String::new();
        track.album = String::new();

        // The headline above the grid uses the never-empty grouping helpers.
        assert_eq!(track.display_artist(), "Unknown Artist");
        assert_eq!(track.display_album(), "Unknown Album");

        layout_info_panel(&track);
    }
}
