#!/bin/sh
# Exercises release packaging without a compiler or either Apple target.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/ghostty-top-package-test.XXXXXX")
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir -p "$fixture/scripts" "$fixture/mock-bin"
cp "$script_dir/build-binaries.sh" "$fixture/scripts/build-binaries.sh"

cat > "$fixture/mock-bin/rustup" <<'MOCK'
#!/bin/sh
set -eu
printf 'rustup %s\n' "$*" >> "$PACKAGING_LOG"
[ "${FAIL_RUSTUP:-0}" = 0 ] || exit 1
printf '%s\n' "$INSTALLED_TARGETS"
MOCK
cat > "$fixture/mock-bin/cargo" <<'MOCK'
#!/bin/sh
set -eu
printf 'cargo %s\n' "$*" >> "$PACKAGING_LOG"
[ "$1 $2 $3" = 'build --release --target' ]
[ "$4" != "${FAIL_TARGET:-}" ] || exit 1
mkdir -p "target/$4/release"
printf 'mock binary\n' > "target/$4/release/ghostty-top"
MOCK
cat > "$fixture/mock-bin/lipo" <<'MOCK'
#!/bin/sh
set -eu
printf 'lipo %s\n' "$*" >> "$PACKAGING_LOG"
[ "$1" != "${FAIL_LIPO:-}" ] || exit 1
case "$1" in
    -create)
        [ "$2" = -output ]
        [ -f "$4" ] && [ -f "$5" ]
        printf 'universal binary\n' > "$3"
        ;;
    -verify_arch)
        [ "$2 $3" = 'arm64 x86_64' ]
        [ -f "$4" ]
        ;;
    -info)
        [ -x "$2" ]
        printf 'Architectures in the fat file: %s are: arm64 x86_64\n' "$2"
        ;;
    *) exit 1 ;;
esac
MOCK
cat > "$fixture/mock-bin/chmod" <<'MOCK'
#!/bin/sh
set -eu
printf 'chmod %s\n' "$*" >> "$PACKAGING_LOG"
case "${CHMOD_MODE:-normal}" in
    fail) exit 1 ;;
    noop) exit 0 ;;
    normal) /bin/chmod "$@" ;;
esac
MOCK
chmod +x "$fixture/mock-bin/"*

export PACKAGING_LOG="$fixture/calls.log"
export PATH="$fixture/mock-bin:$PATH"
export INSTALLED_TARGETS FAIL_RUSTUP FAIL_TARGET FAIL_LIPO CHMOD_MODE
passed=0

reset_fixture() {
    rm -rf "$fixture/target" "$fixture/binaries"
    : > "$PACKAGING_LOG"
    printf '{"name":"ghostty-top","version":"0.1.0"}\n' > "$fixture/package.json"
    printf '[package]\nname = "ghostty-top"\nversion = "0.1.0"\n' > "$fixture/Cargo.toml"
    INSTALLED_TARGETS=$(printf 'aarch64-apple-darwin\nx86_64-apple-darwin\n')
    FAIL_RUSTUP=0
    FAIL_TARGET=
    FAIL_LIPO=
    CHMOD_MODE=normal
}

run_failure() {
    if sh "$fixture/scripts/build-binaries.sh" > "$fixture/output" 2>&1; then
        echo "FAIL: $1 unexpectedly succeeded" >&2
        exit 1
    fi
    passed=$((passed + 1))
}

assert_no_build() {
    if grep -q '^cargo ' "$PACKAGING_LOG"; then
        echo 'FAIL: validation failure ran cargo' >&2
        exit 1
    fi
}

reset_fixture
printf '{"version":"0.2.0"}\n' > "$fixture/package.json"
run_failure 'version mismatch'
assert_no_build
grep -q 'must match Cargo.toml' "$fixture/output"

reset_fixture
printf '{"name":"ghostty-top"}\n' > "$fixture/package.json"
run_failure 'missing package version'
assert_no_build

for missing in aarch64-apple-darwin x86_64-apple-darwin both; do
    reset_fixture
    case "$missing" in
        aarch64-apple-darwin) INSTALLED_TARGETS=x86_64-apple-darwin ;;
        x86_64-apple-darwin) INSTALLED_TARGETS=aarch64-apple-darwin ;;
        both) INSTALLED_TARGETS= ;;
    esac
    run_failure "missing $missing"
    assert_no_build
    grep -q 'both Apple targets are required' "$fixture/output"
done

reset_fixture
FAIL_RUSTUP=1
run_failure 'rustup failure'
assert_no_build

for target in aarch64-apple-darwin x86_64-apple-darwin; do
    reset_fixture
    FAIL_TARGET=$target
    run_failure "build failure for $target"
    if grep -q '^lipo ' "$PACKAGING_LOG"; then
        echo 'FAIL: failed build reached lipo' >&2
        exit 1
    fi
done

for operation in -create -verify_arch -info; do
    reset_fixture
    FAIL_LIPO=$operation
    run_failure "lipo $operation failure"
done

for mode in fail noop; do
    reset_fixture
    CHMOD_MODE=$mode
    run_failure "chmod $mode"
done

reset_fixture
sh "$fixture/scripts/build-binaries.sh" > "$fixture/output" 2>&1
[ "$(grep -c '^cargo ' "$PACKAGING_LOG")" = 2 ]
grep -qx 'lipo -verify_arch arm64 x86_64 binaries/ghostty-top' "$PACKAGING_LOG"
grep -q 'arm64 x86_64' "$fixture/output"
[ -x "$fixture/binaries/ghostty-top" ]
passed=$((passed + 1))
printf 'Passed %s packaging scenarios.\n' "$passed"
