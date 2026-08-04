//! Launch-at-login support through macOS Service Management.
//!
//! `SMAppService::mainAppService` registers the application bundle itself, so
//! DisEQ needs neither a helper executable nor a hand-written LaunchAgent.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::NSString;
use std::ptr;
use std::sync::OnceLock;

use crate::dylib::Framework;

const SERVICE_MANAGEMENT: &str =
    "/System/Library/Frameworks/ServiceManagement.framework/ServiceManagement";

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Status {
    NotRegistered,
    Enabled,
    RequiresApproval,
    /// Service Management is present, but this app has never been registered.
    /// For the main app this state is still registerable.
    NotFound,
    /// The Service Management API itself could not be loaded.
    Unavailable,
}

fn class() -> Option<&'static AnyClass> {
    static FRAMEWORK: OnceLock<Option<Framework>> = OnceLock::new();
    FRAMEWORK
        .get_or_init(|| Framework::open(SERVICE_MANAGEMENT))
        .as_ref()?;
    AnyClass::get(c"SMAppService")
}

fn service() -> Option<Retained<AnyObject>> {
    Some(unsafe { msg_send![class()?, mainAppService] })
}

pub fn status() -> Status {
    let Some(service) = service() else {
        return Status::Unavailable;
    };
    status_from_raw(unsafe { msg_send![&*service, status] })
}

fn status_from_raw(raw: isize) -> Status {
    match raw {
        0isize => Status::NotRegistered,
        1 => Status::Enabled,
        2 => Status::RequiresApproval,
        3 => Status::NotFound,
        _ => Status::Unavailable,
    }
}

pub fn set_enabled(enabled: bool) -> Result<Status, String> {
    let Some(service) = service() else {
        return Err("Launch at Login is unavailable on this macOS build".into());
    };
    let current = status();
    if (enabled && current == Status::Enabled)
        || (!enabled && matches!(current, Status::NotRegistered | Status::NotFound))
    {
        return Ok(current);
    }
    // Re-registering cannot override a denial in System Settings. Leave the
    // registration in place so the user can approve it there.
    if enabled && current == Status::RequiresApproval {
        return Ok(current);
    }

    let mut error: *mut AnyObject = ptr::null_mut();
    let changed: bool = unsafe {
        if enabled {
            msg_send![&*service, registerAndReturnError: &mut error]
        } else {
            msg_send![&*service, unregisterAndReturnError: &mut error]
        }
    };
    if changed {
        Ok(status())
    } else {
        Err(error_description(error))
    }
}

pub fn open_system_settings() -> bool {
    let Some(class) = class() else {
        return false;
    };
    unsafe { msg_send![class, openSystemSettingsLoginItems] }
    true
}

fn error_description(error: *mut AnyObject) -> String {
    let Some(error) = (unsafe { error.as_ref() }) else {
        return "macOS refused to change Launch at Login".into();
    };
    let description: Retained<NSString> = unsafe { msg_send![error, localizedDescription] };
    description.to_string()
}

#[cfg(test)]
mod tests {
    use super::Status;

    #[test]
    fn service_not_found_is_distinct_from_api_unavailable() {
        assert_eq!(super::status_from_raw(3), Status::NotFound);
        assert_eq!(super::status_from_raw(99), Status::Unavailable);
    }

    #[test]
    fn status_query_is_safe_outside_an_app_bundle() {
        let _ = super::status();
    }
}
