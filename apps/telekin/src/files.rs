//! The viewer's side of file transfer: what the UI asks for, and what it is
//! told back.
//!
//! The UI thread never touches the network and the network thread never
//! touches egui. Between them sit a command channel going one way and this
//! shared state coming back, which is the same arrangement the video path
//! already uses.
//!
//! Transfers here are deliberately not tied to the screen stream. A session
//! can sign in, copy a bag file off a robot and disconnect without the
//! encoder ever starting — on a machine where CPU is the scarce resource,
//! that is the difference between a background chore and a visible cost.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use telekin_proto::DirListing;

/// What the UI asks the session to do.
#[derive(Debug, Clone)]
pub enum Command {
    List { path: String },
    MakeDir { path: String },
    Remove { path: String },
    /// One file. `batch` ties it to a folder copy, when it is part of one.
    Download { remote: String, local: PathBuf, batch: Option<u64> },
    Upload { local: PathBuf, remote: String, batch: Option<u64> },
    /// A whole folder, robot to here. Lands as `local_parent/<folder name>`.
    DownloadTree { remote: String, local_parent: PathBuf },
    /// A whole folder, here to the robot. Lands as `remote_parent/<folder name>`.
    UploadTree { local: PathBuf, remote_parent: String },
    /// Create a folder on the robot, parents included, for a folder copy.
    MakeDirAll { path: String },
    /// Fetch a robot file to edit in place.
    Open { path: String },
    /// Write an edited file back to the robot.
    Save { path: String, text: String },
}

/// Which way a transfer is going. Named for the operator's mental model
/// rather than the protocol's: nobody thinks in "uni stream direction".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToRobot,
    FromRobot,
}

#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: u64,
    /// Just the file name — the full paths are on the panes behind it.
    pub name: String,
    pub direction: Direction,
    pub done: u64,
    /// Zero while a download's size is still unknown.
    pub total: u64,
    /// `None` while running; `Some(Ok(()))` or the reason it stopped.
    pub outcome: Option<Result<(), String>>,
    /// The folder copy this belongs to, if any.
    pub batch: Option<u64>,
}

/// A folder being copied: one row in the transfer list standing for every
/// file under it. Fifty rows scrolling past would say less than "37 of 50".
#[derive(Debug, Clone)]
pub struct Batch {
    pub id: u64,
    pub name: String,
    pub direction: Direction,
    /// Files still waiting their turn in the queue.
    pub queued: usize,
    /// Set when the robot cut the listing off at its limit.
    pub truncated: bool,
    /// Something that stopped the whole copy, before any file moved.
    pub error: Option<String>,
}

/// What the transfer list shows for a batch, added up from its files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchProgress {
    pub files_total: usize,
    pub files_done: usize,
    pub files_failed: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

impl BatchProgress {
    pub fn finished(&self) -> bool {
        self.files_done + self.files_failed == self.files_total
    }
}

impl Transfer {
    /// Progress as a fraction, or `None` when the size is not known yet — a
    /// bar that invents a length is worse than one that admits it cannot.
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).clamp(0.0, 1.0))
    }

    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }
}

/// A robot file open in the editor.
#[derive(Debug, Clone, Default)]
pub struct Editor {
    pub path: String,
    /// What is in the box now.
    pub text: String,
    /// What the robot last confirmed, so "changed" is a fact rather than a
    /// guess from keystrokes — an undo back to the original counts as clean.
    pub saved: String,
    /// True between asking for the file and its arriving.
    pub loading: bool,
    /// True between pressing Save and the robot confirming.
    pub saving: bool,
    pub error: Option<String>,
}

impl Editor {
    pub fn dirty(&self) -> bool {
        self.text != self.saved
    }

    /// The file name alone, for a title that fits.
    pub fn name(&self) -> &str {
        remote_file_name(&self.path)
    }
}

