# Telekin

Low-latency remote desktop for robotics work — built to sit in front of a
robot's onboard Linux computer while it runs ROS 2, from a Windows or Linux
workstation.

Same job as NoMachine, different transport. NoMachine's NX protocol compresses
X11 drawing commands over TCP; Telekin treats the screen as a video stream
over QUIC. That trade buys low latency on the lossy WiFi links robots actually
live on, at the cost of NX's very low bandwidth on pure-text screens.

## Why QUIC instead of NX/TCP

A single TCP connection has one ordered byte stream, so a lost packet stalls
everything behind it — head-of-line blocking. On a robot's WiFi link, that is
the difference between "the arm stopped when I released the key" and "the arm
kept going for 300 ms".

Telekin sends **each video frame on its own QUIC stream**. Losing part of
frame 40 delays frame 40 and nothing else; frame 41 keeps flowing. Input
travels on a separate reliable, ordered stream, so a dropped video packet never
delays a key release.

The rest of the transport story:

| Property | How |
|---|---|
| Encryption | TLS 1.3, built into QUIC — always on, no separate tunnel |
| Host identity | SSH-style certificate pinning by SHA-256 fingerprint |
| Congestion control | BBR, which holds throughput far better than CUBIC on lossy links |
| Connection setup | 1-RTT handshake |
| Stale frames | Depth-1 queue: a newer frame replaces a queued one instead of piling up |

## Layout

```
crates/
  telekin-proto      wire protocol: messages, framing, portable key codes
  telekin-transport  QUIC endpoints, TLS, certificate pinning
  telekin-codec      H.264 encode/decode behind swappable traits
  telekin-capture    screen capture (Windows: DXGI, Linux: X11)
  telekin-input      input injection (Windows: SendInput, Linux: XTEST)
apps/
  chassis       runs on the machine being controlled
  telekin       runs on the machine you sit at (egui window)
```

The viewer opens on a connection screen: scan the network or type an address,
sign in, and adjust resolution, frame rate and the host CPU ceiling — before
connecting or while connected. Changing a setting re-issues `StartStream`,
which the host treats as "stop this stream and begin another", so nothing has
to be reconnected.

### Naming and the scan list

A host advertises itself as **the account it runs as**, not as an arbitrary
label — `--name` still overrides it, but the default is the username. That is
the one string worth showing, because it is also what to sign in with: picking
a robot from the scan results fills the username in, leaving only the password
to type.

The machine's hostname rides along in a TXT record and appears underneath, so
two robots sharing an operator account are still distinguishable:

```
● tangox
  tango-desktop · 192.168.200.105
```

### Theme

