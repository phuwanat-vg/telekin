//! Browsing and moving files on the robot.
//!
//! This runs as the account that signed in, so what it can reach is exactly
//! what that user's own shell can reach — the operating system does the
//! enforcing, and there is no second permission model here to get subtly
//! wrong. What this module owes the caller instead is *honesty*: an operation
//! that fails must say why in words an operator can act on, because the whole
//! point of a file pane is that you are not sitting at that machine.
//!
//! Nothing here touches the encoder. A session that only moves files never
//! starts one, which is why copying to a robot costs it almost nothing.

use std::path::{Path, PathBuf};

use anyhow::Context;
use telekin_proto::{DirEntry, DirListing};

/// List a directory. An empty path means the account's home.
pub fn list(path: &str) -> anyhow::Result<DirListing> {
    let dir = resolve(path)?;
    let read = std::fs::read_dir(&dir).with_context(|| describe(&dir))?;

    let mut entries = Vec::new();
    for item in read {
        let item = match item {
            Ok(i) => i,
            // One unreadable entry should not lose the whole listing: a
            // directory with a single root-owned file in it is still worth
            // showing.
            Err(e) => {
                tracing::debug!("skipped an entry in {}: {e}", dir.display());
                continue;
            }
        };
        let name = item.file_name().to_string_lossy().into_owned();

        // Follow links for size and kind, because that is what the operator
        // means when they double-click one; fall back to the link itself when
        // it dangles, so a broken link is still listed rather than vanishing.
        let is_link = item
            .path()
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let meta = match std::fs::metadata(item.path()) {
            Ok(m) => m,
            Err(_) => match item.path().symlink_metadata() {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!("no metadata for {name}: {e}");
                    continue;
                }
            },
        };

        entries.push(DirEntry {
            name,
            is_dir: meta.is_dir(),
            len: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            is_link,
        });
    }

    // Directories first, then by name, case-insensitively — the order every
    // file manager uses, because it is the order people scan in.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(DirListing {
        path: dir.to_string_lossy().into_owned(),
        parent: dir
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty()),
        entries,
    })
}

pub fn make_dir(path: &str) -> anyhow::Result<()> {
    let dir = resolve(path)?;
    anyhow::ensure!(!dir.exists(), "{} already exists", dir.display());
    std::fs::create_dir(&dir).with_context(|| describe(&dir))?;
    tracing::info!("created {}", dir.display());
    Ok(())
}

/// Delete a file, or an empty directory.
///
/// Recursive deletion is not offered on purpose. This pane is driven from
/// another building, often against a robot in the middle of a run; a
/// mis-aimed click that removes a tree has no undo, and refusing costs the
/// operator one `rm -r` in a terminal they already have.
pub fn remove(path: &str) -> anyhow::Result<()> {
    let target = resolve(path)?;
    let meta = target
        .symlink_metadata()
        .with_context(|| describe(&target))?;

    if meta.is_dir() {
        let empty = std::fs::read_dir(&target)
            .with_context(|| describe(&target))?
            .next()
            .is_none();
        anyhow::ensure!(
            empty,
            "{} is not empty. Telekin only deletes empty folders — delete the \
             contents first, or use a terminal if you meant to remove the tree.",
            target.display()
        );
        std::fs::remove_dir(&target).with_context(|| describe(&target))?;
    } else {
        std::fs::remove_file(&target).with_context(|| describe(&target))?;
    }
    tracing::info!("removed {}", target.display());
    Ok(())
}

/// Open a file for sending, returning it with its length.
pub fn open_for_read(path: &str) -> anyhow::Result<(std::fs::File, u64)> {
    let target = resolve(path)?;
    let meta = std::fs::metadata(&target).with_context(|| describe(&target))?;
    anyhow::ensure!(
        !meta.is_dir(),
        "{} is a folder. Open it, or pick the files inside.",
        target.display()
    );
    anyhow::ensure!(
        meta.len() <= telekin_proto::MAX_FILE_BYTES,
        "{} is {} bytes, past the {} byte transfer limit",
        target.display(),
        meta.len(),
        telekin_proto::MAX_FILE_BYTES
    );
    let file = std::fs::File::open(&target).with_context(|| describe(&target))?;
    Ok((file, meta.len()))
}

