//! The viewer's screens: connect, discover, and the live stream with its
//! settings panel.

use std::sync::{Arc, Mutex};

use telekin_proto::InputEvent;
use telekin_transport::discovery::Discovered;

use crate::net::StreamSettings;
use crate::theme;
use crate::{keymap, Session, Waker, DISCOVERY_TIMEOUT};

/// What the window calls itself. One place, so a rename cannot leave the
/// wordmark and the title bar disagreeing.
pub const NAME: &str = "Telekin";
/// The line under it: what the thing actually does, in plain words.
pub const TAGLINE: &str = "remote desktop for robots";
/// Who made it. Small, at the foot of the first screen — a signature, not a
/// banner.
pub const AUTHOR: &str = "phuwanat@IRiSH LAB";

/// What a connected session is showing.
///
/// Files mode is not a lesser desktop: it is a session that never asked the
/// robot to capture anything, which is why it is chosen before connecting
/// rather than found in a menu afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Desktop,
    Files,
}

/// How many robots fit on one page of scan results, for a window this tall.
///
/// Everything above and below the list — the wordmark band, the address box,
/// the scan button, the pager, the credit strip — is fixed furniture, so what
/// is left over is what the rows get. The floor of three keeps the list
/// usable at the smallest window the app allows.
fn rows_per_page(window_height: f32) -> usize {
    /// Vertical space the connection screen spends on everything but the rows.
    const FURNITURE: f32 = 400.0;
    let room = window_height - FURNITURE;
    ((room / ROW_HEIGHT).floor().max(0.0) as usize).clamp(3, 8)
}

/// How tall one robot's row is, including the gap under it.
///
/// Used to work out how many fit on a page. Measured rather than guessed:
/// two lines of 19 px and 14 px, plus the box's own padding.
const ROW_HEIGHT: f32 = 84.0;
/// Corner rounding shared by a row's fill and its outline, so they agree.
const ROW_RADIUS: u8 = 8;

/// Result of a background mDNS scan.
type ScanResult = Arc<Mutex<Option<anyhow::Result<Vec<Discovered>>>>>;

pub struct ViewerApp {
    address: String,
    username: String,
    password: String,
    fingerprint: String,
    settings: StreamSettings,
    view_only: bool,

    pub(crate) session: Option<Session>,
    /// Set when the operator picked a robot from the discovery list.
    picked: Option<Discovered>,
    connect_error: Option<String>,

    scan: ScanResult,
    scanning: bool,
    hosts: Vec<Discovered>,

    waker: Waker,
    texture: Option<egui::TextureHandle>,
    /// Frame generation already uploaded, so a static screen is not re-sent to
    /// the GPU every repaint.
    shown_generation: u64,
    show_stats: bool,
    /// Modifier state last sent, so held modifiers are mirrored to the host.
    modifiers: [bool; 4],
    /// Whether the last pointer position was inside the video.
    pointer_in_video: bool,
    /// Last clipboard text exchanged, in either direction. Used to break the
    /// echo: without it, text pushed from the host would be sent straight
    /// back the first time the operator pasted.
    last_clipboard: String,
    /// Mouse buttons the host currently believes are down. Kept so a release
    /// is never lost and so motion can be damped while dragging.
    held_buttons: Vec<telekin_proto::MouseButton>,
    /// Last pointer position forwarded, normalized. `None` until the first.
    last_sent_pos: Option<(f32, f32)>,
    /// Fullscreen, no chrome, every key forwarded.
    immersive: bool,
    /// When the immersive hint stops being shown.
    hint_until: Option<std::time::Instant>,
    /// A specific address to dial, when the operator pinned one network
    /// rather than letting every advertised address be tried in order.
    chosen_addr: Option<std::net::SocketAddr>,
    /// Which page of scan results is showing.
    page: usize,
    /// Desktop or files. Chosen on the connection screen and switchable
    /// once connected.
    pub(crate) mode: Mode,
    /// Whether this session has ever asked for video. A files-only session
    /// has not, and the robot's encoder has never run.
    pub(crate) streaming: bool,
    // The file panes live in `ui_files`, which is a sibling module rather than
    // a separate type: it needs the session and the local directory, and
    // splitting the state apart would only move the coupling somewhere less
    // obvious.
    /// The directory shown in the local pane.
    pub(crate) local_dir: std::path::PathBuf,
    /// That directory's contents, re-read when it changes.
    pub(crate) local: Option<telekin_proto::DirListing>,
    pub(crate) local_error: Option<String>,
    pub(crate) local_pick: Option<String>,
    pub(crate) remote_pick: Option<String>,
    /// Typed into the "new folder" box on the robot pane.
    pub(crate) new_folder: String,
    /// Set once Close has been pressed on an edited file, so the second press
    /// is the one that discards it.
    pub(crate) close_confirmed: bool,
    /// Whether transfers were running on the previous frame, so the panes
    /// can be refreshed exactly once when the last one lands.
    pub(crate) transfers_were_busy: bool,
    /// Scroll not yet worth a whole line. A trackpad sends a stream of
    /// fractions; dropping each one leaves the wheel feeling dead.
    scroll_carry: egui::Vec2,
    /// The connected computer's own hostname, when discovery told us. The
    /// handshake reports the account to sign in as, not the machine, so a
    /// robot reached by typing its address has none to show.
    machine: Option<String>,
    /// The background update check, once started. `None` when the check was
    /// switched off or has not been started, which is how tests run.
    update: Option<crate::update::Shared>,
}

impl ViewerApp {
    pub fn new(args: &crate::Args) -> Self {
        let address = match (&args.host, &args.name) {
            (Some(h), _) => h.clone(),
            (None, Some(n)) => n.clone(),
            _ => String::new(),
        };
        let mut app = Self {
            address,
            username: args.user.clone().unwrap_or_default(),
            password: args.password.clone().unwrap_or_default(),
            fingerprint: args.fingerprint.clone().unwrap_or_default(),
            settings: StreamSettings {
                monitor: args.monitor,
                max_fps: args.fps,
                bitrate_kbps: args.bitrate_kbps,
                scale_percent: args.scale.clamp(25, 100),
                max_cpu_percent: args.max_cpu_percent,
            },
            view_only: args.view_only,
            session: None,
            picked: None,
            connect_error: None,
            scan: Arc::new(Mutex::new(None)),
            scanning: false,
            hosts: Vec::new(),
            waker: Arc::new(std::sync::OnceLock::new()),
            texture: None,
            shown_generation: 0,
            show_stats: true,
            modifiers: [false; 4],
            pointer_in_video: false,
            last_clipboard: String::new(),
            held_buttons: Vec::new(),
            last_sent_pos: None,
            immersive: false,
            hint_until: None,
            chosen_addr: None,
            page: 0,
            mode: Mode::Desktop,
            streaming: false,
            local_dir: crate::files::local_start(),
            local: None,
            local_error: None,
            local_pick: None,
            remote_pick: None,
            new_folder: String::new(),
            close_confirmed: false,
            transfers_were_busy: false,
            scroll_carry: egui::Vec2::ZERO,
            machine: None,
            update: None,
        };
        if args.connect {
            app.connect();
        }
        app
    }

    /// Look for a newer viewer, in the background.
    pub fn start_update_check(&mut self) {
        self.update = Some(crate::update::start(None));
    }

    fn connect(&mut self) {
        self.connect_error = None;
        let fingerprint = match self.fingerprint.trim() {
            "" => None,
            text => match telekin_transport::parse_fingerprint(text) {
                Ok(f) => Some(f),
                Err(e) => {
                    self.connect_error = Some(format!("Fingerprint: {e}"));
                    return;
                }
            },
        };
        // A pinned network is dialled and nothing else, so a failure names
        // the network the operator chose instead of quietly succeeding over
        // the other one. Otherwise a discovered host is dialled by every
        // address it advertised, and a typed one by exactly what was typed.
        let hosts = match self.chosen_addr {
            Some(addr) => vec![addr],
            None => match crate::resolve(&self.address, self.picked.as_ref()) {
                Ok(h) => h,
                Err(e) => {
                    self.connect_error = Some(format!("{e:#}"));
                    return;
                }
            },
        };
        if self.username.trim().is_empty() || self.password.is_empty() {
            self.connect_error = Some("Enter the username and password for this robot".into());
            return;
        }

        self.machine = self.picked.as_ref().and_then(|p| p.hostname.clone());
        self.session = Some(Session::start(
            hosts,
            fingerprint,
            self.username.trim().to_string(),
            self.password.clone(),
            self.settings,
            self.mode == Mode::Desktop,
            self.waker.clone(),
        ));
        self.texture = None;
        self.shown_generation = 0;
    }

