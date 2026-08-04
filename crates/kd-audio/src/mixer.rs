//! The App Mixer: a volume fader per application.
//!
//! Every controlled application is tapped with
//! `CATapMuteBehavior::MutedWhenTapped`, which takes its audio out of the
//! system mix. A private aggregate device gathers the taps as inputs and the
//! destination device as its output, and one IO proc scales each tap by its
//! fader and sums them back into the destination:
//!
//! ```text
//! Spotify ─▶ tap ─▶ ×0.4 ─┐
//! Brave   ─▶ tap ─▶ ×1.0 ─┼─▶ destination
//! everything untapped ────┘
//! ```
//!
//! The destination is the DisEQ driver when a route is running, so mixed
//! audio still meets the EQ; without the driver it is the hardware directly,
//! and the mixer works on its own.
//!
//! Written from scratch — eqMac's public tree has no App Mixer.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioSubDeviceDriftCompensationKey, kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareDestroyAggregateDevice, AudioObjectID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::devices::Device;
use crate::processes::Process;
use crate::tap::{Tap, TapError};

/// How many applications can have a fader at once.
///
/// Every tap is a real audio object with its own stream in the aggregate
/// device, so this is a resource limit rather than a UI one. Far more than the
/// number of applications that play audio at the same time in practice.
pub const MAX_FADERS: usize = 16;

#[derive(Debug)]
pub enum MixerError {
    Tap(TapError),
    /// `AudioHardwareCreateAggregateDevice` was refused.
    Aggregate(i32),
    /// The destination device publishes no UID, so it cannot be named as a
    /// sub-device.
    DestinationHasNoUid,
    /// `AudioDeviceCreateIOProcID` or `AudioDeviceStart` was refused.
    Io(i32),
    /// More applications than [`MAX_FADERS`].
    TooMany,
}

impl std::fmt::Display for MixerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tap(error) => write!(f, "{error}"),
            Self::Aggregate(status) => {
                write!(f, "the aggregate device was refused (status {status})")
            }
            Self::DestinationHasNoUid => write!(f, "the destination device publishes no UID"),
            Self::Io(status) => write!(f, "the mixer's IO proc was refused (status {status})"),
            Self::TooMany => write!(f, "at most {MAX_FADERS} applications can have a fader"),
        }
    }
}

impl std::error::Error for MixerError {}

impl From<TapError> for MixerError {
    fn from(error: TapError) -> Self {
        Self::Tap(error)
    }
}

/// One application's fader, as the IO proc sees it.
///
/// Only atomics: the proc reads these on a real-time thread while the UI writes
/// them from the main one.
struct Fader {
    /// Linear gain as `f32` bits. Not dB — the multiply is in the callback.
    gain: AtomicU32,
    /// Channels this tap contributes. Read once at setup, but stored here so
    /// the callback needs nothing but this array.
    channels: AtomicU32,
}

impl Fader {
    fn new() -> Self {
        Self {
            gain: AtomicU32::new(1.0f32.to_bits()),
            channels: AtomicU32::new(0),
        }
    }

    fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    fn set_gain(&self, gain: f32) {
        self.gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

/// What the IO proc reads. Shared with the real-time thread, so nothing here
/// allocates or locks.
struct Faders {
    slots: [Fader; MAX_FADERS],
    /// How many of `slots` correspond to a live tap. Only grows and shrinks
    /// when the mixer is rebuilt, which stops the proc first.
    active: AtomicUsize,
    /// Which input buffer the first tap arrives in.
    ///
    /// The aggregate hands over its sub-device's input streams before its taps.
    /// A destination with no inputs — speakers — contributes none and the taps
    /// start at zero; an audio interface with microphone inputs contributes its
    /// own, and mixing from zero mixes the microphone instead.
    offset: AtomicUsize,
}

impl Faders {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| Fader::new()),
            active: AtomicUsize::new(0),
            offset: AtomicUsize::new(0),
        }
    }
}

