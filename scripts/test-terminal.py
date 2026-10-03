#!/usr/bin/env python3
"""Exercise the real TUI in a disposable PTY, with history confined to a temp dir."""

import fcntl
import os
from pathlib import Path
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time


ROOT = Path(__file__).resolve().parent.parent
RESTORE_SEQUENCES = (b"\x1b[?1003l", b"\x1b[?1006l", b"\x1b[?25h", b"\x1b[?1049l")


def test_source(directory):
    source = (ROOT / "src/main.rs").read_text()
    # Rewrite only the disposable test copy; HOME and the production binary
    # stay untouched. Both cfg variants use the same fixture data directory.
    starts = list(re.finditer(r"fn default_data_dir\(\) -> Option<PathBuf> \{", source))
    for match in reversed(starts):
        end = match.end()
        depth = 1
        while depth:
            depth += (source[end] == "{") - (source[end] == "}")
            end += 1
        source = source[:match.start()] + (
            'fn default_data_dir() -> Option<PathBuf> { Some(PathBuf::from('
            'env::var_os("GHOSTTY_TOP_TEST_DATA").expect("fixture directory"))) }'
        ) + source[end:]
    anchor = "    // Started before the first frame"
    assert source.count(anchor) == 1
    source = source.replace(anchor, '''    if env::var_os("GHOSTTY_TOP_TEST_PANIC").is_some() {
        panic!("terminal cleanup probe");
    }
''' + anchor)
    path = directory / "main.rs"
    path.write_text(source)
    version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)[1]
    environment = os.environ.copy()
    environment["CARGO_PKG_VERSION"] = version
    binary = directory / "ghostty-top"
    subprocess.run(["rustc", "--edition=2021", "-C", "panic=abort", str(path), "-o", str(binary)],
                   env=environment, check=True)
    return binary


def read_available(master, output, timeout=0.02):
    if select.select([master], [], [], timeout)[0]:
        try:
            output.extend(os.read(master, 65536))
        except OSError:
            pass


def run_case(binary, directory, name, payload=None, stop_signal=None, panic=False, size=(24, 100)):
    master, slave = os.openpty()
    os.set_blocking(master, False)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
    original = termios.tcgetattr(slave)
    environment = os.environ.copy()
    environment["GHOSTTY_TOP_TEST_DATA"] = str(directory / name)
    if panic:
        environment["GHOSTTY_TOP_TEST_PANIC"] = "1"

    def controlling_terminal():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    process = subprocess.Popen([str(binary)], stdin=slave, stdout=slave, stderr=slave,
                               env=environment, preexec_fn=controlling_terminal)
    output = bytearray()
    deadline = time.monotonic() + 8
    try:
        while b"\x1b[?1006h" not in output and process.poll() is None and time.monotonic() < deadline:
            read_available(master, output)
        assert b"\x1b[?1006h" in output, f"{name}: TUI never entered mouse mode: {output[-300:]!r}"
        pending = memoryview(payload or b"")
        if stop_signal is not None:
            process.send_signal(stop_signal)
        while process.poll() is None and time.monotonic() < deadline:
            if pending:
                try:
                    pending = pending[os.write(master, pending):]
                except BlockingIOError:
                    pass
            read_available(master, output)
        assert process.poll() is not None, f"{name}: keyboard/termination stalled"
        for _ in range(5):
            read_available(master, output)
        for sequence in RESTORE_SEQUENCES:
            assert sequence in output, f"{name}: missing terminal restore {sequence!r}"
        restored = termios.tcgetattr(master)
        # macOS can toggle PENDIN as pending input is consumed. All actual
        # terminal modes and control characters must return to their old values.
        pendin = getattr(termios, "PENDIN", 0)
        original[3] &= ~pendin
        restored[3] &= ~pendin
        assert restored == original, f"{name}: terminal settings were not restored"
        if panic:
            assert process.returncode != 0, f"{name}: expected panic abort"
            assert output.index(b"\x1b[?1049l") < output.index(b"terminal cleanup probe")
        else:
            assert process.returncode == 0, f"{name}: exited {process.returncode}"
        print(f"Passed {name}.")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)


def main():
    with tempfile.TemporaryDirectory(prefix="ghostty-top-terminal-") as temporary:
        directory = Path(temporary)
        binary = test_source(directory)
        run_case(binary, directory, "quit", payload=b"q")
        run_case(binary, directory, "escape then quit", payload=b"\x1bq")
        run_case(binary, directory, "abandoned mouse then quit", payload=b"\x1b[<0;10qqq")
        run_case(binary, directory, "mouse backlog then quit", payload=b"\x1b[<35;10;5M" * 150 + b"q")
        run_case(binary, directory, "short calendar", payload=b"uq", size=(8, 20))
        run_case(binary, directory, "SIGTERM", stop_signal=signal.SIGTERM)
        run_case(binary, directory, "SIGHUP", stop_signal=signal.SIGHUP)
        run_case(binary, directory, "panic abort", panic=True)


if __name__ == "__main__":
    main()
