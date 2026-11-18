//! The Subtitles window: SMPlayer's subtitle controls in one in-app panel.
//!
//! Opened from *Nova → Subtitles*. Everything lives inside an `egui::Window`
//! floating over the main UI, with one scroll area so all options stay reach-
//! able on a short screen.

use super::*;

/// Extensions accepted by the Browse dialog and by file-manager drops.
const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "ass", "ssa", "sub", "vtt"];

pub(super) fn is_subtitle_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SUBTITLE_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// SMPlayer's *Subtitles → Frames per second* choices, in menu order.
const FPS_CHOICES: [(&str, Option<f32>); 6] = [
    ("Default", None),
    ("23.976", Some(23.976)),
    ("24", Some(24.0)),
    ("25", Some(25.0)),
    ("29.970", Some(29.970)),
    ("30", Some(30.0)),
];

fn parse_delay(text: &str) -> Option<i32> {
    text.trim().parse().ok()
}

fn parse_font_size(text: &str) -> Option<f32> {
    text.trim()
        .parse::<f32>()
        .ok()
        .map(|size| size.clamp(10.0, 200.0))
}

impl NovaApp {
    pub(super) fn subtitles_window(&mut self, ctx: &egui::Context) {
        if !self.subtitles_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Nova Musique — Subtitles")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(380.0)
            .default_height(540.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Subtitles");
                    ui.label(RichText::new("Tracks, files and timing").weak());
                    ui.separator();

                    let tracks = self
                        .mpv
                        .as_ref()
                        .map(|mpv| mpv.subtitle_tracks())
                        .unwrap_or_default();

                    ui.label(RichText::new("Primary track").strong());
                    let primary = self.subtitle_primary.clone();
                    track_choices(
                        ui,
                        &tracks,
                        &primary,
                        |id| {
                            self.subtitle_primary = id.to_string();
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.select_subtitle_track(id);
                            }
                        },
                    );
                    ui.add_space(6.0);

                    ui.label(RichText::new("Secondary track").strong());
                    let secondary = self.subtitle_secondary.clone();
                    track_choices(
                        ui,
                        &tracks,
                        &secondary,
                        |id| {
                            self.subtitle_secondary = id.to_string();
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.select_secondary_subtitle_track(id);
                            }
                        },
                    );
                    ui.separator();