/// A running App Mixer. Dropping it destroys the taps, which gives every
/// application its audio back.
pub struct Mixer {
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    /// Held so the taps outlive the IO proc: dropping a tap while the proc is
    /// running would leave it reading a stream that no longer exists.
    taps: Vec<Tap>,
    faders: Arc<Faders>,
    destination: Device,
    running: bool,
}

impl Mixer {
    /// Taps every process in `processes` and starts mixing them into
    /// `destination`.
    ///
    /// Faders start at unity, so starting the mixer is inaudible.
    pub fn start(processes: &[Process], destination: &Device) -> Result<Self, MixerError> {
        if processes.len() > MAX_FADERS {
            return Err(MixerError::TooMany);
        }
        let destination_uid = destination
            .uid
            .clone()
            .ok_or(MixerError::DestinationHasNoUid)?;

        let taps = processes
            .iter()
            .map(Tap::muting)
            .collect::<Result<Vec<_>, _>>()?;

        let faders = Arc::new(Faders::new());
        for (slot, tap) in faders.slots.iter().zip(&taps) {
            slot.channels
                .store(tap.channels() as u32, Ordering::Relaxed);
            slot.set_gain(1.0);
        }
        faders.active.store(taps.len(), Ordering::Release);
        faders.offset.store(
            kd_sys::audio::buffer_count(destination.id, true) as usize,
            Ordering::Release,
        );

        let aggregate = create_aggregate(&destination_uid, &taps)?;

        let mut mixer = Self {
            aggregate,
            proc_id: None,
            taps,
            faders,
            destination: destination.clone(),
            running: false,
        };
        mixer.start_io()?;
        Ok(mixer)
    }

    fn start_io(&mut self) -> Result<(), MixerError> {
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: the ref-con is the fader array, which this struct keeps alive
        // through the `Arc` and which outlives the proc because `Drop` stops
        // and destroys the proc first.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                self.aggregate,
                Some(mix),
                Arc::as_ptr(&self.faders) as *mut c_void,
                NonNull::from(&mut proc_id),
            )
        };
        if status != 0 || proc_id.is_none() {
            return Err(MixerError::Io(status));
        }
        self.proc_id = proc_id;

        // SAFETY: a proc that was just created on this device.
        let status = unsafe { AudioDeviceStart(self.aggregate, proc_id) };
        if status != 0 {
            return Err(MixerError::Io(status));
        }
        self.running = true;
        Ok(())
    }

    pub fn destination(&self) -> &Device {
        &self.destination
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The applications with a fader, in the order their gains are indexed.
    pub fn faders(&self) -> impl Iterator<Item = (&Process, f32)> {
        self.taps
            .iter()
            .enumerate()
            .map(|(index, tap)| (tap.process(), self.faders.slots[index].gain()))
    }

    /// Sets one application's volume, 0..1 linear.
    ///
    /// Returns false if that process has no fader — it started playing after
    /// the mixer was built, and the mixer has to be rebuilt to include it.
    pub fn set_gain(&self, process: AudioObjectID, gain: f32) -> bool {
        match self.slot_of(process) {
            Some(index) => {
                self.faders.slots[index].set_gain(gain);
                true
            }
            None => false,
        }
    }

    pub fn gain(&self, process: AudioObjectID) -> Option<f32> {
        self.slot_of(process)
            .map(|index| self.faders.slots[index].gain())
    }

    fn slot_of(&self, process: AudioObjectID) -> Option<usize> {
        self.taps.iter().position(|tap| tap.process().id == process)
    }

    /// Whether the set of applications this mixer covers still matches what is
    /// playing.
    ///
    /// Adding a fader means new taps and a new aggregate device, so the caller
    /// rebuilds rather than mutating.
    pub fn covers(&self, processes: &[Process]) -> bool {
        processes.len() == self.taps.len()
            && processes
                .iter()
                .all(|process| self.slot_of(process.id).is_some())
    }
}

