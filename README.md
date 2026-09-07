# Telekin

Remote desktop and file transfer for robots, over QUIC.

You sit at a Windows, Ubuntu or macOS computer; the robot runs Ubuntu (Pi 5,
Jetson, or an x86 box) with ROS 2. Telekin shows you the robot's screen with
low latency on the lossy WiFi robots actually live on, and lets you copy and
edit files on it **without starting a video stream at all** — which on a board
that is also running ROS 2 is the difference between a background chore and a
visible CPU cost.

Same job as NoMachine or X forwarding, different transport: those send drawing
commands over one TCP connection, where a single lost packet stalls everything
behind it. Telekin sends each video frame on its own QUIC stream, so loss
delays one frame instead of the session, and input rides a separate stream
that a dropped video packet can never hold up.

---

## Install

Two packages. **chassis** goes on the robot; **telekin** goes on the computer
you sit at. Everything is on the [releases page](https://github.com/phuwanat-vg/telekin/releases).

### Robot (Ubuntu 22.04 / 24.04, ARM64 or x86-64)

One package serves 22.04 and 24.04. Add the repository once, then it
installs and upgrades like any other package:

```bash
curl -fsSL https://phuwanat-vg.github.io/telekin/telekin-archive-keyring.gpg -o /tmp/telekin-archive-keyring.gpg && sudo install -m 644 /tmp/telekin-archive-keyring.gpg /usr/share/keyrings/
echo "deb [signed-by=/usr/share/keyrings/telekin-archive-keyring.gpg] https://phuwanat-vg.github.io/telekin stable main" | sudo tee /etc/apt/sources.list.d/telekin.list
sudo apt update
sudo apt install telekin-host
```

Then run it now and at every boot, as the robot's own account:

```bash
sudo systemctl enable --now chassis@$USER
```

If `apt update` says `NO_PUBKEY CF0138FADD7A535D`, the keyring file did not
land: run the first line again and check that
`/usr/share/keyrings/telekin-archive-keyring.gpg` is about 1.2 kB.

`chassis@` is a template: the part after `@` is the account it runs as. That
account is whose files you will see, whose desktop is captured, and the
username the viewer fills in for you. `apt install` prints this command with
the account already filled in.

The host serves files the moment it starts. The screen needs that account
signed in on an **X11** desktop, and robots with no one at the keyboard want
**autologin** so a reboot comes back by itself. Both, plus the UDP firewall
rule (QUIC is UDP), are one command — shipped with the package, run only when
you choose to, safe to run twice:

```bash
sudo telekin-robot-setup check          # what is set now; changes nothing
sudo telekin-robot-setup all $USER      # Xorg instead of Wayland, autologin, ufw 9631/udp, enable the service
sudo reboot
```

Each step is also available on its own (`x11`, `autologin USER`, `firewall`,
`service USER`, `headless`). It edits GDM's `custom.conf` and keeps a
`.telekin-bak` beside it.

**A robot with no screen shows a black desktop.** With nothing plugged into
HDMI, Xorg invents a 1024×768 screen that GNOME never paints, and the viewer
gets solid black. `check` says so; the fix is one line, applied at the next
reboot, which makes the kernel drive the port as if a 1920×1080 monitor were
attached (a size of your own goes after it, e.g. `headless 1280x720`):

```bash
sudo telekin-robot-setup headless
```

`all` runs it by itself when no monitor is connected. It edits the kernel
command line — `cmdline.txt` on a Pi, `/etc/default/grub` on x86, extlinux on
a Jetson — with the same `.telekin-bak` copy.

Without internet on the robot, install the `.deb` from the releases page
directly: `sudo apt install ./telekin-host_1.0.8-1_arm64.deb`.

### Computer

| Platform | Install | Upgrade |
|---|---|---|
| Windows 10/11 | `telekin-1.0.8-windows-x64-setup.exe` — per-user, no admin needed | run the newer setup; it replaces in place |
| Ubuntu | same repository as above, then `sudo apt install telekin` | `sudo apt upgrade` |
| macOS 11+ | `telekin-1.0.8-macos.dmg`, drag to Applications. Unsigned: right-click → Open the first time | drag the new one over |

The viewer checks for a newer version at start and shows an **Update to
x.y.z** button in the corner of the first screen when there is one. Nothing is
installed behind your back. `--no-update-check` turns it off;
`TELEKIN_UPDATE_URL` points it at a server of your own;
`telekin --check-update` prints the answer and exits, for scripts.

---

## Using it

**Connect.** Press *Scan network*. Robots appear by hostname with every address
they answer on; pick one and the username is filled in. Type the password and
choose **Connect** for the desktop, or **Files only** to skip video entirely.
If a robot has both WiFi and a cable, a *Network* box lets you pin one.

**Desktop.** Mouse, keyboard, scroll and clipboard work in both directions.
**Immersive** (or F11) goes fullscreen and forwards every shortcut to the
robot; F11 brings you back. The settings panel changes resolution, frame rate
and the robot's CPU ceiling live, without reconnecting.

**Files.** Two panes — this computer on the left, the robot on the right —
with arrows between them that point where a file will go. Select a file *or a
folder* and press the arrow. Folders copy whole, nesting and empty folders
included; a folder is one row in the transfer list with files-landed and byte
progress. Double-click a text file on the robot to edit it in place and
Ctrl+S to save (atomic write; binaries are refused rather than mangled).
*Delete* removes files and empty folders only — never a tree.

**CPU.** The one setting that matters when the robot is also running ROS 2 is
**Robot CPU limit**. It is a ceiling on measured CPU time, as a share of one
core: a still screen costs nothing and stays instant, a busy screen trades
frame rate. In `top` on the robot, the per-process `%CPU` column is the same
unit (100 = one core), while the summary line at the top is the whole machine.

---

## What it costs the robot

Measured on a Raspberry Pi 5 (4× A76, Ubuntu 24.04) at 1920×1080, software
H.264 — the Pi 5 has no hardware encoder.

| Situation | Host CPU (of one core) |
|---|---|
| No viewer connected | 0% |
| Viewer connected, screen still | 1.0–1.2% |
| Screen animating, uncapped | ~74% |
| Screen animating, `Robot CPU limit` 20% | 16% |
| Screen animating, resolution 50% | 8% |

Where the delay goes, screen animating, over WiFi:

| Stage | 1920×1080 | 1024×768 |
|---|---|---|
| Capture | 20–24 ms | 8.5 ms |
| Encode | 66–77 ms | 23–34 ms |
| **Before a frame leaves the robot** | **88–101 ms** | **32–42 ms** |
| Network round trip | 6–9 ms | 6–8 ms |

The network is not the bottleneck; encoding is, and it scales with pixel
count. **On a small board, resolution is the latency knob** — frame rate only
trades smoothness. Bandwidth on a mostly-still desktop is 0.1–0.5 Mbps per
station.

Two host-side diagnostics reproduce these numbers on your own robot:
`chassis --bench-capture` (times capture and encode) and
`chassis --test-input` (drives the screen through XTEST so you can measure a
busy display).

---

## A fleet on one WiFi

Robots and operator stations paired one-to-one, all on the same network.

- **Names.** A robot advertises its hostname over mDNS; the account rides in a
  separate record. Twenty robots imaged from one card, all running the same
  account, still appear as twenty rows — the scan list is keyed by each
  robot's certificate fingerprint, not by name. `chassis --name` overrides the
  label if you want.
- **Identity.** The fingerprint lives in `~/.config/telekin` on the robot and
  survives reboots and DHCP. Read it off the robot once and pin it in the
  viewer's *Advanced* box for anything that matters; a fingerprint learned
  from mDNS is trust-on-first-use, not proof.
- **Airtime.** WiFi is shared and half-duplex. Lower the resolution before the
  frame rate, put the robots on 5 GHz, and keep them off the channel their own
  telemetry uses.
- **Discovery does not cross subnets.** Scan works within one broadcast
  domain; across a router, type the address.

`telekin --discover` lists what the network answers with.

---

## Build from source

Rust toolchain plus a C++ compiler for OpenH264 (Visual Studio Build Tools on
Windows, `build-essential` on Ubuntu). No X11 headers: the X11 side is pure
Rust.

```bash
cargo build --release          # target/release/telekin and chassis
cargo test --release
```

Installers: `packaging/build-deb.sh` (Ubuntu, native arch),
`packaging/build-windows.ps1` (needs Inno Setup 6), `packaging/macos/build.sh`
(macOS). Pushing a `v*` tag builds all of them on GitHub Actions, publishes a
release, and regenerates the signed apt repository.

```
crates/
  telekin-proto      wire protocol TK/1: messages, framing, key codes
  telekin-transport  QUIC endpoints, TLS, pinning, mDNS discovery
  telekin-codec      H.264 encode/decode behind swappable traits
  telekin-capture    screen capture: X11 (XDAMAGE + SHM), Windows DXGI
  telekin-input      input injection and clipboard: XTEST, SendInput
apps/
  telekin-host       the robot side — binary `chassis`
  telekin            the viewer
packaging/           debs, systemd unit, Inno script, macOS bundle, apt repo
```

Runs on ARM64 and x86-64 Ubuntu, Windows, and (built on CI, not yet tried on
a real Mac) macOS.

---

## Security

- Sign-in is the robot account's own password, or a Telekin-only account made
  with `chassis --add-user`, checked over TLS 1.3 against an Argon2id hash. Only
  the hash is stored, mode 0600.
- Host identity is SSH-style pinning by SHA-256 fingerprint. Without a pinned
  fingerprint the link is encrypted but the robot is not authenticated: fine
  on a trusted LAN, not on the open internet.
- The host runs as one account and can reach exactly what that account can.
  It releases every held key and button when a session ends, so a dropped
  link cannot leave a robot driving on a stuck key.
- The apt repository is signed; the key fingerprint is in
  [`packaging/apt/FINGERPRINT`](packaging/apt/FINGERPRINT).

---

## Roadmap

1. **Encode only what changed.** Capture already knows which rectangles moved;
   the encoder still walks the whole frame. Sending damaged regions as their
   own coded units is the biggest remaining CPU win on software-encode boards.
2. **Hardware encoding** — NVENC on Jetson, VAAPI on x86 — behind the existing
   `VideoEncoder` trait. Not possible on a Pi 5.
3. **Saved connections and automatic reconnect** after a WiFi blip.
4. **Wayland capture** via PipeWire.

Changes by version are in [CHANGELOG.md](CHANGELOG.md).

MIT — phuwanat@IRiSH LAB
