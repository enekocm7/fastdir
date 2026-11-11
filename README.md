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
- **Fast NTFS scan (opt-in)** - with *Settings → Fast NTFS scan (MFT)* (or
  `--mft` on the command line) and FastDir running as administrator, an NTFS
  volume is scanned by reading its Master File Table directly: large
  sequential reads instead of one listing per folder. Sizes, size on disk,
  hard links and `<Unknown>` follow the same rules as the normal scan. On
  other file systems, without administrator rights, or when following
  junctions or collecting owners, the normal scan runs instead.
- **NTFS alternate data streams (opt-in)** - with *Settings → Count alternate
  data streams* (or `--streams` on the command line), named streams such as
  the `Zone.Identifier` a downloaded file carries are measured too and shown
  in their folder as `<Alternate streams>` instead of being left in
  `<Unknown>`. Free with the fast NTFS scan; the normal scan has to open every
  file, so it is noticeably slower. Non-NTFS volumes are unaffected.
- **Size on disk** - every row shows the allocated size next to the logical
  size, so compressed, sparse and tiny cluster-slack files are counted the way
  the volume sees them. The treemap can use either metric (`. menu` →
  *Treemap: size on disk*).
- **Hard links counted once** - files sharing a file ID (for example under
  `C:\Windows\WinSxS`) are counted the first time and marked as `hard link`
  afterwards, so totals are not inflated.
- **File owners and per-user totals** - opt-in *Collect file owners* scan
  setting (`--owners` in the CLI) records who owns each entry as
  `DOMAIN\user` (the raw SID when the account cannot be resolved). An
  **Owner** column can be shown in the tree and top-files lists, and a
  *By owner* view totals size, size on disk and file count per account.
  Click a user to highlight their files in the treemap and filter the list.
  Off by default: Windows needs one security query per file, which can make
  a scan several times slower, while Unix takes the uid from the listing it
  already does. Memory and the default scan are unaffected (an owner is a
  `u16` in a side array that only exists when the option is on).
- **Colour by age** – switch the treemap from extension colours to a
  new-to-old scale (under a week, a month, six months, a year, three years,
  older). The legend below the treemap totals each bucket, the colour-blind
  palette gets its own safe scale, and the file list can tint its *Last change*
  column to match.
- **Compare two scans** – pick a saved scan of the same folder (`…` →
  *Compare with a saved scan…*). The directory tree gains **Δ size**, **Δ %**
  and **Status** columns (sorted grew-the-most first), a **Removed** tab lists
  what disappeared, and the treemap can colour by change (green = shrank,
  red = grew, with a legend). The status bar shows the net change and, for
  drive scans, the change in free space. Only changed nodes are kept, so the
  comparison does not double the memory of a large scan.
- **Scan history and growth over time** – save timestamped snapshots
  (`…` → *Save snapshot*, automatically after every scan, or from a scheduled
  `fastdir-cli snapshot`) and see how a folder grew: `…` → *Growth over
  time…* charts the selected folder's size across every snapshot that
  contains it, with the change between snapshots and a one-click comparison.
  Retention settings keep the snapshot folder from growing without limit.
- **Standalone HTML report** – *Export* → *HTML report* (or `fastdir-cli scan
  <path> --format html -o report.html`) writes one self-contained `.html`
  file with the summary totals, a treemap image, the largest files and a
  collapsible folder tree. Everything is inlined, so it opens in any browser
  with no network access and without FastDir installed. The export dialog's
  depth and *include files* options also bound the report.
- **Drive overview** – every mounted volume with its label, file system and
  how full it is. Click one to scan it, or pick any folder.
- **`<Free space>` and an explained `<Unknown>`** – a whole-drive scan adds
  the free space and `<Unknown>` (used space the scan could not attribute to
  a file) so the picture adds up to the volume. What can be measured is
  split out of `<Unknown>`:
  - `<NTFS metadata>`: the metadata files a listing never returns (`$MFT`
    and friends, often several GB on a system drive).
  - `<Shadow copies>`: restore-point storage in `System Volume Information`,
    measured when running as administrator. Double-click it to open the
    System Protection settings (nothing is deleted).
  - `<Alternate streams>`: NTFS named data streams (for example
    `Zone.Identifier`), shown inside the folder that holds them when
    *Count alternate data streams* / `--streams` is on.
  - Store apps: `C:\Program Files\WindowsApps` cannot be listed without
    elevation, but each registered package folder is read on its own and
    shown under it, so those GBs are no longer unknown.

  Folders that could not be read are listed by **Show folders** on the
  warning banner, or by double-clicking `<Unknown>`; click one to jump to it
  in the tree. Run as administrator to include them. When *Skip the Windows
  folder* is on, the same dialog notes that its bytes are in `<Unknown>`. The
  CLI lists the unreadable folders in the `unreadable` field and also reads
  Store packages one by one.
- **Live progress** – files, folders, bytes, files/s, the current path, and a
  real percentage when you scan a whole drive.
- **Partial results while scanning** – each top-level folder appears in the
  tree as soon as it finishes, and the treemap preview refreshes about once a
  second, so you can start browsing before the scan ends. Turn it off under
  `…` → *Show partial results while scanning*.
