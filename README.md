# DisEQ

DisEQ is a lightweight macOS menu-bar utility for controlling displays and
system audio from one native AppKit panel.

It provides per-display brightness, resolution and connection controls; marks
and orders the main display; exposes the system-wide Night Shift state; and can
route system audio through a ten-band equalizer with per-application volume.

## Requirements

- macOS 14 or newer on Apple Silicon
- Rust 1.85 or newer
- Xcode command-line tools

## Build and run

DisEQ must run from an application bundle for its menu-bar status item to work:

```sh
make app-release
open target/DisEQ.app
```

For a debug build and restart during development:

```sh
make run
```

Run the verification suite with:

```sh
make check
make test
make lint
```

## Audio driver

Audio enhancement requires the companion HAL plug-in. Building it does not
need administrator privileges:

```sh
make driver
```

Installing it writes to `/Library/Audio/Plug-Ins/HAL` and restarts
`coreaudiod`, briefly interrupting all audio:

```sh
make driver-install
```

Use `make driver-uninstall` to remove it. Display controls work without the
audio driver.

## Implementation notes

The app is written in Rust directly against AppKit through `objc2`. Some
display capabilities and the native Night Shift toggle require dynamically
loaded private macOS interfaces. Missing interfaces disable only the affected
feature rather than preventing the app from launching.

See [technical_docs.md](technical_docs.md) for the system-interface reference
and [PERFORMANCE.md](PERFORMANCE.md) for the measured release footprint and
optimization roadmap.

## Attribution and licensing

Portions of the audio engine are derived from eqMac under Apache License 2.0.
The exact provenance and modifications are listed in
[ATTRIBUTION.md](ATTRIBUTION.md); the applicable license text is included at
[LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt).

No license is granted for the remainder of the repository unless one is added
explicitly.
