# tools

Diagnostics that were run on the robot during development. None are needed
to use Telekin; they answer "did the input actually arrive?" and "what does
the host cost?" without a viewer in the loop.

- `keycheck.py N` — for N seconds, print every key the X server receives with
  its modifiers, so "Ctrl+C arrived" is a fact rather than an inference.
  Needs `xinput`.
- `clickcheck.py N` — the same for mouse buttons and positions.
- `pi-measure.sh` — sample the host's CPU time and the whole machine's over a
  few seconds, in the two units `top` mixes up (one core vs the machine).

Run them on the robot with its display, e.g.
`DISPLAY=:1 python3 keycheck.py 15`.