/// Read a small text file for editing in place.
///
/// Refuses anything that is not valid UTF-8. A file pane cannot tell a config
/// from a firmware image by its name, and handing an editor a binary would let
/// someone save it back with every invalid byte replaced — silent corruption
/// of a file on a robot, which is exactly the sort of thing this tool must not
/// make easy.
pub fn read_text(path: &str) -> anyhow::Result<String> {
    let target = resolve(path)?;
    let meta = std::fs::metadata(&target).with_context(|| describe(&target))?;
    anyhow::ensure!(
        !meta.is_dir(),
        "{} is a folder, not a file",
        target.display()
    );
    anyhow::ensure!(
        meta.len() <= telekin_proto::MAX_TEXT_BYTES,
        "{} is {} — too large to edit here. Copy it across instead.",
        target.display(),
        meta.len()
    );

    let bytes = std::fs::read(&target).with_context(|| describe(&target))?;
    String::from_utf8(bytes).map_err(|_| {
        anyhow::anyhow!(
            "{} is not text — editing it here would corrupt it",
            target.display()
        )
    })
}

/// Write an edited file back.
///
/// Through the same temporary-then-rename path an upload takes, so a failure
/// part way through leaves the original untouched rather than truncated. On a
/// robot the file being edited is often one something else is about to read.
pub fn write_text(path: &str, text: &str) -> anyhow::Result<()> {
    use std::io::Write;

    anyhow::ensure!(
        text.len() as u64 <= telekin_proto::MAX_TEXT_BYTES,
        "that edit is {} bytes, past the {} byte limit for editing in place",
        text.len(),
        telekin_proto::MAX_TEXT_BYTES
    );

    let (mut file, incoming) = begin_write(path)?;
    if let Err(e) = file.write_all(text.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        incoming.abandon();
        return Err(e).with_context(|| format!("writing {path}"));
    }
    drop(file);
    incoming.commit()
}

/// Where an upload should be written while it is in flight.
///
/// Uploads land beside the destination under a temporary name and are renamed
/// into place once complete, so an interrupted transfer cannot be mistaken for
/// a finished file — which on a robot might be a config or a map that
/// something else is about to read.
pub struct Incoming {
    pub temp: PathBuf,
    pub final_path: PathBuf,
}

pub fn begin_write(path: &str) -> anyhow::Result<(std::fs::File, Incoming)> {
    let final_path = resolve(path)?;
    let parent = final_path
        .parent()
        .context("cannot write to a filesystem root")?;
    anyhow::ensure!(
        parent.is_dir(),
        "{} is not a folder on this machine",
        parent.display()
    );
    anyhow::ensure!(
        !final_path.is_dir(),
        "{} is a folder here; pick another name",
        final_path.display()
    );

    let name = final_path
        .file_name()
        .context("that name has no file part")?
        .to_string_lossy()
        .into_owned();
    let temp = parent.join(format!(".{name}.telekin-part"));
    let file = std::fs::File::create(&temp).with_context(|| describe(&temp))?;
    Ok((file, Incoming { temp, final_path }))
}

impl Incoming {
    /// Put a finished upload in place.
    pub fn commit(self) -> anyhow::Result<()> {
        std::fs::rename(&self.temp, &self.final_path).with_context(|| {
            format!(
                "moving {} into place at {}",
                self.temp.display(),
                self.final_path.display()
            )
        })?;
        tracing::info!("received {}", self.final_path.display());
        Ok(())
    }

    /// Drop a transfer that did not finish.
    pub fn abandon(self) {
        if let Err(e) = std::fs::remove_file(&self.temp) {
            tracing::warn!("left {} behind: {e}", self.temp.display());
        }
    }
}

/// Turn a requested path into an absolute one, with `""` meaning home.
///
/// Paths are not sandboxed — see the module docs — but they are made absolute
/// so that a relative path can never be interpreted against whatever working
/// directory the host process happens to have been started in.
fn resolve(path: &str) -> anyhow::Result<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(home());
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        return Ok(clean(p));
    }
    // `~` is shell syntax, not filesystem syntax, but operators type it.
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| {
        (trimmed == "~").then_some("")
    }) {
        return Ok(clean(&home().join(rest)));
    }
    Ok(clean(&home().join(p)))
}

