//! Unit tests for the app's helpers and command parsing.

use super::*;

#[test]
fn elapsed_reads_as_seconds_minutes_hours() {
    assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
    assert_eq!(format_elapsed(Duration::from_secs(59)), "59s");
    assert_eq!(format_elapsed(Duration::from_secs(63)), "1m03s");
    assert_eq!(format_elapsed(Duration::from_secs(3720)), "1h02m");
}

#[test]
fn running_stamp_covers_all_states() {
    use std::time::Duration;
    // Playing within 20 s: a timestamp string is returned.
    let s = running_stamp(PlaybackState::Playing, Duration::from_secs(5));
    assert!(!s.is_empty() && s.parse::<u64>().is_ok());
    // Playing 20 s or more: empty (stale).
    assert_eq!(
        running_stamp(PlaybackState::Playing, Duration::from_secs(20)),
        ""
    );
    // Paused or Stopped: always empty, regardless of age.
    assert_eq!(running_stamp(PlaybackState::Paused, Duration::ZERO), "");
    assert_eq!(running_stamp(PlaybackState::Stopped, Duration::ZERO), "");
}

#[test]
fn default_thread_count_is_one_or_more() {
    assert!(default_nfm_threads().parse::<usize>().unwrap() >= 1);
}

#[test]
fn c_key_pauses_on_shift_and_leaves_alone_ctrl() {
    // Plain C: pause and minimise.
    assert_eq!(NovaApp::c_key(true, false, false, false), (true, true));
    // Shift+C: minimise, keep playing.
    assert_eq!(NovaApp::c_key(true, true, false, false), (false, true));
    // Ctrl+C / Alt+C belong to the desktop, so nothing happens.
    assert_eq!(NovaApp::c_key(true, false, true, false), (false, false));
    assert_eq!(NovaApp::c_key(true, false, false, true), (false, false));
    // No C at all: nothing.
    assert_eq!(NovaApp::c_key(false, true, false, false), (false, false));
}

#[test]
fn hotkeys_step_the_seek_bar_by_five_seconds() {
    assert_eq!(step_seek(10.0, true), 15.0);
    assert_eq!(step_seek(10.0, false), 5.0);
    // Never rewind before the start of the track.
    assert_eq!(step_seek(3.0, false), 0.0);
    assert_eq!(step_seek(0.0, false), 0.0);
}

#[test]
fn hotkeys_step_the_volume_by_one_percent() {
    assert!((step_volume(0.5, true) - 0.51).abs() < f32::EPSILON);
    assert!((step_volume(0.5, false) - 0.49).abs() < f32::EPSILON);
    // Clamped to the slider's ends.
    assert_eq!(step_volume(0.0, false), 0.0);
    assert_eq!(step_volume(1.0, true), 1.0);
    // Repeated presses land on whole percents, not float drift.
    let mut volume = 0.0;
    for _ in 0..50 {
        volume = step_volume(volume, true);
    }
    assert!((volume - 0.5).abs() < 1e-6, "drifted to {volume}");
}

#[test]
fn hotkey_seek_freezes_at_press_and_sets_once_on_release() {
    // The hotkey run is modelled exactly as SMPlayer's press/release:
    // the first press freezes the counter at the playback position, every
    // later press only re-aims the target from that start, and the stream
    // is set to the target exactly once, on release.
    let position = 100.0;
    let mut hotkey_seek: Option<(f32, f32)> = None;

    // First press of the run: freeze the counter and aim one step forward.
    let fwd = true;
    hotkey_seek = Some((position, step_seek(position, fwd)));
    assert_eq!(hotkey_seek, Some((100.0, 105.0)));

    // Further forward presses re-aim from the same frozen start, so the
    // target stays one step ahead instead of stacking — the run settles.
    if let Some((start, _)) = hotkey_seek {
        hotkey_seek = Some((start, step_seek(start, fwd)));
    }
    assert_eq!(hotkey_seek, Some((100.0, 105.0)));

    // The release: set the stream to the target once, only if it moved.
    let mut actions: Vec<f32> = Vec::new();
    if let Some((start, end)) = hotkey_seek.take() {
        if end != start {
            actions.push(end);
        }
    }
    assert_eq!(actions, vec![105.0]);
    assert!(hotkey_seek.is_none());

    // A fresh backward run from the same frozen position.
    let fwd = false;
    hotkey_seek = Some((position, step_seek(position, fwd)));
    assert_eq!(hotkey_seek, Some((100.0, 95.0)));
    let mut actions = Vec::new();
    if let Some((start, end)) = hotkey_seek.take() {
        if end != start {
            actions.push(end);
        }
    }
    assert_eq!(actions, vec![95.0]);
}