    fn start_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let slot = self.scan.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            // Logged either way: "I pressed scan and nothing appeared" is
            // otherwise impossible to tell apart from a scan that answered
            // and a list that failed to draw it.
            tracing::info!("scan started, {:?} budget", DISCOVERY_TIMEOUT);
            let found = telekin_transport::discovery::discover(DISCOVERY_TIMEOUT);
            match &found {
                Ok(hosts) => {
                    tracing::info!("scan answered with {} host(s)", hosts.len());
                    for h in hosts {
                        tracing::info!(
                            "  {} on {} at {:?}",
                            h.name,
                            h.hostname.as_deref().unwrap_or("(no computer name)"),
                            h.addrs
                        );
                    }
                }
                Err(e) => tracing::warn!("scan failed: {e:#}"),
            }
            *slot.lock().expect("scan slot poisoned") = Some(found);
            match waker.get() {
                Some(ctx) => ctx.request_repaint(),
                // Without a context the window will not redraw itself, and
                // the result sits in the slot looking like a dead scan.
                None => tracing::warn!("no window to wake; result may not appear until you move the mouse"),
            }
        });
    }

    fn poll_scan(&mut self) {
        let done = self.scan.lock().expect("scan slot poisoned").take();
        if let Some(result) = done {
            self.scanning = false;
            match result {
                Ok(hosts) => {
                    tracing::info!("showing {} host(s) in the list", hosts.len());
                    self.page = 0;
                    if hosts.is_empty() {
                        self.connect_error = Some(
                            "No robots answered. Check the host is running and that mDNS \
                             (UDP 5353) is not blocked between the machines."
                                .into(),
                        );
                    }
                    self.hosts = hosts;
                }
                Err(e) => self.connect_error = Some(format!("Discovery failed: {e:#}")),
            }
        }
    }

    fn apply_settings(&self) {
        if let Some(s) = &self.session {
            if let Some(tx) = s.settings() {
                let _ = tx.send(crate::net::StreamControl::Start(self.settings));
            }
        }
    }

    /// Copy anything the host copied into this machine's clipboard, so
    /// Ctrl+C on the robot then Ctrl+V here just works.
    fn pull_host_clipboard(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else { return };
        let text = {
            let mut st = session.status.lock().expect("status poisoned");
            st.incoming_clipboard.take()
        };
        if let Some(text) = text {
            // Remember it so the paste we send back is not immediately
            // echoed to the host as a "new" clipboard.
            self.last_clipboard = text.clone();
            ctx.copy_text(text);
        }
    }

    fn disconnect(&mut self) {
        self.session = None;
        self.texture = None;
        self.machine = None;
    }
}

impl eframe::App for ViewerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let _ = self.waker.set(ui.ctx().clone());
        self.poll_scan();

        let status = self.session.as_ref().map(|s| s.snapshot());
        let connected = status.as_ref().is_some_and(|s| s.connected);

        if connected {
            let status = status.expect("checked above");
            self.pull_host_clipboard(ui.ctx());
            match self.mode {
                Mode::Files => {
                    self.session_toolbar(ui, &status);
                    self.files_screen(ui);
                }
                Mode::Desktop => self.stream_screen(ui, &status),
            }
        } else {
            self.connect_screen(ui, status.as_ref());
        }
    }
}

// ---------------------------------------------------------------------------
// Connection screen
// ---------------------------------------------------------------------------

