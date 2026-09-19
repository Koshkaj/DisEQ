//! CoreAudio output device volume.
//!
//! Bindings are hand-written because only a handful of property selectors are
//! needed and the generated CoreAudio surface is enormous.

use std::ffi::c_void;
use std::ptr;

pub type AudioObjectId = u32;

const SYSTEM_OBJECT: AudioObjectId = 1;

// Four-character codes, as CoreAudio spells them.
const fn fourcc(code: &[u8; 4]) -> u32 {
    ((code[0] as u32) << 24) | ((code[1] as u32) << 16) | ((code[2] as u32) << 8) | code[3] as u32
}

const DEFAULT_OUTPUT_DEVICE: u32 = fourcc(b"dOut");
/// Where alerts and the volume keys go. Usually the same device as the default
/// output, but the system tracks it separately — and it is the one the menu-bar
/// slider follows.
const DEFAULT_SYSTEM_OUTPUT_DEVICE: u32 = fourcc(b"sOut");
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const SCOPE_INPUT: u32 = fourcc(b"inpt");
const SCOPE_OUTPUT: u32 = fourcc(b"outp");
const ELEMENT_MAIN: u32 = 0;
/// Virtual main volume rides over whatever channel layout the device has, so it
/// works on devices with no single main channel.
const VIRTUAL_MAIN_VOLUME: u32 = fourcc(b"vmvc");
const VOLUME_SCALAR: u32 = fourcc(b"volm");
const MUTE: u32 = fourcc(b"mute");
const DEVICE_NAME: u32 = fourcc(b"lnam");
const DEVICES: u32 = fourcc(b"dev#");
const STREAM_CONFIGURATION: u32 = fourcc(b"slay");
const DEVICE_UID: u32 = fourcc(b"uid ");
const TRANSPORT_TYPE: u32 = fourcc(b"tran");
const NOMINAL_SAMPLE_RATE: u32 = fourcc(b"nsrt");
/// How far ahead of the read/write head the device's own buffering sits.
/// Bridging two devices needs both, or the ring between them starves.
const SAFETY_OFFSET: u32 = fourcc(b"saft");
const BUFFER_FRAME_SIZE: u32 = fourcc(b"fsiz");
const IS_ALIVE: u32 = fourcc(b"livn");
const LATENCY: u32 = fourcc(b"ltnc");
/// Whether any process on the machine has the device's I/O running.
const IS_RUNNING_SOMEWHERE: u32 = fourcc(b"gone");
/// Finds the plug-in object a bundle ID belongs to. The only way to address a
/// HAL plug-in that is publishing no devices — which ours does until asked.
const TRANSLATE_BUNDLE_TO_PLUGIN: u32 = fourcc(b"bidp");

/// A device published by a plug-in rather than by hardware — ours, eqMac's,
/// aggregate devices, and so on.
pub const TRANSPORT_TYPE_VIRTUAL: u32 = fourcc(b"virt");
pub const TRANSPORT_TYPE_AGGREGATE: u32 = fourcc(b"grup");

#[repr(C)]
#[derive(Copy, Clone)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyData(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> i32;

    fn AudioObjectSetPropertyData(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: u32,
        data: *const c_void,
    ) -> i32;

    fn AudioObjectHasProperty(object: AudioObjectId, address: *const PropertyAddress) -> bool;

    fn AudioObjectIsPropertySettable(
        object: AudioObjectId,
        address: *const PropertyAddress,
        settable: *mut bool,
    ) -> i32;

    fn AudioObjectGetPropertyDataSize(
        object: AudioObjectId,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        data_size: *mut u32,
    ) -> i32;

    fn AudioDeviceCreateIOProcID(
        device: AudioObjectId,
        proc_: IoProc,
        client_data: *mut c_void,
        proc_id: *mut *mut c_void,
    ) -> i32;

    fn AudioDeviceDestroyIOProcID(device: AudioObjectId, proc_id: *mut c_void) -> i32;

    fn AudioDeviceStart(device: AudioObjectId, proc_id: *mut c_void) -> i32;

    fn AudioDeviceStop(device: AudioObjectId, proc_id: *mut c_void) -> i32;
}

/// `AudioDeviceIOProc`. Every pointer is opaque here: the probe that uses it
/// writes nothing, and the HAL has already zeroed the output buffers.
type IoProc = unsafe extern "C" fn(
    AudioObjectId,
    *const c_void,
    *const c_void,
    *const c_void,
    *mut c_void,
    *const c_void,
    *mut c_void,
) -> i32;

