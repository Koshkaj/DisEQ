//! The applications making sound, as Core Audio sees them.
//!
//! macOS 14.4 gave every audio-producing process an `AudioObjectID` of its own,
//! which is what the App Mixer needs: something to hang a fader on, and
//! something [`crate::tap`] can point a tap at.
//!
//! Written from scratch — eqMac's public tree has no App Mixer.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertySelector,
};
use objc2_core_foundation::{CFRetained, CFString};

/// One process that Core Audio knows about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    /// The audio object, not the process. This is what a tap description takes.
    pub id: AudioObjectID,
    pub pid: i32,
    /// `com.spotify.client` and the like. Absent for processes with no bundle —
    /// command-line tools, and some helpers.
    pub bundle_id: Option<String>,
    /// Whether it is producing audio right now. A process that has gone quiet
    /// keeps its object for a while, so this is what separates "playing" from
    /// "played something once".
    pub is_playing: bool,
}

impl Process {
    fn describe(id: AudioObjectID) -> Self {
        Self {
            id,
            pid: pid(id).unwrap_or(0),
            bundle_id: bundle_id(id),
            is_playing: is_playing(id),
        }
    }

    /// The last component of the bundle ID, which is the closest thing to a
    /// display name available without asking `NSRunningApplication`.
    pub fn short_name(&self) -> Option<&str> {
        self.bundle_id.as_deref()?.rsplit('.').next()
    }
}

/// Every process Core Audio has an object for, playing or not.
pub fn all() -> Vec<Process> {
    object_list().into_iter().map(Process::describe).collect()
}

/// Only the ones making sound now — what the mixer shows a fader for.
/// The audio process object for a running process, if the system has one.
///
/// Only processes that have played audio at some point get an object, so this
/// is `None` for a process that has never made a sound.
pub fn by_pid(pid: i32) -> Option<Process> {
    all().into_iter().find(|process| process.pid == pid)
}

pub fn playing() -> Vec<Process> {
    all()
        .into_iter()
        .filter(|process| process.is_playing)
        .collect()
}

/// Whether this build of macOS has process objects at all. False on anything
/// before 14.4, where the App Mixer cannot work.
pub fn are_available() -> bool {
    let address = address(kAudioHardwarePropertyProcessObjectList);
    let mut size = 0u32;
    // SAFETY: a size query on the system object with a well-formed address.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    status == 0
}

fn object_list() -> Vec<AudioObjectID> {
    let address = address(kAudioHardwarePropertyProcessObjectList);
    let mut size = 0u32;
    // SAFETY: as above.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }

    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    let mut ids = vec![0 as AudioObjectID; count];
    // SAFETY: the buffer is exactly the size the query reported.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr() as *mut c_void).unwrap(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
    ids
}

pub fn pid(process: AudioObjectID) -> Option<i32> {
    get(process, kAudioProcessPropertyPID)
}

pub fn is_playing(process: AudioObjectID) -> bool {
    get::<u32>(process, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) != 0
}

pub fn bundle_id(process: AudioObjectID) -> Option<String> {
    let string: *const CFString = get(process, kAudioProcessPropertyBundleID)?;
    if string.is_null() {
        return None;
    }
    // SAFETY: the property returns a +1 CFString, which this takes ownership of
    // so it is released when the retained wrapper drops.
    let string = unsafe { CFRetained::from_raw(NonNull::new(string as *mut CFString)?) };
    let text = string.to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn get<T: Copy + Default>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Option<T> {
    let address = address(selector);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    // SAFETY: `value` is exactly `size` bytes and the address is well-formed;
    // a property of a different size comes back as a non-zero status.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(&mut value as *mut T as *mut c_void)?,
        )
    };
    if status == 0 {
        Some(value)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_objects_exist_on_this_machine() {
        // The App Mixer's one hard requirement. If this fails the OS is older
        // than 14.4 and no amount of the rest of the code helps.
        assert!(are_available());
    }

    #[test]
    fn every_process_has_a_pid_and_survives_being_described() {
        for process in all() {
            assert_ne!(process.id, 0);
            // A process object with no pid is one that exited between the
            // enumeration and the query — allowed, but it must not panic.
            if let Some(name) = process.short_name() {
                assert!(!name.is_empty());
            }
        }
    }

    #[test]
    fn the_short_name_is_the_last_component_of_the_bundle_id() {
        let process = Process {
            id: 1,
            pid: 2,
            bundle_id: Some("com.spotify.client".into()),
            is_playing: true,
        };
        assert_eq!(process.short_name(), Some("client"));
    }
}
