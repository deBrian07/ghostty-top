All 16 populated entries in the [Notion fix database](https://app.notion.com/p/3bd1c29536ab80768397d942205ed395) are addressed on `fix/notion-fixes`. The seventeenth row is blank. This branch includes the existing prepack, focus-timeout and usage-merge commits; the table records how each item was verified or completed.

| Order | Notion item | Result | Main commit |
| --- | --- | --- | --- |
| 1 | [Leak growth window](https://app.notion.com/3bd1c29536ab81a28a2ff5ffe0445bc0) | Warning names the actual rolling span and averaged edges. | `d3d602f` |
| 2 | [Memory field width](https://app.notion.com/3bd1c29536ab81089881eeccb1f9d96d) | Promote units before the rounded value reaches 1000; figures and fallback markers fit nine columns, including TiB and larger units. | `41cad9e` |
| 3 | [Prepack builds the binary](https://app.notion.com/3bd1c29536ab81949e1aea65055bc501) | Existing prepack hook retained and verified with a real universal build and npm pack dry run. | `9842fb6` |
| 3.1 | [Matching release versions](https://app.notion.com/3bd1c29536ab81cb8ad8f4fd8c991791) | Build fails before compilation when package and Cargo versions differ. | `798a5df` |
| 3.2 | [Require both Apple targets](https://app.notion.com/3bd1c29536ab8124953eee950daeda90) | Missing targets fail before compilation; real lipo checks verify each final architecture. | `798a5df`, `799e80e` |
| 4 | [lsof timeout](https://app.notion.com/3bd1c29536ab81d98793edb69bb9e91e) | Three-second deadline, concurrent output draining and cancellation of the owned process group. | `1bdafcf` |
| 5 | [Bound escape decoding](https://app.notion.com/3bd1c29536ab81beb575e10b116b1458) | Incomplete sequences expire after 100 ms; malformed mouse reports have bounded storage and preserve subsequent keys. | `1bdafcf` |
| 5.1 | [Restore terminal after abnormal exit](https://app.notion.com/3bd1c29536ab81cc9900ea25e905ca01) | Panic hook restores before abort; SIGTERM, SIGHUP and SIGINT request normal cleanup. | `1bdafcf` |
| 6 | [Measure terminal columns](https://app.notion.com/3bd1c29536ab819280aaca22024cd3c8) | CJK, combining marks and common joined emoji use display columns for clipping, padding and click geometry. | `479cfec`, `bad4202` |
| 7 | [Stop fetching argv on the UI thread](https://app.notion.com/3bd1c29536ab81dc9153d09549da7f61) | Executable-only `comm` queries; one process-sampling worker in flight; input reads drain bounded batches. | `1bdafcf` |
| 8 | [Clip frame height](https://app.notion.com/3bd1c29536ab8180800fcb2986a1d1b1) | Both dimensions are clipped, footer targets remap, compact calendars fit short windows and narrow controls remain clickable. | `be489a7`, `bad4202` |
| 9 | [Mark RSS fallback](https://app.notion.com/3bd1c29536ab819f9a82e539e1c1b15e) | `~` marks affected readings; v3 telemetry records provenance; changing metric starts a new process leak series. | `e070743` |
| 10 | [Concurrent usage writers](https://app.notion.com/3bd1c29536ab8199bf5deab84e0160c6) | Existing max-count merge runs under an exclusive sidecar lock; contention preserves pending time. Explicit unlock avoids inherited-descriptor races. | `4e38a61`, `2b70d47` |
| 11 | [Focus timeout and retry](https://app.notion.com/3bd1c29536ab81e3a369d6013371d522) | Existing eight-second query budget and normal retry schedule retained. Total probe budget fits the 15-second credit window; monitor reports tracking failures. Unknown presence breaks credit. | `1d7872b`, `be489a7`, `69fac0c` |
| 12 | [Rotate and prune histories](https://app.notion.com/3bd1c29536ab815783e7db8ab20f396a) | Detailed telemetry rotates at 10 MiB with three backups. Recorded migration removes superseded resource/process schemas. Daily usage totals remain. | `036fbee` |
| 13 | [Footprint leak thresholds](https://app.notion.com/3bd1c29536ab8176836fd2655f64e947) | Absolute growth replaces the baseline-relative gate; downward-step allowance is 256 KiB. Tests cover large baselines, oscillation, spikes and plateaus. | `d3d602f` |

History rotation deletes the oldest backup and the schema migration deletes obsolete resource/process files. This changes the former preservation rule deliberately to implement item 12. Tests exercise temporary files only; development did not prune real history. Reinstall the login tracker after merging/building so its copied binary uses the new code.

Validation passed: 86 Rust tests, formatting and Clippy on macOS, Linux-target Clippy, 14 packaging failure/success scenarios, eight disposable PTY cases, a real universal build and npm pack dry run, and a read-only snapshot from the resulting binary. Linux runtime behavior remains covered by CI rather than a local Linux execution.

The leak thresholds are still heuristics. Regression tests show the reported large-baseline leak is caught and the modeled allocation noise is rejected; they do not establish a production false-positive rate. Unicode widths handle the covered terminal cases, though terminal-specific rendering can vary for unusual script shaping or emoji sequences.
