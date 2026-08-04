#!/bin/bash
# Builds DisEQ.app. Pass "release" for an optimised build.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/target/DisEQ.app"

if [ "$PROFILE" = "release" ]; then
	cargo build --release --manifest-path "$ROOT/Cargo.toml" -p kd-app
	BIN="$ROOT/target/release/DisEQ"
else
	cargo build --manifest-path "$ROOT/Cargo.toml" -p kd-app
	BIN="$ROOT/target/debug/DisEQ"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/bundle/Info.plist" "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/DisEQ"

# Ad-hoc signature. A status item requires a bundle, and an unsigned bundle is
# refused on Apple Silicon.
codesign --force --sign - "$APP"

echo "built $APP"
