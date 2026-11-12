#!/usr/bin/env bash
# Empties every workspace crate, for warm-cache.yml.
#
# Only dependencies are cached (rust-cache drops the workspace crates' own
# build output), so the warm-up doesn't need to compile the app itself. With
# the crates emptied, cargo still builds every dependency but skips the slow
# compile and link of the workspace. Dependency builds don't depend on the
# workspace's source, so the release build reuses them as they are.
#
# Build scripts are left alone. Never run this outside CI: it overwrites the
# sources in place.
set -euo pipefail

cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[].targets[]
      | select(.kind[0] | IN("bin", "lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"))
      | "\(.kind[0]) \(.src_path)"' \
  | tr -d '\r' \
  | while read -r kind path; do
      if [ "$kind" = bin ]; then
        echo 'fn main() {}' > "$path"
      else
        : > "$path"
      fi
      echo "stubbed $kind $path"
    done
