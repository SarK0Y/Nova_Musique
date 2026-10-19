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

/// SMPlayer's zoom step and floor, from its `mplayerwindow.h`.
const ZOOM_STEP: f32 = 0.05;
const ZOOM_MIN: f32 = 0.5;
/// SMPlayer has no ceiling. One is needed here: the picture is drawn into a
/// texture this big, so an unbounded zoom would ask for an unbounded one.
const ZOOM_MAX: f32 = 4.0;
/// SMPlayer's nudge step, `MplayerWindow::moveLayer(±16, ±16)`.
const PAN_STEP: f32 = 16.0;

/// SMPlayer's *Deinterlace* submenu (`basegui.cpp:1189-1194`), in its order.
/// The index is what travels over the pipe, so the labels sit beside the wire
/// and the filter each one means sits beside mpv, in `crate::mpv`.
pub const DEINTERLACERS: [&str; 6] = [
    "None",
    "Lowpass5",
    "Yadif (normal)",
    "Yadif (double framerate)",
    "Linear Blend",
    "Kerndeint",
];

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
    /// Same, for the mirror/flip/zoom/pan/move-allowed state.
    sent_state: (bool, bool, f32, eframe::egui::Vec2, bool, usize),
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
            sent_state: (false, false, 1.0, eframe::egui::Vec2::ZERO, true, 0),
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

    /// Tells the picker where the picture currently sits.
    ///
    /// Only sent when it changes, for the same reason the stream list is.
    pub fn send_state(
        &mut self,
        x: bool,
        y: bool,
        zoom: f32,
        pan: eframe::egui::Vec2,
        move_allowed: bool,
        deinterlace: usize,
    ) {
        let state = (x, y, zoom, pan, move_allowed, deinterlace);
        if state == self.sent_state && self.stdin.is_some() {
            return;
        }
        self.sent_state = state;
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let line = format!(
            "state {x} {y} {zoom} {:.2} {:.2} {} {}\n",
            pan.x,
            pan.y,
            u8::from(move_allowed),
            deinterlace
        );
        if let Err(err) = stdin.write_all(line.as_bytes()) {
            log::debug!("video window is gone ({err})");
            self.stdin = None;
        }
    }

    /// Everything the picker asked for since the last call.
    pub fn commands(&mut self) -> Vec<VideoCommand> {
        let mut commands = Vec::new();
        loop {
            match self.picks.try_recv() {
                Ok(line) => commands.extend(parse_command(&line)),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return commands,
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

/// Something the picker asked the main window to do.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoCommand {
    /// Show the video stream with this track id.
    Pick(String),
    /// Mirror the picture left-right (`x`), flip it top-bottom (`y`).
    Flip { x: bool, y: bool },
    /// Scale the picture by this factor; 1.0 is the size that fits.
    Zoom(f32),
    /// Nudge the picture off centre, in points, with +y running down.
    Pan(f32, f32),
    /// Whether the picture may be moved at all.
    MoveAllowed(bool),
    /// Which of [`DEINTERLACERS`] should be on the filter chain.
    Deinterlace(usize),
}

/// Reads one line from the pipe, whether that is the picker's end or nova's
/// own stdin. Anything unrecognised is ignored: this is a pipe, and a line
/// that does not parse is not worth acting on. Both flags are read as "is it
/// 1", so a garbled one counts as off rather than as an error.
pub(crate) fn parse_command(line: &str) -> Option<VideoCommand> {
    if let Some(id) = line.strip_prefix("pick ") {
        return Some(VideoCommand::Pick(id.to_string()));
    }
    if let Some(flag) = line.strip_prefix("move ") {
        return Some(VideoCommand::MoveAllowed(flag.trim() == "1"));
    }
    if let Some(choice) = line.strip_prefix("deint ") {
        let choice = choice.trim().parse::<usize>().ok()?;
        return Some(VideoCommand::Deinterlace(choice));
    }
    if let Some(pan) = line.strip_prefix("pan ") {
        let (x, y) = pan.trim().split_once(' ')?;
        return Some(VideoCommand::Pan(
            x.parse().unwrap_or(0.0),
            y.parse().unwrap_or(0.0),
        ));
    }
    if let Some(zoom) = line.strip_prefix("zoom ") {
        return Some(VideoCommand::Zoom(zoom.trim().parse().unwrap_or(1.0)));
    }
    let (x, y) = line.strip_prefix("flip ")?.split_once(' ')?;
    Some(VideoCommand::Flip {
        x: x == "1",
        y: y == "1",
    })
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
                flip_x: false,
                flip_y: false,
                zoom: 1.0,
                pan: eframe::egui::Vec2::ZERO,
                allow_video_movement: true,
                deinterlace: 0,
                rows: Vec::new(),
                stdout: std::io::stdout(),
            }))
        }),
    )
}