- **WinDirStat-style layout**
  - **Directory tree** with subtree-percentage bars, %, size, items, files,
    folders, last change and (when owners are collected) owner. Sort by any
    column.
  - **Search and filters** – `Ctrl+F` filters the tree by name or glob
    (`*.iso`), expanding to every match. Filter by minimum size, extension and
    modified date (`Ctrl+Shift+F`).
  - **Cleanup finders** – the same dialog (`Ctrl+Shift+F`) has a *cleanup
    finders* choice that narrows the list to empty folders, zero-byte files or
    paths longer than a length limit (260 characters by default). Results are
    ordinary rows: select, open, delete or move them as usual.
  - **Top files** – a flat, sortable list of the N largest files in the whole
    scan (bounded heap, default 1,000), the quickest way to find cleanup
    targets.
  - **Duplicate finder** – the **Duplicates** tab (and `fastdir-cli
    duplicates` / the MCP `find_duplicates` tool) groups byte-identical files
    by size, a 64 KB prefix/suffix sample and then a full BLAKE3 hash, and
    reports how much space each group wastes. Zero-byte files and extra hard
    links are not duplicates. In the GUI, keep one copy of a group and send
    the rest to the Recycle Bin or replace them with hard links.
  - **Cleanup suggestions** – the **Cleanup** tab lists well-known places that
    hold reclaimable space in the current scan, biggest first: temp folders,
    browser caches, `node_modules` and Cargo `target` folders, Windows Update
    downloads, a previous Windows installation (`Windows.old`) and stale
    downloads (older than 90 days). Each suggestion shows its size and item
    count; **Recycle** / **Delete** clean up the selected suggestion after
    asking for confirmation (always, regardless of the confirm settings) and
    run through the same queue as other deletes. `Enter` shows the suggestion
    in the tree.
  - **Extension list** with the colour each type has in the treemap. A
    *By category / By extension* switch groups extensions into file-type
    categories (video, images, audio, archives, documents, code, executables,
    disk images, system and other), expandable to their extensions, with a
    stacked share bar. The same panel switches to *By owner* when the scan
    collected owners. Clicking a category (or an owner) highlights its files
    in the treemap, which can also be coloured by category.
  - **Cushion treemap** (squarified, SequoiaView-style shading), rendered on
    the CPU in parallel and drawn as one image. Hover to see a path, click
    to find the item in the tree, double-click to open it. Zoom into any
    folder and climb back out with the breadcrumb above the treemap.
  - **Sunburst view** as an alternative to the treemap (`…` → *Show as
    sunburst*, or *Settings → Treemap → View*; `layout = "sunburst"` under
    `[treemap]` in the config). The folder sits in the middle and every
    level below it is a ring, up to eight levels deep; zoom in to go
    deeper. It shares everything with the treemap: selection, hover, zoom,
    the breadcrumb, palettes, the extension / age / category / change colour
    modes, highlights, free space / unknown and the size-on-disk metric.
    Clicking the centre selects the folder being shown.
  - Panels can be resized by dragging.
- **File operations** that update the tree in place, with no rescan:
  rename, move (also across drives), move to Recycle Bin, delete permanently,
  refresh a single folder, show in Explorer, open, copy path.
- **Compression** (Windows on NTFS volumes): right-click **Compress…** to
  shrink files and folders with NTFS compression (LZNT1) or the CompactOS /
  WOF algorithms (XPRESS4K, XPRESS8K, XPRESS16K, LZX). The dialog shows the
  item count, total size, an estimated saving and can skip already-compressed
  formats (`.zip`, `.7z`, `.mp4`, `.jpg`, …); folders can also be flagged so
  new files inherit NTFS compression. It runs on the file-operation queue
  with progress and cancel, reports items that could not be compressed, and
  updates the size on disk in place. **Uncompress** reverses it. The actions
  are hidden on non-NTFS volumes and on machines that are not Windows.
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
- **Deletion basket**: press **Ctrl+B** to collect the selection while you
  browse (across folders, tabs, filters and rescans) — the toolbar button
  shows the item count and total size. **Ctrl+Shift+B** (or the toolbar
  button) opens the basket to review the list, take items out, empty it,
  then recycle or delete everything in one confirmed step. Items inside a
  collected folder are covered by it and never counted or deleted twice.
- **Light, dark and system themes, plus deep settings** (`Ctrl+,`). The
  *System* option follows the Windows light / dark setting. Everything is
  remembered between launches: window geometry, panel sizes, sort order, the
  last scanned path, size units (binary / decimal), relative or absolute
  dates, visible columns, percent base, treemap palettes (including a
  colour-blind-safe one), flat shading, cell borders, age-tinted dates,
  list-only mode, excluded
    paths, hidden/system skipping, skipping the Windows folder (opt-in, makes
  system-drive scans much faster), fast NTFS scanning through the MFT (opt-in,
  needs administrator rights), junction following (opt-in), thread count,
  startup mode, single-instance mode, interface scale, update channel and
  frequency, per-action delete confirmations, custom cleanup actions, whether
  the list stays on the current item after a delete (on by default) and the
  default delete action.
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

### Linux

Every Linux package installs both `fastdir` (the GUI) and `fastdir-cli`, plus an
application menu entry, an icon and AppStream metadata. x86_64 only for now.

| Distribution | Install | Updates |
|---|---|---|
| Debian, Ubuntu, Mint, Pop!_OS | `sudo apt install ./FastDir-<version>-amd64.deb` | install the next `.deb` the same way |
| Fedora, RHEL / Alma / Rocky, openSUSE | `sudo dnf install ./FastDir-<version>-x86_64.rpm` (`sudo zypper install …` on openSUSE) | install the next `.rpm` the same way |
| Arch, Manjaro, EndeavourOS | `paru -S fastdir-bin` (or any AUR helper) | your AUR helper |
| NixOS / Nix | `nix run github:enekocm7/fastdir` (CLI: `nix run github:enekocm7/fastdir#fastdir-cli -- scan /`) | `nix profile upgrade` / your flake inputs |
| Any distribution | `chmod +x FastDir-<version>-x86_64.AppImage && ./FastDir-<version>-x86_64.AppImage` | in the app (**Update now** replaces the AppImage file) |
| Any distribution | unpack `FastDir-<version>-linux-x64.tar.gz` | download the next release by hand |

