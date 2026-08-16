# DisEQ — Implementation Plan

A macOS status-bar widget in Rust, UI written directly against AppKit via `objc2`, with two
sections: **Displays** (functional) and **Sound** (greyed placeholder).

This file covers *what we build, in what order*.

---

## 1. Scope

### v1 — in scope
- Status-bar agent app, click to open a blurred anchored panel.
- **Displays** section: one card per attached display.
  - header: icon + name + on/off toggle
  - brightness slider (built-in + DDC externals)
  - resolution slider
  - collapse/expand
- **Sound** section: rendered, disabled, non-interactive.
- Live reaction to display hotplug and external configuration changes.

### v1 — explicitly out of scope
Screen rotation (broken on Apple Silicon), XDR/Apple display presets, streaming/PiP,
video filters, EDID overrides, virtual displays, per-app audio.
Volume-in-display-card ships in Phase 5, not v1.

---

## 2. Architecture

```
DisEQ/
├── Cargo.toml                 # workspace
├── plan.md
├── technical_docs.md
├── references/2.png
├── bundle/
│   ├── Info.plist
│   └── build_app.sh           # cargo build → .app → ad-hoc codesign
└── crates/
    ├── kd-sys/                # ALL unsafe FFI. No other crate contains `unsafe extern`.
    │   ├── dylib.rs           #   dlopen/dlsym symbol loader, Option<fn> per symbol
    │   ├── display.rs         #   CoreGraphics enumeration, modes, snapshots
    │   ├── config.rs          #   configuration transactions, timeout-wrapped
    │   ├── timeout.rs         #   ceiling for blocking WindowServer IPC
    │   ├── iokit.rs           #   registry iteration, ancestry search
    │   ├── ddc.rs             #   IOAVService discovery + I2C + MCCS framing
    │   ├── cg_private.rs      #   CGSConfigureDisplayEnabled et al.      (Phase 2)
    │   ├── display_services.rs#   DisplayServicesGet/SetBrightness       (Phase 3)
    │   └── audio.rs           #   CoreAudio                              (Phase 5)
    ├── kd-core/               # Safe domain layer. Zero UI.
    │   ├── display.rs         #   Display, DisplayCatalog, mode filtering
    │   ├── ddc.rs             #   DdcService worker thread
    │   ├── brightness.rs      #   backend selection + combined 0..2.0 model (Phase 3)
    │   ├── power.rs           #   the three on/off strategies (§3)         (Phase 2)
    │   └── watch.rs           #   reconfiguration callback → event stream  (Phase 1+)
    └── kd-app/                # AppKit binary (objc2)
        ├── main.rs            #   NSApplication + delegate, status item, wiring
        ├── appkit.rs          #   thin local wrappers over the AppKit classes we use
        │                      #   (see §3.2) — labels, stacks, cards, constraints
        ├── panel.rs           #   NSPanel + NSVisualEffectView, anchoring, dismissal
        ├── theme.rs           #   metrics, materials, colours
        └── views.rs           #   root, display cards, row widgets, sound section
```

**Invariants**
1. `kd-sys` is the only crate with `unsafe extern` FFI. Everything above it returns `Result`.
   (`kd-app` still uses `unsafe` for ObjC message sends — that is inherent to AppKit.)
2. Every private symbol is resolved with `dlsym` at startup into an `Option<fn>`. A missing
   symbol disables one capability; it never panics and never blocks launch.
3. `kd-core` is testable without a UI and without a display attached (behind traits + a
   fake backend).
4. All AppKit work on the main thread. DDC I2C is slow and blocking → runs off-thread,
   results posted back via `dispatch_async` to main.
5. Slider callbacks are debounced (~50 ms) before reaching any DDC write.

---

## 3. Key decisions to make before Phase 2

**Which on/off semantics is the default?** Three options, detailed in `technical_docs.md` §3:

| | Reversible | App windows move off it | Panel goes dark | API |
|---|---|---|---|---|
| **A. Mirror trick** | yes | yes | no | public |
| **B. `CGSConfigureDisplayEnabled`** | unverified — may need hotplug/reboot | yes | yes | private |
| **C. DDC power `0xD6`** | mostly | no | yes | private |

Plan assumes **A as default, C as an add-on for DDC externals, B behind an advanced flag**.
Phase 2 begins with a throwaway probe binary that tests B on this actual machine — if
reconnect works reliably on macOS 26.5.2, B becomes the default and A the fallback.

