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
/// `-chk-magic`: accept files by content instead of extension.
const CHECK_MAGIC: &str = "-chk-magic";
/// `-allow-ext "a,b"`: extra extensions to accept.
const ALLOW_EXT: &str = "-allow-ext";

/// The command line, split for its two consumers: the player command
/// `-proxy-run` names (one word or several, like `flatpak run org.app`),
/// and the target argument (a file or a folder) that is left once the
/// flags are skipped.
pub(crate) struct CommandLine {
    pub(crate) proxy_player: Vec<OsString>,
    pub(crate) target: Option<OsString>,
    pub(crate) check_magic: bool,
    pub(crate) allow_ext: Vec<String>,
}

/// The player command is every word after `-proxy-run`. The target is the
/// first argument that is not a flag or `-allow-ext` value — or, when none
/// is given, a final word after the flag that names an existing file:
/// `nova -proxy-run flatpak run info.smplayer.SMPlayer clip.mkv`.
pub(crate) fn parse_args(args: &[OsString]) -> CommandLine {
    let mut proxy_player = Vec::new();
    let mut target = None;
    let mut check_magic = false;
    let mut allow_ext = Vec::new();
    let mut player_next = false;
    let mut ext_next = false;
    for arg in args {
        if ext_next {
            ext_next = false;
            for ext in arg.to_string_lossy().split(',') {
                let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
                if !ext.is_empty() {
                    allow_ext.push(ext);
                }
            }
        } else if arg == CHECK_MAGIC {
            check_magic = true;
        } else if arg == ALLOW_EXT {
            ext_next = true;
        } else if player_next {
            proxy_player.push(arg.clone());
        } else if arg == PROXY_RUN {
            player_next = true;
        } else if target.is_none() {
            target = Some(arg.clone());
        }
    }
    // shortcut: a player word that merely looks like a file would be eaten
    // here — put the target before the flag when that matters.
    if target.is_none() {
        if let Some(last) = proxy_player.last() {
            if std::path::Path::new(last).is_file() {
                target = proxy_player.pop();
            }
        }
    }
    CommandLine {
        proxy_player,
        target,
        check_magic,
        allow_ext,
    }
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
    nova_core357_11::metadata::configure(command_line.check_magic, &command_line.allow_ext);

    // A file nova cannot play itself goes to the player command `-proxy-run`
    // named; the file is appended to its words.
    if let Some(target) = &command_line.target {
        let path = app::drops::parse_target(&target.to_string_lossy());
        if !command_line.proxy_player.is_empty()
            && path.is_file()
            && !nova_core357_11::metadata::is_supported(&path)
        {
            let player = &command_line.proxy_player;
            // The player's stdio is closed off, so it cannot eat the stdin
            // pipe a script may be driving nova through. The dropped child
            // keeps running once this process exits.
            match std::process::Command::new(&player[0])
                .args(&player[1..])
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_child) => return Ok(()),
                Err(err) => {
                    let name = player
                        .iter()
                        .map(|word| word.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(" ");
                    eprintln!("nova: could not start {name} for {}: {err}", path.display());
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
    fn the_flag_names_a_player_command_and_leaves_the_target_alone() {
        let odd = temp("clip.xyz").to_string_lossy().into_owned();
        let song = temp("song.mp3").to_string_lossy().into_owned();
        let say = |list: Vec<String>| list.iter().map(OsString::from).collect::<Vec<_>>();

        // One word or several: the whole command after the flag is the
        // player, and a trailing existing file is the target.
        let parsed = parse_args(&args(&[
            "-proxy-run",
            "mpv",
            odd.as_str(),
        ]));
        assert_eq!(parsed.proxy_player, vec![OsString::from("mpv")]);
        assert_eq!(parsed.target, Some(OsString::from(odd.clone())));

        let parsed = parse_args(&args(&[
            "-proxy-run",
            "flatpak",
            "run",
            "info.smplayer.SMPlayer",
            odd.as_str(),
        ]));
        assert_eq!(
            parsed.proxy_player,
            say(vec!["flatpak".into(), "run".into(), "info.smplayer.SMPlayer".into()])
        );
        assert_eq!(parsed.target, Some(OsString::from(odd.clone())));

        // A target in front of the flag wins, whatever follows the flag.
        let parsed = parse_args(&args(&[
            odd.as_str(),
            "-proxy-run",
            "flatpak",
            "run",
            "info.smplayer.SMPlayer",
        ]));
        assert_eq!(parsed.target, Some(OsString::from(odd.clone())));
        assert_eq!(parsed.proxy_player.len(), 3);

        // A trailing word that is not a file stays part of the command.
        let parsed = parse_args(&args(&["-proxy-run", "flatpak", "run", "info.smplayer.SMPlayer"]));
        assert_eq!(parsed.proxy_player.len(), 3);
        assert_eq!(parsed.target, None);

        // Without the flag every argument is a target, and a flag that
        // names nobody leaves nothing special behind.
        let parsed = parse_args(&args(&[song.as_str()]));
        assert!(parsed.proxy_player.is_empty());
        assert_eq!(parsed.target, Some(OsString::from(song.clone())));
        let parsed = parse_args(&args(&["-proxy-run"]));
        assert!(parsed.proxy_player.is_empty());
        assert_eq!(parsed.target, None);
    }

    #[test]
    fn filter_flags_are_not_mistaken_for_the_target() {
        let song = temp("song.mp3").to_string_lossy().into_owned();
        let parsed = parse_args(&args(&[
            "-chk-magic",
            "-allow-ext",
            " .MP44, avi0 ,,",
            song.as_str(),
        ]));
        assert!(parsed.check_magic);
        assert_eq!(parsed.allow_ext, vec!["mp44", "avi0"]);
        assert_eq!(parsed.target, Some(OsString::from(song)));
    }
}