FastDir never replaces a binary that a package manager installed: for
`.deb`, `.rpm`, AUR and Nix installs the update notification tells you to
update from your package manager instead. It renders through Vulkan, so a
Vulkan driver is needed (Mesa's `mesa-vulkan-drivers` on most distributions).
Flathub ([#114](https://github.com/enekocm7/fastdir/issues/114)), Snap
([#115](https://github.com/enekocm7/fastdir/issues/115)), Alpine
([#116](https://github.com/enekocm7/fastdir/issues/116)), nixpkgs
([#117](https://github.com/enekocm7/fastdir/issues/117)) and the Debian
archive ([#118](https://github.com/enekocm7/fastdir/issues/118)) are planned. Release and packaging
details for contributors are in [`packaging/README.md`](packaging/README.md).

### Updates

FastDir checks GitHub Releases in the background on its configured schedule
(per launch, daily or weekly; stable or pre-release channel). When a
newer version exists, a notification and a toolbar button offer **Update now**
(downloads, verifies the SHA-256 and relaunches), **Release notes** and **Skip
this version**. Use the toolbar's `…` menu to check manually or to turn the
automatic check off. All settings live in `%APPDATA%\FastDir\config.toml`,
grouped into `[scan] [treemap] [list] [sort] [ops] [startup] [updates]` and
`[appearance]`, `[integration]`, `[locale]`, `[snapshots]`, `[categories]` and
`[[actions]]` sections (older config files migrate automatically). Unknown extensions fall
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
fastdir.exe scan.fdscan  open a binary scan saved earlier
```

### Opening a saved scan

A scan exported from the GUI (JSON) or written by `fastdir-cli scan` (JSON or
the compact binary `.fdscan` format) can be opened again without rescanning,
which is handy when the data came from a server, a NAS or another machine:

- **Open scan…** in the toolbar `…` menu or on the drive overview
  (`Ctrl+Shift+O`), with a `.json` / `.fdscan` file picker.
- Pass the file on the command line (`fastdir.exe scan.json`) or drop it on
  the window.
- A banner marks the tree as a **saved scan** and shows the root and the time
  it was scanned.
- Rename, move, recycle, delete, open and reveal only work when the selected
  path still exists on this machine. **Rescan** starts a real scan of the
  saved root (or asks for a folder when it no longer exists).

Both formats keep hard-link markers: an extra link is listed without its
bytes, which are counted once at the entry that owns them. JSON documents
written by older versions have no markers, so those entries come back as
plain files (the bytes are still kept). Drive pseudo-items (`<Free space>`,
`<Unknown>`) and the per-folder `<Alternate streams>` item keep their kind,
which is what lets a later comparison report the change in free space. Owner
names, when the document has them, are kept and shown again. Opening a
truncated document (`--max-depth` or `--no-files`) also means totals only
reflect the entries that were written.

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
### Comparing two scans

**Compare with a saved scan…** in the toolbar `…` menu diffs the current tree
(possibly itself a saved scan) against a saved scan (JSON or `.fdscan`)
opened from disk. Items are matched by their relative path under the root, so
the saved scan does not have to come from this machine.

- The tree gets **Δ size**, **Δ %** and **Status** (New / Grew / Shrank /
  Unchanged) columns. Click them to sort; the initial order is the folder that
  grew the most.
- A **Removed** tab lists items that only exist in the baseline. A removed
  folder is one row with its total size and item count, not one row per file.
- `…` → *Colour treemap by change* switches the treemap to diverging colours
  (green shrank, red grew, grey unchanged) with a legend; the intensity follows
  the size of the change.
- The status bar shows the net change and, when both documents are whole-drive
  scans with volume data, the change in free space.
- A root mismatch (the saved scan is of another folder) shows a warning but
  still compares.

The diff keeps only changed nodes plus the top-most removed entries, so a
comparison starts quickly and does not keep a second copy of the whole tree in
memory.

### Scan history and growth over time

A **snapshot** is a compact saved scan with the time it was taken. Snapshots
live in `%APPDATA%\FastDir\snapshots`, one sub-folder per scanned folder or
drive, named by UTC time (`20261110T081500Z.json`). Each one is an ordinary
saved scan: it opens with *Open scan…*, works with *Compare with a saved
scan…* and with `fastdir-cli diff`.

- **Saving.** `…` → *Save snapshot* saves the scan on screen. Settings →
  Snapshots → *Save a snapshot after every scan* does it automatically (off
  by default, since whole-drive snapshots can be large). For a regular
  history without opening the app, schedule `fastdir-cli snapshot` (below).
- **Growth over time.** `…` → *Growth over time…* shows the selected folder
  (or the scan root) in every snapshot that contains it: snapshots of the
  folder itself *and* of any folder or drive above it, so a nightly snapshot
  of `D:\` gives a history for every folder on `D:`. The dialog charts the
  size per snapshot (plus the current scan), lists the change between
  snapshots and offers **Compare** to open the full comparison view against a
  snapshot of the same root.
- **Retention.** Settings → Snapshots keeps at most 10, 30 (default) or 100
  snapshots per folder, or no limit, and removes snapshots older than 90 days,
  a year (default) or never. It is applied every time a snapshot is saved; the
  newest snapshot is always kept. The same values live in `config.toml`:

```toml
[snapshots]
dir = "D:\\FastDir snapshots"   # optional; default %APPDATA%\FastDir\snapshots
auto_save = false
keep_count = 30                 # 0 = no limit
max_age_days = 365              # 0 = no limit
```

A scheduled task that snapshots `D:` every night and follows the app's
retention settings:

```powershell
# Per-user install path; all-users installs live in C:\Program Files\FastDir
$cli = "$env:LOCALAPPDATA\Programs\FastDir\fastdir-cli.exe"
$action = New-ScheduledTaskAction -Execute $cli -Argument 'snapshot D:\ --config --quiet'
$trigger = New-ScheduledTaskTrigger -Daily -At 3am
Register-ScheduledTask -TaskName 'FastDir snapshot D' -Action $action -Trigger $trigger
```

### Finding duplicates

The **Duplicates** tab next to *Top files* searches the current scan for
files with identical content (1 MB and larger). Hashing runs in the
background with a progress line and a Cancel button; only files that share a
size are read, and only those whose first and last 64 KB also match are
hashed in full.

- Results are grouped, largest waste first: each group header shows how many
  copies there are and how much space the extra ones waste.
- Select a copy to keep it (selecting a group header keeps its first copy),
  then use **Other copies to Recycle Bin** or **Hard-link other copies**.
  Both ask for confirmation and run through the same queue as deletes and
  moves.
- Hard-linking re-checks that the files are still byte-identical, then
  replaces each extra copy with a hard link to the kept one, so every path
  keeps working while the data is stored once. Editing any of the linked
  paths changes all of them. All copies must be on the same NTFS volume.
- The list and treemap update in place: deleted, moved and hard-linked copies
  drop out of their groups. `Enter` shows the selected copy in the tree; the
  context menu and `Delete` act on it like anywhere else.

### Cleaning up known locations

The **Cleanup** tab next to *Duplicates* looks for reclaimable space in the
current scan: temp folders, browser caches, `node_modules` and Cargo `target`
folders (the latter only next to a `Cargo.toml`), Windows Update downloads,
`Windows.old` and downloads older than 90 days. Suggestions below 1 MB are
skipped and only locations inside the scan take part.

- Results are listed biggest first with their size and item count, plus a
  total at the top.
- **Move to Recycle Bin** / **Delete permanently** clean up the selected
  suggestion after asking for confirmation — always, whatever the confirm
  settings say, since a suggestion can stand for thousands of files. Both run
  through the same file-operation queue as other deletes, and the list refreshes
  in place as its targets leave the tree.
- Temp folders, caches and Windows Update files suggest their *contents* (the
  folder itself stays); `Windows.old` and stale downloads suggest the folders
  and files themselves.
- `Enter` shows the selected suggestion in the tree. The regular `Delete` key
  and the context menu stay away from this tab: a suggestion stands for many
  items, so it can only be cleaned up through the tab's own confirmed actions.

### Command line (for scripts and agents)

FastDir also runs headless: it scans, prints JSON, NDJSON or CSV (or writes
an HTML report) and exits without opening a window. Use **`fastdir-cli`**
(shipped next to `fastdir.exe`) from a terminal. `fastdir.exe` accepts the
same commands, but it is a GUI program on Windows, so `cmd.exe` does not wait
for it; it is fine when its output is
piped or redirected.

```text
fastdir-cli scan <path> [options]   the tree under <path> (JSON by default)
fastdir-cli top <path> [options]    the largest files under <path>
fastdir-cli duplicates <path> [options]
                                    groups of identical files and the space they waste
fastdir-cli diff <old> <new>        what changed between two scans
fastdir-cli snapshot <path> [options]
                                    scan <path> and save a timestamped snapshot
fastdir-cli history <path> [options]
                                    the size of <path> in every saved snapshot
fastdir-cli mcp                     MCP server on stdin/stdout (see below)
fastdir-cli config [<file>]         the scan options a config file gives (see --config)
fastdir-cli help [<command>]        help for a command
fastdir-cli --version
```

```powershell
# Ten largest folders directly under C:\Users
fastdir-cli scan C:\Users --max-depth 1 --no-files |
  jq '.tree.children | sort_by(-.size) | .[:10] | map({path, size})'

# Fifty largest files on D:, as CSV
fastdir-cli top D:\ --limit 50 --format csv -o big-files.csv

# A self-contained HTML report of D:\Projects to share
fastdir-cli scan D:\Projects --format html -o projects-report.html

# Duplicate videos over 100 MB under D:, largest waste first
fastdir-cli duplicates D:\ --min-size 100MB --ext .mp4 --pretty

# What grew on D: since Monday's saved scan (saved.json)?
fastdir-cli diff saved.json D:\ --min-change 1GB

# Save a whole drive as a compact binary scan (loads faster than JSON)
fastdir-cli scan D:\ --format bin -o D.fdscan

# Stream a whole drive one entry per line while it scans: folders over 10 GB
fastdir-cli scan D:\ --format ndjson --no-files |
  jq -c 'select(.record == "entry" and .size > 10e9) | {path, size}'

# ISO files over 1 GB that were not touched this year
fastdir-cli top D:\ --ext .iso --min-size 1GB --older-than 1y

# Nightly snapshot of D: (folders only), keeping 60 days
fastdir-cli snapshot D:\ --no-files --max-age 60d --quiet

# How did D:\Projects grow over those snapshots?
fastdir-cli history D:\Projects --format csv
```

`snapshot` scans `<path>`, saves the result in the snapshot folder (see
[Scan history](#scan-history-and-growth-over-time)) and prints a small
`fastdir.snapshot` summary. Without `--keep`, `--max-age` or `--config` it
removes nothing; with `--config` it uses the `[snapshots]` folder and
retention of `config.toml` (flags still win). `history` reads the snapshots
only and never scans.

`diff` takes a saved scan (JSON or binary `.fdscan`) as `<old>` and either
another saved scan or a folder to scan now as `<new>`. It uses the same
matcher as the GUI: entries
are paired by relative path, so the baseline can come from another machine.
A new or removed folder is one row with its subtree total, and every other
row has its own sizes.

| Option | Commands | |
|---|---|---|
| `--format json\|csv\|ndjson\|bin\|html` | scan | Output format (default `json`; `ndjson` streams one entry per line while the scan runs, see [NDJSON streams](#ndjson-streams); `bin` writes the compact `.fdscan` scan; `html` writes one self-contained report with the summary, a treemap image, the largest files and a folder tree, honouring `--max-depth` and `--no-files` but not filters) |
| `--format json\|csv\|ndjson` | top | Output format (default `json`; `ndjson` writes one file per line) |
| `--format json\|csv` | duplicates, diff | Output format (default `json`; CSV uses the export dialog's columns) |
| `-o, --output <file\|->` | scan, top, duplicates, diff | Write to a file instead of stdout |
| `--pretty` | scan, top, duplicates, diff | Indent the JSON (ignored for `ndjson`) |
| `--max-depth <n>` | scan | List entries up to `<n>` levels below `<path>` (`0` = just the root). Sizes always count everything. |
| `--max-depth <n>` | diff | Only list changed entries up to `<n>` levels below the root (`0` = the root row) |
| `--no-files` | scan | List folders only |
| `--name <text\|glob>` | scan, top | Keep entries whose name contains `<text>`, or matches the glob when it contains `*`/`?` (case-insensitive) |
| `--min-size <size>` | scan, top | Keep entries at least this large (`500MB`, `1.5 GiB`) |
| `--ext <ext>` | scan, top | Keep only files with this extension; repeatable |
| `--newer-than <date\|age>` | scan, top | Keep entries modified on/after a date (`2026-01-01`) or within an age (`30d`, `6m`, `1y`) |
| `--older-than <date\|age>` | scan, top | Keep entries modified on/before a date, or older than an age |
| `-n, --limit <n>` | top | How many files (default 20) |
| `-n, --limit <n>` | diff | Cap the number of changed entries (largest \|change\| first) |
| `--min-change <size>` | diff | Hide changes smaller than `<size>` (e.g. `20GB`, `500MB`) |
| `--min-size <size>` | duplicates | Smallest file to hash (default 1 MB); accepts `500MB` or `1.5 GiB` |
| `--ext <ext>` | duplicates | Only files with this extension; repeatable |
| `-n, --limit <groups>` | duplicates | How many groups to report (default all); totals still count every group |
| `--threads <n>` | scan, top, duplicates, diff, snapshot | Scan threads, 1–64 (default automatic) |
| `--exclude <glob>` | scan, top, duplicates, diff, snapshot | Skip entries whose name matches; repeatable |
| `--follow-junctions` | scan, top, duplicates, diff, snapshot | Descend into junctions / symlinked folders |
| `--cross-volumes` | scan, top, duplicates, diff, snapshot | With `--follow-junctions`, also enter other volumes |
| `--skip-hidden` (`--skip-system`) | scan, top, duplicates, diff, snapshot | Skip hidden and system entries |
| `--skip-windows` | scan, top, duplicates, diff, snapshot | Skip the Windows folder (`%SystemRoot%`, usually `C:\Windows`) for a much faster system-drive scan; its bytes count as `<Unknown>` |
| `--mft` | scan, top, duplicates, diff, snapshot | Read the NTFS Master File Table directly (much faster on large volumes). Needs administrator rights; otherwise a normal scan runs and the summary on stderr says so |
| `--streams` | scan, top, duplicates, diff, snapshot | Measure NTFS alternate data streams and show them in each folder as `<Alternate streams>`. Free with `--mft`; the normal scan opens every file, so it is slower. Off by default |
| `--owners` | scan, top, diff, snapshot | Record the owning account of every entry (Owner field, per-user totals) |
| `-q, --quiet` | scan, top, duplicates, diff, snapshot | Nothing on stderr except errors |
| `--dir <folder>` | snapshot, history | Snapshot folder (default `%APPDATA%\FastDir\snapshots`, or `[snapshots] dir` with `--config`) |
| `--keep <n>` | snapshot | Keep at most `<n>` snapshots of this root (`0` = no limit); the newest is always kept |
| `--max-age <age>` | snapshot | Remove snapshots older than `90d`, `6m`, `1y`, … (`0` = no limit) |
| `--max-depth <n>` / `--no-files` | snapshot | Store a smaller snapshot: entries up to `<n>` levels / folders only (sizes still count everything) |
| `--format json\|csv`, `-o`, `--pretty` | history | Output format and destination, as for `scan` |
| `--config[=<file>]` | scan, top, duplicates, diff, snapshot, history, mcp | Use the `[scan]` section of `config.toml` (the default file, or `<file>`); `snapshot` and `history` also read `[snapshots]` |

`scan` filters work like the GUI's filtered tree: matching entries and the
ancestors needed to reach them are kept, while every folder's totals still
count everything below it. `top` filters choose which files are ranked. Ages
use `d` (days), `w` (weeks), `m` (30 days) and `y` (365 days); dates are local
and include the whole day.

Scan options apply to `diff` only when `<new>` is a folder; comparing two
saved scans does not scan anything.

**Using the app's scan settings.** By default the command line uses these
flags only and never reads `config.toml`, which keeps scripts reproducible.
Add `--config` to opt in: the `[scan]` section of
`%APPDATA%\FastDir\config.toml` (or `--config=<file>`; a bare `--config
<file>` also works when the file ends in `.toml`) becomes the base for the
scan. It covers excluded names, hidden/system skipping, Windows-folder
skipping (`skip_windows_dir`), MFT scanning (`use_mft`), alternate stream
counting (`count_streams`), junction following,
cross-volume and thread
count. Explicit flags win over it: `--threads`
replaces the configured count, `--exclude` adds to the configured exclusions,
and switches such as `--follow-junctions` turn things on. A missing or
invalid file is an error (exit code 2), never a silent fallback.
`fastdir-cli config [<file>]` prints the options the file gives, as JSON, for
debugging. Without `--config`, nothing changes.

**Output streams.** The result goes to stdout. stderr gets a live progress
line (only when it is a terminal) and a one-line summary, unless `--quiet`.
**Ctrl+C** stops the scan cleanly; nothing is written to stdout (with
`--format ndjson`, the lines written so far stay, without an `end` line). A second
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

`fastdir scan --format bin` does not write JSON: it writes a separate compact
binary document (a `.fdscan` file, magic `FDSCAN`, format version `2`;
version `1` files still load). It
carries the same facts as the JSON tree, keeps hard-link markers, and loads
much faster for very large scans. It is read back by `diff`, the GUI's *Open
scan…* / *Compare with…* and MCP `compare_scans`; use JSON when another
program has to read the document. The binary format is independent of the
`fastdir.scan` schema and its version number only moves on incompatible
changes.

`fastdir scan` (`schema: "fastdir.scan"`):

```jsonc
{
  "schema": "fastdir.scan", "version": 1, "fastdir_version": "0.2.0",
  "root": "C:\\Users",               // absolute path that was scanned
  "scanned_at": "2026-11-03T12:00:00Z",
  "elapsed_ms": 1234,
  "errors": 2,                        // folders that could not be read
  "unreadable": [                     // their paths (contents missing from the totals)
    "C:\\Users\\Default\\Application Data", "C:\\Users\\other"
  ],
  "tree": {                           // an entry, see below
    "name": "C:\\Users", "path": "C:\\Users", "type": "dir",
    "size": 123456789,                // logical size
    "disk_size": 130000000,           // allocated on disk
    "items": 4567, "files": 4000, "folders": 567,
    "modified": "2026-11-02T18:30:00Z",
    "error": false,                   // this folder could not be read
    "hard_link": false,               // extra hard link, bytes counted elsewhere
    "owner": "DOMAIN\\user",          // only with --owners; omitted otherwise
    "truncated": false,               // dirs only: children cut off by --max-depth
    "children": [ /* entries, largest first; dirs only */ ]
  }
}
```

`type` is `dir` or `file`. Automatic items use their own type: `free_space`,
`unknown` and `streams` (the `<Alternate streams>` item of a folder). Files
have the same fields without `truncated` and
`children` (`files` is `1` and `items` is `1` for a file).

`fastdir top` (`schema: "fastdir.top"`) has the same `schema`, `version`,
`fastdir_version`, `root`, `scanned_at`, `elapsed_ms` and `errors` fields,
plus `limit`, `total` (`size`, `disk_size`, `files`, `folders` of the whole
scan) and `files`: a list of `{ name, path, extension, size, disk_size,
modified, owner? }`, largest first. Hard links are listed once. `owner` is
only present with `--owners`; CSV gains an `owner` column in the same case.
With filter options, `files` contains only matching files while `total` still
describes the whole scan.

#### NDJSON streams

`--format ndjson` (also `jsonl`) writes newline-delimited JSON: one compact
JSON object per line, each with a `record` field saying what it is. A stream
is one `start` line, the data lines, then one `end` line. A stream without an
`end` line was cut short (Ctrl+C, exit code 130, or a write error). The
NDJSON streams have their own schema names and versions; the `fastdir.scan`
and `fastdir.top` documents above are unchanged.

`fastdir scan --format ndjson` (`schema: "fastdir.scan.ndjson"`) writes each
entry **while the scan runs**, as soon as it is final, so a script can start
on the first folders long before a drive scan ends and never has to load one
huge document. While entries keep arriving, the output is flushed about
every 100 ms. Closing the pipe
(`... | head`) stops the scan.

```jsonc
{"record": "start", "schema": "fastdir.scan.ndjson", "version": 1, "fastdir_version": "0.3.2",
 "root": "C:\Users", "scanned_at": "2026-11-03T12:00:00Z"}
{"record": "entry", "name": "notes.txt", "path": "C:\Users\me\notes.txt", "type": "file",
 "depth": 2, "size": 1200, "disk_size": 4096, "items": 1, "files": 1, "folders": 0,
 "modified": "2026-11-02T18:30:00Z", "error": false, "hard_link": false,
 "extension": ".txt"}                // files only; null when the name has none
{"record": "entry", "name": "me", "path": "C:\Users\me", "type": "dir", "depth": 1, ...}
{"record": "entry", "name": "C:\Users", "path": "C:\Users", "type": "dir", "depth": 0, ...}
{"record": "end", "elapsed_ms": 1234, "errors": 2,
 "unreadable": ["C:\Users\other"],  // as in fastdir.scan
 "entries": 4567,                    // entry lines written
 "total": {"size": 123456789, "disk_size": 130000000, "items": 4567, "files": 4000, "folders": 567}}
```

(Shown wrapped here; each record is one line.) An `entry` has the fields of a
`fastdir.scan` entry (including `owner` with `--owners`) without `children`
and `truncated`, plus `depth` (levels below `root`, which is `0`) and, for
files, `extension`. Its `path` is always complete, so a line can be used on
its own.

- **Order.** Entries come in the order the scan finishes them, which varies
  between runs because folders are scanned in parallel. A folder's line comes
  after every line below it (its totals are only known then), so the root is
  always the last entry.
- **Repeated folders.** On Windows, when an unreadable `WindowsApps` folder is
  recovered package by package (see the summary line), its packages are
  written after the scan and `WindowsApps` and each folder above it are
  written again with their larger totals. Keep the last line for a path.
- **Options.** `--max-depth` and `--no-files` decide which entries are
  written. Filters (`--name`, `--min-size`, `--ext`, `--newer-than`,
  `--older-than`) write only the matching entries, without the ancestors the
  JSON tree keeps (each line carries its full path instead).
- With `--mft` the tree is read in one go, so the entries are written right
  after it is read rather than during the scan.

`fastdir top --format ndjson` (`schema: "fastdir.top.ndjson"`) ranks after
the scan, then writes a `start` line (as above), one `file` line per file
(largest first, with the fields of a `fastdir.top` file), and an `end` line
with `elapsed_ms`, `errors`, `limit`, `count` (files written) and `total`.

`fastdir diff` (`schema: "fastdir.diff"`):

```jsonc
{
  "schema": "fastdir.diff", "version": 1, "fastdir_version": "0.2.0",
  "old": { "root": "C:\\Users", "scanned_at": "2026-11-01T09:00:00Z" },
  "new": { "root": "C:\\Users", "scanned_at": "2026-11-03T09:00:00Z" },
  "total": {
    "old_size": 123456789,           // scanned bytes (drive pseudo-items excluded)
    "new_size": 234567890,
    "delta": 111111101,              // new_size - old_size
    "added": 50000000,               // bytes in top-level added entries
    "removed": 3000000,              // bytes in top-level removed entries
    "added_items": 120, "removed_items": 8,
    "free_delta": -111111101         // free-space change, or null
  },
  "min_change": 0,                   // options echo, for scripts
  "limit": null,
  "entries": [                       // largest |delta| first, then by path
    {
      "path": "C:\\Users", "type": "dir", "status": "changed",
      "old_size": 123456789, "new_size": 234567890, "delta": 111111101,
      "old_disk_size": 130000000, "new_disk_size": 240000000
    }
  ]
}
```

`status` is `added`, `removed` or `changed` (grown and shrunk are both
`changed`; read the sign of `delta`). `type` is `dir` or `file`. The root row
is always included; its `delta` equals `total.delta`. An added or removed
folder is one row with its subtree totals. Removed rows use the baseline's
`root`. CSV uses the header
`path,type,status,old_size,new_size,delta,old_disk_size,new_disk_size`.

`fastdir duplicates` (`schema: "fastdir.duplicates"`) has the same scan
metadata (`schema`, `version`, `fastdir_version`, `root`, `scanned_at`,
`elapsed_ms`, `errors`) plus the filters that were applied and the groups:

```jsonc
{
  "schema": "fastdir.duplicates", "version": 1, "fastdir_version": "0.2.0",
  "root": "D:\\Media",
  "scanned_at": "2026-11-03T12:00:00Z", "elapsed_ms": 840,
  "errors": 0,
  "min_size": 1048576,               // the --min-size that was used
  "extensions": [".mp4"],            // empty when --ext was not given
  "limit": null,                     // --limit, or null for every group
  "candidates": 812,                 // files that passed the filters
  "total_groups": 12,                // every group found, before --limit
  "total_wasted": 3221225472,        // reclaimable bytes over every group
  "hashed_bytes": 4831838208,        // read while hashing (samples included)
  "hash_errors": 0,                  // files that could not be read
  "groups": [                        // largest waste first
    {
      "size": 536870912,             // bytes of one copy
      "hash": "5f2d…",               // BLAKE3 of the whole file (64 hex chars)
      "wasted": 536870912,           // size × (files - 1)
      "files": [
        { "path": "D:\\Media\\a.mp4", "modified": "2026-10-01T09:30:00Z" },
        { "path": "D:\\Backup\\a.mp4", "modified": "2026-09-02T18:12:00Z" }
      ]
    }
  ]
}
```

Files are grouped by size, then the first and last 64 KB are hashed, and only
the files that still match get a full BLAKE3 hash. Zero-byte files are
skipped, and extra hard links to an already-counted file are not duplicates.

With `--format csv`, `duplicates` writes one row per file:

```csv
group,wasted,path,size,hash,modified_iso
1,536870912,D:\Media\a.mp4,536870912,5f2d…,2026-10-01T09:30:00Z
1,536870912,D:\Backup\a.mp4,536870912,5f2d…,2026-09-02T18:12:00Z
```

`group` is 1-based; rows of the same group share `wasted` (the group's
reclaimable bytes) and `hash`.

`fastdir snapshot` (`schema: "fastdir.snapshot"`) reports the saved file:

```jsonc
{
  "schema": "fastdir.snapshot", "version": 1, "fastdir_version": "0.3.2",
  "root": "D:\\", "scanned_at": "2026-11-10T03:00:00Z",
  "elapsed_ms": 15400, "errors": 3,
  "file": "C:\\Users\\me\\AppData\\Roaming\\FastDir\\snapshots\\D-…\\20261110T030000Z.json",
  "snapshots_dir": "C:\\Users\\me\\AppData\\Roaming\\FastDir\\snapshots",
  "size": 512000000000, "disk_size": 515000000000,
  "files": 2700000, "folders": 310000,
  "snapshots": 30,                   // snapshots of this root after retention
  "pruned": ["…\\20261010T030000Z.json"]   // files removed by retention
}
```

`fastdir history` (`schema: "fastdir.history"`) lists `<path>` in every
snapshot that contains it, oldest first:

```jsonc
{
  "schema": "fastdir.history", "version": 1, "fastdir_version": "0.3.2",
  "path": "D:\\Projects", "snapshots_dir": "…",
  "change": 2147483648,              // last size - first size, or null
  "points": [
    {
      "scanned_at": "2026-11-09T03:00:00Z", "file": "…", "root": "D:\\",
      "found": true,                 // false when <path> was not in that snapshot
      "size": 10737418240, "disk_size": 10800000000,
      "files": 52000, "folders": 4100,
      "delta": null                  // change since the previous point that had it
    }
  ]
}
```

CSV uses the header
`scanned_at,size,disk_size,files,folders,delta,root,file`.

### MCP server

`fastdir-cli mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server over stdio, so agents such as Claude Code can query disk usage. Scans
stay in memory (the four most recent folders), so follow-up questions do not
rescan. A tool given a folder outside every cached scan scans it first.

| Tool | |
|---|---|
| `scan_path` | Scan (or rescan) a folder or drive; returns its totals and largest entries |
| `get_children` | Entries directly inside a folder, largest first |
| `largest_files` | Largest files anywhere under a folder, optionally filtered by name, minimum size, extension or modified date |
| `search` | Find entries by name substring or glob, minimum size, extension, modified date or type |
| `path_info` | One entry's sizes and counts, and its share of its parent and of the scan |
| `find_duplicates` | Groups of identical files under a folder and the space they waste; reuses the cached scan |
| `compare_scans` | What changed since a cached scan or a saved scan (JSON or `.fdscan`), as `fastdir.diff` |
| `export_scan` | Write a folder to a JSON (`fastdir.scan`), CSV or binary `.fdscan` file, or a standalone HTML report (`format: "html"`) |

`largest_files` and `search` take the same optional filter fields as the
command line: `query` (or `name`), `min_size`, `extension` (a string or a
list), `newer_than` and `older_than` (`2026-01-01` or an age such as `30d`).

`compare_scans` takes `old` (a scan root cached in this session, or a saved
scan file, JSON or `.fdscan`) and `new` (a folder scanned now, or a saved scan
file), plus optional `min_change`, `max_depth` and `limit`. It returns the same
`fastdir.diff` document as the CLI command.

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

Add `--config` (or `--config=<file>`) to `mcp` to apply the `[scan]` section of
`config.toml` to every scan the server runs, so the agent sees the same scan
as the app. Per-call `scan_path` arguments add to or override it. Without
`--config` the file is ignored.

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
| F8 | Move keyboard focus to the treemap (and back) |
| Tab / Shift+Tab | Cycle search box → list → treemap |
| ↑ ↓ ← → *(treemap focused)* | Select the neighbouring cell; the list follows |
| ← → / ↓ / ↑ *(sunburst focused)* | Move round the ring / step out into the largest child / step back in to the parent |
| Enter / Backspace *(treemap focused)* | Zoom into the selected folder / zoom out |
| Esc *(treemap focused)* | Give the keys back to the list |
| Ctrl+E | Show in Explorer |
| Ctrl+Shift+C | Copy the path |
| F2 | Rename |
| Ctrl+M | Move to… |
| Del | Move to Recycle Bin |
| Shift+Del | Delete permanently |
| Ctrl+B | Add the selection to the deletion basket (or take it out) |
| Ctrl+Shift+B | Open the deletion basket |
| F5 | Refresh the selected folder |
| Ctrl+R | Rescan everything |
| Ctrl+O | Scan a folder… |
| Ctrl+F | Focus the search box |
| Ctrl+Shift+F | Minimum size / extension / date filters and cleanup finders |
| Ctrl+1 … Ctrl+9 | Run the custom action bound to that number |
| Esc | Stop scanning, clear the filter, cancel a move or delete |
| Ctrl+T | Toggle light / dark |
| Ctrl+, | Open settings |

### Accessibility

- Everything is reachable by keyboard, including the treemap: press **F8**
  (or click a cell) and the arrow keys walk between cells, with an accent
  focus ring around the selected cell and the panel. The list selection
  stays in sync. The sunburst view works the same way: left and right move
  round a ring, down steps out a level and up steps back in.
- **Tab / Shift+Tab** move between the search box, the list and the
  treemap. Dialogs trap focus, close with Esc and give focus back to where
  it was.
- **Screen readers** (NVDA, Narrator, VoiceOver, Orca) work through GPUI's
  AccessKit integration. Toolbar buttons have names, list rows expose name,
  size and share plus their level / expanded / selected state, the status
  bar is a status region, and a live region announces "scan started", the
  scan summary and each selected item.
- **Contrast:** text and button colours in both themes meet WCAG AA
  (4.5:1); this is enforced by a unit test. The treemap highlight, age and
  change colour modes always show a text legend, and selected rows use a
  marker bar as well as a tint.
- **OS settings:** a Windows high-contrast theme replaces the app palette,
  "Show animations" off disables animations, and "Make text bigger"
  multiplies the interface scale (Settings → Appearance goes up to 200 %).

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
| `crates/fastdir-treemap` | Squarified cushion layout, sunburst layout, hit-testing and CPU rendering |
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
cargo bench -p fastdir-treemap    # squarify layout for 1k / 10k / 100k nodes, CPU render, sunburst render / hit test
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
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=0.3.2 installer\fastdir.iss
```

The installer is written to `dist\`.

### Building the Linux packages locally

`packaging/linux/package.sh` builds the `.deb`, `.rpm` and AppImage from the
release binaries; see [`packaging/README.md`](packaging/README.md) for the
tools it needs and how the release workflow uses it.

## License

FastDir is licensed under the [GNU General Public License v3.0 or later](LICENSE).
