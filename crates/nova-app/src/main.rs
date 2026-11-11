//! Nova — a local music library player with an Apollo-flavoured UI.
#[allow(unused)]
mod app;
mod mpv;
mod videowin;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

/// `-proxy-run <player>`: `player` is run on files nova itself cannot play.
const PROXY_RUN: &str = "-proxy-run";

/// The command line, split for its two consumers: the player `-proxy-run`
/// names, and the first target argument (a file or a folder) that is left
/// once that flag and its player are skipped.
pub(crate) struct CommandLine {
    pub(crate) proxy_player: Option<OsString>,
    pub(crate) target: Option<OsString>,
}

/// Position-independent: `-proxy-run` and the argument after it never count
/// as the target, whichever order they come in.
pub(crate) fn parse_args(args: &[OsString]) -> CommandLine {
    let mut proxy_player = None;
    let mut target = None;
    let mut player_next = false;
    for arg in args {
        if player_next {
            proxy_player = Some(arg.clone());
            player_next = false;
        } else if *arg == PROXY_RUN {
            player_next = true;
        } else if target.is_none() {
            target = Some(arg.clone());
        }
    }
    CommandLine { proxy_player, target }
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Re-launched by the main window to host the video-stream picker in its
    // own process, so that its GL context cannot collide with mpv's.
    if std::env::args_os().any(|arg| arg == videowin::FLAG) {
        return videowin::run();
    }

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let command_line = parse_args(&args);

    // A file nova cannot play itself goes to the player `-proxy-run` named.
    if let (Some(player), Some(target)) = (&command_line.proxy_player, &command_line.target) {
        let path = app::drops::parse_target(&target.to_string_lossy());
        if path.is_file() && !nova_core357_11::metadata::is_supported(&path) {
            // The player's stdio is closed off, so it cannot eat the stdin
            // pipe a script may be driving nova through. The dropped child
            // keeps running once this process exits.
            match std::process::Command::new(player)
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_child) => return Ok(()),
                Err(err) => {
                    eprintln!(
                        "nova: could not start {} for {}: {err}",
                        player.to_string_lossy(),
                        path.display()
                    );
                    std::process::exit(1);
                }
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([880.0, 560.0])
            .with_drag_and_drop(true)
            .with_title("Nova Musique"),
        ..Default::default()
    };

    eframe::run_native(
        "Nova Musique",
        options,
        Box::new(|cc| Ok(Box::new(app::NovaApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = PathBuf::from("/tmp/opencode/nova-proxy-run-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        path
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_flag_names_a_player_and_leaves_the_target_alone() {
        let odd = temp("clip.xyz").to_string_lossy().into_owned();
        let song = temp("song.mp3").to_string_lossy().into_owned();

        // The flag before or after the target makes no difference.
        for list in [
            vec!["-proxy-run", "mpv", odd.as_str()],
            vec![odd.as_str(), "-proxy-run", "mpv"],
        ] {
            let parsed = parse_args(&args(&list));
            assert_eq!(parsed.proxy_player, Some(OsString::from("mpv")));
            assert_eq!(parsed.target, Some(OsString::from(odd.clone())));
        }

        // Without the flag every argument is a target, and a flag that names
        // nobody leaves nothing special behind.
        let parsed = parse_args(&args(&[song.as_str()]));
        assert_eq!(parsed.proxy_player, None);
        assert_eq!(parsed.target, Some(OsString::from(song.clone())));
        let parsed = parse_args(&args(&["-proxy-run"]));
        assert_eq!(parsed.proxy_player, None);
        assert_eq!(parsed.target, None);
    }
}