struct Picker {
    incoming: Arc<Mutex<Vec<String>>>,
    rows: Vec<Row>,
    /// Whether the picture is mirrored left-right and flipped top-bottom.
    flip_x: bool,
    flip_y: bool,
    /// How far the picture is scaled up from the size that fits.
    zoom: f32,
    /// How far the picture is nudged off centre, with +y running down.
    pan: eframe::egui::Vec2,
    /// Whether the picture may be moved at all.
    allow_video_movement: bool,
    /// Which of [`DEINTERLACERS`] is on the filter chain.
    deinterlace: usize,
    stdout: std::io::Stdout,
}

impl Picker {
    /// Applies whatever the parent has sent since the last frame.
    fn drain(&mut self) {
        let lines = std::mem::take(&mut *self.incoming.lock().expect("poisoned"));
        for line in lines {
            if let Some(state) = line.strip_prefix("state ") {
                let mut parts = state.split_whitespace();
                let (Some(x), Some(y), Some(zoom)) = (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                // Tokens follow `VideoWindow::send_state`: x y zoom pan.x pan.y
                // move. Reading the flag before the pan values puts pan.x where
                // the flag belongs, and every state line then reports movement
                // as forbidden — which quietly un-ticks the checkbox.
                let number =
                    |value: Option<&str>| value.and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let pan_x = number(parts.next());
                let pan_y = number(parts.next());
                let move_allowed = match parts.next() {
                    Some(flag) => flag == "1",
                    None => self.allow_video_movement,
                };
                let deinterlace = parts
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|index| *index < DEINTERLACERS.len());
                self.flip_x = x == "1";
                self.flip_y = y == "1";
                self.zoom = zoom.parse().unwrap_or(1.0);
                self.pan = eframe::egui::vec2(pan_x, pan_y);
                self.allow_video_movement = move_allowed;
                if let Some(index) = deinterlace {
                    self.deinterlace = index;
                }
                continue;
            }
            decode(&mut self.rows, &line);
        }
    }

    fn send(&mut self, line: &str) {
        let mut out = &self.stdout;
        if writeln!(out, "{line}").is_err() {
            // The parent is gone, so there is nothing left to control.
            std::process::exit(0);
        }
    }

    fn flip(&mut self) {
        let line = format!("flip {} {}", u8::from(self.flip_x), u8::from(self.flip_y));
        self.send(&line);
    }

    /// SMPlayer nudges the video widget 16 pixels at a time.
    fn pan_by(&mut self, dx: f32, dy: f32) {
        self.pan += eframe::egui::vec2(dx, dy) * PAN_STEP;
        self.send(&format!("pan {:.2} {:.2}", self.pan.x, self.pan.y));
    }

