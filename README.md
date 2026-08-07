# ghostty-top

A per-terminal CPU and memory monitor for Ghostty on macOS, with a calendar of
where your terminal time actually went.

<img width="990" height="531" alt="image" src="https://github.com/user-attachments/assets/891eb8db-dd48-4bf3-8df8-13ce1d583e77" />

## Install

```sh
bun install -g ghostty-top
```

Also `npm install -g ghostty-top`, or `cargo install --path .` from a clone.
macOS only. No dependencies, no configuration.

## Use

```sh
ghostty-top          # interactive monitor
ghostty-top --once   # one snapshot, for scripts
```

Rows are named after their Ghostty tab, falling back to the working directory
when two tabs share one. Ghostty draws every tab in a single process, so its
own CPU and memory are listed separately from the terminals.

Everything is clickable: a column heading sorts (click again to reverse), a row
selects, the selected row expands, and the controls along the bottom do the
rest. Or use the keyboard:

| Key | |
| --- | --- |
| `↑` `↓` | Select a terminal |
| `enter` | Show its processes |
| `c` `m` `g` `p` `a` `n` `t` | Sort by CPU, memory, trend, processes, age, activity, tab |
| `u` | Usage calendar |
| `q` | Quit |

## Usage calendar

Press `u`. Three views, switched with `d`, `w`, and `c`:

- **daily** — a square per day, brighter for more focused time
- **weekly** — a bar per week
- **cumulative** — the running total across the range

Hover any square or column for its total; click it for the per-tab breakdown.

Time is counted only while Ghostty is frontmost **and** you are at the machine.
Nothing is recorded while the lid is shut, while the screen has been idle, or
across a sleep.

To keep recording without the monitor open:

```sh
ghostty-top --track            # in this terminal
ghostty-top --install-tracker  # at login, in the background
```

`--uninstall-tracker` removes the service and keeps the history.

## Your data

History is written to `~/Library/Application Support/ghostty-top/`, is never
pruned, and never leaves your Mac. It records tab names, working directories,
process names, and resource samples — never terminal contents, keystrokes,
environment variables, or command arguments, which may hold secrets.

Reading tab names needs the one-time macOS Automation permission for Ghostty.

## Notes

- CPU is summed across each process tree, so it can exceed 100% on many cores.
- RAM is summed RSS. Processes share pages, so read it as attribution rather
  than exact unique memory.
- A **leak alert** marks a terminal whose memory has climbed steadily for
  several minutes against a strong linear fit. It is a hint worth a look, not
  proof of a leak.

`ghostty-top --help` lists every flag.

## License

[MIT](LICENSE)
