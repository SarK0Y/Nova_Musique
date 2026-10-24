//! Songs → To NFM: the conversion window and the background conversion.

use super::*;

/// The "threads" field's default: one group per core, as before the field
/// existed.
pub(super) fn default_nfm_threads() -> String {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .to_string()
}

impl NovaApp {
    /// "Songs → To NFM": picks a folder, takes the `-set-rate` value and, on
    /// Go, converts every file of the current playlist (or the whole library)
    /// to `.nfm` on a background thread.
    pub(super) fn nfm_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("To NFM")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(RichText::new("NFA5").strong().size(18.0));

                ui.horizontal(|ui| {
                    ui.label("Save to");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.nfm_dir)
                            .desired_width(320.0)
                            .hint_text("folder for the .nfm files"),
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("-set-rate");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.nfm_rate)
                            .desired_width(80.0)
                            .hint_text("bits/sample"),
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("threads");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.nfm_threads)
                            .desired_width(60.0)
                            .hint_text("cores"),
                    );
                });

                let running = self.nfm_rx.is_some();
                let ready = !self.nfm_dir.trim().is_empty();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!running && ready, egui::Button::new("Go"))
                        .clicked()
                    {
                        self.start_nfm_conversion();
                    }
                    if ui
                        .add_enabled(running, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        self.cancel_nfm_conversion();
                    }
                });

                if let Some((done, total)) = self.nfm_progress {
                    ui.label(format!(
                        "Converting {done}/{total}…  {}",
                        format_elapsed(self.nfm_started.map_or(Duration::ZERO, |at| at.elapsed()))
                    ));
                } else if let Some(elapsed) = self.nfm_elapsed {
                    ui.label(format!("Last run took {}", format_elapsed(elapsed)));
                }
                if !self.nfm_active.is_empty() {
                    ui.label(RichText::new(self.nfm_active.join(", ")).weak());
                }
            });
        if !open {
            self.nfm_open = false;
        }
    }

    /// Converts the current playlist (every visible track's file) to `.nfm` in
    /// the chosen folder, on a thread so the window stays live.
    pub(super) fn start_nfm_conversion(&mut self) {
        let typed = self.nfm_dir.trim().to_string();
        if typed.is_empty() {
            self.toast("Set a folder in Save to first");
            return;
        }
        let dir = PathBuf::from(&typed);
        if !dir.is_dir() {
            self.toast(format!("Not a folder: {typed}"));
            return;
        }
        if self.config.path_to_save_converted_files.as_deref() != Some(dir.as_path()) {
            self.config.path_to_save_converted_files = Some(dir.clone());
            self.config.save();
        }
        let rate = match self.nfm_rate.trim().parse::<f64>() {
            Ok(rate) if rate.is_finite() && rate > 0.0 => rate,
            _ => {
                self.toast("-set-rate must be a number greater than 0");
                return;
            }
        };
        let threads = match self.nfm_threads.trim().parse::<usize>() {
            Ok(threads) if threads > 0 => threads,
            _ => {
                self.toast("threads must be a whole number greater than 0");
                return;
            }
        };

        // With a playlist loaded, "current playlist" is exactly its tracks;
        // otherwise the whole library. Reading tags happens on the thread.
        let paths: Vec<PathBuf> = match &self.loaded_playlist {
            Some((_, ids)) => ids
                .iter()
                .filter_map(|id| self.library.track(*id).map(|t| t.path.clone()))
                .collect(),
            None => self
                .library
                .tracks()
                .iter()
                .map(|track| track.path.clone())
                .collect(),
        };
        if paths.is_empty() {
            self.toast("Nothing to convert");
            return;
        }

        let total = paths.len();
        // One thread per group: the files are split into as many contiguous
        // groups as the user asked for (never more groups than files).
        let groups = threads.clamp(1, total);
        let chunk = total.div_ceil(groups);
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.nfm_rx = Some(rx);
        self.nfm_progress = Some((0, total));
        self.nfm_errors = 0;
        self.nfm_started = Some(Instant::now());
        self.nfm_elapsed = None;
        self.nfm_active.clear();
        self.nfm_cancel = Some(Arc::clone(&cancel));
        // NFA5 stores `bits` in its header but the grid is what matters; the
        // rate control is what the user dials.
        let bits = 6u16;
        // Shared across the group threads, so `done` counts the whole run
        // rather than one group, and the last message to arrive is the only
        // one that reads `total`.
        let done = Arc::new(AtomicUsize::new(0));
        for group in paths.chunks(chunk) {
            let group = group.to_vec();
            let tx = tx.clone();
            let done = Arc::clone(&done);
            let cancel = Arc::clone(&cancel);
            let dir = dir.clone();
            std::thread::spawn(move || {
                for src in group {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    if tx
                        .send(NfmMsg::Started(
                            src.file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| src.display().to_string()),
                        ))
                        .is_err()
                    {
                        return;
                    }
                    // A codec panic must not strand the run: count the file
                    // as a failure and keep going, so `done` still reaches
                    // `total` and the window finishes.
                    let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        metadata::convert_to_nfm(&src, &dir, rate, bits)
                    }))
                    .map_err(|_| format!("panic while converting {}", src.display()))
                    .and_then(|result| result.map_err(|err| err.to_string()))
                    .err();
                    let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if tx.send(NfmMsg::Progress { done, total, error }).is_err() {
                        return;
                    }
                }
            });
        }
        // The receivers' end closes only once every group's sender is gone.
        drop(tx);
        self.toast(format!("Converting {total} track(s) to NFM…"));
    }

    /// Asks the running conversion to stop: the group threads stand down at
    /// the next file boundary, so a file mid-conversion is still written.
    pub(super) fn cancel_nfm_conversion(&mut self) {
        if let Some(cancel) = &self.nfm_cancel {
            cancel.store(true, Ordering::Relaxed);
            self.toast("Stopping the conversion…");
        }
    }

    /// Drains the conversion thread's messages, updating the progress shown in
    /// the To NFM window and toasting when the run finishes.
    pub(super) fn drain_nfm(&mut self) {
        let Some(rx) = &self.nfm_rx else { return };
        let mut messages = Vec::new();
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(msg) => messages.push(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        for msg in messages {
            match msg {
                NfmMsg::Started(name) => {
                    self.nfm_active.push(name);
                }
                NfmMsg::Progress { done, total, error } => {
                    // Out-of-order arrivals: keep the highest count, so the
                    // display only ever moves forward.
                    if self.nfm_progress.is_some_and(|(seen, _)| done > seen)
                        || self.nfm_progress.is_none()
                    {
                        self.nfm_progress = Some((done, total));
                    }
                    if let Some(err) = error {
                        self.nfm_errors += 1;
                        log::warn!("NFM conversion failed: {err}");
                    }
                    self.nfm_active.pop();
                    if done == total {
                        let errors = self.nfm_errors;
                        let cancelled = self
                            .nfm_cancel
                            .as_ref()
                            .is_some_and(|flag| flag.load(Ordering::Relaxed));
                        self.nfm_elapsed = self.nfm_started.map(|at| at.elapsed());
                        self.nfm_finish();
                        self.toast(if cancelled {
                            format!("Conversion cancelled ({errors} failure(s))")
                        } else if errors == 0 {
                            format!("Converted {total} track(s) to NFM")
                        } else {
                            format!(
                                "Converted {} of {total} to NFM, {errors} failed",
                                total - errors
                            )
                        });
                    }
                }
            }
        }

        // The threads ended without a last message (cancelled or panicked):
        // stop showing progress rather than leaving the window stuck on
        // "Converting".
        if disconnected && self.nfm_rx.is_some() {
            let errors = self.nfm_errors;
            let cancelled = self
                .nfm_cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed));
            self.nfm_elapsed = self.nfm_started.map(|at| at.elapsed());
            self.nfm_finish();
            self.toast(if cancelled {
                format!("Conversion cancelled ({errors} failure(s))")
            } else {
                format!("Conversion stopped early ({errors} failure(s))")
            });
        }
    }

    /// Clears everything a finished (or cancelled) conversion leaves behind.
    pub(super) fn nfm_finish(&mut self) {
        self.nfm_progress = None;
        self.nfm_errors = 0;
        self.nfm_started = None;
        self.nfm_active.clear();
        self.nfm_cancel = None;
        self.nfm_rx = None;
    }
}
