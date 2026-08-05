#!/bin/bash
# Builds target/DisEQ-<version>.dmg: the app, and somewhere to drag it.
#
# Dragging is the whole installation. Finder cannot run anything from a disk
# image, so the audio driver is not installed here — the app carries it and
# offers to install it the first time it runs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# shellcheck source=bundle/common.sh
source "$ROOT/bundle/common.sh"
VERSION="$(diseq_version "$ROOT")"

APP="$ROOT/target/DisEQ.app"
STAGE="$ROOT/target/dmg-stage"
VOLUME="DisEQ $VERSION"
WRITABLE="$ROOT/target/DisEQ-rw.dmg"
DMG="$ROOT/target/DisEQ-$VERSION.dmg"

"$ROOT/bundle/build_app.sh" release

rm -rf "$STAGE" "$WRITABLE" "$DMG"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/DisEQ.app"
ln -s /Applications "$STAGE/Applications"
# Otherwise the volume ships with a directory of file-system events recorded
# while it was being built.
mkdir -p "$STAGE/.fseventsd"
touch "$STAGE/.fseventsd/no_log"

# A writable image first: the window layout below is Finder state stored on the
# volume, and Finder cannot write to the compressed image that ships.
hdiutil create -srcfolder "$STAGE" -volname "$VOLUME" -fs HFS+ \
	-format UDRW -ov "$WRITABLE" >/dev/null

DEVICE="$(hdiutil attach -readwrite -noverify -noautoopen "$WRITABLE" \
	| grep '^/dev/' | head -1 | awk '{print $1}')"
trap 'hdiutil detach "$DEVICE" -quiet 2>/dev/null || true' EXIT

# Driving Finder needs Automation consent, which a release runner has no way to
# grant. The layout is a nicety; the image is correct without it, so a refusal
# is reported and not fatal.
if ! osascript >/dev/null 2>&1 <<EOF
tell application "Finder"
	tell disk "$VOLUME"
		open
		set current view of container window to icon view
		set toolbar visible of container window to false
		set statusbar visible of container window to false
		set the bounds of container window to {200, 150, 800, 570}
		set options to the icon view options of container window
		set arrangement of options to not arranged
		set icon size of options to 128
		set position of item "DisEQ.app" of container window to {150, 190}
		set position of item "Applications" of container window to {450, 190}
		close
		open
		update without registering applications
		delay 1
	end tell
end tell
EOF
then
	echo "note: could not arrange the disk image window — Finder automation was" >&2
	echo "      refused. The image is fine; it opens with Finder's own layout." >&2
fi

sync
hdiutil detach "$DEVICE" -quiet
trap - EXIT

hdiutil convert "$WRITABLE" -format UDZO -imagekey zlib-level=9 -o "$DMG" >/dev/null
rm -f "$WRITABLE"
rm -rf "$STAGE"

echo "built $DMG"
