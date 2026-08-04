# DisEQ — Sound Plan

Two features: a **10-band EQ** and an **App Mixer** (per-app volume), fed by a virtual audio
device so outputs with no hardware volume control still get a working slider.

Companion documents: [`plan.md`](./plan.md) (displays), [`technical_docs.md`](./technical_docs.md).

Source studied: `references/eqMac` (v1.3.2, **Apache-2.0**).

---

## 1. Evidence gathered on this machine

### 1.1 The premise is real

`make audio-probe` enumerates every output device and asks CoreAudio whether its volume is
settable:

```
DELL U2720Q                     volume: None       settable: false
MacBook Pro Speakers            volume: Some(0.25) settable: true
MacBook Pro Speakers (eqMac)    volume: Some(0.25) settable: true   ← default output
eqMac Export                    volume: None       settable: false
```

The Dell's DisplayPort audio exposes **no volume property at all** — neither
`kAudioHardwareServiceDeviceProperty_VirtualMainVolume` nor
`kAudioDevicePropertyVolumeScalar`. No CoreAudio call will give it a slider. The only route
is to render the audio ourselves at a software gain and hand the device a full-scale signal.
That is exactly what a virtual output device buys.

### 1.2 What eqMac installs

Inspected on disk, not inferred:

| Fact | Value |
|---|---|
| Location | `/Library/Audio/Plug-Ins/HAL/eqMac.driver` |
| `CFPlugInTypes` | `443ABAB8-E7B3-491A-B985-BEB9187030DB` = `kAudioServerPlugInTypeUUID` |
| `sandboxSafe` | `true` (or `coreaudiod` refuses to load it) |
| Binary | universal `x86_64` + `arm64`, Mach-O **bundle** |
| Language | Swift |
| Devices published | `MacBook Pro Speakers (eqMac)` (output), `eqMac Export` (input) |
| DriverKit extensions | **none** — `/Library/DriverExtensions` empty, `systemextensionsctl` → 0 |

A userspace HAL plug-in: no kext, no DriverKit `.dext`, no system-extension approval, no SIP
involvement. Just a signed bundle in a protected directory. Worth copying: the virtual device
is **named after the hardware it proxies**, so users pick an output that reads like their own
speakers.

### 1.3 Licence — Apache-2.0, we can port

`references/eqMac/LICENSE` is the **Apache License 2.0**. Porting their code is fine provided
we keep the copyright notice, include the licence, and state what we changed. There is no
`NOTICE` file to propagate.

The public repository is v1.3.2 with the Pro features removed; later work lives in a private
fork. Confirmed by grep: **the App Mixer is not in the public tree** — no `CATapDescription`,
no process-tap code, nothing per-app. So the split matches the brief: port the EQ, write the
mixer from scratch.

---

## 2. How eqMac actually works

Reading `native/app/Source/Audio/`, the design is not what the plan first assumed.

### 2.1 The EQ is Apple's, not hand-written

`Equalizer.swift` creates an **`AVAudioUnitEQ`** and configures each band:

```swift
eq = AVAudioUnitEQ(numberOfBands: 10)
eq.globalGain = 0                 // preamp
for band in eq.bands {
  band.filterType = .parametric
  band.bandwidth  = 0.5           // octaves
  band.bypass     = false
}
```

Frequencies are the ISO set `[32, 64, 125, 250, 500, 1k, 2k, 4k, 8k, 16k]`, matching the
reference screenshot exactly. Presets are just ten gain values plus a global gain, with
`Transition.perform` ramping between them so preset changes do not click.

**This replaces the biquad cascade the plan originally specified.** Apple's unit is
real-time-safe, handles sample-rate changes, and is maintained by someone else.
`objc2-avf-audio` 0.3.2 exposes `AVAudioUnitEQ` (`numberOfBands`, `bands`, `filterType`,
`bandwidth`, `gain`, `bypass`, `globalGain`) and `AVAudioEngine`, so it is reachable from
Rust without writing any DSP.

### 2.2 Routing: two engines bridged by a circular buffer

The non-obvious part, and the part genuinely worth porting.