unsafe extern "C" fn silent_io(
    _device: AudioObjectId,
    _now: *const c_void,
    _input: *const c_void,
    _input_time: *const c_void,
    _output: *mut c_void,
    _output_time: *const c_void,
    _client_data: *mut c_void,
) -> i32 {
    0
}

fn address(selector: u32, scope: u32) -> PropertyAddress {
    PropertyAddress {
        selector,
        scope,
        element: ELEMENT_MAIN,
    }
}

fn get<T>(object: AudioObjectId, address: &PropertyAddress, out: &mut T) -> bool {
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            address,
            0,
            ptr::null(),
            &mut size,
            out as *mut T as *mut c_void,
        )
    };
    status == 0
}

fn set<T>(object: AudioObjectId, address: &PropertyAddress, value: &T) -> bool {
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            address,
            0,
            ptr::null(),
            std::mem::size_of::<T>() as u32,
            value as *const T as *const c_void,
        )
    };
    status == 0
}

pub fn default_output_device() -> Option<AudioObjectId> {
    let mut device: AudioObjectId = 0;
    get(
        SYSTEM_OBJECT,
        &address(DEFAULT_OUTPUT_DEVICE, SCOPE_GLOBAL),
        &mut device,
    )
    .then_some(device)
    .filter(|id| *id != 0)
}

/// The device the system plays alerts through and the volume keys act on.
pub fn default_system_output_device() -> Option<AudioObjectId> {
    let mut device: AudioObjectId = 0;
    let address = address(DEFAULT_SYSTEM_OUTPUT_DEVICE, SCOPE_GLOBAL);
    get(SYSTEM_OBJECT, &address, &mut device)
        .then_some(device)
        .filter(|id| *id != 0)
}

/// Points the system output — alerts and the volume keys — at `device`.
pub fn set_default_system_output_device(device: AudioObjectId) -> bool {
    let address = address(DEFAULT_SYSTEM_OUTPUT_DEVICE, SCOPE_GLOBAL);
    set(SYSTEM_OBJECT, &address, &device)
}

pub fn device_name(device: AudioObjectId) -> Option<String> {
    string_property(device, DEVICE_NAME, SCOPE_GLOBAL)
}

/// The device's persistent identifier. Unlike the name, this survives a rename
/// and identifies the same device across reboots — which is what a saved
/// preference has to key on.
pub fn device_uid(device: AudioObjectId) -> Option<String> {
    string_property(device, DEVICE_UID, SCOPE_GLOBAL)
}

/// Finds a device by its UID. `None` means the device is not present — for our
/// own device, that it is not installed or coreaudiod refused to load it.
pub fn device_by_uid(uid: &str) -> Option<AudioObjectId> {
    all_devices()
        .into_iter()
        .find(|device| device_uid(*device).as_deref() == Some(uid))
}

fn string_property(object: AudioObjectId, selector: u32, scope: u32) -> Option<String> {
    use objc2_core_foundation::{CFRetained, CFString};

    let mut value: *const CFString = ptr::null();
    let address = address(selector, scope);
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            &mut size,
            &mut value as *mut _ as *mut c_void,
        )
    };
    if status != 0 || value.is_null() {
        return None;
    }
    // These properties follow the Copy rule.
    let value = unsafe { CFRetained::from_raw(ptr::NonNull::new(value.cast_mut())?) };
    Some(value.to_string())
}

/// Writes a `CFStringRef`-valued property.
///
/// Only used for our driver's custom name selector — CoreAudio's own string
/// properties are all read-only.
pub fn set_string_property(object: AudioObjectId, selector: u32, value: &str) -> bool {
    use objc2_core_foundation::CFString;

    let string = CFString::from_str(value);
    let raw: *const CFString = &*string;
    let address = address(selector, SCOPE_GLOBAL);
    // SAFETY: the property takes a single CFStringRef, which is what is passed;
    // the driver copies it, so it need not outlive this call.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            std::mem::size_of::<*const CFString>() as u32,
            &raw as *const _ as *const c_void,
        )
    };
    status == 0
}

