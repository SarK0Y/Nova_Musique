//! The "Jump focus" window: pick a main-window element and jump to it.

use super::*;

/// Places in the main window the Jump focus window can land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpTarget {
    Folder,
    Search,
    Songs,
    Albums,
    Artists,
    Queue,
    Info,
}

impl JumpTarget {
    pub(super) const ALL: [JumpTarget; 7] = [
        JumpTarget::Folder,
        JumpTarget::Search,
        JumpTarget::Songs,
        JumpTarget::Albums,
        JumpTarget::Artists,
        JumpTarget::Queue,
        JumpTarget::Info,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            JumpTarget::Folder => "Folder field",
            JumpTarget::Search => "Search field",
            JumpTarget::Songs => "Songs view",
            JumpTarget::Albums => "Albums view",
            JumpTarget::Artists => "Artists view",
            JumpTarget::Queue => "Queue view",
            JumpTarget::Info => "Track info panel",
        }
    }
}

impl NovaApp {
    /// Draws the picker while it is open. The list scrolls with the wheel and
    /// with ↑/↓; Enter or a click jumps to the highlighted element.
    pub(super) fn jump_window(&mut self, ctx: &egui::Context) {
        let (down, up, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
            )
        });
        let selected = step_choice(self.jump_selected, JumpTarget::ALL.len(), down, up);
        self.jump_selected = selected;

        let mut chosen = None;
        let mut open = true;
        egui::Window::new("Jump focus")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .default_width(260.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("↑/↓ or the mouse wheel · Enter to jump")
                        .weak()
                        .size(12.0),
                );
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for (index, target) in JumpTarget::ALL.iter().enumerate() {
                            let response =
                                ui.selectable_label(index == selected, target.label());
                            if index == selected
                                && (down || up)
                                && !ui.clip_rect().contains_rect(response.rect)
                            {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                chosen = Some(*target);
                            }
                        }
                    });
            });

        if !open {
            self.jump_open = false;
        }
        if enter {
            chosen = Some(JumpTarget::ALL[selected]);
        }
        if let Some(target) = chosen {
            self.jump_to(target);
            self.jump_open = false;
        }
    }

    fn jump_to(&mut self, target: JumpTarget) {
        match target {
            JumpTarget::Folder | JumpTarget::Search => self.jump_focus = Some(target),
            JumpTarget::Songs => self.view = View::Songs,
            JumpTarget::Albums => self.view = View::Albums,
            JumpTarget::Artists => self.view = View::Artists,
            JumpTarget::Queue => self.view = View::Queue,
            JumpTarget::Info => self.info_open = true,
        }
    }
}

/// Moves the highlight one row, stopping at both ends.
fn step_choice(selected: usize, len: usize, down: bool, up: bool) -> usize {
    let last = len.saturating_sub(1);
    if down {
        (selected + 1).min(last)
    } else if up {
        selected.saturating_sub(1)
    } else {
        selected.min(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlight_stops_at_both_ends() {
        assert_eq!(step_choice(0, 7, false, true), 0);
        assert_eq!(step_choice(3, 7, true, false), 4);
        assert_eq!(step_choice(6, 7, true, false), 6);
    }
}