```
apps ──▶ eqMac driver device  (system default output, loopback)
              │
              ▼  as INPUT to a processing engine
        AVAudioEngine.inputNode
              │
        AVAudioUnitEQ                    ← the DSP
              │  AudioUnitAddRenderNotify, post-render
              ├────────────▶ CircularBuffer<Float>, indexed by sample time
              ▼
        mainMixerNode (outputVolume = 0) ← deliberately sunk to nothing
                             │
   separate output AudioUnit │ AURenderCallback pulls from the buffer
                             ▼
                    real hardware device
```

Three things make this work:

1. **The processing graph never plays anything.** `mainMixerNode.outputVolume = 0` sinks it.
   Processed audio is harvested from a post-render notify callback instead. The graph is used
   purely as a DSP host.
2. **A sample-time-indexed circular buffer bridges two devices with independent clocks.** The
   output callback reads `from = sampleTime + sampleOffset − safetyOffset`. `computeOffset()`
   establishes the relationship on first run; `resetOffsets()` recovers from drift.
3. **Underruns go silent rather than glitching** — `makeBufferSilent(abl)` on any error.

The clock-drift handling is the hard-won part. Two audio devices are never sample-locked, and
without an offset plus a safety margin the buffer either starves or overruns within seconds.

---

## 3. Architecture for us

### 3.1 The two features need two different mechanisms

A virtual output device receives audio **already mixed** by the HAL. Per-app volume is not
merely awkward there — the information no longer exists. Hence:

| Feature | Mechanism |
|---|---|
| **EQ + master volume** | Virtual device → `AVAudioUnitEQ` → hardware, per §2.2 |
| **App Mixer** | Core Audio **process taps** (macOS 14.4+) |

### 3.2 Process taps for the mixer

Public API since macOS 14.4; this machine runs 26.5.2.

- `AudioHardwareCreateProcessTap(CATapDescription*, AudioObjectID*)`
- `kAudioHardwarePropertyProcessObjectList` enumerates audio-producing processes;
  `kAudioProcessPropertyPID` / `kAudioProcessPropertyBundleID` name them (and give us the
  icons the reference UI shows).
- `CATapDescription.muteBehavior = .mutedWhenTapped` is the whole feature: the process's audio
  stops going straight to the output and arrives through our tap, where we apply its gain.
- Rendering happens through an **aggregate device** built with `kAudioAggregateDeviceTapListKey`.
- Requires TCC audio-capture consent (`NSAudioCaptureUsageDescription`), prompted once.
- Reference: `insidegui/AudioCap`.

### 3.3 The two paths join up

Because per-app gain is a scalar multiply, it does not need the EQ graph. Tapped audio is
scaled and written into the virtual device, where it joins everything else and meets the EQ:

```
tapped app ─▶ ×gain ─┐
tapped app ─▶ ×gain ─┼─▶ virtual device ─▶ AVAudioUnitEQ ─▶ ×master ─▶ hardware
untapped audio ──────┘
```

One DSP chain, one master volume, and the mixer is a pre-stage. Apps that refuse to be tapped
still reach the EQ through the virtual device — they simply have no individual fader.

---

## 4. Components

```
crates/kd-audio/          # engine, no UI
    devices.rs            #   enumeration, default-device tracking (moves out of kd-sys/audio.rs)
    ring.rs               #   sample-time circular buffer      — ported from eqMac
    engine.rs             #   AVAudioEngine + AVAudioUnitEQ host
    output.rs             #   HAL output unit, offset/drift handling — ported from eqMac
    eq.rs                 #   band/preset model, preset ramping
    tap.rs                #   process enumeration, tap lifecycle  — written from scratch
    mixer.rs              #   per-app gain, aggregate device      — written from scratch

driver/                   # separate build product
    Source/               #   AudioServerPlugIn, from Apple's NullAudio sample
    Info.plist            #   kAudioServerPlugInTypeUUID, sandboxSafe = true
    install.sh            #   install to /Library/Audio/Plug-Ins/HAL, restart coreaudiod

crates/kd-app/
    views/sound.rs        #   EQ view + App Mixer view
```

