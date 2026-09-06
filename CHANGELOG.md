# Changelog

## 1.0.0 — 2026-09-06

First release.

**Robot side (`chassis`)**
- Serves the desktop over QUIC with one stream per frame, H.264 via OpenH264,
  XDAMAGE-driven capture: a still screen costs nothing.
- Serves the filesystem — browse, copy both ways, delete, new folder — with
  no video stream running. Sign-in works with no desktop session at all.
- Finds the signed-in X display by itself; no `DISPLAY` needed when started
  over SSH or from systemd.
- Measured CPU governor: `--max-cpu-percent` caps the host to a share of one
  core, paced on CPU time rather than wall time.
- Advertises itself over mDNS as its hostname, with the sign-in account in a
  separate record. Discovery is keyed by certificate fingerprint, so a fleet
  imaged from one card still appears as one row per robot.
- Ubuntu packages for ARM64 and x86-64 with a `chassis@<user>` systemd unit.

**Operator side (`telekin`)**
- Scan the LAN, pick a robot, sign in with its own account. Pinned
  fingerprints, TOFU otherwise.
- Desktop view with clipboard both ways, immersive mode (F11), scroll and
  shortcut forwarding.
- Files view: two panes, drawn icons, arrows that point where the file goes,
  progress with percentages, and an in-place text editor for small files
  (Ctrl+S saves; refuses binaries; atomic writes).
- Files-only connection that never starts the robot's encoder.
- Network picker for robots reachable on more than one interface.
- Update check against `latest.json` on the release page; `--check-update`
  for scripts; `--no-update-check` and `TELEKIN_UPDATE_URL` to control it.
- Installers: Windows 10/11 (Inno Setup, upgrades in place), Ubuntu .deb,
  macOS .dmg (built on CI; unsigned).

**Protocol** TK/1 v7.
