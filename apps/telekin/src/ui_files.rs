//! The file panes: this computer on the left, the robot on the right.
//!
//! Both sides are drawn by the same widget, because they are the same thing to
//! the person using them — the only asymmetry is that one side's listing
//! arrives over the network and can be late or fail.
//!
//! The layout follows the two-pane convention every file client has used for
//! thirty years. That is not nostalgia: it is the one arrangement where "copy
//! this there" is a single gesture with an unambiguous direction, which
//! matters when *there* is a robot in another room.

use egui::RichText;
use telekin_proto::{DirEntry, DirListing};

use crate::files;
use crate::theme;
use crate::ui::ViewerApp;

/// Width of the strip between the panes that holds the copy arrows.
const ARROW_COLUMN: f32 = 78.0;
/// Height reserved under the robot list for its folder controls.
const REMOTE_CONTROLS: f32 = 56.0;
/// The narrowest either pane may be squeezed to before the other has to yield.
const MIN_PANE: f32 = 260.0;
/// Shown when Close is pressed on a file with unsaved changes.
const CLOSE_WARNING: &str =
    "This file has changes that are not on the robot. Press Close again to discard them.";

impl ViewerApp {
    pub(crate) fn files_screen(&mut self, ui: &mut egui::Ui) {
        // The local side is read here rather than on a thread: it is a local
        // directory, and a spinner for something that takes a millisecond is
        // worse than the millisecond.
        if self.local.is_none() && self.local_error.is_none() {
            self.reload_local();
        }
        self.ensure_remote_listed();
        self.pump_transfers();

        // The editor takes the whole screen when it is open. Splitting the
        // window between an editor and two file lists would leave all three
        // too small to use, and while you are editing a file the panes behind
        // it are not what you are doing.
        if self.with_files(|st| st.editor.is_some()).unwrap_or(false) {
            self.editor_screen(ui);
            return;
        }

        // Everything below this line is paper. The switch happens once, here,
        // so no widget inside has to know which surface it is on.
        theme::dense_list(ui);

        egui::Panel::bottom("transfers")
            .frame(theme::panel(theme::SURFACE, 12))
            .show(ui, |ui| self.transfer_list(ui));

        // Three panels, not two panels and whatever is left over. The arrows
        // used to live in the central panel and were squeezed to a ten-pixel
        // sliver — the one control that moves files must have a width of its
        // own, not the remainder of someone else's arithmetic.
        // A resizable panel remembers its width in pixels, so a pane dragged
        // wide on a maximised window kept that width when the window was made
        // small and squeezed the robot down to a strip. Tie the ceiling to
        // what is actually on screen.
        let room = ui.available_width();
        egui::Panel::left("local_pane")
            .default_size((room * 0.42).max(240.0))
            // Two ceilings, whichever bites first: leave the robot a workable
            // strip, and never let one side take much more than half. The
            // first alone still allowed a 620/300 split on a small window,
            // which is not two panes so much as a pane and a margin.
            .min_size(200.0)
            .max_size(
                (room - ARROW_COLUMN - MIN_PANE)
                    .min(room * 0.6)
                    .max(200.0),
            )
            .resizable(true)
            .frame(theme::panel(theme::BASE, 14))
            .show(ui, |ui| self.local_pane(ui));

        egui::Panel::left("arrow_column")
            .default_size(ARROW_COLUMN)
            .resizable(false)
            .frame(theme::panel(theme::SURFACE, 6))
            .show(ui, |ui| self.arrow_column(ui));

        egui::CentralPanel::default_margins()
            .frame(theme::panel(theme::BASE, 14))
            .show(ui, |ui| self.remote_pane(ui));
    }

