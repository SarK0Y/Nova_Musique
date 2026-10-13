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

/// Whether a path looks like a file the vendored No_FFT codecs can decode.
///
/// Detection is by extension here; the decoder itself works from the file's
/// magic bytes, so a mislabelled file still plays once indexed.
#[cfg(feature = "nofft")]
pub fn is_codec(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|ext| nova_codec::CODEC_EXTENSIONS.contains(&ext.as_str()))
}

/// Whether a path looks like an audio file we can index.
pub fn is_supported(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    #[cfg(feature = "nofft")]
    if nova_codec::CODEC_EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    false
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
    #[cfg(feature = "nofft")]
    if is_codec(path) {
        return read_codec_track(path);
    }

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

/// Builds a track from a No_FFT codec header.
///
/// These containers carry no tags, so the title falls back to the file stem and
/// the artist is left empty, exactly as for an untagged MP3. Stream properties
/// come from the codec header rather than a tag reader.
#[cfg(feature = "nofft")]
fn read_codec_track(path: &Path) -> Result<Track> {
    let info = nova_codec::info(path).map_err(|e| CoreError::metadata(path, e))?;
    let duration = info.duration();
    Ok(Track {
        id: track_id(path),
        path: path.to_path_buf(),
        title: fallback_title(path),
        artist: String::new(),
        album_artist: String::new(),
        album: String::new(),
        genre: None,
        year: None,
        track_no: None,
        disc_no: None,
        duration: (duration > std::time::Duration::ZERO).then(|| duration.as_secs_f64()),
        sample_rate: (info.sample_rate > 0).then_some(info.sample_rate),
        channels: u8::try_from(info.channels).ok().filter(|&c| c > 0),
        has_cover: false,
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

    #[cfg(feature = "nofft")]
    #[test]
    fn codec_extensions_are_indexed_but_not_confused_with_audio() {
        assert!(is_supported(Path::new("/a/b/Song.no_fft")));
        assert!(is_supported(Path::new("/a/b/Song.NADC")));
        assert!(is_codec(Path::new("/a/b/Song.poly.no_fft")));
        assert!(is_codec(Path::new("/a/b/Song.nfa2.nadc")));
        assert!(!is_codec(Path::new("/a/b/Song.mp3")));
    }

    #[cfg(feature = "nofft")]
    #[test]
    fn codec_files_report_stream_properties_from_their_header() {
        let dir = std::env::temp_dir().join(format!("nova-codec-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Encode a known signal, then drop the intermediate WAV so only the two
        // codec files remain to be scanned.
        let wav = dir.join("signal.wav");
        nova_codec::generate_wav(&wav, 0.5, 2, 44_100).expect("generate wav");
        nova_codec::encode_nofft(&wav, &dir.join("song.poly.no_fft"), 256, 8).expect("encode poly");
        nova_codec::encode_nadc(
            &wav,
            &dir.join("song.nfa1.nadc"),
            6,
            nova_codec::NadcVariant::Nfa1,
        )
        .expect("encode nadc");
        std::fs::remove_file(&wav).expect("remove wav");

        let (library, report) = crate::Library::scan(&dir, &mut |_| {}).expect("scan");
        assert_eq!(report.indexed, 2, "both codec files are indexed");
        assert_eq!(report.failed, 0);

        for track in library.tracks() {
            assert_eq!(track.sample_rate, Some(44_100), "{}", track.title);
            assert_eq!(track.channels, Some(2), "{}", track.title);
            let duration = track.duration.expect("header carries a duration");
            assert!(
                (duration - 0.5).abs() < 0.02,
                "{}: expected ~0.5s, got {duration}s",
                track.title
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
