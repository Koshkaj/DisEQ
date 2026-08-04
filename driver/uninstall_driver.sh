#!/bin/bash
# Removes DisEQ.driver from /Library/Audio/Plug-Ins/HAL.
#
# Needs an administrator password and restarts coreaudiod, which drops all audio
# on the machine for about a second.
set -euo pipefail

INSTALLED="/Library/Audio/Plug-Ins/HAL/DisEQ.driver"

if [ ! -d "$INSTALLED" ]; then
	echo "not installed — nothing to remove"
	exit 0
fi

echo "About to remove $INSTALLED and restart coreaudiod."
echo "If DisEQ's device is the current output, macOS will pick another."
echo ""

sudo rm -rf "$INSTALLED"
sudo killall coreaudiod

echo "removed."