Attribution: files ported from eqMac carry the Apache-2.0 notice and a line naming what
changed. `ATTRIBUTION.md` records the provenance at project level.

### 4.1 Language split

The driver stays **C**, from Apple's `NullAudio` sample. The `AudioServerPlugIn` interface is
a COM-style vtable of ~40 C function pointers under hard real-time constraints; every working
implementation starts from that sample, and the driver is a passthrough with no logic worth
porting to Rust. eqMac wrote theirs in Swift, which is possible but buys nothing here.

Everything above the driver is Rust.

### 4.2 Real-time discipline

The render callbacks run on real-time threads: **no allocation, no locks, no logging, no
Objective-C message sends**. Parameters reach them through atomics or a lock-free triple
buffer. This is the most common way audio code fails, and the reason preset changes are
precomputed and ramped rather than recalculated in the callback.

---

## 5. Phases

### Permissions, and why this project needs none

Three ways to get system audio out of coreaudiod and into an app that can process it:

| Route | Permission | Who uses it |
|---|---|---|
| the virtual device's input stream, via `AVAudioEngine` | **Microphone** | eqMac (`Sources.swift:44` blocks startup until granted) |
| a global process tap | **System Audio Recording** | this project, briefly |
| POSIX shared memory written by our own plug-in | **none** | this project now |

The audio is already inside the plug-in — coreaudiod hands it over in `DoIOOperation(WriteMix)`
because the plug-in *is* the output device, and no permission guards that. The only question is
how it gets out, and the sandbox that constrains the plug-in answers it:

    /System/Library/Sandbox/Profiles/com.apple.audio.coreaudiod.sb:130
    (allow ipc-posix-shm)

So the driver publishes a ring at `/DisEQ.audio` and the app maps it read-only. TCC never
enters the picture. The App Mixer still needs process taps — per-application audio exists
nowhere else — but that prompt now appears when the mixer is switched on rather than at launch.

### How eqMac does it, read from its source

Worth writing down, because the shape of the whole feature follows from it
(`native/app/Source/Application.swift`, `Audio/Volume/Volume.swift`,
`Audio/Sources/System/Driver.swift`).

**The virtual device is a stand-in for the hardware, not a device in its own right.**
`startPassthrough()` reads the device the user currently has selected, then copies its
identity onto the driver: `Driver.name = "\(sourceName ?? name) (eqMac)"`, `Driver.latency =
selectedDevice.latency`, `matchDriverSampleRateToOutput()`, and the hardware's current volume.
Only then does it take both defaults — `AudioDevice.currentOutputDevice` **and**
`currentSystemDevice`. The second is what the volume keys and the menu-bar slider act on.

**Volume lands on the hardware when the hardware has a volume control, and in software when
it does not** (`Volume.swift`):

```swift
if (volumeSupported) { device.setVirtualMasterVolume(Float32(gain)) }   // real hardware
else                 { virtualVolume = gain }                           // our own mixer
Driver.device!.setVirtualMasterVolume(Float32(gain))                    // mirror, always
mixer.outputVolume = Float(virtualVolume)
```

So the slider always moves the virtual device (that is what the user sees), and the gain is
applied as late as possible — on the Scarlett's own volume when it has one, in the mixer for a
monitor that has none.

**Every write it makes is guarded against being read back as a user action.**
`ignoreNextVolumeEvent`, `ignoreEvents`, and a 500–1000 ms delay around every device change.
Without that, taking the default output reads a moment later as "the user picked something
else", and the app rebuilds against the stale device — the flapping this project hit.

**Picking another output in Sound settings is a target change, not an escape.** `selectOutput`
stops the engines, waits 500 ms, sets that device, and `startPassthrough` immediately takes
the default back. The device you pick becomes what eqMac plays *to*.

### Status

