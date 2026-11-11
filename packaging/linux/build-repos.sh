#!/usr/bin/env bash
# Builds signed apt and dnf/zypper repositories from a release's .deb and
# .rpm, ready to publish as static files (GitHub Pages).
#
#   packaging/linux/build-repos.sh <dist-dir> <out-dir> <base-url>
#
#   dist-dir  holds FastDir-<v>-amd64.deb and FastDir-<v>-x86_64.rpm
#   out-dir   receives the site: apt/, rpm/, fastdir.gpg, fastdir.asc,
#             fastdir.repo, fastdir.sources, index.html
#   base-url  where out-dir is served, e.g. https://enekocm7.github.io/fastdir
#
# Signs with the secret key in gpg's keyring (GPG_KEY_ID picks one if there
# are several). Needs apt-utils (apt-ftparchive), createrepo-c, rpm
# (rpmsign) and gnupg.
set -euo pipefail

dist=$(realpath "${1:?usage: build-repos.sh <dist-dir> <out-dir> <base-url>}")
mkdir -p "${2:?out-dir}"
out=$(realpath "$2")
base=${3:?base-url}
base=${base%/}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

key=${GPG_KEY_ID:-$(gpg --list-secret-keys --with-colons | awk -F: '/^sec/ { print $5; exit }')}
if [ -z "$key" ]; then
    echo "no signing key in the gpg keyring" >&2
    exit 1
fi

shopt -s nullglob
debs=("$dist"/*.deb)
rpms=("$dist"/*.rpm)
if [ ${#debs[@]} -eq 0 ] || [ ${#rpms[@]} -eq 0 ]; then
    echo "no .deb or .rpm in $dist" >&2
    exit 1
fi

gpg --export "$key" > "$out/fastdir.gpg"
gpg --armor --export "$key" > "$out/fastdir.asc"

# ----------------------------------------------------------------------
# apt: one suite ("stable"), one component ("main"), amd64.
# ----------------------------------------------------------------------
apt="$out/apt"
mkdir -p "$apt/pool/main/f/fastdir" "$apt/dists/stable/main/binary-amd64"
cp "${debs[@]}" "$apt/pool/main/f/fastdir/"
(
    cd "$apt"
    apt-ftparchive packages pool > dists/stable/main/binary-amd64/Packages
    gzip -9kn dists/stable/main/binary-amd64/Packages
    apt-ftparchive \
        -o APT::FTPArchive::Release::Origin=FastDir \
        -o APT::FTPArchive::Release::Label=FastDir \
        -o APT::FTPArchive::Release::Suite=stable \
        -o APT::FTPArchive::Release::Codename=stable \
        -o APT::FTPArchive::Release::Architectures=amd64 \
        -o APT::FTPArchive::Release::Components=main \
        -o "APT::FTPArchive::Release::Description=FastDir releases" \
        release dists/stable > "$out/Release.tmp"
    mv "$out/Release.tmp" dists/stable/Release
    gpg --batch --yes --local-user "$key" --clearsign \
        -o dists/stable/InRelease dists/stable/Release
    gpg --batch --yes --local-user "$key" --armor --detach-sign \
        -o dists/stable/Release.gpg dists/stable/Release
)

cat > "$out/fastdir.sources" <<SOURCES
Types: deb
URIs: $base/apt
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/fastdir.gpg
SOURCES

# ----------------------------------------------------------------------
# rpm: packages signed with the same key, and signed repodata.
# ----------------------------------------------------------------------
rpm_dir="$out/rpm"
mkdir -p "$rpm_dir/x86_64"
cp "${rpms[@]}" "$rpm_dir/x86_64/"
for f in "$rpm_dir"/x86_64/*.rpm; do
    rpmsign --define "_gpg_name $key" --addsign "$f"
done
createrepo_c --quiet "$rpm_dir"
gpg --batch --yes --local-user "$key" --armor --detach-sign \
    -o "$rpm_dir/repodata/repomd.xml.asc" "$rpm_dir/repodata/repomd.xml"

cat > "$out/fastdir.repo" <<REPO
[fastdir]
name=FastDir
baseurl=$base/rpm
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$base/fastdir.asc
REPO

# ----------------------------------------------------------------------
# Landing page with the setup commands.
# ----------------------------------------------------------------------
version=$(basename "${debs[0]}" | sed -n 's/^FastDir-\(.*\)-amd64\.deb$/\1/p')
sed -e "s|@BASE@|$base|g" -e "s|@VERSION@|$version|g" \
    "$here/repo-index.html" > "$out/index.html"
touch "$out/.nojekyll"

find "$out" -type f | sort