    fn zoom_by(&mut self, step: f32) {
        self.zoom = (self.zoom + step).clamp(ZOOM_MIN, ZOOM_MAX);
        self.send(&format!("zoom {:.2}", self.zoom));
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

        // SMPlayer's *Mirror image* and *Flip image*. There it asks for the
        // `hflip` and `vflip` lavfi filters; mpv has no property for either, but
        // the `vf` command puts them in the pipeline exactly the same way.
        ui.horizontal(|ui| {
            if ui.selectable_label(self.flip_x, "Mirror").clicked() {
                self.flip_x = !self.flip_x;
                self.flip();
            }
            if ui.selectable_label(self.flip_y, "Flip").clicked() {
                self.flip_y = !self.flip_y;
                self.flip();
            }
        });
        // SMPlayer's *Deinterlace* submenu, as a picker over the same six
        // choices. Choosing one replaces whatever was on the chain rather than
        // adding to it, which is what `Core::changeDeinterlace` does for mpv.
        ui.horizontal(|ui| {
            ui.label("Deinterlace:");
            let previous = self.deinterlace;
            eframe::egui::ComboBox::from_id_salt("deinterlace")
                .selected_text(DEINTERLACERS[self.deinterlace])
                .show_ui(ui, |ui| {
                    for (index, name) in DEINTERLACERS.iter().enumerate() {
                        ui.selectable_value(&mut self.deinterlace, index, *name);
                    }
                });
            if self.deinterlace != previous {
                self.send(&format!("deint {}", self.deinterlace));
            }
        });
        // SMPlayer's *Zoom*: stepping, and a way back to the size that fits. It
        // scales the video widget and re-centres it; nothing goes to mpv, and
        // neither does anything here.
        ui.horizontal(|ui| {
            if ui.button("Zoom \u{2212}").clicked() {
                self.zoom_by(-ZOOM_STEP);
            }
            if ui.button("Reset").clicked() {
                self.zoom = 1.0;
                self.send("zoom 1.00");
            }
            if ui.button("Zoom +").clicked() {
                self.zoom_by(ZOOM_STEP);
            }
            ui.label(format!("{:.0}%", self.zoom * 100.0));
        });
        // SMPlayer's *Move left/right/up/down*, under the same Zoom menu. The
        // main window clamps these so the picture cannot leave the panel; that
        // is the check MplayerWindow::moveLeft does before moving.
        ui.horizontal(|ui| {
            // Words, not arrows: egui's bundled font has no glyph for U+2190
            // and friends, and SMPlayer's own labels are words too.
            if ui.button("Left").clicked() {
                self.pan_by(-1.0, 0.0);
            }
            if ui.button("Right").clicked() {
                self.pan_by(1.0, 0.0);
            }
            if ui.button("Up").clicked() {
                self.pan_by(0.0, -1.0);
            }
            if ui.button("Down").clicked() {
                self.pan_by(0.0, 1.0);
            }
            if ui.button("Centre").clicked() {
                self.pan = eframe::egui::Vec2::ZERO;
                self.send("pan 0.00 0.00");
            }
        });
        // SMPlayer's `allow_video_movement`, which decides whether the picture
        // may be pushed around at all or only while it reaches past the panel.
        let allow = ui
            .checkbox(&mut self.allow_video_movement, "Allow moving the video")
            .on_hover_text(
                "Off: the video can only be moved while it is bigger than the \
                 panel. On: it can be moved freely, and never leaves the panel.",
            );
        // The parent owns the setting, so the click has to travel: without it
        // this box only flips a local copy and the next state line puts it back.
        if allow.changed() {
            self.send(&format!("move {}", self.allow_video_movement as u8));
        }
        ui.separator();

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
                self.send(&format!("pick {id}"));
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
    fn a_state_line_lands_field_by_field() {
        // The exact line `send_state` writes: x y zoom pan.x pan.y move.
        let mut picker = Picker {
            incoming: Arc::new(Mutex::new(vec![
                "state 1 0 1.50 -16.00 32.00 1 3".to_string()
            ])),
            rows: Vec::new(),
            flip_x: false,
            flip_y: false,
            zoom: 1.0,
            pan: eframe::egui::Vec2::ZERO,
            allow_video_movement: false,
            deinterlace: 0,
            stdout: std::io::stdout(),
        };

        picker.drain();

        assert!(picker.flip_x, "mirror");
        assert!(!picker.flip_y, "flip");
        assert_eq!(picker.zoom, 1.5, "zoom");
        assert_eq!(picker.pan, eframe::egui::vec2(-16.0, 32.0), "pan");
        // The flag is the last token. Read one place early it becomes pan.x == "1",
        // which is false for every real pan, so the checkbox always came back off.
        assert!(picker.allow_video_movement, "allow moving");
        // Last token, and only accepted when it names a real choice.
        assert_eq!(picker.deinterlace, 3, "deinterlace");
    }

    #[test]
    fn a_deinterlace_choice_travels_as_an_index() {
        assert_eq!(parse_command("deint 3"), Some(VideoCommand::Deinterlace(3)));
        assert_eq!(parse_command("deint nope"), None);
        assert_eq!(parse_command("deint"), None);
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
    fn commands_from_the_picker_are_read() {
        assert_eq!(
            parse_command("pick 2"),
            Some(VideoCommand::Pick("2".to_string()))
        );
        assert_eq!(
            parse_command("flip 1 0"),
            Some(VideoCommand::Flip { x: true, y: false })
        );
        assert_eq!(
            parse_command("flip 1 1"),
            Some(VideoCommand::Flip { x: true, y: true })
        );
    }

    #[test]
    fn zoom_comes_back_as_a_number() {
        assert_eq!(parse_command("zoom 1.35"), Some(VideoCommand::Zoom(1.35)));
        // A garbled zoom is a reset, not a crash.
        assert_eq!(parse_command("zoom soon"), Some(VideoCommand::Zoom(1.0)));
    }

    #[test]
    fn pan_comes_back_as_a_pair() {
        assert_eq!(
            parse_command("pan -16.00 32.50"),
            Some(VideoCommand::Pan(-16.0, 32.5))
        );
        assert_eq!(
            parse_command("pan 0.00 0.00"),
            Some(VideoCommand::Pan(0.0, 0.0))
        );
    }

    #[test]
    fn rubbish_from_the_pipe_is_ignored() {
        for line in [
            "", "pick", "flip", "flip x", "pan", "pan 1", "state", "nonsense",
        ] {
            assert_eq!(parse_command(line), None, "{line:?} was not ignored");
        }
        let mut rows = Vec::new();
        for line in ["", "pick 2", "no tabs here", "tracks not a number"] {
            decode(&mut rows, line);
        }

        assert!(rows.is_empty(), "{rows:?}");
    }
}