impl ViewerApp {
    fn connect_screen(&mut self, ui: &mut egui::Ui, status: Option<&crate::net::Status>) {
        // The wordmark sits on the same black band the session bar uses, so
        // every screen has one fixed edge and the application reads as one
        // thing whether or not it is connected to anything.
        egui::Panel::top("wordmark")
            .frame(theme::panel(theme::BAR, 18))
            .show(ui, |ui| {
                theme::bar(ui);
                ui.horizontal(|ui| {
                    // Centre the wordmark by hand: the mark and the text are
                    // two widgets, so the usual centring helper would only
                    // centre one of them. Measure rather than assume a width —
                    // the last hard-coded number was sized for a name this
                    // product no longer has.
                    const MARK: f32 = 44.0;
                    const GAP: f32 = 10.0;
                    let name = egui::RichText::new(NAME).size(38.0).strong();
                    let tagline = egui::RichText::new(TAGLINE).size(16.0);
                    let painter = ui.painter();
                    let one = painter.layout_no_wrap(
                        NAME.to_owned(),
                        egui::FontId::proportional(38.0),
                        theme::BAR_TEXT,
                    );
                    let two = painter.layout_no_wrap(
                        TAGLINE.to_owned(),
                        egui::FontId::proportional(16.0),
                        theme::BAR_MUTED,
                    );
                    let widest = one.rect.width().max(two.rect.width()).ceil();

                    let block = MARK + GAP + widest;
                    ui.add_space(((ui.available_width() - block) * 0.5).max(0.0));
                    theme::logo(ui, MARK, theme::BAR_ACCENT);
                    ui.add_space(GAP);
                    ui.vertical(|ui| {
                        ui.label(name.color(theme::BAR_TEXT));
                        ui.label(tagline.color(theme::BAR_MUTED));
                    });
                });
            });

        // The signature sits in its own strip at the foot of the window, so it
        // stays put rather than drifting up and down with the length of a
        // scan result.
        egui::Panel::bottom("credit")
            .frame(theme::panel(theme::SURFACE, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(AUTHOR)
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        self.version_status(ui);
                    });
                });
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(18.0);
                // Picking a robot and signing in are one job, so they sit
                // side by side rather than on separate screens.
                ui.columns(2, |cols| {
                    self.robot_picker(&mut cols[0]);
                    self.sign_in(&mut cols[1], status);
                });
            });
        });
    }

    /// The version, and whether a newer one exists, in the corner of the
    /// first screen. Drawn right-to-left, so the button (when there is one)
    /// sits at the edge and the version reads inward from it.
    fn version_status(&mut self, ui: &mut egui::Ui) {
        use crate::update::{Outcome, CURRENT};

        let outcome = self
            .update
            .as_ref()
            .map(|slot| slot.lock().expect("update slot poisoned").clone());

        match outcome {
            Some(Outcome::Available { version, notes, url }) => {
                let label = format!("Update to {version}");
                if ui
                    .add(egui::Button::new(egui::RichText::new(label).size(12.0)))
                    .on_hover_text(if notes.is_empty() {
                        "Opens the download. Installing it replaces this version."
                    } else {
                        notes.as_str()
                    })
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
                ui.label(
                    egui::RichText::new(format!("v{CURRENT}"))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            }
            Some(Outcome::Checking) => {
                // The check runs on its own thread with nothing to wake the
                // window when it finishes; poll gently until it has.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(500));
                ui.label(
                    egui::RichText::new(format!("v{CURRENT} · checking for updates…"))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            }
            // The three resting states are the same clickable label: the
            // check runs by itself at start, but a viewer left open for a
            // week would never notice a release, so the version is also the
            // "check again" control.
            Some(Outcome::UpToDate) => {
                if recheck_label(ui, &format!("v{CURRENT} · up to date")) {
                    self.start_update_check();
                }
            }
            Some(Outcome::Unavailable(why)) => {
                // Not an error banner: on a robot LAN with no internet this is
                // the normal case, and the reason is one hover away.
                let clicked = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(format!("v{CURRENT}"))
                                .size(12.0)
                                .color(theme::MUTED),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text(format!(
                        "Could not check for updates: {why}
Click to try again."
                    ))
                    .clicked();
                if clicked {
                    self.start_update_check();
                }
            }
            None => {
                if recheck_label(ui, &format!("v{CURRENT}")) {
                    self.start_update_check();
                }
            }
        }
    }

    fn robot_picker(&mut self, ui: &mut egui::Ui) {
        ui.scope(|ui| {
            ui.set_width(ui.available_width() - 14.0);
            theme::section(ui, "Robot");

            theme::field_label(ui, "Address or name");
            ui.add_space(5.0);
            let response = ui.add_sized(
                [ui.available_width(), theme::CONTROL_HEIGHT],
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("192.168.200.105")
                    .font(egui::TextStyle::Monospace)
                    .vertical_align(egui::Align::Center),
            );
            if response.changed() {
                // Typing an address means the operator is no longer using
                // whatever was picked from the list, nor the network chosen
                // for it.
                self.picked = None;
                self.chosen_addr = None;
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_sized([160.0, theme::CONTROL_HEIGHT], egui::Button::new("Scan network"))
                    .clicked()
                {
                    self.start_scan();
                }
                if self.scanning {
                    ui.add_space(6.0);
                    ui.spinner();
                } else if !self.hosts.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!("{} online", self.hosts.len()))
                            .size(12.0)
                            .color(theme::GOOD),
                    );
                }
            });

            ui.add_space(14.0);
            self.discovery_list(ui);
            self.network_picker(ui);
        });
    }

    /// Which of a robot's networks to dial, when it answered on more than one.
    ///
    /// Automatic is right nearly always — the addresses are already ranked and
    /// tried in turn, so an unplugged cable costs one failed attempt. It stops
    /// being right when both paths work but only one is wanted: a wired link
    /// during a long build, or WiFi while the robot drives away from its dock.
    /// Nothing in the addresses says which is which, so this offers the choice
    /// rather than guessing.
    fn network_picker(&mut self, ui: &mut egui::Ui) {
        let Some(picked) = self.picked.clone() else {
            return;
        };
        let choices: Vec<std::net::SocketAddr> = picked
            .addrs
            .iter()
            .copied()
            .filter(|a| a.is_ipv4())
            .collect();
        if choices.len() < 2 {
            return;
        }

        ui.add_space(14.0);
        theme::field_label(ui, "Network");
        ui.add_space(5.0);

        let selected = match self.chosen_addr {
            Some(a) => a.ip().to_string(),
            None => "Automatic".to_string(),
        };
        egui::ComboBox::from_id_salt("network_pick")
            .width(ui.available_width())
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.chosen_addr, None, "Automatic");
                for addr in &choices {
                    ui.selectable_value(
                        &mut self.chosen_addr,
                        Some(*addr),
                        addr.ip().to_string(),
                    );
                }
            });
        if self.chosen_addr.is_none() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Tries each address in turn, closest network first.")
                    .size(11.5)
                    .color(theme::MUTED),
            );
        }
    }

    fn sign_in(&mut self, ui: &mut egui::Ui, status: Option<&crate::net::Status>) {
        ui.scope(|ui| {
            theme::section(ui, "Sign in");

            theme::field_label(ui, "Username");
            ui.add_space(5.0);
            ui.add_sized(
                [ui.available_width(), theme::CONTROL_HEIGHT],
                egui::TextEdit::singleline(&mut self.username)
                    .hint_text("the robot's own account")
                    .font(egui::TextStyle::Monospace)
                    .vertical_align(egui::Align::Center),
            );

            ui.add_space(12.0);
            theme::field_label(ui, "Password");
            ui.add_space(5.0);
            let password = ui.add_sized(
                [ui.available_width(), theme::CONTROL_HEIGHT],
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text("that account's password")
                    .vertical_align(egui::Align::Center),
            );

            ui.add_space(18.0);
            let ready = !self.address.trim().is_empty()
                && !self.username.trim().is_empty()
                && !self.password.is_empty();

            // Enter in the password box is the obvious way to submit a form.
            let submitted =
                password.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            let clicked = ui
                .add_enabled_ui(ready, |ui| {
                    ui.add_sized(
                        [ui.available_width(), 44.0],
                        theme::primary_button("Connect"),
                    )
                })
                .inner
                .clicked();
            if clicked || (submitted && ready) {
                self.mode = Mode::Desktop;
                self.connect();
            }

            // Offered here rather than behind a connection, because it is a
            // different job with a different cost: files only, no capture, no
            // encoder, nothing asked of the robot but a socket.
            ui.add_space(8.0);
            let files = ui
                .add_enabled_ui(ready, |ui| {
                    ui.add_sized([ui.available_width(), 38.0], egui::Button::new("Files only"))
                })
                .inner
                .on_hover_text("Copy files to and from the robot without starting its encoder");
            if files.clicked() {
                self.mode = Mode::Files;
                self.connect();
            }

            if let Some(e) = &self.connect_error {
                ui.add_space(12.0);
                ui.colored_label(ui.visuals().error_fg_color, e);
            } else if let Some(s) = status {
                if !s.message.is_empty() {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(&s.message).size(13.0).color(theme::MUTED));
                }
            }

            ui.add_space(14.0);
            ui.collapsing("Advanced", |ui| {
                ui.add_space(6.0);
                theme::field_label(ui, "Fingerprint");
                ui.add_space(5.0);
                ui.add_sized(
                    [ui.available_width(), theme::CONTROL_HEIGHT],
                    egui::TextEdit::singleline(&mut self.fingerprint)
                        .hint_text("optional - proves it is your robot")
                        .font(egui::TextStyle::Monospace)
                        .vertical_align(egui::Align::Center),
                );
                if self.fingerprint.trim().is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "Without one the link is encrypted, but nothing proves the machine answering is your robot. Fine on a trusted LAN.",
                        )
                        .size(12.0)
                        .color(ui.visuals().warn_fg_color),
                    );
                }
                ui.add_space(16.0);
                self.settings_controls(ui, false);
            });
        });
    }

    fn discovery_list(&mut self, ui: &mut egui::Ui) {
        if self.hosts.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Robots announce themselves over mDNS. Scan, or type an address above.",
                )
                .small()
                .color(theme::MUTED),
            );
            return;
        }

        // Pages rather than a scroll bar. With a fleet you are looking for one
        // robot by name, and a list you can see all of at once is quicker to
        // read than one that moves under the pointer — and there is nothing to
        // scroll past by accident while reaching for a row.
        // Measured against the window, not against `available_height`: inside
        // a scroll area that reports the scrollable extent rather than what
        // is on screen, which put three robots on a page that had room for
        // five.
        let per_page = rows_per_page(ui.input(|i| i.viewport_rect().height()));
        let pages = self.hosts.len().div_ceil(per_page);
        self.page = self.page.min(pages.saturating_sub(1));

        let shown: Vec<Discovered> = self
            .hosts
            .iter()
            .skip(self.page * per_page)
            .take(per_page)
            .cloned()
            .collect();

        ui.scope(|ui| {
            // Rows are two lines that belong together; the page's airy
            // spacing between separate items is too much between them.
            ui.spacing_mut().item_spacing.y = 6.0;
            for host in shown {
                let selected = self.picked.as_ref().is_some_and(|p| p.name == host.name);
                if self.discovered_row(ui, &host, selected) {
                    self.choose(&host);
                }
            }
        });

        if pages > 1 {
            ui.add_space(10.0);
            self.pager(ui, pages, per_page);
        }
    }

    /// Page controls, shown only when there is more than one page.
    fn pager(&mut self, ui: &mut egui::Ui, pages: usize, per_page: usize) {
        ui.horizontal(|ui| {
            let back = ui
                .add_enabled_ui(self.page > 0, |ui| {
                    theme::arrow_button(
                        ui,
                        egui::vec2(46.0, theme::CONTROL_HEIGHT),
                        theme::Arrow::Left,
                        "Previous page",
                    )
                })
                .inner
                .clicked();
            if back {
                self.page -= 1;
            }

            // Which robots are on this page, not just which page it is: with
            // twenty of them "7-12 of 20" is the useful fact.
            let first = self.page * per_page + 1;
            let last = ((self.page + 1) * per_page).min(self.hosts.len());
            ui.label(
                egui::RichText::new(format!("{first}\u{2013}{last} of {}", self.hosts.len()))
                    .size(15.0)
                    .color(theme::MUTED),
            );

            let forward = ui
                .add_enabled_ui(self.page + 1 < pages, |ui| {
                    theme::arrow_button(
                        ui,
                        egui::vec2(46.0, theme::CONTROL_HEIGHT),
                        theme::Arrow::Right,
                        "Next page",
                    )
                })
                .inner
                .clicked();
            if forward {
                self.page += 1;
            }
        });
    }

    /// One robot in the scan results. Returns true when it was chosen.
    ///
    /// Note: the address line lists every network it answered on — see
    /// [`address_summary`].
    ///
    /// The instance name *is* the account to sign in with, so it leads; the
    /// machine name and address are secondary, and only there to tell two
    /// robots sharing an operator account apart.
    fn discovered_row(&self, ui: &mut egui::Ui, host: &Discovered, selected: bool) -> bool {
        let addr = address_summary(&host.addrs);
        // Only when it adds something. The name defaults to the hostname, so
        // repeating it would be noise; it is worth showing when someone has
        // given the robot a name of its own with `--name`.
        let machine = host
            .hostname
            .as_deref()
            .filter(|h| !h.is_empty() && *h != host.name);

        // A drawn box, not just a hover tint. Each robot is a target to be
        // clicked, often in a hurry, and an outline says where that target
        // begins and ends before the pointer is anywhere near it.
        let response = egui::Frame::new()
            .fill(if selected {
                theme::ACCENT.gamma_multiply(0.14)
            } else {
                theme::BASE
            })
            .corner_radius(egui::CornerRadius::same(ROW_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    // A live host earns the one bright colour on this screen.
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, theme::GOOD);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        // Two lines that belong together; the panel's usual
                        // airy spacing would read as two separate items.
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&host.name)
                                    .size(19.0)
                                    .strong()
                                    .color(if selected { theme::ACCENT } else { theme::TEXT }),
                            );
                            // The computer's own name. With one account per
                            // fleet this is the only thing that tells two
                            // robots apart before you have their addresses
                            // memorised.
                            if let Some(machine) = machine {
                                ui.label(
                                    egui::RichText::new(format!("on {machine}"))
                                        .size(15.0)
                                        .color(theme::MUTED),
                                );
                            }
                        });
                        ui.label(
                            egui::RichText::new(addr)
                                .monospace()
                                .size(14.0)
                                .color(theme::MUTED),
                        );
                    });
                });
            })
            .response;

        let hit = response.interact(egui::Sense::click());

        // Painted after the frame so it can react to this frame's hover; a
        // border baked into the Frame would always be one repaint behind.
        let (colour, width) = if selected {
            (theme::ACCENT, 2.0)
        } else if hit.hovered() {
            (theme::ACCENT, 1.5)
        } else {
            (theme::ROW_EDGE, 1.0)
        };
        ui.painter().rect_stroke(
            hit.rect,
            egui::CornerRadius::same(ROW_RADIUS),
            egui::Stroke::new(width, colour),
            egui::StrokeKind::Inside,
        );

        if hit.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        hit.clicked() || hit.double_clicked()
    }

    /// Adopt a discovered robot, leaving only the password to type.
    ///
    /// The username comes from the account the host advertises, not from its
    /// name: those were the same thing when a robot was named after the
    /// account it ran as, and stopped being the same when the name became the
    /// hostname so that a fleet could be told apart. Older hosts publish no
    /// account, and for those the name is still the best guess.
    fn choose(&mut self, host: &Discovered) {
        self.address = host.name.clone();
        if self.username.trim().is_empty() {
            self.username = host
                .account
                .clone()
                .unwrap_or_else(|| host.name.clone());
        }
        // The advertised fingerprint is a convenience, not proof — anything
        // on the LAN can publish a record. Filling it in saves typing; it
        // should still be compared against what the robot printed.
        if let Some(fp) = &host.fingerprint {
            self.fingerprint = fp.clone();
        }
        // A network pinned on one robot means nothing on another.
        self.chosen_addr = None;
        self.picked = Some(host.clone());
    }

    fn settings_controls(&mut self, ui: &mut egui::Ui, live: bool) {
        let before = self.settings;

        theme::field_label(ui, "Resolution");
        ui.add(
            egui::Slider::new(&mut self.settings.scale_percent, 25..=100)
                .suffix("%")
                .trailing_fill(true),
        );
        ui.label(
            egui::RichText::new(
                "Share of the robot's screen to encode. The strongest control over both lag and the robot's CPU — its own display is untouched.",
            )
            .small()
            .color(theme::MUTED),
        );

        ui.add_space(14.0);
        theme::field_label(ui, "Frame rate");
        ui.add(
            egui::Slider::new(&mut self.settings.max_fps, 1..=60)
                .suffix(" fps")
                .trailing_fill(true),
        );

        ui.add_space(14.0);
        theme::field_label(ui, "Robot CPU limit");
        let mut capped = self.settings.max_cpu_percent.is_some();
        if ui.checkbox(&mut capped, "Cap it").changed() {
            self.settings.max_cpu_percent = capped.then_some(25);
        }
        if let Some(cap) = &mut self.settings.max_cpu_percent {
            ui.add(
                egui::Slider::new(cap, 5..=100)
                    .suffix("% of one core")
                    .trailing_fill(true),
            );
        }
        ui.label(
            egui::RichText::new(
                "A ceiling the robot will not exceed: it drops frame rate to stay under. A still screen costs nothing either way.",
            )
            .small()
            .color(theme::MUTED),
        );

        ui.add_space(14.0);
        theme::field_label(ui, "Bitrate");
        ui.add(
            egui::Slider::new(&mut self.settings.bitrate_kbps, 500..=50_000)
                .suffix(" kbps")
                .logarithmic(true)
                .trailing_fill(true),
        );

        if live {
            if let Some(session) = &self.session {
                let monitors = session.snapshot().monitors;
                if monitors.len() > 1 {
                    ui.add_space(14.0);
                    theme::field_label(ui, "Monitor");
                    egui::ComboBox::from_id_salt("monitor_pick")
                        .width(ui.available_width())
                        .selected_text(
                            monitors
                                .iter()
                                .find(|m| m.id == self.settings.monitor)
                                .map(|m| format!("{} ({}x{})", m.name, m.width, m.height))
                                .unwrap_or_else(|| "-".into()),
                        )
                        .show_ui(ui, |ui| {
                            for m in &monitors {
                                ui.selectable_value(
                                    &mut self.settings.monitor,
                                    m.id,
                                    format!("{} ({}x{})", m.name, m.width, m.height),
                                );
                            }
                        });
                }
            }
        }

        ui.add_space(14.0);
        ui.checkbox(&mut self.view_only, "View only (do not control the robot)");

        // Applying on change keeps the panel honest: what is shown is what
        // the robot was told.
        if live && self.settings != before {
            self.apply_settings();
        }
    }
}