#[test]
fn seek_drag_holds_the_handle_and_seeks_once_on_release() {
    // The press grabs the handle at the playback position; nothing goes
    // out while it is held.
    let (drag, seek) = seek_drag(None, 10.0, 30.0, true, true, false);
    assert_eq!(seek, None);
    assert_eq!(drag, Some((10.0, 30.0)));
    // Frames while held keep the dragged value, still no seek.
    let (drag, seek) = seek_drag(drag, 30.0, 45.0, false, true, false);
    assert_eq!(seek, None);
    assert_eq!(drag, Some((10.0, 45.0)));
    // The release seeks exactly once, to where the handle landed.
    let (drag, seek) = seek_drag(drag, 45.0, 45.0, false, false, true);
    assert_eq!(drag, None);
    assert_eq!(seek, Some(45.0));
}

#[test]
fn seek_drag_ignores_a_click_that_did_not_move() {
    // Pressed and released on the spot playback already shows: no seek —
    // the spurious same-position seek SMPlayer's `slider_has_moved` avoids.
    let (drag, seek) = seek_drag(None, 10.0, 10.0, true, false, false);
    assert_eq!((drag, seek), (Some((10.0, 10.0)), None));
    let (drag, seek) = seek_drag(drag, 10.0, 10.0, false, false, true);
    assert_eq!((drag, seek), (None, None));
    // A value change with no pointer holding the slider (arrow key on a
    // focused slider) seeks straight away.
    assert_eq!(
        seek_drag(None, 10.0, 12.0, false, true, false),
        (None, Some(12.0))
    );
}

#[test]
fn a_remote_line_becomes_a_command() {
    use videowin::VideoCommand;
    // The picker's own wire language travels unchanged.
    assert!(matches!(
        remote_command("deint 2"),
        Some(RemoteCommand::Video(VideoCommand::Deinterlace(2)))
    ));
    assert!(matches!(
        remote_command("flip 1 0"),
        Some(RemoteCommand::Video(VideoCommand::Flip {
            x: true,
            y: false
        }))
    ));
    assert!(matches!(
        remote_command("  zoom 1.5  "),
        Some(RemoteCommand::Video(VideoCommand::Zoom(zoom))) if zoom == 1.5
    ));
    // Transport sits on top of it.
    assert!(matches!(
        remote_command("toggle"),
        Some(RemoteCommand::Action(Action::Toggle))
    ));
    assert!(matches!(
        remote_command("seek 900"),
        Some(RemoteCommand::Action(Action::Seek(seconds))) if seconds == 900.0
    ));
    assert!(matches!(
        remote_command("volume 0.5"),
        Some(RemoteCommand::Action(Action::SetVolume(volume))) if volume == 0.5
    ));
    // Volume stays inside the slider's range, so nothing outside what the
    // UI can set is ever written to the config.
    assert!(matches!(
        remote_command("volume 9"),
        Some(RemoteCommand::Action(Action::SetVolume(volume))) if volume == 1.0
    ));
    assert!(matches!(
        remote_command("play /tmp/a.mp3"),
        Some(RemoteCommand::Play(path)) if path == PathBuf::from("/tmp/a.mp3")
    ));
    assert!(matches!(
        remote_command("picker 1"),
        Some(RemoteCommand::Picker(true))
    ));
    assert!(matches!(
        remote_command("  picker 0"),
        Some(RemoteCommand::Picker(false))
    ));
    assert!(matches!(
        remote_command("playlist 1"),
        Some(RemoteCommand::Playlist(true))
    ));
    assert!(matches!(
        remote_command("mode reverse"),
        Some(RemoteCommand::AudioMode(mode)) if mode == "reverse"
    ));
    assert!(matches!(
        remote_command("  mode Left channel  "),
        Some(RemoteCommand::AudioMode(mode)) if mode == "Left channel"
    ));
    // Lines that mean nothing are ignored, not treated as errors.
    assert!(remote_command("pause").is_none());
    assert!(remote_command("mode").is_none());
    assert!(remote_command("seek now").is_none());
    assert!(remote_command("play").is_none());
    assert!(remote_command("play ").is_none());
    assert!(remote_command("").is_none());
}

