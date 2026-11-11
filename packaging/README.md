# Packaging

Release artifacts are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
when a `v*` tag is pushed (or by a manual run, which uploads them as workflow
artifacts without publishing anything).

| Job | Produces |
|---|---|
| `windows` | `FastDir-<v>-setup-x64.exe` (Inno Setup, [`installer/`](../installer)) and `FastDir-<v>-portable-x64.zip` |
| `macos` | `FastDir-<v>-macos-arm64.dmg` and `.tar.gz` |
| `linux` | `FastDir-<v>-linux-x64.tar.gz`, `FastDir-<v>-amd64.deb`, `FastDir-<v>-x86_64.rpm`, `FastDir-<v>-x86_64.AppImage` |
| `linux-test` | installs the `.deb` on Debian 12 and Ubuntu 24.04, the `.rpm` on Fedora and the AppImage on Debian 12, then runs both binaries and scans `/` |
| `nix` | builds the flake (`nix build .#fastdir`) |
| `flatpak-sources`, `flatpak` | generates `cargo-sources.json` and builds the Flatpak (workflow artifact `FastDir-flatpak`) |
| `snap` | builds the classic snap, installs it on the runner and runs both commands |
| `alpine` | builds [`alpine/APKBUILD`](alpine/APKBUILD) from the checkout on Alpine edge (musl), installs the `.apk` and runs both binaries |
| `repos`, `repos-test` | builds the signed apt and dnf repositories and installs FastDir from them on Debian 12 and Fedora |
| `publish` | (tags only) one GitHub release with all files and a single `SHA256SUMS.txt` |
| `aur` | (stable tags only) pushes [`aur/fastdir-bin/PKGBUILD`](aur/fastdir-bin/PKGBUILD) to the AUR |
| `pages` | (stable tags only) publishes the apt/dnf repositories on GitHub Pages |
| `snap-publish` | (stable tags only) uploads the snap to the Snap Store's stable channel |

The publishing jobs (`aur`, `pages`, `snap-publish`) are skipped until their
secret exists. The Flatpak, snap and Alpine builds are not release files: they
are checked on every run, and the stores build or receive them.

The in-app updater downloads `-setup-x64.exe`, `-portable-x64.zip` or
`-x86_64.AppImage` and checks it against `SHA256SUMS.txt`, so keep those names
and keep every file in the checksum list.

## Linux

| File | What it is |
|---|---|
| [`linux/package.sh`](linux/package.sh) | Stages the install tree, works out runtime dependencies and builds the `.deb`/`.rpm` (nfpm) and the AppImage (linuxdeploy) |
| [`linux/io.github.enekocm7.FastDir.desktop`](linux/io.github.enekocm7.FastDir.desktop) | Desktop entry (application menu launcher) |
| [`linux/io.github.enekocm7.FastDir.metainfo.xml`](linux/io.github.enekocm7.FastDir.metainfo.xml) | AppStream metadata for GNOME Software, KDE Discover, Flathub and AppImageHub |
| [`nix/package.nix`](nix/package.nix) | Nix derivation used by the root [`flake.nix`](../flake.nix) |
| [`aur/fastdir-bin/PKGBUILD`](aur/fastdir-bin/PKGBUILD) | AUR binary package, repackaging the `.deb` |
| [`linux/build-repos.sh`](linux/build-repos.sh), [`linux/repo-index.html`](linux/repo-index.html) | Signed apt and dnf/zypper repositories (and their landing page) from a release's `.deb` and `.rpm` |
| [`flatpak/io.github.enekocm7.FastDir.yml`](flatpak/io.github.enekocm7.FastDir.yml) | Flatpak manifest (`org.freedesktop.Platform` 26.08) |
| [`flatpak/fastdir-launcher.sh`](flatpak/fastdir-launcher.sh), [`flatpak/generate-sources.sh`](flatpak/generate-sources.sh) | Flatpak launcher (desktop trash) and the `cargo-sources.json` generator |
| [`snap/snapcraft.yaml`](snap/snapcraft.yaml) | Snap, classic confinement |
| [`alpine/APKBUILD`](alpine/APKBUILD), [`alpine/build.sh`](alpine/build.sh) | Alpine package for aports, and the script CI builds it with |
| [`nix/nixpkgs/package.nix`](nix/nixpkgs/package.nix), [`nix/update-nixpkgs.sh`](nix/update-nixpkgs.sh) | nixpkgs package (fetches the release tag) and the script that bumps its version and hashes |

Every package installs:

```text
/usr/bin/fastdir                      GUI (also runs the CLI subcommands)
/usr/bin/fastdir-cli                  headless CLI and MCP server
/usr/share/applications/io.github.enekocm7.FastDir.desktop
/usr/share/metainfo/io.github.enekocm7.FastDir.metainfo.xml
/usr/share/icons/hicolor/{16..512}/apps/io.github.enekocm7.FastDir.png, scalable/…svg
/usr/share/doc/fastdir/README.md, /usr/share/licenses/fastdir/LICENSE
```

