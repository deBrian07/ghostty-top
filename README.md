# ghostty-top

A small, htop-style macOS monitor for CPU and resident memory used by each
Ghostty terminal surface.

<img width="872" height="380" alt="image" src="https://github.com/user-attachments/assets/59604e3c-ed13-41d9-891b-bf0ddfcac675" />


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

| Key | Action |
| --- | --- |
| `↑` / `↓` or `j` / `k` | Select terminal |
| Home / End | Jump to the first or last terminal |
| `e`, space, or enter | Show/hide processes in the selected terminal |
| `c` | Sort by CPU (press again to reverse) |
| `m` | Sort by memory (press again to reverse) |
| `t` | Sort by terminal ID |
| `r` | Reverse current sort |
| `q` or Ctrl-C | Quit |

Use `ghostty-top --once` for script-friendly output, or change the one-second
refresh with `ghostty-top --interval 0.5`.

## Measurement notes

- CPU is the sum of macOS `ps` CPU percentages for the process tree. It can
  exceed 100% on a multi-core machine.
- RAM is summed RSS (resident set size). Processes can share physical pages, so
  this is an attribution metric rather than a perfect measure of unique memory.
- A process that daemonizes and gets re-parented can no longer be attributed to
  the terminal that launched it.