    /// Editing one robot file in place.
    ///
    /// The point is not to replace an editor; it is to save a trip. Changing
    /// one number in a launch file should not mean starting a video stream,
    /// waiting for a desktop to draw, and driving a mouse from another room.
    fn editor_screen(&mut self, ui: &mut egui::Ui) {
        let Some(editor) = self.with_files(|st| st.editor.clone()).flatten() else {
            return;
        };

        egui::Panel::top("editor_bar")
            .frame(theme::panel(theme::SURFACE, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_sized([110.0, theme::CONTROL_HEIGHT], egui::Button::new("Close"))
                        .clicked()
                    {
                        self.close_editor();
                    }
                    ui.add_space(8.0);

                    ui.label(
                        RichText::new(editor.name())
                            .size(17.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    // An unsaved file says so in words, not with a dot nobody
                    // has been taught to read.
                    if editor.dirty() {
                        ui.label(
                            RichText::new("edited, not saved")
                                .size(14.0)
                                .color(ui.visuals().warn_fg_color),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_save = editor.dirty() && !editor.saving && !editor.loading;
                        let saved = ui
                            .add_enabled_ui(can_save, |ui| {
                                ui.add_sized(
                                    [130.0, theme::CONTROL_HEIGHT],
                                    theme::primary_button("Save"),
                                )
                            })
                            .inner
                            .on_hover_text("Write this back to the robot (Ctrl+S)")
                            .clicked();
                        if saved {
                            self.save_editor();
                        }
                        if editor.saving {
                            ui.spinner();
                        }
                    });
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(&editor.path)
                        .monospace()
                        .size(13.0)
                        .color(theme::MUTED),
                );
            });

        egui::CentralPanel::default_margins()
            .frame(theme::panel(theme::BASE, 12))
            .show(ui, |ui| {
                if let Some(e) = &editor.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                    ui.add_space(8.0);
                }
                if editor.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("reading the file...")
                                .size(15.0)
                                .color(theme::MUTED),
                        );
                    });
                    return;
                }

                // Ctrl+S, because every editor has it and nobody will look for
                // the button twice.
                let save_pressed =
                    ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));

                let mut text = editor.text.clone();
                let response = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_sized(
                            ui.available_size(),
                            egui::TextEdit::multiline(&mut text)
                                .font(egui::TextStyle::Monospace)
                                .code_editor()
                                .desired_width(f32::INFINITY),
                        )
                    })
                    .inner;

                if response.changed() {
                    self.with_files(|st| {
                        if let Some(open) = st.editor.as_mut() {
                            open.text = text;
                        }
                    });
                }
                if save_pressed && editor.dirty() && !editor.saving {
                    self.save_editor();
                }
            });
    }

    fn save_editor(&mut self) {
        let Some((path, text)) = self
            .with_files(|st| st.editor.as_ref().map(|e| (e.path.clone(), e.text.clone())))
            .flatten()
        else {
            return;
        };
        self.send_file_command(files::Command::Save { path, text });
    }

    /// Leave the editor.
    ///
    /// Refuses once when there are unsaved changes, so a misplaced click does
    /// not throw away an edit made over a slow link; the second press goes
    /// through. A modal dialog would be the usual answer, but this pane is
    /// often driven one-handed and a dialog is one more thing to aim at.
    fn close_editor(&mut self) {
        let dirty = self
            .with_files(|st| st.editor.as_ref().is_some_and(|e| e.dirty()))
            .unwrap_or(false);
        if dirty && !self.close_confirmed {
            self.close_confirmed = true;
            self.with_files(|st| {
                if let Some(editor) = st.editor.as_mut() {
                    editor.error = Some(CLOSE_WARNING.to_string());
                }
            });
            return;
        }
        self.close_confirmed = false;
        self.with_files(|st| st.editor = None);
    }

    /// The column of arrows between the two lists.
    ///
    /// A file manager's one irreversible-ish action is "copy this over there",
    /// so the control for it is a large arrow pointing at the pane it will
    /// land in, and it is greyed out until something is actually selected.
    fn arrow_column(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            // Sit the pair a little above the middle of the lists: that is
            // where the eye goes when comparing two columns, and it stays put
            // as the window is resized.
            ui.add_space((ui.available_height() * 0.30).max(60.0));

            let can_send = self.local_pick.is_some() && self.remote_dir().is_some();
            let send = ui
                .add_enabled_ui(can_send, |ui| {
                    theme::arrow_button(
                        ui,
                        egui::vec2(54.0, 44.0),
                        theme::Arrow::Right,
                        "Copy the selected file to the robot",
                    )
                })
                .inner
                .clicked();
            if send {
                self.upload_selected();
            }

            ui.add_space(10.0);

            let can_get = self.remote_pick.is_some();
            let get = ui
                .add_enabled_ui(can_get, |ui| {
                    theme::arrow_button(
                        ui,
                        egui::vec2(54.0, 44.0),
                        theme::Arrow::Left,
                        "Copy the selected file to this computer",
                    )
                })
                .inner
                .clicked();
            if get {
                self.download_selected();
            }
        });
    }

    fn reload_local(&mut self) {
        match files::local_listing(&self.local_dir) {
            Ok(listing) => {
                self.local_dir = std::path::PathBuf::from(&listing.path);
                self.local = Some(listing);
                self.local_error = None;
            }
            Err(e) => {
                self.local = None;
                self.local_error = Some(format!("{}: {e}", self.local_dir.display()));
            }
        }
    }

    /// Start queued transfers, a few at a time, and refresh both panes once
    /// everything has landed so the copied files are simply there.
    fn pump_transfers(&mut self) {
        /// Transfers in flight at once. Enough to keep a WiFi link busy;
        /// few enough that a robot's SD card is not asked for two hundred
        /// files simultaneously.
        const MAX_INFLIGHT: usize = 4;

        let (ready, busy) = self
            .with_files(|st| (st.take_ready(MAX_INFLIGHT), st.busy()))
            .unwrap_or_default();
        for cmd in ready {
            self.send_file_command(cmd);
        }

        if self.transfers_were_busy && !busy {
            self.reload_local();
            if let Some(dir) = self.remote_dir() {
                self.send_file_command(files::Command::List { path: dir });
            }
        }
        self.transfers_were_busy = busy;
    }

    /// Ask for the robot's home directory the first time the pane is opened.
    fn ensure_remote_listed(&mut self) {
        let needed = self
            .with_files(|st| st.listing.is_none() && !st.loading && st.error.is_none())
            .unwrap_or(false);
        if needed {
            self.send_file_command(files::Command::List { path: String::new() });
        }
    }

    /// Do something with the session's shared file state.
    ///
    /// A closure rather than a getter: the state lives behind an `Arc` owned
    /// by the session, and a lock taken on a temporary clone of that `Arc`
    /// would not outlive the statement it was taken in. Returns `None` when
    /// there is no session, which the panes read as "nothing to show".
    fn with_files<R>(&self, f: impl FnOnce(&mut files::State) -> R) -> Option<R> {
        let session = self.session.as_ref()?;
        let shared = session.files.clone();
        let mut state = shared.lock().expect("file state poisoned");
        Some(f(&mut state))
    }

    pub(crate) fn send_file_command(&mut self, command: files::Command) {
        let Some(session) = &self.session else { return };
        let Some(tx) = session.files() else { return };
        let _ = tx.send(command);
    }

    // -----------------------------------------------------------------------
    // This computer
    // -----------------------------------------------------------------------

    fn local_pane(&mut self, ui: &mut egui::Ui) {
        ui.scope(|ui| {
            theme::section(ui, "This computer");

            let mut typed = self
                .local
                .as_ref()
                .map(|l| l.path.clone())
                .unwrap_or_else(|| self.local_dir.display().to_string());
            ui.horizontal(|ui| {
                let up = self.local_dir.parent().map(|p| p.to_path_buf());
                if ui
                    .add_enabled_ui(up.is_some(), |ui| {
                        theme::arrow_button(
                            ui,
                            egui::vec2(38.0, theme::CONTROL_HEIGHT),
                            theme::Arrow::Up,
                            "Go to the folder above",
                        )
                    })
                    .inner
                    .clicked()
                {
                    if let Some(parent) = up {
                        self.local_dir = parent;
                        self.local_pick = None;
                        self.reload_local();
                    }
                }
                let response = ui.add_sized(
                    [ui.available_width(), theme::CONTROL_HEIGHT],
                    egui::TextEdit::singleline(&mut typed)
                        .font(egui::TextStyle::Monospace)
                        .vertical_align(egui::Align::Center),
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.local_dir = std::path::PathBuf::from(typed.trim());
                    self.local_pick = None;
                    self.reload_local();
                }
            });

            if let Some(e) = &self.local_error {
                ui.add_space(6.0);
                ui.colored_label(ui.visuals().error_fg_color, e);
            }

            ui.add_space(8.0);
            let listing = self.local.clone();
            let mut open: Option<String> = None;
            if let Some(listing) = &listing {
                let picked = self.local_pick.clone();
                let height = ui.available_height();
                let (clicked, opened) =
                    entry_list(ui, "local", listing, picked.as_deref(), height);
                if let Some(name) = clicked {
                    self.local_pick = Some(name);
                }
                open = opened;
            }
            if let Some(name) = open {
                self.local_dir = self.local_dir.join(name);
                self.local_pick = None;
                self.reload_local();
            }
        });
    }

    fn upload_selected(&mut self) {
        let (Some(name), Some(remote_dir)) = (self.local_pick.clone(), self.remote_dir()) else {
            return;
        };
        let local = self.local_dir.join(&name);
        if local.is_dir() {
            self.send_file_command(files::Command::UploadTree {
                local,
                remote_parent: remote_dir,
            });
            return;
        }
        let remote = files::remote_join(&remote_dir, &name);
        self.send_file_command(files::Command::Upload { local, remote, batch: None });
    }

    // -----------------------------------------------------------------------
    // The robot
    // -----------------------------------------------------------------------

    fn remote_dir(&self) -> Option<String> {
        self.with_files(|st| st.listing.as_ref().map(|l| l.path.clone()))
            .flatten()
    }

    fn remote_pane(&mut self, ui: &mut egui::Ui) {
        let (listing, loading, error) = self
            .with_files(|st| (st.listing.clone(), st.loading, st.error.clone()))
            .unwrap_or((None, false, None));

        ui.scope(|ui| {
            theme::section(ui, "Robot");

            ui.horizontal(|ui| {
                let up = listing.as_ref().and_then(|l| l.parent.clone());
                if ui
                    .add_enabled_ui(up.is_some(), |ui| {
                        theme::arrow_button(
                            ui,
                            egui::vec2(38.0, theme::CONTROL_HEIGHT),
                            theme::Arrow::Up,
                            "Go to the folder above",
                        )
                    })
                    .inner
                    .clicked()
                {
                    if let Some(parent) = up {
                        self.remote_pick = None;
                        self.send_file_command(files::Command::List { path: parent });
                    }
                }
                let mut path = listing.as_ref().map(|l| l.path.clone()).unwrap_or_default();
                let response = ui.add_sized(
                    [ui.available_width(), theme::CONTROL_HEIGHT],
                    egui::TextEdit::singleline(&mut path)
                        .hint_text("/home")
                        .font(egui::TextStyle::Monospace)
                        .vertical_align(egui::Align::Center),
                );
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.remote_pick = None;
                    self.send_file_command(files::Command::List { path });
                }
            });

            if loading {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("reading the robot…")
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                });
            }
            if let Some(e) = &error {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                });
                if ui.small_button("Dismiss").clicked() {
                    self.with_files(|st| st.error = None);
                }
            }

            ui.add_space(8.0);
            let mut open: Option<String> = None;
            if let Some(listing) = &listing {
                let picked = self.remote_pick.clone();
                // Leave room for the folder controls under the list — two
                // rows of them when the pane is too narrow for one.
                let reserved = if ui.available_width() > 420.0 {
                    REMOTE_CONTROLS
                } else {
                    REMOTE_CONTROLS * 2.0
                };
                let height = ui.available_height() - reserved;
                let (clicked, opened) =
                    entry_list(ui, "remote", listing, picked.as_deref(), height);
                if let Some(name) = clicked {
                    self.remote_pick = Some(name);
                }
                open = opened;
            }
            if let (Some(name), Some(listing)) = (open, &listing) {
                let path = files::remote_join(&listing.path, &name);
                let is_dir = listing.entries.iter().any(|e| e.name == name && e.is_dir);
                self.remote_pick = None;
                if is_dir {
                    self.send_file_command(files::Command::List { path });
                } else {
                    // A double-click on a file opens it, the way it does in
                    // every file client that talks to a remote machine.
                    self.close_confirmed = false;
                    self.send_file_command(files::Command::Open { path });
                }
            }

            ui.add_space(10.0);
            // Two rows when the pane is narrow, one when there is room. A
            // single row that does not fit does not shrink politely — the
            // last control simply leaves the window, which is how Delete
            // ended up half off the screen.
            let roomy = ui.available_width() > 420.0;
            let buttons = |app: &mut Self, ui: &mut egui::Ui| {
                let named = !app.new_folder.trim().is_empty();
                if ui
                    .add_enabled_ui(named, |ui| {
                        ui.add_sized([110.0, theme::CONTROL_HEIGHT], egui::Button::new("New folder"))
                    })
                    .inner
                    .clicked()
                {
                    app.make_remote_folder();
                }
                if ui
                    .add_enabled_ui(app.remote_pick.is_some(), |ui| {
                        ui.add_sized([76.0, theme::CONTROL_HEIGHT], egui::Button::new("Delete"))
                    })
                    .inner
                    .clicked()
                {
                    app.remove_selected();
                }
            };

            if roomy {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [ui.available_width() - 200.0, theme::CONTROL_HEIGHT],
                        egui::TextEdit::singleline(&mut self.new_folder)
                            .hint_text("new folder name")
                            .vertical_align(egui::Align::Center),
                    );
                    buttons(self, ui);
                });
            } else {
                ui.add_sized(
                    [ui.available_width(), theme::CONTROL_HEIGHT],
                    egui::TextEdit::singleline(&mut self.new_folder)
                        .hint_text("new folder name")
                        .vertical_align(egui::Align::Center),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| buttons(self, ui));
            }
        });
    }

    fn download_selected(&mut self) {
        let (Some(name), Some(listing)) = (
            self.remote_pick.clone(),
            self.with_files(|st| st.listing.clone()).flatten(),
        ) else {
            return;
        };
        let remote = files::remote_join(&listing.path, &name);
        if listing.entries.iter().any(|e| e.name == name && e.is_dir) {
            self.send_file_command(files::Command::DownloadTree {
                remote,
                local_parent: self.local_dir.clone(),
            });
            return;
        }
        let local = self.local_dir.join(&name);
        self.send_file_command(files::Command::Download { remote, local, batch: None });
    }

    fn remove_selected(&mut self) {
        let (Some(name), Some(dir)) = (self.remote_pick.clone(), self.remote_dir()) else {
            return;
        };
        self.send_file_command(files::Command::Remove {
            path: files::remote_join(&dir, &name),
        });
        self.remote_pick = None;
        // The listing is stale the moment the delete lands, so ask again.
        self.send_file_command(files::Command::List { path: dir });
    }

    fn make_remote_folder(&mut self) {
        let Some(dir) = self.remote_dir() else { return };
        let name = self.new_folder.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.send_file_command(files::Command::MakeDir {
            path: files::remote_join(&dir, &name),
        });
        self.new_folder.clear();
        self.send_file_command(files::Command::List { path: dir });
    }

    // -----------------------------------------------------------------------
    // Transfers
    // -----------------------------------------------------------------------

    fn transfer_list(&mut self, ui: &mut egui::Ui) {
        let (transfers, running) = self
            .with_files(|st| (st.transfers.clone(), st.running() + st.queued()))
            .unwrap_or_default();

        theme::section(ui, "Transfers");
        let any_batches = self
            .with_files(|st| !st.batches.is_empty())
            .unwrap_or(false);
        if transfers.is_empty() && !any_batches {
            ui.label(
                RichText::new("Pick a file on either side, then press the arrow pointing where it should go.")
                    .size(12.0)
                    .color(theme::MUTED),
            );
            ui.add_space(8.0);
            return;
        }

        let batches: Vec<(files::Batch, files::BatchProgress)> = self
            .with_files(|st| {
                st.batches
                    .iter()
                    .map(|b| (b.clone(), st.batch_progress(b.id)))
                    .collect()
            })
            .unwrap_or_default();

        egui::ScrollArea::vertical()
            .max_height(140.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (b, p) in &batches {
                    batch_row(ui, b, p);
                }
                for t in transfers.iter().filter(|t| t.batch.is_none()) {
                    transfer_row(ui, t);
                }
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if running == 0 && ui.button("Clear finished").clicked() {
                self.with_files(files::State::clear_finished);
            }
            if running > 0 {
                ui.label(
                    RichText::new(format!("{running} in progress"))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            }
        });
        ui.add_space(8.0);
    }
}