// ---------------------------------------------------------------------------
// Live stream
// ---------------------------------------------------------------------------

impl ViewerApp {
    /// The bar across the top of a connected session.
    ///
    /// Shared by both modes: which one you are in is a switch on this bar, not
    /// a different window, because moving a file and watching the screen are
    /// two things you do to the same robot in the same sitting.
    pub(crate) fn session_toolbar(&mut self, ui: &mut egui::Ui, status: &crate::net::Status) {
        egui::Panel::top("toolbar")
            .frame(theme::panel(theme::BAR, 8))
            .show(ui, |ui| {
                theme::bar(ui);
                ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .add_sized([120.0, theme::CONTROL_HEIGHT], egui::Button::new("Disconnect"))
                    .clicked()
                {
                    self.disconnect();
                    return;
                }

                ui.add_space(6.0);
                ui.label(egui::RichText::new("●").size(11.0).color(theme::GOOD));
                ui.label(
                    egui::RichText::new(&status.host_name)
                        .size(16.0)
                        .strong()
                        .color(theme::BAR_TEXT),
                );
                if let Some(machine) = &self.machine {
                    ui.label(
                        egui::RichText::new(format!("on {machine}"))
                            .size(13.0)
                            .color(theme::BAR_MUTED),
                    );
                }

                ui.add_space(10.0);
                // Resolution and frame rate only mean something while the
                // robot is encoding. A files-only session showed `0x0` and
                // `0 fps`, which reads as a fault rather than as "nobody
                // asked for video".
                if self.streaming {
                    stat(
                        ui,
                        "resolution",
                        &format!("{}x{}", status.frame_size.0, status.frame_size.1),
                    );
                    stat(ui, "fps", &format!("{:.0}", status.view_fps));
                } else {
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("encoder idle")
                            .size(12.0)
                            .color(theme::BAR_MUTED),
                    )
                    .on_hover_text("No video is being captured on the robot");
                }
                stat(ui, "latency", &format!("{:.0} ms", status.rtt_ms));
                if self.view_only {
                    ui.add_space(6.0);
                    ui.colored_label(ui.visuals().warn_fg_color, "view only");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // A toggle rather than a link: it has two states and the
                    // button should show which one it is in.
                    let files = self.mode == Mode::Files;
                    // A robot with no desktop session has nothing to show. The
                    // switch says so rather than handing over a black screen
                    // and letting the operator wonder which end is broken.
                    let screen_available = !status.monitors.is_empty();
                    let label = if files { "Desktop" } else { "Files" };
                    let hint = if files && !screen_available {
                        "No desktop session on the robot — sign in there first. Files still work."
                    } else if files {
                        "Watch the robot's screen. This starts its encoder."
                    } else {
                        "Copy files to and from the robot. Stops the encoder."
                    };
                    let enabled = files_switch_enabled(files, screen_available);
                    if ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(label)
                                .min_size(egui::vec2(110.0, theme::CONTROL_HEIGHT)),
                        )
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.set_mode(if files { Mode::Desktop } else { Mode::Files });
                    }
                    let label = if self.show_stats { "Hide settings" } else { "Settings" };
                    let button = egui::Button::new(label)
                        .selected(self.show_stats)
                        .min_size(egui::vec2(130.0, theme::CONTROL_HEIGHT));
                    if ui.add(button).clicked() {
                        self.show_stats = !self.show_stats;
                    }
                    let immersive = ui.add(
                        egui::Button::new("Immersive")
                            .min_size(egui::vec2(120.0, theme::CONTROL_HEIGHT)),
                    );
                    if immersive
                        .on_hover_text("Fullscreen, and every shortcut goes to the robot (F11)")
                        .clicked()
                    {
                        self.set_immersive(ui.ctx(), true);
                    }
                });
            });
            ui.add_space(6.0);
        });
    }

    /// Switch between watching the screen and moving files.
    ///
    /// Entering Desktop is what actually asks the robot to capture and encode;
    /// leaving it stops that again. A session parked on Files costs the robot
    /// nothing but a socket, which is the point.
    fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            Mode::Desktop => {
                self.apply_settings();
                self.streaming = true;
            }
            Mode::Files => {
                if self.streaming {
                    if let Some(session) = &self.session {
                        if let Some(tx) = session.settings() {
                            let _ = tx.send(crate::net::StreamControl::Stop);
                        }
                    }
                    self.streaming = false;
                    // The last frame is stale the moment the encoder stops.
                    self.texture = None;
                }
            }
        }
    }

    fn stream_screen(&mut self, ui: &mut egui::Ui, status: &crate::net::Status) {
        // F11 is the way out of immersive mode, so it is deliberately never
        // forwarded. Everything else is, which is the whole point of the mode.
        if ui.input(|i| i.key_pressed(egui::Key::F11)) {
            self.set_immersive(ui.ctx(), !self.immersive);
        }

        if self.immersive {
            self.immersive_screen(ui);
            return;
        }

        self.session_toolbar(ui, status);

        if self.show_stats {
            egui::Panel::right("settings_panel")
                .default_size(360.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add_space(16.0);
                        theme::section(ui, "Stream");
                        self.settings_controls(ui, true);

                        ui.add_space(24.0);
                        theme::section(ui, "Timing");
                        meter(ui, "Capture", status.capture_ms);
                        meter(ui, "Encode", status.encode_ms);
                        ui.add_space(4.0);
                        readout(
                            ui,
                            "On the robot",
                            &format!("{:.1} ms", status.capture_ms + status.encode_ms),
                        );
                        readout(ui, "Network", &format!("{:.1} ms", status.rtt_ms));
                        readout(ui, "Bandwidth", &format!("{} kbps", status.bitrate_kbps));

                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(
                                "If the robot's own time dominates the round trip, lower the resolution rather than the frame rate.",
                            )
                            .size(11.5)
                            .color(theme::MUTED),
                        );
                        ui.add_space(16.0);
                    });
                });
        }

        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(theme::VIDEO_BACKDROP))
            .show(ui, |ui| {
                self.video(ui);
            });
    }

    /// Enter or leave immersive mode.
    fn set_immersive(&mut self, ctx: &egui::Context, on: bool) {
        if self.immersive == on {
            return;
        }
        self.immersive = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        if on {
            // Nothing in this app should hold the keyboard while the robot
            // is meant to receive every key.
            ctx.memory_mut(|m| m.stop_text_input());
            self.hint_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
        } else {
            self.hint_until = None;
        }
    }

    /// Immersive mode: the screen and nothing else.
    fn immersive_screen(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(theme::VIDEO_BACKDROP))
            .show(ui, |ui| {
                self.video(ui);

                // A brief reminder of the way out. Without it the mode is a
                // trap: the toolbar is gone and F11 is not guessable.
                if let Some(until) = self.hint_until {
                    if std::time::Instant::now() < until {
                        let anchor = ui.max_rect().center_top() + egui::vec2(0.0, 24.0);
                        egui::Area::new("immersive_hint".into())
                            .fixed_pos(anchor - egui::vec2(150.0, 0.0))
                            .order(egui::Order::Foreground)
                            .show(ui.ctx(), |ui| {
                                egui::Frame::new()
                                    .fill(egui::Color32::from_black_alpha(200))
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::symmetric(14, 10))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new("Immersive — press F11 to leave")
                                                .color(theme::TEXT),
                                        );
                                    });
                            });
                        ui.ctx().request_repaint();
                    } else {
                        self.hint_until = None;
                    }
                }
            });
    }

    fn video(&mut self, ui: &mut egui::Ui) {
        let Some(session) = &self.session else { return };

        // Upload only when the decoder has produced something new.
        {
            let frame = session.frame.lock().expect("frame slot poisoned");
            if !frame.pixels.is_empty() && frame.generation != self.shown_generation {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.pixels,
                );
                self.shown_generation = frame.generation;
                match &mut self.texture {
                    Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        self.texture = Some(ui.ctx().load_texture(
                            "screen",
                            image,
                            egui::TextureOptions::LINEAR,
                        ))
                    }
                }
            }
        }

        let Some(texture) = &self.texture else {
            ui.centered_and_justified(|ui| {
                ui.spinner();
            });
            return;
        };

        // Letterbox to the host's aspect ratio, and work out where the image
        // actually lands rather than asking the layout afterwards.
        //
        // This used to draw through `centered_and_justified` and map pointer
        // positions with the returned response rect. A justified child
        // reports the *whole* panel, letterbox bars included, so every
        // coordinate sent to the host was shifted by the bar size — around
        // 60 px vertically at a typical window shape. Clicks landed below
        // where they looked, which is enough to miss a title bar completely:
        // its buttons stopped working and the window could not be dragged,
        // while clicks on large targets inside the window still seemed fine.
        let image_rect = letterbox(ui.max_rect(), texture.size_vec2());

        egui::Image::new(egui::load::SizedTexture::new(texture.id(), image_rect.size()))
            .paint_at(ui, image_rect);

        // The same rect for drawing and for input, so the two cannot drift.
        self.forward_input(ui, image_rect);
    }

    /// Translate this frame's egui input into protocol events.
    fn forward_input(&mut self, ui: &mut egui::Ui, video: egui::Rect) {
        // A focused widget in this window eats keys the robot should get —
        // egui uses Tab, the arrows, space and Enter to move between and
        // activate controls. Give focus up whenever the operator is working
        // in the remote screen rather than in the settings panel.
        let pointer_over_video = ui
            .input(|i| i.pointer.latest_pos())
            .is_some_and(|p| video.contains(p));
        if self.immersive || pointer_over_video {
            ui.ctx().memory_mut(|m| m.stop_text_input());
        }

        if self.view_only {
            return;
        }
        let Some(session) = &self.session else { return };
        let (Some(tx), Some(clipboard_tx)) = (session.input(), session.clipboard()) else {
            return;
        };
        let (tx, clipboard_tx) = (tx.clone(), clipboard_tx.clone());
        let send = |e: InputEvent| {
            let _ = tx.send(e);
        };

        let to_normalized = |p: egui::Pos2| normalize(p, video);

        // While a button is held, motion smaller than this (in normalized
        // screen units, about 6 px on a 1920-wide host) is not forwarded.
        // A small viewer image is mapped onto a large host screen, so hand
        // jitter of a couple of pixels here arrives as ten or more there —
        // past GTK's drag threshold, which turns a click on a headerbar
        // button into a window drag. Real drags clear this easily.
        const HELD_DEADZONE: f32 = 6.0 / 1920.0;

        /// Points in one scrolled line, for platforms that report pixels.
        /// Matches the step GTK and Qt use for a wheel notch.
        const POINTS_PER_LINE: f32 = 50.0;
        /// Lines in one page, for the rare platform that reports pages.
        const LINES_PER_PAGE: f32 = 20.0;
        /// Ceiling on one event, so a stuck or hostile delta cannot make the
        /// host inject thousands of button clicks.
        const MAX_SCROLL_LINES: f32 = 20.0;

        ui.input(|i| {
            // Modifiers are flags in egui, not key events; mirror transitions
            // so the host sees Ctrl and Shift go down and up.
            for (idx, (key, down)) in keymap::modifier_keys(i.modifiers).into_iter().enumerate() {
                if self.modifiers[idx] != down {
                    self.modifiers[idx] = down;
                    send(InputEvent::Key { key, down });
                }
            }

            // Losing the window (alt-tab, click elsewhere) while a button is
            // down means its release will never arrive here. Release it on
            // the host now rather than leave the robot in a drag.
            if !i.focused && !self.held_buttons.is_empty() {
                for b in self.held_buttons.drain(..) {
                    send(InputEvent::MouseButton { button: b, down: false });
                }
            }

            if let Some(p) = i.pointer.latest_pos() {
                let inside = video.contains(p);
                // While dragging, keep tracking past the image edge (clamped)
                // so a drag to the edge continues instead of freezing.
                if inside || !self.held_buttons.is_empty() {
                    let (x, y) = to_normalized(p);
                    let moved_enough = match self.last_sent_pos {
                        Some((lx, ly)) if !self.held_buttons.is_empty() => {
                            (x - lx).abs() > HELD_DEADZONE || (y - ly).abs() > HELD_DEADZONE
                        }
                        Some((lx, ly)) => x != lx || y != ly,
                        None => true,
                    };
                    if moved_enough {
                        self.last_sent_pos = Some((x, y));
                        send(InputEvent::MouseMove { x, y });
                    }
                }
                self.pointer_in_video = inside;
            }

            for event in &i.events {
                match event {
                    egui::Event::PointerButton { button, pressed, pos, .. } => {
                        let Some(b) = keymap::mouse_button(*button) else { continue };
                        if *pressed {
                            // Only a press has to start inside the image.
                            if !video.contains(*pos) {
                                continue;
                            }
                            // Land the press exactly where it was clicked,
                            // independent of whatever motion was last sent.
                            let (x, y) = to_normalized(*pos);
                            self.last_sent_pos = Some((x, y));
                            send(InputEvent::MouseMove { x, y });
                            if !self.held_buttons.contains(&b) {
                                self.held_buttons.push(b);
                            }
                            send(InputEvent::MouseButton { button: b, down: true });
                        } else {
                            // A release is forwarded no matter where the
                            // pointer ended up: the close button sits on the
                            // image's edge, and a release a pixel past it
                            // used to leave the button held on the robot —
                            // after which every click became a drag.
                            self.held_buttons.retain(|h| *h != b);
                            send(InputEvent::MouseButton { button: b, down: false });
                        }
                    }
                    egui::Event::MouseWheel { unit, delta, .. } if self.pointer_in_video => {
                        // The protocol speaks lines, but egui reports whatever
                        // unit the platform used, and a mouse wheel on Windows
                        // reports *lines* — one per notch. Dividing that by 40
                        // as though it were pixels sent 0.025, which the host
                        // rounded to zero clicks: the wheel did nothing at all.
                        let lines = match unit {
                            egui::MouseWheelUnit::Line => *delta,
                            egui::MouseWheelUnit::Point => *delta / POINTS_PER_LINE,
                            egui::MouseWheelUnit::Page => *delta * LINES_PER_PAGE,
                        };
                        // Carry the fraction instead of rounding it away, so a
                        // trackpad's stream of small deltas still adds up.
                        self.scroll_carry += lines;
                        let whole = egui::vec2(
                            self.scroll_carry.x.trunc().clamp(-MAX_SCROLL_LINES, MAX_SCROLL_LINES),
                            self.scroll_carry.y.trunc().clamp(-MAX_SCROLL_LINES, MAX_SCROLL_LINES),
                        );
                        if whole != egui::Vec2::ZERO {
                            self.scroll_carry -= whole;
                            send(InputEvent::MouseWheel { dx: whole.x, dy: whole.y });
                        }
                    }
                    // F11 toggles immersive mode, so it is the viewer's key
                    // rather than the robot's. Everything else goes through.
                    egui::Event::Key { key: egui::Key::F11, .. } => {}
                    egui::Event::Key { key, pressed, repeat, .. } => {
                        if *repeat {
                            continue;
                        }
                        if let Some(k) = keymap::key(*key) {
                            send(InputEvent::Key { key: k, down: *pressed });
                        }
                    }
                    // Only what the physical-key path cannot express, so
                    // ASCII does not arrive twice.
                    egui::Event::Text(text) if !text.is_ascii() => {
                        send(InputEvent::Text { text: text.clone() });
                    }
                    // egui turns Ctrl+C and Ctrl+X into these and swallows the
                    // letter, so the host would otherwise see the modifier go
                    // down and up with nothing in between. Synthesise the key
                    // it kept; the Ctrl itself is already mirrored above.
                    egui::Event::Copy => {
                        send(InputEvent::Key { key: telekin_proto::KeyCode::C, down: true });
                        send(InputEvent::Key { key: telekin_proto::KeyCode::C, down: false });
                    }
                    egui::Event::Cut => {
                        send(InputEvent::Key { key: telekin_proto::KeyCode::X, down: true });
                        send(InputEvent::Key { key: telekin_proto::KeyCode::X, down: false });
                    }
                    // Ctrl+V here means "paste into the robot". Push the text
                    // to the host's clipboard first; the Ctrl+V keystroke
                    // itself still goes through the normal key path, so the
                    // remote application performs its own paste.
                    // Same swallowing as Copy: push the text to the robot's
                    // clipboard first, then press the key that makes the
                    // remote application read it.
                    egui::Event::Paste(text) => {
                        if text != &self.last_clipboard
                            && text.len() <= telekin_proto::MAX_CLIPBOARD_BYTES
                        {
                            self.last_clipboard = text.clone();
                            let _ = clipboard_tx.send(text.clone());
                        }
                        send(InputEvent::Key { key: telekin_proto::KeyCode::V, down: true });
                        send(InputEvent::Key { key: telekin_proto::KeyCode::V, down: false });
                    }
                    _ => {}
                }
            }
        });
    }
}

