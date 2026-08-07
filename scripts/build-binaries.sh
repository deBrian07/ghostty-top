#!/bin/sh
# Builds the macOS binary shipped by the npm package.
#
# Both architectures are merged into one universal binary so the package needs
# no launcher script: `bin` points straight at the executable, which keeps it
# working under Bun, which does not run install scripts by default and may have
# no Node available to run a shim.
set -eu

cd "$(dirname "$0")/.."
mkdir -p binaries

slices=""
missing=""
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    if rustup target list --installed | grep -qx "$target"; then
        cargo build --release --target "$target"
        slices="$slices target/$target/release/ghostty-top"
    else
        missing="$missing $target"
    fi
done

if [ -z "$slices" ]; then
    echo "error: no Apple targets installed; run: rustup target add aarch64-apple-darwin" >&2
    exit 1
fi

lipo -create -output binaries/ghostty-top $slices
chmod +x binaries/ghostty-top

if [ -n "$missing" ]; then
    echo >&2
    for target in $missing; do
        echo "warning: $target is missing, so the package will not run there." >&2
        echo "         fix with: rustup target add $target" >&2
    done
fi

echo
lipo -info binaries/ghostty-top
