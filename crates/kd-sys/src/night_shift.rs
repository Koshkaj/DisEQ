//! Native macOS Night Shift control.
//!
//! Night Shift is one system-wide policy applied to every compatible display;
//! CoreBrightness does not expose independent per-display state. Apple exposes
//! no public API for the toggle, so the private client is loaded dynamically:
//! an OS update that removes it makes the feature unavailable instead of
//! preventing DisEQ from launching.

use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use std::sync::OnceLock;

use crate::dylib::Framework;

const CORE_BRIGHTNESS: &str =
    "/System/Library/PrivateFrameworks/CoreBrightness.framework/CoreBrightness";

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Time {
    hour: i32,
    minute: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Schedule {
    from: Time,
    to: Time,
}

/// Layout returned by `-[CBBlueLightClient getBlueLightStatus:]` on current
/// macOS. The final byte was added after the original six-field structure;
/// keeping it here also gives older implementations a safely oversized buffer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Status {
    active: u8,
    enabled: u8,
    sun_schedule_permitted: u8,
    mode: i32,
    schedule: Schedule,
    disable_flags: u64,
    supported: u8,
}

// SAFETY: this is the exact anonymous-struct encoding reported by the current
// Objective-C runtime for `-[CBBlueLightClient getBlueLightStatus:]`.
unsafe impl RefEncode for Status {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "?",
        &[
            Encoding::Bool,
            Encoding::Bool,
            Encoding::Bool,
            Encoding::Int,
            Encoding::Struct(
                "?",
                &[
                    Encoding::Struct("?", &[Encoding::Int, Encoding::Int]),
                    Encoding::Struct("?", &[Encoding::Int, Encoding::Int]),
                ],
            ),
            Encoding::ULongLong,
            Encoding::Bool,
        ],
    ));
}

fn client() -> Option<Retained<AnyObject>> {
    // Loading registers CBBlueLightClient with the Objective-C runtime. The
    // one cached handle remains open for the process lifetime.
    static FRAMEWORK: OnceLock<Option<Framework>> = OnceLock::new();
    FRAMEWORK
        .get_or_init(|| Framework::open(CORE_BRIGHTNESS))
        .as_ref()?;
    let class = AnyClass::get(c"CBBlueLightClient")?;
    Some(unsafe { msg_send![class, new] })
}

fn status() -> Option<Status> {
    let client = client()?;
    let mut status = Status::default();
    let read: bool = unsafe { msg_send![&*client, getBlueLightStatus: &mut status] };
    read.then_some(status)
}

/// The native global Night Shift state. `None` means the private client is not
/// available or did not return a valid status.
pub fn enabled() -> Option<bool> {
    status().map(|status| status.enabled != 0)
}

/// Changes only the current native Night Shift override. The schedule and
/// temperature remain owned by macOS.
pub fn set_enabled(enabled: bool) -> bool {
    let Some(client) = client() else {
        return false;
    };
    unsafe { msg_send![&*client, setEnabled: enabled] }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_status_query_is_safe_when_unavailable() {
        let _ = super::enabled();
    }
}