/// Every network a robot answered on, for the second line of its row.
///
/// A robot with a cable and WiFi both up advertises both. Showing only the
/// first hid the wired address completely, which reads as "the scan cannot
/// see my LAN" when in fact it had already found it — the operator has no
/// other way to tell.
///
/// IPv6 is left out when there is any IPv4: those addresses are long, mostly
/// link-local, and never what anyone types into the address box.
fn address_summary(addrs: &[std::net::SocketAddr]) -> String {
    /// Beyond this the line is wider than the panel, so the rest is counted.
    const SHOWN: usize = 3;
    const SEP: &str = "  ·  ";

    let v4: Vec<String> = addrs
        .iter()
        .filter(|a| a.is_ipv4())
        .map(|a| a.ip().to_string())
        .collect();

    if v4.is_empty() {
        // IPv6-only is unusual, but the row should still say something true
        // rather than claim the robot has no address at all.
        return match addrs.first() {
            Some(a) => a.ip().to_string(),
            None => "no address".to_string(),
        };
    }
    if v4.len() <= SHOWN {
        return v4.join(SEP);
    }
    format!("{}{SEP}+{} more", v4[..SHOWN].join(SEP), v4.len() - SHOWN)
}

/// Largest rect with `content`'s aspect ratio that fits inside `available`,
/// centred. This is the rect the frame is painted into *and* the rect pointer
/// positions are measured against; they must be the same one.
fn letterbox(available: egui::Rect, content: egui::Vec2) -> egui::Rect {
    if content.x <= 0.0 || content.y <= 0.0 {
        return available;
    }
    let scale = (available.width() / content.x).min(available.height() / content.y);
    egui::Rect::from_center_size(available.center(), content * scale)
}

