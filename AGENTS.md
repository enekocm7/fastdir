# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository.

## What this is

FastDir is a fast disk usage analyzer for Windows (WinDirStat / WizTree
style), written in Rust (edition 2024) with GPUI Kit. It also ships a
headless CLI (`fastdir-cli`) with JSON/CSV output and an MCP server. See
[README.md](README.md) for the user-facing feature list.

## Workspace layout

| Crate | Contents |
|---|---|
| `fastdir` (root) | `src/main.rs` (GUI, or the CLI for subcommands), `build.rs` (icon / resource embedding) |
| `crates/fastdir-core` | Tree model, parallel scanner, drives, file-system ops, formatting, categories |
| `crates/fastdir-treemap` | Squarified cushion layout, hit-testing and CPU rendering |
| `crates/fastdir-updater` | Release check, verified download and self-update |
| `crates/fastdir-settings` | Persisted settings (`%APPDATA%\FastDir\config.toml`) |
| `crates/fastdir-locale` | UI string tables (16 languages) and locale-aware number/date formatting |
| `crates/fastdir-ui` | GPUI window, list/treemap panels, dialogs and menus |
| `crates/fastdir-cli` | Headless CLI, agent JSON schema and MCP server |

Dependency direction: `ui -> treemap -> core`, `ui -> updater`,
`ui -> settings`, `cli -> core`. **Only `fastdir-ui` may depend on GPUI**;
keep everything else GUI-free so it builds and tests quickly. Put reusable
logic in `fastdir-core` (or the relevant sub-crate), not in the UI.

## Building, running and testing: use the cargo aliases

`.cargo/config.toml` defines aliases that enable nightly-only compiler
speedups (`.cargo/nightly.toml`: parallel frontend and shared generics).
**Always use them instead of the plain cargo commands:**

| Alias | Instead of | Example |
|---|---|---|
| `cargo bn` | `cargo build` | `cargo bn`, `cargo bn --release` |
| `cargo rn` | `cargo run` | `cargo rn`, `cargo rn -p fastdir-cli -- scan C:\Users --max-depth 1` |
| `cargo cn` | `cargo check` | `cargo cn --workspace --all-targets` |
| `cargo tn` | `cargo test` | see below |

Why: switching between the aliases and plain `cargo build`/`test` changes
the rustflags and invalidates the whole build cache, so mixing them makes
every build a full rebuild. Pick the aliases and stick to them. They require
a nightly toolchain (the default here); if the active toolchain is stable,
prefix with `+nightly` (`cargo +nightly tn ...`). CI builds on stable with
plain `cargo`, so do not rely on nightly-only language features.

Run commands from the repo root (the alias config paths are relative).

Common commands:

```sh
# GUI-free test suite (fast, no GPUI)
cargo tn -p fastdir-core -p fastdir-treemap -p fastdir-updater -p fastdir-settings -p fastdir-cli -p fastdir-locale

# Type-check everything, including the GUI, benches and examples
cargo cn --workspace --all-targets

# Run the app
cargo rn --release
```

Before opening a PR, at minimum the test suite above and
`cargo cn --workspace --all-targets` must pass. Also run `cargo fmt --all`
and `cargo clippy --workspace --all-targets --config .cargo/nightly.toml`
and fix new warnings in code you touched.

## Benchmarks: required for anything that touches scanning or performance

Criterion benchmarks live in `crates/fastdir-core/benches/scanner.rs`
(scan default / exclusions / 1 vs all threads, arena build, sorting) and
`crates/fastdir-treemap/benches/treemap.rs` (squarify layout for 1k / 10k /
100k nodes, CPU render).

**You must run the benchmarks** when a change:

- touches the scanner or anything on the scan path in `fastdir-core`
  (directory listing, the thread pool, tree / arena building, hard-link
  handling, exclusions, sorting, size accounting, extension / category
  stats), **or**
- could otherwise affect performance (hot loops, allocations, data
  structures, dependencies such as `rayon` / `parking_lot` / `smallvec`,
  build profiles), **or**
- touches the treemap layout or renderer (then run the treemap benches too).

Compare against `main` using a saved baseline:

```sh
git checkout main
cargo bench -p fastdir-core -- --save-baseline main
git checkout <your-branch>
cargo bench -p fastdir-core -- --baseline main
# and, for treemap changes:
cargo bench -p fastdir-treemap -- --baseline main   # after saving a main baseline the same way
```

(`cargo bench` uses the bench profile, so it does not share the dev cache
with the aliases; running it plainly is fine.)

The scan benches use a synthetic tree generated once in
`target/fastdir-bench-tree`; these are warm-cache numbers, so run on an
otherwise idle machine and rerun anything that looks noisy.

**Document the results in the PR description**, in a `## Benchmarks`
section: the benchmarks you ran, the before/after numbers (or criterion's
change %), and the machine/OS. **Any regression must be called out
explicitly**, even a small one: say which benchmark regressed, by how much,
and why it is acceptable (or what the trade-off buys). Never hide or omit a
regression.

## Pull requests

- **Always say which issue the PR closes** using a GitHub closing keyword in
  the description, e.g. `Closes #66` (one line per issue). If there is no
  issue, say so explicitly.
- Title: short imperative summary followed by the issue number, e.g.
  `Add file-type categories and category breakdown (#66)`.
- Description sections: `## What changed` (bullets), `## Tests` (the exact
  commands you ran), and `## Benchmarks` when required (see above).
- Branch off `main`; never commit directly to `main`.

## Code conventions

- **User-facing strings** go through `fastdir-locale` (`t` / `fmt`), never
  hard-coded in the UI. Add new keys to `en.rs` and translate them in **all**
  16 languages; the `every_language_covers_every_key` test enforces this.
  Numbers and dates use the locale helpers (`format_count`, `format_date`,
  `format_relative`, ...).
- **Settings** live in `fastdir-settings`; new fields need a sensible
  default and must keep older `config.toml` files loading (migrations are
  automatic, don't break them).
- **CLI JSON schema** (`fastdir.scan` / `fastdir.top`, `version: 1`): fields
  may be added but never renamed or removed. Update the README's schema
  section when you add one.
- Windows is the primary target, but `fastdir-core` and the benches must
  keep building on Linux and macOS (`cfg(windows)` / `cfg(unix)` paths).
- File operations must update the tree in place (no full rescan).
- Update `README.md` when you add or change a user-visible feature, a CLI
  flag, a setting or a keyboard shortcut.
- Add tests for new logic in the GUI-free crates.
