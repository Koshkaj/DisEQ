//! Process taps: taking one application's audio out of the system mix so we can
//! put it back at a different level.
//!
//! `CATapMuteBehavior::MutedWhenTapped` is the whole feature. The tapped
//! process stops reaching the output device directly and reaches us instead;
//! whatever gain we apply on the way through is that application's volume. It
//! costs nothing while nobody is reading the tap, which is why an app with its
//! fader at 100% can be left tapped.
//!
//! Written from scratch — eqMac's public tree has no App Mixer. Needs macOS
//! 14.4 or newer and the user's consent to capture audio.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioTapPropertyFormat,
    kAudioTapPropertyUID, AudioHardwareCreateProcessTap, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_foundation::{NSArray, NSNumber, NSString};

use crate::processes::Process;

#[derive(Debug)]
pub enum TapError {
    /// `AudioHardwareCreateProcessTap` was refused. `-4` is
    /// `kAudio_UnimplementedError` — an OS with no tap support. Most other
    /// failures here are the user having declined audio capture.
    Refused(i32),
    /// The tap was created but publishes no UID, so it cannot be named in an
    /// aggregate device's tap list and is useless.
    NoUid,
}

impl std::fmt::Display for TapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(status) => write!(
                f,
                "the system refused a process tap (status {status}) — \
                 this needs macOS 14.4+ and permission to record audio"
            ),
            Self::NoUid => write!(f, "the tap published no UID"),
        }
    }
}

impl std::error::Error for TapError {}

/// A live tap on one process. Dropping it un-mutes the process and gives its
/// audio straight back to the output device.
pub struct Tap {
    id: AudioObjectID,
    uid: String,
    process: Process,
    format: Option<AudioStreamBasicDescription>,
}

impl Tap {
    /// Taps `process`, taking its audio out of the system mix.
    ///
    /// The tap is private — it exists only for this process — and a stereo
    /// mixdown, so every fader carries the same two channels whatever the
    /// application produces.
    pub fn muting(process: &Process) -> Result<Self, TapError> {
        // SAFETY: a description built entirely from setters, then handed to the
        // creation call that copies it.
        let description = unsafe {
            let objects = NSArray::from_retained_slice(&[NSNumber::new_u32(process.id)]);
            let description =
                CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &objects);
            description.setName(&NSString::from_str(&format!(
                "DisEQ fader for pid {}",
                process.pid
            )));
            // The point of the whole exercise: the process no longer reaches
            // the hardware on its own.
            description.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
            // Nobody else needs to see this tap, and a public one would show up
            // in every audio application's device list.
            description.setPrivate(true);
            // A process that restarts is a new object with a new fader; keeping
            // the old tap alive would mute an application nobody asked about.
            description.setProcessRestoreEnabled(false);
            description
        };

        let mut id: AudioObjectID = 0;
        // SAFETY: the description outlives the call; `id` is a valid out
        // pointer.
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut id) };
        if status != 0 || id == 0 {
            return Err(TapError::Refused(status));
        }

        let Some(uid) = uid(id) else {
            // SAFETY: `id` came from a successful create and has not been
            // destroyed.
            unsafe { AudioHardwareDestroyProcessTap(id) };
            return Err(TapError::NoUid);
        };

        Ok(Self {
            id,
            uid,
            process: process.clone(),
            format: format(id),
        })
    }

    pub fn id(&self) -> AudioObjectID {
        self.id
    }

    /// The UID an aggregate device's tap list refers to it by.
    pub fn uid(&self) -> &str {
        &self.uid
    }

    pub fn process(&self) -> &Process {
        &self.process
    }

    /// The format the tap delivers. Stereo by construction, but the sample rate
    /// follows whatever the process is playing at.
    pub fn format(&self) -> Option<&AudioStreamBasicDescription> {
        self.format.as_ref()
    }

    /// How many channels this tap contributes to an aggregate device's input.
    pub fn channels(&self) -> usize {
        self.format
            .map(|format| format.mChannelsPerFrame as usize)
            .unwrap_or(crate::format::CHANNELS)
    }

    /// Retained by a description that is still meaningful for a
    /// [`crate::processes::Process`] that has since exited.
    fn description(&self) -> String {
        match self.process.short_name() {
            Some(name) => name.to_string(),
            None => format!("pid {}", self.process.pid),
        }
    }
}

impl std::fmt::Debug for Tap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tap")
            .field("id", &self.id)
            .field("process", &self.description())
            .field("channels", &self.channels())
            .finish()
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        // SAFETY: `id` came from a successful create and is destroyed once.
        // Leaving it alive would leave the application muted with nothing
        // playing its audio.
        unsafe { AudioHardwareDestroyProcessTap(self.id) };
    }
}

fn uid(tap: AudioObjectID) -> Option<String> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyUID,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut string: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    // SAFETY: a well-formed address and an out pointer of the right size.
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut string).cast::<c_void>(),
        )
    };
    if status != 0 || string.is_null() {
        return None;
    }
    // SAFETY: the property returns a +1 CFString this takes ownership of.
    let string = unsafe { CFRetained::from_raw(NonNull::new(string as *mut CFString)?) };
    Some(string.to_string())
}

fn format(tap: AudioObjectID) -> Option<AudioStreamBasicDescription> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioTapPropertyFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut description = AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: 0,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 0,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 0,
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    // SAFETY: as above.
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut description).cast::<c_void>(),
        )
    };
    if status == 0 && description.mChannelsPerFrame > 0 {
        Some(description)
    } else {
        None
    }
}

/// Whether taps can be created at all on this machine, without leaving one
/// behind.
///
/// Answers the two questions the UI needs before it offers an App Mixer: is the
/// OS new enough, and has the user allowed audio capture. A refusal here is why
/// the mixer says it is unavailable rather than silently showing nothing.
pub fn are_permitted() -> Result<(), TapError> {
    // An exclusive tap of nothing taps everything, which is the cheapest
    // description the system will accept as a permission check.
    // SAFETY: setters on a freshly allocated description.
    let description = unsafe {
        let none: Retained<NSArray<NSNumber>> = NSArray::new();
        let description = CATapDescription::initStereoGlobalTapButExcludeProcesses(
            CATapDescription::alloc(),
            &none,
        );
        // Unmuted: this must not take anyone's audio away, even briefly.
        description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        description.setPrivate(true);
        description
    };

    let mut id: AudioObjectID = 0;
    // SAFETY: as in `Tap::muting`.
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut id) };
    if status != 0 || id == 0 {
        return Err(TapError::Refused(status));
    }
    // SAFETY: destroying the tap we just made, once.
    unsafe { AudioHardwareDestroyProcessTap(id) };
    Ok(())
}
