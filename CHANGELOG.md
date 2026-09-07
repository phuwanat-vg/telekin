# Changelog

## 1.0.8 — 2026-09-07

- Desktop: `:`, `*` and the other shifted symbols type correctly. Punctuation
  and shifted digits now travel as text, which the robot types character for
  character whatever its keyboard layout; letters, digits, arrows and every
  Ctrl/Alt shortcut still travel as key presses. `:` in particular used to
  vanish, because egui reports Shift+; as a key of its own.

## 1.0.7 — 2026-09-07

- Ubuntu 22.04 (jammy) is supported for real: the Linux packages are now
  built on 22.04, so the same `.deb` installs on 22.04 and 24.04. Earlier
  releases were built on 24.04 and needed a newer glibc than 22.04 has.

## 1.0.6 — 2026-09-07

- Discovery: a host now advertises on network interfaces that come up after
  it started — a cable plugged in later, or WiFi switched off leaving only
  Ethernet. Seen on a Pi 5 as "typing the address works, Scan finds nothing"
  until the service was restarted. The mDNS library moved from mdns-sd 0.11
  to 0.21, which reworked interface tracking.

## 1.0.5 — 2026-09-07

- `telekin-robot-setup headless [WxH] [CONNECTOR]`: a robot with nothing on
  HDMI gives the viewer a black screen, because Xorg makes up a display GNOME
  never paints. The new step adds `video=<port>:<WxH>@60D` to the kernel
  command line (Pi `cmdline.txt`, GRUB, or extlinux) so the port is driven as
  if a monitor were there. `check` reports connected outputs and any forced
  mode; `all` adds `headless` when no monitor is connected.

## 1.0.4 — 2026-09-06

- The **Update to x.y.z** button now opens the download. Every earlier
  build silently dropped the click: the viewer is built without eframe's
  default features and the one that opens links was never turned back on.
  Installs of 1.0.1–1.0.3 must fetch this version by hand from the releases
  page; from here on the button works.

## 1.0.3 — 2026-09-06

- Files view, Windows: the Up arrow from a drive root (`C:\`) now shows the
  list of drives, so `D:` and a USB stick are a click away instead of a typed
  path. The copy arrows are disabled while the drive list is showing.

## 1.0.2 — 2026-09-06

- `telekin-robot-setup`, shipped with telekin-host and run only on request:
  `x11` (Xorg instead of Wayland in GDM), `autologin USER`, `firewall`
  (ufw 9631/udp), `service USER`, `all USER`, and `check`. Idempotent; keeps a
  backup of GDM's custom.conf.
- The host explains a port clash ("another chassis is running") instead of
  printing only "Address already in use".
- Robot-side diagnostic scripts kept under `tools/`.

## 1.0.1 — 2026-09-06

- Files view: whole folders copy in either direction (empty folders and
  nesting preserved; symlinks skipped; a robot-side tree is capped at 20,000
  entries and says so). A folder shows as one row with files-landed and byte
  progress. At most four files move at once, so a robot's SD card is not
  asked for hundreds simultaneously. Both panes refresh when the last file
  lands.
- The version label re-runs the update check when clicked.
- Protocol TK/1 v8 (Tree, MakeDirAll).

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
