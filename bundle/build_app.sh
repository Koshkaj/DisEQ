#!/bin/bash
# Builds DisEQ.app. Pass "release" for an optimised, universal build.
#
# The HAL plug-in is built and carried inside Contents/Resources. The app
# installs it from there itself, so a copy dragged out of a disk image needs
# nothing from a terminal.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/target/DisEQ.app"

# shellcheck source=bundle/common.sh
source "$ROOT/bundle/common.sh"
VERSION="$(diseq_version "$ROOT")"

"$ROOT/driver/build_driver.sh"

if [ "$PROFILE" = "release" ]; then
	# Universal. Sonoma still runs on Intel, and a release is dragged onto
	# machines this build does not get to choose.
	for target in aarch64-apple-darwin x86_64-apple-darwin; do
		cargo build --release --target "$target" --manifest-path "$ROOT/Cargo.toml" -p kd-app
	done
	BIN="$ROOT/target/DisEQ-universal"
	lipo -create -output "$BIN" \
		"$ROOT/target/aarch64-apple-darwin/release/DisEQ" \
		"$ROOT/target/x86_64-apple-darwin/release/DisEQ"
else
	# Host architecture only: a debug bundle is for iterating, not for shipping.
	cargo build --manifest-path "$ROOT/Cargo.toml" -p kd-app
	BIN="$ROOT/target/debug/DisEQ"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/bundle/Info.plist" "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/DisEQ"
cp "$ROOT/bundle/DisEQ.icns" "$APP/Contents/Resources/DisEQ.icns"
stamp_version "$APP/Contents/Info.plist" "$VERSION"

# ditto rather than cp -R: the plug-in is already signed, and this copies the
# bundle without disturbing that.
ditto "$ROOT/target/DisEQ.driver" "$APP/Contents/Resources/DisEQ.driver"

# Ad-hoc signature. A status item requires a bundle, and an unsigned bundle is
# refused on Apple Silicon. The nested plug-in is signed by build_driver.sh
# before it is copied in, because signing the app seals what it contains —
# nested code signed afterwards would invalidate the outer signature.
codesign --force --sign - "$APP"

echo "built $APP ($VERSION, $(lipo -archs "$APP/Contents/MacOS/DisEQ"))"
