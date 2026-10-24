//! In-window video: showing it, sizing it, and the commands from the video-stream picker.

use super::*;

/// How the main window is shown while a video plays: the three states that
/// <kbd>F</kbd> cycles through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WindowMode {
    /// The window as the user sized it, with every bar around the picture.
    Normal,
    /// Borderless full screen: the picture fills the display and the top bar,
    /// transport bar and side panels go away.
    Fullscreen,
    /// A maximised window: it fills the desktop but keeps its title bar and
    /// the bars around the picture.
    Maximized,
}

impl WindowMode {
    /// The next state in the <kbd>F</kbd> cycle:
    /// normal → full screen → maximised → normal.
    pub(super) fn next(self) -> Self {
        match self {
            Self::Normal => Self::Fullscreen,
            Self::Fullscreen => Self::Maximized,
            Self::Maximized => Self::Normal,
        }
    }
}

impl NovaApp {
    /// Puts the main window into `mode`.
    ///
    /// Leaving full screen is always asked for before maximising, so the two
    /// requests never fight over the window.
    pub(super) fn set_window_mode(&mut self, ctx: &egui::Context, mode: WindowMode) {
        match mode {
            WindowMode::Normal => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            }
            WindowMode::Fullscreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            WindowMode::Maximized => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        self.window_mode = mode;
    }

    /// <kbd>F</kbd>: moves to the next [`WindowMode`].
    pub(super) fn cycle_window_mode(&mut self, ctx: &egui::Context) {
        let next = self.window_mode.next();
        self.set_window_mode(ctx, next);
    }

    /// Whether the picture owns the whole screen: a video is up and the
    /// window is in full screen, so every bar is hidden.
    pub(super) fn fullscreen_video(&self) -> bool {
        self.window_mode == WindowMode::Fullscreen && self.video_showing()
    }

    /// Whether the video surface should take over the content area.
    ///
    /// True only while a video is actually loaded: `current_track` keeps
    /// answering with the queue's selection after a stop, so the state is what
    /// decides, or the library would never come back.
    pub(super) fn video_showing(&self) -> bool {
        self.mpv.is_some()
            && self.engine.state() != PlaybackState::Stopped
            && self
                .engine
                .current_track(&self.library)
                .is_some_and(Track::is_video)
    }

    /// Aspect ratio of the video on screen, from its declared frame size.
    pub(super) fn video_aspect(&self) -> f32 {
        if let Some(aspect) = self.aspect_ratio_override {
            return aspect;
        }
        self.engine
            .current_track(&self.library)
            .and_then(|track| track.video)
            .filter(|v| v.width > 0 && v.height > 0)
            .map_or(16.0 / 9.0, |v| v.width as f32 / v.height as f32)
    }

    /// Draws the video, centred in whatever space the panel offers.
    ///
    /// The sidebar is hidden while this runs, so the surface spans the full
    /// width under the top bar.
    pub(super) fn video_view(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let Some(mpv) = self.mpv.clone() else {
            return;
        };
        let aspect = self.video_aspect();
        let panel = ui.max_rect();
        // Zoom scales the picture past the panel, which clips it — the same
        // effect SMPlayer gets by resizing its video widget and re-centring it.
        let size = crate::mpv::fit(panel.size(), aspect) * self.zoom;

        // How far the picture's centre may travel. SMPlayer gates this on
        // `allow_video_movement`: with it off, the picture can only be moved
        // while it reaches past the window, and with it on it can be pushed
        // around freely. The picture never leaves the panel either way, so it
        // cannot be lost.
        let limit = if allow_video_movement() {
            panel.size() / 2.0
        } else {
            egui::vec2(
                (size.x - panel.width()).max(0.0) / 2.0,
                (size.y - panel.height()).max(0.0) / 2.0,
            )
        };
        let pan = egui::vec2(
            self.pan.x.clamp(-limit.x, limit.x),
            self.pan.y.clamp(-limit.y, limit.y),
        );
        let rect = egui::Rect::from_center_size(panel.center() + pan, size);

        // Dragging the picture moves it with the cursor, the same way moving the
        // video widget by hand does in SMPlayer. It answers to the checkbox:
        // unchecked, the picture stays where the buttons left it and a drag
        // writes nothing, so there is no way past the block by hand.
        if allow_video_movement() {
            let drag = ui.interact(rect, ui.id().with("video-pan"), egui::Sense::drag());
            if drag.dragged() {
                self.pan += drag.drag_delta();
            }
        }

        // Render at the on-screen pixel size so the frame is decoded and scaled
        // exactly once, at the size it is displayed.
        let pixels = size * ui.ctx().pixels_per_point();
        match mpv.texture(frame, pixels, aspect) {
            Some(texture) => {
                let sized = egui::load::SizedTexture::new(texture, size);
                egui::Image::new(sized).paint_at(ui, rect);
            }
            None => {
                ui.colored_label(Color32::RED, "video could not be started");
            }
        }
    }

