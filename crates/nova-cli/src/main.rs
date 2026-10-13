//! Tiny CLI that exercises `nova-core` without any GUI.
//!
//! ```text
//! nova-cli scan   <dir>
//! nova-cli search <dir> <query>
//! nova-cli play   <dir> [--index N] [--volume V] [--null] [--seconds S]
//! ```

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use nova_core::{Library, NullBackend, PlaybackEngine, PlaybackState};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("scan") => cmd_scan(&args[1..]),
        Some("search") => cmd_search(&args[1..]),
        Some("play") => cmd_play(&args[1..]),
        _ => {
            usage();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "nova-cli — local music library player\n\n\
         USAGE:\n\
         \x20 nova-cli scan   <dir>\n\
         \x20 nova-cli search <dir> <query>\n\
         \x20 nova-cli play   <dir> [--index N] [--volume V] [--null] [--seconds S]\n\n\
         OPTIONS:\n\
         \x20 --index N     first track to play (default 0)\n\
         \x20 --volume V    output volume 0.0..=1.5 (default 0.8)\n\
         \x20 --null        use the silent null backend (each track lasts 3s)\n\
         \x20 --seconds S   stop after S seconds of real time (default 30)\n"
    );
}

fn scan(dir: &str) -> Result<(Library, nova_core::ScanReport), String> {
    let mut last_print = Instant::now();
    let (library, report) = Library::scan(dir, &mut |progress| {
        if last_print.elapsed() > Duration::from_millis(200) {
            eprint!("\rindexing… {} files", progress.indexed);
            let _ = std::io::stderr().flush();
            last_print = Instant::now();
        }
    })
    .map_err(|e| e.to_string())?;
    eprintln!("\r\x1b[2Kindexed {} tracks", library.len());
    Ok((library, report))
}

fn cmd_scan(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("missing <dir>")?;
    let (library, report) = scan(dir)?;

    println!("root:      {}", dir);
    println!("found:     {}", report.found);
    println!("indexed:   {}", report.indexed);
    println!("failed:    {}", report.failed);
    println!("elapsed:   {:.2?}", report.elapsed);
    println!("albums:    {}", library.albums().len());
    println!("artists:   {}", library.artists().len());

    println!("\nfirst 15 tracks:");
    for track in library.tracks().iter().take(15) {
        println!(
            "  {:>4}  {:<40}  {:<24}  {}",
            track.track_no.map(|n| n.to_string()).unwrap_or_default(),
            truncate(&track.title, 40),
            truncate(track.display_artist(), 24),
            truncate(track.display_album(), 28),
        );
    }

    if !report.errors.is_empty() {
        println!("\nfirst failures:");
        for error in report.errors.iter().take(5) {
            println!("  {error}");
        }
    }
    Ok(())
}

fn cmd_search(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: nova-cli search <dir> <query>".into());
    }
    let (library, _) = scan(&args[0])?;
    let query = args[1..].join(" ");
    let results = library.search(&query, 50);

    println!("{} result(s) for {query:?}", results.len());
    for track in results {
        println!(
            "  {:<40}  {:<24}  {}",
            truncate(&track.title, 40),
            truncate(track.display_artist(), 24),
            truncate(track.display_album(), 28),
        );
    }
    Ok(())
}

fn cmd_play(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("missing <dir>")?;
    let mut index = 0usize;
    let mut volume = 0.8f32;
    let mut use_null = false;
    let mut max_seconds = 30u64;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--index" => {
                index = take_value(args, &mut i)?.parse().map_err(|e| format!("--index: {e}"))?;
            }
            "--volume" => {
                volume = take_value(args, &mut i)?.parse().map_err(|e| format!("--volume: {e}"))?;
            }
            "--seconds" => {
                max_seconds = take_value(args, &mut i)?.parse().map_err(|e| format!("--seconds: {e}"))?;
            }
            "--null" => use_null = true,
            other => return Err(format!("unknown option {other}")),
        }
        i += 1;
    }

    let (library, _) = scan(dir)?;
    if library.is_empty() {
        return Err("no audio files found".into());
    }

    let mut engine = if use_null {
        PlaybackEngine::with_backend(Box::new(NullBackend::with_track_duration(
            Duration::from_secs(3),
        )))
    } else {
        PlaybackEngine::new()
    };
    engine.set_volume(volume);
    engine.set_queue(&library, library.all_track_ids(), index).map_err(|e| e.to_string())?;

    println!("backend: {}", engine.backend_name());
    println!("playing {} track(s), volume {:.0}%", library.len(), volume * 100.0);

    let deadline = Instant::now() + Duration::from_secs(max_seconds);
    let mut last_line = String::new();
    while Instant::now() < deadline {
        engine.tick(&library);

        if let Some(error) = engine.take_error() {
            eprintln!("warning: {error}");
        }

        let line = match engine.current_track(&library) {
            Some(track) => {
                let position = engine.position();
                let total = engine.duration(&library).unwrap_or_default();
                format!(
                    "[{}/{}] {} — {}  {} / {}",
                    engine.queue().current_item_index().map(|i| i + 1).unwrap_or(0),
                    library.len(),
                    track.title,
                    track.display_artist(),
                    format_clock(position),
                    format_clock(total),
                )
            }
            None => "— idle —".to_string(),
        };
        if line != last_line {
            println!("{line}");
            last_line = line;
        }

        if engine.state() == PlaybackState::Stopped {
            println!("playback finished");
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(())
}

fn take_value<'a>(args: &'a [String], i: &mut usize) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i).map(String::as_str).ok_or_else(|| "missing option value".to_string())
}

fn format_clock(duration: Duration) -> String {
    let total = duration.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn truncate(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        text.to_string()
    } else {
        let mut out: String = chars[..width.saturating_sub(1)].iter().collect();
        out.push('…');
        out
    }
}
