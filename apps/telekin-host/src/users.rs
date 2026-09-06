//! Who is allowed to connect to this host.
//!
//! Two sources, tried in that order:
//!
//! 1. **The machine's own accounts.** The password is the one the operator
//!    already uses to log into the robot, which is what people expect and
//!    what NoMachine and friends do. Verified through `unix_chkpwd`, the
//!    setgid-shadow helper that `pam_unix` itself shells out to — linking
//!    libpam would need a development package on every robot, and this needs
//!    nothing installed.
//! 2. **A Telekin account file**, for hosts where the first is
//!    unavailable: Windows, a container without shadow access, or a service
//!    account that should not share the login password.
//!
//! Stored as one `username:phc-hash` line per account next to the host's TLS
//! identity. Only the hash is kept, so reading the file off a robot's SD card
//! does not hand over a working credential.
//!
//! Argon2id is used rather than a plain digest because these are human-chosen
//! passwords: the point is to make each guess expensive, which a fast hash
//! does not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;

const FILE: &str = "users";

pub fn path(identity_dir: &Path) -> PathBuf {
    identity_dir.join(FILE)
}

/// Read the account table. A missing file is an empty table, not an error —
/// a host with no accounts simply refuses every login.
pub fn load(identity_dir: &Path) -> anyhow::Result<HashMap<String, String>> {
    let file = path(identity_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };

    let mut users = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The hash itself contains ':' inside its PHC string, so split once.
        let (name, hash) = line
            .split_once(':')
            .with_context(|| format!("{}:{} is not `user:hash`", file.display(), n + 1))?;
        users.insert(name.to_string(), hash.to_string());
    }
    Ok(users)
}

/// Add or replace an account.
pub fn add(identity_dir: &Path, username: &str, password: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !username.is_empty() && !username.contains(':') && !username.contains(char::is_whitespace),
        "a username cannot be empty or contain ':' or spaces"
    );
    anyhow::ensure!(
        password.chars().count() >= 8,
        "a password must be at least 8 characters"
    );

    // A fresh random salt is generated per password by the hasher itself.
    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("hashing failed: {e}"))?
        .to_string();

    let mut users = load(identity_dir)?;
    let replaced = users.insert(username.to_string(), hash).is_some();

    std::fs::create_dir_all(identity_dir)
        .with_context(|| format!("creating {}", identity_dir.display()))?;
    let mut out = String::from("# Telekin accounts: username:argon2-hash\n");
    let mut names: Vec<_> = users.keys().cloned().collect();
    names.sort();
    for name in names {
        out.push_str(&format!("{name}:{}\n", users[&name]));
    }
    write_private(&path(identity_dir), out.as_bytes())?;

    println!(
        "{} account {username:?} in {}",
        if replaced { "Updated" } else { "Added" },
        path(identity_dir).display()
    );
    Ok(())
}

/// Whether this host can check passwords against the machine's own accounts.
pub fn system_auth_available() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new(CHKPWD).exists()
}

#[cfg(target_os = "linux")]
const CHKPWD: &str = "/usr/sbin/unix_chkpwd";
#[cfg(not(target_os = "linux"))]
const CHKPWD: &str = "";

/// Check a password against the machine's own account database.
///
/// Note what this deliberately cannot do: run as an unprivileged process,
/// `unix_chkpwd` only accepts the *calling user's own* password. A host
/// running as `tangox` can therefore only be logged into as `tangox` — other
/// accounts fall through to the file. That is a useful floor rather than a
/// limitation: a compromised viewer cannot use this path to reach root.
#[cfg(target_os = "linux")]
fn system_auth(username: &str, password: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if username.is_empty() || username.contains(['\0', '\n']) {
        return false;
    }

    let mut child = match Command::new(CHKPWD)
        .arg(username)
        .arg("nonull")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("could not run {CHKPWD}: {e}");
            return false;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // The trailing NUL is required, not decoration: the helper splits its
        // input on NUL bytes to support several passwords, so a password
        // without one is read as zero passwords and always rejected. This is
        // exactly what pam_unix writes (`strlen(pass) + 1`).
        let mut buf = Vec::with_capacity(password.len() + 1);
        buf.extend_from_slice(password.as_bytes());
        buf.push(0);
        let _ = stdin.write_all(&buf);
        // Dropping the pipe gives the helper its EOF.
    }

    match child.wait() {
        Ok(status) => status.success(),
        Err(e) => {
            tracing::debug!("{CHKPWD} did not exit cleanly: {e}");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn system_auth(_username: &str, _password: &str) -> bool {
    false
}

/// Check only the machine's own account. Exposed for `--test-password`, so a
/// failure can be attributed to a source rather than guessed at.
pub fn system_only(username: &str, password: &str) -> bool {
    system_auth(username, password)
}

/// Check a login against both sources.
pub fn authenticate(users: &HashMap<String, String>, username: &str, password: &str) -> bool {
    if system_auth(username, password) {
        tracing::debug!("{username} authenticated against the system account");
        return true;
    }
    verify(users, username, password)
}

/// Check a login against the Telekin account file only. Returns false for
/// both a wrong password and an unknown user, and takes similar time either
/// way so the two cannot be told apart.
pub fn verify(users: &HashMap<String, String>, username: &str, password: &str) -> bool {
    // A fixed hash to verify against when the user does not exist, so a
    // missing account costs the same Argon2 work as a wrong password.
    const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0c2E$\
                         Kq6ZJKPz6qkqXEbpB0i1B4tPPCT8QjxsQ0YvAzOaKtA";

    let stored = users.get(username).map(String::as_str).unwrap_or(DUMMY);
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    let ok = Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok();
    // Never accept the dummy, however it compares.
    ok && users.contains_key(username)
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_reject() {
        let dir = std::env::temp_dir().join(format!("telekin-users-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        add(&dir, "alice", "correct-horse").unwrap();
        let users = load(&dir).unwrap();

        assert!(verify(&users, "alice", "correct-horse"));
        assert!(!verify(&users, "alice", "wrong"));
        assert!(!verify(&users, "bob", "correct-horse"));
        // An unknown user must not be accepted by the dummy-hash path.
        assert!(!verify(&users, "nobody", ""));

        // Adding a second account keeps the first.
        add(&dir, "bob", "another-password").unwrap();
        let users = load(&dir).unwrap();
        assert!(verify(&users, "alice", "correct-horse"));
        assert!(verify(&users, "bob", "another-password"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_input() {
        let dir = std::env::temp_dir().join(format!("telekin-users-bad-{}", std::process::id()));
        assert!(add(&dir, "has:colon", "long-enough-password").is_err());
        assert!(add(&dir, "alice", "short").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
