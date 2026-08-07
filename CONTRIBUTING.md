# Contributing

Thanks for taking a look. Bug reports and pull requests are welcome.

## Before you start

Open an issue first for anything beyond a small fix. It saves you writing code
that doesn't get merged, and it's the fastest way to find out whether an idea
fits.

## Two rules that shape everything

These are not negotiable, so check your change against them first:

1. **No dependencies.** `Cargo.lock` contains exactly one package: this one.
   The whole tool is the Rust standard library plus command line programs that
   already ship with macOS. A pull request that adds a crate will be turned
   down, however useful the crate is.
2. **Nothing to set up.** Install it, run it, done. No config file, no flags
   you have to pass, no setup steps.

The tool is macOS only, because it reads Ghostty and macOS specific things.

## Getting set up

You need Rust. Nothing else.

```sh
git clone https://github.com/deBrian07/ghostty-top
cd ghostty-top
cargo run
```

## Before you open a pull request

Run these four. CI runs the same ones, so if they pass locally the build
should be green:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --once
```

Warnings are treated as errors, so a clippy warning will fail the build.

## Writing the change

All the code is in `src/main.rs`. It's one file on purpose.

`AGENTS.md` is the most useful thing to read before changing anything. It
explains the parts that look odd until you know why they are that way — how
mouse targets are worked out, why the slow lookups run on their own threads,
why a tab sometimes shows a folder path instead of its name. Getting one of
those wrong is the easiest way to break something quietly.

A few habits that keep the code consistent:

- Comments should say *why*, not what. Most lines don't need one. The ones
  worth writing explain a macOS quirk or a rule that looks arbitrary.
- Use whole words in names (`descending`, not `desc`).
- Name tests as sentences, like `sleep_and_stalls_are_not_counted_as_focused_time`.
- Keep logic in small functions that can be tested without a terminal or a
  running Ghostty. Anything that draws should take the terminal size as an
  argument rather than looking it up.

## Testing your change

Add a test for anything with logic in it. The existing tests in `src/main.rs`
are a good guide.

Two of them are worth knowing about, because they catch a lot: one draws every
screen at every terminal size to make sure nothing crashes or spills past the
edge, and one checks that every clickable thing sits where it's drawn. If you
add a screen or a button, they will cover it automatically.

Some things can't be tested automatically, so check them by hand and say in the
pull request that you did:

- Resize the window while it's running, including very narrow.
- Switch between the monitor and the calendar, and through all three calendar
  views.
- Click everything you changed.

## Commits and pull requests

Keep commit messages short and direct, like `feat: add touch interaction` or
`fix: correct the memory total`. If the change needs explaining, put that in
the body.

In the pull request, say what it does and why. If it changes something you can
see, a screenshot helps a lot.

## Reporting bugs

Open an issue with the version (`ghostty-top --version`), your macOS version,
whether your Mac is Apple Silicon or Intel, and what you expected to happen.

If it looks like a security problem, don't open an issue. See
[SECURITY.md](SECURITY.md).
