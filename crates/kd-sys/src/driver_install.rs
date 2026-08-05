//! Installing and removing DisEQ.driver, the HAL plug-in, from inside the app.
//!
//! The plug-in lives in `/Library/Audio/Plug-Ins/HAL`, which is root-owned, so
//! copying it there needs an authorisation the app does not have. The elevation
//! goes through `NSAppleScript` running `do shell script … with administrator
//! privileges`: a public API whose authorisation dialog is attributed to DisEQ
//! itself. Shelling out to `osascript` would work too, but the dialog would then
//! name `osascript`, which is exactly what a password prompt should not do.
//!
//! A copy of the driver rides along inside the app bundle, so a release that is
//! dragged out of a disk image carries everything it needs and the user is never
//! sent to a terminal.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{
    ns_string, NSAppleScript, NSDictionary, NSMutableDictionary, NSNumber, NSString,
};
use std::path::{Path, PathBuf};

/// Where coreaudiod looks for HAL plug-ins.
pub const HAL_DIRECTORY: &str = "/Library/Audio/Plug-Ins/HAL";
/// The installed plug-in itself.
pub const INSTALLED_PATH: &str = "/Library/Audio/Plug-Ins/HAL/DisEQ.driver";
/// The copy carried inside the app bundle, relative to `Contents`.
const BUNDLED_RELATIVE: &str = "Resources/DisEQ.driver";

/// AppleScript's code for a dialog the user dismissed. Cancelling is a choice,
/// not a failure, and must not be reported as one.
const USER_CANCELLED: i32 = -128;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    /// The plug-in is installed and matches the copy in this app.
    Current,
    /// Installed, but built by a different version of DisEQ.
    Outdated {
        installed: String,
        bundled: String,
    },
    NotInstalled,
    /// This binary has no driver to install — a development build run outside a
    /// bundle, rather than anything the user did wrong.
    NoBundledDriver,
}

impl State {
    /// Whether an install would change anything.
    pub fn needs_install(&self) -> bool {
        matches!(self, Self::NotInstalled | Self::Outdated { .. })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    /// The authorisation dialog was dismissed.
    Cancelled,
    NoBundledDriver,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Cancelled"),
            Self::NoBundledDriver => {
                f.write_str("This build of DisEQ does not carry a copy of the audio driver")
            }
            Self::Failed(message) => f.write_str(message),
        }
    }
}

/// The driver bundled inside the running app, if there is one.
///
/// Derived from the executable's own path rather than `NSBundle` so that it
/// answers honestly — with `None` — when the binary is run straight out of
/// `target/` instead of from `DisEQ.app`.
pub fn bundled_path() -> Option<PathBuf> {
    // …/DisEQ.app/Contents/MacOS/DisEQ → …/DisEQ.app/Contents/Resources/DisEQ.driver
    let executable = std::env::current_exe().ok()?;
    let contents = executable.parent()?.parent()?;
    let driver = contents.join(BUNDLED_RELATIVE);
    driver.is_dir().then_some(driver)
}

pub fn state() -> State {
    let Some(bundled) = bundled_path() else {
        return State::NoBundledDriver;
    };
    let installed = Path::new(INSTALLED_PATH);
    if !installed.is_dir() {
        return State::NotInstalled;
    }
    match (bundle_version(&bundled), bundle_version(installed)) {
        (Some(bundled), Some(installed)) if bundled == installed => State::Current,
        (Some(bundled), Some(installed)) => State::Outdated { installed, bundled },
        // A plug-in whose version cannot be read is present but unidentifiable.
        // Treating it as current avoids nagging about a driver that may well be
        // working; a reinstall from Settings is still available by hand.
        _ => State::Current,
    }
}

/// Copies the bundled driver into place and restarts coreaudiod.
///
/// Must be called from the main thread: `NSAppleScript` runs the authorisation
/// dialog, and it blocks until the user answers.
pub fn install() -> Result<(), Error> {
    let bundled = bundled_path().ok_or(Error::NoBundledDriver)?;
    let source = bundled.to_string_lossy();
    // Staged through a temporary directory so the quarantine flag can be
    // stripped from a copy rather than from the signed app bundle, and so a
    // failed copy never leaves a half-written plug-in where coreaudiod will
    // try to load it.
    let script = format!(
        "set -e\n\
         staging=$(mktemp -d)\n\
         trap 'rm -rf \"$staging\"' EXIT\n\
         ditto {source} \"$staging/DisEQ.driver\"\n\
         xattr -cr \"$staging/DisEQ.driver\"\n\
         rm -rf {installed}\n\
         mkdir -p {directory}\n\
         ditto \"$staging/DisEQ.driver\" {installed}\n\
         chown -R root:wheel {installed}\n\
         chmod -R 755 {installed}\n\
         killall coreaudiod\n",
        source = shell_quote(&source),
        installed = shell_quote(INSTALLED_PATH),
        directory = shell_quote(HAL_DIRECTORY),
    );
    run_privileged(
        &script,
        "DisEQ needs to install its audio driver. Audio will stop for about a second.",
    )
}