Open questions for you:
1. Is "monitor still lit but out of the layout" (A) acceptable, or is a dark panel required?
2. Minimum macOS target — 26.x only, or back to 14/15?

### 3.1 UI stack — decided

**AppKit directly via `objc2-app-kit`. No GUI framework.**

Rationale: ObjC interop is unavoidable regardless (`NSStatusItem`, `NSScreen`,
`DisplayServicesSetBrightness`, `IOAVService*`), so a framework adds a layer without
removing the cost. The panel needs a non-activating window at `NSPopUpMenuWindowLevel`
anchored to a status item with behind-window vibrancy — which AppKit gives for free and
every winit-based framework fights.

| Rejected | Why |
|---|---|
| `gpui` 0.2.2 (+ `gpui-component` 0.5.1) | pre-1.0 churn; Zed aesthetic ≠ macOS; still hand-roll `NSStatusItem`; zed#42821 freezes the app when a `PopUp` window spawns a window; long compiles |
| Tauri v2 | ~60–120 MB WKWebView resident for an always-on utility; native controls only CSS-faked; IPC boundary for what is a local, synchronous UI |
| iced 0.14 / egui / slint | winit does not model non-activating panels or status-item anchoring; non-native controls |
| `cacao` | evaluated in detail — see §3.2 |
| Swift/SwiftUI | best technical fit (all reference implementations are Swift, private APIs need zero binding work) — rejected on the Rust requirement, not on merit |

Accepted costs: verbose, manual Auto Layout, no hot reload. Mitigation: `NSStackView`
everywhere; keep all logic in `kd-core` so UI iteration is the only slow loop.

### 3.2 `cacao` — evaluated and rejected

`cacao` is the closest thing to what we want (safe idiomatic Rust wrappers over AppKit), so
it was checked properly rather than dismissed on its crates.io version.

*State (verified Aug 2026):* last commit **2024-12-28**, ~19 months stale; last release
**0.4.0-beta2, Aug 2023**; 50 open issues; not archived; README self-describes as
"experimental… early stages and may have bugs — your usage of it is at your own risk."

*Disqualifiers, in order of weight:*

1. **It does not wrap the widgets this app is made of.** Present: `Button`, `Label`,
   `Switch`, `ScrollView`, `View`, `Autolayout`. **Absent: `NSStatusItem`,
   `NSVisualEffectView`, `NSSlider`, `NSStackView`, `NSPanel`/`NSPopover`** — i.e. the entry
   point, the look, three of the four controls per card, and the layout primitive.
2. **It is built on `objc` 0.2** — unmaintained and known-unsound. Migration issue #28 has
   been open since June 2022. The ecosystem (winit, alacritty, the `cocoa` crate) moved to
   `objc2`; `cocoa` is formally deprecated in `objc2`'s favour.
3. (1) + (2) compose badly: the missing pieces must be written with raw message sends
   anyway, so cacao's entire value proposition evaporates — and those sends would target
   the old runtime rather than `objc2`. Strictly worse than starting on `objc2`.

*Credit where due:* cacao's `LayoutConstraint::activate` API and Rust-struct delegate
pattern are more ergonomic than `objc2`'s `define_class!` boilerplate. Were it maintained
and did it wrap `NSSlider`/`NSStatusItem`, it would be a genuine contender.

*Consequence for us:* no maintained "safe AppKit wrapper on `objc2`" crate exists; the
ecosystem answer is to use `objc2` directly. So `kd-app` grows a thin internal wrapper
module over the ~10 AppKit classes actually touched — cacao's ergonomics, only the parts we
need, on a live runtime. Built once in Phase 0.

---

## 4. Phases

### Phase 0 — Shell (no display logic)
Cargo workspace; `NSApplication` + delegate via `define_class!`; `Info.plist` with
`LSUIElement=1`; `build_app.sh` producing an ad-hoc signed `.app`; `NSStatusItem` with an
SF Symbol; click toggles a non-activating `NSPanel` with an `NSVisualEffectView` content
view, rounded corners, anchored under the status item; global mouse monitor + `resignKey` +
`Esc` dismiss it.

Also in Phase 0: the `appkit/` wrapper module (§3.2), then the five row widgets (`header`,
`slider`, `disclosure`, `action`, `caret`) against dummy data — so Phase 1 is pure
data-binding.