/// Everything the file panes read, written by the session thread.
#[derive(Debug, Default)]
pub struct State {
    /// The robot directory currently shown.
    pub listing: Option<DirListing>,
    /// A listing has been asked for and has not come back.
    pub loading: bool,
    /// The last thing that went wrong, for the pane to show and the operator
    /// to dismiss.
    pub error: Option<String>,
    pub transfers: Vec<Transfer>,
    /// The file being edited, if any.
    pub editor: Option<Editor>,
    /// Folder copies in progress or finished.
    pub batches: Vec<Batch>,
    /// Transfers waiting to start. A folder of five hundred files must not
    /// open five hundred streams at once; the UI drains this a few at a
    /// time, so the robot sees a steady handful rather than a flood.
    queue: std::collections::VecDeque<Command>,
    /// Tree requests in flight: request id → (batch, where to put it).
    pending_trees: HashMap<u64, (u64, PathBuf)>,
    /// Where each in-flight download is being written. The host's reply
    /// carries only an id, so the destination has to be remembered here.
    destinations: HashMap<u64, PathBuf>,
    /// Request ids that were saves. Replies carry only an id, and a save must
    /// report on the editor rather than in the file pane's error banner.
    saves: std::collections::HashSet<u64>,
    next_id: u64,
}

pub type Shared = Arc<Mutex<State>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(State::default()))
}

impl State {
    /// Take the next request id. Ids only have to be unique within a session.
    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn remember_destination(&mut self, id: u64, path: PathBuf) {
        self.destinations.insert(id, path);
    }

    pub fn destination(&mut self, id: u64) -> Option<PathBuf> {
        self.destinations.remove(&id)
    }

    pub fn begin(
        &mut self,
        id: u64,
        name: String,
        direction: Direction,
        total: u64,
        batch: Option<u64>,
    ) {
        self.transfers.push(Transfer {
            id,
            name,
            direction,
            done: 0,
            total,
            outcome: None,
            batch,
        });
        if let Some(b) = batch.and_then(|b| self.batches.iter_mut().find(|x| x.id == b)) {
            b.queued = b.queued.saturating_sub(1);
        }
    }

    // ----------------------------------------------------------- folder copies

    /// Open a folder copy. Files are added as the tree arrives (downloads)
    /// or as the local walk finds them (uploads).
    pub fn new_batch(&mut self, name: String, direction: Direction) -> u64 {
        let id = self.next_id();
        self.batches.push(Batch {
            id,
            name,
            direction,
            queued: 0,
            truncated: false,
            error: None,
        });
        id
    }

    pub fn batch_mut(&mut self, id: u64) -> Option<&mut Batch> {
        self.batches.iter_mut().find(|b| b.id == id)
    }

    /// Put a command at the back of the queue. Downloads and uploads count
    /// against their batch so the row can say how many are still waiting.
    pub fn enqueue(&mut self, command: Command) {
        let batch = match &command {
            Command::Download { batch, .. } | Command::Upload { batch, .. } => *batch,
            _ => None,
        };
        if let Some(b) = batch.and_then(|b| self.batch_mut(b)) {
            b.queued += 1;
        }
        self.queue.push_back(command);
    }

