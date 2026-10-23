//! Views, actions and the messages that cross thread and process boundaries.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Songs,
    Albums,
    Artists,
    Queue,
}

impl View {
    pub(super) const ALL: [View; 4] = [View::Songs, View::Albums, View::Artists, View::Queue];
}

/// Deferred UI intent, applied after the frame has been drawn.
#[derive(Debug, Clone)]
pub(super) enum Action {
    SetView(View),
    OpenAlbum(AlbumKey),
    CloseAlbum,
    OpenArtist(String),
    CloseArtist,
    SelectPath(String),
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
    /// Save the song list as it is on screen to a playlist file.
    /// `None` keeps the current file ("as is"), `Some` names a new one.
    SavePlaylist(Option<PathBuf>),
    /// Show the playlist file's tracks in place of the whole library.
    LoadPlaylist(PathBuf),
    /// Go back from a loaded playlist to the whole library.
    UnloadPlaylist,
    /// Empty the active playlist: drop its tracks from the library and clear
    /// the playlist file and the list on screen.
    ClearPlaylist,
    /// Drop the library cache and the hidden list, then re-index the folder.
    ClearCache,
    /// Open the "Songs → To NFM" conversion window.
    OpenNfm,
}

/// Messages sent by the scanner thread.
pub(super) enum ScanMsg {
    Progress(usize),
    Done(Box<(Library, ScanReport)>),
    Failed(String),
}

/// Messages sent by the "To NFM" conversion threads. One per file, so the
/// window can show progress and count the failures, plus one per file as it
/// starts, so it can show what is running now.
pub(super) enum NfmMsg {
    Started(String),
    Progress {
        done: usize,
        total: usize,
        /// The conversion error text, when this file failed.
        error: Option<String>,
    },
}

/// One line of the stdin remote control.
///
/// It speaks the picker's own wire language, so anything the video window can
/// ask for can be asked for from a pipe, plus the transport a script needs to
/// drive playback.
#[derive(Debug)]
pub(super) enum RemoteCommand {
    Action(Action),
    Video(videowin::VideoCommand),
    /// Start this file the way the command line would.
    Play(PathBuf),
    /// Open or close the video-stream picker window.
    Picker(bool),
    /// Open or close the playlist window.
    Playlist(bool),
    /// Which of SMPlayer's *Audio → Stereo mode* choices is on the chain.
    AudioMode(String),
}