/// A folder copy as one line: how many files have landed, and how far along
/// the bytes are.
fn batch_row(ui: &mut egui::Ui, b: &files::Batch, p: &files::BatchProgress) {
    let arrow = match b.direction {
        files::Direction::ToRobot => "→",
        files::Direction::FromRobot => "←",
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(arrow).size(15.0).strong().color(theme::ACCENT));
        theme::folder_icon(ui, 16.0);
        ui.label(RichText::new(&b.name).size(13.0).color(theme::TEXT));
        if b.truncated {
            ui.label(RichText::new("(cut off at the robot's limit)").size(11.0).color(ui.visuals().warn_fg_color));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(e) = &b.error {
                ui.label(RichText::new(e.as_str()).size(12.0).color(ui.visuals().error_fg_color));
                return;
            }
            let files = format!("{} / {} files", p.files_done, p.files_total);
            if p.finished() && b.queued == 0 {
                let colour = if p.files_failed == 0 { theme::GOOD } else { ui.visuals().error_fg_color };
                let text = if p.files_failed == 0 {
                    "100%".to_string()
                } else {
                    format!("{} failed", p.files_failed)
                };
                ui.label(RichText::new(text).monospace().size(13.0).strong().color(colour));
                ui.label(RichText::new(files).monospace().size(11.0).color(theme::MUTED));
                return;
            }
            let fraction = if p.bytes_total > 0 {
                (p.bytes_done as f32 / p.bytes_total as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.label(
                RichText::new(format!("{:>3.0}%", fraction * 100.0))
                    .monospace()
                    .size(13.0)
                    .strong()
                    .color(theme::ACCENT),
            );
            ui.add_sized(
                [180.0, 10.0],
                egui::ProgressBar::new(fraction).desired_height(10.0).fill(theme::ACCENT),
            );
            ui.label(RichText::new(files).monospace().size(11.0).color(theme::MUTED));
        });
    });
}

