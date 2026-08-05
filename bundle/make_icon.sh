#!/bin/bash
# Regenerates bundle/DisEQ.icns from images/logo.png.
#
# Not part of the build: the result is committed, because it changes only when
# the artwork does. This is also why it may use Pillow — it runs on a machine
# with a working Python, never on a release runner.
#
# The logo is wider than it is tall and floats in a large transparent frame, so
# it is trimmed to what is actually drawn and re-centred in a square with the
# inset Apple's icon grid expects. Cropping the frame instead would leave the
# artwork small and off-centre.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="$ROOT/images/logo.png"
ICONSET="$ROOT/target/DisEQ.iconset"
SQUARE="$ROOT/target/icon-square.png"

rm -rf "$ICONSET"
mkdir -p "$ICONSET"

python3 - "$SOURCE" "$SQUARE" <<'PY'
import sys
from PIL import Image

source, destination = sys.argv[1], sys.argv[2]
CANVAS = 1024
# Apple's icon grid leaves a margin. Filling the square edge to edge is what
# makes an icon look larger than every other one in the Dock rather than
# better drawn — but the mark is wide, so the margin is small and the height
# takes care of itself.
CONTENT = int(CANVAS * 0.92)
# The logo carries a barely-visible haze over its whole frame, so `getbbox`
# alone reports the full image and trims nothing. Anything under this is glow
# and drop shadow, not the mark.
FLOOR = 16

image = Image.open(source).convert("RGBA")
drawn = image.getchannel("A").point(lambda value: 255 if value > FLOOR else 0)
image = image.crop(drawn.getbbox())
image.thumbnail((CONTENT, CONTENT), Image.LANCZOS)

canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
canvas.paste(image, ((CANVAS - image.width) // 2, (CANVAS - image.height) // 2))
canvas.save(destination)
PY

for size in 16 32 128 256 512; do
	sips -z "$size" "$size" "$SQUARE" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
	sips -z $((size * 2)) $((size * 2)) "$SQUARE" \
		--out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done

iconutil --convert icns "$ICONSET" --output "$ROOT/bundle/DisEQ.icns"
rm -rf "$ICONSET" "$SQUARE"

echo "wrote $ROOT/bundle/DisEQ.icns"
