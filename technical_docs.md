# DisEQ — Technical Reference

Analysis of the BetterDisplay reference UI (`references/2.png`), decomposed into technical
definitions, and the macOS system interfaces required to reproduce each element.

Target environment (this machine): macOS 26.5.2 (Tahoe), arm64 (Apple Silicon), Rust 1.95.

---

## 1. Reference UI decomposition

### 1.1 Container

| Property | Technical definition |
|---|---|
| Trigger | `NSStatusItem` in `NSStatusBar.system`, `button.action` toggles panel |
| Surface | Borderless, non-activating panel anchored below the status item |
| Width | ~400 pt fixed; height content-driven, scroll on overflow |
| Background | Vibrancy/blur (`NSVisualEffectView`, or `CGSSetWindowBackgroundBlurRadius`) |
| Corner radius | ~16 pt, full-bleed rounded clip |
| Level | `NSPopUpMenuWindowLevel` — above normal windows, below the menu bar |
| Dismissal | Click-outside / resign-key / `Esc` |
| Process type | Agent app (`LSUIElement = 1`) — no Dock icon, no main menu |

### 1.2 Structural elements in the screenshot

The panel is a **vertical stack of display cards + one tools card + a footer**.

```
┌─ card: display #1 ────────────────────────┐
│ [icon] Built-in Display          [toggle] │  ← header row
│ Brightness (Combined, Upscaling)     110% │  ← labelled slider
│ Volume                                59% │  ← labelled slider
│ Resolution           1590x1028 ·      92% │  ← labelled slider
│ ─ 12 disclosure rows (chevron `>`)        │  ← push-navigation
│ ─ 6 action/checkbox rows (no chevron)     │  ← immediate toggles
│                    ^                      │  ← collapse caret
├─ card: display #2 ────────────────────────┤
│ [icon] Virtual 16:9              [toggle] │
│ Brightness (Software)               100%  │
│ Resolution           3840x2160 ·     95%  │
│                    v                      │  ← expand caret
├─ card: tools ─────────────────────────────┤
│ Displays And Virtual Screens            > │
│ Groups                                  > │
│ Video Filter Window                     > │
│ System Colors                           > │
│ Check for Updates                         │
└─ footer: [gear] [ellipsis] [x] ───────────┘
```

**Row taxonomy** — five distinct widget types, everything else is a composition:

1. **Header row** — icon + title + `Switch`. Switch = display connected/disconnected.
2. **Slider row** — caption (left) + value readout (right) on line 1, leading glyph +
   track on line 2. Value readout is a *derived* label, not free text.
3. **Disclosure row** — icon + label + `>`. Pushes a sub-view **inside the same panel**.
4. **Action row** — circular glyph + label, no chevron. Immediate toggle or command.
5. **Caret row** — centered `^` / `v`. Expands/collapses the card body.

**Two visual weights of icon** exist and they encode behaviour: square/rounded-rect glyphs
mark navigation rows, circular glyphs mark stateful toggles (`High Resolution (HiDPI)`,
`Display Notch`, `Set as Main Display`, `Brightness Upscaling`, `Auto Brightness`).

### 1.3 Semantic reading of the value readouts

| Readout | Meaning |
|---|---|
| `Brightness (Combined, Upscaling)` `110%` | Single control fusing **hardware** brightness (0–100%) with **software upscaling** above 100% via EDR headroom. The parenthetical names the active backends. |
| `Brightness (Software)` `100%` | Virtual display has no hardware backend → gamma/overlay only. Caption is computed from available backends. |
| `Volume` `59%` | Audio output volume of the device **associated with this display**. |
| `Resolution` `1590x1028 · 92%` | Point (logical) resolution + scale relative to native. Slider indexes a sorted mode list, not a continuous range. |

---

## 2. Display identity & enumeration

