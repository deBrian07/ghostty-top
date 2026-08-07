# ghostty-top

Shows how much CPU and memory each Ghostty tab is using, and how much time you
spend in each one.

<img width="990" height="531" alt="image" src="https://github.com/user-attachments/assets/891eb8db-dd48-4bf3-8df8-13ce1d583e77" />

## Install

```sh
bun install -g ghostty-top
```

Or `npm install -g ghostty-top`. Or build it yourself with
`cargo install --path .`.

Nothing else to install and nothing to configure.

**Linux:** build from source with `cargo install --path .`. The monitor works
the same, but the usage calendar doesn't: Ghostty only reports which tab you're
looking at on macOS, so there's no way to measure per-tab time elsewhere. Rows
are named by folder instead of tab name for the same reason. The npm package
ships a Mac binary only.

## Run it

```sh
ghostty-top          # the live monitor
ghostty-top --once   # print once and exit
```

Each row is one Ghostty tab. Ghostty draws all tabs in a single process, so the
memory that process uses is shown on its own line at the top.

You can use the mouse for everything. Click a column title to sort by it, click
it again to flip the order, click a row to select it, and click it once more to
see the processes inside. The controls at the bottom are buttons too.

Keys, if you prefer them:

| Key | What it does |
| --- | --- |
| `↑` `↓` | Move between tabs |
| `enter` | Show the processes in a tab |
| `c` `m` `g` `p` `a` `n` `t` | Sort by CPU, memory, trend, processes, age, activity, tab |
| `u` | Open the usage calendar |
| `q` | Quit |

## Usage calendar

Press `u`. There are three ways to look at it, with `d`, `w`, and `c`:

- **daily** – one square per day. Brighter means more time.
- **weekly** – one bar per week.
- **cumulative** – the total adding up over time.

Point at any square or bar to see its total. Click it to see which tabs the
time went to.

This part is macOS only. Time only counts when Ghostty is the app you're using
and you're actually at your Mac. Nothing is counted when the lid is closed, when the screen has been
idle, or while the Mac is asleep.

To keep counting when the monitor isn't open:

```sh
ghostty-top --track           # in this window
ghostty-top --install-tracker # in the background, starting at login
```

Run `--install-tracker` again after updating ghostty-top, so the background
copy is updated too. `--uninstall-tracker` stops it and keeps your history.

## What gets saved

Everything is saved in `~/Library/Application Support/ghostty-top/` and stays
on your Mac. It saves tab names, folder paths, program names, and CPU and
memory readings. It does not save anything you type or anything printed in your
terminal, and it never saves command arguments or environment variables,
because those can contain passwords and keys.

The first time it reads tab names, macOS asks you to allow it to control
Ghostty. That permission is only needed once.

## Good to know

- CPU adds up every process in a tab, so it can go over 100% on a Mac with
  several cores.
- Memory adds up each process, and programs often share memory, so treat the
  number as a rough guide.
- A **leak warning** means a tab's memory has been climbing steadily for a few
  minutes. It's worth a look, but it isn't proof of a problem.

Run `ghostty-top --help` to see every option.

## Contributing

Bug reports and pull requests are welcome. See
[CONTRIBUTING.md](CONTRIBUTING.md) for how to get set up and what to check
before opening one.

## License

[MIT](LICENSE)
