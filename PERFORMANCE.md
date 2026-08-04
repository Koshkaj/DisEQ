# DisEQ performance profile

Measured on 2026-08-04 using a release build on an Apple M1 Pro with 16 GB
RAM, running macOS 26.6 (25G72). These numbers are a baseline for this machine,
not universal guarantees.

## Baseline

The release profile already uses full LTO, one code-generation unit, and symbol
stripping.

| Scenario | Result |
| --- | ---: |
| Idle physical footprint | 14–16 MB |
| Peak physical footprint during launch | 21 MB |
| Idle CPU | 0.0% in `top` |
| CPU time added during a 30-second idle soak | 0.01 s |
| Idle threads | 6 |
| Live malloc allocations | 11.0 MB across about 25,000 allocations |
| Release app/binary size | 2.9 MB / 3,005,536 bytes |
| Synchronous backend discovery | 1.02 s |
| Display catalog load, warm | 60–70 ms |
| Display catalog load, cold | 390 ms |
| Audio output enumeration, warm | 120–130 ms |
| Audio output enumeration, cold | 500 ms |

The 30-second soak held a 16 MB footprint at both ends; RSS fell from 48.2 MB
to 45.3 MB. RSS includes shared framework pages, so physical footprint is the
more useful macOS memory-pressure number. `leaks` reported 14.2 KB, all in
system-framework XPC retain cycles visible under the process; it did not
identify an actionable DisEQ-owned leak.

The custom source icon is embedded in the executable. It accounts for roughly
2.0 MB of the 2.18 MB `__TEXT,__const` section. Its decoded menu-bar backing is
small, so this mostly affects binary size and cold I/O rather than the steady
physical footprint.

The new `DisEQ.driver` was not installed during this run; only the legacy
pre-rename driver was present. Therefore these figures cover the app with
enhanced routing unavailable, not the audio engine or its plug-in process.
Installing a HAL plug-in restarts system audio, so that scenario should be
profiled separately and deliberately.

## Findings and implementation order

### 1. Stop rebuilding the whole panel for the level meter

When enhanced routing is active, `SoundService::tick` requests a redraw every
eight 33 ms ticks: about 3.75 complete panel rebuilds per second while the panel
is visible. Every rebuild reloads `DisplayCatalog`; that operation alone costs
60–70 ms warm on this two-display setup, before creating and laying out a new
AppKit view tree. This can consume roughly 225 ms of wall time per second and
creates avoidable allocation churn.

Keep stable references to the level meter and changing labels, then update only
their values. Cache the display catalog for the life of the open panel and
invalidate it on the existing display-reconfiguration callback, a display
action, or an explicit refresh. A full rebuild should be reserved for navigation
and structural state changes.

This is the highest-impact interactive optimization. Validate it with the panel
open and routing active; target less than 2% CPU and no upward allocation trend.

### 2. Move DDC probing off the launch callback

`applicationDidFinishLaunching` currently constructs `Service`, whose backend
probe waits for DDC responses. The read-only backend probe took 1.02 seconds on
this machine. The status icon is created first, but AppKit's main thread cannot
respond normally until probing returns.

Construct the service immediately with safe provisional backends, run DDC
survey/validation on the existing worker, and deliver the result to the main
thread. Rebuild only if the panel is open. The target is a responsive status
item within 200 ms; backend accuracy may settle asynchronously within two
seconds.

### 3. Replace the permanent installed-driver timer with events

`SoundService::wants_ticks` keeps the 33 ms AppKit timer alive whenever the
driver is installed, even when no route or EQ ramp is active. Use Core Audio
property listeners for default-output and volume changes. Keep the fast timer
only for an active ramp or route; a low-frequency fallback poll is acceptable
if a device does not emit reliable events.

This needs measurement after installing `DisEQ.driver`, because the current
idle result does not exercise that branch.

### 4. Cache the output-device descriptions

Describing all Core Audio outputs costs 120–130 ms warm. It currently occurs
only when building the output picker, so it is not an idle problem, but it makes
that screen feel slower and compounds any full rebuild there. Cache the list
and invalidate it from Core Audio's device-list/default-device notifications.

### 5. Embed a prepared menu-bar asset

Pre-crop and downsample `icon.png` into a small Retina menu-bar PNG during asset
preparation, then embed that derived image instead of the 2.0 MB source artwork.
Keep the original as the high-resolution application artwork. This should cut
most of the current 2.9 MB executable without materially changing runtime RAM.

### 6. Profile the real audio path

After installing the renamed driver, record three additional scenarios:

1. driver installed, enhancement off;
2. enhancement on while silent;
3. enhancement on while playing audio, with the panel both closed and open.

Use Instruments' Audio System Trace and Time Profiler, and measure the DisEQ
app plus the HAL plug-in host. Watch callback deadline misses, ring-buffer
realignments, wakeups, app CPU, plug-in CPU, and combined physical footprint.
The real-time callback must remain allocation-free and lock-free.

## Performance budgets

| Scenario | Physical footprint | CPU |
| --- | ---: | ---: |
| App idle, driver unavailable | at most 16 MB | below 0.1% |
| App idle, driver installed, enhancement off | at most 18 MB | below 0.1% |
| Enhancement active, panel closed | at most 25 MB | below 1% when silent |
| Enhancement active, panel open | at most 25 MB | below 2% excluding audio workload |

Also target a responsive menu-bar icon within 200 ms, no main-thread operation
over 16 ms during ordinary interaction, and a release executable near 1 MB once
the oversized embedded widget source is replaced.

## Reproducing the baseline

Build and verify the signed release app:

```sh
make app-release
codesign --verify --deep --strict target/DisEQ.app
```

For a running release process:

```sh
pid=$(pgrep -n -x DisEQ)
footprint "$pid"
vmmap -summary "$pid"
top -pid "$pid" -l 5 -s 2 -stats pid,cpu,mem,threads,time,command
sample "$pid" 5 1 -file /tmp/diseq.sample
heap "$pid"
leaks "$pid"
```

The backend and catalog timings can be repeated with the release examples:

```sh
cargo build --release -p kd-core --examples
/usr/bin/time -p target/release/examples/backends
/usr/bin/time -p target/release/examples/probe
/usr/bin/time -p target/release/examples/audio_probe
```