| Need | Interface |
|---|---|
| Enumerate | `CGGetOnlineDisplayList` (all attached) / `CGGetActiveDisplayList` (in the desktop layout) |
| Stable ID across replug | `CGDisplayVendorNumber` + `CGDisplayModelNumber` + `CGDisplaySerialNumber` + `CGDisplayUnitNumber` |
| Human name | `NSScreen.localizedName` (public, 10.15+) — preferred. Fallback: IORegistry `DisplayAttributes` → `ProductAttributes` → `ProductName` |
| Built-in? | `CGDisplayIsBuiltin` |
| Active / online | `CGDisplayIsActive`, `CGDisplayIsOnline`, `CGDisplayIsAsleep` |
| Main? | `CGDisplayIsMain` / `CGMainDisplayID` |
| Mirroring | `CGDisplayIsInMirrorSet`, `CGDisplayMirrorsDisplay` |
| Geometry | `CGDisplayBounds`, `CGDisplayRotation` |
| Hotplug / layout change | `CGDisplayRegisterReconfigurationCallback`, or `NSApplicationDidChangeScreenParametersNotification` |

`NSScreen` ↔ `CGDirectDisplayID` bridge: `NSScreen.deviceDescription[@"NSScreenNumber"]`.

**Configuration transactions** — all layout mutations must be batched:

```
CGBeginDisplayConfiguration(&cfg)
  CGConfigureDisplayWithDisplayMode(cfg, display, mode, opts)
  CGConfigureDisplayOrigin(cfg, display, x, y)
  CGConfigureDisplayMirrorOfDisplay(cfg, display, master)
CGCompleteDisplayConfiguration(cfg, kCGConfigurePermanently)
```
Scopes: `kCGConfigureForAppOnly` | `ForSession` | `Permanently`.

---

## 3. The on/off toggle — three distinct mechanisms

This is the core feature and there is **no single public API**. Three implementations exist,
with materially different semantics. Naming them precisely matters:

### 3.1 Soft disable via mirroring (public, reversible) — **recommended default**

Mirror the target display onto another active display. macOS removes it from the desktop
arrangement; windows migrate off it; the panel stays powered but shows a duplicate.

- API: `CGConfigureDisplayMirrorOfDisplay(cfg, target, master)`; undo with `kCGNullDirectDisplay`.
- Public API. Fully reversible. Works on Intel + Apple Silicon.
- Limitation: the panel remains lit (a mirror is still being scanned out). Combine with §3.3
  to kill the backlight on externals.
- This is what open-source SimpleDisplay uses.

### 3.2 Hard disconnect via private CoreGraphics SPI (irreversible risk)

```c
CGError CGSConfigureDisplayEnabled(CGDisplayConfigRef cfg, CGDirectDisplayID d, bool enabled);
```

