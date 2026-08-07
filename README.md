# ghostty-top

A small, htop-style macOS monitor for CPU and resident memory used by each
Ghostty terminal surface.

<img width="990" height="531" alt="image" src="https://github.com/user-attachments/assets/891eb8db-dd48-4bf3-8df8-13ce1d583e77" />

Ghostty renders all tabs and split panes inside one shared app process. macOS
does not expose a supported way to divide that renderer memory per tab, so the
display intentionally separates:

- **Ghostty shared**: the app/renderer's CPU and RAM.
- **Terminals**: each terminal's login shell and every descendant process.

Each row is named after its Ghostty tab. Ghostty's scripting dictionary exposes
no tty, so a tab is matched to a terminal by working directory; when two tabs
sit in the same directory nothing tells them apart, and the row falls back to
that directory and then to the tty (`ttys002`). Process data cannot distinguish
a tab from a split pane, so both appear as terminals.

## Build and run

```sh
cargo build --release
./target/release/ghostty-top
```

To install it somewhere already on your shell path:

```sh
cargo install --path .
ghostty-top
```

No runtime dependencies or elevated permissions are required.

## Controls

Mouse controls work directly in Ghostty: click a terminal to select it, click
the selected terminal again to expand its processes, use the scroll wheel to
move through the list, or click a column heading to sort by it. The active sort
column is highlighted, and the hint line along the bottom is clickable too.

| Key | Action |
| --- | --- |
| `↑` / `↓` or `j` / `k` | Select terminal |
| Home / End | Jump to the first or last terminal |
| `e`, space, or enter | Show/hide processes in the selected terminal |
| `c` | Sort by CPU (press again to reverse) |
| `m` | Sort by memory (press again to reverse) |
| `g` | Sort by memory growth trend |
| `p` | Sort by process count |
| `a` | Sort by terminal age |
| `n` | Sort by activity name |
| `t` | Sort by tab name |
| `r` | Reverse current sort |
| `u` | Toggle the usage calendar |
| `d` / `w` / `c` | Calendar shading: daily, weekly, or cumulative |
| `q` or Ctrl-C | Quit |

Use `ghostty-top --once` for script-friendly output, or change the one-second
refresh with `ghostty-top --interval 0.5`.

## Focused tab usage tracking

The interactive monitor records time only when Ghostty is the frontmost app and
a tab remains selected across consecutive five-second samples. Time is not
counted while the lid is shut, while the screen has been idle for five minutes,
or across a sleep: a gap is credited only when both the monotonic and wall
clocks agree it was continuous, so whichever clock the system froze, the gap is
discarded. Ghostty 1.3 or newer and the one-time macOS Automation permission
are required. History is stored outside the application and repository at:

```text
~/Library/Application Support/ghostty-top/
```

To collect usage without leaving the full monitor open, run:

```sh
ghostty-top --track
```

For automatic tracking at login, install the included LaunchAgent:

```sh
ghostty-top --install-tracker
ghostty-top --tracker-status
```

The service starts immediately, launches again at login, and is restarted by
macOS if it exits. Re-run `--install-tracker` after replacing the binary to
upgrade the background copy. To remove only the service and its installed
binary:

```sh
ghostty-top --uninstall-tracker
```

Installing, upgrading, or uninstalling the service never removes this directory
or its historical files.

The tracker maintains five complementary datasets:

- `usage.tsv`: daily focused time per stable Ghostty tab ID.
- `tab-history-v1.tsv`: append-only snapshots of every window, tab, and split,
  including IDs, titles, indexes, focus/selection, and working directories.
- `resources-v1.tsv`: periodic Ghostty and terminal CPU, RAM, process, age,
  activity, memory-growth, and leak-alert summaries.
- `processes-v1.tsv`: joinable per-process snapshots with terminal ownership,
  PID/parent PID, CPU, RAM, age, derived start time, executable name, and path.
- `tracker-events-v1.tsv`: collector starts, schema and app versions, process ID,
  OS/architecture, and the sampling intervals used for that session.

Tab snapshots are recorded when state changes and at a 15-minute heartbeat;
resource summaries are recorded every five minutes. These files are never
automatically pruned or replaced during an update or reboot. New schemas use new
versioned files so older history remains readable.

Together these preserve stable UI identity, names and working directories,
focus/selection state, daily duration, process-tree identity, resource trends,
and the collection context needed to interpret old samples. The normalized
timestamps and IDs let later features join datasets without rewriting history.

The tracker does not capture terminal contents, keystrokes, environment
variables, or full command arguments, which may contain secrets. It retains the
executable name and path so workloads remain classifiable without storing
tokens or passwords. It does not send usage data anywhere.

Press `u` to open the usage calendar. Every square is one day, and brighter
squares represent more focused Ghostty time; the grid fits as many weeks as the
window allows, up to a full year. Hover a square for its total, or click it for
that day's per-tab breakdown. `d`, `w`, and `c` switch shading between the day's
own total, its week's total, and a running total across the range. Press `u` to
return to live process usage.

To populate most of the previous 55 days with deterministic, varied calendar
test data, run `ghostty-top --seed-demo-history`. Demo rows use reserved
`demo:` tab IDs, so they can be removed without changing real usage:

```sh
ghostty-top --clear-demo-history
```

Stop or reinstall the login tracker around either operation so its in-memory
copy cannot overwrite the edited usage file.

## Measurement notes

- CPU is the sum of macOS `ps` CPU percentages for the process tree. It can
  exceed 100% on a multi-core machine.
- RAM is summed RSS (resident set size). Processes can share physical pages, so
  this is an attribution metric rather than a perfect measure of unique memory.
- A process that daemonizes and gets re-parented can no longer be attributed to
  the terminal that launched it.

## Potential memory leak alerts

`ghostty-top` keeps a 15-minute rolling RAM history for each terminal. A row is
marked only after at least eight samples spanning three minutes show all of the
following: at least 48 MiB and 10% growth, a regression slope of at least
4 MiB/minute, a strong linear fit, mostly upward samples, and continued growth
in the recent half of the window. Three consecutive qualifying windows are
required before an alert appears, and ten clear windows are required to remove
an active alert.

Starting or stopping a process resets that terminal's evidence so a compiler,
server, or other newly launched workload is not mistaken for growth in an
existing process. Large allocations that settle into a stable plateau are also
rejected. The alert remains a diagnostic hint—not proof of a leak—and restarting
`ghostty-top` resets its in-memory detector state.