#[test]
fn plain_paths_are_used_verbatim() {
    assert_eq!(parse_target("/music/a.mp3"), PathBuf::from("/music/a.mp3"));
    // A relative path must keep its first segment.
    assert_eq!(
        parse_target("Album/track.mp3"),
        PathBuf::from("Album/track.mp3")
    );
    // Literal percent signs in a real path are not treated as escapes.
    assert_eq!(
        parse_target("/music/100%25.mp3"),
        PathBuf::from("/music/100%25.mp3")
    );
}

#[test]
fn file_urls_strip_the_scheme() {
    assert_eq!(
        parse_target("file:///music/a.mp3"),
        PathBuf::from("/music/a.mp3")
    );
    // Spaces may appear literally (as in a shell-quoted URL).
    assert_eq!(
        parse_target("file:///music/10. What's It Gonna Be.mp3"),
        PathBuf::from("/music/10. What's It Gonna Be.mp3")
    );
    // ...or percent-encoded.
    assert_eq!(
        parse_target("file:///music/a%20b.mp3"),
        PathBuf::from("/music/a b.mp3")
    );
    // A host/authority component is dropped.
    assert_eq!(
        parse_target("file://localhost/music/a.mp3"),
        PathBuf::from("/music/a.mp3")
    );
}

#[test]
fn percent_decode_handles_malformed_input() {
    assert_eq!(percent_decode("a%2Fb"), "a/b");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
    assert_eq!(percent_decode("%2"), "%2");
}