- Removes the display from the device tree entirely — it disappears from System Information.
- **Known failure mode**: on several configurations the display cannot be re-enabled by the
  same call; it requires a physical hotplug or a reboot (displayplacer#109).
- BetterDisplay ships a working reconnect on Apple Silicon / macOS 13+; the exact reconnect
  path is not published. Treat reconnect as **unverified** until tested on this hardware.
- Must be gated behind an explicit user opt-in + warning.

### 3.3 DDC power mode (external displays only, reversible)

MCCS VCP `0xD6` (Power Mode): `1` = on, `4` = standby, `5` = off.

- Turns the physical panel off. The display stays in the macOS layout (windows do **not**
  migrate). Reversible over the same I2C channel — but many monitors drop DDC while in
  standby, so wake may need the physical button.

### 3.4 Decision matrix

| Mechanism | Reversible | App windows move off | Panel dark | Public API | Works on virtual displays |
|---|---|---|---|---|---|
| Mirror (§3.1) | yes | yes | no | yes | yes |
| `CGSConfigureDisplayEnabled` (§3.2) | **unverified** | yes | yes | no | no (reported failures) |
| DDC `0xD6` (§3.3) | mostly | no | yes | no | no |

Product recommendation: default to §3.1, offer §3.3 as a combined "disconnect + power off"
for DDC-capable externals, and expose §3.2 only behind an advanced toggle.

---

## 4. Brightness

Three stacked backends; the UI fuses them into one 0–200% control (`Combined`).

| Backend | Range | Interface |
|---|---|---|
| Built-in panel | 0–100% | `DisplayServices.framework` (private): `DisplayServicesGetBrightness(CGDirectDisplayID, float*)`, `DisplayServicesSetBrightness(CGDirectDisplayID, float)` — load via `dlopen`/`dlsym` |
| External hardware | 0–100% | DDC/CI VCP `0x10` (luminance), `0x12` (contrast) |
| Software dim | 0–100% below hw floor | Gamma ramp: `CGSetDisplayTransferByTable` / `CGSetDisplayTransferByFormula`; or a black overlay window |
| Software upscale | 100–200% | EDR: `CAMetalLayer` overlay with multiply blending, clamped to `NSScreen.maximumExtendedDynamicRangeColorComponentValue` |

**Tahoe regression (affects this machine):** `CGSetDisplayTransferByTable` is reported broken
on macOS 26.3/26.4 on M5-class hardware (breaking BetterDisplay, MonitorControl, f.lux;
FB18559786 / FB19136488, still open as of Mar 2026). An earlier variant — gamma silently
ignored when "Automatically adjust brightness" is on — was fixed in 26 beta 5.
→ **Do not build software dimming on gamma alone.** Use an overlay window as the primary
implementation and gamma only as an optimisation where it is verified working at runtime.

Legacy `IODisplaySetFloatParameter(kIODisplayBrightnessKey)` is dead on Apple Silicon.

`Auto Brightness` row → `DisplayServicesGetBrightnessAutoAdjustEnabled` /
`DisplayServicesSetBrightnessAutoAdjustEnabled` (ambient light compensation).

---

## 5. DDC/CI transport

Required for every external-display feature (brightness, contrast, volume, input, power).

### Apple Silicon
Private IOKit symbols, present in `IOKit.framework`, resolved with `dlsym`:

```c
IOAVServiceRef IOAVServiceCreateWithService(CFAllocatorRef, io_service_t);
IOReturn IOAVServiceWriteI2C(IOAVServiceRef, uint32_t chip, uint32_t offset, void *buf, uint32_t len);
IOReturn IOAVServiceReadI2C (IOAVServiceRef, uint32_t chip, uint32_t offset, void *buf, uint32_t len);
```

- DDC chip address `0x37`, data offset `0x51`.
- Packet framing (verified working, `crates/kd-sys/src/ddc.rs`):
  - Get VCP: `[0x82, 0x01, vcp, checksum]`, then a ≥50 ms wait, then a 12-byte read.
  - Set VCP: `[0x84, 0x03, vcp, value_hi, value_lo, checksum]`.
  - **Checksum seed is `0x6E` — the 8-bit chip address (`0x37 << 1`) — not `0x6E ^ 0x51`.**
    Both appear in the wild; FreeDisplay uses the latter and MonitorControl the former.
  - Reply: `[src, len, 0x02, result, vcp, type, max_hi, max_lo, cur_hi, cur_lo, chk]`.
    A `len` byte of `0x80` is a **null message**: the display received the packet but
    declined to answer. Treat it as "unsupported", never as data — the trailing bytes are
    stale bus contents and parsing them yields plausible-looking garbage.
- Service discovery: walk the IORegistry for `DCPAVServiceProxy` entries whose `Location`
  is `External`, then probe each with a read to confirm it answers.
- **Correlating a service to a `CGDirectDisplayID` is the subtle part.** `DisplayAttributes`
  does *not* live on the `DCPAVServiceProxy` node, nor anywhere in its parent chain: on
  Apple Silicon it sits on a sibling `AppleCLCD2` under a shared `dispN@...` ancestor. A
  parent-only search — the usual approach — finds nothing and silently degrades to matching
  displays by enumeration order. The working method is to climb ancestors one level at a
  time and search each ancestor's *descendants*, taking the first hit so the nearest (i.e.
  correct) display wins. On the reference hardware the match was **9 levels up**.