*Done when:* icon in the menu bar, panel opens/closes correctly with vibrancy, no Dock icon,
static mock card matches the reference screenshot.
*Risks:* status item retention; `NSPanel` focus/level behaviour; `define_class!` ergonomics.

### Phase 1 — Read-only Displays section
`kd-sys/display.rs` enumeration; `kd-core::Display` model; hotplug watcher via
`CGDisplayRegisterReconfigurationCallback` feeding GPUI updates; card widgets rendering
name, built-in/external icon, current mode, current brightness — all read-only, sliders
disabled. Sound section rendered greyed.

*Done when:* the panel mirrors real hardware live, and plugging/unplugging updates it.

### Phase 2 — The toggle
Probe binary for strategy B (see §3); implement A + C; per-display strategy selection based
on capability probe; persist and restore the pre-disable arrangement so re-enable returns
displays to their original origins.

*Done when:* toggling a display off removes it from the arrangement and toggling it back on
restores the previous layout exactly.

### Phase 3 — Brightness
`DisplayServices` for built-in; DDC `0x10` for externals (with the IORegistry↔`CGDirectDisplayID`
correlation and retry/backoff); overlay-window software dimming; runtime probe for whether
gamma works on this machine (Tahoe regression) before using it; combined 0–200% slider model.

*Done when:* the slider drives real brightness on both built-in and an external, with the
caption naming the active backend as in the reference UI.

### Phase 4 — Resolution & refresh rate
Mode enumeration with `kCGDisplayShowDuplicateLowResolutionModes`, aspect filtering, index
mapping for the slider, config-transaction application with revert-on-timeout;
`Refresh Rate` and `Display Mode` disclosure sub-views.

*Done when:* resolution changes apply and the percentage readout matches the reference
format (`1590x1028 · 92%`).

### Phase 5 — Volume in the display card
CoreAudio for the display-associated output device; DDC `0x62`/`0x8D` for monitor speakers.
Groundwork for the Sound section.

### Phase 6 — Sound section
Un-grey it. Output/input device pickers, volume, mute, live device-change notifications.
Scope to be defined at that point.

---

## 5. Dependencies

```toml
objc2               = "0.6"   # runtime, define_class!, MainThreadMarker
objc2-app-kit       = "0.3"   # NSStatusItem, NSPanel, NSVisualEffectView, NSStackView, NSSwitch, NSSlider
objc2-foundation    = "0.3"
objc2-core-graphics = "0.3"   # display enumeration, modes, config transactions
objc2-core-audio    = "0.3"   # phase 5
libc                = "0.2"   # dlopen/dlsym
serde, serde_json             # persisted per-display prefs
```

No GUI-framework dependency at all — the tree is `objc2` + `libc` + `serde`.

Private symbols (`CGSConfigureDisplayEnabled`, `DisplayServices*`, `IOAVService*`) are
hand-declared in `kd-sys` and resolved via `dlsym` — never linked.

---

## 6. Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Hard-disconnect not reversible on macOS 26.5.2 | Core feature degraded | Probe in Phase 2; ship mirror strategy as default |
| AppKit-from-Rust verbosity slows UI work | Schedule | `NSStackView` over raw constraints; widgets built once in Phase 0 and reused |
| No hot reload | Slow UI iteration | Keep all logic in `kd-core`; UI layer stays thin and mostly static |
| `objc2` 0.3/0.6 API churn between releases | Build breaks | Pin exact versions; `objc2` is stable and widely used, low risk |
| Private symbols vanish in a macOS update | Feature loss | Runtime resolution + per-capability graceful degradation |
| Tahoe gamma regression | Software dimming broken | Overlay window as primary path; gamma only after a runtime probe |
| DDC quirks per monitor | Flaky external control | Capability probe, retries with backoff, per-model overrides |
| Rotation unavailable on Apple Silicon | Feature cut | Excluded from v1 and stated as such |
| No App Sandbox possible | No Mac App Store | Direct distribution, Developer ID + notarisation |

---

## 6.1 Status

**Phases 0–4 complete.** Builds clean with zero warnings; every backend verified against
real hardware by `make selftest`, which restores whatever it changes.

```
volume        [pass] set 0.70 -> read 0.70      [pass] restored 0.31
DELL U2720Q   backend: Software
              [pass] brightness set 0.40 -> 0.40
              [pass] colour adjustment applied  [pass] colour restored
              [pass] resolution -> 1680x945     [pass] resolution restored
              [pass] disconnect refused on last display (LastActiveDisplay)
```

