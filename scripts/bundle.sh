#!/bin/sh
# Build "Rust Computer Use.app" in target/release. macOS keeps permission grants across updates
# only for a stable signature, so releases are signed with the release certificate:
# $SIGNING_P12 (password in $SIGNING_P12.password), with rcodesign. $CODESIGN_IDENTITY signs with
# a keychain identity instead. Without either the build is signed ad hoc, which changes per build.
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
p12="${SIGNING_P12:-$HOME/Library/Application Support/RustComputerUse/signing/release.p12}"
if [ -z "$CODESIGN_IDENTITY" ] && [ -f "$p12" ]; then
    rcodesign sign --p12-file "$p12" --p12-password "$(cat "$p12.password")" \
        --code-signature-flags runtime "$app" >/dev/null
else
    codesign --force --sign "${CODESIGN_IDENTITY:--}" --options runtime "$app"
fi
echo "$app"