- With that node found, `ProductAttributes` gives `LegacyManufacturerID`, `ProductID` and
  `SerialNumber`, which matched `CGDisplayVendorNumber` / `CGDisplayModelNumber` /
  `CGDisplaySerialNumber` exactly. Matching on all three (rather than vendor+product alone)
  removes the ambiguity that makes positional fallback necessary.
- Reference implementations: `MonitorControl/Support/Arm64DDC.swift`, `waydabber/AppleSiliconDDC`.

**Field note — not every display answers.** A Dell U2720Q on the development machine
completes the I2C handshake and returns a well-formed null message to *every* VCP code. A
brute-force sweep of all 256 checksum seeds produced no valid reply, confirming the framing
is correct and the display is refusing DDC/CI (typically an OSD setting: Menu → Others →
DDC/CI). Any DDC-backed row must therefore degrade gracefully rather than assume support.

### Intel
`IOFramebufferPortFromCGDisplayID` (private, CoreDisplay) → `IOI2CInterfaceOpen` /
`IOI2CSendRequest` on the framebuffer service. Out of scope for v1 on this hardware.

### VCP codes used by the reference UI

| Code | Function | Reference UI row |
|---|---|---|
| `0x10` | Luminance | Brightness |
| `0x12` | Contrast | Image Adjustments |
| `0x16/0x18/0x1A` | Red/Green/Blue gain | Image Adjustments |
| `0x14` | Colour preset | Color Mode |
| `0x60` | Input source | Device Control |
| `0x62` | Speaker volume | Volume |
| `0x8D` | Audio mute | Volume (mute glyph) |
| `0xD6` | Power mode | on/off toggle (§3.3) |
| `0x04` | Restore factory defaults | Device Control |
| `0xDF` | MCCS version | capability probe |

Capability discovery: DDC capabilities string (`0xF3` request) — parse to decide which rows
to show per display. Absent/garbled capabilities ⇒ hide DDC-dependent rows.

---

## 6. Resolution, refresh rate, scaling

**Native resolution has two traps.** `CGDisplayPixelsWide/High` reports the *current* mode,
not the panel. And the largest mode by pixel area is not native either: scaled HiDPI modes
render into a backing store larger than the panel and macOS downsamples — a 4K panel offers
3360x1890@2x, i.e. a 6720x3780 backing store. Only 1x modes (`pixelWidth == width`) describe
real hardware, so native = the largest of those.

| Need | Interface |
|---|---|
| Mode list | `CGDisplayCopyAllDisplayModes(display, opts)` with `kCGDisplayShowDuplicateLowResolutionModes = kCFBooleanTrue` — without this, HiDPI/1x duplicates are hidden |
| Mode fields | `CGDisplayModeGetWidth/GetHeight` (points), `GetPixelWidth/GetPixelHeight` (backing pixels), `GetRefreshRate`, `GetIOFlags`, `GetIODisplayModeID` |
| HiDPI detection | `pixelWidth > width` ⇒ 2x backing store |
| Apply | `CGConfigureDisplayWithDisplayMode` inside a transaction |
| Usable-mode filter | `CGDisplayModeIsUsableForDesktopGUI` |

**Slider semantics:** filter modes to those matching the display's aspect ratio, sort by
point width, and map the slider to the *index*. The `92%` readout = point width ÷ native
point width. `1590x1028` is a non-native scaled mode of exactly this kind.

**Arbitrary resolutions** (the ones macOS refuses to offer) are not achievable through
`CGDisplayMode` alone. BetterDisplay's approach: create a `CGVirtualDisplay` at the desired
mode and mirror the physical display to it. The EDID-override route
(`/Library/Displays/Contents/Resources/Overrides`) requires SIP disabled — reject it.

`Refresh Rate` row = same mode list grouped by resolution, choosing among refresh rates.
On ProMotion built-ins the rate is adaptive and the list is short/synthetic.

---

## 7. Virtual displays