/// The object ID of an installed HAL plug-in, by its bundle identifier.
///
/// `None` means `coreaudiod` has not loaded a plug-in with that bundle ID —
/// which is the same answer whether it is missing or refused.
pub fn plugin_for_bundle_id(bundle_id: &str) -> Option<AudioObjectId> {
    use objc2_core_foundation::CFString;

    let string = CFString::from_str(bundle_id);
    let qualifier: *const CFString = &*string;
    let address = address(TRANSLATE_BUNDLE_TO_PLUGIN, SCOPE_GLOBAL);

    let mut plugin: AudioObjectId = 0;
    let mut size = std::mem::size_of::<AudioObjectId>() as u32;
    // SAFETY: the qualifier is the CFStringRef this property documents, and it
    // outlives the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &address,
            std::mem::size_of::<*const CFString>() as u32,
            &qualifier as *const _ as *const c_void,
            &mut size,
            &mut plugin as *mut _ as *mut c_void,
        )
    };
    (status == 0 && plugin != 0).then_some(plugin)
}

/// Writes a boolean-valued custom property — a `CFPropertyList` carrying a
/// `CFBoolean`, which is how a HAL plug-in declares one.
pub fn set_bool_property(object: AudioObjectId, selector: u32, value: bool) -> bool {
    use objc2_core_foundation::CFBoolean;

    // `kCFBooleanTrue` and `kCFBooleanFalse` are constants, not allocations, so
    // this borrows rather than owns.
    let boolean: &CFBoolean = CFBoolean::new(value);
    let raw: *const CFBoolean = boolean;
    let address = address(selector, SCOPE_GLOBAL);
    // SAFETY: a single CFPropertyListRef, which the plug-in copies out of.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            std::mem::size_of::<*const CFBoolean>() as u32,
            &raw as *const _ as *const c_void,
        )
    };
    status == 0
}

/// Writes a number to a property that takes a CFPropertyList — how a plug-in's
/// custom properties carry anything that is not a string.
pub fn set_number_property(object: AudioObjectId, selector: u32, value: i64) -> bool {
    use objc2_core_foundation::CFNumber;

    let number = CFNumber::new_i64(value);
    let raw: *const CFNumber = &*number;
    let address = address(selector, SCOPE_GLOBAL);
    // SAFETY: a single CFPropertyListRef, which the plug-in reads during the
    // call; `number` outlives it.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object,
            &address,
            0,
            ptr::null(),
            std::mem::size_of::<*const CFNumber>() as u32,
            &raw as *const _ as *const c_void,
        )
    };
    status == 0
}

/// Output latency the device reports, in frames — what a player adds to know
/// when a sample it writes is actually heard.
pub fn output_latency(device: AudioObjectId) -> Option<u32> {
    let mut value: u32 = 0;
    get(device, &address(LATENCY, SCOPE_OUTPUT), &mut value).then_some(value)
}

/// Output volume in 0.0..=1.0.
pub fn volume(device: AudioObjectId) -> Option<f64> {
    let mut value: f32 = 0.0;

    let virtual_main = address(VIRTUAL_MAIN_VOLUME, SCOPE_OUTPUT);
    if unsafe { AudioObjectHasProperty(device, &virtual_main) }
        && get(device, &virtual_main, &mut value)
    {
        return Some(value as f64);
    }

    let scalar = address(VOLUME_SCALAR, SCOPE_OUTPUT);
    if unsafe { AudioObjectHasProperty(device, &scalar) } && get(device, &scalar, &mut value) {
        return Some(value as f64);
    }
    None
}

pub fn set_volume(device: AudioObjectId, value: f64) -> bool {
    let value = value.clamp(0.0, 1.0) as f32;

    let virtual_main = address(VIRTUAL_MAIN_VOLUME, SCOPE_OUTPUT);
    if unsafe { AudioObjectHasProperty(device, &virtual_main) }
        && set(device, &virtual_main, &value)
    {
        return true;
    }

    let scalar = address(VOLUME_SCALAR, SCOPE_OUTPUT);
    let has_scalar = unsafe { AudioObjectHasProperty(device, &scalar) };
    has_scalar && set(device, &scalar, &value)
}

pub fn is_muted(device: AudioObjectId) -> Option<bool> {
    let mut value: u32 = 0;
    get(device, &address(MUTE, SCOPE_OUTPUT), &mut value).then_some(value != 0)
}

pub fn set_muted(device: AudioObjectId, muted: bool) -> bool {
    let value: u32 = u32::from(muted);
    set(device, &address(MUTE, SCOPE_OUTPUT), &value)
}

