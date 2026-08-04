# DisEQ development handoff

Last updated: 2026-08-04 (Asia/Tbilisi)

## Repository state

- Workspace: `/Users/koshka/projects/DisEQ`
- Remote: `git@github.com:Koshkaj/DisEQ.git`
- Branch: `main`, tracking `origin/main`
- Latest committed and pushed revision: `7bb0a8b Initial commit`
- The changes described under **Uncommitted work after the initial commit** are
  intentionally still in the working tree and have not been committed or
  pushed.
- `target/`, `.claude/`, and `references/` are ignored. Build products should
  not be committed.
- Release bundle: `target/DisEQ.app`
- App data: `~/Library/Application Support/DisEQ/settings.json`

Current modified/untracked source files at the time of this handoff:

```text
M  crates/kd-app/Cargo.toml
M  crates/kd-app/src/appkit.rs
M  crates/kd-app/src/main.rs
M  crates/kd-app/src/sound.rs
M  crates/kd-app/src/state.rs
M  crates/kd-app/src/theme.rs
M  crates/kd-app/src/views.rs
M  crates/kd-sys/src/lib.rs
?? crates/kd-sys/src/login_item.rs
?? handoff.md
```

## Work completed in the initial commit

The initial commit contains the app, driver, documentation, build tooling, and
the earlier UX work requested during this session.

### Project and product rename

- Renamed the product from `kooldisound` to `DisEQ` throughout source, bundle
  metadata, driver metadata, documentation, build scripts, binary names,
  identifiers, shared-memory names, and application-support paths.
- Renamed the root project directory to `DisEQ`.
- Configured and pushed the GitHub origin listed above.
- Created a repository-appropriate `.gitignore` and excluded generated build
  products and local-only material.

### Audio routing behavior

- Fixed disabling Audio Enhancement so it does not unexpectedly switch to a
  different hardware output.
- The route now relinquishes the virtual device while preserving the selected
  hardware target as the system output. See the route teardown/preservation
  logic in `crates/kd-app/src/sound.rs` and `crates/kd-audio/src/router.rs`.
- Sound exposes the active hardware output and an output picker. Selecting an
  output updates both direct audio and the enhanced route target.
- Manual preamp can be reset to `0.0 dB` by clicking its numeric readout.

### Widget icon

- Replaced the status item symbol with the supplied root `icon.png`.
- The image is embedded at compile time and processed for menu-bar use in
  `crates/kd-app/src/appkit.rs`.
- Cropping/scaling was adjusted so the artwork fills the status-item area
  instead of appearing as a tiny image inside a large transparent canvas.
- The original source asset is approximately 2 MB and is responsible for most
  of the release binary's constant-data size. A prepared small Retina asset is
  recommended later; see `PERFORMANCE.md`.

### Display cards

- Display cards default to a compact header. Clicking a connected card expands
  it to show brightness and resolution; the power switch remains on the right.
- Card open/closed state persists in `settings.json` using a stable hardware
  identity rather than the transient display ID.
- Expanded cards expose the deeper controls through the centered disclosure
  caret.
- Main display is sorted first.
- A compact white circular marker identifies the main display. It is a fixed
  6-point custom view rather than an SF Symbol, avoiding the symbol's oversized
  intrinsic metrics.
- Night Shift is exposed only on the main display. Night Shift is a macOS-wide
  state, not an independently configurable per-monitor property, so showing it
  on every external display would be misleading.
- HiDPI labeling reflects the mode macOS actually publishes. If an external
  monitor does not expose HiDPI modes, DisEQ does not fabricate them; the
  existing mode/backend availability determines whether HiDPI actions work.

### Sound card

- Sound uses the same compact/expanded card pattern and persists its disclosure
  state.
- The full folded card is clickable; the output/source text remains a separate
  selector when expanded.
- The sound card does not have a redundant power toggle.
- Clicking the current source opens the physical output picker.

