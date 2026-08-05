#!/bin/bash
# Builds DisEQ.driver — a universal, ad-hoc signed AudioServerPlugIn bundle.
#
# Building needs no privileges. Installing does; see install_driver.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRIVER="$ROOT/target/DisEQ.driver"
SOURCE="$ROOT/driver/Source/DisEQ.c"

# shellcheck source=bundle/common.sh
source "$ROOT/bundle/common.sh"

rm -rf "$DRIVER"
mkdir -p "$DRIVER/Contents/MacOS"
cp "$ROOT/driver/Info.plist" "$DRIVER/Contents/Info.plist"
# The version the app compares against to decide whether an installed plug-in
# is the one it carries, so it has to come from the same place as the app's.
stamp_version "$DRIVER/Contents/Info.plist" "$(diseq_version "$ROOT")"

# A HAL plug-in is a Mach-O bundle, not a dylib: coreaudiod loads it with
# CFBundle. Both architectures, because coreaudiod's is not ours to choose.
clang \
	-arch x86_64 -arch arm64 \
	-mmacosx-version-min=14.0 \
	-bundle \
	-std=c11 \
	-O2 \
	-Wall -Wextra -Werror \
	-fvisibility=hidden \
	-framework CoreAudio \
	-framework CoreFoundation \
	-o "$DRIVER/Contents/MacOS/DisEQ" \
	"$SOURCE"

# The factory has to be findable by name; everything else stays hidden.
if ! nm -gU "$DRIVER/Contents/MacOS/DisEQ" | grep -q "_DisEQ_Create"; then
	echo "error: DisEQ_Create is not exported — coreaudiod will not find the factory" >&2
	exit 1
fi

# Unsigned bundles do not load on Apple Silicon. Ad-hoc is enough locally;
# distributing to anyone else needs a Developer ID.
codesign --force --sign - "$DRIVER"

echo "built $DRIVER"
echo "install it with: make driver-install   (asks for your password)"
