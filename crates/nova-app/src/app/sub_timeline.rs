//! The `.ctrl` sub-timeline: per-media-file jump-over / go-up-to / repeat
//! marks that make a fragment of the main timeline playable on loop.
//!
//! `jo` is the start of a fragment, `gut` its end; when the cursor reaches
//! `gut` it jumps to the next `jo` and plays up to the nearest `gut` again.
//! With `repeat` the fragment loops endlessly, otherwise it stops at the
//! last `gut`. One `.ctrl` governs one media file and sits beside it, named
//! after it: `/path/to/<name of media file w/ ext>.ctrl`.

use super::*;

/// One `.ctrl` file: the time marks of a single media file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ctrl {
    /// Jump-over marks: where a new fragment starts.
    #[serde(default)]
    pub jo: Vec<f64>,
    /// Go-up-to marks: where a fragment ends.
    #[serde(default)]
    pub gut: Vec<f64>,
    /// Loop the whole fragment instead of stopping at the last `gut`.
    #[serde(default)]
    pub repeat: bool,
}

impl Ctrl {
    /// The `.ctrl` path that governs a media file with `path`: the full
    /// file name with `.ctrl` appended, so `ep.mkv` gives `ep.mkv.ctrl`.
    pub(super) fn path_for(media: &Path) -> Option<PathBuf> {
        media
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| media.with_file_name(format!("{name}.ctrl")))
    }

    /// Load the `.ctrl` that governs `media`, if one sits beside it.
    pub(super) fn load(media: &Path) -> Option<Ctrl> {
        let path = Self::path_for(media)?;
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Save this `.ctrl` next to `media`.
    pub(super) fn save(&self, media: &Path) -> std::io::Result<()> {
        let path = Self::path_for(media)
            .ok_or_else(|| std::io::Error::other("cannot determine .ctrl path"))?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
    }

    /// The first `jo` at or after `from`, or `None` when the cursor is past
    /// the last mark.
    pub(super) fn next_jo(&self, from: f64) -> Option<f64> {
        self.jo.iter().copied().find(|&mark| mark >= from)
    }

    /// The first `gut` strictly after `from` — the fragment's end.
    pub(super) fn next_gut(&self, from: f64) -> Option<f64> {
        self.gut
            .iter()
            .copied()
            .find(|&mark| mark > from + f64::EPSILON)
    }

    /// Advance the cursor: if it has reached a fragment's `gut`, jump to
    /// the next `jo`. Return `Some(seek)` when a jump is owed, `None` when
    /// the cursor is still inside the fragment. The landing mark is at or
    /// ahead of the cursor, so a repeated `repeat` wraps to the first `jo`
    /// when the last fragment ends.
    pub(super) fn advance(&self, position: f64) -> Option<f64> {
        // A jump is owed when the cursor sits at-or-past a `gut` that
        // started after the previous jump. The last `gut` at-or-before the
        // cursor is the one we just ran into; its successor `jo` is the
        // target.
        let hit = self
            .gut
            .iter()
            .copied()
            .filter(|&mark| mark <= position + f64::EPSILON)
            .last();
        hit.is_some().then(|| {
            let from = hit.unwrap();
            self.next_jo(from)
                .or_else(|| self.jo.first().copied())
                .unwrap_or(0.0)
        })
    }
}

impl NovaApp {
    /// The media path the active ctrl is attached to, if any.
    pub(super) fn ctrl_media_path(&self) -> Option<PathBuf> {
        self.active_ctrl_for.clone()
    }

    /// Ensure we have a ctrl object for the current track (create empty one if missing).
    pub(super) fn ensure_ctrl(&mut self) -> Option<&mut Ctrl> {
        let current = self.engine.current_track(&self.library)?;
        if self.active_ctrl_for.as_ref() != Some(&current.path) {
            self.active_ctrl_for = Some(current.path.clone());
            self.active_ctrl = Some(Ctrl {
                jo: Vec::new(),
                gut: Vec::new(),
                repeat: false,
            });
        }
        if self.active_ctrl.is_none() {
            self.active_ctrl_for = Some(current.path.clone());
            self.active_ctrl = Some(Ctrl {
                jo: Vec::new(),
                gut: Vec::new(),
                repeat: false,
            });
        }
        self.active_ctrl.as_mut()
    }