impl Drop for Mixer {
    fn drop(&mut self) {
        // Order matters: stop the proc, then unregister it, then destroy the
        // device, and only then let the taps drop. Any other order leaves the
        // real-time thread reading something that has gone away.
        // SAFETY: each object was created by this struct and is destroyed once.
        unsafe {
            if let Some(proc_id) = self.proc_id {
                AudioDeviceStop(self.aggregate, Some(proc_id));
                AudioDeviceDestroyIOProcID(self.aggregate, Some(proc_id));
            }
            AudioHardwareDestroyAggregateDevice(self.aggregate);
        }
        self.running = false;
    }
}

/// The mix itself, on the aggregate device's real-time thread.
///
/// No allocation, no locks, no logging, no Objective-C. Input buffers arrive
/// one per tap, in the order the tap list named them, interleaved within each
/// buffer.
unsafe extern "C-unwind" fn mix(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    ref_con: *mut c_void,
) -> i32 {
    if ref_con.is_null() {
        return 0;
    }
    let faders = &*(ref_con as *const Faders);

    let output_list = output.as_ptr();
    let output_buffers = (*output_list).mNumberBuffers as usize;
    if output_buffers == 0 {
        return 0;
    }
    // Everything is summed into the first output buffer: the destination is one
    // device with one output stream. Any others are left silent rather than
    // filled with a guess.
    let sink = &mut *(*output_list).mBuffers.as_mut_ptr();
    if sink.mData.is_null() {
        return 0;
    }
    let sink_channels = sink.mNumberChannels.max(1) as usize;
    let sink_samples = sink.mDataByteSize as usize / std::mem::size_of::<f32>();
    let sink_frames = sink_samples / sink_channels;
    let sink_data = std::slice::from_raw_parts_mut(sink.mData as *mut f32, sink_samples);
    // The buffer arrives holding whatever the last cycle left in it.
    sink_data.fill(0.0);
    for index in 1..output_buffers {
        let other = &mut *(*output_list).mBuffers.as_mut_ptr().add(index);
        if !other.mData.is_null() {
            std::ptr::write_bytes(other.mData as *mut u8, 0, other.mDataByteSize as usize);
        }
    }

    let input_list = input.as_ptr();
    let offset = faders.offset.load(Ordering::Acquire);
    let available = ((*input_list).mNumberBuffers as usize).saturating_sub(offset);
    let taps = available.min(faders.active.load(Ordering::Acquire));

    for index in 0..taps {
        let gain = faders.slots[index].gain();
        if gain <= 0.0 {
            // A muted application costs nothing beyond staying tapped.
            continue;
        }
        let source = &*(*input_list).mBuffers.as_ptr().add(offset + index);
        if source.mData.is_null() {
            continue;
        }
        let source_channels = source.mNumberChannels.max(1) as usize;
        let source_samples = source.mDataByteSize as usize / std::mem::size_of::<f32>();
        let source_data = std::slice::from_raw_parts(source.mData as *const f32, source_samples);
        let frames = sink_frames.min(source_samples / source_channels);

        for frame in 0..frames {
            for channel in 0..sink_channels {
                // A mono tap feeds every output channel; a tap with more
                // channels than the destination has its extras folded onto the
                // last one rather than dropped.
                let from = source_channels.min(channel + 1) - 1;
                sink_data[frame * sink_channels + channel] +=
                    source_data[frame * source_channels + from] * gain;
            }
        }
    }

    0
}

