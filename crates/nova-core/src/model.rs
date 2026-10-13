//! Plain data types describing a music library.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Stable identifier for a track, derived from its canonical path.
pub type TrackId = u64;

/// A single audio file with the metadata we care about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Stable id derived from [`Track::path`].
    pub id: TrackId,
    /// Absolute path of the file on disk.
    pub path: PathBuf,
    /// Track title (falls back to the file name when the tag is missing).
    pub title: String,
    /// Performing artist, empty when unknown.
    pub artist: String,
    /// Album artist, defaults to [`Track::artist`] when the tag is absent.
    pub album_artist: String,
    /// Album title, empty when unknown.
    pub album: String,
    /// Genre, when tagged.
    pub genre: Option<String>,
    /// Release year, when tagged.
    pub year: Option<u32>,
    /// 1-based track number inside its disc.
    pub track_no: Option<u32>,
    /// 1-based disc number.
    pub disc_no: Option<u32>,
    /// Duration in seconds, when the container reported it.
    pub duration: Option<f64>,
    /// Sample rate in Hz, when known.
    pub sample_rate: Option<u32>,
    /// Channel count, when known.
    pub channels: Option<u8>,
    /// Whether the file embeds cover art.
    pub has_cover: bool,
}

impl Track {
    /// Artist used for grouping, never empty.
    pub fn display_artist(&self) -> &str {
        if !self.artist.is_empty() {
            &self.artist
        } else if !self.album_artist.is_empty() {
            &self.album_artist
        } else {
            "Unknown Artist"
        }
    }

    /// Album used for grouping, never empty.
    pub fn display_album(&self) -> &str {
        if self.album.is_empty() {
            "Unknown Album"
        } else {
            &self.album
        }
    }

    /// Duration as a [`Duration`], when known.
    pub fn duration(&self) -> Option<Duration> {
        self.duration.map(Duration::from_secs_f64)
    }

    /// Key used to group tracks into albums.
    pub fn album_key(&self) -> AlbumKey {
        let album_artist = if !self.album_artist.is_empty() {
            self.album_artist.clone()
        } else if !self.artist.is_empty() {
            self.artist.clone()
        } else {
            "Unknown Artist".to_string()
        };
        AlbumKey {
            album_artist,
            album: self.display_album().to_string(),
        }
    }
}

/// Identity of an album: album artist plus album title.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AlbumKey {
    /// Album artist (or track artist as a fallback).
    pub album_artist: String,
    /// Album title.
    pub album: String,
}

impl AlbumKey {
    /// A single-line representation, handy for UI labels and caches.
    pub fn display(&self) -> String {
        format!("{} — {}", self.album_artist, self.album)
    }
}

/// An album, i.e. a set of tracks sharing an [`AlbumKey`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    /// Grouping key.
    pub key: AlbumKey,
    /// Album title.
    pub title: String,
    /// Album artist.
    pub album_artist: String,
    /// Release year, taken from the first track that has one.
    pub year: Option<u32>,
    /// Track ids, already ordered by disc and track number.
    pub track_ids: Vec<TrackId>,
}

/// An artist with aggregate counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artist {
    /// Artist name.
    pub name: String,
    /// Number of tracks credited to this artist.
    pub track_count: usize,
    /// Number of distinct albums this artist appears on.
    pub album_count: usize,
}

/// Stable id for a path. FNV-1a over the raw OS bytes: cheap and reproducible
/// between runs, which matters because the library cache is persisted.
pub fn track_id(path: &Path) -> TrackId {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_id_is_stable_for_equal_paths() {
        let a = track_id(Path::new("/music/a.flac"));
        let b = track_id(Path::new("/music/a.flac"));
        let c = track_id(Path::new("/music/b.flac"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn album_key_prefers_album_artist() {
        let mut track = Track {
            id: 1,
            path: PathBuf::from("/x.mp3"),
            title: "t".into(),
            artist: "Performer".into(),
            album_artist: "Various Artists".into(),
            album: "Comp".into(),
            genre: None,
            year: None,
            track_no: None,
            disc_no: None,
            duration: None,
            sample_rate: None,
            channels: None,
            has_cover: false,
        };
        assert_eq!(track.album_key().album_artist, "Various Artists");
        track.album_artist.clear();
        assert_eq!(track.album_key().album_artist, "Performer");
    }
}