    /// Commands that may start now: everything that is not a transfer, plus
    /// as many transfers as fit under `max_inflight` running at once.
    pub fn take_ready(&mut self, max_inflight: usize) -> Vec<Command> {
        let mut out = Vec::new();
        let mut running = self.running();
        while let Some(front) = self.queue.front() {
            let is_transfer = matches!(front, Command::Download { .. } | Command::Upload { .. });
            if is_transfer && running >= max_inflight {
                break;
            }
            if is_transfer {
                running += 1;
            }
            out.push(self.queue.pop_front().expect("front was Some"));
        }
        out
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn remember_tree(&mut self, request: u64, batch: u64, local_parent: PathBuf) {
        self.pending_trees.insert(request, (batch, local_parent));
    }

    pub fn take_tree(&mut self, request: u64) -> Option<(u64, PathBuf)> {
        self.pending_trees.remove(&request)
    }

    /// Add up a batch from its files.
    pub fn batch_progress(&self, batch: u64) -> BatchProgress {
        let mut p = BatchProgress::default();
        for t in self.transfers.iter().filter(|t| t.batch == Some(batch)) {
            p.files_total += 1;
            p.bytes_total += t.total;
            p.bytes_done += t.done.min(t.total.max(t.done));
            match &t.outcome {
                Some(Ok(())) => p.files_done += 1,
                Some(Err(_)) => p.files_failed += 1,
                None => {}
            }
        }
        if let Some(b) = self.batches.iter().find(|b| b.id == batch) {
            p.files_total += b.queued;
        }
        p
    }

    /// True while any transfer runs or waits.
    pub fn busy(&self) -> bool {
        self.running() > 0 || !self.queue.is_empty()
    }

    pub fn advance(&mut self, id: u64, done: u64) {
        if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
            t.done = done;
        }
    }

    pub fn set_total(&mut self, id: u64, total: u64) {
        if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
            t.total = total;
        }
    }

    /// Record how a transfer ended.
    ///
    /// A `Done` can arrive for something that is not a transfer at all — a
    /// delete, a new folder — so an unknown id is not an error here.
    pub fn finish_save(&mut self, outcome: Result<(), String>) {
        let Some(editor) = self.editor.as_mut() else { return };
        editor.saving = false;
        match outcome {
            // The saved copy becomes what was just written, so the file stops
            // being marked as changed.
            Ok(()) => {
                editor.saved = editor.text.clone();
                editor.error = None;
            }
            Err(reason) => editor.error = Some(reason),
        }
    }

    pub fn finish(&mut self, id: u64, outcome: Result<(), String>) {
        if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
            // A finished transfer shows a full bar rather than stopping at
            // whatever the last chunk boundary happened to be.
            if outcome.is_ok() && t.total > 0 {
                t.done = t.total;
            }
            t.outcome = Some(outcome);
        } else if let Err(reason) = outcome {
            self.error = Some(reason);
        }
        self.destinations.remove(&id);
    }

    pub fn remember_save(&mut self, id: u64) {
        self.saves.insert(id);
    }

    pub fn was_save(&mut self, id: u64) -> bool {
        self.saves.remove(&id)
    }

    /// Note that a file has been asked for, so the editor can open with a
    /// spinner rather than appearing only once the bytes arrive.
    pub fn begin_open(&mut self, path: String) {
        self.editor = Some(Editor {
            path,
            loading: true,
            ..Default::default()
        });
    }

    /// Fill the editor with what the robot sent.
    ///
    /// Ignored when it is not the file the editor is showing: a slow read of a
    /// file the operator has already closed must not overwrite whatever they
    /// opened next, or worse, what they have typed into it.
    pub fn opened(&mut self, path: &str, text: String) {
        let Some(editor) = self.editor.as_mut() else { return };
        if editor.path != path {
            return;
        }
        editor.saved = text.clone();
        editor.text = text;
        editor.loading = false;
        editor.error = None;
    }

    /// Drop finished rows, keeping anything still running.
    pub fn clear_finished(&mut self) {
        let done: Vec<u64> = self
            .batches
            .iter()
            .filter(|b| {
                let p = self.batch_progress(b.id);
                b.queued == 0 && (p.finished() || b.error.is_some())
            })
            .map(|b| b.id)
            .collect();
        self.batches.retain(|b| !done.contains(&b.id));
        self.transfers
            .retain(|t| t.running() || t.batch.is_some_and(|b| !done.contains(&b)));
    }

    pub fn running(&self) -> usize {
        self.transfers.iter().filter(|t| t.running()).count()
    }
}

