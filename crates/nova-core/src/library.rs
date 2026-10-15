//! In-memory music library: scanning, grouping and search.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::error::{CoreError, Result};
use crate::metadata::{self, is_supported};
use crate::model::{Album, AlbumKey, Artist, Track, TrackId};

/// Version of the on-disk cache format.
const CACHE_VERSION: u32 = 1;

/// Progress notification emitted while scanning.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Number of audio files indexed so far.
    pub indexed: usize,
    /// File that was just processed.
    pub path: PathBuf,
}

/// Summary of a completed scan.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    /// Audio files discovered on disk.
    pub found: usize,
    /// Files successfully indexed.
    pub indexed: usize,
    /// Files that failed to parse (kept in `errors`).
    pub failed: usize,
    /// Human readable messages for the failures.
    pub errors: Vec<String>,
    /// Wall-clock time spent scanning.
    pub elapsed: Duration,
}

/// A scanned collection of tracks, with album/artist views built alongside.
#[derive(Debug, Clone, Default)]
pub struct Library {
    root: Option<PathBuf>,
    tracks: Vec<Track>,
    index_by_id: HashMap<TrackId, usize>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    root: Option<PathBuf>,
    tracks: Vec<Track>,
}

impl Library {
    /// Creates an empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a library from an already-known set of tracks (used by the cache).
    pub fn from_tracks(root: Option<PathBuf>, tracks: Vec<Track>) -> Self {
        let mut library = Library {
            root,
            tracks,
            ..Default::default()
        };
        library.reindex();
        library
    }

