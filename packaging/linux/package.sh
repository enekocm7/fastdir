#!/usr/bin/env bash
# Builds the Linux packages for a release: .deb and .rpm (with nfpm) and an
# AppImage (with linuxdeploy), from already-built release binaries.
#
#   packaging/linux/package.sh <version> [bin-dir] [out-dir]
#
#   version  release version without the leading "v" (e.g. 0.3.2)
#   bin-dir  directory holding `fastdir` and `fastdir-cli` (default: target/release)
#   out-dir  where the packages are written (default: dist)
#
# Needs a Debian/Ubuntu host (dpkg-shlibdeps computes the .deb dependencies)
# with objdump, rsvg-convert, nfpm and linuxdeploy. Set NFPM / LINUXDEPLOY
# to override the tool paths. SKIP_APPIMAGE=1 skips the AppImage.
set -euo pipefail

version=${1:?usage: package.sh <version> [bin-dir] [out-dir]}
bin_dir=$(realpath "${2:-target/release}")
mkdir -p "${3:-dist}"
out_dir=$(realpath "${3:-dist}")
nfpm=${NFPM:-nfpm}
linuxdeploy=${LINUXDEPLOY:-linuxdeploy}

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
app_id=io.github.enekocm7.FastDir
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# ----------------------------------------------------------------------
# Staging tree: the files every package installs, below /usr.
# ----------------------------------------------------------------------
stage="$work/root"
install -Dm755 "$bin_dir/fastdir" "$stage/usr/bin/fastdir"
install -Dm755 "$bin_dir/fastdir-cli" "$stage/usr/bin/fastdir-cli"
install -Dm644 "$here/$app_id.desktop" "$stage/usr/share/applications/$app_id.desktop"
install -Dm644 "$repo/assets/logo.svg" "$stage/usr/share/icons/hicolor/scalable/apps/$app_id.svg"
for size in 16 24 32 48 64 128 256 512; do
    dir="$stage/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    rsvg-convert -w "$size" -h "$size" "$repo/assets/logo.svg" -o "$dir/$app_id.png"
done
install -Dm644 "$repo/README.md" "$stage/usr/share/doc/fastdir/README.md"
install -Dm644 "$repo/LICENSE" "$stage/usr/share/licenses/fastdir/LICENSE"

# AppStream metadata. Software centers show the newest <release>, so make sure
# the one being built is listed even if nobody added it by hand.
metainfo="$stage/usr/share/metainfo/$app_id.metainfo.xml"
install -Dm644 "$here/$app_id.metainfo.xml" "$metainfo"
if ! grep -q "<release version=\"$version\"" "$metainfo"; then
    sed -i "s|<releases>|<releases>\n    <release version=\"$version\" date=\"$(date -u +%Y-%m-%d)\"/>|" "$metainfo"
fi

# ----------------------------------------------------------------------
# Runtime dependencies, audited from the binaries themselves.
# ----------------------------------------------------------------------
bins=("$stage/usr/bin/fastdir" "$stage/usr/bin/fastdir-cli")
mapfile -t needed < <(objdump -p "${bins[@]}" | awk '/NEEDED/ { print $2 }' | sort -u)
glibc=$(objdump -T "${bins[@]}" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n1)
echo "Linked libraries: ${needed[*]}"
echo "Newest glibc symbol: $glibc"

# Loaded with dlopen at run time, so invisible to objdump / dpkg-shlibdeps:
# the Vulkan loader (rendering), Wayland and xkbcommon (windowing, input)
# and fontconfig (font lookup).
deb_extra=(libvulkan1 libwayland-client0 libxkbcommon0 libxkbcommon-x11-0 libfontconfig1)
rpm_extra=("libvulkan.so.1()(64bit)" "libwayland-client.so.0()(64bit)"
    "libxkbcommon.so.0()(64bit)" "libxkbcommon-x11.so.0()(64bit)"
    "libfontconfig.so.1()(64bit)")

