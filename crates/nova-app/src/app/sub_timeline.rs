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

    /// Sort marks ascending and deduplicate them.
    pub(super) fn normalize(&mut self) {
        self.jo
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.gut
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.jo.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        self.gut.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    }

    /// Load the `.ctrl` that governs `media`, if one sits beside it.
    pub(super) fn load(media: &Path) -> Option<Ctrl> {
        let path = Self::path_for(media)?;
        let raw = std::fs::read_to_string(path).ok()?;
        let mut ctrl: Ctrl = serde_json::from_str(&raw).ok()?;
        ctrl.normalize();
        Some(ctrl)
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
    /// the cursor is still inside the fragment.
    pub(super) fn advance(&self, position: f64) -> Option<f64> {
        // Find which fragment we're in by pairing jo and gut in order
        if self.jo.is_empty() || self.gut.is_empty() {
            return None;
        }
        for i in 0..self.gut.len() {
            let gut = self.gut[i];
            if position >= gut - 1e-9 {
                // We've reached this gut - jump to next jo or wrap to first
                let next_jo = self.jo.get(i + 1).copied().or_else(|| self.jo.first().copied());
                return next_jo;
            }
        }
        None
    }
}

impl NovaApp {
    /// The media path the active ctrl is attached to, if any.
    pub(super) fn ctrl_media_path(&self) -> Option<PathBuf> {
        self.active_ctrl_for.clone()
    }

    fn ctrl_should_jump(&self, ctrl: &Ctrl, position: f64, target: f64) -> bool {
        // Don't jump if we're already very close to the target
        if (position - target).abs() < 0.8 {
            return false;
        }
        // Don't jump back to a jo if we just passed a gut but are already past that jo
        true
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
            ctrl.normalize();
        }
        if let Some(ctrl) = self.active_ctrl.as_mut() {
            ctrl.normalize();
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
            ctrl.normalize();
        }
        if let Some(ctrl) = self.active_ctrl.as_mut() {
            ctrl.normalize();
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
        let path = self.ctrl_media_path().or_else(|| {
            self.engine.current_track(&self.library).map(|t| t.path.clone())
        });
        let Some(path) = path else {
            return;
        };
        // When ctrl not enabled, try to load/create for current track
        if !self.ctrl_enabled {
            self.active_ctrl_for = Some(path.clone());
            if self.active_ctrl.is_none() {
                self.active_ctrl = Ctrl::load(&path);
            }
            if self.active_ctrl.is_none() {
                self.active_ctrl = Some(Ctrl {
                    jo: Vec::new(),
                    gut: Vec::new(),
                    repeat: false,
                });
            }
            self.ctrl_enabled = true;
        }
        let repeat = if let Some(ctrl) = self.active_ctrl.as_mut() {
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
        eprintln!("CTRL_TICK: enabled={} has_ctrl={} jo={:?} gut={:?}", self.ctrl_enabled, self.active_ctrl.is_some(), self.active_ctrl.as_ref().map(|c| &c.jo), self.active_ctrl.as_ref().map(|c| &c.gut));
        let Some(ctrl) = &self.active_ctrl else {
            return;
        };
        let position = self.engine.position().as_secs_f64();
        // If we've reached/passed the last gut and not repeating, seek to last gut and pause
        if !ctrl.repeat {
            if let Some(&last_gut) = ctrl.gut.last() {
                if position >= last_gut - 0.25 {
                    self.engine.pause();
                    return;
                }
            }
        }
        // When repeating and we reach/past the last gut, jump to first jo
        if ctrl.repeat && !ctrl.gut.is_empty() && !ctrl.jo.is_empty() {
            let last_gut = ctrl.gut[ctrl.gut.len()-1];
            if position >= last_gut - 2.0 {
                let first_jo = ctrl.jo[0];
                eprintln!("CTRL_REPEAT: pos={} last_gut={} jo[0]={} dur={:?} enabled={}", position, last_gut, first_jo, Duration::from_secs_f64(first_jo), self.ctrl_enabled);
                self.engine.seek(Duration::from_secs_f64(first_jo));
                return;
            }
        }
        let Some(target) = ctrl.advance(position) else {
            return;
        };
        if (position - target).abs() < 0.3 {
            return;
        }
        // The jump is the engine's next seek; the engine clamps it to the
        // duration, so a stray mark past the end is harmless.
        self.engine.seek(Duration::from_secs_f64(target));
    }
}