                    ui.label(RichText::new("Load").strong());
                    ui.label(
                        RichText::new("Drop subtitle files here, or pick one below")
                            .weak()
                            .size(12.0),
                    );
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.subtitle_path_input)
                                .desired_width(190.0)
                                .hint_text("/path/to/subtitles.srt"),
                        );
                        if ui.button("Browse…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Subtitles", &SUBTITLE_EXTENSIONS)
                                .pick_file()
                            {
                                self.subtitle_path_input = path.display().to_string();
                            }
                        }
                    });
                    if ui.button("Load external subtitle").clicked() {
                        let path = PathBuf::from(self.subtitle_path_input.trim());
                        if path.is_file() {
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.load_external_subtitle(&path);
                            }
                            self.toast("subtitle loaded".to_string());
                        } else {
                            self.toast("choose an existing subtitle file first".to_string());
                        }
                    }
                    if ui.button("Unload current subtitle").clicked() {
                        if let Some(mpv) = self.mpv.clone() {
                            mpv.unload_subtitle();
                        }
                        self.subtitle_primary = "no".to_string();
                    }
                    ui.separator();

                    ui.label(RichText::new("Set delay").strong());
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.subtitle_delay_text)
                                .desired_width(80.0),
                        );
                        ui.label("ms");
                        if ui.button("Apply").clicked() {
                            match parse_delay(&self.subtitle_delay_text) {
                                Some(ms) => {
                                    if let Some(mpv) = self.mpv.clone() {
                                        mpv.set_subtitle_delay_ms(ms);
                                    }
                                    self.toast(format!("subtitle delay {ms} ms"));
                                }
                                None => self.toast("delay must be a whole number".to_string()),
                            }
                        }
                    });
                    ui.add_space(6.0);

                    ui.label(RichText::new("Frames per second").strong());
                    egui::ComboBox::from_id_salt("nova-subtitle-fps")
                        .selected_text(FPS_CHOICES[self.subtitle_fps_index].0)
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for (index, (label, _)) in FPS_CHOICES.iter().enumerate() {
                                if ui
                                    .selectable_value(
                                        &mut self.subtitle_fps_index,
                                        index,
                                        *label,
                                    )
                                    .changed()
                                {
                                    if let Some(mpv) = self.mpv.clone() {
                                        mpv.set_subtitle_fps(FPS_CHOICES[index].1);
                                    }
                                }
                            }
                        });
                    ui.add_space(6.0);

                    ui.label(RichText::new("Font size").strong());
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.subtitle_font_size_text)
                                .desired_width(80.0),
                        );
                        if ui.button("Apply").clicked() {
                            match parse_font_size(&self.subtitle_font_size_text) {
                                Some(size) => {
                                    if let Some(mpv) = self.mpv.clone() {
                                        mpv.set_subtitle_font_size(size);
                                    }
                                    self.toast(format!("subtitle font size {size:.0}"));
                                }
                                None => self.toast("font size must be a number".to_string()),
                            }
                        }
                    });
                });
            });
        if !open {
            self.subtitles_open = false;
        }
    }

    /// Loads subtitle files dropped onto any window. mpv selects the last one
    /// added, so the track lists light up on their own; resetting our local
    /// choice lets mpv's selection show through.
    pub(super) fn drop_subtitles(&mut self, paths: Vec<PathBuf>) {
        let Some(mpv) = self.mpv.clone() else {
            self.toast("Load a video before adding subtitles".to_string());
            return;
        };
        for path in &paths {
            mpv.load_external_subtitle(path);
            self.subtitle_path_input = path.display().to_string();
        }
        self.subtitle_primary = "no".to_string();
        self.subtitle_secondary = "no".to_string();
        self.subtitles_open = true;
        self.toast(format!("Loaded {} subtitle file(s)", paths.len()));
    }
}

/// The two track menus share one mpv track list; only the selection and the
/// property they drive differ. "Off" is first, as in SMPlayer's menus.
fn track_choices(
    ui: &mut egui::Ui,
    tracks: &[crate::mpv::SubtitleTrack],
    selected: &str,
    mut choose: impl FnMut(&str),
) {
    if ui.selectable_label(selected == "no", "Off").clicked() {
        choose("no");
    }
    if tracks.is_empty() {
        ui.label(RichText::new("No subtitle tracks reported by mpv.").weak());
        return;
    }
    for track in tracks {
        // Until the user picks something here, show whatever mpv selected on
        // its own when the file loaded.
        let active = if selected == "no" {
            track.selected
        } else {
            selected == track.id
        };
        let label = format!("{} — {}", track.id, track.title);
        if ui.selectable_label(active, label).clicked() {
            choose(&track.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_and_font_size_are_parsed_and_clamped() {
        assert_eq!(parse_delay(" -250 "), Some(-250));
        assert_eq!(parse_delay("nope"), None);
        assert_eq!(parse_font_size("72"), Some(72.0));
        assert_eq!(parse_font_size("1000"), Some(200.0));
        assert_eq!(parse_font_size("1"), Some(10.0));
        assert_eq!(parse_font_size("big"), None);
    }

    #[test]
    fn fps_choices_start_with_mpvs_default() {
        assert_eq!(FPS_CHOICES[0], ("Default", None));
        assert_eq!(FPS_CHOICES[5].1, Some(30.0));
    }

    #[test]
    fn subtitle_drops_are_recognised_by_extension() {
        assert!(is_subtitle_file(Path::new("/tmp/movie.SRT")));
        assert!(is_subtitle_file(Path::new("sub.ass")));
        assert!(is_subtitle_file(Path::new("a.b.vtt")));
        assert!(!is_subtitle_file(Path::new("/tmp/movie.mkv")));
        assert!(!is_subtitle_file(Path::new("/tmp/notes.txt")));
    }
}