    /// Add current position as a `jo` mark and save the ctrl.
    pub(super) fn ctrl_add_jo(&mut self) {
        let Some(path) = self.ctrl_media_path().or_else(|| {
            self.engine.current_track(&self.library).map(|t| t.path.clone())
        }) else {
            return;
        };
        let position = self.engine.position().as_secs_f64();
        if let Some(ctrl) = self.ensure_ctrl() {
            ctrl.jo.push(position);
            ctrl.jo.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            ctrl.jo.dedup_by(|a, b| (*a - *b).abs() < f64::EPSILON);
        }
        if let Some(ctrl) = self.active_ctrl.as_ref() {
            if let Err(err) = ctrl.save(&path) {
                self.toast(format!("Ctrl save failed: {err}"));
                return;
            }
        }
        self.active_ctrl_for = Some(path);
        self.toast("jo mark added".to_string());
    }

    /// Add current position as a `gut` mark and save the ctrl.
    pub(super) fn ctrl_add_gut(&mut self) {
        let Some(path) = self.ctrl_media_path().or_else(|| {
            self.engine.current_track(&self.library).map(|t| t.path.clone())
        }) else {
            return;
        };
        let position = self.engine.position().as_secs_f64();
        if let Some(ctrl) = self.ensure_ctrl() {
            ctrl.gut.push(position);
            ctrl.gut.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            ctrl.gut.dedup_by(|a, b| (*a - *b).abs() < f64::EPSILON);
        }
        if let Some(ctrl) = self.active_ctrl.as_ref() {
            if let Err(err) = ctrl.save(&path) {
                self.toast(format!("Ctrl save failed: {err}"));
                return;
            }
        }
        self.active_ctrl_for = Some(path);
        self.toast("gut mark added".to_string());
    }

    /// Toggle repeat flag and save the ctrl.
    pub(super) fn ctrl_toggle_repeat(&mut self) {
        let Some(path) = self.ctrl_media_path().or_else(|| {
            self.engine.current_track(&self.library).map(|t| t.path.clone())
        }) else {
            return;
        };
        let repeat = if let Some(ctrl) = self.ensure_ctrl() {
            ctrl.repeat = !ctrl.repeat;
            ctrl.repeat
        } else {
            return;
        };
        if let Some(ctrl) = self.active_ctrl.as_ref() {
            if let Err(err) = ctrl.save(&path) {
                self.toast(format!("Ctrl save failed: {err}"));
                return;
            }
        }
        self.active_ctrl_for = Some(path);
        self.toast(if repeat { "ctrl repeat on".to_string() } else { "ctrl repeat off".to_string() });
    }

    /// Toggle ctrl activation for current track (Shift+Esc behavior).
    pub(super) fn ctrl_reload(&mut self) {
        if !self.ctrl_enabled {
            let current = match self.engine.current_track(&self.library) {
                Some(track) => track,
                None => return,
            };
            self.active_ctrl_for = Some(current.path.clone());
            self.active_ctrl = Ctrl::load(&current.path);
            if self.active_ctrl.is_some() {
                self.ctrl_enabled = true;
                self.toast("ctrl enabled".to_string());
                return;
            }
            self.toast("no ctrl file found".to_string());
            return;
        }
        self.ctrl_enabled = false;
        self.active_ctrl = None;
        self.active_ctrl_for = None;
        self.toast("ctrl disabled".to_string());
    }

    /// Drive the active `.ctrl` sub-timeline: only runs when explicitly enabled.
    /// when the cursor reaches a fragment's `gut`, jump it to the next `jo`.
    /// When the fragment is done — the cursor is past the last `gut` — stop
    /// when `repeat` is off and wrap to the first `jo` when it is on.
    pub(super) fn tick_ctrl(&mut self) {
        if !self.ctrl_enabled {
            return;
        }
        // A track switch invalidates the sub-timeline.
        let current = match self.engine.current_track(&self.library) {
            Some(track) => track,
            None => {
                self.active_ctrl = None;
                self.active_ctrl_for = None;
                self.ctrl_enabled = false;
                return;
            }
        };
        if self.active_ctrl_for.as_ref() != Some(&current.path) {
            self.active_ctrl_for = Some(current.path.clone());
            self.active_ctrl = Ctrl::load(&current.path);
            if self.active_ctrl.is_none() {
                self.ctrl_enabled = false;
                self.toast("ctrl disabled".to_string());
                return;
            }
        }
        let Some(ctrl) = &self.active_ctrl else {
            return;
        };
        let position = self.engine.position().as_secs_f64();
        // If we've reached/passed the last gut and not repeating, stop at it
        if !ctrl.repeat {
            if let Some(&last_gut) = ctrl.gut.last() {
                if position >= last_gut - 0.25 {
                    self.engine.stop();
                    return;
                }
            }
        }
        let Some(target) = ctrl.advance(position) else {
            return;
        };
        // The jump is the engine's next seek; the engine clamps it to the
        // duration, so a stray mark past the end is harmless.
        self.engine.seek(Duration::from_secs_f64(target));
    }
}
