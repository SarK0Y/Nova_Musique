//! Commands read from stdin, one per line, so a script can drive the app.

use super::*;

/// Reads commands from stdin, one per line, waking the frame for each one.
///
/// A paused window repaints only when something asks it to, and nothing that
/// happens in a pipe reaches winit, so the wake-up has to come from here.
/// A terminal's stdin is left alone on purpose: this app is run from a shell
/// as often as from a launcher, and reading it would take those keystrokes.
pub(super) fn remote_reader(ctx: egui::Context) -> Option<Receiver<String>> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    Some(rx)
}

/// Turns one stdin line into something to do. Anything unrecognised is
/// ignored, the same way the picker's end of the pipe ignores it: this is a
/// pipe, and a line that does not parse is not worth acting on.
pub(super) fn remote_command(line: &str) -> Option<RemoteCommand> {
    let line = line.trim();
    match line {
        "toggle" => return Some(RemoteCommand::Action(Action::Toggle)),
        "next" => return Some(RemoteCommand::Action(Action::Next)),
        "prev" => return Some(RemoteCommand::Action(Action::Prev)),
        _ => {}
    }
    if let Some(seconds) = line.strip_prefix("seek ") {
        let seconds = seconds.trim().parse::<f32>().ok()?;
        return Some(RemoteCommand::Action(Action::Seek(seconds)));
    }
    if let Some(volume) = line.strip_prefix("volume ") {
        // Stays inside what the volume slider can set, so a stray number
        // cannot be saved to the config as something the UI would not offer.
        let volume = volume.trim().parse::<f32>().ok()?.clamp(0.0, 1.0);
        return Some(RemoteCommand::Action(Action::SetVolume(volume)));
    }
    if let Some(flag) = line.strip_prefix("picker ") {
        return Some(RemoteCommand::Picker(flag.trim() == "1"));
    }
    if let Some(flag) = line.strip_prefix("playlist ") {
        return Some(RemoteCommand::Playlist(flag.trim() == "1"));
    }
    if let Some(path) = line.strip_prefix("play ") {
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        return Some(RemoteCommand::Play(PathBuf::from(path)));
    }
    if let Some(mode) = line.strip_prefix("mode ") {
        return (!mode.trim().is_empty())
            .then(|| RemoteCommand::AudioMode(mode.trim().to_string()));
    }
    videowin::parse_command(line).map(RemoteCommand::Video)
}

impl NovaApp {
    /// Applies whatever stdin asked for since the last frame.
    pub(super) fn remote_commands(&mut self, actions: &mut Vec<Action>) {
        let lines: Vec<String> = match &self.remote {
            Some(remote) => remote.try_iter().collect(),
            None => return,
        };
        for line in lines {
            match remote_command(&line) {
                Some(RemoteCommand::Action(action)) => actions.push(action),
                Some(RemoteCommand::Video(command)) => self.apply_video_command(command),
                Some(RemoteCommand::Play(path)) => {
                    if !self.play_path(&path) {
                        self.toast(format!("{} is not in the library", path.display()));
                    }
                }
                Some(RemoteCommand::Picker(open)) => {
                    self.video_open = open;
                    if !open {
                        // Dropping it kills the child, which is what closing
                        // the window does.
                        self.video_window = None;
                    }
                }
                Some(RemoteCommand::Playlist(open)) => self.playlist_open = open,
                Some(RemoteCommand::AudioMode(mode)) => {
                    self.audio_mode = mode;
                    self.apply_audio_mode();
                }
                None => {}
            }
        }
    }
}
