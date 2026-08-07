# AGENTS.md

Guidance for agents working in this repository. See `README.md` for what the
tool does from a user's point of view.

## What this is

A per-terminal CPU and memory monitor for Ghostty on macOS, plus a focused-tab
time tracker. One binary, one file: `src/main.rs`.

## Hard constraints

These are the rules that shape most decisions here. Check a change against them
before writing code.

- **Zero dependencies.** `Cargo.lock` contains exactly one package: this one.
  Everything is the standard library plus command-line tools that ship with
  macOS (`ps`, `lsof`, `ioreg`, `osascript`, `stty`, `launchctl`, `pgrep`).
  Do not add a crate. If something seems to need one, it almost certainly does
  not — the TUI, the ANSI handling, the mouse decoding, and the civil-date maths
  are all hand-rolled here already.
- **Setup stays trivial.** Build and run; no config file, no flags required, no
  setup steps. The intent is for this to be installable with a single command
  (a Bun-installable package is the eventual goal), so anything that adds an
  install step works against the point of the tool.
- **Never modify Ghostty.** Ghostty is observed, never patched or configured.
  Everything comes from read-only queries: its AppleScript dictionary at
  `/Applications/Ghostty.app/Contents/Resources/Ghostty.sdef`, plus `ps`/`lsof`.
- **macOS only.** `main` exits early on other platforms.
- **Data is append-only and versioned.** Files under
  `~/Library/Application Support/ghostty-top/` are never pruned or rewritten.
  A schema change means a new `-v2` file, not an edited `-v1` one.

## Commands

```sh
cargo build              # or --release
cargo test               # 32 unit tests, all in the tests module in main.rs
cargo clippy --all-targets   # expected to be silent
cargo fmt
```

Run it without touching your real history by pointing `HOME` at a scratch
directory — every data path is derived from `HOME`:

```sh
HOME=/tmp/scratch ./target/debug/ghostty-top --seed-demo-history
HOME=/tmp/scratch ./target/debug/ghostty-top --track
```

`--once` prints a single snapshot and needs no terminal, which makes it the
quickest way to check the monitor. The interactive TUI needs a tty; drive it
under `script` when you need to see a real frame:

```sh
(printf 'u'; sleep 3; printf 'q') | script -q /dev/null ./target/debug/ghostty-top
```

## Releasing

The npm package ships one **universal** binary and `bin` points straight at it.
There is deliberately no launcher script: Bun does not run install scripts by
default, and a Bun-only user may have no Node to run a JS shim with. Keep it
that way — a shim would break `bun install -g` for exactly the audience this
tool is for.

```sh
rustup target add x86_64-apple-darwin   # once, for Intel support
./scripts/build-binaries.sh             # lipo both slices into binaries/
npm publish                             # version comes from package.json
```

`scripts/build-binaries.sh` warns and keeps going when a target is missing, so
check its `lipo -info` output says `arm64 x86_64` before publishing. Keep
`version` in `package.json` and `Cargo.toml` in step.

## Layout of `src/main.rs`

Roughly in file order: constants and types, `App` (state and input handling),
`UsageStore`/`UsageTracker` (persistence and sampling), the macOS query helpers,
`main` and the event loop, process aggregation, leak analysis, the two render
functions, small formatters, then `mod tests`.

Two views share one `App`: `View::Monitor` and `View::Usage`.

## Things that are easy to get wrong

**Click targets must be computed, not hardcoded.** Rendering returns a `Layout`
holding `ClickZone`s built while the line is assembled, so hit boxes and drawn
text cannot drift apart. Earlier versions hardcoded column ranges and they
silently went stale. When adding a clickable thing, emit a zone; do not write a
column number. `hint_line`, `scale_selector`, and `monitor_header` all show the
pattern, and tests assert the zone spans match the rendered text.

**Column widths are shared between heading and rows.** `monitor_flex_widths`
is the single source; the header and the data rows both read it. A heading wider
than its field pushes the whole row out of step — this is why the sort arrow was
removed from headings.

**Count visible columns, not bytes.** Rendered lines carry ANSI escapes and
multi-byte glyphs (`·`, `↑↓`, `■`). Track column positions with `chars().count()`
and strip escapes before measuring. `strip_ansi` in the tests exists for this.

**Nothing slow may run on the UI thread.** The event loop polls input every
50 ms. AppleScript is slow and variable: the focused-tab query takes ~150 ms and
the full inventory over 1.5 s, scaling with tab count. Both run on worker
threads and report back through `mpsc`; `LabelCache` and `poll_inventory` show
the shape. Give each `osascript` call a timeout matched to its real cost — a
too-tight timeout fails silently and looks like missing data.

**Ghostty is matched by executable path.** `pgrep -x ghostty` does *not* match
the bundled binary on macOS, because `-x` compares against the full path. Use
`is_ghostty_app`. This bug disabled every AppleScript query for a while, and the
only symptom was empty history.

**Tabs are joined to ttys by working directory.** Nothing else links them: the
scripting dictionary exposes no tty or pid, and `ps` knows nothing about tabs.
The join is therefore ambiguous whenever two tabs share a directory, and
`resolve_tab_labels` deliberately refuses to name a row unless exactly one
surface owns that directory. Showing the directory is correct; guessing a name
puts the wrong label on a row. Do not "improve" this into a heuristic.

**Tracking correctness beats tracking coverage.** Time is credited only when
Ghostty is frontmost, the user is present (`away_reason`), and both the
monotonic and wall clocks agree the gap was continuous (`credited_time`). Under-
counting is acceptable; inventing time the user did not spend is not.

## Conventions

- Comments explain *why*, and are worth writing only where the reason is not
  evident from the code — a non-obvious macOS behaviour, or a rule that looks
  arbitrary until you know what it prevents. Do not narrate what the code does.
- Prefer whole words in names (`descending`, not `desc`), matching what is there.
- Tests are behavioural and named as sentences, e.g.
  `sleep_and_stalls_are_not_counted_as_focused_time`. Keep the pure logic in
  free functions so it can be tested without a terminal or a running Ghostty.
- Commit messages: short and direct, e.g. `feat: add touch interaction`. Do not
  add a co-author trailer.