### Primary-display ordering and marker

- `DisplayCatalog` sorts the main display first, then built-in status and ID.
- The UI adds a white circular main-display marker with a tooltip and
  accessibility label.
- The marker was iterated from an SF Symbol to a true fixed-size dot because
  the symbol remained visually too thick even after changing its nominal size.

### Performance work and documentation

- Profiled the release app on an Apple M1 Pro, 16 GB, macOS 26.6.
- Wrote the complete measurements and optimization roadmap in
  `PERFORMANCE.md`.
- Measured baseline highlights:
  - 14–16 MB idle physical footprint.
  - 21 MB peak physical footprint during launch.
  - 0.0% reported idle CPU; approximately 0.01 seconds added during a
    30-second idle soak.
  - Six idle threads.
  - Approximately 11 MB in live malloc allocations.
  - Approximately 2.9 MB release app/binary, with about 2 MB attributable to
    the embedded source icon.
  - DDC/backend discovery around 1.02 seconds.
  - Warm display catalog load around 60–70 ms.
  - Warm audio output enumeration around 120–130 ms.
- Highest-priority future optimizations documented there:
  1. Update meter labels in place rather than rebuilding the entire panel.
  2. Move DDC discovery off the launch/main thread.
  3. Replace the permanent installed-driver polling timer with Core Audio
     property listeners.
  4. Cache output-device descriptions.
  5. Embed a prepared menu-bar asset.
  6. Profile the real renamed driver in active audio scenarios.

### Initial repository publication

- Added licensing, attribution, technical documentation, plans, driver tooling,
  app bundle tooling, examples, probes, tests, and README material to the
  initial commit.
- Created commit `7bb0a8b` and pushed it to `origin/main`.

## Uncommitted work after the initial commit

These changes are present in the working tree and need a new commit when the UI
has received final user approval.

### Live resolution preview

- Resolution text now changes continuously while the slider is dragged, just
  like brightness.
- Applying a display mode is still deferred until mouse-up so WindowServer is
  reconfigured only once per gesture.
- Added `KDDeferredSlider` in `appkit.rs`:
  - Sends ordinary continuous preview actions while tracking.
  - Sends a separate `resolutionCommitted:` action after mouse-up.
- `AppDelegate` caches the `DisplayCatalog` snapshot used to build the visible
  panel so preview labels do not re-enumerate displays for every mouse event.
- Keyboard and accessibility changes commit immediately because they have no
  mouse tracking loop.

### Settings route and footer

- Removed the three-dot footer button.
- Activated the gear button and added a dedicated Settings route.
- The X button still only closes the widget; it does not quit the application.
- Gear and X have tooltips and accessibility labels.
- Settings contains:
  - Launch at Login toggle.
  - Conditional shortcut to Login Items in System Settings when approval is
    required.
  - Reconnect Displays.
  - Restart DisEQ.
  - Quit DisEQ.
- Reconnect result messages appear in Settings when invoked there.
- Restart launches a helper shell that waits for the old PID to exit before
  opening the replacement app. This lets `applicationWillTerminate:` restore
  audio routing and display gamma before the new process starts.

### Launch at Login

- Added `crates/kd-sys/src/login_item.rs` using the public macOS 13+
  `SMAppService.mainAppService` API.
- The implementation dynamically loads ServiceManagement so a missing API
  degrades gracefully.
- Supported states are `NotRegistered`, `Enabled`, `RequiresApproval`,
  `NotFound`, and `Unavailable`.
- Important status bug fixed: a main app that has never registered may report
  Service Management status `notFound` (`3`) but is still registerable. The UI
  previously conflated this with the API being unavailable and disabled the
  switch. Only `Unavailable` now disables it.
- A disposable signed probe verified on this machine that status `3` changes
  to `Enabled` after registration and to `NotRegistered` after unregistering.
  The probe was removed/moved to Trash afterward.
