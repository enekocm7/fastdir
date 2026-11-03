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
- **Colour by age** – switch the treemap from extension colours to a
  new-to-old scale (under a week, a month, six months, a year, three years,
  older). The legend below the treemap totals each bucket, the colour-blind
  palette gets its own safe scale, and the file list can tint its *Last change*
  column to match.
- **Drive overview** – every mounted volume with its label, file system and
  how full it is. Click one to scan it, or pick any folder.
- **Live progress** – files, folders, bytes, files/s, the current path, and a
  real percentage when you scan a whole drive.
- **Partial results while scanning** – each top-level folder appears in the
  tree as soon as it finishes, and the treemap preview refreshes about once a
  second, so you can start browsing before the scan ends. Turn it off under
  `…` → *Show partial results while scanning*.
- **WinDirStat-style layout**
  - **Directory tree** with subtree-percentage bars, %, size, items, files,
    folders and last change. Sort by any column.
  - **Search and filters** – `Ctrl+F` filters the tree by name or glob
    (`*.iso`), expanding to every match. Filter by minimum size, extension and
    modified date (`Ctrl+Shift+F`).
  - **Top files** – a flat, sortable list of the N largest files in the whole
    scan (bounded heap, default 1,000), the quickest way to find cleanup
    targets.
  - **Extension list** with the colour each type has in the treemap. A
    *By category / By extension* switch groups extensions into file-type
    categories (video, images, audio, archives, documents, code, executables,
    disk images, system and other), expandable to their extensions, with a
    stacked share bar. Clicking a category highlights its files in the
    treemap, which can also be coloured by category.
  - **Cushion treemap** (squarified, SequoiaView-style shading), rendered on
    the CPU in parallel and drawn as one image. Hover to see a path, click
    to find the item in the tree, double-click to open it. Zoom into any
    folder and climb back out with the breadcrumb above the treemap.
  - Panels can be resized by dragging.
- **File operations** that update the tree in place, with no rescan:
  rename, move (also across drives), move to Recycle Bin, delete permanently,
  refresh a single folder, show in Explorer, open, copy path.