/// Every audio device the system knows about.
pub fn all_devices() -> Vec<AudioObjectId> {
    let address = address(DEVICES, SCOPE_GLOBAL);
    let mut size: u32 = 0;
    if unsafe { AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, ptr::null(), &mut size) }
        != 0
    {
        return Vec::new();
    }

    let count = size as usize / std::mem::size_of::<AudioObjectId>();
    let mut devices = vec![0 as AudioObjectId; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &address,
            0,
            ptr::null(),
            &mut size,
            devices.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    devices
}

/// Whether the device carries any output channels.
pub fn has_output(device: AudioObjectId) -> bool {
    has_channels(device, SCOPE_OUTPUT)
}

/// Whether the device carries any input channels. Our virtual device has both:
/// apps play into the output side, and we record the mix from the input side.
pub fn has_input(device: AudioObjectId) -> bool {
    has_channels(device, SCOPE_INPUT)
}

/// Channels the device carries on one scope, summed across its streams.
///
/// The size of the reply is not enough to go on: CoreAudio pads
/// `AudioBufferList` and hands back a whole one even when it describes no
/// buffers, so "bigger than the count field" is true for every device on both
/// scopes — which makes every microphone look like an output device.
fn channel_count(device: AudioObjectId, scope: u32) -> u32 {
    let address = address(STREAM_CONFIGURATION, scope);
    let mut size: u32 = 0;
    if unsafe { AudioObjectGetPropertyDataSize(device, &address, 0, ptr::null(), &mut size) } != 0 {
        return 0;
    }
    if (size as usize) < std::mem::size_of::<u32>() {
        return 0;
    }

    let mut bytes = vec![0u8; size as usize];
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            &address,
            0,
            ptr::null(),
            &mut size,
            bytes.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return 0;
    }

    // AudioBufferList: a count, then that many AudioBuffers. Read through the
    // bytes rather than casting, since the reply is not guaranteed aligned for
    // the struct.
    let count = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let buffers_at = std::mem::size_of::<AudioBufferHeader>();
    let stride = std::mem::size_of::<AudioBufferHeader>() + std::mem::size_of::<usize>();

    let mut channels = 0u32;
    for index in 0..count {
        let at = buffers_at + index * stride;
        if at + 4 > bytes.len() {
            break;
        }
        channels += u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    }
    channels
}

/// `mNumberChannels` and `mDataByteSize` — the two `UInt32`s at the head of an
/// `AudioBuffer`, and the same two that pad `AudioBufferList` before its first
/// buffer. Named for the arithmetic above rather than declared as the real
/// structs, which would drag `AudioBufferList` into a crate that has no other
/// use for it.
#[repr(C)]
struct AudioBufferHeader {
    _first: u32,
    _second: u32,
}

fn has_channels(device: AudioObjectId, scope: u32) -> bool {
    channel_count(device, scope) > 0
}

/// How many buffers a device contributes on one scope.
///
/// Needed because an aggregate device's input buffers arrive in a fixed order:
/// its sub-devices' input streams first, then its taps. An audio interface with
/// microphone inputs — a Scarlett, say — therefore pushes the tap along by
/// however many buffers it contributes, while speakers with no inputs push it
/// along by none.
pub fn buffer_count(device: AudioObjectId, input: bool) -> u32 {
    let scope = if input { SCOPE_INPUT } else { SCOPE_OUTPUT };
    let address = address(STREAM_CONFIGURATION, scope);
    let mut size: u32 = 0;
    if unsafe { AudioObjectGetPropertyDataSize(device, &address, 0, ptr::null(), &mut size) } != 0 {
        return 0;
    }
    if (size as usize) < std::mem::size_of::<u32>() {
        return 0;
    }

    let mut bytes = vec![0u8; size as usize];
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            &address,
            0,
            ptr::null(),
            &mut size,
            bytes.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return 0;
    }
    u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// How this device reaches the machine — USB, HDMI, virtual, and so on.
/// Compare against [`TRANSPORT_TYPE_VIRTUAL`] to tell plug-in devices apart
/// from real hardware.
pub fn transport_type(device: AudioObjectId) -> Option<u32> {
    let mut value: u32 = 0;
    get(device, &address(TRANSPORT_TYPE, SCOPE_GLOBAL), &mut value).then_some(value)
}

pub fn is_alive(device: AudioObjectId) -> bool {
    let mut value: u32 = 0;
    get(device, &address(IS_ALIVE, SCOPE_GLOBAL), &mut value) && value != 0
}

