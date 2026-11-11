#!/usr/bin/env bash
# Writes packaging/flatpak/cargo-sources.json from Cargo.lock with
# flatpak-cargo-generator, so flatpak-builder can build offline.
#
#   packaging/flatpak/generate-sources.sh [Cargo.lock] [output]
#
# Needs python3 with aiohttp and tomlkit (pip install aiohttp tomlkit).
# FLATPAK_CARGO_GENERATOR overrides the generator script path; otherwise it
# is downloaded from flatpak-builder-tools at a pinned commit.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
lock=${1:-$here/../../Cargo.lock}
out=${2:-$here/cargo-sources.json}
rev=${FLATPAK_BUILDER_TOOLS_REV:-9d86d336a39b9af38b86f114b6ce5820fe260f91}

generator=${FLATPAK_CARGO_GENERATOR:-}
if [ -z "$generator" ]; then
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    generator="$tmp/flatpak-cargo-generator.py"
    curl -fsSLo "$generator" \
        "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$rev/cargo/flatpak-cargo-generator.py"
fi

python3 "$generator" "$lock" -o "$out"
echo "Wrote $out"
