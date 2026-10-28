
 //! The `.ctrl` sub-timeline: per-media-file jump-over / go-up-to / repeat
//! marks that make a fragment of the main timeline playable on loop.
//!
//! `jo` is the start of a fragment, `gut` its end; when the cursor reaches
//! `gut` it jumps to the next `jo` and plays up to the nearest `gut` again.
//! With `repeat` the fragment loops endlessly, otherwise it stops at the last `gut`.

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
    pub(super) fn path_for(media: &Path) -> Option<PathBuf> {
        media
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| media.with_file_name(format!("{name}.ctrl")))
    }

    pub(super) fn normalize(&mut self) {
        self.jo
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.gut
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.jo.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        self.gut.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    }

    pub(super) fn load(media: &Path) -> Option<Ctrl> {
        let path = Self::path_for(media)?;
        let raw = std::fs::read_to_string(path).ok()?;
        let mut ctrl: Ctrl = serde_json::from_str(&raw).ok()?;
        ctrl.normalize();
        Some(ctrl)
    }

    pub(super) fn save(&self, media: &Path) -> std::io::Result<()> {
        let path = Self::path_for(media)
            .ok_or_else(|| std::io::Error::other("cannot determine .ctrl path"))?;
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
    }

    pub(super) fn next_jo(&self, from: f64) -> Option<f64> {
        self.jo.iter().copied().find(|&mark| mark >= from)
    }

    pub(super) fn next_gut(&self, from: f64) -> Option<f64> {
        self.gut
            .iter()
            .copied()
            .find(|&mark| mark > from + f64::EPSILON)
    }

    /// Advance the cursor when it reaches a `gut`.
    ///
    /// Supports a `gut` before the first `jo`, for example:
    /// jo  = [780.341]
    /// gut = [390.341, 1220.341]
    pub(super) fn advance(&self, position: f64) -> Option<f64> {
        if self.jo.is_empty() || self.gut.is_empty() {
            return None;
        }

        // Before the first jo, an earlier gut means we should enter
        // the first marked section.
        let last_jo = self
            .jo
            .iter()
            .copied()
            .take_while(|&jo| jo <= position)
            .last();

        let target = match last_jo {
            None => {
                let first_jo = self.jo[0];

                let reached_preceding_gut = self.gut.iter().any(|&gut| {
                    gut < first_jo && position >= gut - 1e-9
                });

                if reached_preceding_gut {
                    Some(first_jo)
                } else {
                    None
                }
            }

            Some(jo) => {
                // Find the first gut after the current jo.
                let gut = self.gut.iter().copied().find(|&gut| gut > jo)?;

                // Still inside this section.
                if position < gut - 1e-9 {
                    return None;
                }

                // Jump to the next jo after this gut, or wrap to jo[0].
                self.jo
                    .iter()
                    .copied()
                    .find(|&candidate| candidate > gut)
                    .or_else(|| self.jo.first().copied())
            }
        };

        // Avoid repeated seeks when already at the destination.
        target.filter(|target| (position - *target).abs() >= 0.3)
    }
}

impl NovaApp {
    /// The media path the active ctrl is attached to, if any.
    pub(super) fn ctrl_media_path(&self) -> Option<PathBuf> {
        self.active_ctrl_for.clone()
    }

    fn ctrl_should_jump(&self, _ctrl: &Ctrl, position: f64, target: f64) -> bool {
        (position - target).abs() >= 0.8
    }

    /// Ensure we have a ctrl object for the current track.
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

    /// Add current position as a jo mark and save the ctrl.
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

    /// Add current position as a gut mark and save the ctrl.
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
        self.toast(if repeat {
            "ctrl repeat on".to_string()
        } else {
            "ctrl repeat off".to_string()
        });
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

    /// Drive the active .ctrl sub-timeline.
    pub(super) fn tick_ctrl(&mut self) {
        if !self.ctrl_enabled {
            return;
        }

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

        if !ctrl.repeat {
            if let Some(&last_gut) = ctrl.gut.last() {
                if position >= last_gut - 0.25 {
                    self.engine.pause();
                    return;
                }
            }
        }

        // Use the same advance logic for both the initial jump and repeat.
        if let Some(target) = ctrl.advance(position) {
            self.engine.seek(Duration::from_secs_f64(target));
        }
    }
}