/// Where a pointer sits within `video`, as a fraction of it. Clamped, so a
/// drag that leaves the image keeps pushing against the host's edge instead
/// of jumping somewhere unrelated.
fn normalize(p: egui::Pos2, video: egui::Rect) -> (f32, f32) {
    (
        ((p.x - video.min.x) / video.width().max(1.0)).clamp(0.0, 1.0),
        ((p.y - video.min.y) / video.height().max(1.0)).clamp(0.0, 1.0),
    )
}


/// The version label, clickable to run the update check again.
fn recheck_label(ui: &mut egui::Ui, text: &str) -> bool {
    let response = ui
        .add(
            egui::Label::new(egui::RichText::new(text).size(12.0).color(theme::MUTED))
                .sense(egui::Sense::click()),
        )
        .on_hover_text("Click to check for a newer version now");
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// Which columns a file row has room for, given its width.
///
/// The date goes first: it is the least often needed and the widest. Split
/// out from the drawing so the thresholds can be checked without a window.
pub(crate) fn meta_columns(room: f32) -> (bool, bool) {
    (room > 320.0, room > 190.0)
}

/// Whether the Desktop/Files switch can be pressed.
///
/// Going *to* files is always possible; going to the desktop needs something
/// to look at. Split out so the rule can be tested without a window.
fn files_switch_enabled(showing_files: bool, screen_available: bool) -> bool {
    !showing_files || screen_available
}

/// A label on the left, a machine value right-aligned. Monospace keeps the
/// column steady while the numbers change.
fn readout(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        theme::field_label(ui, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(theme::value(value));
        });
    });
}

/// A small labelled figure for the toolbar.
fn stat(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.add_space(14.0);
    ui.label(
        egui::RichText::new(label)
            .size(12.0)
            .color(theme::BAR_MUTED),
    );
    ui.add_space(4.0);
    // Explicit rather than `theme::value`, which is coloured for paper and
    // would be near-invisible here.
    ui.label(
        egui::RichText::new(value)
            .monospace()
            .size(15.0)
            .color(theme::BAR_TEXT),
    );
}

