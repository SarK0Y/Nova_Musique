//! End-to-end tests over a real directory tree.
//!
//! The WAV files are synthesized here so the suite has no external tooling
//! dependencies (no ffmpeg, no fixture downloads).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nova_core::{Library, NullBackend, PlaybackEngine, PlaybackState};

/// Writes a mono 16-bit PCM WAV containing a quiet square-ish tone.
fn write_wav(path: &Path, seconds: f32, sample_rate: u32, frequency: f32) -> std::io::Result<()> {
    use std::io::Write;

    let frames = (seconds * sample_rate as f32) as u32;
    let mut data = Vec::with_capacity(frames as usize * 2);
    for n in 0..frames {
        let t = n as f32 / sample_rate as f32;
        let value = if (t * frequency) % 1.0 < 0.5 { 0.2 } else { -0.2 };
        let sample = (value * i16::MAX as f32) as i16;
        data.extend_from_slice(&sample.to_le_bytes());
    }

    let byte_rate = sample_rate * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);

    let mut file = std::fs::File::create(path)?;
    file.write_all(&out)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nova-test-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scans_a_directory_tree_and_groups_it() {
    let dir = TempDir::new("scan");
    std::fs::create_dir_all(dir.path().join("Band/Nested")).unwrap();
    write_wav(&dir.path().join("Band/one.wav"), 1.0, 8_000, 220.0).unwrap();
    write_wav(&dir.path().join("Band/Nested/two.wav"), 2.0, 8_000, 330.0).unwrap();
    // A non-audio file must be ignored.
    std::fs::write(dir.path().join("Band/notes.txt"), b"not audio").unwrap();

    let (library, report) = Library::scan(dir.path(), &mut |_| {}).expect("scan works");

    assert_eq!(report.found, 2, "only audio files are discovered");
    assert_eq!(report.indexed, 2);
    assert_eq!(report.failed, 0);
    assert_eq!(library.len(), 2);

    // Untagged files fall back to their file name.
    let titles: Vec<&str> = library.tracks().iter().map(|t| t.title.as_str()).collect();
    assert!(titles.contains(&"one"));
    assert!(titles.contains(&"two"));

    // Duration comes from the container, not from tags.
    let one = library.tracks().iter().find(|t| t.title == "one").unwrap();
    let seconds = one.duration.expect("duration is known");
    assert!(
        (seconds - 1.0).abs() < 0.05,
        "expected ~1s, got {seconds}s"
    );

    assert_eq!(library.albums().len(), 1);
    assert_eq!(library.artists().len(), 1);
}

#[test]
fn cache_round_trips() {
    let dir = TempDir::new("cache");
    write_wav(&dir.path().join("a.wav"), 0.5, 8_000, 440.0).unwrap();

    let (library, _) = Library::scan(dir.path(), &mut |_| {}).unwrap();
    let cache = dir.path().join("library.json");
    library.save_cache(&cache).expect("save cache");

    let restored = Library::load_cache(&cache).expect("load cache");
    assert_eq!(restored.len(), library.len());
    assert_eq!(restored.tracks(), library.tracks());
}

#[test]
fn plays_through_a_scanned_library_headlessly() {
    let dir = TempDir::new("play");
    for (i, name) in ["a.wav", "b.wav", "c.wav"].iter().enumerate() {
        write_wav(&dir.path().join(name), 0.5, 8_000, 200.0 + i as f32 * 40.0).unwrap();
    }

    let (library, _) = Library::scan(dir.path(), &mut |_| {}).unwrap();
    let mut engine = PlaybackEngine::with_backend(Box::new(NullBackend::with_track_duration(
        Duration::from_millis(15),
    )));
    engine
        .set_queue(&library, library.all_track_ids(), 0)
        .expect("queue starts");
    assert_eq!(engine.state(), PlaybackState::Playing);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && engine.state() != PlaybackState::Stopped {
        engine.tick(&library);
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(engine.state(), PlaybackState::Stopped);
    assert_eq!(engine.queue().current_item_index(), Some(library.len() - 1));
}
