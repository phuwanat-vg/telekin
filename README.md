# Telekin

**Remote desktop and file transfer for robots.**

Sit at your own computer. See the robot's screen, use its keyboard and mouse,
and copy or edit its files. Built for the WiFi robots actually live on, and
light enough to run beside ROS 2.

| | Supported |
|---|---|
| **Robot** | Ubuntu 22.04 or 24.04, ARM64 or x86-64. Raspberry Pi 5, Jetson, PC |
| **Your computer** | Windows 10/11, Ubuntu, macOS 11+ |
| **Network** | Same LAN or WiFi. No internet or cloud account needed |
| **Price** | Free, including commercial use |

**[Download the latest version](https://github.com/phuwanat-vg/telekin/releases/latest)**

---

## Quick start

### 1. On the robot

Paste these lines into a terminal. The first one installs `curl`, which a
fresh Ubuntu does not always have:

```bash
sudo apt install -y curl
sudo curl -fsSL https://phuwanat-vg.github.io/telekin/telekin-archive-keyring.gpg -o /usr/share/keyrings/telekin-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/telekin-archive-keyring.gpg] https://phuwanat-vg.github.io/telekin stable main" | sudo tee /etc/apt/sources.list.d/telekin.list
sudo apt update
sudo apt install telekin-host
```

Then set the robot up and reboot:

```bash
sudo telekin-robot-setup all $USER
sudo reboot
```

That one command does everything a robot needs. See
[what it changes](#robot-setup-in-detail) below.

### 2. On your computer

| Platform | How to install |
|---|---|
| **Windows** | Download `telekin-…-windows-x64-setup.exe` from the [latest release](https://github.com/phuwanat-vg/telekin/releases/latest) and run it. No admin rights needed |
| **Ubuntu** | Add the repository with the first three lines above, then `sudo apt update && sudo apt install telekin` |
| **macOS** | Download `telekin-…-macos.dmg` and drag it to Applications. The first time, right-click the app and choose Open |

### 3. Connect

1. Open Telekin and press **Scan network**.
2. Click your robot in the list.
3. Type the robot account's password.
4. Press **Connect** for the desktop, or **Files only** to skip video.

---

## Using Telekin

### Desktop

- Mouse, keyboard, scroll and clipboard all work, in both directions.
- **Immersive** or **F11** goes fullscreen and sends your shortcuts to the
  robot. Press F11 again to leave.
- The settings panel changes resolution, frame rate and CPU limit while you
  are connected.

### Files

- Your computer is on the left, the robot on the right.
- Select a file or a folder and press the arrow that points where it should
  go. Folders copy whole.
- Double-click a text file on the robot to edit it. **Ctrl+S** saves.
- Delete removes files and empty folders only, never a whole tree.
- On Windows, the up arrow from `C:\` shows all your drives.

### Keeping the robot's CPU free

The robot has other work to do. Two settings keep Telekin out of its way:

| Setting | What it does |
|---|---|
| **Resolution** | The strongest lever. Half the resolution costs about a tenth of the CPU and cuts the delay by more than half |
| **Robot CPU limit** | A ceiling on how much of one CPU core Telekin may use. A still screen costs almost nothing either way |

**Files only** mode uses no video at all, so it costs the robot nothing while
idle.

---

## Updating

| Where | How |
|---|---|
| **Robot** | `sudo apt update && sudo apt upgrade`, then `sudo systemctl restart chassis@$USER` |
| **Windows and macOS** | Telekin shows an **Update to x.y.z** button on the first screen. Press it, then run the download |
| **Ubuntu viewer** | `sudo apt update && sudo apt upgrade` |

Nothing updates by itself.

---

## Troubleshooting

| Problem | Cause | Fix |
|---|---|---|
| **Black screen** after connecting | No monitor is plugged into the robot | `sudo telekin-robot-setup headless` then reboot |
| **Black screen**, monitor is plugged in | The robot's desktop runs Wayland | `sudo telekin-robot-setup x11` then reboot |
| **Desktop button is greyed out** | Nobody is signed in on the robot | `sudo telekin-robot-setup autologin $USER` then reboot |
| **Robot is not in the scan list** | Different subnet, or the service is not running | Type the robot's IP address instead. On the robot, run `systemctl status chassis@$USER` |
| **Connection drops after a while** | Ubuntu put the robot to sleep | `sudo telekin-robot-setup nosleep $USER` |
| **Cannot connect at all** | Firewall blocks UDP port 9631 | `sudo telekin-robot-setup firewall` |
| `apt update` says **NO_PUBKEY** | The key file did not download, usually because `curl` was missing | `sudo apt install -y curl`, then run the `curl` line again |
| Service fails with **port already taken** | A second copy of `chassis` is running | `sudo systemctl restart chassis@$USER` |

To see what is set on a robot without changing anything:

```bash
sudo telekin-robot-setup check
```

To read the robot's log:

```bash
journalctl -u chassis@$USER -n 50
```

---

## Robot setup in detail

`telekin-robot-setup` ships with the package. It only runs when you call it,
and running it twice is safe. It keeps a `.telekin-bak` copy of every file it
edits.

| Command | What it does |
|---|---|
| `check` | Shows the current state. Changes nothing |
| `all USER` | Runs every step below that the robot needs |
| `x11` | Makes the desktop use Xorg instead of Wayland |
| `autologin USER` | Signs the account in at boot, so the screen is there after a reboot |
| `nosleep USER` | Stops the robot from suspending, blanking or locking when idle |
| `headless [WxH]` | Makes the robot act as if a monitor is attached. Default 1920x1080 |
| `firewall` | Opens UDP port 9631 in `ufw`, if `ufw` is on |
| `service USER` | Starts Telekin now and at every boot |

The service is named `chassis@USER`. The part after `@` is the robot account
it runs as: the account whose desktop you see, whose files you reach, and
whose password you type.

**No internet on the robot?** Download the `.deb` from the
[latest release](https://github.com/phuwanat-vg/telekin/releases/latest) on
another computer, copy it over, and install it with
`sudo apt install ./telekin-host_…_arm64.deb`. Use `amd64` for a PC.

---

## Many robots on one network

- **Each robot shows up as its own row**, even when they share a hostname and
  account. Telekin tells them apart by a fingerprint unique to each robot.
- **Rename a robot in the list** with `chassis --name`.
- **Scan works inside one network.** Across a router, type the IP address.
- **WiFi is shared.** Lower the resolution first, and use 5 GHz.
- **Cable and WiFi together?** The viewer's *Network* box lets you pick one.

---

## Performance

Measured on a Raspberry Pi 5 with Ubuntu 24.04 at 1920×1080. The Pi 5 has no
hardware video encoder, so this is the hard case.

| Situation | Robot CPU, share of one core |
|---|---|
| Nobody connected | 0% |
| Connected, screen still | about 1% |
| Screen animating, no limit | about 74% |
| Screen animating, CPU limit 20% | 16% |
| Screen animating, resolution 50% | 8% |

| Delay before a frame leaves the robot | 1920×1080 | 1024×768 |
|---|---|---|
| Capture and encode | 88 to 101 ms | 32 to 42 ms |
| Network round trip over WiFi | 6 to 9 ms | 6 to 8 ms |

The network is not the slow part. Encoding is, and it depends on resolution.
A mostly still desktop uses 0.1 to 0.5 Mbps.

---

## Security

- **Sign-in** uses the robot account's own password, sent over an encrypted
  connection. Telekin never stores a password.
- **Everything is encrypted** with TLS 1.3.
- **Pin the fingerprint for anything that matters.** Each robot has a unique
  fingerprint, shown in its log and by `telekin --discover`. Paste it into the
  viewer's *Advanced* box and Telekin will refuse any other machine. Without
  it the link is encrypted but the robot is not verified, which is fine on a
  trusted LAN and not on the open internet.
- **Telekin can reach what the robot account can reach**, no more.
- **A dropped link releases every key and button**, so a robot is never left
  driving on a stuck key.
- **The apt repository is signed.** The key fingerprint is in
  [packaging/apt/FINGERPRINT](packaging/apt/FINGERPRINT).

---

## Command line

| Command | What it does |
|---|---|
| `telekin --discover` | Lists the robots on the network and exits |
| `telekin --check-update` | Says whether a newer version exists and exits |
| `telekin --no-update-check` | Starts without checking for updates |
| `telekin --trace-input` | Prints every key event, for reporting a key that does not type |
| `chassis --name NAME` | Sets the name shown in the scan list |
| `chassis --add-user NAME` | Creates a Telekin-only account, separate from the robot's own |
| `chassis --bench-capture` | Measures capture and encode time on this robot |

---

## Coming next

1. Lower CPU use by encoding only the part of the screen that changed.
2. Hardware encoding on Jetson and x86.
3. Saved connections and automatic reconnect after a WiFi drop.
4. Wayland support.

Changes by version are in [CHANGELOG.md](CHANGELOG.md).

## Licence

Telekin is free to use, including for commercial work. It is closed source
from version 1.0.13. You may not redistribute or modify it. The full terms
are in [LICENSE](LICENSE). The open-source components it is built on are
credited in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md). Versions
1.0.0 to 1.0.12 were released under the MIT License and stay under it.

phuwanat@IRiSH LAB
