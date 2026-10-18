//! The video-stream picker, run as its own process.
//!
//! Opening a second native window inside the main app hands it a GL context of
//! its own — eframe creates one per window and shares nothing between them — and
//! that is what libmpv's render context was fighting: mpv is bound to the main
//! window's context for good and cannot draw into the second one. Behind a
//! process boundary the picker's context belongs to a different process
//! entirely, so it never touches mpv. The picture stays in the main window.
//!
//! The two ends talk in plain lines over the child's stdin and stdout: the
//! parent sends the video streams it read from mpv, the child sends back the one
//! that got picked. No parsing library needed for five kinds of line.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::mpv::VideoTrack;

/// The flag that turns this same binary into the picker window.
pub const FLAG: &str = "--video-window";

// ---------------------------------------------------------------------------
// Parent side
// ---------------------------------------------------------------------------

/// The picker process, owned by the main window.
pub struct VideoWindow {
    child: Child,
    /// Gone once the child stops reading, which is how a dead child shows up.
    stdin: Option<std::process::ChildStdin>,
    picks: Receiver<String>,
    /// What the child already knows, so the list is only sent when it changes.
    sent: Vec<VideoTrack>,
}

impl VideoWindow {
    /// Starts the picker. `None` when this binary cannot re-launch itself.
    pub fn spawn() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let mut child = match Command::new(exe)
            .arg(FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The child writes only "pick <id>" lines; anything else mpv or
            // eframe prints there would be read as a pick.
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                log::warn!("could not start the video window ({err})");
                return None;
            }
        };

        let stdin = child.stdin.take();
        // Reading the child's stdout blocks, so it happens off the UI thread and
        // arrives as a queue.
        let picks = match child.stdout.take() {
            Some(out) => {
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                });
                rx
            }
            None => return None,
        };

        Some(VideoWindow {
            child,
            stdin,
            picks,
            sent: Vec::new(),
        })
    }

    /// Whether the picker has exited, so the menu can stop claiming it is open.
    pub fn closed(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Sends the current video streams, but only when they differ from what the
    /// child last got. mpv reports these the same way every tick, so sending
    /// unconditionally would rewrite the list on every frame.
    pub fn send_tracks(&mut self, tracks: &[VideoTrack]) {
        if tracks == self.sent && self.stdin.is_some() {
            return;
        }
        self.sent.clear();
        self.sent.extend_from_slice(tracks);

        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        log::debug!("video window: {} streams", tracks.len());
        if let Err(err) = stdin.write_all(encode(tracks).as_bytes()) {
            // A dead child is normal, not an error worth logging every frame.
            log::debug!("video window is gone ({err})");
            self.stdin = None;
        }
    }

    /// The stream picked since the last call, if any.
    pub fn picked(&mut self) -> Option<String> {
        let mut picked = None;
        loop {
            match self.picks.try_recv() {
                Ok(line) => {
                    if let Some(id) = line.strip_prefix("pick ") {
                        picked = Some(id.to_string());
                    }
                }
                Err(TryRecvError::Empty) => return picked,
                Err(TryRecvError::Disconnected) => return picked,
            }
        }
    }
}

impl Drop for VideoWindow {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Child side
// ---------------------------------------------------------------------------

/// One row of the picker's list, as the parent described it.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    id: String,
    title: String,
    size: Option<(u32, u32)>,
    selected: bool,
}

/// Packs the streams into the lines the picker reads.
///
/// A count comes first so a half-written list never shows: the picker clears its
/// rows when it sees one and only ever adds what follows.
fn encode(tracks: &[VideoTrack]) -> String {
    let mut lines = format!("tracks {}\n", tracks.len());
    for track in tracks {
        let size = match track.size {
            Some((width, height)) => format!("{width}x{height}"),
            None => "-".to_string(),
        };
        // The title goes last so it may contain tabs.
        lines.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            track.id,
            size,
            u8::from(track.selected),
            track.title
        ));
    }
    lines
}

/// Unpacks one line into `rows`. Anything unrecognised is ignored, because this
/// reads a pipe a process on the other end is writing.
fn decode(rows: &mut Vec<Row>, line: &str) {
    if let Some(count) = line.strip_prefix("tracks ") {
        rows.clear();
        rows.reserve(count.trim().parse().unwrap_or(0));
        return;
    }
    let mut parts = line.splitn(4, '\t');
    let (Some(id), Some(size), Some(selected)) = (parts.next(), parts.next(), parts.next()) else {
        return;
    };
    rows.push(Row {
        id: id.to_string(),
        title: parts.next().unwrap_or_default().to_string(),
        size: size
            .split_once('x')
            .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?))),
        selected: selected == "1",
    });
}