| Phase | State | Evidence |
|---|---|---|
| A — EQ | **done** | `cargo test -p kd-audio --test eq_response`: 8 offline renders through the real `AVAudioUnitEQ`. `bass_boost_is_measurably_present` asserts >6 dB at 60 Hz with the mids and treble unmoved |
| B — Driver | **carries the audio again** | No longer carries audio — the tap does that. It is the stand-in device: the name, the sample rate and the volume control the hardware may not have. It is the stand-in device — name, sample rate, volume control — *and* the capture side: `WriteMix` publishes the mix through a POSIX shared ring that the app maps read-only. `'kdsh'` publishes and retracts the device, so it exists only while the app runs |
| C — Routing | **working** | Virtual device → shared ring → EQ → varispeed → hardware, with no capture permission anywhere in it. Measured from the bundled app: `level -23 dB · drift -4 frames · 0 realignments` with audio playing. The route starts with the application, follows the device picked in Sound settings, and follows a sample-rate change made in Audio MIDI Setup |
| D — App Mixer | **working** | `make mixer ARGS=--run` creates the taps and the aggregate device on this machine and tears them down cleanly. Verified: taps permitted, both playing processes tapped, stereo float format at 48 kHz |
| E — UI and persistence | **done** | The Sound card carries the master volume, four switches, the preset picker, ten faders and a per-application strip. Everything is written to `~/Library/Application Support/DisEQ/settings.json` a second after it changes and on the way out, and restored without a ramp at launch |

Two decisions worth keeping in the persistence layer. The **gains are the record, not the
preset name** — choosing a preset writes its gains, and moving a band afterwards leaves gains
no preset matches, so applying a saved name on top would throw the edits away; the label is
derived from the gains instead. And the **saved volume is captured before the route starts**,
because `Route::start` adopts the hardware's own volume and would otherwise overwrite it.

Eight traps, all of which present as "the route runs and nothing comes out". `Health.level`
exists so that this class of failure is visible at all: every one of them leaves the timing,
the drift and the realignment count looking perfect.

**TCC attributes a command-line process to the terminal that started it**, and a process
without audio-capture consent is handed *silence rather than an error*. `cargo run --example
route` therefore reports a healthy tap carrying nothing, for ever. The same code inside the
app bundle, started by LaunchServices, works:
`open -n --env KD_SOUND_LOG=/tmp/route.log target/DisEQ.app`.

**An aggregate device with a tap list and no sub-devices runs perfectly and delivers zeros.**
The clock device has to appear in `kAudioAggregateDeviceSubDeviceListKey` as well as in
`kAudioAggregateDeviceMainSubDeviceKey`, and `kAudioAggregateDeviceTapAutoStartKey` has to be
1 — the IO proc is called on time at the right rate either way.

**Choosing the route's target before retracting a stale device answers the wrong question.**
A run that was killed rather than quit leaves the virtual device published *and selected*, so
"what is the system playing through?" answers "itself" — and the launch falls back to a
remembered UID that may be a device the user is no longer listening to. That is the relaunch
where sound only starts after switching output by hand. `SoundService::new` retracts first and
chooses second; the system's fallback after the retract is the device the user can actually
hear.

**A republished device is a different object, and the old one accepts writes for ever.** The
route holds a device snapshot; if anything retracts and republishes the plug-in — a previous
run killed rather than quit, a probe, a second instance — every later write to the held object
succeeds and does nothing. Worse, both incarnations can be
alive at once, so comparing against "the device with our UID" picks a winner arbitrarily. The
authority is the system's default output: while routing it must be the object the route holds,
and if it is *our device but not that object*, the route is rebuilt.

**A retracted device stays in the system's device list for tens of milliseconds after it
goes**, still answering with its UID — and every write to it succeeds and does nothing. A
publish immediately after a retract therefore hands back the corpse of the last device, and
the default-output change, the volume and the name are all accepted and discarded. `is_alive`
is the discriminator; `devices::diseq` filters on it and takes the newest match, and
`unpublish` waits for the device to actually go.

**An aggregate device hands over its sub-devices' input streams before its taps.** Reading
`mBuffers[0]` therefore reads the tap only when the sub-device has no inputs. Speakers have
none, so this worked on the built-in output and on the monitors, and delivered silence on a
Scarlett 2i2 — an interface with two microphone inputs, which occupy buffer 0. The tap's index
is now `kd_sys::audio::buffer_count(device, input: true)`, in both the route's capture and the
App Mixer.

