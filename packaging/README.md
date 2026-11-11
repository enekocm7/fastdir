# Packaging

Release artifacts are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml)
when a `v*` tag is pushed (or by a manual run, which uploads them as workflow
artifacts without publishing anything).

| Job | Produces |
|---|---|
| `windows` | `FastDir-<v>-setup-x64.exe` (Inno Setup, [`installer/`](../installer)) and `FastDir-<v>-portable-x64.zip`, code-signed when a signing provider is configured |
| `macos` | `FastDir-<v>-macos-arm64.dmg` and `.tar.gz` |
| `linux` | `FastDir-<v>-linux-x64.tar.gz`, `FastDir-<v>-amd64.deb`, `FastDir-<v>-x86_64.rpm`, `FastDir-<v>-x86_64.AppImage` |
| `linux-test` | installs the `.deb` on Debian 12 and Ubuntu 24.04, the `.rpm` on Fedora and the AppImage on Debian 12, then runs both binaries and scans `/` |
| `nix` | builds the flake (`nix build .#fastdir`) |
| `publish` | (tags only) one GitHub release with all files and a single `SHA256SUMS.txt` |
| `aur` | (stable tags only) pushes [`aur/fastdir-bin/PKGBUILD`](aur/fastdir-bin/PKGBUILD) to the AUR |
| `winget` | (stable tags only) opens a winget-pkgs pull request for `enekocm7.FastDir` |
| `scoop` | (stable tags only) updates `bucket/fastdir.json` in the Scoop bucket repository |
| `chocolatey` | (stable tags only) packs [`chocolatey/`](chocolatey) and pushes it to the Chocolatey community repository |

Every publishing job (`aur`, `winget`, `scoop`, `chocolatey`) is skipped
until its secret exists, so a fork or a fresh setup still releases cleanly.

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

## Windows

| File | What it is |
|---|---|
| [`../installer/fastdir.iss`](../installer/fastdir.iss) | Inno Setup script (per-user by default; `/ALLUSERS` or `/CURRENTUSER` pick the scope silently) |
| [`winget/`](winget) | winget manifests for the first submission of `enekocm7.FastDir` |
| [`scoop/fastdir.json`](scoop/fastdir.json) | Scoop manifest template (portable zip; `checkver`/`autoupdate` included) |
| [`chocolatey/`](chocolatey) | Chocolatey package that runs the installer for all users |
| [`windows/signpath/artifact-configuration.xml`](windows/signpath/artifact-configuration.xml) | SignPath artifact configuration (sign every `.exe`) |

Updates: winget and Chocolatey run the Inno Setup installer, so FastDir's own
**Update now** keeps working for them (winget reads the new version from Apps &
features; `choco upgrade` just reinstalls). A Scoop install (`manifest.json` and
`install.json` next to the exe) is package-managed: the updater tells the user
to run `scoop update fastdir` instead of replacing files Scoop owns.

### Windows code signing (one-time)

The `windows` job signs `fastdir.exe` and `fastdir-cli.exe` before they go into
the installer and the zip, then signs the installer, and fails the build if a
signature does not verify. It signs on tag builds, and on manual runs with the
`sign` input. Configure **one** of:

- **Azure Artifact Signing** (formerly Trusted Signing; a few dollars a month,
  but identity validation is limited to some countries and to organizations
  or individuals that qualify). Create a signing account, a public-trust
  certificate profile and an app registration with the *Artifact Signing
  Certificate Profile Signer* role, then add the secrets `AZURE_TENANT_ID`,
  `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` and the variables
  `AZURE_SIGNING_ENDPOINT` (e.g. `https://weu.codesigning.azure.net/`),
  `AZURE_SIGNING_ACCOUNT` and `AZURE_SIGNING_PROFILE`.
- **SignPath** (free for open-source projects through the SignPath
  Foundation). Apply for the project, install the SignPath GitHub app, create
  two artifact configurations, `binaries` and `installer`, both from
  [`windows/signpath/artifact-configuration.xml`](windows/signpath/artifact-configuration.xml),
  and a `release-signing` policy. Then add the secret `SIGNPATH_API_TOKEN` and
  the variable `SIGNPATH_ORGANIZATION_ID` (optionally `SIGNPATH_PROJECT_SLUG`,
  default `fastdir`, and `SIGNPATH_POLICY_SLUG`, default `release-signing`).

If both are configured, Azure wins. The uninstaller that Inno Setup writes
(`unins000.exe`) is not signed; the installer, the app and the CLI are.
SmartScreen reputation builds up per certificate over the first downloads.

### winget setup (one-time)

1. Fork [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) to
   the account that owns the releases.
2. Submit the first version by hand from [`winget/`](winget), for example
   with `wingetcreate submit packaging/winget` or
   `komac submit packaging/winget` (check with `winget validate --manifest packaging/winget` first).
   Update the version, URL and `InstallerSha256` first if a newer release is out.
3. Add the secret `WINGET_TOKEN` (classic PAT with `public_repo`). Set the
   variable `WINGET_FORK_USER` if the fork is not under the repository owner.

After that the `winget` job opens the pull request for every stable release.

### Scoop setup (one-time)

1. Create a bucket repository, `enekocm7/scoop-bucket` (or set the variable
   `SCOOP_BUCKET_REPO`), with an empty `bucket/` folder.
2. Add the secret `SCOOP_BUCKET_TOKEN` (fine-grained PAT, Contents read/write
   on that repository).

Users install with:

```powershell
scoop bucket add fastdir https://github.com/enekocm7/scoop-bucket
scoop install fastdir/fastdir
```

The manifest also has `checkver`/`autoupdate`, so it can be proposed to
ScoopInstaller/Extras later, where Scoop's own bot keeps it updated.

### Chocolatey setup (one-time)

1. Create an account on [community.chocolatey.org](https://community.chocolatey.org)
   and copy its API key.
2. Add the secret `CHOCOLATEY_API_KEY`.

The first version goes through manual moderation (a few days); later ones are
usually approved automatically once the automated checks pass.