/// One line in the transfer list.
///
/// The percentage is spelled out as a number as well as a bar: a bar alone
/// answers "roughly how far", and the question someone standing over a robot
/// actually has is "is it moving, and how much longer".
fn transfer_row(ui: &mut egui::Ui, t: &files::Transfer) {
    let (arrow, verb) = match t.direction {
        files::Direction::ToRobot => ("→", "Upload"),
        files::Direction::FromRobot => ("←", "Download"),
    };

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(arrow)
                .size(15.0)
                .strong()
                .color(theme::ACCENT),
        )
        .on_hover_text(verb);
        ui.label(RichText::new(&t.name).size(13.0).color(theme::TEXT));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match &t.outcome {
                Some(Ok(())) => {
                    ui.label(
                        RichText::new("100%")
                            .monospace()
                            .size(13.0)
                            .strong()
                            .color(theme::GOOD),
                    );
                }
                Some(Err(reason)) => {
                    ui.label(
                        RichText::new(reason.as_str())
                            .size(12.0)
                            .color(ui.visuals().error_fg_color),
                    );
                }
                None => match t.fraction() {
                    Some(f) => {
                        // Fixed width and monospace so the number does not
                        // jitter the bar sideways as it counts up.
                        ui.label(
                            RichText::new(format!("{:>3.0}%", f * 100.0))
                                .monospace()
                                .size(13.0)
                                .strong()
                                .color(theme::ACCENT),
                        );
                        ui.add_sized(
                            [180.0, 10.0],
                            egui::ProgressBar::new(f)
                                .desired_height(10.0)
                                .fill(theme::ACCENT),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} / {}",
                                files::human_size(t.done),
                                files::human_size(t.total)
                            ))
                            .monospace()
                            .size(11.0)
                            .color(theme::MUTED),
                        );
                    }
                    None => {
                        // The size is not known yet, so report what has landed
                        // rather than a bar pretending to know how far along
                        // it is.
                        ui.label(
                            RichText::new(files::human_size(t.done))
                                .monospace()
                                .size(12.0)
                                .color(theme::MUTED),
                        );
                        ui.spinner();
                    }
                },
            }
        });
    });
}

