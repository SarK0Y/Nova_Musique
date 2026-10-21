//! Reading audio metadata with [`lofty`].

use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Whether a path is an NFM container (magic `nfm`).
#[cfg(feature = "nofft")]
pub fn is_nfm(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|ext| nova_codec::NFM_EXTENSIONS.contains(&ext.as_str()))
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
    if crate::video::VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    #[cfg(feature = "nofft")]
    if nova_codec::CODEC_EXTENSIONS.contains(&ext.as_str())
        || nova_codec::NFM_EXTENSIONS.contains(&ext.as_str())
    {
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
    if crate::video::is_video(path) {
        return crate::video::read_video_track(path);
    }

    #[cfg(feature = "nofft")]
    if is_codec(path) {
        return read_codec_track(path);
    }

    #[cfg(feature = "nofft")]
    if is_nfm(path) {
        return read_nfm_track(path);
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
        .and_then(|t| {
            t.get_string(ItemKey::Year)
                .or_else(|| t.get_string(ItemKey::RecordingDate))
        })
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
        video: None,
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
        video: None,
    })
}

/// Builds a track from an NFM container.
///
/// NFM wraps any No_FFT codec with optional text metadata and a cover image.
/// The inner payload is decoded for stream properties; title/artist/album come
/// from the NFM header.
#[cfg(feature = "nofft")]
fn read_nfm_track(path: &Path) -> Result<Track> {
    let nfm = nova_codec::nfm_read(path).map_err(|e| CoreError::metadata(path, e))?;

    // Write the inner payload to a temp file so the codec decoder can read it.
    let tmp = std::env::temp_dir().join(format!("nova-nfm-{}.tmp", track_id(path)));
    std::fs::write(&tmp, &nfm.payload).map_err(|e| CoreError::metadata(path, e))?;

    let info = nova_codec::info(&tmp).map_err(|e| CoreError::metadata(path, e))?;
    let duration = info.duration();

    let _ = std::fs::remove_file(&tmp);

    Ok(Track {
        id: track_id(path),
        path: path.to_path_buf(),
        title: nfm
            .meta
            .title
            .clone()
            .unwrap_or_else(|| fallback_title(path)),
        artist: nfm.meta.artist.clone().unwrap_or_default(),
        album_artist: String::new(),
        album: nfm.meta.album.clone().unwrap_or_default(),
        genre: None,
        year: None,
        track_no: None,
        disc_no: None,
        duration: (duration > Duration::ZERO).then(|| duration.as_secs_f64()),
        sample_rate: (info.sample_rate > 0).then_some(info.sample_rate),
        channels: u8::try_from(info.channels).ok().filter(|&c| c > 0),
        has_cover: nfm.cover.is_some(),
        video: None,
    })
}

/// Read the first embedded picture of a file, if any.
///
/// Prefers a front cover, then falls back to any other picture.
pub fn read_cover(path: &Path) -> Result<Option<CoverArt>> {
    if crate::video::is_video(path) {
        // Videos carry no embedded picture we can read cheaply, so the poster
        // is a frame grabbed on demand — only for videos actually displayed.
        return Ok(crate::video::poster(path).map(|data| CoverArt { data }));
    }

    #[cfg(feature = "nofft")]
    if is_nfm(path) {
        let nfm = nova_codec::nfm_read(path).map_err(|e| CoreError::metadata(path, e))?;
        return Ok(nfm.cover.map(|c| CoverArt { data: c.data }));
    }

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

/// Converts one source audio file (mp3/flac/wav/...) to an `.nfm` container
/// wrapping a rate-controlled NFA5 payload.
///
/// The file is decoded in-process to PCM, written to a temporary PCM16 WAV,
/// encoded to NFA5 with `rate` bits/sample, then wrapped into `.nfm` together
/// with the source's tags and embedded cover (PNG/JPEG only). Returns the
/// written path.
#[cfg(all(feature = "audio", feature = "nofft"))]
pub fn convert_to_nfm(src: &Path, out_dir: &Path, rate: f64, bits: u16) -> Result<PathBuf> {
    use rodio::{Decoder, Source};
    use std::fs::File;

    let file = File::open(src)?;
    let decoder = Decoder::try_from(file)
        .map_err(|e| CoreError::Audio(format!("cannot decode {}: {e}", src.display())))?;
    let channels = decoder.channels().get() as u32;
    let sample_rate = decoder.sample_rate().get() as u32;
    let samples: Vec<f32> = decoder.collect();
    if samples.is_empty() {
        return Err(CoreError::Audio(format!(
            "{} decoded to no samples",
            src.display()
        )));
    }

    // Sequential conversion, so one pid-scoped temp pair is enough; both are
    // removed whatever the outcome.
    let temp = format!(".nova-convert-{}", std::process::id());
    let wav = out_dir.join(format!("{temp}.wav"));
    let nadc = out_dir.join(format!("{temp}.nadc"));

    let result = (|| {
        write_wav_pcm16(&wav, &samples, channels, sample_rate)?;
        nova_codec::encode_nadc5_rate(&wav, &nadc, bits, rate)
            .map_err(|e| CoreError::metadata(src, e))?;
        let payload = std::fs::read(&nadc)?;

        let track = read_track(src)?;
        let meta = nova_codec::NfmMeta {
            title: (!track.title.is_empty()).then_some(track.title),
            artist: (!track.artist.is_empty()).then_some(track.artist),
            album: (!track.album.is_empty()).then_some(track.album),
        };
        let cover = read_cover(src).ok().flatten().and_then(|c| {
            nfm_img_type(&c.data).map(|img_type| nova_codec::NfmCover {
                data: c.data,
                img_type,
            })
        });

        let out = out_dir.join(format!("{}.nfm", file_stem(src)));
        nova_codec::nfm_write(&out, &payload, &meta, cover.as_ref())
            .map_err(|e| CoreError::metadata(src, e))?;
        Ok(out)
    })();

    let _ = std::fs::remove_file(&wav);
    let _ = std::fs::remove_file(&nadc);
    result
}

/// Writes interleaved `f32` samples as a canonical 16-bit PCM WAV — the format
/// the codec's `wav_load` (drwav) reads back.
#[cfg(all(feature = "audio", feature = "nofft"))]
fn write_wav_pcm16(path: &Path, samples: &[f32], channels: u32, sample_rate: u32) -> Result<()> {
    use std::io::Write;

    let channels = channels.max(1);
    let data_len = u32::try_from(samples.len() * 2)
        .map_err(|_| CoreError::Audio("audio too long for a WAV".to_string()))?;
    let byte_rate = sample_rate * channels * 2;
    let block_align = (channels * 2) as u16;

    let mut file = std::fs::File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&(channels as u16).to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        file.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

/// NFM only carries PNG or JPEG covers; anything else is dropped.
#[cfg(all(feature = "audio", feature = "nofft"))]
fn nfm_img_type(data: &[u8]) -> Option<u8> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(nova_codec::NFM_IMG_PNG)
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(nova_codec::NFM_IMG_JPEG)
    } else {
        None
    }
}