- The real DisEQ Launch at Login toggle was not changed automatically during
  verification. Test it manually from the packaged app if needed; macOS may
  require approval under System Settings > General > Login Items.

### Offline display behavior

- A display that is toggled off immediately returns to its compact state.
- Its persisted open state is removed, so re-enabling it starts folded.
- An offline card is a plain, non-hoverable surface; only its power switch is
  clickable.
- The action handler also refuses stale disclosure clicks, preventing a click
  queued before rebuild from opening a disconnected display.

### Consistent alignment

- Added a shared 20-point leading-icon column (`ROW_ICON_WIDTH`).
- Leading icons in display headers/actions, Sound, presets, outputs, switches,
  and Settings are centered in that fixed-width slot, removing variation from
  SF Symbols' different intrinsic widths.
- Clickable rows now apply padding vertically only; card insets supply the
  horizontal breathing room. This aligns clickable action rows with toggle-only
  rows and fixes the Settings Launch at Login offset.
- Adjusted the preamp readout width after removing clickable-row horizontal
  padding.

### Click reliability and hover rendering

Several iterations isolated two shared problems in the custom clickable-row
implementation:

1. Passive `NSTextField` labels are subclasses of `NSControl`. The original
   broad `NSControl` hit-test exception allowed labels to swallow clicks, so
   clicking text failed while nearby padding worked.
2. Custom rows manually dispatched their action from `mouseDown:` and often
   triggered an immediate full panel rebuild while the sender was still inside
   its mouse handler. Hover code also repeatedly changed/drew overlapping
   translucent layers inside the active vibrancy hierarchy.

The final implementation replaces those mechanics:

- `KDRow` now subclasses `NSButton`, using AppKit's standard press, tracking,
  mouse-up, enabled-state, and action-delivery lifecycle.
- The row only overrides hit-testing to preserve genuinely interactive nested
  controls (`NSButton`, `NSSlider`, `NSSwitch`, or another `KDRow`). Passive
  text and image views resolve to the row button.
- Folded display/Sound surfaces can highlight as a whole. Expanded cards do not
  receive whole-card hover.
- The current Sound source uses `cursor_clickable_row`: pointing-hand cursor,
  no local highlight.
- Highlighted rows own one wrapper layer. Hover changes only that wrapper's
  background color inside a `CATransaction` with implicit actions disabled.
  It never redraws or mutates the card, content, or vibrancy layers.
- Tracking uses one `NSTrackingArea` with `InVisibleRect`; it is not destroyed
  and recreated during hover.
- All custom clickable rows/surfaces expose a pointing-hand cursor.
- Disabled action rows now truly disable the row itself because `KDRow` is an
  `NSControl`, in addition to greying the subtree.

### Interaction regression probe

- Added a release-safe diagnostic mode:

  ```sh
  target/DisEQ.app/Contents/MacOS/DisEQ --interaction-self-test
  ```

- It runs on the actual AppKit main thread and verifies:
  - Clicking a passive label resolves to the row rather than the label.
  - Standard button actions are delivered.
  - Disabled rows do not deliver actions.
  - A clickable card does not intercept its nested switch.
  - 250 enter/exit hover transitions create no implicit Core Animation keys.
- Latest packaged result: `interaction self-test passed`.

## File-by-file summary of the uncommitted changes

### `crates/kd-app/Cargo.toml`

- Enabled `CATransaction` alongside `CALayer` in `objc2-quartz-core` for
  animation-free hover state changes.

### `crates/kd-app/src/appkit.rs`

- Embedded/prepared status icon support and fixed-size main-display dot.
- Fixed-width leading icon containers.
- Deferred resolution slider and live label helpers.
- Reworked clickable rows as `NSButton` subclasses with stable hit-testing,
  cursors, hover tracking, standard action delivery, and disabled behavior.
- Added the interaction self-test implementation.
- Added button tooltips/accessibility descriptions.
- Added axis-specific row padding.

