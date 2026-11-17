//! Group marks: instances started with the same `-group-mark <name>` share
//! pause+minimise and a PID list in `/dev/shm/nova`.
//!
//! Pause+minimise travels through an append-only command log at
//! `/dev/shm/nova/<name>`; every member also appends its pid to
//! `/dev/shm/nova/pid.<name>`. Shift+K force-kills every pid in that list,
//! with the signalling instance itself last.

use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

/// The shared command file and the pid list for one group name.
pub(super) struct GroupMark {
    path: PathBuf,
    pid_path: PathBuf,
    /// Bytes already read, so only new lines are applied.
    offset: u64,
}

impl GroupMark {
    /// Opens (creating if needed) `/dev/shm/nova/<name>`, starts reading at
    /// its end so an old session's commands are not replayed, and registers
    /// this instance's pid in `/dev/shm/nova/pid.<name>`.
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
        let mark = Self::at(path);
        mark.register_pid();
        Some(mark)
    }

    fn at(path: PathBuf) -> Self {
        let pid_path = path.with_file_name(format!(
            "pid.{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            pid_path,
            offset,
        }
    }

    /// Adds this instance to the group's pid list.
    pub(super) fn register_pid(&self) {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.pid_path)
        else {
            return;
        };
        let _ = writeln!(file, "{}", std::process::id());
    }

    /// Force-kills every member, killing this instance last so it stays alive
    /// long enough to deliver the rest.
    pub(super) fn terminate_all(&self) {
        let text = std::fs::read_to_string(&self.pid_path).unwrap_or_default();
        let self_pid = std::process::id() as i32;
        for pid in kill_order(&pids_in(&text), self_pid) {
            // A stale pid may have been reused by another process since an
            // earlier session registered it.
            if pid == self_pid || is_nova(pid) {
                force_kill(pid);
            }
        }
    }

    /// Wakes the frame whenever the command file grows, so a pause from
    /// another member is picked up even while this window is minimised.
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

    pub(super) fn send_pause_minimize(&self) {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let _ = writeln!(file, "pause-minimize");
    }

    /// Whether a whole pause+minimise command was written since the last call.
    /// A file that shrank or vanished just restarts from its current end.
    pub(super) fn drain(&mut self) -> bool {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            self.offset = 0;
            return false;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len <= self.offset {
            if len < self.offset {
                self.offset = len;
            }
            return false;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut text = String::new();
        if file.read_to_string(&mut text).is_err() {
            return false;
        }
        // Only whole lines count; a half-written last line waits for its rest.
        let complete = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.offset += complete as u64;
        text[..complete].lines().any(|line| line == "pause-minimize")
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

fn pids_in(text: &str) -> Vec<i32> {
    text.lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Duplicates collapse, and the signalling instance always comes last.
fn kill_order(pids: &[i32], self_pid: i32) -> Vec<i32> {
    let mut order = Vec::new();
    for &pid in pids {
        if pid != self_pid && !order.contains(&pid) {
            order.push(pid);
        }
    }
    order.push(self_pid);
    order
}

fn is_nova(pid: i32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmd| cmdline_is_nova(&cmd))
}

fn cmdline_is_nova(cmd: &[u8]) -> bool {
    cmd.split(|&b| b == 0)
        .any(|word| word == b"nova" || word.ends_with(b"/nova"))
}

fn force_kill(pid: i32) {
    // Linux group files live in /dev/shm, so SIGKILL is 9.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    if unsafe { kill(pid, SIGKILL) } != 0 {
        log::warn!(
            "nova: could not kill pid {pid}: {}",
            std::io::Error::last_os_error()
        );
    }
}

impl NovaApp {
    /// Pauses and minimises here, and asks every group member to do the same.
    pub(super) fn group_pause_minimize(&mut self, ctx: &egui::Context) {
        if let Some(group) = &self.group {
            group.send_pause_minimize();
        }
        self.pause_and_minimize(ctx);
    }

    /// Shift+K force-kills the group's pid list; without a group, close here.
    pub(super) fn close_group(&self, ctx: &egui::Context) {
        match &self.group {
            Some(group) => group.terminate_all(),
            None => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// Applies what other members wrote since the last frame.
    pub(super) fn poll_group(&mut self, ctx: &egui::Context) {
        let received = match &mut self.group {
            Some(group) => group.drain(),
            None => return,
        };
        if received {
            self.pause_and_minimize(ctx);
        }
    }

    fn pause_and_minimize(&mut self, ctx: &egui::Context) {
        self.engine.pause();
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
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

        member.send_pause_minimize();
        member.send_pause_minimize();
        assert!(member.drain());
        assert!(!member.drain());

        // A late joiner starts at the end: what happened before it joined is
        // not replayed.
        let mut late = GroupMark::at(path.clone());
        assert!(!late.drain());

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
        assert!(!member.drain());

        writeln!(file, "minimize").expect("write");
        assert!(member.drain());

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn each_member_registers_its_pid_in_the_group_file() {
        let path = group_file("pids");
        let member = GroupMark::at(path.clone());

        member.register_pid();
        member.register_pid();
        let text = std::fs::read_to_string(&member.pid_path).expect("read pids");
        assert_eq!(pids_in(&text), vec![std::process::id() as i32; 2]);

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn only_numeric_pid_lines_are_used() {
        assert_eq!(pids_in("123\njunk\n\n 456 \n"), vec![123, 456]);
    }

    #[test]
    fn stale_pids_must_still_look_like_nova() {
        assert!(cmdline_is_nova(b"/usr/bin/nova\0--flag\0"));
        assert!(cmdline_is_nova(b"nova\0"));
        assert!(!cmdline_is_nova(b"/usr/bin/bash\0"));
    }

    #[test]
    fn the_signalling_instance_goes_last() {
        assert_eq!(kill_order(&[100, 200, 300, 200], 200), vec![100, 300, 200]);
        assert_eq!(kill_order(&[100], 999), vec![100, 999]);
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