    /// Keeps the external video-stream picker in step with mpv.
    ///
    /// This is what SMPlayer calls *Video → Track*: a container carrying more
    /// than one video stream (multi-angle discs, dual-cam footage) lets you
    /// pick which one mpv shows. The picker runs as its own process, so the
    /// picture stays here and nothing about mpv's GL context is disturbed.
    pub(super) fn video_window(&mut self) {
        if !self.video_open {
            return;
        }
        if self.video_window.is_none() {
            self.video_window = videowin::VideoWindow::spawn();
            if self.video_window.is_none() {
                // Spawning failed; leave the flag off so this is not retried on
                // every frame. Opening it again from the menu tries afresh.
                self.video_open = false;
                return;
            }
        }
        let Some(window) = self.video_window.as_mut() else {
            return;
        };

        // Closing the picker window, or its process dying, closes it here too.
        if window.closed() {
            log::debug!("the video window closed");
            self.video_window = None;
            self.video_open = false;
            return;
        }

        if let Some(mpv) = self.mpv.clone() {
            window.send_tracks(&mpv.video_tracks());
        }
        window.send_state(
            self.flip_x,
            self.flip_y,
            self.zoom,
            self.pan,
            allow_video_movement(),
            self.deinterlace,
            self.aspect_ratio_override,
            self.video_rotation,
            self.noise_hqdn3d,
            self.noise_deband,
            self.noise_nlmeans,
        );
        let commands = window.commands();

        // The window's borrow ends with its last use above, so the app can be
        // changed here.
        for command in commands {
            self.apply_video_command(command);
        }
    }

    /// Puts SMPlayer's chosen deinterlacer on mpv's filter chain, taking the
    /// previous one off first. `Core::changeDeinterlace` does the same, and mpv
    /// matches a removal against the whole parameterised filter, so the string
    /// that went on comes back off unchanged.
    pub(super) fn set_deinterlace(&mut self, choice: usize) {
        if choice >= videowin::DEINTERLACERS.len() || choice == self.deinterlace {
            return;
        }
        if let Some(mpv) = self.mpv.clone() {
            if let Some(previous) = deinterlace_filter(self.deinterlace) {
                mpv.set_video_filter(previous, false);
            }
            if let Some(filter) = deinterlace_filter(choice) {
                mpv.set_video_filter(filter, true);
            }
            log::debug!("deinterlace -> {}", videowin::DEINTERLACERS[choice]);
        }
        self.deinterlace = choice;
    }

    /// Does what the video window asked for. Stdin speaks the same language,
    /// so this is the one place that changes the picture from outside.
    pub(super) fn apply_video_command(&mut self, command: videowin::VideoCommand) {
        match command {
            videowin::VideoCommand::Pick(id) => {
                log::info!("video stream {id} selected");
                if let Some(mpv) = self.mpv.clone() {
                    mpv.select_video_track(&id);
                }
            }
            videowin::VideoCommand::Zoom(zoom) => self.zoom = zoom,
            videowin::VideoCommand::Pan(x, y) => self.pan = egui::vec2(x, y),
            videowin::VideoCommand::MoveAllowed(allowed) => set_allow_video_movement(allowed),
            videowin::VideoCommand::Deinterlace(choice) => self.set_deinterlace(choice),
            videowin::VideoCommand::AspectRatio(ratio) => {
                self.aspect_ratio_override = ratio;
                self.pan = egui::Vec2::ZERO;
            }
            videowin::VideoCommand::Rotate(degrees) => {
                self.video_rotation = degrees.rem_euclid(360);
                if let Some(mpv) = self.mpv.clone() {
                    mpv.set_video_rotation(self.video_rotation);
                }
            }
            videowin::VideoCommand::NoiseFilter { kind, enabled } => {
                let (state, filter) = match kind {
                    0 => (&mut self.noise_hqdn3d, "lavfi=[hqdn3d]"),
                    1 => (&mut self.noise_deband, "lavfi=[deband]"),
                    2 => (&mut self.noise_nlmeans, "lavfi=[nlmeans]"),
                    _ => return,
                };
                if *state != enabled {
                    *state = enabled;
                    if let Some(mpv) = self.mpv.clone() {
                        mpv.set_video_filter(filter, enabled);
                    }
                }
            }
            videowin::VideoCommand::Flip { x, y } => {
                self.flip_x = x;
                self.flip_y = y;
                if let Some(mpv) = self.mpv.clone() {
                    mpv.set_video_filter("hflip", x);
                    mpv.set_video_filter("vflip", y);
                }
            }
        }
    }
}
