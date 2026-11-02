<p align="center">
  <img src="assets/logo.svg" width="128" height="128" alt="FastDir logo">
</p>

<h1 align="center">FastDir</h1>

<p align="center">
  <b>A blazing fast disk usage analyzer for Windows, in the spirit of WinDirStat and WizTree.</b><br>
  Written in Rust with <a href="https://gpui-kit.com">GPUI Kit</a> (GPUI Fast backend).
</p>

---

FastDir scans a drive or folder on every CPU core, then shows you exactly where
the space went: a sortable directory tree, a per-extension breakdown, and a
cushion-shaded **treemap** where every little square is a file. Found the
culprit? Rename, move, recycle or delete it right there.

## Features

- **Fast, multi-threaded scanning** – a work-stealing thread pool lists
  directories in parallel. On Windows each listing is a batched
  `FindFirstFileExW` call (`FIND_FIRST_EX_LARGE_FETCH`), so there's no
  per-file `stat`. ~2.7 million files / 500 GB in about 17 s on a warm cache.
- **Drive overview** – every mounted volume with its label, file system and
  how full it is. Click one to scan it, or pick any folder.
- **Live progress** – files, folders, bytes, files/s, the current path, and a
  real percentage when you scan a whole drive.
- **WinDirStat-style layout**
  - **Directory tree** with subtree-percentage bars, %, size, items, files,
    folders and last change. Sort by any column.
  - **Extension list** with the colour each type has in the treemap.
  - **Cushion treemap** (squarified, SequoiaView-style shading), rendered on
    the CPU in parallel and drawn as one image. Hover to see a path, click
    to find the item in the tree.
  - Panels can be resized by dragging.
- **File operations** that update the tree in place, with no rescan:
  rename, move (also across drives), move to Recycle Bin, delete permanently,
  refresh a single folder, show in Explorer, open, copy path.
- **Light and dark themes.**
- **Explorer integration** (optional, from the installer): right-click any
  folder or drive → *Analyze with FastDir*.

## Install

Download the latest release from the
[Releases page](https://github.com/enekocm7/fastdir/releases):

| File | |
|---|---|
| `FastDir-<version>-setup-x64.exe` | Installer: Start menu shortcut, optional desktop icon and Explorer context menu, uninstaller. Installs per-user by default (no admin prompt). |
| `FastDir-<version>-portable-x64.zip` | Just the executable, no installation needed. |

### Updates

Every time FastDir starts it checks GitHub Releases in the background. When a
newer version exists, a notification and a toolbar button offer **Update now**
(downloads, verifies the SHA-256 and relaunches), **Release notes** and **Skip
this version**. Use the toolbar's `…` menu to check manually or to turn the
automatic check off. Settings live in `%APPDATA%\FastDir\settings.json`.

## Usage

```text
fastdir.exe              open the drive overview
fastdir.exe D:\          scan a drive right away
fastdir.exe "C:\Users"   scan a folder right away
```

### Keyboard shortcuts

| Key | Action |
|---|---|
| ↑ ↓ / PgUp PgDn / Home End | Move the selection |
| → / ← | Expand / collapse (or go to parent) |
| Enter | Toggle the folder |
| Backspace | Select the parent folder |
| Ctrl+Enter | Open with the default app |
| Ctrl+E | Show in Explorer |
| Ctrl+Shift+C | Copy the path |
| F2 | Rename |
| Ctrl+M | Move to… |
| Del | Move to Recycle Bin |
| Shift+Del | Delete permanently |
| F5 | Refresh the selected folder |
| Ctrl+R | Rescan everything |
| Ctrl+O | Scan a folder… |
| Esc | Stop scanning |
| Ctrl+T | Toggle light / dark |

## Building from source

Requirements: a recent Rust toolchain (edition 2024) and, on Windows, the MSVC
build tools.

```sh
cargo run --release
```

The application icon is generated at build time: `build.rs` rasterizes
[`assets/logo.svg`](assets/logo.svg) into a multi-size `.ico` and embeds it
in the executable.

### Building the installer locally

Install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then:

```powershell
cargo build --release
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=0.1.0 installer\fastdir.iss
```

The installer is written to `dist\`.

## Releasing

Push a tag that starts with `v`:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The [Release workflow](.github/workflows/release.yml) builds the release
binary, the installer and a portable zip with SHA-256 checksums, and publishes
them as a GitHub release. Running the workflow by hand (*Actions → Release →
Run workflow*) builds the same files as a workflow artifact without publishing
a release.

## Project layout

| Path | |
|---|---|
| `src/scanner.rs` | Parallel directory scanner |
| `src/model.rs` | The scanned tree, extension stats, and how they update after each operation |
| `src/treemap.rs` | Squarified layout, cushion shading, hit testing |
| `src/treemap_view.rs` | Treemap panel (off-thread rendering, selection, hover) |
| `src/app.rs` | Main window: drive overview, progress, tree, extension list, toolbar, dialogs |
| `src/drives.rs` | Volume detection |
| `src/fsops.rs` | Delete / rename / move / reveal |
| `src/updater.rs` | GitHub release check, verified download, installer / portable self-update |
| `src/settings.rs` | Small persisted config |
| `installer/fastdir.iss` | Inno Setup installer script |
