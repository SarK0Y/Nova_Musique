//! Video metadata, read with `ffprobe`.
//!
//! Nova has no video decoder of its own: playback is mpv's job (see the app).
//! All this needs is enough to index a file and show it in a list, so it shells
//! out to the `ffprobe` binary rather than binding libavformat.
//!
//! shortcut: `ffprobe`/`ffmpeg` must be on PATH. Indexing skips a file when
//! they are missing, so nothing breaks — only the metadata is worse.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::error::{CoreError, Result};
use crate::model::{track_id, Track, VideoStream};

/// Video containers we index, lower-cased and without the dot.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "webm", "avi", "mov", "mp4", "m4v", "mpg", "mpeg", "wmv", "flv", "ts", "m2ts", "ogv",
    "3gp", "rmvb", "vob", "divx", "asf", "mxf",
];

/// Whether a path looks like a video file.
pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// Run a tool with `args` and return stdout, or `None` if it could not run.
fn tool(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new(program).args(args).output().ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

/// Index a video file: title from the title tag, duration and resolution.
pub fn read_video_track(path: &Path) -> Result<Track> {
    let probe =
        probe(path).ok_or_else(|| CoreError::metadata(path, "ffprobe could not read this file"))?;

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let duration = probe
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| *d > 0.0);

    let video = probe
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|s| s.first())
        .map(|s| VideoStream {
            width: s.get("width").and_then(|w| w.as_u64()).unwrap_or(0) as u32,
            height: s.get("height").and_then(|h| h.as_u64()).unwrap_or(0) as u32,
        })
        .unwrap_or(VideoStream {
            width: 0,
            height: 0,
        });

    Ok(Track {
        id: track_id(path),
        path: path.to_path_buf(),
        title: tag(&probe, "title").unwrap_or(stem),
        artist: tag(&probe, "artist").unwrap_or_default(),
        album_artist: String::new(),
        album: tag(&probe, "album").unwrap_or_default(),
        genre: tag(&probe, "genre"),
        year: tag(&probe, "date").and_then(|d| d.get(..4).and_then(|y| y.parse::<u32>().ok())),
        track_no: None,
        disc_no: None,
        duration,
        sample_rate: Some(48_000),
        channels: Some(2),
        // Posters are extracted on demand; the scanner never sees one.
        has_cover: false,
        // A video with no declared size still counts as one, so the player
        // routes to mpv instead of trying to decode it as audio.
        video: Some(video),
    })
}

/// A still frame from the video, for use as cover art.
pub fn poster(path: &Path) -> Option<Vec<u8>> {
    // A tenth of the way in: past the fade-in from black and any title card.
    tool(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-ss",
            "10",
            "-i",
            &path.to_string_lossy(),
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ],
    )
    .filter(|png| !png.is_empty())
}

/// Total running time, used to place the seek at a tenth of the way in.
pub fn duration_of(path: &Path) -> Option<Duration> {
    probe(path)
        .as_ref()
        .and_then(|p| {
            p.get("format")
                .and_then(|f| f.get("duration"))
                .and_then(|d| d.as_str())
                .and_then(|d| d.parse::<f64>().ok())
        })
        .filter(|d| *d > 0.0)
        .map(Duration::from_secs_f64)
}

/// `ffprobe -show_format -show_streams` as JSON, if it is readable.
fn probe(path: &Path) -> Option<serde_json::Value> {
    let path = path.to_string_lossy().into_owned();
    let out = tool(
        "ffprobe",
        &[
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            &path,
        ],
    )?;
    serde_json::from_slice(&out).ok()
}

/// A container tag, trying the case variants ffprobe uses across containers.
fn tag(probe: &serde_json::Value, key: &str) -> Option<String> {
    let tags = probe.get("format")?.get("tags")?.as_object()?;
    let key = key.to_ascii_lowercase();
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&key))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions_are_recognised_but_audio_is_not() {
        assert!(is_video(Path::new("/a/clip.MP4")));
        assert!(is_video(Path::new("/a/clip.mkv")));
        assert!(is_video(Path::new("/a/clip.webm")));
        assert!(!is_video(Path::new("/a/song.flac")));
        assert!(!is_video(Path::new("/a/noextension")));
    }

    /// Generates a clip with ffmpeg; `None` when ffmpeg is not installed.
    fn make_clip(dir: &Path, name: &str, title: Option<&str>) -> Option<std::path::PathBuf> {
        let clip = dir.join(name);
        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=24:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ]);
        if let Some(title) = title {
            cmd.arg("-metadata")
                .arg(format!("title={title}"))
                .arg("-metadata")
                .arg("artist=Nova Test");
        }
        let ok = cmd
            .arg(&clip)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        ok.then_some(clip)
    }

    #[test]
    fn ffprobe_reports_title_duration_and_resolution() {
        let dir = std::env::temp_dir().join(format!("nova-video-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let Some(clip) = make_clip(&dir, "clip.mp4", Some("Probe Me")) else {
            // shortcut: ffmpeg not installed, nothing to assert about ffprobe.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };

        let track = read_video_track(&clip).expect("video indexed");
        assert!(track.is_video());
        assert_eq!(track.title, "Probe Me");
        assert_eq!(track.artist, "Nova Test");
        assert_eq!(track.resolution().as_deref(), Some("320x240"));
        let duration = track.duration().expect("duration");
        assert!(duration > Duration::from_secs(1), "got {duration:?}");

        // An untagged clip still indexes, falling back to the file stem.
        let plain = make_clip(&dir, "plain.mkv", None).expect("untagged clip");
        let track = read_video_track(&plain).expect("untagged video indexed");
        assert_eq!(track.title, "plain");
        assert!(track.is_video());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