#[test]
fn classify_drop_separates_folders_audio_and_junk() {
    let base = std::env::temp_dir().join(format!("nova-drop-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).unwrap();

    let song = base.join("song.mp3");
    std::fs::write(&song, b"not really audio").unwrap();
    std::fs::write(base.join("cover.jpg"), b"x").unwrap();
    std::fs::write(base.join("notes.txt"), b"x").unwrap();
    let missing = base.join("missing.flac");

    let (dirs, files, skipped) = classify_drop(vec![
        base.join("sub"),
        song.clone(),
        base.join("cover.jpg"),
        base.join("notes.txt"),
        missing.clone(),
    ]);

    assert_eq!(dirs, vec![base.join("sub")]);
    // Audio is matched by extension, whether or not the file exists yet.
    assert_eq!(files, vec![song, missing]);
    assert_eq!(skipped, 2);

    let _ = std::fs::remove_dir_all(&base);
}

/// A dropped folder is walked recursively; its audio comes back in file
/// name order and its junk is counted, not silently kept.
#[test]
fn a_folder_expands_to_its_audio_in_order() {
    let base = std::env::temp_dir().join(format!("nova-expand-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).unwrap();
    std::fs::write(base.join("b.mp3"), b"x").unwrap();
    std::fs::write(base.join("a.flac"), b"x").unwrap();
    std::fs::write(base.join("cover.jpg"), b"x").unwrap();
    std::fs::write(base.join("sub").join("c.ogg"), b"x").unwrap();
    std::fs::write(base.join("sub").join("notes.txt"), b"x").unwrap();

    let (files, skipped) = expand_to_audio(vec![base.clone()]);
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(
        files,
        vec![
            base.join("a.flac"),
            base.join("b.mp3"),
            base.join("sub").join("c.ogg"),
        ]
    );
    assert_eq!(skipped, 2);
}

#[test]
fn the_playing_row_shows_a_marker_instead_of_its_number() {
    let mut track = sample_track("/music/a.flac");
    track.track_no = Some(7);

    let numbered = track_row_text(&track, false);
    assert!(numbered.starts_with("  7   "), "got {numbered:?}");
    assert!(numbered.contains("Test NFM Track"));
    assert!(numbered.contains("Test Artist"));

    // Playing: the marker takes the number column so the cursor stays the
    // only filled row.
    let playing = track_row_text(&track, true);
    assert!(playing.starts_with("  \u{25b6}   "), "got {playing:?}");

    // No track number and not playing: the middle dot.
    track.track_no = None;
    assert!(track_row_text(&track, false).starts_with("  \u{b7}   "));
}

#[test]
fn format_labels_name_known_containers() {
    assert_eq!(format_label(Path::new("/a/b.nfm")), "NFM container");
    assert_eq!(format_label(Path::new("/a/b.no_fft")), "NoFFT");
    assert_eq!(format_label(Path::new("/a/b.nadc")), "NADC adaptive");
    assert_eq!(format_label(Path::new("/a/b.flac")), "FLAC");
    assert_eq!(format_label(Path::new("/a/b.MP3")), "MP3");
    // Unknown extensions are upper-cased; missing ones are Unknown.
    assert_eq!(format_label(Path::new("/a/b.xyz")), "XYZ");
    assert_eq!(format_label(Path::new("/a/b")), "Unknown");
}

#[test]
fn sample_rates_and_channels_are_human_readable() {
    assert_eq!(format_sample_rate(44_100), "44.1 kHz");
    assert_eq!(format_sample_rate(48_000), "48.0 kHz");
    assert_eq!(format_sample_rate(8_000), "8.0 kHz");
    assert_eq!(format_sample_rate(320), "320 Hz");

    assert_eq!(format_channels(1), "1 (mono)");
    assert_eq!(format_channels(2), "2 (stereo)");
    assert_eq!(format_channels(6), "6 ch");
}

fn sample_track(path: &str) -> Track {
    Track {
        id: 1,
        path: PathBuf::from(path),
        title: "Test NFM Track".to_string(),
        artist: "Test Artist".to_string(),
        album_artist: String::new(),
        album: "Test Album".to_string(),
        genre: None,
        year: None,
        track_no: None,
        disc_no: None,
        duration: Some(236.0),
        sample_rate: Some(44_100),
        channels: Some(2),
        has_cover: true,
        video: None,
    }
}

/// A saved playlist names every track and stays playable elsewhere.
#[test]
fn playlists_are_written_as_m3u8() {
    let mut bare = sample_track("/music/b.flac");
    bare.artist = String::new();
    bare.duration = None;
    let owned = [sample_track("/music/a.mp3"), bare];
    let tracks: Vec<&Track> = owned.iter().collect();

    let dir = std::env::temp_dir().join(format!("nova-m3u8-{}", std::process::id()));
    let path = dir.join("nested").join("list.m3u8");
    write_playlist(&path, &tracks).expect("write");

    let body = std::fs::read_to_string(&path).expect("read back");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        body,
        "#EXTM3U\n\
         #EXTINF:236,Test Artist - Test NFM Track\n/music/a.mp3\n\
         #EXTINF:-1,Test NFM Track\n/music/b.flac\n"
    );
}

/// A saved playlist reads back as the paths it listed, in order, with the
/// format's comments dropped and relative lines resolved against the
/// playlist's own folder.
#[test]
fn playlists_are_read_back_in_order() {
    let dir = std::env::temp_dir().join(format!("nova-m3u8-read-{}", std::process::id()));
    let path = dir.join("nested").join("list.m3u8");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &path,
        "#EXTM3U\n#EXTINF:236,Test Artist - Test NFM Track\n\
         /music/a.mp3\n\nsub/b.flac\n#EXTINF:-1,skipped\n\n/music/c.ogg\n",
    )
    .expect("write fixture");

    let files = read_playlist(&path).expect("read");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        files,
        vec![
            PathBuf::from("/music/a.mp3"),
            dir.join("nested").join("sub/b.flac"),
            PathBuf::from("/music/c.ogg"),
        ]
    );
}

/// Draws the info panel headlessly. This cannot assert on content, but it
/// proves the grid, scroll area and label wrapping all lay out cleanly.
fn layout_info_panel(track: &Track) {
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            track_info_body(ui, track, None);
        });
    });
}

