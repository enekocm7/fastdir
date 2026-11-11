#!/bin/sh
# Builds packaging/alpine/APKBUILD from this checkout on Alpine (musl), then
# installs the package and runs both binaries. Used by the release
# workflow's `alpine` job; locally:
#
#   docker run --rm -v "$PWD:/src" alpine:edge /src/packaging/alpine/build.sh
#
# The built .apk files are copied to /src/dist-alpine.
set -eu

src=/src
apk add --no-cache alpine-sdk sudo git

adduser -D builder
addgroup builder abuild
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$src/Cargo.toml" | head -n1)
aport=/home/builder/aport
mkdir -p "$aport"
# Build the checkout as it is: the source becomes a local tarball of HEAD
# instead of the release tag's download.
sed -e "s/^pkgver=.*/pkgver=$version/" \
    -e "s|^source=.*|source=\"fastdir-$version.tar.gz\"|" \
    "$src/packaging/alpine/APKBUILD" > "$aport/APKBUILD"
git config --global --add safe.directory "$src"
git -C "$src" archive --format=tar.gz --prefix="fastdir-$version/" \
    -o "$aport/fastdir-$version.tar.gz" HEAD
chown -R builder:builder "$aport"

cat > /home/builder/build.sh <<SCRIPT
set -eu
# busybox su keeps root's HOME; abuild keeps its keys and packages there.
export HOME=/home/builder REPODEST=/home/builder/packages
cd "$aport"
abuild-keygen -a -n -i
abuild checksum
abuild -r
SCRIPT
su builder -c "sh /home/builder/build.sh"

arch=$(apk --print-arch)
pkg=$(ls /home/builder/packages/*/"$arch"/fastdir-"$version"-r*.apk | grep -v -- '-doc-')
apk add "$pkg"

# Every library resolves (musl's ldd), the files are in place, and both
# binaries run; the GUI binary loads all of its libraries for a CLI subcommand.
if ldd /usr/bin/fastdir /usr/bin/fastdir-cli 2>&1 | grep -i 'not found'; then
    exit 1
fi
test -f /usr/share/applications/io.github.enekocm7.FastDir.desktop
test -f /usr/share/metainfo/io.github.enekocm7.FastDir.metainfo.xml
fastdir scan /usr --max-depth 1 > /dev/null
fastdir-cli scan / --max-depth 1 | head -c 400
echo

mkdir -p "$src/dist-alpine"
cp /home/builder/packages/*/"$arch"/*.apk "$src/dist-alpine/"
ls -l "$src/dist-alpine"
