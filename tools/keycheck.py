#!/usr/bin/env python3
"""Report key events as the X server receives them, with modifier state.

Run on the Pi with a session connected, then press shortcuts in the viewer.
Each line says which key and which modifiers were held, so "Ctrl+C arrived" is
a fact rather than an inference.
"""
import re
import subprocess
import sys

# X keycode -> label, for the keys worth naming here.
NAMES = {
    37: "Ctrl_L", 105: "Ctrl_R", 50: "Shift_L", 62: "Shift_R",
    64: "Alt_L", 108: "Alt_R", 133: "Super_L", 134: "Super_R",
    54: "C", 53: "X", 55: "V", 38: "A", 28: "T", 25: "W", 24: "Q",
    36: "Return", 23: "Tab", 9: "Escape", 65: "space", 22: "BackSpace",
    95: "F11", 119: "Delete",
}


def main():
    seconds = int(sys.argv[1]) if len(sys.argv) > 1 else 20
    print(f"press shortcuts in the viewer — listening {seconds}s\n", flush=True)

    proc = subprocess.Popen(
        ["timeout", str(seconds), "xinput", "test-xi2", "--root"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )

    kind = None
    held = set()
    for line in proc.stdout:
        if "RawKeyPress" in line:
            kind = "press"
        elif "RawKeyRelease" in line:
            kind = "release"
        elif kind and (m := re.search(r"detail:\s*(\d+)", line)):
            code = int(m.group(1))
            name = NAMES.get(code, f"keycode {code}")
            if kind == "press":
                held.add(name)
            mods = sorted(n for n in held if "Ctrl" in n or "Shift" in n or "Alt" in n or "Super" in n)
            combo = "+".join(mods + [name]) if mods and name not in mods else name
            print(f"{kind:8} {combo}", flush=True)
            if kind == "release":
                held.discard(name)
            kind = None


main()
