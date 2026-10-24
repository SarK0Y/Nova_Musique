//! The top bar and the bottom transport bar.

use super::*;

impl NovaApp {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // Keep the release number left-aligned while the product name stays
        // visually centered in the main window, independent of menu widths.
        let header_height = 24.0;
        let (header_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), header_height),
            egui::Sense::hover(),
        );
        ui.painter().text(
            header_rect.left_center() + egui::vec2(2.0, 0.0),
            egui::Align2::LEFT_CENTER,
            concat!("v", env!("CARGO_PKG_VERSION")),
            egui::FontId::proportional(12.0),
            ui.visuals().weak_text_color(),
        );
        ui.painter().text(
            header_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Nova Musique",
            egui::FontId::proportional(16.0),
            ui.visuals().text_color(),
        );
        ui.horizontal(|ui| {
            ui.menu_button(RichText::new("Nova").strong().size(18.0), |ui| {
                if ui.button("Playlist").clicked() {
                    self.playlist_open = true;
                    ui.close();
                }
                ui.menu_button("Video", |ui| {
                    if ui.button("Video tracks and controls…").clicked() {
                        self.video_open = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Aspect ratio", |ui| {
                        let choices: [(&str, Option<f32>); 6] = [
                            ("Auto (source)", None),
                            ("16:9", Some(16.0 / 9.0)),
                            ("4:3", Some(4.0 / 3.0)),
                            ("16:10", Some(16.0 / 10.0)),
                            ("1:1", Some(1.0)),
                            ("2.35:1", Some(2.35)),
                        ];
                        for (label, ratio) in choices {
                            let selected = match (self.aspect_ratio_override, ratio) {
                                (None, None) => true,
                                (Some(a), Some(b)) => (a - b).abs() < 0.001,
                                _ => false,
                            };
                            if ui.selectable_label(selected, label).clicked() {
                                self.aspect_ratio_override = ratio;
                                self.pan = egui::Vec2::ZERO;
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Rotate", |ui| {
                        for (label, degrees) in [
                            ("0°", 0),
                            ("90° clockwise", 90),
                            ("180°", 180),
                            ("270° clockwise", 270),
                        ] {
                            if ui
                                .selectable_label(self.video_rotation == degrees, label)
                                .clicked()
                            {
                                self.video_rotation = degrees;
                                if let Some(mpv) = self.mpv.clone() {
                                    mpv.set_video_rotation(degrees);
                                }
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Filters", |ui| {
                        ui.label("Noise suppression");
                        if ui
                            .checkbox(&mut self.noise_hqdn3d, "HQDN3D denoise")
                            .changed()
                        {
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.set_video_filter("lavfi=[hqdn3d]", self.noise_hqdn3d);
                            }
                        }
                        if ui.checkbox(&mut self.noise_deband, "Deband").changed() {
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.set_video_filter("lavfi=[deband]", self.noise_deband);
                            }
                        }
                        if ui
                            .checkbox(&mut self.noise_nlmeans, "NLMeans denoise (slow)")
                            .changed()
                        {
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.set_video_filter("lavfi=[nlmeans]", self.noise_nlmeans);
                            }
                        }
                        ui.separator();
                        ui.label("Image orientation");
                        if ui.checkbox(&mut self.flip_x, "Mirror image").changed() {
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.set_video_filter("hflip", self.flip_x);
                            }
                        }
                        if ui.checkbox(&mut self.flip_y, "Flip image").changed() {
                            if let Some(mpv) = self.mpv.clone() {
                                mpv.set_video_filter("vflip", self.flip_y);
                            }
                        }
                        ui.separator();
                        ui.label("Deinterlace");
                        for (index, name) in videowin::DEINTERLACERS.iter().enumerate() {
                            if ui
                                .selectable_label(self.deinterlace == index, *name)
                                .clicked()
                            {
                                self.set_deinterlace(index);
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                if ui.button("Audio").clicked() {
                    self.audio_open = true;
                    ui.close();
                }
            });
            ui.separator();

            ui.label("Folder");
            let folder_field = ui.add(
                egui::TextEdit::singleline(&mut self.folder_input)
                    .desired_width(300.0)
                    .hint_text("/home/you/Music"),
            );
            // Enter does what the Scan button does, so a typed path needs no
            // mouse. A one-line field drops focus on Enter, so `has_focus` is
            // already false by the time the frame closes; `lost_focus` plus the
            // key is how egui itself spells this.
            let entered =
                folder_field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if entered || ui.button("Scan").clicked() {
                actions.push(Action::SelectPath(self.folder_input.trim().to_string()));
            }
            if ui.button("Browse…").clicked() {
                let mut dialog = rfd::FileDialog::new();
                if let Some(dir) = self.music_dir.as_deref() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.folder_input = dir.display().to_string();
                    actions.push(Action::SelectPath(dir.display().to_string()));
                }
            }

            ui.separator();
            ui.label("Search");
            let search_field = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(220.0)
                    .hint_text("title, artist, album"),
            );
            // Enter reports how many tracks the filter kept, the same way the
            // Folder field's Enter commits: a one-line field drops focus on
            // Enter, so `lost_focus` is where the key is still visible. An
            // empty box never searched anything, so it has nothing to report.
            let searched =
                search_field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if searched && !self.search.trim().is_empty() {
                self.search_report = Some(self.visible_songs().len());
                self.search_report_fresh = true;
            }
            if ui.button("✕").on_hover_text("Clear search").clicked() {
                self.search.clear();
            }
            // Mirror the box into the faav cell so any crate depending on
            // nova-faav sees the filter; the field itself lives in this binary.
            nova_faav::search(Some(self.search.clone()));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .selectable_label(self.info_open, "ℹ Info")
                    .on_hover_text("Track info panel (I)")
                    .clicked()
                {
                    self.info_open = !self.info_open;
                }
                ui.separator();
                if let Some((message, _)) = &self.toast {
                    ui.label(RichText::new(message).color(Color32::from_rgb(255, 200, 120)));
                } else if let Some(status) = &self.scan_status {
                    ui.label(RichText::new(status).weak());
                } else if !self.library.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "{} tracks · {} albums",
                            self.library.len(),
                            self.library.albums().len()
                        ))
                        .weak(),
                    );
                }
            });
        });
    }

    pub(super) fn player_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let track = self.engine.current_track(&self.library);
        let position = self.engine.position();
        let total = self.engine.duration(&self.library);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            // Cover thumbnail.
            cover_thumb(ui, track.and_then(|t| self.covers.get(t.id)), 72.0);

            ui.vertical(|ui| {
                ui.add_space(4.0);
                match track {
                    Some(track) => {
                        ui.label(
                            RichText::new(truncate(&track.title, 46))
                                .strong()
                                .size(16.0),
                        );
                        ui.label(RichText::new(track.display_artist()).weak());
                    }
                    None => {
                        ui.label(RichText::new("Nothing playing").weak());
                    }
                }
            });

            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.button("⏮").on_hover_text("Previous").clicked() {
                        actions.push(Action::Prev);
                    }
                    let play_label = if self.engine.state() == PlaybackState::Playing {
                        "⏸"
                    } else {
                        "▶"
                    };
                    if ui
                        .button(play_label)
                        .on_hover_text("Play / pause (space)")
                        .clicked()
                    {
                        actions.push(Action::Toggle);
                    }
                    if ui.button("⏭").on_hover_text("Next").clicked() {
                        actions.push(Action::Next);
                    }
                    if ui
                        .selectable_label(self.engine.queue().shuffle(), "🔀")
                        .on_hover_text("Shuffle")
                        .clicked()
                    {
                        actions.push(Action::ToggleShuffle);
                    }
                    if ui
                        .selectable_label(
                            self.engine.queue().repeat() != nova_core::RepeatMode::Off,
                            "🔁",
                        )
                        .on_hover_text(self.engine.queue().repeat().label())
                        .clicked()
                    {
                        actions.push(Action::CycleRepeat);
                    }
                });

                let seconds_total = total.map(|d| d.as_secs_f32()).unwrap_or(0.0).max(0.001);
                let mut seconds_position = position.as_secs_f32().min(seconds_total);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format_clock(position)).size(12.0).weak());
                    let slider = egui::Slider::new(&mut seconds_position, 0.0..=seconds_total)
                        .show_value(false);
                    if ui
                        .add_sized([ui.available_width() - 52.0, 18.0], slider)
                        .changed()
                    {
                        actions.push(Action::Seek(seconds_position));
                    }
                    ui.label(
                        RichText::new(format_clock(total.unwrap_or_default()))
                            .size(12.0)
                            .weak(),
                    );
                });
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut volume = self.engine.volume();
                if ui
                    .add_sized(
                        [120.0, 18.0],
                        egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false),
                    )
                    .changed()
                {
                    actions.push(Action::SetVolume(volume));
                }
                ui.label(RichText::new("🔈").weak());
            });
        });
        ui.add_space(6.0);
    }
}
