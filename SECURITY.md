# Security

## Reporting a problem

Please don't open a public issue for a security problem.

Report it at
[Security → Report a vulnerability](https://github.com/deBrian07/ghostty-top/security/advisories/new),
which is private. Expect a reply within a day.

## What this tool can see

Worth knowing when judging whether something is a security problem:

- It reads the process list with `ps` and `lsof`, and asks Ghostty for its tab
  names and folder paths through macOS Automation. It reads only; it never
  changes Ghostty or any other program.
- It saves history to `~/Library/Application Support/ghostty-top/`. That stays
  on your Mac. Nothing is ever uploaded.
- It deliberately does not save what you type, what your terminal prints,
  environment variables, or command arguments, because those often contain
  passwords and API keys. It keeps program names and paths only.

A change that starts recording any of those, or that sends anything off the
machine, is a security problem, not a feature.
