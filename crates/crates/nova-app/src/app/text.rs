//! Small formatting helpers for clocks, durations, file formats and labels.

use super::*;

/// A short duration for the conversion window: `12s`, `1m03s`, `1h02m`.
pub(super) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60)
    }
}

pub(super) fn format_clock(duration: Duration) -> String {
    let total = duration.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// Contents of the `running` dump file: the unix epoch (seconds) when the
/// current engine state began, while playing and within 20 s of that; empty
/// otherwise, so a script can tell a live playback from a stale one.
pub(super) fn running_stamp(state: PlaybackState, since: Duration) -> String {
    const STALE: Duration = Duration::from_secs(20);
    if state == PlaybackState::Playing && since < STALE {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or_else(|_| "0".into(), |d| d.as_secs().to_string())
    } else {
        String::new()
    }
}

pub(super) fn format_duration(duration: Option<Duration>) -> String {
    duration
        .map(format_clock)
        .unwrap_or_else(|| "--:--".to_string())
}

/// Short, human-readable label for the container/codec behind a file path.
pub(super) fn format_label(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if nova_core357_11::video::VIDEO_EXTENSIONS.contains(&ext.as_str()) {
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

pub(super) fn format_sample_rate(hz: u32) -> String {
    if hz >= 1000 {
        format!("{:.1} kHz", hz as f64 / 1000.0)
    } else {
        format!("{hz} Hz")
    }
}

pub(super) fn format_channels(count: u8) -> String {
    match count {
        1 => "1 (mono)".to_string(),
        2 => "2 (stereo)".to_string(),
        other => format!("{other} ch"),
    }
}

/// A playlist entry's label: its file name, or the whole path if it has none.
pub(super) fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

pub(super) fn truncate(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        text.to_string()
    } else {
        let mut out: String = chars[..width.saturating_sub(1)].iter().collect();
        out.push('…');
        out
    }
}
