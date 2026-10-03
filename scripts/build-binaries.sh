#!/bin/sh
# Builds the macOS binary shipped by the npm package.
#
# Both architectures are merged into one universal binary so the package needs
# no launcher script: `bin` points straight at the executable, which keeps it
# working under Bun, which does not run install scripts by default and may have
# no Node available to run a shim.
set -eu

cd "$(dirname "$0")/.."

package_version=$(awk '
    match($0, /"version"[[:space:]]*:[[:space:]]*"[^"]*"/) {
        value = substr($0, RSTART, RLENGTH)
        sub(/^[^:]*:[[:space:]]*"/, "", value)
        sub(/"$/, "", value)
        print value
        exit
    }
' package.json)
cargo_version=$(awk '
    /^[[:space:]]*\[package\][[:space:]]*$/ { package = 1; next }
    /^[[:space:]]*\[/ { package = 0 }
    package && /^[[:space:]]*version[[:space:]]*=/ {
        value = $0
        sub(/^[^=]*=[[:space:]]*"/, "", value)
        sub(/"[[:space:]]*(#.*)?$/, "", value)
        print value
        exit
    }
' Cargo.toml)
if [ -z "$package_version" ] || [ -z "$cargo_version" ] || [ "$package_version" != "$cargo_version" ]; then
    echo "error: package.json version ($package_version) must match Cargo.toml version ($cargo_version)" >&2
    exit 1
fi

installed_targets=$(rustup target list --installed)
missing=""
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    if ! printf '%s\n' "$installed_targets" | grep -qx "$target"; then
        missing="$missing $target"
    fi
done
if [ -n "$missing" ]; then
    echo "error: both Apple targets are required; run: rustup target add$missing" >&2
    exit 1
fi

for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --target "$target"
done

mkdir -p binaries
lipo -create -output binaries/ghostty-top \
    target/aarch64-apple-darwin/release/ghostty-top \
    target/x86_64-apple-darwin/release/ghostty-top
lipo -verify_arch arm64 x86_64 binaries/ghostty-top
chmod +x binaries/ghostty-top
if [ ! -x binaries/ghostty-top ]; then
    echo "error: binaries/ghostty-top is not executable" >&2
    exit 1
fi

echo
lipo -info binaries/ghostty-top
