//! Connecting and disconnecting displays.
//!
//! There is no public API for this. Three mechanisms exist with materially
//! different semantics, so the caller picks one explicitly rather than getting
//! whichever happened to be implemented.

use std::time::Duration;

use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureOption, CGDisplayConfigRef, CGError,
};

use crate::config::{self, Scope};
use crate::display::{self, DisplayId};
use crate::dylib::Framework;
use crate::timeout::run_with_timeout;

const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(10);
const CORE_GRAPHICS: &str = "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics";
const SKYLIGHT: &str = "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";
/// Enough to cover any Mac's display count with room to spare.
const MAX_DISPLAYS: u32 = 32;

type ConfigureDisplayEnabled = unsafe extern "C" fn(CGDisplayConfigRef, u32, bool) -> CGError;
type GetDisplayList = unsafe extern "C" fn(u32, *mut u32, *mut u32) -> CGError;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Strategy {
    /// Mirror the display onto another, removing it from the desktop layout.
    /// Public API and reversible, but the panel stays lit.
    Mirror,
    /// `CGSConfigureDisplayEnabled` — a true disconnect. The display leaves the
    /// device tree entirely. Reconnect is not guaranteed on every machine.
    HardDisconnect,
}

#[derive(Debug)]
pub enum PowerError {
    /// Refused: this is the only display left with a desktop on it.
    LastActiveDisplay,
    /// Refused: mirroring needs somewhere to mirror to.
    NoMirrorTarget,
    /// The private symbol is not present on this macOS build.
    Unavailable,
    /// The system rejected the change or the transaction timed out.
    Failed,
}

fn configure_display_enabled() -> Option<ConfigureDisplayEnabled> {
    use std::sync::OnceLock;
    static SYMBOL: OnceLock<Option<ConfigureDisplayEnabled>> = OnceLock::new();
    *SYMBOL.get_or_init(|| {
        let framework = Framework::open(CORE_GRAPHICS)?;
        unsafe { framework.symbol("CGSConfigureDisplayEnabled") }
    })
}

pub fn hard_disconnect_available() -> bool {
    configure_display_enabled().is_some()
}

fn get_display_list() -> Option<GetDisplayList> {
    use std::sync::OnceLock;
    static SYMBOL: OnceLock<Option<GetDisplayList>> = OnceLock::new();
    *SYMBOL.get_or_init(|| {
        let framework = Framework::open(SKYLIGHT)?;
        unsafe { framework.symbol("CGSGetDisplayList") }
    })
}

/// Every display the window server knows about, including ones that have been
/// disabled.
///
/// `CGGetOnlineDisplayList` drops a hard-disconnected display entirely, which
/// leaves nothing to re-enable it by. The window server keeps listing it, so
/// this is what makes that disconnect reversible without a replug.
pub fn known_display_ids() -> Vec<DisplayId> {
    let Some(list) = get_display_list() else {
        return display::online_displays();
    };

    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count: u32 = 0;
    let err = unsafe { list(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    if err != CGError::Success {
        return display::online_displays();
    }

    let mut out: Vec<DisplayId> = ids[..count as usize]
        .iter()
        .copied()
        .map(DisplayId)
        .collect();
    // The window server has been seen omitting a display the public list still
    // has, so the two are merged rather than trusted one over the other.
    for id in display::online_displays() {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// Removes `display` from the desktop layout.
///
/// Refuses when it would leave no active display: the user would be left with a
/// dark machine and no interface to undo it with.
pub fn disconnect(display: DisplayId, strategy: Strategy) -> Result<(), PowerError> {
    let active = config::active_displays();
    if active.len() <= 1 {
        return Err(PowerError::LastActiveDisplay);
    }

    match strategy {
        Strategy::Mirror => {
            let target = active
                .iter()
                .copied()
                .find(|id| *id != display)
                .ok_or(PowerError::NoMirrorTarget)?;
            if config::set_mirror(display, target, Scope::Session) {
                Ok(())
            } else {
                Err(PowerError::Failed)
            }
        }
        Strategy::HardDisconnect => set_enabled(display, false),
    }
}

/// Brings `display` back into the desktop layout.
pub fn connect(display: DisplayId, strategy: Strategy) -> Result<(), PowerError> {
    match strategy {
        Strategy::Mirror => {
            if config::clear_mirror(display, Scope::Session) {
                Ok(())
            } else {
                Err(PowerError::Failed)
            }
        }
        Strategy::HardDisconnect => set_enabled(display, true),
    }
}

/// Whether the display currently counts as connected for UI purposes.
pub fn is_connected(display: DisplayId) -> bool {
    let snapshot = display::snapshot(display);
    snapshot.is_active && snapshot.mirrors.is_none()
}

fn set_enabled(display: DisplayId, enabled: bool) -> Result<(), PowerError> {
    let Some(configure) = configure_display_enabled() else {
        return Err(PowerError::Unavailable);
    };

    let ok = run_with_timeout(TRANSACTION_TIMEOUT, false, move || {
        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        if unsafe { CGBeginDisplayConfiguration(&mut config) } != CGError::Success {
            return false;
        }
        if unsafe { configure(config, display.0, enabled) } != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        // Session scope, never permanent: a disconnect that survives reboot
        // could leave the machine with no usable display at login.
        let completed =
            unsafe { CGCompleteDisplayConfiguration(config, CGConfigureOption::ForSession) };
        if completed != CGError::Success {
            unsafe { CGCancelDisplayConfiguration(config) };
            return false;
        }
        true
    });

    if ok {
        Ok(())
    } else {
        Err(PowerError::Failed)
    }
}