fn row<'a>(rows: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
    rows.iter()
        .find(|(label, _)| *label == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn info_rows_describe_an_nfm_track() {
    let rows = track_info_rows(&sample_track("/music/test.nfm"));

    // The container is named, and the stream properties are those of the
    // inner payload that gets decoded for playback.
    assert_eq!(row(&rows, "Format"), Some("NFM container"));
    assert_eq!(row(&rows, "Duration"), Some("03:56"));
    assert_eq!(row(&rows, "Sample rate"), Some("44.1 kHz"));
    assert_eq!(row(&rows, "Channels"), Some("2 (stereo)"));
    assert_eq!(row(&rows, "Embedded art"), Some("Yes"));

    layout_info_panel(&sample_track("/music/test.nfm"));
}

#[test]
fn info_rows_omit_fields_the_file_does_not_carry() {
    let rows = track_info_rows(&sample_track("/music/tst.nfm"));

    assert_eq!(row(&rows, "Format"), Some("NFM container"));
    // This container has no genre, year or numbering of any kind.
    assert_eq!(row(&rows, "Genre"), None);
    assert_eq!(row(&rows, "Year"), None);
    assert_eq!(row(&rows, "Track"), None);
    assert_eq!(row(&rows, "Disc"), None);
}

#[test]
fn info_rows_report_full_tagging_when_present() {
    let mut track = sample_track("/music/tagged.flac");
    track.album_artist = "Album Artist".to_string();
    track.genre = Some("Ambient".to_string());
    track.year = Some(2024);
    track.track_no = Some(3);
    track.disc_no = Some(2);
    track.has_cover = false;

    let rows = track_info_rows(&track);

    assert_eq!(row(&rows, "Format"), Some("FLAC"));
    assert_eq!(row(&rows, "Album artist"), Some("Album Artist"));
    assert_eq!(row(&rows, "Genre"), Some("Ambient"));
    assert_eq!(row(&rows, "Year"), Some("2024"));
    assert_eq!(row(&rows, "Track"), Some("Disc 2 · Track 3"));
    assert_eq!(row(&rows, "Embedded art"), Some("No"));
}

#[test]
fn info_panel_falls_back_when_text_fields_are_missing() {
    let mut track = sample_track("/music/tst.nfm");
    track.title = "tst".to_string();
    track.artist = String::new();
    track.album = String::new();

    // The headline above the grid uses the never-empty grouping helpers.
    assert_eq!(track.display_artist(), "Unknown Artist");
    assert_eq!(track.display_album(), "Unknown Album");

    layout_info_panel(&track);
}

#[test]
fn window_mode_cycles_normal_fullscreen_maximized() {
    assert_eq!(WindowMode::Normal.next(), WindowMode::Fullscreen);
    assert_eq!(WindowMode::Fullscreen.next(), WindowMode::Maximized);
    assert_eq!(WindowMode::Maximized.next(), WindowMode::Normal);
    // Three presses of F are a full lap.
    let lap = WindowMode::Normal.next().next().next();
    assert_eq!(lap, WindowMode::Normal);
}