/// Room the size and date columns need, so the name can be given the rest.
///
/// Fixed rather than measured: both are monospace and bounded — a date is
/// always 16 characters, a size never more than eight — so a constant is
/// exact enough and does not cost a text layout per row per frame.
fn meta_width(show_date: bool, show_size: bool) -> f32 {
    let mut w = 0.0;
    if show_date {
        w += 112.0;
    }
    if show_size {
        w += 62.0;
    }
    w
}

/// One pane's list of entries.
///
/// Returns the name clicked (a selection) and the name double-clicked (open a
/// folder). Both are returned rather than acted on so the caller keeps control
/// of navigation, which differs between the two sides.
fn entry_list(
    ui: &mut egui::Ui,
    salt: &str,
    listing: &DirListing,
    selected: Option<&str>,
    height: f32,
) -> (Option<String>, Option<String>) {
    let mut clicked = None;
    let mut opened = None;

    egui::ScrollArea::vertical()
        .id_salt(salt)
        .max_height(height.max(80.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if listing.entries.is_empty() {
                ui.label(RichText::new("empty").size(12.0).color(theme::MUTED));
                return;
            }
            for entry in &listing.entries {
                let is_selected = selected == Some(entry.name.as_str());
                let response = entry_row(ui, entry, is_selected);
                if response.clicked() {
                    clicked = Some(entry.name.clone());
                }
                // Files as well as folders: a folder opens, a file is edited,
                // and which of those happens is the caller's decision.
                if response.double_clicked() {
                    opened = Some(entry.name.clone());
                }
            }
        });

    (clicked, opened)
}