/// A size a person can read at a glance.
///
/// Binary units, because that is what `ls -lh` and every file manager on a
/// robot will report, and a mismatch invites a bug hunt that is really a unit
/// mismatch.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Join a directory and a name the way the *host* spells paths.
///
/// The viewer cannot use [`std::path::Path`] for this: a Windows viewer
/// browsing a Linux robot would produce backslashes the robot cannot resolve.
/// The separator already in the host's own path is the reliable clue.
pub fn remote_join(dir: &str, name: &str) -> String {
    let windows = dir.contains('\\') && !dir.contains('/');
    let sep = if windows { '\\' } else { '/' };
    if dir.ends_with(sep) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{sep}{name}")
    }
}

/// A tree-relative path (`maps/old/b.pgm`, always `/`) as a local path.
///
/// Components only, never the string: a `..` from a robot must not climb out
/// of the folder the operator chose to receive into.
pub fn rel_to_local(base: &std::path::Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        out.push(part);
    }
    out
}

/// A tree-relative path joined onto a robot directory, in the robot's own
/// separator.
pub fn remote_join_rel(base: &str, rel: &str) -> String {
    let mut out = base.to_string();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        out = remote_join(&out, part);
    }
    out
}

/// The file-name part of a host path, for naming a download locally.
pub fn remote_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

/// List a directory on *this* machine, in the same shape the host reports.
///
/// Sharing [`DirListing`] with the remote side means one pane widget draws
/// both, which is what keeps the two halves of the window feeling like one
/// thing. It is a separate implementation from the host's rather than a
/// shared crate because the constraints differ: here the path always exists,
/// so it can be canonicalised, and the errors are about a disk the operator
/// is sitting in front of.
pub fn local_listing(path: &std::path::Path) -> anyhow::Result<DirListing> {
    #[cfg(windows)]
    if is_drive_list(path) {
        return Ok(drive_listing());
    }
    let dir = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());

    let mut entries = Vec::new();
    for item in std::fs::read_dir(&dir)? {
        let Ok(item) = item else { continue };
        let Ok(meta) = item.metadata() else { continue };
        entries.push(telekin_proto::DirEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            len: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            is_link: item.file_type().map(|t| t.is_symlink()).unwrap_or(false),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(DirListing {
        path: display_path(&dir),
        parent: dir.parent().map(display_path),
        entries,
    })
}

/// A path as a person would type it.
///
/// Windows canonicalisation returns a verbatim path, which is correct and
/// unreadable; the prefix is stripped for display and for the address box,
/// which round-trips fine for every path a file pane will meet.
fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(VERBATIM_PREFIX)
        .unwrap_or(&text)
        .to_string()
}

/// What Windows puts in front of a canonicalised path: backslash, backslash,
/// question mark, backslash.
const VERBATIM_PREFIX: &str = r"\\?\";

/// The place above every drive root on Windows: the empty path. The local
/// pane lists the drives there, so the Up arrow from `C:\` has somewhere to
/// go and a second disk is one click away instead of a path to be typed.
/// Elsewhere `/` is the top and this is never true.
pub fn is_drive_list(path: &std::path::Path) -> bool {
    cfg!(windows) && path.as_os_str().is_empty()
}

/// Where the local Up arrow goes: the folder above, the drive list from a
/// drive root on Windows, or nowhere.
pub fn local_up(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if is_drive_list(path) {
        return None;
    }
    match path.parent() {
        Some(parent) => Some(parent.to_path_buf()),
        None if cfg!(windows) => Some(std::path::PathBuf::new()),
        None => None,
    }
}

/// Every drive letter Windows currently has mounted, as `C:\`, in the shape
/// of a folder listing so the pane draws it like any other.
#[cfg(windows)]
fn drive_listing() -> DirListing {
    use windows::Win32::Storage::FileSystem::GetLogicalDrives;
    // A bitmask, bit 0 = A:. Asking the kernel rather than probing letters
    // means an empty card reader does not stall the pane.
    let mask = unsafe { GetLogicalDrives() };
    let entries = (0u32..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| telekin_proto::DirEntry {
            name: format!("{}:\\", (b'A' + bit as u8) as char),
            is_dir: true,
            len: 0,
            modified: None,
            is_link: false,
        })
        .collect();
    DirListing {
        path: String::new(),
        parent: None,
        entries,
    }
}

