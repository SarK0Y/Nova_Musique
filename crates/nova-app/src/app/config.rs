//! Persisted configuration: the JSON config file, the XDG paths, and `allow_video_movement`.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Config {
    #[serde(default)]
    pub(super) music_dir: Option<PathBuf>,
    #[serde(default = "default_volume")]
    pub(super) volume: f32,
    #[serde(default = "default_allow_video_movement")]
    pub(super) allow_video_movement: bool,
    /// Playlists the user opened or saved, newest first — the Load menu.
    #[serde(default)]
    pub(super) playlists: Vec<PathBuf>,
    /// The playlist `Save → as is` writes and `Load` last opened. Kept in the
    /// config so the same file stays current across restarts.
    #[serde(default)]
    pub(super) current_playlist: Option<PathBuf>,
    /// Folder `Songs → To NFM` writes converted files to.
    #[serde(default)]
    pub(super) path_to_save_converted_files: Option<PathBuf>,
}

pub(super) fn default_volume() -> f32 {
    0.8
}

pub(super) fn default_allow_video_movement() -> bool {
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
            playlists: Vec::new(),
            current_playlist: None,
            path_to_save_converted_files: None,
        }
    }
}

impl Config {
    pub(super) fn load() -> Self {
        config_file()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(super) fn save(&self) {
        let Some(path) = config_file() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

pub(super) fn xdg_dir(env: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(env)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
}

pub(super) fn config_file() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("nova").join("config.json"))
}

pub(super) fn cache_file() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache").map(|dir| dir.join("nova").join("library.json"))
}

/// Tracks the user hid from the library, kept beside the library cache so both
/// live in the same place. Edit or delete this file to bring songs back.
pub(super) fn hidden_file() -> Option<PathBuf> {
    cache_file().map(|path| path.with_file_name("hidden.json"))
}

/// Where `Save → as is` writes before `as…` has picked a file: one playlist
/// beside the config, like the hidden-tracks list beside the cache.
pub(super) fn default_playlist_file() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config").map(|dir| dir.join("nova").join("playlist.m3u8"))
}