The `Virtual 16:9` card is a `CGVirtualDisplay` instance — private CoreGraphics
Objective-C classes:

```
CGVirtualDisplay, CGVirtualDisplayDescriptor, CGVirtualDisplaySettings, CGVirtualDisplayMode
```

- Instantiated via ObjC runtime (`objc2` / `msg_send!`) since there are no headers.
- Activation on recent macOS also involves SkyLight's `SLSConfigureDisplayEnabled`.
- Reference implementations: BetterDummy, DeskPad, FreeDisplay, force-hidpi, opendisplay.
- Header reference: `w0lfschild/macOS_headers` → `CoreGraphics/CGVirtualDisplay.h`.
- Same private API DisplayLink ships against, so it is comparatively unlikely to vanish —
  but it *has* changed shape between releases (notably macOS 14).

Uses: HiDPI forcing, arbitrary resolutions, headless/dummy screens.

---

## 8. Row-by-row API mapping

| Reference row | Mechanism | Risk |
|---|---|---|
| Header toggle | §3 (mirror / `CGSConfigureDisplayEnabled` / DDC `0xD6`) | 🟡/🔴 |
| Brightness slider | `DisplayServicesSetBrightness` \| DDC `0x10` \| EDR overlay | 🟡 |
| Volume slider | CoreAudio `kAudioHardwareServiceDeviceProperty_VirtualMainVolume` \| DDC `0x62` | 🟢/🟡 |
| Resolution slider | `CGDisplayCopyAllDisplayModes` + config transaction | 🟢 |
| Display Mode | extend / mirror / main / off — config transaction | 🟢 |
| Refresh Rate | mode subset by `CGDisplayModeGetRefreshRate` | 🟢 |
| Color Mode | ColorSync `ColorSyncDeviceSetCustomProfiles`; bit depth via mode flags | 🟢 |
| Mirror Display | `CGConfigureDisplayMirrorOfDisplay` | 🟢 |
| Stream Display | ScreenCaptureKit `SCStream` + `SCContentFilter(display:)` → window | 🟢 (needs Screen Recording consent) |
| Picture in Picture | same capture into a floating always-on-top panel | 🟢 |
| Move Display | `CGConfigureDisplayOrigin` (all displays repositioned in one transaction) | 🟢 |
| Screen Rotation | `IOServiceRequestProbe(svc, kIOFBSetTransform \| (code << 16))` — **returns `0x10000003` on Apple Silicon**; fallback is a rotated virtual-display composite | 🔴 |
| Image Adjustments | gamma (`CGSetDisplayTransferByFormula`) + DDC `0x16/0x18/0x1A` | 🟡 (Tahoe gamma regression) |
| Device Control | DDC `0x60` input, `0xD6` power, `0x04` reset | 🟡 |
| Apple Display Preset | private `CoreDisplay` reference-mode/preset APIs (XDR panels) | 🔴 |
| Configuration Protection | app-level: snapshot desired config, restore on `CGDisplayRegisterReconfigurationCallback` | 🟢 |
| Manage Display | app-level per-display preferences | 🟢 |
| High Resolution (HiDPI) | pick the mode whose `pixelWidth == 2 × width`; force via mirrored virtual display | 🟡 |
| Display Notch | select a mode excluding the safe area; read `NSScreen.safeAreaInsets` / `auxiliaryTopLeftArea` | 🟡 |
| Set as Main Display | config transaction moving target origin to `(0,0)`, others offset | 🟢 |
| Brightness Upscaling | EDR overlay layer | 🟡 |
| Auto Brightness | `DisplayServicesSetBrightnessAutoAdjustEnabled` | 🟡 |
| Tools ▸ Displays And Virtual Screens | virtual display manager (§7) | 🟡 |
| Tools ▸ Video Filter Window | overlay window with a Metal filter | 🟢 |
| Tools ▸ System Colors | ColorSync profile switcher | 🟢 |

🟢 public API · 🟡 private but widely deployed · 🔴 broken or unproven on this hardware

---

## 9. Sound section (deferred — greyed out in v1)