/// Removes the installed driver and restarts coreaudiod.
pub fn uninstall() -> Result<(), Error> {
    let script = format!(
        "set -e\nrm -rf {installed}\nkillall coreaudiod\n",
        installed = shell_quote(INSTALLED_PATH),
    );
    run_privileged(
        &script,
        "DisEQ needs to remove its audio driver. Audio will stop for about a second.",
    )
}

fn run_privileged(script: &str, prompt: &str) -> Result<(), Error> {
    let source = format!(
        "do shell script \"{script}\" with prompt \"{prompt}\" with administrator privileges",
        script = applescript_quote(script),
        prompt = applescript_quote(prompt),
    );
    let Some(applescript) =
        NSAppleScript::initWithSource(NSAppleScript::alloc(), &NSString::from_str(&source))
    else {
        return Err(Error::Failed(
            "macOS refused to compile the install step".into(),
        ));
    };

    let mut info: Option<Retained<NSDictionary<NSString, AnyObject>>> = None;
    // Safety: the error dictionary AppleScript produces is keyed by NSString.
    let _ = unsafe { applescript.executeAndReturnError(Some(&mut info)) };

    // A nil error dictionary is the only signal of success the API offers; the
    // returned descriptor is a valid object either way.
    match info {
        None => Ok(()),
        Some(info) => Err(describe(&info)),
    }
}

fn describe(info: &NSDictionary<NSString, AnyObject>) -> Error {
    let number = info
        .objectForKey(unsafe { objc2_foundation::NSAppleScriptErrorNumber })
        .and_then(|value| value.downcast_ref::<NSNumber>().map(NSNumber::as_i32));
    if number == Some(USER_CANCELLED) {
        return Error::Cancelled;
    }
    let message = info
        .objectForKey(unsafe { objc2_foundation::NSAppleScriptErrorMessage })
        .and_then(|value| value.downcast_ref::<NSString>().map(NSString::to_string))
        .unwrap_or_else(|| "macOS refused to change the audio driver".into());
    Error::Failed(message)
}

/// `CFBundleVersion` from a bundle's `Info.plist`.
///
/// Read as a property list rather than as text, so a plug-in whose plist has
/// been rewritten in the binary format still identifies itself.
fn bundle_version(bundle: &Path) -> Option<String> {
    let plist = bundle.join("Contents/Info.plist");
    let path = NSString::from_str(&plist.to_string_lossy());
    // Safety: an Info.plist is a dictionary keyed by strings, or unreadable, in
    // which case this returns nil.
    let dictionary: Retained<NSMutableDictionary<NSString, AnyObject>> =
        unsafe { NSMutableDictionary::dictionaryWithContentsOfFile(&path) }?;
    let value = dictionary.objectForKey(ns_string!("CFBundleVersion"))?;
    Some(value.downcast_ref::<NSString>()?.to_string())
}

/// Wraps `value` so a shell reads it as one literal word.
fn shell_quote(value: &str) -> String {
    // A single-quoted string ends at the first quote, so an embedded one is
    // closed, escaped outside the quotes, and reopened.
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Escapes `value` for an AppleScript string literal.
fn applescript_quote(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_a_quote_stays_one_shell_word() {
        assert_eq!(
            shell_quote("/Users/o'brien/DisEQ.app"),
            r"'/Users/o'\''brien/DisEQ.app'"
        );
    }

    #[test]
    fn applescript_escaping_survives_backslashes_and_quotes() {
        assert_eq!(applescript_quote(r#"say "hi\"#), r#"say \"hi\\"#);
    }

    #[test]
    fn a_missing_bundle_has_no_version() {
        assert_eq!(bundle_version(Path::new("/nonexistent/DisEQ.driver")), None);
    }

    #[test]
    fn only_a_missing_or_older_driver_asks_to_be_installed() {
        assert!(State::NotInstalled.needs_install());
        assert!(State::Outdated {
            installed: "0.1.0".into(),
            bundled: "0.2.0".into()
        }
        .needs_install());
        assert!(!State::Current.needs_install());
        assert!(!State::NoBundledDriver.needs_install());
    }

    #[test]
    fn state_is_safe_outside_an_app_bundle() {
        let _ = state();
    }
}