- **Custom actions** - wire cleanup commands into the right-click **Run**
  menu (`cargo clean`, `git gc`, `docker system prune`, "open a terminal
  here", "open in VS Code", ...) from `[[actions]]` in `config.toml` or
  Settings → Actions. Placeholders (`{path}`, `{name}`, `{parent}`, `{ext}`),
  folder/file filters, confirmations, optional output windows and an optional
  folder refresh afterwards. See [Custom actions](#custom-actions).
- **Multi-select**: Ctrl+click toggles, Shift+click selects a range (also in
  the treemap, where every selected cell is outlined). The status bar shows
  the count and total size; move / recycle / delete run on the whole
  selection after one confirmation.
- **Light, dark and system themes, plus deep settings** (`Ctrl+,`). The
  *System* option follows the Windows light / dark setting. Everything is
  remembered between launches: window geometry, panel sizes, sort order, the
  last scanned path, size units (binary / decimal), relative or absolute
  dates, visible columns, percent base, treemap palettes (including a
  colour-blind-safe one), flat shading, cell borders, age-tinted dates,
  list-only mode, excluded
    paths, hidden/system skipping, junction following (opt-in), thread count,
  startup mode, single-instance mode, interface scale, update channel and
  frequency, per-action delete confirmations, custom cleanup actions, whether
  the list stays on the current item after a delete (opt-in) and the default
  delete action.
- **Localized UI** (Settings → Language, applies immediately): English,
  Spanish, French, German, Italian, Portuguese, Dutch, Polish, Russian,
  Ukrainian, Turkish, Chinese, Japanese, Korean, Arabic and Czech. New
  installs follow the Windows display language; numbers and dates (digit
  grouping, decimal separator, date order, relative times like
  "3 days ago") follow the selected locale too.
- **Explorer integration** (optional, from the installer): right-click any
  folder or drive → *Analyze with FastDir*. It can also be toggled per-user
  under Settings → Integration (all-users installs stay installer-managed).

## Install

Download the latest release from the
[Releases page](https://github.com/enekocm7/fastdir/releases):

| File | |
|---|---|
| `FastDir-<version>-setup-x64.exe` | Installer: Start menu shortcut, optional desktop icon and Explorer context menu, uninstaller. Installs per-user by default (no admin prompt). |
| `FastDir-<version>-portable-x64.zip` | Just the executables (`FastDir.exe` and the console `fastdir-cli.exe`), no installation needed. |

### Updates

FastDir checks GitHub Releases in the background on its configured schedule
(per launch, daily or weekly; stable or pre-release channel). When a
newer version exists, a notification and a toolbar button offer **Update now**
(downloads, verifies the SHA-256 and relaunches), **Release notes** and **Skip
this version**. Use the toolbar's `…` menu to check manually or to turn the
automatic check off. All settings live in `%APPDATA%\FastDir\config.toml`,
grouped into `[scan] [treemap] [list] [sort] [ops] [startup] [updates]` and
`[appearance]`, `[integration]`, `[locale]`, `[categories]` and `[[actions]]`
sections (older config files migrate automatically). Unknown extensions fall
into *Other*; move or add extensions to a category in `[categories]`:

```toml
[categories]
video = ["mp4", "m4v", "custom-video"]
documents = ["pdf", "txt", "my-notes"]
```

## Usage

```text
fastdir.exe              open the drive overview
fastdir.exe D:\          scan a drive right away
fastdir.exe "C:\Users"   scan a folder right away
fastdir.exe scan.json    open a scan saved earlier
```

### Opening a saved scan

A scan exported from the GUI (JSON) or written by `fastdir-cli scan` can be
opened again without rescanning, which is handy when the data came from a
server, a NAS or another machine:

- **Open scan…** in the toolbar `…` menu or on the drive overview
  (`Ctrl+Shift+O`), with a `.json` file picker.
- Pass the file on the command line (`fastdir.exe scan.json`) or drop it on
  the window.
- A banner marks the tree as a **saved scan** and shows the root and the time
  it was scanned.
- Rename, move, recycle, delete, open and reveal only work when the selected
  path still exists on this machine. **Rescan** starts a real scan of the
  saved root (or asks for a folder when it no longer exists).

JSON is a browsing snapshot: hard-link markers and drive pseudo-items
(`<Free space>`, `<Unknown>`) come back as plain files, so their bytes are
kept but the UI cannot tell them apart. Opening a truncated document
(`--max-depth` or `--no-files`) also means totals only reflect the entries
that were written.

### Custom actions

FastDir can run your own commands on the selected file or folder. They appear
in the right-click **Run** submenu (and, optionally, on `Ctrl+1`…`Ctrl+9`).
Define them under Settings → Actions or directly in `config.toml`:

```toml
[[actions]]
name = "cargo clean"
command = "cargo"
args = ["clean"]
working_dir = "{path}"        # placeholders: {path} {name} {parent} {ext}
applies_to = "dir"            # dir | file | both
match = "Cargo.toml"          # file the folder must contain, or a name glob
confirm = true                # show the exact command line first
refresh_after = true          # re-scan the folder in place afterwards
show_output = false           # show stdout/stderr in a dialog
enabled = true
shortcut = 1                  # optional: Ctrl+1

[[actions]]
name = "Open in VS Code"
command = "code"
args = ["{path}"]
applies_to = "both"
confirm = false
```

- **Placeholders** are replaced per item. `{path}` is the full path, `{name}`
  the file/folder name, `{parent}` the containing folder and `{ext}` the
  extension (empty for folders).
- **Direct execution**: commands are spawned directly, not through a shell,
  so paths with spaces or non-ASCII characters work without quoting. Set
  `shell = true` to run through `cmd /C` / `sh -c` instead; placeholder
  values are then quoted for the platform shell.
- **`match`** narrows where an action appears: a plain name must be a file the
  folder contains (`Cargo.toml`, `.git`), and a glob (`*.zip`) is matched
  against the item's name. For files, a plain name compares with the file name.
- **`working_dir`** defaults to the item (a file's folder for files); `{path}`
  and `{parent}` can be used explicitly.
- **Safety**: actions with `confirm = true` show the full command line before
  anything runs. FastDir ships four disabled examples (open terminal here,
  open in VS Code, `cargo clean`, `git gc`) to copy from. Multi-select runs
  the action once per item through the same queue as deletes and moves.

### Command line (for scripts and agents)

FastDir also runs headless: it scans, prints JSON or CSV and exits without
opening a window. Use **`fastdir-cli`** (shipped next to `fastdir.exe`) from
a terminal. `fastdir.exe` accepts the same commands, but it is a GUI program
on Windows, so `cmd.exe` does not wait for it; it is fine when its output is
piped or redirected.

```text
fastdir-cli scan <path> [options]   the tree under <path> (JSON by default)
fastdir-cli top <path> [options]    the largest files under <path>
fastdir-cli mcp                     MCP server on stdin/stdout (see below)
fastdir-cli help [<command>]        help for a command
fastdir-cli --version
```

```powershell
# Ten largest folders directly under C:\Users
fastdir-cli scan C:\Users --max-depth 1 --no-files |
  jq '.tree.children | sort_by(-.size) | .[:10] | map({path, size})'

# Fifty largest files on D:, as CSV
fastdir-cli top D:\ --limit 50 --format csv -o big-files.csv
```

| Option | Commands | |
|---|---|---|
| `--format json\|csv` | scan, top | Output format (default `json`; CSV uses the export dialog's columns) |
| `-o, --output <file\|->` | scan, top | Write to a file instead of stdout |
| `--pretty` | scan, top | Indent the JSON |
| `--max-depth <n>` | scan | List entries up to `<n>` levels below `<path>` (`0` = just the root). Sizes always count everything. |
| `--no-files` | scan | List folders only |
| `-n, --limit <n>` | top | How many files (default 20) |
| `--threads <n>` | scan, top | Scan threads, 1–64 (default automatic) |
| `--exclude <glob>` | scan, top | Skip entries whose name matches; repeatable |
| `--follow-junctions` | scan, top | Descend into junctions / symlinked folders |
| `--cross-volumes` | scan, top | With `--follow-junctions`, also enter other volumes |
| `--skip-hidden` (`--skip-system`) | scan, top | Skip hidden and system entries |
| `-q, --quiet` | scan, top | Nothing on stderr except errors |

The command line uses these flags only; it does not read `config.toml`.

**Output streams.** The result goes to stdout. stderr gets a live progress
line (only when it is a terminal) and a one-line summary, unless `--quiet`.
**Ctrl+C** stops the scan cleanly; nothing is written to stdout. A second
Ctrl+C ends the process at once.

**Exit codes.**

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error while scanning or writing the output |
| 2 | Invalid arguments |
| 3 | Path not found or not a directory |
| 4 | Permission denied |
| 130 | Cancelled with Ctrl+C |

#### JSON schema

Each document has a `schema` name and a `version` (currently `1`). Within a
version, fields may be added but are never renamed or removed. Sizes are
bytes, and timestamps are ISO 8601 UTC (`null` when unknown).

`fastdir scan` (`schema: "fastdir.scan"`):

```jsonc
{
  "schema": "fastdir.scan", "version": 1, "fastdir_version": "0.2.0",
  "root": "C:\\Users",               // absolute path that was scanned
  "scanned_at": "2026-11-03T12:00:00Z",
  "elapsed_ms": 1234,
  "errors": 2,                        // folders that could not be read
  "tree": {                           // an entry, see below
    "name": "C:\\Users", "path": "C:\\Users", "type": "dir",
    "size": 123456789,                // logical size
    "disk_size": 130000000,           // allocated on disk
    "items": 4567, "files": 4000, "folders": 567,
    "modified": "2026-11-02T18:30:00Z",
    "error": false,                   // this folder could not be read
    "hard_link": false,               // extra hard link, bytes counted elsewhere
    "truncated": false,               // dirs only: children cut off by --max-depth
    "children": [ /* entries, largest first; dirs only */ ]
  }
}
```

`type` is `dir` or `file`. Files have the same fields without `truncated` and
`children` (`files` is `1` and `items` is `1` for a file).

`fastdir top` (`schema: "fastdir.top"`) has the same `schema`, `version`,
`fastdir_version`, `root`, `scanned_at`, `elapsed_ms` and `errors` fields,
plus `limit`, `total` (`size`, `disk_size`, `files`, `folders` of the whole
scan) and `files`: a list of `{ name, path, extension, size, disk_size,
modified }`, largest first. Hard links are listed once.

### MCP server

`fastdir-cli mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server over stdio, so agents such as Claude Code can query disk usage. Scans
stay in memory (the four most recent folders), so follow-up questions do not
rescan. A tool given a folder outside every cached scan scans it first.

| Tool | |
|---|---|
| `scan_path` | Scan (or rescan) a folder or drive; returns its totals and largest entries |
| `get_children` | Entries directly inside a folder, largest first |
| `largest_files` | Largest files anywhere under a folder |
| `search` | Find entries by name substring or glob, minimum size, extension or type |
| `path_info` | One entry's sizes and counts, and its share of its parent and of the scan |
| `export_scan` | Write a folder to a JSON (`fastdir.scan`) or CSV file |

Only read-only tools are offered by default. Start the server with
`--allow-destructive` to also offer `recycle_path` and `delete_path`
(permanent; the tool requires `confirm: true`).

Register it with Claude Code:

```sh
# Per-user install path; all-users installs live in C:\Program Files\FastDir
claude mcp add fastdir -- "%LOCALAPPDATA%\Programs\FastDir\fastdir-cli.exe" mcp
```

Or in any client's JSON configuration:

```json
{ "mcpServers": { "fastdir": { "command": "fastdir-cli", "args": ["mcp"] } } }
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
| Ctrl+1 … Ctrl+9 | Run the custom action bound to that number |
| Esc | Stop scanning, clear the filter, cancel a move or delete |
| Ctrl+T | Toggle light / dark |
| Ctrl+, | Open settings |

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
| `fastdir` (root) | `src/main.rs` (GUI, or the CLI for subcommands), `build.rs` (icon / resource embedding) |
| `crates/fastdir-core` | Tree model, parallel scanner, drives, file-system ops, formatting |
| `crates/fastdir-treemap` | Squarified cushion layout, hit-testing and CPU rendering |
| `crates/fastdir-updater` | Release check, verified download and self-update |
| `crates/fastdir-settings` | Persisted settings (`config.toml`) |
| `crates/fastdir-ui` | GPUI window, list/treemap panels, dialogs and menus |
| `crates/fastdir-cli` | Headless CLI (`fastdir-cli` binary), agent JSON schema and MCP server |

Dependency direction is `ui -> treemap -> core`, `ui -> updater` and
`ui -> settings`; `cli -> core`. Only `fastdir-ui` depends on GPUI.

Run the GUI-free test suite with:

```sh
cargo test -p fastdir-core -p fastdir-treemap -p fastdir-updater -p fastdir-settings -p fastdir-cli
```

### Benchmarks

Scanner, tree-building and treemap benchmarks use
[criterion](https://github.com/bheisler/criterion.rs) and work on Windows,
Linux and macOS:

```sh
cargo bench -p fastdir-core       # scan (default, exclusions, 1 vs all threads), arena build, sorting
cargo bench -p fastdir-treemap    # squarify layout for 1k / 10k / 100k nodes, CPU render
```

The scan benchmarks use a synthetic tree (200k files, 20k directories, a few
huge folders, fixed seed) that is generated once in `target/fastdir-bench-tree`
and reused; delete that folder to regenerate it. Throughput is reported in
items per second next to the time. Run on an otherwise idle machine; these are
**warm-cache** numbers.

To check a change against a baseline:

```sh
git checkout main
cargo bench -p fastdir-core -- --save-baseline main
git checkout my-branch
cargo bench -p fastdir-core -- --baseline main
```

For a **cold-cache** run before a release, scan a real folder after dropping the
OS file cache: reboot (or on Linux `sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`,
on macOS `sudo purge`), then run
`cargo run --release -p fastdir-cli -- scan <path>` once and note the time.
Subsequent runs are warm.

### Faster local builds

On Windows, `.cargo/config.toml` links with `rust-lld` (ships with rustup), and
debug builds keep only line tables for workspace crates and no debug info for
dependencies, which keeps incremental rebuilds to a few seconds. On a nightly
toolchain, the `cargo bn` / `rn` / `cn` / `tn` aliases (build / run / check /
test) also turn on the parallel frontend and shared generics
(`.cargo/nightly.toml`). Pick one style and stick to it, because switching
rustflags rebuilds everything.

### Building the installer locally

Install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then:

```powershell
cargo build --release
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=0.2.0 installer\fastdir.iss
```

The installer is written to `dist\`.