To build the packages locally (Debian/Ubuntu host with `dpkg-dev`,
`binutils`, `librsvg2-bin`, [nfpm](https://nfpm.goreleaser.com) and
[linuxdeploy](https://github.com/linuxdeploy/linuxdeploy) installed):

```sh
cargo build --release --locked
packaging/linux/package.sh 0.3.2 target/release dist   # SKIP_APPIMAGE=1 to skip the AppImage
```

### Decisions

- **License:** GPL-3.0-or-later ([`LICENSE`](../LICENSE), `license` in `[workspace.package]`).
- **App ID:** `io.github.enekocm7.FastDir`. It names the desktop file, the
  icon, the AppStream component and the window's Wayland `app_id` / X11
  `WM_CLASS`, and it is what Flathub will use. Do not change it: software
  centers and desktop shortcuts key off it.
- **One package for both binaries.** FastDir does not link GTK or WebKit,
  so the GUI's dependencies (fontconfig, xkbcommon, Wayland, the Vulkan
  loader) are small enough that a separate `fastdir-cli` package is not worth
  it yet.
- **Dependencies** are worked out from the binaries at build time
  (`dpkg-shlibdeps` for the `.deb`, sonames from `objdump` for the `.rpm`),
  plus the libraries GPUI opens with `dlopen` (Vulkan loader, Wayland,
  xkbcommon), which those tools cannot see.
- **Updates:** a deb/rpm/AUR/Nix/Flatpak/Snap/Homebrew install is owned by
  its package manager. The updater recognises it (`InstallKind::PackageManager`)
  and tells the user to update from there instead of replacing the binary. The
  AppImage updates itself by replacing the file at `$APPIMAGE`. An unpacked
  tarball points to the Releases page.
- **Architecture:** x86_64 only for now. Nothing in `package.sh` assumes it
  beyond the `arch` field and the file names.

### AUR setup (one-time)

1. Create an AUR account, add an SSH public key to it, and claim `fastdir-bin`
   by pushing the first PKGBUILD (or let the workflow create it).
2. Add the private key as the repository secret `AUR_SSH_PRIVATE_KEY`.
   Optionally set the variables `AUR_COMMIT_USERNAME` / `AUR_COMMIT_EMAIL`.

Without the secret the `aur` job is skipped.

### Signed apt and dnf repositories (one-time)

Until FastDir is in the distributions' own archives, `pages` publishes signed
repositories at `https://enekocm7.github.io/fastdir/` so that `apt upgrade` /
`dnf upgrade` pick up new releases. They hold only the latest version (older
ones stay on the Releases page).

1. Create a signing key without a passphrase, used only for this:
   `gpg --quick-gen-key "FastDir packages <…>" ed25519 sign 5y`, and export
   it with `gpg --armor --export-secret-keys <key id>`.
2. Add it as the secret `REPO_GPG_PRIVATE_KEY`.
3. Settings → Pages → Source: **GitHub Actions**.

The `.deb` is copied unchanged; the `.rpm` in the repository is signed with
the key (`rpmsign`), so its checksum differs from the release file. To rotate
the key, replace the secret; users then download `fastdir.gpg` /
`fastdir.asc` again.

### Flathub (one-time)

The manifest builds offline from `cargo-sources.json`. Submit the first
release after 0.3.2: older tags do not contain `packaging/linux` or
`packaging/flatpak`, which the build installs from (the same holds for Alpine
and nixpkgs below). To submit it:

1. Fork [flathub/flathub](https://github.com/flathub/flathub), branch off
   `new-pr`, and add `io.github.enekocm7.FastDir.yml` with the module's
   `dir` source replaced by the release:
   ```yaml
       sources:
         - type: git
           url: https://github.com/enekocm7/fastdir.git
           tag: v<version>
           commit: <full commit hash of the tag>
         - cargo-sources.json
   ```
   and add `cargo-sources.json` from
   [`flatpak/generate-sources.sh`](flatpak/generate-sources.sh). The launcher
   script is installed from the checkout, so nothing else is needed.
2. Open the pull request against `new-pr`. The Flathub bot test-builds it and
   a reviewer goes through the permissions below.
3. After the merge you own `flathub/io.github.enekocm7.FastDir`. Each release
   is a PR there that bumps `tag`/`commit` and regenerates
   `cargo-sources.json`. Flathub's external data checker can open those PRs
   by itself.

**Permissions.** `--filesystem=host` (plus `/run/media`, `/media`, `/mnt`):
a disk usage analyzer has to see and delete from the whole disk, so the
sandbox is a convenience for installing and updating, **not a security
boundary**. `--device=dri` for Vulkan, `--socket=wayland` /
`--socket=fallback-x11`, and `--share=network` for the update check (inside
Flatpak it only tells the user to update from their software center). The
runtime's `/usr` replaces the host's, so scanning `/` from the Flatpak shows
the runtime's `/usr`; use a native package to analyze the host's. Custom
actions also run inside the sandbox.

**Trash.** Inside Flatpak `XDG_DATA_HOME` is the app's private directory, so
**Move to Trash** would put files where no file manager looks.
[`fastdir-launcher.sh`](flatpak/fastdir-launcher.sh) points it at the host's
`~/.local/share` before it starts the GUI.

### Snap (one-time)

**Decision (#115): ship a snap with classic confinement.** Strict confinement
cannot read most of the file system (`removable-media` and `system-files`
only open selected paths) or delete outside `$HOME`, so a disk usage analyzer
would be pointless there. A classic snap runs against the host's libraries,
like the `.deb` (glibc 2.35 or newer, plus the Vulkan loader, Wayland,
xkbcommon and fontconfig, which every desktop has). The cost is a one-time
manual review.

1. `snapcraft register fastdir` with a Snap Store account.
2. Request classic confinement on the
   [Snapcraft forum](https://forum.snapcraft.io/c/store-requests/19) (*store
   requests*): FastDir is a disk usage analyzer that must read and delete
   anywhere on the file system. Ask for the `fastdir-cli` alias in the same
   thread.
3. Run `snapcraft export-login --snaps fastdir --acls package_access,package_push,package_update,package_release -`
   and add the output as the secret `SNAPCRAFT_STORE_CREDENTIALS`.

Users then run `sudo snap install fastdir --classic`. The CLI is
`fastdir.cli`, or `fastdir-cli` once the alias is granted.

### Alpine (one-time)

FastDir builds and runs on musl. The `alpine` job builds the APKBUILD on
Alpine edge with Alpine's own Rust toolchain, installs the package and runs
both binaries. The build has to be dynamically linked (Alpine's default):
GPUI loads the Vulkan loader, Wayland and xkbcommon with `dlopen`, which a
static `x86_64-unknown-linux-musl` binary cannot do. That is also why there is
no static musl tarball on the Releases page.

To get it into Alpine, open a merge request on
[aports](https://gitlab.alpinelinux.org/alpine/aports) that adds
`testing/fastdir/APKBUILD` (this file, with the release's `pkgver` and the
output of `abuild checksum`). New packages start in `testing`, and
maintainers move them to `community` later.

### nixpkgs (one-time)

[`nix/nixpkgs/package.nix`](nix/nixpkgs/package.nix) is the file for
`pkgs/by-name/fa/fastdir/package.nix`. Unlike the flake's derivation, it
fetches the release tag and uses `cargoHash`.

1. `packaging/nix/update-nixpkgs.sh <version>` sets the version and both
   hashes for a release and builds the package (`--check` rebuilds it as it
   is). The file in this repository holds placeholder hashes until then.
2. In a nixpkgs checkout, add yourself as `enekocm7` to
   `maintainers/maintainer-list.nix` in one commit. In a second commit
   (`fastdir: init at <version>`), copy the file to
   `pkgs/by-name/fa/fastdir/package.nix`. Then run `nix-build -A fastdir` and
   open the pull request.
3. After the merge, r-ryantm proposes version bumps on its own
   (`passthru.updateScript`).

### Debian and Ubuntu archives (long-term)

Ubuntu syncs new packages from Debian, so Debian is the one to target. Debian
builds Rust programs from source against its own packaged crates
(`librust-*-dev`, made with debcargo). FastDir depends on GPUI and several
hundred crates, many of them not in Debian. Each missing crate has to be
packaged first, mostly through the Debian Rust team, and uploads need a
Debian Developer as sponsor, so this takes months. The steps:

1. File an ITP (Intent To Package) bug against `wnpp`:
   ```text
   Package: wnpp
   Severity: wishlist
   Owner: <you>
   Subject: ITP: fastdir -- fast disk usage analyzer with a treemap

   * Package name    : fastdir
     Version         : <version>
     Upstream Contact: https://github.com/enekocm7/fastdir/issues
   * URL             : https://github.com/enekocm7/fastdir
   * License         : GPL-3.0-or-later
     Programming Lang: Rust
     Description     : fast disk usage analyzer with a treemap (GUI and CLI)
   ```
2. Find the missing crates (`cargo debstatus`) and package them with the
   Debian Rust team (`#debian-rust` on OFTC, the `debcargo-conf` repository).
3. Package `fastdir` itself (a `debian/` directory with `dh-cargo`) on Salsa
   and ask for a sponsor on debian-mentors (an RFS bug).

Until then, users get updates through the signed apt repository above.