/// A file name without its extension, used for the `.nfm` output name.
#[cfg(all(feature = "audio", feature = "nofft"))]
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "track".to_string())
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

    #[cfg(all(feature = "audio", feature = "nofft"))]
    #[test]
    fn converts_a_wav_to_a_rate_controlled_nfm() {
        let dir = std::env::temp_dir().join(format!("nova-nfm-convert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let wav = dir.join("source.wav");
        nova_codec::generate_wav(&wav, 0.3, 2, 44_100).expect("generate wav");

        let out = convert_to_nfm(&wav, &dir, 4.0, 6).expect("convert");
        assert_eq!(out.file_name().unwrap(), "source.nfm");

        let nfm = nova_codec::nfm_read(&out).expect("read nfm");
        assert!(nfm.payload.starts_with(b"NFA5"), "payload is NFA5");

        let track = read_track(&out).expect("read converted track");
        assert_eq!(track.sample_rate, Some(44_100));
        assert_eq!(track.channels, Some(2));

        let temp = format!(".nova-convert-{}", std::process::id());
        assert!(
            !dir.join(format!("{temp}.wav")).exists(),
            "temp wav removed"
        );
        assert!(
            !dir.join(format!("{temp}.nadc")).exists(),
            "temp nadc removed"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(is_nfm(Path::new("/a/b/Song.nfm")));
        assert!(is_supported(Path::new("/a/b/Song.NFM")));
        assert!(!is_nfm(Path::new("/a/b/Song.nadc")));
    }

    #[cfg(feature = "nofft")]
    #[test]
    fn nfm_files_report_metadata_and_cover() {
        let dir = std::env::temp_dir().join(format!("nova-nfm-meta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let wav = dir.join("signal.wav");
        nova_codec::generate_wav(&wav, 0.5, 2, 44_100).expect("generate wav");
        let inner = dir.join("song.nfa1.nadc");
        nova_codec::encode_nadc(&wav, &inner, 6, nova_codec::NadcVariant::Nfa1).expect("encode");
        std::fs::remove_file(&wav).expect("remove wav");

        let payload = std::fs::read(&inner).expect("read payload");
        let nfm = dir.join("wrapped.nfm");
        let meta = nova_codec::NfmMeta {
            title: Some("Wrapped Title".to_string()),
            artist: Some("Wrapped Artist".to_string()),
            album: Some("Wrapped Album".to_string()),
        };
        let png = {
            let mut p = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            p.extend_from_slice(b"payload");
            p
        };
        let cover = nova_codec::NfmCover {
            data: png.clone(),
            img_type: nova_codec::NFM_IMG_PNG,
        };
        nova_codec::nfm_write(&nfm, &payload, &meta, Some(&cover)).expect("nfm_write");

        let track = read_track(&nfm).expect("read_track");
        assert_eq!(track.title, "Wrapped Title");
        assert_eq!(track.artist, "Wrapped Artist");
        assert_eq!(track.album, "Wrapped Album");
        assert_eq!(track.sample_rate, Some(44_100));
        assert_eq!(track.channels, Some(2));
        assert!(track.has_cover);

        let read = read_cover(&nfm).expect("read_cover").expect("cover");
        assert_eq!(read.data, png);

        let _ = std::fs::remove_dir_all(&dir);
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