/// Runs the picker window. This is the whole child process.
pub fn run() -> eframe::Result<()> {
    let incoming = Arc::new(Mutex::new(Vec::new()));
    {
        let incoming = Arc::clone(&incoming);
        // The child's stdin blocks, so lines land on a queue for the UI to drain.
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines().map_while(Result::ok) {
                if incoming.lock().expect("poisoned").len() < 4096 {
                    incoming.lock().expect("poisoned").push(line);
                }
            }
            // The parent is gone, so this window has nothing left to choose for.
            // Closing it here means a killed or crashed main window does not
            // leave a picker behind.
            std::process::exit(0);
        });
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([320.0, 240.0])
            .with_min_inner_size([240.0, 160.0]),
        ..Default::default()
    };
    eframe::run_native(
        "video",
        options,
        Box::new(|_cc| {
            Ok(Box::new(Picker {
                incoming,
                rows: Vec::new(),
                stdout: std::io::stdout(),
            }))
        }),
    )
}

struct Picker {
    incoming: Arc<Mutex<Vec<String>>>,
    rows: Vec<Row>,
    stdout: std::io::Stdout,
}

impl Picker {
    /// Applies whatever the parent has sent since the last frame.
    fn drain(&mut self) {
        let lines = std::mem::take(&mut *self.incoming.lock().expect("poisoned"));
        for line in lines {
            decode(&mut self.rows, &line);
        }
    }

    fn pick(&mut self, id: &str) {
        let mut out = &self.stdout;
        if writeln!(out, "pick {id}").is_err() {
            // The parent is gone, so there is nothing left to choose for.
            std::process::exit(0);
        }
    }
}

impl eframe::App for Picker {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ui.input(|input| input.viewport().close_requested()) {
            return;
        }

        // Nothing here changes on its own, and the app is idle while paused, so
        // without this the list would sit on whatever it first drew — usually
        // "<empty>", because the window opens before playback starts.
        ctx.request_repaint_after(Duration::from_millis(200));

        self.drain();

        ui.heading("Video");
        if self.rows.is_empty() {
            // Same wording as SMPlayer's empty track list.
            ui.add_enabled(false, eframe::egui::Button::new("<empty>"));
            ui.label(eframe::egui::RichText::new("No video stream is loaded").weak());
            return;
        }

        ui.label(eframe::egui::RichText::new("Select the video stream").weak());
        ui.separator();
        for i in 0..self.rows.len() {
            let row = &self.rows[i];
            let label = match row.size {
                Some((width, height)) => format!("{}  ({width}x{height})", row.title),
                None => row.title.clone(),
            };
            let clicked = ui.selectable_label(row.selected, label).clicked();
            if clicked && !row.selected {
                let id = self.rows[i].id.clone();
                self.pick(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str, size: Option<(u32, u32)>, selected: bool) -> VideoTrack {
        VideoTrack {
            id: id.to_string(),
            title: title.to_string(),
            size,
            selected,
        }
    }

    #[test]
    fn tracks_survive_the_pipe() {
        let tracks = vec![
            track("1", "h264", Some((640, 360)), true),
            track("2", "hevc\twith a tab", None, false),
        ];

        let mut rows = Vec::new();
        for line in encode(&tracks).lines() {
            decode(&mut rows, line);
        }

        assert_eq!(
            rows,
            vec![
                Row {
                    id: "1".to_string(),
                    title: "h264".to_string(),
                    size: Some((640, 360)),
                    selected: true,
                },
                Row {
                    id: "2".to_string(),
                    title: "hevc\twith a tab".to_string(),
                    size: None,
                    selected: false,
                },
            ]
        );
    }

    #[test]
    fn a_new_list_replaces_the_old_one() {
        let mut rows = Vec::new();
        for line in encode(&[track("1", "h264", Some((640, 360)), true)]).lines() {
            decode(&mut rows, line);
        }
        // Playback stopped: the parent sends an empty list, and the picker must
        // not keep showing what it had.
        for line in encode(&[]).lines() {
            decode(&mut rows, line);
        }

        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn rubbish_from_the_pipe_is_ignored() {
        let mut rows = Vec::new();
        for line in ["", "pick 2", "no tabs here", "tracks not a number"] {
            decode(&mut rows, line);
        }

        assert!(rows.is_empty(), "{rows:?}");
    }
}
