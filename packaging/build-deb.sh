#!/usr/bin/env bash
# Build the Ubuntu packages for whichever architecture this machine is.
#
# Run it on an ARM64 box for the arm64 .debs and on an x86-64 box for the
# amd64 ones — a native build, because the encoder is C++ compiled from source
# and cross-compiling it is more fragile than owning two machines.
#
#   packaging/build-deb.sh            # both packages
#   packaging/build-deb.sh host       # just telekin-host (the robot side)
#   packaging/build-deb.sh viewer     # just telekin (the operator side)
#
# Output lands in dist/. Install with:  sudo apt install ./dist/<file>.deb
set -euo pipefail

cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

command -v cargo-deb >/dev/null || {
    echo "cargo-deb is not installed; run: cargo install cargo-deb --locked" >&2
    exit 1
}

what="${1:-all}"
arch="$(dpkg --print-architecture)"
mkdir -p dist

build() {
    local pkg="$1"
    echo "== $pkg ($arch) =="
    # --no-strip keeps symbol names in panic messages; the binaries are small
    # enough that the size does not matter and a readable backtrace from a
    # robot does.
    cargo deb -p "$pkg" --no-strip -o dist/ 2>&1 | grep -vE '^\s*(Compiling|Finished|Running)' || true
}

case "$what" in
    all)    build telekin-host; build telekin ;;
    host)   build telekin-host ;;
    viewer) build telekin ;;
    *) echo "usage: $0 [all|host|viewer]" >&2; exit 2 ;;
esac

echo
echo "built:"
ls -1 dist/*_"$arch".deb