### Working

| Control | Backend |
|---|---|
| Brightness slider | built-in → `DisplayServices`/`CoreDisplay`; external → DDC `0x10`; else gamma ramp |
| Volume slider + output name | CoreAudio `kAudioHardwareServiceDeviceProperty_VirtualMainVolume` |
| Resolution slider | `CGConfigureDisplayWithDisplayMode`, snapping to real modes |
| Connect toggle | mirror-based soft disable, guarded (see below) |
| High Resolution (HiDPI) | switches to the same point size at the opposite backing scale |
| Set as Main Display | moves the display to the layout origin, shifting the rest |
| Auto Brightness | `CoreDisplay_Display_SetAutoBrightnessIsEnabled` |
| Display Mode / Refresh Rate / Mirror / Move Display | mode + origin + mirror transactions |
| Color Mode / Image Adjustments | gamma ramp, single-owner |
| Device Control | DDC input source and panel power |
| Configuration Protection | pins mode + origin + mirror, restores on reconfiguration |
| Manage Display | identity, backend and DDC match confidence |

### Safety rails

- **Disconnect refuses to remove the last active display.** Doing so would leave a dark
  machine with no interface left to undo it. Verified: returns `LastActiveDisplay`, the
  switch springs back, and the card explains why rather than failing silently.
- **Off means off.** The toggle prefers the hard disconnect (`CGSConfigureDisplayEnabled`),
  which darkens the panel and takes the display out of the layout in one step. When the
  symbol is missing or the call is refused, it falls back to the mirror trick plus DDC
  standby (`0xD6` = 5) — mirroring alone leaves a lit screen showing a duplicate desktop.
  Reconnect via the private call is unverified on this hardware; the failure mode is a
  display that only comes back on a physical replug.
- Every disconnect is recorded in `kd_core::offline` with the snapshot taken beforehand, so
  a hard-disconnected display keeps its card — and its toggle — after it leaves the device
  tree and stops appearing in `CGGetOnlineDisplayList`. Reconnect reverses whichever
  mechanism was used. A display that returns on its own drops out of the registry.
- **The record outlives the process.** It is written to
  `~/Library/Application Support/DisEQ/offline.tsv`, because quitting while a display
  was off used to leave no way back but a physical replug. Ids are reassigned every boot, so
  records carry vendor/model/serial (unit for built-in panels) and are re-keyed onto the new
  id when the panel reappears.
- `CGSGetDisplayList` (SkyLight) lists displays the public API has dropped, which is what
  makes a disabled display addressable at all. `power::known_display_ids()` merges it with
  the public list; `Reconnect Displays` in the status item's right-click menu — and
  `make reconnect` outside the app — re-enables everything it finds offline. A stale id in a
  record falls back to the same sweep.
- Disconnects use session scope, never permanent: one that survived a reboot could leave
  the machine with no usable display at login.
- **A reconnect is refused when there is nothing to reconnect.**
  `CGSConfigureDisplayEnabled` does not fail on a port with nothing plugged into it: it
  succeeds, and the window server invents a display that takes windows and leaves the port
  in a state a real monitor plugged in afterwards comes up "No Signal" on. CoreGraphics
  cannot tell the two apart — a disabled display and an unplugged one are both simply
  missing from `CGGetOnlineDisplayList` — so `kd_sys::panel` reads EDID out of the
  IORegistry instead (`AppleCLCD2` → `DisplayAttributes` → `ProductAttributes`). Only a
  panel on the other end of the cable produces that, and it survives the display being
  disabled, so "switched off" and "unplugged" are finally distinguishable. A record whose
  panel is gone loses its card rather than keeping a toggle that would invent one.
- **The built-in panel stays dark while the lid is shut.** The private enable call sits
  below whatever normally enforces that, so `AppleClamshellState` on `IOPMrootDomain` is
  checked before it, and again after any blind sweep.