fn entry_row(ui: &mut egui::Ui, entry: &DirEntry, selected: bool) -> egui::Response {
    let response = egui::Frame::new()
        .fill(if selected {
            theme::ACCENT.gamma_multiply(0.16)
        } else {
            egui::Color32::TRANSPARENT
        })
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if entry.is_dir {
                    theme::folder_icon(ui, 16.0);
                } else {
                    theme::file_icon(ui, 16.0);
                }

                // Give the name an explicit share of the row and let it
                // truncate. Drawing it at full width and then painting the
                // size and date over the top from the right is what made a
                // narrow pane show `.fontconfig2026-03-30 04:13`.
                let (show_date, show_size) = crate::ui::meta_columns(ui.available_width());
                let meta = meta_width(show_date, show_size);
                let name_width = (ui.available_width() - meta).max(48.0);

                ui.allocate_ui(egui::vec2(name_width, ui.available_height()), |ui| {
                    let name = RichText::new(&entry.name).size(13.0).color(theme::TEXT);
                    ui.add(
                        egui::Label::new(if entry.is_dir { name.strong() } else { name })
                            .truncate(),
                    )
                    .on_hover_text(&entry.name);
                    if entry.is_link {
                        ui.label(RichText::new("link").size(10.0).color(theme::MUTED));
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if show_date {
                        if let Some(secs) = entry.modified {
                            ui.label(
                                RichText::new(files::short_date(secs))
                                    .monospace()
                                    .size(11.0)
                                    .color(theme::MUTED),
                            );
                        }
                    }
                    if show_size && !entry.is_dir {
                        ui.label(
                            RichText::new(files::human_size(entry.len))
                                .monospace()
                                .size(11.0)
                                .color(theme::MUTED),
                        );
                    }
                });
            });
        })
        .response
        .interact(egui::Sense::click());

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}
