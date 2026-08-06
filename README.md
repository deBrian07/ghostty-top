# ghostty-top

A small, htop-style macOS monitor for CPU and resident memory used by each
Ghostty terminal surface.

<img width="990" height="531" alt="image" src="https://github.com/user-attachments/assets/891eb8db-dd48-4bf3-8df8-13ce1d583e77" />

Ghostty renders all tabs and split panes inside one shared app process. macOS
does not expose a supported way to divide that renderer memory per tab, so the
display intentionally separates:

- **Ghostty shared**: the app/renderer's CPU and RAM.
- **Terminals**: each terminal's login shell and every descendant process.

The terminal identifier (`ttys002`, for example) is stable for that surface's
lifetime. Process data cannot distinguish a Ghostty tab from a split pane, so
both appear as terminals.

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
move through the list, or click the column headings and footer actions.
The active sort column is highlighted and shows its direction.

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
| `t` | Sort by terminal ID |
| `r` | Reverse current sort |
| `q` or Ctrl-C | Quit |

Use `ghostty-top --once` for script-friendly output, or change the one-second
refresh with `ghostty-top --interval 0.5`.

## Focused tab usage tracking

The interactive monitor automatically records time only when Ghostty is the
frontmost app and a tab remains selected across consecutive five-second
samples. Ghostty 1.3 or newer and the one-time macOS Automation permission are
required. Usage is stored at:

```text
~/Library/Application Support/ghostty-top/usage.tsv
```

To collect usage without leaving the full monitor open, run:

```sh
ghostty-top --track
```

The tracker flushes its small local data file every 30 seconds. It does not send
usage data anywhere.

## Measurement notes

- CPU is the sum of macOS `ps` CPU percentages for the process tree. It can
  exceed 100% on a multi-core machine.
- RAM is summed RSS (resident set size). Processes can share physical pages, so
  this is an attribution metric rather than a perfect measure of unique memory.
- A process that daemonizes and gets re-parented can no longer be attributed to
  the terminal that launched it.

## Potential memory leak alerts

`ghostty-top` keeps a five-minute rolling RAM history for each terminal. A row
is marked as a potential leak only after at least six samples over 30 seconds,
when its recent average has grown by both 32 MiB and 15% over its baseline.
This deliberately avoids warning on a single allocation spike. The alert is a
diagnostic hint—not proof of a leak—because caches and legitimate workloads can
also grow steadily. Restarting `ghostty-top` resets the history.