Dark blue, pinned rather than following the OS: a bright panel next to the
video is glare, not a preference. Everything clickable is at least 34 px tall
(egui's default is around 20), because this gets used one-handed next to a
powered robot rather than at a desk. The stats panel draws each pipeline stage
as a bar rather than a number — which stage dominates is the question being
asked, and comparing two lengths is faster than comparing two figures.

`telekin-codec` and `telekin-capture` expose traits (`VideoEncoder`, `VideoDecoder`,
`ScreenCapture`), so a hardware encoder or a Wayland capture backend drops in
without touching the transport or the apps.

## Install (v1.0.0)

Two packages: **chassis** goes on the robot, **telekin** goes on the computer
you sit at. Everything is in the [releases](https://github.com/phuwanat-vg/telekin/releases).

### Robot — Ubuntu 22.04/24.04, ARM64 (Pi 5, Jetson) or x86-64

```bash
# pick the file for the robot's architecture: arm64 or amd64
sudo apt install ./telekin-host_1.0.0-1_arm64.deb

# run it now and at every boot, as the robot's own account
sudo systemctl enable --now chassis@tangox
systemctl status chassis@tangox
```

The host serves files the moment it starts. The screen is available once that
account is signed in on the robot's desktop; with autologin that happens by
itself after a reboot:

```bash
sudo sed -i 's/^#\s*AutomaticLoginEnable.*/AutomaticLoginEnable=true/; s/^#\s*AutomaticLogin\s*=.*/AutomaticLogin=tangox/' /etc/gdm3/custom.conf
```

Upgrading is the same `apt install` with the newer file. Removing:
`sudo apt remove telekin-host` (stops running instances, keeps them enabled
for a reinstall).

### Computer — the viewer

| Platform | File | Notes |
|---|---|---|
| Windows 10/11 | `telekin-1.0.0-windows-x64-setup.exe` | Installs per-user by default (no admin needed). Running a newer setup upgrades in place. |
| Ubuntu x86-64 | `telekin_1.0.0-1_amd64.deb` | `sudo apt install ./telekin_1.0.0-1_amd64.deb` — adds a launcher entry. |
| Ubuntu ARM64 | `telekin_1.0.0-1_arm64.deb` | same |
| macOS 11+ | `telekin-1.0.0-macos.dmg` | Drag to Applications. Unsigned: right-click → Open the first time. |

The viewer checks for a newer version when it starts (one HTTPS request for
`latest.json`; nothing is installed automatically) and shows an **Update to
x.y.z** button in the corner of the first screen. Turn that off with
`--no-update-check`, or point it at a server of your own with
`TELEKIN_UPDATE_URL=https://.../latest.json`. `telekin --check-update` prints
the result and exits, for scripts.

### Building the installers yourself

```bash
packaging/build-deb.sh              # on Ubuntu; makes the .debs for that machine's architecture
powershell -File packaging/build-windows.ps1   # on Windows; needs Inno Setup 6
packaging/macos/build.sh            # on macOS
python3 packaging/make-manifest.py --version 1.0.0 --base https://.../v1.0.0
```

Pushing a `v*` tag runs all of these on GitHub Actions and publishes a release
(`.github/workflows/release.yml`).

## Build

Needs a Rust toolchain, plus a C++ compiler for OpenH264: Visual Studio Build
Tools with the C++ workload on Windows, `build-essential` on Ubuntu. No X11
development headers are required — the X11 backends use `x11rb`, which speaks
the protocol in pure Rust.

```bash
cargo build --release
```

## Run

On the robot, create an account once:

```bash
chassis --add-user tangox
```

Then serve:

```bash
chassis --listen 0.0.0.0:9631
```

It prints a certificate fingerprint at startup. Copy it — that is what
authenticates the *host* to you, separately from the password that
authenticates you to it.

On your workstation, just run the viewer and fill in the window:

```bash
telekin
```

Passwords are stored only as Argon2id hashes, in `users` next to the host's
TLS identity, mode 0600. Reading that file off a robot's SD card does not
yield a working credential. An unknown username costs the same verification
time as a wrong password, so the two cannot be told apart by timing.

Useful flags:

- `--add-user <name>`, `--list-users` (host) — manage accounts
- `--discover` (viewer) — list the hosts on this LAN
- `--connect` (viewer) — skip the connection screen and dial straight away
- `--list-monitors` (host) — show what can be streamed
- `--scale N` (viewer) — stream at N% of the host's resolution
- `--max-cpu-percent N` — ceiling on the host's CPU, as a share of one core
- `--fps`, `--bitrate-kbps` — tune for your link
- `--view-only` — watch without sending input; also available on the host, which
  is the safer setting when a robot is powered up
- `--identity-dir` (host) — where the certificate and accounts live
- `--no-advertise` (host) — stay off mDNS

The viewer reads `TELEKIN_USER` and `TELEKIN_PASSWORD` if you would rather not type
them, but a password on a command line ends up in shell history — the window is
the better place for it.

## Running against a robot (Ubuntu)

Verified on Ubuntu 24.04: the host builds, X11 capture enumerates and streams,
and a Windows viewer holds a session against it across the network at 3840x1080
(13-20 fps, well under 0.5 Mbps on a mostly-idle desktop). Three things actually
stop people here, in this order:

**The session must be X11, not Wayland.** Ubuntu 22.04+ defaults to Wayland on
the desktop; this backend captures X11 only. Check with `echo
$XDG_SESSION_TYPE`. To switch, uncomment `WaylandEnable=false` in
`/etc/gdm3/custom.conf` and reboot.

**A headless robot has no X server at all.** Most robots run with no monitor
attached, so there is nothing to capture. Start a virtual display and point the
host at it — this also gives you a desktop to run RViz or rqt in:

```bash
sudo apt install -y xvfb && Xvfb :99 -screen 0 1920x1080x24 &
```

```bash
./target/release/chassis --display :99
```

`--display` matters under systemd too, where `DISPLAY` is not inherited. If the
X connection fails, the error names the likely cause and the fix rather than
just reporting a failed connect.

**QUIC is UDP, not TCP.** This catches people who open the port by habit:

```bash
sudo ufw allow 9631/udp
```

### CPU budget alongside ROS 2

The software encoder takes up to 8 threads (half the core count, capped), which
competes with ROS 2 nodes on a small board. Cost tracks the frame rate almost
linearly (see [CPU cost](#cpu-cost)), so drop `--fps` rather than resolution:
10-15 fps is comfortable for RViz, rqt and terminals, and roughly thirds the
encoder's CPU versus the 60 fps default.

```bash
./target/release/chassis --display :99 --view-only
```

The viewer picks the rate, so pass it there:

```bash
telekin --host <robot-ip> --user <name> --fps 15
```

`--view-only` is the safer default while a robot is powered up — it streams the
screen but ignores input, so a stray click cannot command anything. Hardware
encoding (see below) is what removes this trade-off.

## Running a fleet on a LAN

For a room of robots and operator stations paired one-to-one, two things stop
the address-and-fingerprint approach from scaling, and both are handled.

**Stable identity.** The host keeps its certificate in `--identity-dir`
(default `~/.config/telekin`), created on first run. The fingerprint an
operator pins therefore survives reboots and DHCP changes — pin once per robot
rather than after every restart.

**Discovery.** Each host advertises itself over mDNS as `_telekin._udp.local`,
publishing its name and fingerprint. To see what is on the network:

```bash
telekin --discover
```

```
NAME                     ADDRESS                FINGERPRINT
robot-07                 192.168.1.101:9631     2b:e8:46:68:...
robot-12                 192.168.1.104:9631     b9:03:c0:5f:...
```

Then connect by name — no IP needed, and it keeps working when DHCP moves the
robot:

```bash
telekin --name robot-07 --user <name> --fingerprint 2b:e8:46:68:...
```

A host advertises every interface address it has, including virtual adapters
and IPv6 addresses it cannot serve when bound to `0.0.0.0`. The viewer tries
them in order — private IPv4 first — until one connects, so a machine with VM
adapters does not need special handling.

Omitting `--fingerprint` with `--name` pins whatever mDNS advertised and warns
loudly. That is trust-on-first-use, not authentication: anything on the LAN can
publish a record. Read the fingerprint off the robot once and pass it
explicitly for anything that matters. `--no-advertise` turns the record off.

### Why X forwarding and NX were slow here

X forwarding sends drawing commands and waits for replies. On congested WiFi a
single round trip can cost tens of milliseconds, and one window repaint is
hundreds of round trips — which is how a redraw becomes a visible stall. NX
compresses that traffic well but still runs over TCP, so one lost packet stalls
everything queued behind it.

Telekin sends frames one-way with no per-frame acknowledgement, and each
frame rides its own QUIC stream, so loss delays one frame instead of the
session. Measured bandwidth on an idle desktop is 0.1-0.5 Mbps per stream; at
that rate twenty stations fit inside roughly 10 Mbps.

WiFi is a shared half-duplex medium, so *airtime*, not link speed, is the
budget with twenty stations. Two things help most:

- Drop `--fps`. Cost is close to linear in frame rate on the host and on the
  air. 10-15 fps is comfortable for RViz and terminals.
- Put the robots on 5 GHz, and keep the stations off the same channel as the
  robots' own telemetry if you can.

## Raspberry Pi 5 — measured

Measured on a Pi 5 Model B (4x Cortex-A76 @ 2.4 GHz, 4 GB, Ubuntu 24.04
aarch64, on WiFi), building and running natively. 1080p, software H.264, the
default 2 encoder threads for a 4-core board:

| Stage | Pi 5 | Desktop i7-13650HX (2 threads) |
|---|---|---|
| BGRA to I420 conversion | 4.3 ms | 2.1 ms |
| H.264 encode | 18.4 ms | 14.5 ms |
| **Total per frame** | **22.7 ms** (≈44 fps ceiling) | 16.6 ms |

Three consecutive runs from a settled board: 22.6, 22.9, 22.7 ms.

**That figure is a floor, not what a desktop costs.** The bench encodes a flat
synthetic pattern. Against the real GNOME desktop the pipeline costs, per
frame at 1920x1080:

| Screen state | Capture | Encode | Delivered | Host CPU |
|---|---|---|---|---|
| Static (unlocked desktop) | ~20 ms | ~62 ms | 1.6 fps | **29% of one core** |
| Actively animating | ~23-28 ms | 65-130 ms | 2-4 fps | ~35% of one core |

Two things drive those numbers.

**X11 has no damage events.** The backend therefore grabs and compares against
the previous frame, returning "unchanged" so the pipeline can skip encoding
entirely — the same contract DXGI gets for free on Windows. Without it a
static desktop was re-encoded 15 times a second to emit 13 bytes each time:
measured 85% of a core before the comparison, 29% after. A memcmp of 8 MB is
far cheaper than an H.264 frame.

**A busy 1080p screen is beyond software encode on this board.** At 65-130 ms
per frame the Pi delivers single-digit fps while a window is animating. If the
robot's screen is genuinely busy, drop the resolution rather than the frame
rate — 1280x720 is 45% of the pixels. (Not measured here; the display was
fixed at 1920x1080.)

Delivered frame rate is low on a static screen *by design*: nothing changed,
so nothing is sent beyond the twice-a-second refresh keyframe. It is not a
measure of responsiveness.

Two host-only diagnostics reproduce all of this against your own machine:

```bash
chassis --display :1 --bench-capture
```

reports how much the screen changes between two grabs and times capture,
encode of an identical frame, encode of alternating frames, and a forced
keyframe — separating "this screen is expensive" from "something else is
stealing CPU".

```bash
chassis --display :1 --test-input
```

injects Meta, a mouse sweep and Escape directly through XTEST, with no viewer
in the loop. On the Pi this moved 67.6% of the screen's bytes (against 0.0024%
idle), which is what confirmed the Linux input path end to end.

On the synthetic bench a Pi 5 is only about 1.4x slower per frame than a
recent desktop CPU at the same thread count — far closer than core counts and
clocks suggest, because OpenH264 ships NEON assembly for aarch64 and the colour
conversion vectorizes there too. On a real desktop the numbers above are what
to plan around: roughly one core for ~11 fps at 1080p, so a Pi 5 streaming
RViz comfortably is a case for lower resolution, lower frame rate, or — the
real fix — not encoding frames in which nothing changed.

Sustained load is stable rather than thermally limited. Six back-to-back runs:

```
round 1: 22.8 ms/frame  temp=65.0C
round 2: 22.7 ms/frame  temp=67.2C
round 3: 22.7 ms/frame  temp=66.1C
round 4: 22.6 ms/frame  temp=66.7C
round 5: 22.6 ms/frame  temp=66.7C
round 6: 22.6 ms/frame  temp=68.8C
```

Frame time does not drift and the temperature settles below 70 C, well under
the ~80 C where a Pi starts throttling. (A single reading taken right after a
three-minute compile showed 82 C and a slower 26.1 ms/frame — residual heat,
not the steady state. Let the board settle before trusting a thermal number.)

Building on the Pi itself takes about 3 minutes:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```

```bash
cd ~/telekin && cargo build --release -p telekin-host
```

## Where the latency actually goes

The host reports its own cost every two seconds, and pings the viewer to time
the control-stream round trip, so a slow session can be attributed rather than
guessed at. Measured on the Pi 5 over WiFi, with the screen genuinely
animating (driven by `--test-input`):

| Stage | 1920x1080 | 1024x768 |
|---|---|---|
| Capture (X11 SHM grab) | 20-24 ms | 8.5 ms |
| H.264 encode | 66-77 ms | 23-34 ms |
| **Host-side, before a frame is sent** | **88-101 ms** | **32-42 ms** |
| Network round trip | 6-9 ms | 6-8 ms |

**The network is not the bottleneck — it is under 10 ms.** Better than 90% of
the delay is the robot turning pixels into H.264, and both stages scale with
pixel count: 1024x768 is 38% of 1080p's pixels and costs 38-45% as much.

So on an SBC, *resolution is the latency knob*, not frame rate. Frame rate
trades away smoothness; resolution buys latency almost linearly. A Pi 5 at
1024x768 lands around 40 ms host-side, which is competitive with an NX session
on the same link; at 1080p it cannot be.

One WiFi sample in that run came back at 50 ms RTT against a 6-9 ms baseline.
Single-digit averages hide occasional spikes, which is exactly the case QUIC's
per-frame streams are meant to contain: the spike delays one frame instead of
stalling the session behind it.

The viewer prints this line while connected:

```
host: 4.5 fps, 156 kbps | capture 9.0 ms + encode 31.3 ms = 40.2 ms before send | rtt 6.5 ms
```

### XDAMAGE: what it fixed, and what it did not

The X11 backend now registers for XDAMAGE and uses the reported rectangles to
decide what to read: nothing repainted means no grab at all, and a small edit
grabs only its own rectangle and patches it into the frame buffer. Measured on
the Pi 5 at 1920x1080:

| | Before | After |
|---|---|---|
| Capture, idle desktop | 20-24 ms | **0.0 ms** |
| Capture, screen animating | 20-24 ms | 17-25 ms |
| Host CPU, idle | 29% of a core | **~19%** |
| Encode | 51-77 ms | 63-78 ms |

**Capture is solved; encode is now the whole problem.** At idle the host does
no framebuffer read whatsoever, and the remaining cost is one H.264 frame
every 500 ms to keep a late joiner in sync.

Two things worth recording from getting there:

- Draining every damage rectangle made the animating case *worse* — capture
  went to 30-48 ms — because a window animation queues hundreds of rectangles
  per frame whose union is the whole screen anyway. Merging is capped at
  `MAX_DAMAGE_EVENTS`, after which the frame is treated as fully dirty; that
  restored the animating case and roughly doubled its delivered frame rate.
- Damage can be missed, so a full grab still runs every `FULL_RESYNC` (2 s)
  and is compared against the previous frame. It costs one read every two
  seconds and means a compositor that repaints without reporting cannot leave
  a permanently stale image on the viewer.

What remains is that a full-frame H.264 encode of 1080p costs 63-78 ms on this
board no matter how little changed — the encoder still walks every macroblock.
Beating NX on desktop-shaped content needs the *encode* to shrink to the
damaged area too, which means sending sub-rectangles as their own coded units
rather than one full frame per update. That is the next change, and it is a
protocol change rather than a capture one.

### Idle cost: pay only for what changes

With capture free, the remaining idle cost was self-inflicted: the host
re-encoded the screen every 500 ms so a late joiner would converge. On a still
1080p desktop that is two full keyframes a second to retransmit a picture the
viewer already had.

It is redundant. A viewer that joins mid-session already receives an IDR,
because the encoder is built fresh per stream; one that loses frames or hits a
decode error asks for a keyframe over the control stream. `IDLE_REFRESH` is
now a 10-second backstop rather than a 500 ms heartbeat.

Measured on the Pi 5 at 1920x1080:

| Screen state | Host CPU |
|---|---|
| Idle, viewer connected | **1.2-2.6% of one core** |
| Actively animating | 74% of one core |
| 5 s after motion stops | 2.6% |

So an operator station left connected to a parked robot costs well under 1% of
the board, and cost tracks what the screen is actually doing.

### Capping CPU outright

Encoding cost is per frame, so the only honest way to bound CPU is to encode
less often. `--max-cpu-percent` does exactly that: after each frame the host
waits long enough that work-over-wall-time stays under the ceiling. A still
screen costs nothing and stays instant; a busy screen trades frame rate.

| Screen state | Uncapped | `--max-cpu-percent 20` |
|---|---|---|
| Actively animating | 74% of one core | **16%** |

This is the setting that matters when the board is also running ROS 2: it
turns "the remote desktop might eat the CPU" into a number you choose.

**The ceiling is enforced against measured CPU, not estimated CPU.** The first
version paced on wall-clock time spent in capture and encode, which is a
different quantity: the encoder runs on several threads, so 30 ms of encoding
can be 60 ms of CPU, and QUIC encrypts every frame on the runtime's threads
where no stage timer sees it. A cap of 5% delivered 26%. The loop now samples
the whole process's CPU time (`/proc/self/stat`, `GetProcessTimes`) and sleeps
until the frame has occupied `used / cap` of wall time. Measured on the Pi 5
with the screen animating:

| Cap | Measured |
|---|---|
| 5% | 4.8% |
| 15% | 12.0% |
| 40% | 24.6% |

The last one sits well under its ceiling because at 35% scale the pipeline
does not have 40% of a core's worth of work to do — the cap is a limit, not a
target.

Two smaller findings from chasing the same goal:

- `TELEKIN_LOW_CPU=1` biases the encoder for speed (low complexity, no
  deblocking, no scene-change detection, one reference frame). It is worth far
  less than it sounds: 72.8 -> 71.7 ms on a delta frame, 61.8 -> 53.8 ms on a
  keyframe. The cost is walking every macroblock, which no quality setting
  avoids.
- **A compositing desktop defeats fine-grained damage.** Damage is now
  tracked as individual rectangles rather than one bounding box, because a
  clock in the corner plus a cursor mid-screen have a bounding box covering
  almost everything. Under GNOME it still does not help: mutter repaints the
  whole root window every frame, so the server honestly reports full-screen
  damage even when 0.035% of pixels changed, and capture stays at ~23 ms. On a
  robot running a plain X session or Xvfb without a compositor, the rectangles
  are tight and this path is what makes capture nearly free. Worth checking
  which one your robots run.

### Waiting instead of asking

Polling for damage at the frame rate cost a round trip per frame forever, and
still noticed a change up to one frame late. The X11 backend now blocks in
`wait_for_event` on a dedicated connection, so the server wakes it the moment
something is repainted. That is both cheaper and faster — there is no polling
interval left to be unlucky with.

Measured on the Pi 5 at 1920x1080 with a viewer connected and the desktop idle:

| | Host CPU |
|---|---|
| Polling at 20 Hz | 2.4-7% of one core |
| Blocking on damage | **1.0-1.2%** |

Combined with the scale and CPU-ceiling controls, a station left connected to a
parked robot costs about a quarter of a percent of the board.

The scale control does what the measurements predicted. On the Pi 5, with the
screen animating:

| Scale | Encode | Host CPU during motion |
|---|---|---|
| 100% | 68-78 ms | 74% of a core (uncapped) |
| 50% | 32 ms | 8% |

Capture stays full-resolution — the whole screen is grabbed and then
downscaled — so the saving is all in the encoder, which is where the cost was.

One bug this exposed, worth keeping in mind for any damage-driven capture:
damage reports *changes*, so a session that has never captured anything gets
told nothing is happening. A viewer connecting to a still desktop waited
several seconds for its first picture until the resync timer fired. The first
frame of a session is now always a full grab, regardless of what damage says.

## CPU cost

Measured on an i7-13650HX (20 logical cores), host on Ubuntu 24.04 streaming a
3840x1080 X11 desktop to a Windows viewer. Percentages are of **one** core, so
100% is one core busy out of twenty.

| State | Host | Viewer |
|---|---|---|
| Idle, no viewer connected | 0% (6 MB) | — |
| Streaming @ 10 fps | 32% | 31% |
| Streaming @ 30 fps | 94% | 87% |

Host cost scales close to linearly with the frame rate, so `--fps` is the
lever: on a robot board, 10-15 fps is plenty for watching RViz or a terminal
and leaves the CPU to ROS 2. Note this is a 4.1-megapixel desktop — a 1080p
robot display is roughly half of it.

Encoder threads (`TELEKIN_ENCODER_THREADS`, default half the cores capped at 8)
matter far less than you would expect, because slice-parallel H.264 scales
poorly. Per 1080p frame on the same machine:

| Threads | Encode | Total per frame |
|---|---|---|
| 1 | 19.5 ms | 21.8 ms |
| 2 | 14.5 ms | 16.6 ms |
| 4 | 15.0 ms | 17.4 ms |
| 8 | 14.1 ms | 16.5 ms |

Almost all of the win is in the first extra thread. Capping at 2 costs about
1% of frame time versus 8 and hands six cores back to ROS 2, which is usually
the better trade on a robot.

When the viewer disconnects the host stops encoding and returns to 0%; a
dropped link does not leave a core pinned. (Resident memory stays high after a
session — that is glibc's per-thread malloc arenas holding freed frame buffers
for reuse, not a leak; it is reused by the next session rather than growing
without bound.)

## Measured performance

1080p, software H.264, on a 20-core desktop. Encoder-side cost per frame:

| Stage | Time |
|---|---|
| BGRA to I420 conversion | 2.5 ms |
| H.264 encode | 13.3 ms |
| **Total** | **15.8 ms** (≈63 fps ceiling) |

End-to-end with host and viewer on the *same* machine (so both compete for
CPU) against a real desktop: **16–19 fps at 1920×1080, 0.1–0.5 Mbps**. On
separate machines the host keeps its full CPU and does better; the bandwidth
figure is the notable one — well under what NX typically uses for the same
screen.

Reproduce with:

```bash
cargo test -p telekin-codec --release -- --nocapture where_time_goes
```

Two findings worth recording, since both were silent failures:

- OpenH264 initializes lazily on its first `encode()` call, so its parameters
  cannot be read or written before then. Configuring slice threading at
  construction fails with error code 4 and the encoder silently stays
  single-threaded. `H264Encoder` now does a throwaway encode to force
  initialization, applies threading, and forces an IDR for the first real
  frame. This was worth 18.6 ms → 13.3 ms.
- The viewer's event loop ran on `ControlFlow::Poll`, which spins as fast as
  the CPU allows and polled a mutex for new frames. It burned a full core even
  at 10 fps, where it should be nearly idle. It now sleeps on
  `ControlFlow::Wait` and the decode thread wakes it through an
  `EventLoopProxy` once per decoded frame: 126% -> 31% of a core at 10 fps.
- `iMultipleThreadIdc` on its own changes nothing: the default is one slice per
  frame, and a slice cannot be split across threads. Slice mode must be set
  too, which the safe wrapper does not expose — hence the raw-API call in
  `enable_slice_threading`, which verifies the setting took rather than
  trusting it.

## Status and what's next

Working today: QUIC transport with pinning, H.264 video, mouse and keyboard,
multi-monitor selection, Windows and Linux/X11 on both ends.

The highest-value next steps, roughly in order:

1. **Hardware encoding** — NVENC on Jetson, VAAPI/QSV on x86 SBCs, V4L2 M2M on
   boards that expose one. The trait boundary is already in place. This is what
   takes 1080p60 from marginal to comfortable and drops host CPU to near zero,
   which matters when the same board is running ROS 2 nodes.

   Not an option on every board: on a Raspberry Pi 5 the only codec device is
   `rpivid` (a decoder) alongside the camera ISP — Broadcom dropped the H.264
   encode block that the Pi 4 had, so a Pi 5 is software-encode only. Checked
   on hardware:

   ```bash
   for d in /dev/video*; do cat /sys/class/video4linux/$(basename $d)/name; done | sort -u
   ```
2. **GPU-side scaling and presentation in the viewer.** It currently does
   nearest-neighbour scaling on the CPU.
3. **Wayland capture** via PipeWire, for newer robot images.
4. **Adaptive bitrate** driven by the QUIC congestion controller's own
   estimate, which is already measuring the link.
5. **Clipboard and file transfer**, and audio.
6. **AV1** for a further bandwidth cut where hardware support exists.

## Input forwarding: three ways a click can go wrong

Clicking a window's close/maximise button on the remote desktop did nothing,
while ordinary clicks worked. Three defects in the viewer, each of which alone
can produce exactly that:

- **A release outside the image was dropped.** Press and release were both
  gated on "is the pointer inside the video". The close button sits on the
  image's top-right edge, so a release a pixel past it never reached the host,
  which then held Button1 down — and every later click became a drag. Presses
  must start inside the image; releases are now forwarded unconditionally.
- **No deadzone while a button is held.** The pointer position was re-sent
  every frame. A small viewer image maps onto a 1920-pixel host, so a couple
  of pixels of hand jitter here arrive as ten or more there — past GTK's
  headerbar drag threshold, which turns a click on a button into a window
  move. Motion under ~6 host pixels is now suppressed while a button is down.
  Real drags clear it trivially.
- **A press was not preceded by a move to its own position.** It relied on
  whatever motion was last sent. Each press now sends its exact position
  first.

Also added: if the viewer window loses focus with a button held (alt-tab, a
click on another app), the release will never arrive from the OS, so the
viewer releases it on the host itself. A robot should never be left mid-drag
because the operator switched windows.

## Clipboard

Text copied on the robot appears on the workstation's clipboard, and pasting
into the viewer places the text on the robot's clipboard before the keystroke
is forwarded — so copying a command or a stack trace across works in both
directions.

X11 makes this less trivial than it sounds. The clipboard there is not a
buffer: whoever copied *owns* the selection and must stay running to answer
`SelectionRequest` from whoever pastes. The host therefore keeps a small
background thread with its own X connection and a hidden window to serve those
requests, and polls the selection when it does not own it. Windows is a buffer,
so that backend is a poll loop guarded by the clipboard sequence number.

Verified in both directions against GTK as an independent party: text copied by
another application is picked up by the bridge, and text the bridge publishes
is readable by another application.

## Security notes

- Authentication is a username and password, verified against an Argon2id hash
  over TLS. Only the hash is stored, mode 0600; an unknown user costs the same
  time as a wrong password.
- A fingerprint learned from mDNS is trust-on-first-use, not proof: any host on
  the LAN can publish a record. Pass `--fingerprint` explicitly once you have
  read it off the robot.
- Without `--fingerprint` the connection is encrypted but the host is not
  authenticated — usable on a trusted LAN, not over the open internet.
- The host releases every held key and mouse button when a session ends, so a
  dropped link cannot leave a robot driving on a stuck key.
- On Windows the host cannot inject input into elevated windows unless it is
  itself elevated; `SendInput` fails and this is reported rather than ignored.
