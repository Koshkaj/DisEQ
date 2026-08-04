#!/bin/bash
# Installs DisEQ.driver into /Library/Audio/Plug-Ins/HAL.
#
# This needs an administrator password and restarts coreaudiod, which drops all
# audio on the machine for about a second. Nothing else on the system changes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILT="$ROOT/target/DisEQ.driver"
HAL="/Library/Audio/Plug-Ins/HAL"
INSTALLED="$HAL/DisEQ.driver"

if [ ! -d "$BUILT" ]; then
	echo "error: $BUILT does not exist — run 'make driver' first" >&2
	exit 1
fi

if [ -d "$HAL/eqMac.driver" ]; then
	echo "note: eqMac is installed at $HAL/eqMac.driver."
	echo "      Both drivers can be installed at once, but only one can be the"
	echo "      default output. Testing DisEQ means selecting its device."
	echo ""
fi

echo "About to install:"
echo "  from  $BUILT"
echo "  to    $INSTALLED"
echo "  then  killall coreaudiod   (all audio stops for about a second)"
echo ""

sudo rm -rf "$INSTALLED"
sudo mkdir -p "$HAL"
sudo cp -R "$BUILT" "$INSTALLED"
# coreaudiod runs as _coreaudiod and will not load a plug-in it cannot trust.
sudo chown -R root:wheel "$INSTALLED"
sudo chmod -R 755 "$INSTALLED"

sudo killall coreaudiod

echo ""
echo "installed. Waiting for coreaudiod to come back…"
sleep 2

# Says whether coreaudiod actually loaded it, which a successful copy does not.
cargo run -q --manifest-path "$ROOT/Cargo.toml" -p kd-audio --example driver_probe
