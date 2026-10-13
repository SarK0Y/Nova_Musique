//! Reading audio metadata with [`lofty`].

use std::path::Path;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey};

use crate::error::{CoreError, Result};
use crate::model::{track_id, Track};

/// File extensions Nova will try to index, lower-cased and without the dot.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "wav", "wave", "m4a", "m4b", "mp4", "aac", "aiff", "aif",
    "aifc", "wv", "ape", "mpc", "mka", "mkv", "alac", "caf",
];

/// Whether a path looks like an audio file we can index.
pub fn is_supported(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext = ext.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&ext.as_str())
        }
        None => false,
    }
}

/// Cover art extracted from a file's tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverArt {
    /// Raw image bytes exactly as stored in the tag (may be PNG/JPEG/...).
    pub data: Vec<u8>,
}

/// Read tags and stream properties for a single file.
///
/// Missing tags never fail: the title falls back to the file stem and artists
/// fall back to `"Unknown Artist"`.
pub fn read_track(path: &Path) -> Result<Track> {
    let tagged = lofty::read_from_path(path).map_err(|e| CoreError::metadata(path, e))?;

    let props = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let title = tag
        .and_then(|t| t.title())
        .map(|t| t.into_owned())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| fallback_title(path));

    let artist = tag
        .and_then(|t| t.artist())
        .map(|a| a.into_owned())
        .unwrap_or_default();

    let album_artist = tag
        .and_then(|t| t.get_string(ItemKey::AlbumArtist))
        .map(str::to_owned)
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| artist.clone());

    let album = tag
        .and_then(|t| t.album())
        .map(|a| a.into_owned())
        .unwrap_or_default();

    let genre = tag
        .and_then(|t| t.genre())
        .map(|g| g.into_owned())
        .filter(|g| !g.trim().is_empty());

    let year = tag
        .and_then(|t| t.get_string(ItemKey::Year).or_else(|| t.get_string(ItemKey::RecordingDate)))
        .and_then(parse_year);

    let duration = match props.duration().as_secs_f64() {
        secs if secs > 0.0 => Some(secs),
        _ => None,
    };

    Ok(Track {
        id: track_id(path),
        path: path.to_path_buf(),
        title,
        artist,
        album_artist,
        album,
        genre,
        year,
        track_no: tag.and_then(|t| t.track()),
        disc_no: tag.and_then(|t| t.disk()),
        duration,
        sample_rate: props.sample_rate(),
        channels: props.channels(),
        has_cover: tag.map(|t| t.picture_count() > 0).unwrap_or(false),
    })
}

/// Read the first embedded picture of a file, if any.
///
/// Prefers a front cover, then falls back to any other picture.
pub fn read_cover(path: &Path) -> Result<Option<CoverArt>> {
    let tagged = lofty::read_from_path(path).map_err(|e| CoreError::metadata(path, e))?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };

    let picture = tag
        .get_picture_type(lofty::picture::PictureType::CoverFront)
        .or_else(|| tag.pictures().first());

    Ok(picture.map(|p| CoverArt {
        data: p.data().to_vec(),
    }))
}

fn fallback_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Unknown Title".to_string())
}

/// Extract a year from strings like `1999`, `1999-04-01` or `"1999/2000"`.
fn parse_year(raw: &str) -> Option<u32> {
    let digits: String = raw
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .take(4)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_parsing_handles_common_shapes() {
        assert_eq!(parse_year("1999"), Some(1999));
        assert_eq!(parse_year("1999-04-01"), Some(1999));
        assert_eq!(parse_year("rec 2007"), Some(2007));
        assert_eq!(parse_year("nope"), None);
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_supported(Path::new("/a/b/Track.MP3")));
        assert!(is_supported(Path::new("/a/b/track.flac")));
        assert!(!is_supported(Path::new("/a/b/cover.jpg")));
        assert!(!is_supported(Path::new("/a/b/README")));
    }
}