/// A stage cost as a labelled bar. Reading two numbers against each other is
/// harder than seeing which bar is longer, and that comparison is the whole
/// point of this panel.
fn meter(ui: &mut egui::Ui, label: &str, ms: f32) {
    // 120 ms is about where a session stops feeling direct, so it makes a
    // meaningful full scale.
    let fraction = (ms / 120.0).clamp(0.0, 1.0);
    let colour = if ms < 25.0 {
        theme::GOOD
    } else if ms < 70.0 {
        theme::ACCENT
    } else {
        ui.visuals().warn_fg_color
    };

    ui.horizontal(|ui| {
        theme::field_label(ui, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("{ms:.1} ms")).color(theme::TEXT));
        });
    });
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 6.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(3), theme::track_colour());
    let mut filled = rect;
    filled.set_width(rect.width() * fraction);
    painter.rect_filled(filled, egui::CornerRadius::same(3), colour);
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render one frame of a screen and return every string it painted.
    ///
    /// The connection screen is the part of this app a user can be blocked
    /// by, and "I clicked scan and nothing appeared" is not something the
    /// type checker can catch. Driving the real widget code headlessly and
    /// reading back the painted text is the only way to know a row actually
    /// reaches the screen.
    fn painted_text(app: &mut ViewerApp, size: egui::Vec2) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };

        // `run_ui` hands over the same root Ui that eframe builds, so this
        // exercises the real path rather than an approximation of it.
        let output = ctx.run_ui(input, |ui| app.connect_screen(ui, None));

        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut found);
        }
        found
    }

    fn app_with(hosts: Vec<Discovered>) -> ViewerApp {
        let args = <crate::Args as clap::Parser>::parse_from(["telekin"]);
        let mut app = ViewerApp::new(&args);
        app.hosts = hosts;
        app
    }

    fn robot(n: u32) -> Discovered {
        Discovered {
            name: format!("robot-{n:02}"),
            addrs: vec![format!("192.168.1.{n}:9631").parse().expect("address")],
            fingerprint: None,
            proto_version: Some(telekin_proto::PROTO_VERSION),
            hostname: Some(format!("tango-{n:02}")),
            account: Some("tangox".into()),
        }
    }

    fn a_robot() -> Discovered {
        Discovered {
            name: "tangox".into(),
            addrs: vec![
                "192.168.1.118:9631".parse().expect("address"),
                "192.168.2.100:9631".parse().expect("address"),
            ],
            fingerprint: Some("aa:bb".into()),
            proto_version: Some(telekin_proto::PROTO_VERSION),
            hostname: Some("tango-desktop".into()),
            account: Some("tangox".into()),
        }
    }

    /// The file panes must survive being drawn with no session behind them.
    ///
    /// They are only reached inside one, but every "no session" path in there
    /// is a branch nothing else exercises, and a panic in a file manager is a
    /// lost window rather than a lost pixel.
    #[test]
    fn the_desktop_switch_is_offered_only_when_there_is_a_desktop() {
        // On files, with a screen: the operator can go and look.
        assert!(files_switch_enabled(true, true));
        // On files, robot sitting at its login screen: nothing to show.
        assert!(!files_switch_enabled(true, false));
        // On the desktop: switching to files is always allowed, and is the
        // way out of a session that cannot render anything.
        assert!(files_switch_enabled(false, false));
        assert!(files_switch_enabled(false, true));
    }

    #[test]
    fn the_file_panes_draw_without_a_session() {
        let mut app = app_with(Vec::new());
        app.mode = Mode::Files;

        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 800.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| app.files_screen(ui));

        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut text = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut text);
        }

        // Tracked capitals, so match on the letters rather than the spacing.
        let flat: String = text.join(" ").chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flat.contains("THISCOMPUTER"), "no local pane: {text:?}");
        assert!(flat.contains("ROBOT"), "no robot pane: {text:?}");
        assert!(flat.contains("TRANSFERS"), "no transfer list: {text:?}");
        // The arrows are drawn shapes, not glyphs — egui's bundled fonts do
        // not cover them, and the first version of this shipped three empty
        // boxes. Count line segments instead: three arrows, three strokes
        // each, plus the two `up` buttons on the panes.
        fn segments(shape: &egui::Shape) -> usize {
            match shape {
                egui::Shape::LineSegment { .. } => 1,
                egui::Shape::Vec(v) => v.iter().map(segments).sum(),
                _ => 0,
            }
        }
        let lines: usize = output.shapes.iter().map(|c| segments(&c.shape)).sum();
        assert!(
            lines >= 9,
            "the copy arrows were not drawn: only {lines} line segments on the whole screen"
        );
    }

    /// Above `C:\` the local pane lists the drives. The first version of the
    /// panes stopped dead at the drive root, and the only way onto `D:` was
    /// to know that the address box accepted a typed path.
    #[cfg(windows)]
    #[test]
    fn the_local_pane_lists_drives_above_the_drive_root() {
        let mut app = app_with(Vec::new());
        app.mode = Mode::Files;
        app.local_dir = std::path::PathBuf::new();
        app.local = Some(crate::files::local_listing(&app.local_dir).expect("drive list"));

        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 800.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| app.files_screen(ui));

        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut text = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut text);
        }
        assert!(
            text.iter().any(|t| t.eq_ignore_ascii_case("C:\\")),
            "no drive row on screen: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("Pick a drive")),
            "the empty address box should say what to do: {text:?}"
        );
    }

    /// Text painted by the file panes, with positions, at a given size.
    fn file_pane_text(width: f32, height: f32) -> Vec<(egui::Pos2, String)> {
        let mut app = app_with(Vec::new());
        app.mode = Mode::Files;
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| app.files_screen(ui));

        fn walk(shape: &egui::Shape, out: &mut Vec<(egui::Pos2, String)>) {
            match shape {
                egui::Shape::Text(t) => out.push((t.pos, t.galley.text().to_owned())),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut found);
        }
        found
    }

    /// A shrunk window must not push controls out of it.
    ///
    /// This is the failure the user hit: a pane dragged wide kept its pixel
    /// width when the window was made small, the robot side was squeezed to a
    /// strip, names and dates were painted over each other, and Delete left
    /// the screen entirely.
    #[test]
    fn nothing_leaves_the_window_when_it_is_shrunk() {
        const W: f32 = 1000.0;
        let text = file_pane_text(W, 700.0);
        assert!(!text.is_empty(), "nothing was drawn at all");

        for (pos, label) in &text {
            assert!(
                pos.x >= -1.0 && pos.x < W,
                "{label:?} was painted at x={} , outside a {W}px window",
                pos.x
            );
        }

        // Both panes still exist, and the robot side is not a sliver.
        let flat: String = text
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(flat.contains("THISCOMPUTER"), "local pane gone: {text:?}");
        assert!(flat.contains("ROBOT"), "robot pane gone: {text:?}");
    }

    /// Shrinking a window the panes were already laid out in.
    ///
    /// The real failure needed *history*: a resizable panel stores its width
    /// in pixels, so one laid out on a maximised window kept that width when
    /// the window was made small and squeezed the other pane to a strip. A
    /// fresh context cannot reproduce that, so this reuses one across two
    /// frames — wide, then narrow — which is what the operator did.
    #[test]
    fn a_pane_sized_on_a_wide_window_gives_way_when_it_shrinks() {
        let mut app = app_with(Vec::new());
        app.mode = Mode::Files;
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);

        let frame = |ctx: &egui::Context, app: &mut ViewerApp, w: f32| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(w, 700.0),
                )),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| app.files_screen(ui));
            fn walk(shape: &egui::Shape, out: &mut Vec<(egui::Pos2, String)>) {
                match shape {
                    egui::Shape::Text(t) => out.push((t.pos, t.galley.text().to_owned())),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut found = Vec::new();
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut found);
            }
            found
        };

        frame(&ctx, &mut app, 1900.0);
        let narrow = frame(&ctx, &mut app, 1000.0);

        // The robot heading marks where its pane begins. Everything from
        // there to the window edge is the space it has to work in.
        let robot_x = narrow
            .iter()
            .find(|(_, t)| t.chars().filter(|c| !c.is_whitespace()).eq("ROBOT".chars()))
            .map(|(pos, _)| pos.x)
            .expect("the robot pane vanished entirely");
        assert!(
            1000.0 - robot_x > 260.0,
            "the robot pane was squeezed to {:.0}px by the pane beside it",
            1000.0 - robot_x
        );
        for (pos, label) in &narrow {
            assert!(pos.x < 1000.0, "{label:?} was pushed off the window at x={}", pos.x);
        }
    }

    #[test]
    fn a_narrow_row_drops_columns_rather_than_overlapping() {
        // Wide enough for everything.
        assert_eq!(meta_columns(500.0), (true, true));
        // Middle: the date is the first thing to go, being the least useful.
        assert_eq!(meta_columns(250.0), (false, true));
        // Very narrow: the name is all there is room for.
        assert_eq!(meta_columns(120.0), (false, false));
    }

    /// Bigger type must not push the connection screen out of its window.
    ///
    /// Type sizes went up across the board for legibility beside a robot;
    /// this is the check that legibility did not cost anyone the Connect
    /// button at the default window size or at the minimum one.
    /// A fleet's worth of robots must not push the sign-in off the screen.
    ///
    /// The deployment this is built for is twenty robots on one network, so
    /// "one was found" is the unusual case, not the normal one.
    #[test]
    fn a_page_holds_more_robots_on_a_taller_window() {
        // The minimum window still shows a usable handful.
        assert_eq!(rows_per_page(560.0), 3);
        assert_eq!(rows_per_page(780.0), 4);
        assert_eq!(rows_per_page(900.0), 5);
        // And a big screen does not turn into one enormous page.
        assert_eq!(rows_per_page(4000.0), 8);
    }

    #[test]
    fn a_shorter_scan_result_cannot_strand_the_viewer_on_an_empty_page() {
        let mut app = app_with((1..=20).map(robot).collect());
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let draw = |ctx: &egui::Context, app: &mut ViewerApp| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1180.0, 780.0),
                )),
                ..Default::default()
            };
            let out = ctx.run_ui(input, |ui| app.connect_screen(ui, None));
            fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut text = Vec::new();
            for c in &out.shapes {
                walk(&c.shape, &mut text);
            }
            text
        };

        app.page = 4;
        draw(&ctx, &mut app);
        // A rescan finds two robots; the old page number points past the end.
        app.hosts = vec![robot(1), robot(2)];
        let text = draw(&ctx, &mut app);
        assert!(
            text.iter().any(|t| t == "robot-01"),
            "left on an empty page after the list shrank: {text:?}"
        );
        assert_eq!(app.page, 0, "the page was not brought back into range");
    }

    #[test]
    fn a_long_scan_result_scrolls_instead_of_growing_the_page() {
        let fleet: Vec<Discovered> = (1..=20)
            .map(|n| Discovered {
                name: format!("robot-{n:02}"),
                addrs: vec![format!("192.168.1.{n}:9631").parse().expect("address")],
                fingerprint: None,
                proto_version: Some(telekin_proto::PROTO_VERSION),
                hostname: Some(format!("tango-{n:02}")),
                account: Some("tangox".into()),
            })
            .collect();

        for (w, h) in [(1180.0, 780.0), (640.0, 560.0)] {
            let mut app = app_with(fleet.clone());
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(w, h),
                )),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| app.connect_screen(ui, None));

            fn walk(shape: &egui::Shape, out: &mut Vec<(egui::Pos2, String)>) {
                match shape {
                    egui::Shape::Text(t) => out.push((t.pos, t.galley.text().to_owned())),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut text = Vec::new();
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut text);
            }

            assert!(
                text.iter().any(|(_, t)| t == "Connect"),
                "twenty robots pushed Connect off a {w}x{h} window"
            );
            for (pos, label) in &text {
                assert!(
                    pos.y < h,
                    "{label:?} was drawn below the bottom of a {h}px window (y={})",
                    pos.y
                );
            }
            // The list is bounded, so only the first few robots are painted;
            // the rest are reached by scrolling rather than by the page
            // getting taller.
            let painted = text.iter().filter(|(_, t)| t.starts_with("robot-")).count();
            assert!(
                painted < fleet.len(),
                "every one of {} robots was laid out at once at {w}x{h}",
                fleet.len()
            );
        }
    }

    #[test]
    fn the_connection_screen_fits_the_window_it_is_given() {
        // The default size, and the smallest the window may be made.
        for (w, h) in [(1180.0, 780.0), (640.0, 560.0)] {
            let mut app = app_with(vec![a_robot()]);
            let ctx = egui::Context::default();
            crate::theme::apply(&ctx);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(w, h),
                )),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| app.connect_screen(ui, None));

            fn walk(shape: &egui::Shape, out: &mut Vec<(egui::Pos2, String)>) {
                match shape {
                    egui::Shape::Text(t) => out.push((t.pos, t.galley.text().to_owned())),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            let mut text = Vec::new();
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut text);
            }

            assert!(!text.is_empty(), "nothing drawn at {w}x{h}");
            for (pos, label) in &text {
                assert!(
                    pos.x >= -1.0 && pos.x < w,
                    "{label:?} sits at x={} in a {w}px window",
                    pos.x
                );
            }
            assert!(
                text.iter().any(|(_, t)| t == "Connect"),
                "the Connect button was pushed off a {w}x{h} window"
            );
        }
    }

    #[test]
    fn the_connection_screen_paints_at_all() {
        let text = painted_text(&mut app_with(Vec::new()), egui::vec2(1100.0, 800.0));
        assert!(
            text.iter().any(|t| t.contains("Telekin")),
            "no wordmark painted; the harness is not rendering: {text:?}"
        );
        assert!(
            text.iter().any(|t| t == AUTHOR),
            "the author credit is missing from the first screen: {text:?}"
        );
    }

    #[test]
    fn a_scanned_robot_reaches_the_screen() {
        let text = painted_text(&mut app_with(vec![a_robot()]), egui::vec2(1100.0, 800.0));
        assert!(
            text.iter().any(|t| t.contains("tangox")),
            "the discovered robot was never painted: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("tango-desktop")),
            "the computer name was never painted: {text:?}"
        );
        // The bug this guards: the robot answered on both WiFi and the cable,
        // and the row showed only WiFi — so a wired LAN looked undiscovered.
        assert!(
            text.iter().any(|t| t.contains("192.168.2.100")),
            "the wired address was found but never shown: {text:?}"
        );
    }

    #[test]
    fn picking_a_two_homed_robot_offers_its_networks() {
        let mut app = app_with(vec![a_robot()]);
        app.choose(&a_robot());
        let text = painted_text(&mut app, egui::vec2(1100.0, 800.0));
        assert!(
            text.iter().any(|t| t == "Network"),
            "no network picker for a robot on two LANs: {text:?}"
        );
        assert!(
            text.iter().any(|t| t.contains("Automatic")),
            "the picker did not default to trying every address: {text:?}"
        );
    }

    #[test]
    fn a_single_homed_robot_gets_no_pointless_choice() {
        let mut only_wifi = a_robot();
        only_wifi.addrs = vec!["192.168.1.118:9631".parse().expect("address")];
        let mut app = app_with(vec![only_wifi.clone()]);
        app.choose(&only_wifi);
        let text = painted_text(&mut app, egui::vec2(1100.0, 800.0));
        assert!(
            !text.iter().any(|t| t == "Network"),
            "offered a choice of one network: {text:?}"
        );
    }

    #[test]
    fn choosing_another_robot_drops_the_pinned_network() {
        let mut app = app_with(vec![a_robot()]);
        app.chosen_addr = Some("192.168.2.100:9631".parse().expect("address"));
        app.choose(&a_robot());
        assert_eq!(
            app.chosen_addr, None,
            "a network pinned on one robot leaked onto the next"
        );
    }

    #[test]
    fn both_networks_are_listed() {
        let summary = address_summary(&a_robot().addrs);
        assert!(summary.contains("192.168.1.118"), "{summary}");
        assert!(summary.contains("192.168.2.100"), "{summary}");
    }

    #[test]
    fn ipv6_is_hidden_behind_ipv4() {
        let addrs = vec![
            "192.168.1.118:9631".parse().expect("address"),
            "[fe80::1]:9631".parse().expect("address"),
        ];
        assert_eq!(address_summary(&addrs), "192.168.1.118");
    }

    #[test]
    fn an_ipv6_only_robot_still_shows_something() {
        let addrs = vec!["[fe80::1]:9631".parse().expect("address")];
        assert_eq!(address_summary(&addrs), "fe80::1");
        assert_eq!(address_summary(&[]), "no address");
    }

    #[test]
    fn a_crowded_robot_is_counted_rather_than_wrapped() {
        let addrs: Vec<std::net::SocketAddr> = (1..=6)
            .map(|n| format!("10.0.0.{n}:9631").parse().expect("address"))
            .collect();
        let summary = address_summary(&addrs);
        assert!(summary.ends_with("+3 more"), "{summary}");
        assert!(!summary.contains("10.0.0.4"), "{summary}");
    }

    #[test]
    fn a_scanned_robot_survives_a_narrow_window() {
        // Two columns inside a scroll area: the list is the first thing that
        // loses room when the window is small.
        let text = painted_text(&mut app_with(vec![a_robot()]), egui::vec2(640.0, 560.0));
        assert!(
            text.iter().any(|t| t.contains("tangox")),
            "the discovered robot vanished in a narrow window: {text:?}"
        );
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    #[test]
    fn letterbox_keeps_aspect_and_centres() {
        // A 16:9 host shown in a taller panel: bars above and below.
        let panel = rect(0.0, 0.0, 1600.0, 1000.0);
        let img = letterbox(panel, egui::vec2(1920.0, 1080.0));
        assert_eq!(img.width(), 1600.0);
        assert!((img.height() - 900.0).abs() < 0.01);
        assert!((img.min.y - 50.0).abs() < 0.01, "expected 50px bars, got {}", img.min.y);
        assert_eq!(img.center(), panel.center());
    }

    #[test]
    fn top_of_image_maps_to_top_of_host() {
        // The regression that broke title bars: mapping against the panel
        // instead of the image put the host's y=0 at the top of the *bars*,
        // so every click landed low by the bar height.
        let panel = rect(0.0, 0.0, 1600.0, 1000.0);
        let img = letterbox(panel, egui::vec2(1920.0, 1080.0));

        let (_, y) = normalize(egui::pos2(800.0, img.min.y), img);
        assert!(y.abs() < 1e-6, "top edge should map to 0.0, got {y}");

        // Mapping against the panel would have put it here instead.
        let (_, wrong) = normalize(egui::pos2(800.0, img.min.y), panel);
        assert!(wrong > 0.04, "sanity: the old behaviour really was offset");
        // On a 1080-high host that is a ~54 px error — enough to miss a
        // title bar entirely.
        assert!((wrong * 1080.0) > 45.0);
    }

    #[test]
    fn corners_and_centre_are_exact() {
        let panel = rect(20.0, 60.0, 1200.0, 700.0);
        let img = letterbox(panel, egui::vec2(1920.0, 1080.0));

        assert_eq!(normalize(img.min, img), (0.0, 0.0));
        assert_eq!(normalize(img.max, img), (1.0, 1.0));
        let (cx, cy) = normalize(img.center(), img);
        assert!((cx - 0.5).abs() < 1e-6 && (cy - 0.5).abs() < 1e-6);
    }

    #[test]
    fn outside_the_image_is_clamped() {
        let img = rect(0.0, 0.0, 800.0, 450.0);
        assert_eq!(normalize(egui::pos2(-50.0, -50.0), img), (0.0, 0.0));
        assert_eq!(normalize(egui::pos2(9999.0, 9999.0), img), (1.0, 1.0));
    }

    #[test]
    fn wide_panel_gets_side_bars() {
        // The mirror case: a panel wider than the host aspect.
        let panel = rect(0.0, 0.0, 2000.0, 900.0);
        let img = letterbox(panel, egui::vec2(1920.0, 1080.0));
        assert_eq!(img.height(), 900.0);
        assert!((img.width() - 1600.0).abs() < 0.01);
        assert!((img.min.x - 200.0).abs() < 0.01);
    }
}

