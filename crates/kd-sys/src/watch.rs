//! Display reconfiguration notifications.

use std::sync::{Mutex, OnceLock};

use objc2_core_graphics::{
    CGDisplayChangeSummaryFlags, CGDisplayRegisterReconfigurationCallback,
    CGDisplayRemoveReconfigurationCallback,
};

use crate::display::DisplayId;

type Handler = Box<dyn Fn(DisplayId, Change) + Send + 'static>;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Change {
    Added,
    Removed,
    Moved,
    ModeChanged,
    Other,
}

fn handlers() -> &'static Mutex<Vec<Handler>> {
    static HANDLERS: OnceLock<Mutex<Vec<Handler>>> = OnceLock::new();
    HANDLERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Registers `handler` for every later reconfiguration.
///
/// The callback has to be a plain C function, so handlers live in a process-wide
/// list rather than being passed through `userInfo`. That also avoids handing
/// CoreGraphics a pointer whose lifetime we would then have to guarantee.
pub fn on_reconfiguration(handler: impl Fn(DisplayId, Change) + Send + 'static) {
    let mut list = match handlers().lock() {
        Ok(list) => list,
        Err(_) => return,
    };
    if list.is_empty() {
        unsafe {
            CGDisplayRegisterReconfigurationCallback(Some(callback), std::ptr::null_mut());
        }
    }
    list.push(Box::new(handler));
}

pub fn stop() {
    if let Ok(mut list) = handlers().lock() {
        list.clear();
    }
    unsafe {
        CGDisplayRemoveReconfigurationCallback(Some(callback), std::ptr::null_mut());
    }
}

unsafe extern "C-unwind" fn callback(
    display: u32,
    flags: CGDisplayChangeSummaryFlags,
    _user_info: *mut std::ffi::c_void,
) {
    // Fires once before the change and once after; only the settled state is
    // useful, so the "begin" pass is ignored.
    if flags.contains(CGDisplayChangeSummaryFlags::BeginConfigurationFlag) {
        return;
    }

    let change = if flags.contains(CGDisplayChangeSummaryFlags::AddFlag) {
        Change::Added
    } else if flags.contains(CGDisplayChangeSummaryFlags::RemoveFlag) {
        Change::Removed
    } else if flags.contains(CGDisplayChangeSummaryFlags::MovedFlag) {
        Change::Moved
    } else if flags.contains(CGDisplayChangeSummaryFlags::SetModeFlag) {
        Change::ModeChanged
    } else {
        Change::Other
    };

    if let Ok(list) = handlers().lock() {
        for handler in list.iter() {
            handler(DisplayId(display), change);
        }
    }
}
