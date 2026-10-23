//! The Audio window (SMPlayer's audio controls).

use super::*;

impl NovaApp {
    /// Separate SMPlayer-inspired audio control window.
    pub(super) fn audio_window(&mut self, ctx: &egui::Context) {
        if !self.audio_open {
            return;
        }
        let viewport_id = egui::ViewportId::from_hash_of("nova-audio-window");
        let builder = egui::ViewportBuilder::default()
            .with_title("Nova Musique — Audio")
            .with_inner_size([420.0, 620.0])
            .with_min_inner_size([340.0, 420.0]);
        ctx.show_viewport_immediate(viewport_id, builder, |viewport_ctx, _class| {
            if viewport_ctx.input(|i| i.viewport().close_requested()) {
                self.audio_open = false;
                return;
            }
            egui::CentralPanel::default().show(viewport_ctx, |ui| {
                ui.heading("Audio");
                ui.label(RichText::new("Audio stream, processing and synchronization").weak());
                ui.separator();

                ui.collapsing("1. Pick stream", |ui| {
                    if let Some(mpv) = self.mpv.clone() {
                        let tracks = mpv.audio_tracks();
                        if tracks.is_empty() {
                            ui.label(RichText::new("No audio streams reported by mpv.").weak());
                        } else {
                            for track in tracks {
                                if ui.selectable_label(track.selected,
                                    format!("{} — {}", track.id, track.title)).clicked() {
                                    mpv.select_audio_track(&track.id);
                                }
                            }
                        }
                    } else {
                        ui.label("libmpv is unavailable.");
                    }
                });
                ui.separator();

                ui.collapsing("2. Filters", |ui| {
                    if ui.checkbox(&mut self.audio_karaoke, "Karaoke (voice removal)").changed() {
                        if let Some(mpv) = self.mpv.clone() {
                            mpv.set_audio_filter("karaoke", "karaoke", self.audio_karaoke);
                        }
                    }
                    if ui.checkbox(&mut self.audio_tempo, "Tempo scaling (preserve pitch)").changed() {
                        if let Some(mpv) = self.mpv.clone() {
                            mpv.set_audio_filter("tempo", "scaletempo", self.audio_tempo);
                        }
                    }
                    if ui.checkbox(&mut self.audio_normalize, "Volume normalization").changed() {
                        if let Some(mpv) = self.mpv.clone() {
                            mpv.set_audio_filter("normalize", "lavfi=[dynaudnorm]", self.audio_normalize);
                        }
                    }
                });
                ui.separator();

                ui.collapsing("3. Channels", |ui| {
                    egui::ComboBox::from_id_salt("nova-audio-channels")
                        .selected_text(&self.audio_channels)
                        .show_ui(ui, |ui| {
                            for value in ["auto", "stereo", "mono", "5.1", "7.1"] {
                                if ui.selectable_value(&mut self.audio_channels, value.to_string(), value).changed() {
                                    if let Some(mpv) = self.mpv.clone() { mpv.set_audio_channels(&self.audio_channels); }
                                }
                            }
                        });
                    ui.label(RichText::new("Available layouts depend on the audio stream and output.").weak().size(11.0));
                });
                ui.separator();

                ui.collapsing("4. Send sound to", |ui| {
                    // SMPlayer's BaseGuiPlus::updateSendAudioMenu() uses mpv's
                    // audio-device-list and passes the backend-qualified device
                    // name to changeAO(). libmpv exposes the same list directly.
                    let devices = self.mpv.as_ref().map(|mpv| mpv.audio_devices()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("nova-audio-output-device")
                        .selected_text(if self.audio_device == "auto" {
                            "Default audio device".to_owned()
                        } else {
                            devices.iter().find(|device| device.name == self.audio_device)
                                .map(|device| device.description.clone())
                                .unwrap_or_else(|| self.audio_device.clone())
                        })
                        .width(300.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_value(&mut self.audio_device, "auto".to_owned(), "Default audio device").changed() {
                                if let Some(mpv) = self.mpv.clone() { mpv.set_audio_device("auto"); }
                            }
                            for device in &devices {
                                let label = if device.is_default {
                                    format!("{} (backend default)", device.description)
                                } else {
                                    device.description.clone()
                                };
                                if ui.selectable_value(&mut self.audio_device, device.name.clone(), label).changed() {
                                    if let Some(mpv) = self.mpv.clone() { mpv.set_audio_device(&self.audio_device); }
                                }
                            }
                        });
                    if devices.is_empty() {
                        ui.label(RichText::new("mpv has not reported any named output devices. The default device is still available.").weak().size(11.0));
                    }
                    ui.collapsing("Manual device ID…", |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.audio_device)
                                .desired_width(250.0)
                                .hint_text("auto or backend/device-name"));
                            if ui.button("Apply").clicked() {
                                if let Some(mpv) = self.mpv.clone() { mpv.set_audio_device(self.audio_device.trim()); }
                            }
                        });
                    });
                    ui.label(RichText::new("Device list is supplied by mpv; the active backend determines which outputs appear.").weak().size(11.0));
                });
                ui.separator();

                ui.collapsing("5. Load external audio", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.external_audio_input)
                            .desired_width(250.0).hint_text("/path/to/audio-file"));
                        if ui.button("Browse…").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Audio", &["mp3", "flac", "ogg", "opus", "wav", "m4a", "aac", "wma"])
                                .pick_file() {
                                self.external_audio_input = path.display().to_string();
                            }
                        }
                    });
                    if ui.button("Load and select external audio").clicked() {
                        let path = PathBuf::from(self.external_audio_input.trim());
                        if path.is_file() {
                            if let Some(mpv) = self.mpv.clone() { mpv.load_external_audio(&path); }
                        } else {
                            self.toast = Some(("Choose an existing audio file first".to_string(), Instant::now()));
                        }
                    }
                });
                ui.separator();

                ui.collapsing("6. Modes", |ui| {
                    egui::ComboBox::from_id_salt("nova-audio-mode")
                        .selected_text(&self.audio_mode)
                        .show_ui(ui, |ui| {
                            // Match SMPlayer's Audio -> Stereo mode menu labels and choices.
                            for mode in ["Stereo", "Left channel", "Right channel", "Mono", "Reverse channels"] {
                                if ui.selectable_value(&mut self.audio_mode, mode.to_string(), mode).changed() {
                                    self.apply_audio_mode();
                                }
                            }
                        });
                });
                ui.separator();

                ui.collapsing("7. Set delay", |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.audio_delay_ms)
                            .speed(10).range(-10000..=10000).suffix(" ms"));
                        if ui.button("Apply delay").clicked() {
                            if let Some(mpv) = self.mpv.clone() { mpv.set_audio_delay_ms(self.audio_delay_ms); }
                        }
                        if ui.button("Reset").clicked() {
                            self.audio_delay_ms = 0;
                            if let Some(mpv) = self.mpv.clone() { mpv.set_audio_delay_ms(0); }
                        }
                    });
                });
            });
        });
    }

    /// Applies SMPlayer's *Audio → Stereo mode* to both playback paths: mpv
    /// gets a `pan` lavfi filter swap (with a position-preserving reload),
    /// rodio gets the shared cell that rewrites frames live.
    pub(super) fn apply_audio_mode(&mut self) {
        if let Some(mpv) = self.mpv.clone() {
            mpv.set_audio_mode(&self.audio_mode);
        }
        self.engine
            .set_stereo_mode(crate::mpv::stereo_mode(&self.audio_mode));
    }
}