/// Whether some process — this one or any other — is playing through the
/// device right now. A device that is running is one whose I/O starts.
pub fn is_running_somewhere(device: AudioObjectId) -> bool {
    let mut value: u32 = 0;
    get(
        device,
        &address(IS_RUNNING_SOMEWHERE, SCOPE_GLOBAL),
        &mut value,
    ) && value != 0
}

/// Whether the device's I/O actually starts, found out by starting it with a
/// callback that plays silence and stopping it again.
///
/// Nothing short of this answers the question. An external display publishes
/// an audio device whether or not its link will carry audio, and one that will
/// not reports every property exactly as one that will — until I/O is asked to
/// start and Core Audio gives up waiting for the clock, returning
/// `kAudioHardwareNotRunningError` about ten seconds later. So this blocks for
/// that long on such a device, and belongs on a thread of its own.
pub fn io_starts(device: AudioObjectId) -> bool {
    let mut proc_id: *mut c_void = ptr::null_mut();
    // SAFETY: `silent_io` matches `AudioDeviceIOProc` and touches nothing, so
    // it needs no client data; the proc is destroyed on every path out.
    unsafe {
        if AudioDeviceCreateIOProcID(device, silent_io, ptr::null_mut(), &mut proc_id) != 0 {
            return false;
        }
        let started = AudioDeviceStart(device, proc_id) == 0;
        if started {
            AudioDeviceStop(device, proc_id);
        }
        AudioDeviceDestroyIOProcID(device, proc_id);
        started
    }
}

/// The rate the device is configured for.
pub fn nominal_sample_rate(device: AudioObjectId) -> Option<f64> {
    let mut value: f64 = 0.0;
    get(
        device,
        &address(NOMINAL_SAMPLE_RATE, SCOPE_GLOBAL),
        &mut value,
    )
    .then_some(value)
}

pub fn set_nominal_sample_rate(device: AudioObjectId, rate: f64) -> bool {
    set(device, &address(NOMINAL_SAMPLE_RATE, SCOPE_GLOBAL), &rate)
}

/// Frames of slack between where the device says it is and where it can safely
/// be read or written. Scope picks the direction.
pub fn safety_offset(device: AudioObjectId, output: bool) -> Option<u32> {
    let scope = if output { SCOPE_OUTPUT } else { SCOPE_INPUT };
    let mut value: u32 = 0;
    get(device, &address(SAFETY_OFFSET, scope), &mut value).then_some(value)
}

/// Frames the device hands over per IO cycle.
pub fn buffer_frame_size(device: AudioObjectId, output: bool) -> Option<u32> {
    let scope = if output { SCOPE_OUTPUT } else { SCOPE_INPUT };
    let mut value: u32 = 0;
    get(device, &address(BUFFER_FRAME_SIZE, scope), &mut value).then_some(value)
}

/// Makes this device the system's output. Everything that is not routed
/// somewhere specific follows it.
/// Like [`set_default_output_device`], but hands back the status code.
///
/// Worth having: a refusal here is silence with a working-looking route, and
/// the four-character code says which refusal it was.
pub fn set_default_output_device_status(device: AudioObjectId) -> i32 {
    let address = address(DEFAULT_OUTPUT_DEVICE, SCOPE_GLOBAL);
    // SAFETY: a well-formed address and a value of the size the property wants.
    unsafe {
        AudioObjectSetPropertyData(
            SYSTEM_OBJECT,
            &address,
            0,
            ptr::null(),
            std::mem::size_of::<AudioObjectId>() as u32,
            &device as *const AudioObjectId as *const c_void,
        )
    }
}

pub fn set_default_output_device(device: AudioObjectId) -> bool {
    set(
        SYSTEM_OBJECT,
        &address(DEFAULT_OUTPUT_DEVICE, SCOPE_GLOBAL),
        &device,
    )
}

/// Whether the system will let us change this device's volume.
///
/// Many outputs — HDMI and DisplayPort audio in particular — expose no settable
/// volume at all. Those are exactly the devices that need routing through a
/// virtual device to get a working volume slider.
pub fn volume_is_settable(device: AudioObjectId) -> bool {
    for selector in [VIRTUAL_MAIN_VOLUME, VOLUME_SCALAR] {
        let address = address(selector, SCOPE_OUTPUT);
        if !unsafe { AudioObjectHasProperty(device, &address) } {
            continue;
        }
        let mut settable = false;
        if unsafe { AudioObjectIsPropertySettable(device, &address, &mut settable) } == 0
            && settable
        {
            return true;
        }
    }
    false
}