/// Where a file pane should open before anyone has navigated.
pub fn local_start() -> std::path::PathBuf {
    dirs_home().unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

/// A timestamp as a short local date, for the listing column.
pub fn short_date(secs: u64) -> String {
    // Formatted by hand rather than pulling in a date crate for one column.
    // Days since the epoch convert to a civil date with Howard Hinnant's
    // algorithm, which is exact and about ten lines.
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", time / 3600, (time % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_match_known_instants() {
        // Checked against `date -u -d @N`.
        assert_eq!(short_date(0), "1970-01-01 00:00");
        assert_eq!(short_date(946_684_800), "2000-01-01 00:00");
        // A leap day, which is where hand-rolled date maths usually breaks.
        assert_eq!(short_date(1_709_164_800), "2024-02-29 00:00");
        assert_eq!(short_date(1_709_164_800 + 3_661), "2024-02-29 01:01");
    }

    #[test]
    fn the_local_pane_can_list_a_real_directory() {
        let dir = std::env::temp_dir().join(format!("telekin-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("dirs");
        std::fs::write(dir.join("a.txt"), b"1234").expect("file");

        let listing = local_listing(&dir).expect("listing");
        assert_eq!(listing.entries.len(), 2);
        assert!(listing.entries[0].is_dir, "folders first");
        assert_eq!(listing.entries[1].len, 4);
        assert!(
            !listing.path.starts_with(VERBATIM_PREFIX),
            "the address box would show a verbatim path: {}",
            listing.path
        );
        assert!(!listing.path.starts_with(r"\?\"), "{}", listing.path);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_ordinary_folder_goes_up_to_its_parent() {
        let dir = std::env::temp_dir().join("telekin-up-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert_eq!(local_up(&dir), dir.parent().map(|p| p.to_path_buf()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn above_a_drive_root_sits_the_drive_list() {
        let root = std::path::Path::new(r"C:\");
        let up = local_up(root).expect("a drive root has somewhere to go up to");
        assert!(is_drive_list(&up));
        assert!(local_up(&up).is_none(), "the drive list is the top");

        let listing = local_listing(&up).expect("drive list");
        assert!(listing.parent.is_none());
        assert!(!listing.entries.is_empty());
        assert!(
            listing.entries.iter().all(|e| e.is_dir && e.name.ends_with(":\\")),
            "{:?}",
            listing.entries.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        assert!(listing.entries.iter().any(|e| e.name.eq_ignore_ascii_case("C:\\")));

        // Picking a drive in the pane is a plain join onto the empty path,
        // and lands on a listing of that drive.
        let opened = up.join("C:\\");
        assert!(!is_drive_list(&opened));
        assert!(local_listing(&opened).is_ok());
        assert!(is_drive_list(&local_up(&opened).expect("back up")));
    }

    #[cfg(not(windows))]
    #[test]
    fn the_filesystem_root_is_the_top_elsewhere() {
        assert!(local_up(std::path::Path::new("/")).is_none());
        assert!(!is_drive_list(std::path::Path::new("")));
    }

    #[test]
    fn an_edit_is_only_dirty_against_what_the_robot_has() {
        let mut state = State::default();
        state.begin_open("/etc/hosts".into());
        assert!(state.editor.as_ref().expect("editor").loading);

        state.opened("/etc/hosts", "127.0.0.1 localhost
".into());
        let editor = state.editor.as_ref().expect("editor");
        assert!(!editor.loading);
        assert!(!editor.dirty(), "a freshly opened file is not changed");

        state.editor.as_mut().expect("editor").text = "changed
".into();
        assert!(state.editor.as_ref().expect("editor").dirty());

        // Typing it back to the original counts as clean, which a keystroke
        // counter would get wrong.
        state.editor.as_mut().expect("editor").text = "127.0.0.1 localhost
".into();
        assert!(!state.editor.as_ref().expect("editor").dirty());
    }

    #[test]
    fn a_late_read_cannot_overwrite_what_is_being_typed() {
        let mut state = State::default();
        state.begin_open("/etc/hosts".into());
        state.opened("/etc/hosts", "first".into());
        state.editor.as_mut().expect("editor").text = "my edit".into();

        // A read of some other file finally comes back.
        state.opened("/etc/fstab", "somebody else's file".into());
        assert_eq!(state.editor.as_ref().expect("editor").text, "my edit");
    }

    #[test]
    fn a_successful_save_clears_the_changed_mark() {
        let mut state = State::default();
        state.begin_open("/tmp/a".into());
        state.opened("/tmp/a", "one".into());
        state.editor.as_mut().expect("editor").text = "two".into();
        state.editor.as_mut().expect("editor").saving = true;

        state.finish_save(Ok(()));
        let editor = state.editor.as_ref().expect("editor");
        assert!(!editor.saving);
        assert!(!editor.dirty());

        // A failed save keeps the mark, so nobody closes thinking it landed.
        state.editor.as_mut().expect("editor").text = "three".into();
        state.finish_save(Err("read-only file system".into()));
        let editor = state.editor.as_ref().expect("editor");
        assert!(editor.dirty(), "a failed save pretended to succeed");
        assert_eq!(editor.error.as_deref(), Some("read-only file system"));
    }

    #[test]
    fn sizes_read_the_way_a_person_would_say_them() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MiB");
        // Past three digits the decimal is noise.
        assert_eq!(human_size(1024 * 512), "512 KiB");
    }

    #[test]
    fn remote_paths_keep_the_hosts_own_separator() {
        assert_eq!(remote_join("/home/tangox", "map.yaml"), "/home/tangox/map.yaml");
        assert_eq!(remote_join("/", "etc"), "/etc");
        // A Windows robot, browsed from anywhere.
        assert_eq!(remote_join(r"C:\Users\t", "a.txt"), r"C:\Users\t\a.txt");
        assert_eq!(remote_join(r"C:\", "a.txt"), r"C:\a.txt");
    }

    #[test]
    fn a_download_is_named_after_the_file_not_the_path() {
        assert_eq!(remote_file_name("/home/tangox/bag/run3.bag"), "run3.bag");
        assert_eq!(remote_file_name(r"C:\logs\today.txt"), "today.txt");
        // A trailing separator should not produce an empty name.
        assert_eq!(remote_file_name("/home/tangox/"), "tangox");
    }

    #[test]
    fn relative_paths_cannot_climb_out_of_the_target() {
        let base = std::path::Path::new("C:/dl");
        assert_eq!(rel_to_local(base, "maps/old/b.pgm"), PathBuf::from("C:/dl/maps/old/b.pgm"));
        assert_eq!(rel_to_local(base, "../../etc/passwd"), PathBuf::from("C:/dl/etc/passwd"));
        assert_eq!(remote_join_rel("/home/t/in", "a/b.txt"), "/home/t/in/a/b.txt");
        assert_eq!(remote_join_rel("/home/t/in", "../x"), "/home/t/in/x");
    }

    #[test]
    fn a_folder_copy_starts_a_few_files_at_a_time() {
        let mut state = State::default();
        let batch = state.new_batch("maps".into(), Direction::FromRobot);
        for i in 0..10 {
            state.enqueue(Command::Download {
                remote: format!("/r/{i}"),
                local: PathBuf::from(format!("l/{i}")),
                batch: Some(batch),
            });
        }
        assert_eq!(state.queued(), 10);

        // Only four may run at once.
        let first = state.take_ready(4);
        assert_eq!(first.len(), 4);
        assert_eq!(state.queued(), 6);
        for (n, cmd) in first.iter().enumerate() {
            let Command::Download { batch, .. } = cmd else { panic!() };
            state.begin(n as u64 + 100, format!("{n}"), Direction::FromRobot, 10, *batch);
        }
        // With four running, nothing more is released...
        assert!(state.take_ready(4).is_empty());
        // ...until one finishes.
        state.finish(100, Ok(()));
        assert_eq!(state.take_ready(4).len(), 1);

        let p = state.batch_progress(batch);
        assert_eq!(p.files_total, 10, "queued files must still count in the total");
        assert_eq!(p.files_done, 1);
        assert!(!p.finished());
    }

    #[test]
    fn folder_setup_commands_are_not_throttled() {
        // mkdirs are cheap and must run ahead of the files that need them.
        let mut state = State::default();
        for i in 0..6 {
            state.enqueue(Command::MakeDirAll { path: format!("/r/d{i}") });
        }
        assert_eq!(state.take_ready(1).len(), 6);
    }

    #[test]
    fn a_finished_batch_is_cleared_with_its_files() {
        let mut state = State::default();
        let batch = state.new_batch("maps".into(), Direction::ToRobot);
        state.enqueue(Command::Upload { local: "a".into(), remote: "/r/a".into(), batch: Some(batch) });
        let cmd = state.take_ready(4).remove(0);
        let Command::Upload { batch: b, .. } = cmd else { panic!() };
        state.begin(7, "a".into(), Direction::ToRobot, 3, b);
        state.finish(7, Ok(()));
        assert!(state.batch_progress(batch).finished());
        state.clear_finished();
        assert!(state.batches.is_empty());
        assert!(state.transfers.is_empty());
    }

    #[test]
    fn a_finished_transfer_shows_a_full_bar() {
        let mut state = State::default();
        let id = state.next_id();
        state.begin(id, "run3.bag".into(), Direction::FromRobot, 1000, None);
        state.advance(id, 993);
        state.finish(id, Ok(()));
        let t = &state.transfers[0];
        assert_eq!(t.done, 1000, "stopped short of the end it reached");
        assert_eq!(t.fraction(), Some(1.0));
        assert!(!t.running());
    }

    #[test]
    fn an_unknown_failure_becomes_a_visible_error() {
        // Deletes and mkdirs have ids too, and no progress row to fail on.
        let mut state = State::default();
        state.finish(42, Err("permission denied".into()));
        assert_eq!(state.error.as_deref(), Some("permission denied"));
    }

    #[test]
    fn a_download_of_unknown_size_does_not_pretend() {
        let mut state = State::default();
        let id = state.next_id();
        state.begin(id, "x".into(), Direction::FromRobot, 0, None);
        assert_eq!(state.transfers[0].fraction(), None);
        state.set_total(id, 2048);
        state.advance(id, 512);
        assert_eq!(state.transfers[0].fraction(), Some(0.25));
    }

    #[test]
    fn clearing_keeps_what_is_still_moving() {
        let mut state = State::default();
        let a = state.next_id();
        let b = state.next_id();
        state.begin(a, "done.bin".into(), Direction::ToRobot, 10, None);
        state.begin(b, "busy.bin".into(), Direction::ToRobot, 10, None);
        state.finish(a, Ok(()));
        state.clear_finished();
        assert_eq!(state.transfers.len(), 1);
        assert_eq!(state.transfers[0].name, "busy.bin");
        assert_eq!(state.running(), 1);
    }

    #[test]
    fn ids_are_unique_within_a_session() {
        let mut state = State::default();
        let ids: Vec<u64> = (0..5).map(|_| state.next_id()).collect();
        let mut sorted = ids.clone();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "{ids:?}");
    }
}
