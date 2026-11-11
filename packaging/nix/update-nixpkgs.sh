#!/usr/bin/env bash
# Sets the version, source hash and cargoHash in nixpkgs/package.nix for a
# released tag, then builds it to prove the result.
#
#   packaging/nix/update-nixpkgs.sh <version>     e.g. 0.3.3
#   packaging/nix/update-nixpkgs.sh --check       build the file as it is
#
# Needs Nix with the nix-command feature. The nixpkgs submission uses the
# file this writes.
set -euo pipefail

version=${1:?usage: update-nixpkgs.sh <version> | --check}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
pkg="$here/nixpkgs/package.nix"
fake="sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

build() {
    # Outside nixpkgs `lib.maintainers.enekocm7` does not exist yet.
    NIX_PATH=${NIX_PATH:-nixpkgs=channel:nixos-unstable} nix-build --no-out-link -E "
      let pkgs = import <nixpkgs> { }; in
      pkgs.callPackage (builtins.toFile \"fastdir.nix\"
        (builtins.replaceStrings [ \"[ enekocm7 ]\" ] [ \"[ ]\" ]
          (builtins.readFile $pkg))) { }"
}

if [ "$version" = --check ]; then
    out=$(build)
    "$out/bin/fastdir-cli" scan / --max-depth 1 > /dev/null
    echo "built $out"
    exit 0
fi

url="https://github.com/enekocm7/fastdir/archive/refs/tags/v$version.tar.gz"
src_hash=$(nix hash convert --hash-algo sha256 --to sri "$(nix-prefetch-url --unpack "$url")")

sed -i -e "s|^  version = \".*\";|  version = \"$version\";|" "$pkg"
# The first hash in the file is the source's, the second the vendored crates'.
awk -v src="$src_hash" -v fake="$fake" '
    /^    hash = "sha256-/ && !done_src { sub(/sha256-[^"]*/, src); done_src = 1 }
    /^  cargoHash = "sha256-/ { sub(/sha256-[^"]*/, fake) }
    { print }
' "$pkg" > "$pkg.tmp" && mv "$pkg.tmp" "$pkg"

# A build with the placeholder cargoHash fails and reports the real one.
cargo_hash=$(build 2>&1 | sed -n 's/^ *got: *\(sha256-[^ ]*\).*/\1/p' | tail -n1 || true)
if [ -z "$cargo_hash" ]; then
    echo "could not determine cargoHash" >&2
    exit 1
fi
sed -i "s|^  cargoHash = \".*\";|  cargoHash = \"$cargo_hash\";|" "$pkg"

out=$(build)
"$out/bin/fastdir-cli" scan / --max-depth 1 > /dev/null
echo "version $version, hash $src_hash, cargoHash $cargo_hash"
echo "built $out"