    /// Root directory that was scanned, if any.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// All tracks, ordered by album then disc/track number.
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Looks a track up by id.
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.index_by_id.get(&id).map(|&i| &self.tracks[i])
    }

    /// Number of indexed tracks.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether the library holds no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Album views, sorted by album artist then title.
    pub fn albums(&self) -> &[Album] {
        &self.albums
    }

    /// Artist views, sorted by name.
    pub fn artists(&self) -> &[Artist] {
        &self.artists
    }

    /// Tracks belonging to an album, in disc/track order.
    pub fn album_tracks(&self, key: &AlbumKey) -> Vec<&Track> {
        self.albums
            .iter()
            .find(|a| &a.key == key)
            .map(|a| {
                a.track_ids
                    .iter()
                    .filter_map(|&id| self.track(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Tracks credited to an artist, in library order.
    pub fn artist_tracks(&self, artist: &str) -> Vec<&Track> {
        self.tracks
            .iter()
            .filter(|t| t.display_artist() == artist || t.album_artist == artist)
            .collect()
    }

    /// Track ids of every track, in library order.
    pub fn all_track_ids(&self) -> Vec<TrackId> {
        self.tracks.iter().map(|t| t.id).collect()
    }

    /// Case-insensitive substring search over title, artist and album.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Track> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        self.tracks
            .iter()
            .filter(|t| {
                t.title.to_lowercase().contains(&needle)
                    || t.artist.to_lowercase().contains(&needle)
                    || t.album.to_lowercase().contains(&needle)
                    || t.album_artist.to_lowercase().contains(&needle)
            })
            .take(limit)
            .collect()
    }

    /// Scans `root` recursively, reporting progress as files are indexed.
    pub fn scan(
        root: impl AsRef<Path>,
        on_progress: &mut dyn FnMut(ScanProgress),
    ) -> Result<(Self, ScanReport)> {
        let root = root.as_ref();
        let started = Instant::now();
        let mut report = ScanReport::default();
        let mut tracks = Vec::new();

        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_hidden(e.path()));

        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    report.failed += 1;
                    report.errors.push(err.to_string());
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_supported(path) {
                continue;
            }

            report.found += 1;
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            match metadata::read_track(&canonical) {
                Ok(track) => {
                    tracks.push(track);
                    report.indexed += 1;
                    on_progress(ScanProgress {
                        indexed: report.indexed,
                        path: canonical,
                    });
                }
                Err(err) => {
                    report.failed += 1;
                    if report.errors.len() < 100 {
                        report.errors.push(err.to_string());
                    }
                    log::warn!("skipping {}: {err}", path.display());
                }
            }
        }

        report.elapsed = started.elapsed();
        let mut library = Library::from_tracks(Some(root.to_path_buf()), tracks);
        library.deduplicate();
        Ok((library, report))
    }

    /// Loads a cached library from a JSON file.
    pub fn load_cache(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let cache: CacheFile = serde_json::from_slice(&bytes)?;
        if cache.version != CACHE_VERSION {
            return Err(CoreError::Cache(serde_json::Error::io(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "cache version {} is not supported (expected {CACHE_VERSION})",
                        cache.version
                    ),
                ),
            )));
        }
        Ok(Library::from_tracks(cache.root, cache.tracks))
    }

    /// Writes the library to a JSON cache file, creating parent directories.
    pub fn save_cache(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let cache = CacheFile {
            version: CACHE_VERSION,
            root: self.root.clone(),
            tracks: self.tracks.clone(),
        };
        let json = serde_json::to_vec_pretty(&cache)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    fn deduplicate(&mut self) {
        let mut seen = HashSet::new();
        self.tracks.retain(|t| seen.insert(t.id));
        self.reindex();
    }

    fn reindex(&mut self) {
        self.tracks.sort_by(|a, b| {
            let key = |t: &Track| {
                (
                    t.album_artist.to_lowercase(),
                    t.album.to_lowercase(),
                    t.disc_no.unwrap_or(u32::MAX),
                    t.track_no.unwrap_or(u32::MAX),
                    t.title.to_lowercase(),
                )
            };
            key(a).cmp(&key(b))
        });

        self.index_by_id = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, i))
            .collect();

        // Albums, grouped in library order.
        let mut albums: Vec<Album> = Vec::new();
        let mut album_index: HashMap<AlbumKey, usize> = HashMap::new();
        for track in &self.tracks {
            let key = track.album_key();
            let slot = *album_index.entry(key.clone()).or_insert_with(|| {
                albums.push(Album {
                    key: key.clone(),
                    title: track.display_album().to_string(),
                    album_artist: key.album_artist.clone(),
                    year: None,
                    track_ids: Vec::new(),
                });
                albums.len() - 1
            });
            let album = &mut albums[slot];
            album.track_ids.push(track.id);
            if album.year.is_none() {
                album.year = track.year;
            }
        }

        // Artists, with track and album counts.
        let mut stats: HashMap<String, (usize, HashSet<AlbumKey>)> = HashMap::new();
        for track in &self.tracks {
            let entry = stats.entry(track.display_artist().to_string()).or_default();
            entry.0 += 1;
            entry.1.insert(track.album_key());
        }
        let mut artists: Vec<Artist> = stats
            .into_iter()
            .map(|(name, (track_count, albums))| Artist {
                name,
                track_count,
                album_count: albums.len(),
            })
            .collect();
        artists.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        self.albums = albums;
        self.artists = artists;
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.') && n != ".")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a small library without touching the file system.
    fn track(id: TrackId, title: &str, artist: &str, album: &str, no: Option<u32>) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("/music/{id}.flac")),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: artist.to_string(),
            album: album.to_string(),
            genre: None,
            year: Some(2000),
            track_no: no,
            disc_no: None,
            duration: Some(180.0),
            sample_rate: Some(44_100),
            channels: Some(2),
            has_cover: false,
        }
    }

    #[test]
    fn groups_albums_and_artists_and_sorts_by_track_number() {
        let tracks = vec![
            track(1, "Second", "A", "Record", Some(2)),
            track(2, "First", "A", "Record", Some(1)),
            track(3, "Other", "B", "Solo", Some(1)),
        ];
        let lib = Library::from_tracks(None, tracks);

        assert_eq!(lib.len(), 3);
        assert_eq!(lib.albums().len(), 2);
        assert_eq!(lib.artists().len(), 2);

        let album_a = lib
            .albums()
            .iter()
            .find(|a| a.album_artist == "A")
            .expect("album A");
        // Track numbers drive the order, not insertion order.
        assert_eq!(album_a.track_ids, vec![2, 1]);
    }

    #[test]
    fn search_matches_title_artist_and_album() {
        let lib = Library::from_tracks(
            None,
            vec![
                track(1, "Blue Sky", "Radiohead", "Kid A", Some(1)),
                track(2, "Green", "Portishead", "Dummy", Some(1)),
            ],
        );
        assert_eq!(lib.search("blue", 10).len(), 1);
        assert_eq!(lib.search("portishead", 10).len(), 1);
        assert_eq!(lib.search("dummy", 10).len(), 1);
        assert_eq!(lib.search("", 10).len(), 0);
    }

    #[test]
    fn search_respects_limit() {
        let tracks: Vec<Track> = (0..20)
            .map(|i| track(i, "Same Title", "Same Artist", "Same Album", None))
            .collect();
        let lib = Library::from_tracks(None, tracks);
        assert_eq!(lib.search("same", 5).len(), 5);
    }
}
