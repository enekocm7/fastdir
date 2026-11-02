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

- **Fast, multi-threaded scanning** - a work-stealing thread pool lists
  directories in parallel. On Windows each listing is a batched
  `GetFileInformationByHandleEx` call (`FileIdBothDirectoryInfo`) that returns
  names, sizes, timestamps and file IDs together, so there's no per-file
  `stat`. ~2.7 million files / 500 GB in about 17 s on a warm cache.
- **Size on disk** - every row shows the allocated size next to the logical
  size, so compressed, sparse and tiny cluster-slack files are counted the way
  the volume sees them. The treemap can use either metric (`. menu` →
  *Treemap: size on disk*).
- **Hard links counted once** - files sharing a file ID (for example under
  `C:\Windows\WinSxS`) are counted the first time and marked as `hard link`
  afterwards, so totals are not inflated.
- **Drive overview** – every mounted volume with its label, file system and
  how full it is. Click one to scan it, or pick any folder.
- **Live progress** – files, folders, bytes, files/s, the current path, and a
  real percentage when you scan a whole drive.
- **WinDirStat-style layout**
  - **Directory tree** with subtree-percentage bars, %, size, items, files,
    folders and last change. Sort by any column.
  - **Search and filters** – `Ctrl+F` filters the tree by name or glob
    (`*.iso`), expanding to every match. Filter by minimum size, extension and
    modified date (`Ctrl+Shift+F`).
  - **Top files** – a flat, sortable list of the N largest files in the whole
    scan (bounded heap, default 1,000), the quickest way to find cleanup
    targets.
  - **Extension list** with the colour each type has in the treemap.
  - **Cushion treemap** (squarified, SequoiaView-style shading), rendered on
    the CPU in parallel and drawn as one image. Hover to see a path, click
    to find the item in the tree, double-click to open it. Zoom into any
    folder and climb back out with the breadcrumb above the treemap.
  - Panels can be resized by dragging.
- **File operations** that update the tree in place, with no rescan:
  rename, move (also across drives), move to Recycle Bin, delete permanently,
  refresh a single folder, show in Explorer, open, copy path.
- **Multi-select**: Ctrl+click toggles, Shift+click selects a range (also in
  the treemap, where every selected cell is outlined). The status bar shows
  the count and total size; move / recycle / delete run on the whole
  selection after one confirmation.
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
| Shift+↑ / Shift+↓ | Extend the selection |
| → / ← | Expand / collapse (or go to parent) |
| Enter | Toggle the folder |
| Backspace | Select the parent folder |
| Ctrl+Enter | Open with the default app |
| Z | Zoom the treemap into the selected folder |
| Shift+Z | Zoom the treemap out one folder |
| Ctrl+E | Show in Explorer |
| Ctrl+Shift+C | Copy the path |
| F2 | Rename |
| Ctrl+M | Move to… |
| Del | Move to Recycle Bin |
| Shift+Del | Delete permanently |
| F5 | Refresh the selected folder |
| Ctrl+R | Rescan everything |
| Ctrl+O | Scan a folder… |
| Ctrl+F | Focus the search box |
| Ctrl+Shift+F | Minimum size / extension / date filters |
| Esc | Stop scanning, clear the filter, cancel a move or delete |
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

### Project layout

FastDir is a Cargo workspace. The root crate is a thin binary; all reusable
logic lives in focused sub-crates so it can be built and tested without GPUI:

| Crate | Contents |
|---|---|
| `fastdir` (root) | `src/main.rs`, `build.rs` (icon / resource embedding) |
| `crates/fastdir-core` | Tree model, parallel scanner, drives, file-system ops, formatting |
| `crates/fastdir-treemap` | Squarified cushion layout, hit-testing and CPU rendering |
| `crates/fastdir-updater` | Release check, verified download and self-update |
| `crates/fastdir-settings` | Persisted settings |
| `crates/fastdir-ui` | GPUI window, list/treemap panels, dialogs and menus |

Dependency direction is `ui -> treemap -> core`, `ui -> updater` and
`ui -> settings`. Only `fastdir-ui` depends on GPUI.

Run the GUI-free test suite with:

```sh
cargo test -p fastdir-core -p fastdir-treemap -p fastdir-updater -p fastdir-settings
```

### Building the installer locally

Install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then:

```powershell
cargo build --release
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=0.1.0 installer\fastdir.iss
```

The installer is written to `dist\`.
