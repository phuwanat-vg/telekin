#!/usr/bin/env python3
"""Report where injected clicks actually land, against the real title bar.

Run on the Pi with the session connected. Click a window's title bar buttons
in the viewer; each click is reported with the offset from where that button
really is, so a mapping error shows up as a number instead of a guess.
"""
import re
import subprocess
import sys
import time

import gi
gi.require_version("Gdk", "3.0")
gi.require_version("Wnck", "3.0")
from gi.repository import Gdk, Wnck  # noqa: E402


def windows():
    scr = Wnck.Screen.get_default()
    scr.force_update()
    out = []
    for w in scr.get_windows():
        if w.get_window_type() == Wnck.WindowType.NORMAL:
            x, y, ww, hh = w.get_geometry()
            out.append((w.get_name(), x, y, ww, hh))
    return out


def describe(x, y, wins):
    """Which window, and how far from its title bar, a point falls."""
    for name, wx, wy, ww, wh in wins:
        if wx <= x <= wx + ww and wy <= y <= wy + wh:
            # GNOME header bars are about 45 px tall.
            in_title = y <= wy + 45
            return (
                f"inside {name!r} at +{x - wx},+{y - wy}"
                f" [{'TITLE BAR' if in_title else 'content, ' + str(y - wy - 45) + 'px below title bar'}]"
            )
    return "not over any normal window"


def main():
    seconds = int(sys.argv[1]) if len(sys.argv) > 1 else 20
    wins = windows()
    print("windows right now:")
    for name, x, y, ww, hh in wins:
        print(f"  {name!r:45} x={x} y={y} w={ww} h={hh}"
              f"  title bar y={y}..{y+45}, buttons near x={x+ww-160}..{x+ww-10}")
    print(f"\nclick title-bar buttons now — listening {seconds}s\n")

    proc = subprocess.Popen(
        ["xinput", "test-xi2", "--root"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )
    deadline = time.time() + seconds
    kind = None
    try:
        while time.time() < deadline:
            line = proc.stdout.readline()
            if not line:
                break
            if "ButtonPress" in line and "Raw" not in line:
                kind = "press"
            elif "ButtonRelease" in line and "Raw" not in line:
                kind = "release"
            elif kind and (m := re.search(r"root:\s+([\d.]+)/([\d.]+)", line)):
                x, y = int(float(m.group(1))), int(float(m.group(2)))
                print(f"{kind:8} at ({x:4},{y:4})  {describe(x, y, wins)}")
                kind = None
    finally:
        proc.terminate()


main()
