# Attribution

## eqMac — Apache License 2.0

Copyright © Bitgapp Ltd.
Source: https://github.com/bitgapp/eqMac (v1.3.2)
Licence: Apache License 2.0 — https://www.apache.org/licenses/LICENSE-2.0

Portions of the audio engine are derived from eqMac, specifically:

| Ours | Derived from | Change |
|---|---|---|
| `crates/kd-audio/src/eq.rs` | `native/app/Source/Audio/Effects/Equalizers/` | Ported Swift → Rust. The preset table and the 500 ms / 30 fps transition are eqMac's; the auto-preamp, the pull-based ramp, and the "Manual" fallback are ours |
| `crates/kd-audio/src/engine.rs` | `native/app/Source/Audio/Effects/Equalizers/Equalizer.swift` | Ported Swift → Rust. Band configuration and the `globalGain`-as-preamp convention kept |
| `crates/kd-audio/src/ring.rs` | `native/shared/Source/CircularBuffer.swift` | Ported Swift → Rust |
| `crates/kd-audio/src/bridge.rs` | `native/app/Source/Audio/Outputs/Output.swift` | The sample-time offset, the safety offset and the re-alignment on a failed read are eqMac's. Changed: the state lives in atomics on a shared object rather than globals reached through `Application`, and sample times are integers throughout |
| `crates/kd-audio/src/playback.rs` | `native/app/Source/Audio/Outputs/Output.swift` | Ported Swift → Rust. The varispeed PID controller and its ±0.2% bounds are eqMac's. Changed: audio is supplied by an `AVAudioSourceNode` rather than by installing a render callback on the varispeed unit's input, the equaliser sits in this graph rather than in a second one, and the controller is stepped by the caller rather than by its own dispatch timer |

Not derived from eqMac:

- `crates/kd-audio/src/offline.rs` — offline rendering and Goertzel measurement, written to
  make the EQ's effect testable. eqMac has no equivalent.
- `crates/kd-audio/src/format.rs`, `router.rs`, `unit.rs` — format handling, route lifecycle,
  and the message-encoding workaround for `-[AVAudioIONode audioUnit]`.
- `crates/kd-audio/src/shared.rs` and the shared ring in `driver/Source/DisEQ.c` — the
  capture side. eqMac reads its own driver's input stream through `AVAudioEngine`
  (`Engine.swift:42`) and asks for the microphone to do it
  (`AVCaptureDevice.requestAccess(for: .audio)`, `InputSource.swift:33`, with a modal that
  refuses to start without it). The routing architecture here is eqMac's — the audio arrives
  because our own plug-in is the output device — but it leaves coreaudiod through POSIX shared
  memory rather than through an audio input, so no capture API is involved and no permission is
  asked for.
- the process-tap App Mixer (`processes.rs`, `tap.rs`, `mixer.rs`) — eqMac's public tree
  contains no per-app mixer.

Each ported file carries this notice in its header.

## The driver

`driver/Source/DisEQ.c` is written against the public `AudioServerPlugIn.h` in the
macOS SDK. Its shape — the vtable, the per-object property dispatch, the zero-timestamp
arithmetic — follows the pattern Apple's `NullAudio` sample established and every HAL
plug-in since has used. The loopback specifics follow eqMac's driver
(`native/driver/Source/EQMDevice.swift`, Apache-2.0): the ring buffer indexed by sample
time, accumulate-on-write, clear-a-ring-behind-on-read.

Changed from eqMac: ported Swift → C; the mutex in the IO path is replaced with atomics;
volume is applied in the driver rather than in the app, and ramped per sample so a slider
drag does not zipper.

## FreeDisplay — reference only

`references/FreeDisplay` was read for its documented macOS display-API pitfalls. No code was
copied; the DDC framing in `crates/kd-sys/src/ddc.rs` follows MonitorControl's checksum
convention instead, which is the one that works.