Reserved surface; nothing wired. Planned mechanisms, all public CoreAudio:

- Device enumeration — `kAudioHardwarePropertyDevices`, `kAudioObjectPropertyName`.
- Default output/input — `kAudioHardwarePropertyDefaultOutputDevice` /
  `DefaultInputDevice` / `DefaultSystemOutputDevice`.
- Volume / mute — `kAudioHardwareServiceDeviceProperty_VirtualMainVolume`,
  `kAudioDevicePropertyMute`.
- Live change notifications — `AudioObjectAddPropertyListenerBlock`.
- Per-app volume and audio capture require a virtual audio driver
  (`AudioServerPlugIn`, e.g. the BlackHole model) — separate, much larger project. Out of scope.

v1 renders the section header + rows at reduced opacity with pointer events disabled.

---

## 10. Rust ↔ macOS binding strategy

| Layer | Crate |
|---|---|
| UI — AppKit directly, no GUI framework | `objc2`, `objc2-app-kit`, `objc2-foundation` 0.3.x |
| CoreGraphics display APIs | `objc2-core-graphics` 0.3.x |
| CoreAudio | `objc2-core-audio` 0.3.x |
| IOKit, DisplayServices, CG/SkyLight SPI | hand-written `extern "C"` declarations + runtime `dlopen`/`dlsym` |

The ObjC-interop cost is unavoidable here — `NSStatusItem`, `NSScreen`,
`DisplayServicesSetBrightness` and `IOAVService*` all require it regardless of UI choice.
Given that, a UI framework buys nothing and costs native fidelity, so the UI is written
against AppKit directly. Rejected alternatives are recorded in `plan.md` §3.

**Rule for every private symbol:** resolve at runtime, never link. A missing symbol must
degrade the corresponding feature to unavailable — never abort startup. This is the single
most important structural decision, because every private API listed here can disappear in a
macOS point release.

---

## 11. AppKit UI construction

Everything is programmatic — no `.xib`, no storyboard.

### 11.1 App lifecycle
- `NSApplication::sharedApplication(mtm)`, delegate defined with `objc2::define_class!`,
  then `app.run()`.
- Agent behaviour: `LSUIElement = 1` in `Info.plist` (equivalently
  `setActivationPolicy(NSApplicationActivationPolicy::Accessory)`).
- Every AppKit type needs a `MainThreadMarker`; all UI work is main-thread only. Background
  work (DDC I2C, which is slow and blocking) must hop threads and post results back.

### 11.2 Status item
```
NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength)
item.button().setImage(NSImage::imageWithSystemSymbolName(...))   // SF Symbol
item.button().setTarget(&handler); item.button().setAction(sel!(togglePanel:))
```
The status item must be retained for the app's lifetime or it vanishes from the menu bar.

### 11.3 The panel
The reference UI is a **detached rounded panel with a gap below the menu bar and no arrow**
⇒ `NSPanel`, not `NSPopover`.

- `NSPanel` with `NSWindowStyleMask::NonactivatingPanel | Borderless` — opens without
  stealing focus from the frontmost app.
- `setLevel(NSPopUpMenuWindowLevel)`, `setOpaque(false)`, `setBackgroundColor(clear)`,
  `setHidesOnDeactivate(true)`, `collectionBehavior` = `CanJoinAllSpaces | Transient`.
- Vibrancy: `NSVisualEffectView` as content view, `material = .hudWindow` (or `.popover`),
  `blendingMode = .behindWindow`, `state = .active`, `wantsLayer = true`,
  `layer.cornerRadius = 16`, `layer.masksToBounds = true`.
- Anchoring: `statusItem.button().window().frame()` → convert to screen coords, centre the
  panel under it, clamp to `NSScreen.visibleFrame`.
- Dismissal: `NSEvent::addGlobalMonitorForEventsMatchingMask(LeftMouseDown | RightMouseDown)`
  + window `resignKey` + local `Esc` monitor.

