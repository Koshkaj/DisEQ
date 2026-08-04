//! Display configuration transactions.
//!
//! Every mutation is batched into one `CGBeginDisplayConfiguration` /
//! `CGCompleteDisplayConfiguration` pair and run under a timeout, because
//! completing a configuration blocks on WindowServer IPC and can hang.

use std::time::Duration;

use objc2_core_foundation::{CFArray, CFRetained};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayMirrorOfDisplay, CGConfigureDisplayOrigin, CGConfigureDisplayWithDisplayMode,
    CGConfigureOption, CGDisplayConfigRef, CGDisplayMode, CGError, CGGetActiveDisplayList,
};

use crate::display::{self, DisplayId, DisplayMode};
use crate::timeout::run_with_timeout;

/// Ceiling for a whole transaction. Ten seconds is far beyond a healthy
/// reconfiguration and short enough that a wedged WindowServer is survivable.
const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(10);

/// `kCGNullDirectDisplay` — clears a mirror relationship.
const NULL_DISPLAY: u32 = 0;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Applies only while this app runs; reverted on exit.
    AppOnly,
    /// Applies until logout.
    Session,
    /// Persists across reboots.
    Permanent,
}

impl Scope {
    fn as_option(self) -> CGConfigureOption {
        match self {
            Scope::AppOnly => CGConfigureOption::ForAppOnly,
            Scope::Session => CGConfigureOption::ForSession,
            Scope::Permanent => CGConfigureOption::Permanently,
        }
    }
}

/// Makes `target` mirror `source`.
pub fn set_mirror(target: DisplayId, source: DisplayId, scope: Scope) -> bool {
    if target == source {
        return false;
    }
    transaction(scope, move |config| unsafe {
        CGConfigureDisplayMirrorOfDisplay(config, target.0, source.0)
    })
}

/// Stops `target` mirroring anything.
pub fn clear_mirror(target: DisplayId, scope: Scope) -> bool {
    transaction(scope, move |config| unsafe {
        CGConfigureDisplayMirrorOfDisplay(config, target.0, NULL_DISPLAY)
    })
}

/// Moves `display` to `(x, y)` in the global desktop layout.
pub fn set_origin(display: DisplayId, x: i32, y: i32, scope: Scope) -> bool {
    transaction(scope, move |config| unsafe {
        CGConfigureDisplayOrigin(config, display.0, x, y)
    })
}

/// Switches `display` to the mode with `io_mode_id`.
///
/// The mode is looked up on the worker thread rather than passed in, because
/// `CGDisplayMode` is a CoreFoundation object and moving one across threads
/// would mean sending a raw pointer.
pub fn set_mode(display: DisplayId, io_mode_id: i32, scope: Scope) -> bool {
    let option = scope.as_option();
    run_with_timeout(TRANSACTION_TIMEOUT, false, move || {
        let Some(mode) = find_mode(display, io_mode_id) else {
            return false;
        };

        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        if unsafe { CGBeginDisplayConfiguration(&mut config) } != CGError::Success {
            return false;
        }
        let applied =
            unsafe { CGConfigureDisplayWithDisplayMode(config, display.0, Some(&mode), None) };
        if applied != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        if unsafe { CGCompleteDisplayConfiguration(config, option) } != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        true
    })
}

fn find_mode(display: DisplayId, io_mode_id: i32) -> Option<CFRetained<CGDisplayMode>> {
    let options = display::mode_query_options();
    let array =
        unsafe { objc2_core_graphics::CGDisplayCopyAllDisplayModes(display.0, Some(&options)) }?;
    let array: CFRetained<CFArray<CGDisplayMode>> = unsafe { CFRetained::cast_unchecked(array) };
    array
        .iter()
        .find(|mode| CGDisplayMode::io_display_mode_id(Some(mode)) == io_mode_id)
}

/// Makes `display` the main display by moving it to the layout origin and
/// shifting every other display by the same delta, preserving their relative
/// positions.
pub fn set_main_display(display: DisplayId, scope: Scope) -> bool {
    let bounds = display::snapshot(display);
    let (dx, dy) = (-bounds.origin.0 as i32, -bounds.origin.1 as i32);
    if dx == 0 && dy == 0 {
        return true;
    }

    let others: Vec<(DisplayId, i32, i32)> = active_displays()
        .into_iter()
        .filter(|id| *id != display)
        .map(|id| {
            let snapshot = display::snapshot(id);
            (
                id,
                snapshot.origin.0 as i32 + dx,
                snapshot.origin.1 as i32 + dy,
            )
        })
        .collect();

    let option = scope.as_option();
    run_with_timeout(TRANSACTION_TIMEOUT, false, move || {
        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        if unsafe { CGBeginDisplayConfiguration(&mut config) } != CGError::Success {
            return false;
        }
        let mut ok =
            unsafe { CGConfigureDisplayOrigin(config, display.0, 0, 0) } == CGError::Success;
        for (id, x, y) in others {
            ok &= unsafe { CGConfigureDisplayOrigin(config, id.0, x, y) } == CGError::Success;
        }
        if !ok {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        if unsafe { CGCompleteDisplayConfiguration(config, option) } != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        true
    })
}

pub fn active_displays() -> Vec<DisplayId> {
    let mut ids = [0u32; 16];
    let mut count: u32 = 0;
    let err = unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
    if err != CGError::Success {
        return Vec::new();
    }
    ids[..count as usize]
        .iter()
        .copied()
        .map(DisplayId)
        .collect()
}

/// Picks the HiDPI (or 1x) twin of the current mode: same point size, opposite
/// backing scale.
pub fn hidpi_twin(display: DisplayId, want_hidpi: bool) -> Option<DisplayMode> {
    let current = display::current_mode(display)?;
    display::modes(display).into_iter().find(|mode| {
        mode.width == current.width
            && mode.height == current.height
            && mode.is_hidpi() == want_hidpi
            && mode.io_mode_id != current.io_mode_id
    })
}

/// Opens a configuration, applies `body`, and completes it — all inside the
/// timeout. A failure anywhere cancels the transaction so no partial layout is
/// committed.
fn transaction<F>(scope: Scope, body: F) -> bool
where
    F: FnOnce(CGDisplayConfigRef) -> CGError + Send + 'static,
{
    let option = scope.as_option();
    run_with_timeout(TRANSACTION_TIMEOUT, false, move || {
        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        if unsafe { CGBeginDisplayConfiguration(&mut config) } != CGError::Success {
            return false;
        }

        if body(config) != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }

        if unsafe { CGCompleteDisplayConfiguration(config, option) } != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        true
    })
}
