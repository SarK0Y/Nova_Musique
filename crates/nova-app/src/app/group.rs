//! Group marks: instances started with the same `-group-mark <name>` share
//! pause+minimise and close through a command file in `/dev/shm/nova`.
//!
//! The file is an append-only log. Each member remembers how many bytes it has
//! already read, so commands written before it started are not replayed and a
//! command written by any member reaches all of them, once.

use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupCommand {
    PauseMinimize,
    Close,
}

impl GroupCommand {
    fn as_str(self) -> &'static str {
        match self {
            GroupCommand::PauseMinimize => "pause-minimize",
            GroupCommand::Close => "close",
        }
    }

    fn parse(line: &str) -> Option<GroupCommand> {
        match line {
            "pause-minimize" => Some(GroupCommand::PauseMinimize),
            "close" => Some(GroupCommand::Close),
            _ => None,
        }
    }
}

/// The shared command file for one group name.
pub(super) struct GroupMark {
    path: PathBuf,
    /// Bytes already read, so only new lines are applied.
    offset: u64,
}

impl GroupMark {
    /// Opens (creating if needed) `/dev/shm/nova/<name>` and starts reading
    /// from its end, so a fresh start does not replay an old session.
    pub(super) fn new(name: &str) -> Option<Self> {
        let Some(name) = sanitize(name) else {
            log::warn!(
                "nova: ignoring -group-mark {name:?}: use letters, digits, '.', '-' or '_'"
            );
            return None;
        };
        let dir = std::path::Path::new("/dev/shm/nova");
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(&name);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path);
        Some(Self::at(path))
    }

    fn at(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self { path, offset }
    }

    /// Wakes the frame whenever the file grows, so a command from another
    /// member is picked up even while this window is paused or minimised.
    pub(super) fn watch(&self, ctx: egui::Context) {
        let path = self.path.clone();
        std::thread::spawn(move || {
            let mut last = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if len != last {
                    last = len;
                    ctx.request_repaint();
                }
            }
        });
    }

    pub(super) fn send(&self, command: GroupCommand) {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let _ = writeln!(file, "{}", command.as_str());
    }

    /// Commands written since the last call. A file that shrank or vanished
    /// (a fresh group, or a cleanup) just restarts from its current end.
    pub(super) fn drain(&mut self) -> Vec<GroupCommand> {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            self.offset = 0;
            return Vec::new();
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len <= self.offset {
            if len < self.offset {
                self.offset = len;
            }
            return Vec::new();
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut text = String::new();
        if file.read_to_string(&mut text).is_err() {
            return Vec::new();
        }
        // Only whole lines count; a half-written last line waits for its rest.
        let complete = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.offset += complete as u64;
        text[..complete]
            .lines()
            .filter_map(GroupCommand::parse)
            .collect()
    }
}

/// A name must stay inside `/dev/shm/nova`: no separators and no `.`/`..`.
fn sanitize(name: &str) -> Option<String> {
    let name = name.trim();
    let safe = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        // `/dev/shm/nova/*.out` belongs to the state dump.
        && !name.ends_with(".out")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    safe.then(|| name.to_string())
}

impl NovaApp {
    /// Writes a command for the whole group and acts on it here as well.
    pub(super) fn group_command(&mut self, ctx: &egui::Context, command: GroupCommand) {
        if let Some(group) = &self.group {
            group.send(command);
        }
        self.apply_group_command(ctx, command);
    }

    /// Applies what other members wrote since the last frame.
    pub(super) fn poll_group(&mut self, ctx: &egui::Context) {
        let commands = match &mut self.group {
            Some(group) => group.drain(),
            None => return,
        };
        for command in commands {
            self.apply_group_command(ctx, command);
        }
    }

    fn apply_group_command(&mut self, ctx: &egui::Context, command: GroupCommand) {
        match command {
            GroupCommand::PauseMinimize => {
                self.engine.pause();
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            GroupCommand::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group_file(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nova-group-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("living-room")
    }

    #[test]
    fn members_read_each_command_once_and_late_joiners_skip_old_ones() {
        let path = group_file("replay");
        let mut member = GroupMark::at(path.clone());

        member.send(GroupCommand::PauseMinimize);
        member.send(GroupCommand::Close);
        assert_eq!(
            member.drain(),
            vec![GroupCommand::PauseMinimize, GroupCommand::Close]
        );
        assert!(member.drain().is_empty());

        // A late joiner starts at the end: what happened before it joined is
        // not replayed.
        let mut late = GroupMark::at(path.clone());
        assert!(late.drain().is_empty());

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let path = group_file("partial");
        let mut member = GroupMark::at(path.clone());

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open");
        write!(file, "pause-").expect("write");
        assert!(member.drain().is_empty());

        writeln!(file, "minimize").expect("write");
        assert_eq!(member.drain(), vec![GroupCommand::PauseMinimize]);

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn group_names_stay_inside_the_shared_directory() {
        assert_eq!(sanitize("living-room"), Some("living-room".to_string()));
        assert_eq!(sanitize(" room_2 "), Some("room_2".to_string()));
        assert_eq!(sanitize("../etc"), None);
        assert_eq!(sanitize("playlist.out"), None);
        assert_eq!(sanitize("a/b"), None);
        assert_eq!(sanitize(""), None);
    }
}
