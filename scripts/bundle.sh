#!/bin/sh
# Build "Rust Computer Use.app" in target/release. Sign with $CODESIGN_IDENTITY (default: ad-hoc).
# A stable identity keeps macOS permission grants across rebuilds; ad-hoc ones change per build.
set -e
cd "$(dirname "$0")/.."
cargo build --release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="target/release/Rust Computer Use.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/rust-computer-use "$app/Contents/MacOS/"
cp macos/AppIcon.icns "$app/Contents/Resources/"
sed "s/VERSION/$version/g" macos/Info.plist > "$app/Contents/Info.plist"
codesign --force --sign "${CODESIGN_IDENTITY:--}" --options runtime "$app"
echo "$app"