`NSPopover` with `behavior = .transient` is the shortcut — free anchoring, dismissal and
animation — but it draws an arrow and cannot reproduce the detached look. Fallback only.

### 11.4 Widget mapping (from §1.2)

| Row type | AppKit |
|---|---|
| Header row | `NSStackView` (H): `NSImageView` + `NSTextField` (label) + `NSSwitch` |
| Slider row | `NSStackView` (V): caption/value `NSTextField` pair + `NSSlider` (`isContinuous`) |
| Disclosure row | `NSButton` (borderless) containing icon + label + chevron `NSImageView` |
| Action row | `NSButton` (borderless), toggle state reflected in the circular glyph |
| Caret row | `NSButton` (borderless), animates card body height |
| Card | `NSBox`/`NSView` + `wantsLayer`, `cornerRadius ≈ 10`, subtle fill |
| Root | `NSScrollView` → `NSStackView` (V, `spacing`, `edgeInsets`) |

- Layout: `NSStackView` everywhere so explicit `NSLayoutConstraint` is needed only for
  widths and slider hugging. This is the main lever against Auto-Layout verbosity in Rust.
- Icons: `NSImage::imageWithSystemSymbolName` (SF Symbols) — matches the reference glyphs.
- Callbacks: target/action needs a real ObjC class → one `define_class!` controller per view
  with `#[unsafe(method(...))]` selectors; state lives in Rust `Ivar`s.
- `>` navigation: swap views inside the panel's container with a Core Animation slide; the
  panel animates its own height via `setFrame_display_animate`.

### 11.5 Consequences
- No hot reload. UI iteration is compile-cycle bound — budget for it.
- Sliders fire continuously; DDC writes must be coalesced/debounced (~50 ms) or the monitor
  will be flooded and stall.

---

## 12. Packaging & permissions

- Must ship as an `.app` bundle (status items require a bundle); plain `cargo run` will not
  behave correctly.
- `Info.plist`: `LSUIElement=1`, `CFBundleIdentifier`, `LSMinimumSystemVersion`,
  `NSHighResolutionCapable=true`.
- **App Sandbox is impossible** — private frameworks and IOKit are unreachable inside it.
  Distribution is outside the Mac App Store; ad-hoc signing locally, Developer ID +
  notarisation for release.
- Permissions: none for v1. Screen Recording consent needed only if Stream/PiP is added;
  Accessibility only if brightness-key interception is added.

---

## Sources

- [objc2](https://github.com/madsmtm/objc2) · [objc2-app-kit docs](https://docs.rs/objc2-app-kit/latest/objc2_app_kit/) · [NSStatusBar](https://developer.apple.com/documentation/appkit/nsstatusbar)
- [BetterDisplay](https://github.com/waydabber/BetterDisplay) · [AppleSiliconDDC](https://github.com/waydabber/AppleSiliconDDC)
- [MonitorControl Arm64DDC.swift](https://github.com/MonitorControl/MonitorControl/blob/main/MonitorControl/Support/Arm64DDC.swift) · [The journey to controlling external monitors on M1 Macs](https://alinpanaitiu.com/blog/journey-to-ddc-on-m1-macs/)
- [FreeDisplay](https://github.com/huberdf/FreeDisplay) · [SimpleDisplay](https://simpledisplay.app/) · [DisplayDeck](https://github.com/oabdrabo/DisplayDeck)
- [displayplacer#109 — disabled screen can't be enabled again](https://github.com/jakehilborn/displayplacer/issues/109)
- [CGVirtualDisplay.h header dump](https://github.com/w0lfschild/macOS_headers/blob/master/macOS/Frameworks/CoreGraphics/1336/CGVirtualDisplay.h) · [force-hidpi](https://github.com/sammcj/force-hidpi)
- [Apple DevForums — CGSetDisplayTransferByTable broken on Tahoe](https://developer.apple.com/forums/thread/795074) · [fb-rotate](https://github.com/thiscodedbox/fb-rotate) · [rotation on Apple Silicon](https://developer.apple.com/forums/thread/692575)
