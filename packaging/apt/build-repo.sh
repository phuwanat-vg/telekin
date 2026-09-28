#!/usr/bin/env bash
# Turn the .debs in dist/ into a signed APT repository under repo/.
#
#   APT_SIGNING_KEY="$(cat telekin-apt-signing-key.asc)" packaging/apt/build-repo.sh
#
# The result is plain files — pool/, dists/stable/..., the public key — that
# any static web host can serve. CI publishes it to GitHub Pages, after which
# a robot needs the three lines in README "Robot" once and can then run
# `sudo apt install telekin-host` and `sudo apt upgrade` like any other
# package.
#
# Needs dpkg-dev (dpkg-scanpackages), apt-utils (apt-ftparchive) and gnupg.
# The private key comes in through APT_SIGNING_KEY (ASCII-armoured); when that
# is unset, whatever secret key is already in the local keyring is used, which
# is how a developer rebuilds the repo by hand.
set -euo pipefail

cd "$(dirname "$0")/../.."
out=repo
suite=stable
component=main
archs="arm64 amd64"

for t in dpkg-scanpackages apt-ftparchive gpg; do
    command -v "$t" >/dev/null || {
        echo "$t is missing — apt-get install -y dpkg-dev apt-utils gnupg" >&2
        exit 1
    }
done
ls dist/*.deb >/dev/null 2>&1 || { echo "no .deb files in dist/" >&2; exit 1; }

# A throwaway keyring when the key is handed in, so CI never touches a real
# one and a developer's keyring never gains a copy of the CI key.
if [ -n "${APT_SIGNING_KEY:-}" ]; then
    export GNUPGHOME
    GNUPGHOME="$(mktemp -d)"
    chmod 700 "$GNUPGHOME"
    printf '%s\n' "$APT_SIGNING_KEY" | gpg --batch --quiet --import
fi
keyid="$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^sec/{print $5; exit}')"
[ -n "$keyid" ] || { echo "no secret key available to sign with" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/pool/$component"
for arch in $archs; do mkdir -p "$out/dists/$suite/$component/binary-$arch"; done
cp dist/*.deb "$out/pool/$component/"

# One Packages index per architecture. Paths inside are relative to the repo
# root, which is why this runs from inside it.
for arch in $archs; do
    (
        cd "$out"
        dpkg-scanpackages --arch "$arch" "pool/" > "dists/$suite/$component/binary-$arch/Packages"
        gzip -9 -k -f "dists/$suite/$component/binary-$arch/Packages"
    )
done

# The Release file lists every index with its checksums; InRelease is the
# same thing signed inline, which is the form modern apt prefers.
(
    cd "$out/dists/$suite"
    apt-ftparchive \
        -o "APT::FTPArchive::Release::Origin=Telekin" \
        -o "APT::FTPArchive::Release::Label=Telekin" \
        -o "APT::FTPArchive::Release::Suite=$suite" \
        -o "APT::FTPArchive::Release::Codename=$suite" \
        -o "APT::FTPArchive::Release::Architectures=$archs" \
        -o "APT::FTPArchive::Release::Components=$component" \
        -o "APT::FTPArchive::Release::Description=Telekin: remote desktop and file transfer for robots" \
        release . > Release
    gpg --batch --yes --default-key "$keyid" --clearsign -o InRelease Release
    gpg --batch --yes --default-key "$keyid" -abs -o Release.gpg Release
)

# The public key, in the binary form apt's signed-by wants and the armoured
# form a person can read.
gpg --batch --export "$keyid" > "$out/telekin-archive-keyring.gpg"
gpg --batch --armor --export "$keyid" > "$out/telekin.asc"

# GitHub Pages otherwise runs Jekyll over the tree and drops files it does
# not like the look of.
touch "$out/.nojekyll"

cat > "$out/index.html" <<'EOF'
<!doctype html><meta charset="utf-8"><title>Telekin APT repository</title>
<style>body{font:16px/1.5 system-ui;max-width:42em;margin:3em auto;padding:0 1em}pre{background:#f4f7fb;padding:1em;overflow:auto}</style>
<h1>Telekin APT repository</h1>
<p>Ubuntu packages for <a href="https://github.com/phuwanat-vg/telekin">Telekin</a>, for ARM64 and x86-64. Add it once:</p>
<pre>curl -fsSL https://phuwanat-vg.github.io/telekin/telekin-archive-keyring.gpg | sudo tee /usr/share/keyrings/telekin-archive-keyring.gpg &gt;/dev/null
echo "deb [signed-by=/usr/share/keyrings/telekin-archive-keyring.gpg] https://phuwanat-vg.github.io/telekin stable main" | sudo tee /etc/apt/sources.list.d/telekin.list
sudo apt update</pre>
<p>Then, on a robot:</p>
<pre>sudo apt install telekin-host
sudo systemctl enable --now chassis@$USER</pre>
<p>On a computer you sit at:</p>
<pre>sudo apt install telekin</pre>
EOF

echo "repository written to $out/ (signed with key $keyid):"
find "$out" -type f | sort | sed 's/^/  /'