- **A monitor plugged back into a port that was switched off comes back on its own.**
  The window server's disable belongs to the port, not to the panel that was on it, so it
  outlives the cable being pulled: switch a display off, unplug it, plug it back in, and it
  stays dark forever. Its record is gone by then — dropped when the panel it described
  disappeared, which is what stops the card offering to invent a monitor — so nothing is
  left to press either. `power::orphaned_panels` names that state (attached, dark, and no
  record explaining why) and a watchdog recovers it. A display the user switched off keeps
  its record and is left alone; only hardware that is dark for no reason anyone chose is
  touched. Each panel is retried once per appearance, and the pass runs on a worker thread
  because the transactions block for as long as the window server takes.
  Polled, not driven by `CGDisplayRegisterReconfigurationCallback`: the display in question
  is one CoreGraphics does not believe exists, so plugging a monitor into that port
  publishes no reconfiguration to wait for. This is the only thing left on that slow poll —
  everything else redraws off the callback, below.
- Where the registry cannot be read at all, every one of those checks reports `Unknown` and
  allows the operation. A machine this code does not recognise keeps its old behaviour
  rather than losing the feature.

### An open panel redraws as soon as it stops being true

Whatever the user does, and from whichever direction. Three things feed that, because no one
of them covers the others:

- `CGDisplayRegisterReconfigurationCallback` for anything CoreGraphics considers a change —
  plug, unplug, mode, arrangement. It fires on a thread of the window server's choosing, so
  the handler only sets an atomic flag; a 150 ms timer on the main thread turns that into a
  rebuild. That also coalesces the burst a single hotplug produces into one redraw.
- A fingerprint of **both** the online display list and the attached panel list, compared on
  that same timer. The layout alone is not enough: a display switched off from the panel has
  already left `CGGetOnlineDisplayList` while its card is still on screen, so pulling its
  cable moves nothing there — the layout has nothing left to lose. Only the panel list still
  holds it, and only the panel list moves when the cable does. Watching the layout alone left
  a card offering to reconnect a monitor that had been unplugged in front of it.
- The 3 s watchdog, for the one case nothing reports at all: a port the window server has
  switched off publishes no reconfiguration when a monitor is plugged into it, because
  CoreGraphics does not believe that display exists. Recovery is its only job now — a display
  it brings back joins the layout, which the timer above sees like any other hotplug.

The two reads cost well under a millisecond together (the IOKit panel walk being the cheaper
of the two, ~65 µs against ~385 µs for `CGGetOnlineDisplayList`) and are only paid while the
panel is on screen. Both lists are sorted before comparison: neither enumeration promises an
order, and one that came back shuffled would read as a change and rebuild the view tree out
from under the pointer several times a second.

`rebuild` is what records the fingerprint and clears the flag, because it is the only thing
that can make them true — and it clears the flag *before* reading the state, never after, so
a reconfiguration landing between the two is redrawn a tick later instead of being dropped.
A rebuild refused mid-press records nothing, leaving the change to be redrawn once the button
is released.

Volume needed nothing: the frame timer already runs continuously while the driver is
installed, and `Route::tick` follows the device's own volume control every frame, so the
menu-bar slider and the volume keys are picked up in ~16 ms.

### The display transactions lie about their own outcome

On this hardware `CGSConfigureDisplayEnabled` reports failure for changes it applies:
a disable returns `kCGErrorFailure` (1001) from `CGCompleteDisplayConfiguration` in under a
millisecond, and an enable does not report back inside ten seconds — while the display list
reflects both within three. Believing the status meant a working disconnect looked like a
failure and fell through to the mirror fallback, and a working reconnect was reported to the
user as refused.

So `power::set_enabled` starts the transaction and then watches
`CGGetOnlineDisplayList`, which is the state everything downstream reads anyway.
`set_enabled_detailed` still reports which step returned what, for probes only.
- Backends are re-resolved whenever the layout changes. Which mechanism drives a display's
  brightness is decided by what answered at probe time, so a display that was off then had
  no backend for the rest of the session — a reconnected monitor came back with a dead
  brightness slider. A reconfiguration callback marks the map stale and the next panel
  rebuild re-probes.
- Brightness and resolution are greyed out on a display that is switched off: there is no
  backlight to set and no mode to pick while it is out of the layout.
- The panel body lives in an `NSScrollView` and grows only to the screen's usable height:
  two unfolded display cards are taller than the screen, and the overflow used to be
  unreachable.