mkdir -p "$work/shlibdeps/debian"
echo "Source: fastdir" > "$work/shlibdeps/debian/control"
shlibs=$(cd "$work/shlibdeps" && dpkg-shlibdeps -O "${bins[@]}" | sed -n 's/^shlibs:Depends=//p')
mapfile -t deb_depends < <(
    { tr ',' '\n' <<< "$shlibs"; printf '%s\n' "${deb_extra[@]}"; } \
        | sed 's/^ *//; s/ *$//' | awk 'NF && !seen[$1]++'
)
rpm_depends=("libc.so.6($glibc)(64bit)")
for lib in "${needed[@]}"; do rpm_depends+=("$lib()(64bit)"); done
for lib in "${rpm_extra[@]}"; do
    printf '%s\n' "${rpm_depends[@]}" | grep -qxF "$lib" || rpm_depends+=("$lib")
done
echo "deb Depends: ${deb_depends[*]}"
echo "rpm Requires: ${rpm_depends[*]}"

yaml_list() { for item in "$@"; do printf '    - "%s"\n' "$item"; done; }
# One entry per file: listing directories (nfpm's `tree`) would make the
# packages own /usr, /usr/bin, ... and clash with the distro's `filesystem`
# package over their modes.
contents_list() {
    (cd "$stage" && find . -type f | sort) | while read -r f; do
        printf '  - src: "%s"\n    dst: "/%s"\n' "$stage/${f#./}" "${f#./}"
    done
}

# ----------------------------------------------------------------------
# .deb and .rpm
# ----------------------------------------------------------------------
cat > "$work/nfpm.yaml" <<EOF
name: fastdir
arch: amd64
platform: linux
version: "$version"
section: utils
priority: optional
maintainer: "FastDir <https://github.com/enekocm7/fastdir/issues>"
description: |
  Blazing fast disk usage analyzer with a treemap.
  FastDir scans a drive or folder on every CPU core and shows where the space
  went: a sortable directory tree, a per-extension breakdown and a
  cushion-shaded treemap. Also installs fastdir-cli, a headless scanner with
  JSON/CSV output and an MCP server.
vendor: FastDir
homepage: https://github.com/enekocm7/fastdir
license: GPL-3.0-or-later
contents:
$(contents_list)
  - src: $repo/LICENSE
    dst: /usr/share/doc/fastdir/copyright
    file_info: { mode: 0644 }
    packager: deb
overrides:
  deb:
    depends:
$(yaml_list "${deb_depends[@]}")
    recommends:
      - "mesa-vulkan-drivers | vulkan-icd"
  rpm:
    depends:
$(yaml_list "${rpm_depends[@]}")
    recommends:
      - "mesa-vulkan-drivers"
deb:
  fields:
    Bugs: https://github.com/enekocm7/fastdir/issues
rpm:
  group: Applications/System
  summary: Blazing fast disk usage analyzer with a treemap
EOF

"$nfpm" package --config "$work/nfpm.yaml" --packager deb --target "$out_dir/FastDir-$version-amd64.deb"
"$nfpm" package --config "$work/nfpm.yaml" --packager rpm --target "$out_dir/FastDir-$version-x86_64.rpm"

# ----------------------------------------------------------------------
# AppImage
# ----------------------------------------------------------------------
if [ "${SKIP_APPIMAGE:-0}" != 1 ]; then
    appdir="$work/AppDir"
    mkdir -p "$appdir"
    cp -a "$stage/usr" "$appdir/"
    # linuxdeploy bundles the libraries that are not part of every desktop
    # (its excludelist keeps glibc, the GPU drivers and the Vulkan loader
    # out); the AppImage runtime sets $APPIMAGE, which the updater uses to
    # replace the file in place.
    (
        cd "$work"
        ARCH=x86_64 LDAI_OUTPUT="$out_dir/FastDir-$version-x86_64.AppImage" \
            OUTPUT="$out_dir/FastDir-$version-x86_64.AppImage" \
            "$linuxdeploy" --appdir "$appdir" \
            --executable "$appdir/usr/bin/fastdir" \
            --executable "$appdir/usr/bin/fastdir-cli" \
            --desktop-file "$appdir/usr/share/applications/$app_id.desktop" \
            --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/$app_id.png" \
            --output appimage
    )
fi

ls -l "$out_dir"
