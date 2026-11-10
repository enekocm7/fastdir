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
| `publish` | (tags only) one GitHub release with all files and a single `SHA256SUMS.txt` |
| `aur` | (stable tags only) pushes [`aur/fastdir-bin/PKGBUILD`](aur/fastdir-bin/PKGBUILD) to the AUR |

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