/// Builds the private aggregate device: the destination as its only sub-device,
/// every tap in its tap list.
fn create_aggregate(destination_uid: &str, taps: &[Tap]) -> Result<AudioObjectID, MixerError> {
    let uid = format!("com.koshka.DisEQ.mixer.{}", std::process::id());

    let sub_devices = NSArray::from_retained_slice(&[dictionary(&[
        (
            kAudioSubDeviceUIDKey,
            NSString::from_str(destination_uid).into(),
        ),
        // The destination sets the clock; the taps follow it.
        (
            kAudioSubDeviceDriftCompensationKey,
            NSNumber::new_u32(0).into(),
        ),
    ])]);

    let tap_list: Vec<Retained<NSDictionary<NSString, objc2::runtime::AnyObject>>> = taps
        .iter()
        .map(|tap| {
            dictionary(&[
                (kAudioSubTapUIDKey, NSString::from_str(tap.uid()).into()),
                // Taps run on the tapped process's clock, which is not the
                // destination's. Letting Core Audio reconcile them is the whole
                // reason the mixer needs no ring buffer of its own.
                (
                    kAudioSubTapDriftCompensationKey,
                    NSNumber::new_u32(1).into(),
                ),
            ])
        })
        .collect();
    let tap_list = NSArray::from_retained_slice(&tap_list);

    let description = dictionary(&[
        (kAudioAggregateDeviceUIDKey, NSString::from_str(&uid).into()),
        (
            kAudioAggregateDeviceNameKey,
            NSString::from_str("DisEQ Mixer").into(),
        ),
        // Private: visible to this process only, so it does not appear in every
        // other application's device list.
        (
            kAudioAggregateDeviceIsPrivateKey,
            NSNumber::new_u32(1).into(),
        ),
        (
            kAudioAggregateDeviceIsStackedKey,
            NSNumber::new_u32(0).into(),
        ),
        (
            kAudioAggregateDeviceMainSubDeviceKey,
            NSString::from_str(destination_uid).into(),
        ),
        (kAudioAggregateDeviceSubDeviceListKey, sub_devices.into()),
        (kAudioAggregateDeviceTapListKey, tap_list.into()),
        // The tap has to start with the device. Left off, the aggregate runs
        // and its IO proc is called on time with buffers full of nothing.
        (
            kAudioAggregateDeviceTapAutoStartKey,
            NSNumber::new_u32(1).into(),
        ),
    ]);

    let mut aggregate: AudioObjectID = 0;
    // SAFETY: NSDictionary is toll-free bridged to CFDictionary, and the
    // description outlives the call, which copies what it needs.
    let status = unsafe {
        let bridged = &*(Retained::as_ptr(&description) as *const CFDictionary);
        AudioHardwareCreateAggregateDevice(bridged, NonNull::from(&mut aggregate))
    };
    if status != 0 || aggregate == 0 {
        return Err(MixerError::Aggregate(status));
    }
    Ok(aggregate)
}

/// An `NSDictionary` keyed by the C string constants Core Audio publishes.
fn dictionary(
    entries: &[(&std::ffi::CStr, Retained<objc2::runtime::AnyObject>)],
) -> Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> {
    let keys: Vec<Retained<NSString>> = entries
        .iter()
        .map(|(key, _)| NSString::from_str(&key.to_string_lossy()))
        .collect();
    let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let values: Vec<&objc2::runtime::AnyObject> =
        entries.iter().map(|(_, value)| &**value).collect();
    NSDictionary::from_slices(&keys, &values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fader_clamps_to_the_range_a_slider_can_produce() {
        let fader = Fader::new();
        fader.set_gain(2.0);
        assert_eq!(fader.gain(), 1.0);
        fader.set_gain(-1.0);
        assert_eq!(fader.gain(), 0.0);
        fader.set_gain(0.25);
        assert_eq!(fader.gain(), 0.25);
    }

    #[test]
    fn faders_start_at_unity_so_starting_the_mixer_is_inaudible() {
        let faders = Faders::new();
        assert!(faders.slots.iter().all(|slot| slot.gain() == 1.0));
    }

    #[test]
    fn the_description_dictionary_carries_the_keys_core_audio_asks_for() {
        let built = dictionary(&[
            (
                kAudioAggregateDeviceUIDKey,
                NSString::from_str("uid").into(),
            ),
            (
                kAudioAggregateDeviceIsPrivateKey,
                NSNumber::new_u32(1).into(),
            ),
        ]);
        assert_eq!(built.len(), 2);
        assert!(built.objectForKey(&NSString::from_str("uid")).is_some());
        assert!(built.objectForKey(&NSString::from_str("private")).is_some());
    }
}