/// Resolve `.` and `..` textually.
///
/// Deliberately not [`std::fs::canonicalize`]: that requires the path to
/// exist, which is wrong for an upload destination, and on Windows it returns
/// `\\?\` paths that are correct but unreadable in a UI.
fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// An error message naming the path, since the operator is not on this machine
/// and cannot go and look.
fn describe(path: &Path) -> String {
    format!("{}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telekin-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn lists_directories_before_files_and_reports_sizes() {
        let dir = temp_dir("list");
        std::fs::create_dir(dir.join("zzz-folder")).expect("dir");
        std::fs::write(dir.join("aaa.txt"), b"hello").expect("file");

        let listing = list(&dir.to_string_lossy()).expect("list");
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].name, "zzz-folder", "folders come first");
        assert!(listing.entries[0].is_dir);
        assert_eq!(listing.entries[1].name, "aaa.txt");
        assert_eq!(listing.entries[1].len, 5);
        assert!(listing.parent.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dot_dot_is_resolved_without_the_path_existing() {
        // canonicalize() would fail here; an upload destination does not exist
        // yet, which is the whole reason this is textual.
        let p = resolve("/tmp/a/b/../c/new-file.txt").expect("resolve");
        assert!(p.ends_with("c/new-file.txt") || p.ends_with("c\\new-file.txt"), "{p:?}");
        assert!(!p.to_string_lossy().contains(".."), "{p:?}");
    }

    #[test]
    fn a_relative_path_lands_in_home_not_the_working_directory() {
        let p = resolve("notes.txt").expect("resolve");
        assert!(p.is_absolute(), "{p:?}");
        assert!(p.starts_with(home()), "{p:?}");
    }

    #[test]
    fn refuses_to_delete_a_directory_with_anything_in_it() {
        let dir = temp_dir("rm");
        let full = dir.join("full");
        std::fs::create_dir(&full).expect("dir");
        std::fs::write(full.join("something"), b"x").expect("file");

        let err = remove(&full.to_string_lossy()).expect_err("should refuse");
        assert!(format!("{err}").contains("not empty"), "{err}");
        assert!(full.exists(), "it was deleted anyway");

        // An empty one is fine.
        let empty = dir.join("empty");
        std::fs::create_dir(&empty).expect("dir");
        remove(&empty.to_string_lossy()).expect("remove empty");
        assert!(!empty.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_upload_is_invisible_until_it_finishes() {
        let dir = temp_dir("put");
        let dest = dir.join("map.yaml");
        let (file, incoming) = begin_write(&dest.to_string_lossy()).expect("begin");
        drop(file);

        assert!(!dest.exists(), "a partial upload appeared under its real name");
        assert!(incoming.temp.exists(), "nothing was written at all");

        incoming.commit().expect("commit");
        assert!(dest.exists(), "the file never arrived");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_abandoned_upload_leaves_nothing_behind() {
        let dir = temp_dir("abandon");
        let dest = dir.join("half.bin");
        let (file, incoming) = begin_write(&dest.to_string_lossy()).expect("begin");
        drop(file);
        let temp = incoming.temp.clone();
        incoming.abandon();

        assert!(!temp.exists(), "the part file was left behind");
        assert!(!dest.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn text_survives_a_round_trip_through_the_editor() {
        let dir = temp_dir("edit");
        let file = dir.join("params.yaml");
        std::fs::write(&file, "speed: 0.5
").expect("seed");

        let path = file.to_string_lossy().into_owned();
        assert_eq!(read_text(&path).expect("read"), "speed: 0.5
");

        write_text(&path, "speed: 1.25
turn: 0.4
").expect("write");
        assert_eq!(
            std::fs::read_to_string(&file).expect("reread"),
            "speed: 1.25
turn: 0.4
"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_binary_file_is_refused_rather_than_mangled() {
        let dir = temp_dir("binary");
        let file = dir.join("firmware.bin");
        // Invalid UTF-8: a lone continuation byte.
        std::fs::write(&file, [0x00, 0xFF, 0xFE, 0x80]).expect("seed");

        let err = read_text(&file.to_string_lossy()).expect_err("should refuse");
        assert!(format!("{err}").contains("not text"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_save_leaves_the_original_alone() {
        let dir = temp_dir("saveguard");
        let file = dir.join("keep.txt");
        std::fs::write(&file, "original").expect("seed");

        // Past the editor's limit, so the write is refused outright.
        let huge = "x".repeat(telekin_proto::MAX_TEXT_BYTES as usize + 1);
        let err = write_text(&file.to_string_lossy(), &huge).expect_err("should refuse");
        assert!(format!("{err}").contains("past the"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&file).expect("reread"),
            "original",
            "the file was damaged by a refused save"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_is_not_sent_as_a_file() {
        let dir = temp_dir("getdir");
        let err = open_for_read(&dir.to_string_lossy()).expect_err("should refuse");
        assert!(format!("{err}").contains("folder"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
