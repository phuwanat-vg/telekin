#!/usr/bin/env python3
"""Write dist/latest.json, the file the viewer's update check reads.

    python3 packaging/make-manifest.py --version 1.0.0 \
        --base https://github.com/phuwanat-vg/telekin/releases/download/v1.0.0

Looks in dist/ for the artifacts the build scripts produce and records, for
each one found, its download URL and SHA-256. Artifacts that are not there
are simply left out, so a release built on two machines can be assembled
from either side without lying about what exists.
"""

import argparse
import hashlib
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"

# Which file is the download for which platform, by name pattern.
PLATFORMS = {
    "windows": "telekin-{v}-windows-x64-setup.exe",
    "linux_x86_64": "telekin_{v}-1_amd64.deb",
    "linux_aarch64": "telekin_{v}-1_arm64.deb",
    "macos": "telekin-{v}-macos.dmg",
}
# Robot-side packages, listed for people rather than for the updater.
HOST = {
    "host_x86_64": "telekin-host_{v}-1_amd64.deb",
    "host_aarch64": "telekin-host_{v}-1_arm64.deb",
}


def sha256(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True, help="e.g. 1.0.0")
    ap.add_argument("--base", required=True, help="URL the files will be served under")
    ap.add_argument("--notes", default="", help="one line shown next to the update button")
    ap.add_argument("--page", default="https://github.com/phuwanat-vg/telekin/releases/latest")
    args = ap.parse_args()

    base = args.base.rstrip("/")
    downloads, checksums, host = {}, {}, {}
    for key, pattern in {**PLATFORMS, **HOST}.items():
        name = pattern.format(v=args.version)
        path = DIST / name
        if not path.exists():
            print(f"  (no {name})", file=sys.stderr)
            continue
        url = f"{base}/{name}"
        (host if key.startswith("host_") else downloads)[key] = url
        checksums[name] = sha256(path)
        print(f"  {name}  {checksums[name][:12]}…", file=sys.stderr)

    manifest = {
        "version": args.version,
        "notes": args.notes,
        "page": args.page,
        "downloads": downloads,
        "host": host,
        "sha256": checksums,
    }
    out = DIST / "latest.json"
    out.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