### `crates/kd-app/src/main.rs`

- Settings, login-item, Login Items settings, restart, and Settings notice
  actions.
- Live resolution preview and deferred commit actions.
- Cached display catalog for slider preview.
- Offline-card action guards and forced close/persistence cleanup.
- Context-sensitive Reconnect Displays notices.
- `--interaction-self-test` command-line diagnostic entry point.

### `crates/kd-app/src/sound.rs`

- Persistent close helper used when a display is switched off.
- Existing persistent display/Sound card state remains the source of truth.

### `crates/kd-app/src/state.rs`

- Added `Route::Settings` and `settings_notice`.

### `crates/kd-app/src/theme.rs`

- Added shared row-icon width and retained separate card/row corner radii and
  row padding.

### `crates/kd-app/src/views.rs`

- Settings UI and footer changes.
- Fixed-width leading icons across the app.
- Offline card rendering.
- Live resolution preview wiring.
- Folded-only card hover and cursor-only Sound source.
- Shared switch-row action configuration and consistent alignment.

### `crates/kd-sys/src/lib.rs`

- Exports the new `login_item` module.

### `crates/kd-sys/src/login_item.rs`

- New dynamic `SMAppService` wrapper, error extraction, state mapping, System
  Settings shortcut, and status mapping tests.

## Verification completed

Latest successful verification sequence:

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p kd-core --quiet
cargo test -p kd-sys --quiet
cargo test -p kd-app --quiet
cargo test -p kd-audio --lib --quiet
make app-release
target/DisEQ.app/Contents/MacOS/DisEQ --interaction-self-test
codesign --verify --deep --strict --verbose=2 target/DisEQ.app
git diff --check
```

Results:

- Clippy passed with warnings denied.
- `kd-core`: 6 tests passed.
- `kd-sys`: 3 tests passed.
- `kd-app`: 8 tests passed.
- `kd-audio` library: 59 tests passed.
- Total in the latest scoped run: 76 tests passed.
- Release build succeeded.
- Packaged interaction self-test passed.
- Ad-hoc code-signature verification passed and the bundle satisfies its
  designated requirement.
- `git diff --check` passed.

### Known test caveat

`crates/kd-audio/tests/eq_response.rs` previously aborted with:

```text
fatal runtime error: Rust cannot catch foreign exceptions
```

This occurred inside the AVAudio offline integration path while DisEQ and the
virtual HAL drivers were actively loaded/routed. All 59 `kd-audio` library
tests pass. The integration test had passed earlier when that live audio state
was not present. Do not kill the user's running audio process merely to make
this test pass; rerun it later in a deliberately clean Core Audio state.

## Runtime and packaging notes

- The packaged release is ad-hoc signed by `bundle/build_app.sh`.
- Rebuilding replaces `target/DisEQ.app`, but a currently running process still
  has the older executable mapped. Restart DisEQ after building to exercise the
  new code.
- The app restores audio routing and display gamma in
  `applicationWillTerminate:`. Prefer the in-app Restart/Quit actions over
  force-killing it.
- At one diagnostic point both the renamed `DisEQ.driver` and a legacy
  pre-rename driver were loaded. Be cautious when testing the full Core Audio
  integration path.
- Installing/uninstalling the HAL driver restarts `coreaudiod` and interrupts
  system audio; do not do it without explicit intent.

## Recommended next steps

1. Restart the packaged app and perform final manual visual acceptance for
   hover rendering and click behavior across:
   - Folded display cards.
   - Display submenu/action rows.
   - Folded and expanded Sound cards.
   - Sound source selector.
   - Presets and output picker.
   - Settings actions and Launch at Login.
2. If accepted, review `git diff`, add `handoff.md`, and create the next commit.
3. Push `main` to the configured origin only when requested.
4. Later, implement the performance roadmap in `PERFORMANCE.md`, beginning with
   in-place meter updates and asynchronous DDC discovery.