**`kd_sys::audio::has_channels`** decided a device had channels by comparing the reply size against
`sizeof(u32)`. CoreAudio pads `AudioBufferList` and returns a whole one even for no buffers, so
every microphone read as an output device — and the route's fallback target picked one.
Fixed by summing `mNumberChannels` across the buffers.

**`-[AVAudioIONode audioUnit]`** is encoded by the runtime with its
Carbon name, `^{ComponentInstanceRecord=[1q]}`, while `objc2-avf-audio` declares it as
`*mut OpaqueAudioComponentInstance`. objc2 verifies encodings on every message send in a debug
build, so the mismatch panicked on the first route — in development builds only, which is why
it survived unit tests. `crates/kd-audio/src/unit.rs` does the send with the runtime's
spelling and casts the result.


### Phase A — EQ through the existing signal path
`AVAudioUnitEQ` hosted in Rust, ten ISO bands, preamp, presets, ramping. Verified against a
known-good source before any routing work.

*Done when:* a bass boost is measurably present in captured output.

### Phase B — Driver
Apple's `NullAudio` sample rebuilt as `DisEQ.driver`, ad-hoc signed, installed, volume
control published.

*Done when:* the device appears in Audio MIDI Setup and passes audio cleanly.

### Phase C — Routing
Two-engine design from §2.2: processing engine reading the virtual device, output unit
writing the hardware device, circular buffer with offset and safety margin between them.

*Done when:* **the Dell has a working volume slider** — the acceptance test for this plan.

### Phase D — App Mixer
Process enumeration, taps with `.mutedWhenTapped`, aggregate device, per-app gain, icons and
faders.

*Done when:* Spotify's volume moves independently of Brave.

### Phase E — UI and persistence
EQ view (10 faders, preamp, preset picker, Basic/Advanced tabs), App Mixer strip. The faders
are horizontal rows, not eqMac's vertical bank: the panel is fixed at 400 pt by the menu bar,
which leaves a vertical column narrower than its own frequency label.
Presets and per-app gains persisted by bundle ID. Restore the previous output device on quit.

---

## 6. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| **eqMac is installed and owns the default output** | Two virtual devices competing | Detect `com.bitgapp.eqmac.driver` at startup and say so; testing needs eqMac disabled |
| Clock drift between virtual and hardware device | Dropouts or growing latency | Port eqMac's offset + safety-margin approach rather than reinventing it |
| Driver install needs admin + `killall coreaudiod` | All audio drops ~1s | Only on install/uninstall; warn first |
| Unsigned HAL bundles do not load on Apple Silicon | Driver silently absent | Ad-hoc sign locally; Developer ID to distribute |
| Taps may deliver attenuated audio | EQ operating on a pre-scaled signal | Known Apple forum issue — measure before trusting |
| Not every process can be tapped | Some audio has no fader | Those still reach the EQ via the driver; list only what we can control |
| Real-time callback violations | Audible dropouts | No allocation/locks in callbacks; atomics for parameters |
| TCC consent refused | No App Mixer | Degrade to unavailable with a stated reason, as the display rows already do |

---

## 7. Open questions

1. **Disable eqMac while developing?** Both want to be the default output; coexisting means
   proxying theirs, which doubles latency and is fragile. Recommend disabling it.
2. **Ship the driver, or start with the mixer?** Phase D needs no driver and no admin
   password — process taps alone give per-app volume. The driver is what fixes the Dell.
3. **Band count.** Ten, per the reference. eqMac also offers 31-band in Advanced.
4. **Distribution.** A HAL plug-in in `/Library` needs a real installer and Developer ID
   signing for anyone but you. Ad-hoc is fine for this machine.

---

## 8. Recommendation

Order: **A → B → C → D → E**, i.e. EQ first, then the driver and routing, then the mixer.

The EQ is now low-risk — Apple's unit does the DSP and eqMac proves the configuration. That
makes Phase A a fast, verifiable win. Phases B and C carry the real difficulty (install
mechanics and clock drift) and deliver the thing you actually asked for: a volume slider that
works on the Dell. The App Mixer is independent of all three and can slot in whenever, since
process taps do not need the driver at all.