#[cfg(test)]
mod layout_dump {
    #[test]
    #[ignore]
    fn dump() {
        let args = <crate::Args as clap::Parser>::parse_from(["telekin"]);
        let mut app = super::ViewerApp::new(&args);
        app.hosts = (1..=20)
            .map(|n| telekin_transport::discovery::Discovered {
                name: format!("robot-{n:02}"),
                addrs: vec![format!("192.168.1.{n}:9631").parse().expect("addr")],
                fingerprint: None,
                proto_version: None,
                hostname: Some(format!("tango-{n:02}")),
                account: Some("tangox".into()),
            })
            .collect();
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1870.0, 900.0),
            )),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| app.connect_screen(ui, None));
        fn walk(s: &egui::Shape, o: &mut Vec<(egui::Pos2, String)>) {
            match s {
                egui::Shape::Text(t) => o.push((t.pos, t.galley.text().to_owned())),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, o)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for c in &out.shapes {
            walk(&c.shape, &mut found);
        }
        for (pos, text) in found.iter().take(40) {
            println!("text  {:7.0},{:7.0}  {:?}", pos.x, pos.y, text);
        }
        // The arrows are strokes, so they need looking at separately.
        fn lines(s: &egui::Shape, o: &mut Vec<(egui::Pos2, egui::Pos2)>) {
            match s {
                egui::Shape::LineSegment { points, .. } => o.push((points[0], points[1])),
                egui::Shape::Vec(v) => v.iter().for_each(|s| lines(s, o)),
                _ => {}
            }
        }
        let mut segs = Vec::new();
        for c in &out.shapes {
            lines(&c.shape, &mut segs);
        }
        println!("{} line segments", segs.len());
        for (a, b) in segs.iter() {
            println!("line  {:7.0},{:7.0} -> {:7.0},{:7.0}", a.x, a.y, b.x, b.y);
        }
    }
}