- **The master volume is applied at exactly one stage.** Turning the effects on must not
  change how loud the machine is, and it did — twice over, for two separate reasons. The
  driver applied its volume control to the shared ring the app reads and the app applied it
  again on the way out, so a cubic taper met a linear one and half volume came out at a
  sixteenth. And `Route::start` seeded the playback mixer with the master volume while
  adopting that same volume *from* the hardware, which kept applying it too — so every
  route came up quiet and stayed quiet until the user touched the slider, which
  unknowingly repaired the staging. The driver's control is now a control surface only,
  and `Route::apply_volume` is the single place that decides where the gain lands:
  the hardware's own volume control where there is one, our mixer where there is not.
  `make volume-probe` measures both halves, the second one *as started*.
- Gamma is reset on quit, since a dimmed ramp outlives the process.
- Resolution applies on mouse-up, not during the drag.
- DDC writes are coalesced per display and code, so dragging a slider cannot back up the bus.

### Removed from the panel

Rows with no working backend behind them were dropped rather than shipped as dead ends:
Stream Display and Picture in Picture (ScreenCaptureKit plus a Screen Recording prompt),
Screen Rotation (`IOServiceRequestProbe` returns `0x10000003` on Apple Silicon), Image
Adjustments (read-only over the gamma path — Color Mode already sets it), and Apple Display
Preset (XDR panels only). Their `Submenu` discriminants are left as gaps because the values
are encoded into view tags.

### Not implemented, and why

| Row | Blocker |
|---|---|
| Displays And Virtual Screens | Needs `CGVirtualDisplay`, a private ObjC API that has changed shape between releases |
| Brightness Upscaling | Needs EDR headroom applied display-wide, only reachable through private CoreDisplay preset APIs |
| Display Notch | Needs a mode excluding the menu-bar safe area, which only notched built-in panels publish |
| Groups, Video Filter Window, Check for Updates | App-level features with no system backend to build on |

### Field notes from this hardware

- **`DisplayServices` answers for external displays too**, reporting a plausible value and
  silently ignoring writes. Backend selection has to check `CGDisplayIsBuiltin` *first*;
  trusting the framework picked the wrong backend for the Dell.
- **The gamma ramp works on this machine** (`CGSetDisplayTransferByTable` verified by
  read-back), despite the macOS 26 regression reports. `gamma::verify` measures it rather
  than assuming, because the call returns success even when ignored.
- The Dell U2720Q refuses DDC/CI entirely, so its brightness falls to the gamma path.

## 7. Immediate next steps

1. **Check DDC/CI on the Dell** — Menu → Others → DDC/CI → Enable. If it starts answering,
   its brightness moves from the gamma path to real backlight control with no code change.
2. **Run the mirror probe once a second display is attached** —
   `KD_MIRROR_PROBE=1 make mirror-probe`. It decides whether the mirror strategy survives on
   Apple Silicon; FreeDisplay abandoned `CGConfigureDisplayMirrorOfDisplay` over
   hardware-mirror takeover and cursor stutter. Judge by eye.
3. **Verify the hard disconnect on two displays.** It is now the default; confirm reconnect
   works on this machine, because the failure mode is a display that only comes back on a
   physical replug. The mirror + DDC standby fallback is the safety net if it does not.
4. Answer the remaining question in §3 (min macOS target). Disconnect semantics: decided —
   a dark panel is required, so hard disconnect first, mirror + DDC standby second.
5. The three features blocked on decisions rather than effort: Screen Recording consent for
   Stream/PiP, and accepting `CGVirtualDisplay` for virtual screens.

## 8. Architecture crates

```
crates/kd-sys      audio, brightness, config, ddc, display, dylib, gamma, iokit, panel,
                   power, timeout, watch            — all unsafe FFI lives here
crates/kd-core     ddc (worker thread), display, offline, power, protection, service
crates/kd-app      appkit, main, panel, state, theme, views
```

`kd_sys::panel` answers what CoreGraphics cannot: which panels are physically plugged in,
and whether the lid is shut. `kd_core::power` is the policy over `kd_sys::power` that uses
those answers to decide whether a reconnect is a recovery or an invention.

Probes, all runnable via `make`:
`probe` (displays and modes), `ddc` (read-only VCP), `backends` (resolved backends),
`selftest` (end-to-end, self-restoring), `mirror-probe` (guarded, needs 2 displays),
`reconnect` (re-enables displays left switched off, including by a previous run),
`attach-probe` (CoreGraphics vs the window server vs IOKit, read-only),
`guard-probe` (the reconnect guards, against a scratch home),
`volume-probe` (that the driver hands the app its audio unattenuated).
