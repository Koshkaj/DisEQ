//! Output devices, described rather than merely enumerated.
//!
//! `kd-sys::audio` is the raw property access. This is the view the rest of the
//! engine works with: which devices exist, which one is ours, and what each one
//! can and cannot do about its own volume.

use kd_sys::audio::{self, AudioObjectId};

/// The UID our HAL plug-in publishes. Matches `kDeviceUID` in
/// `driver/Source/DisEQ.c`; the two have to agree or nothing finds
/// anything.
pub const DISEQ_UID: &str = "DisEQDevice";

/// eqMac's driver, so we can say so when it is in the way.
pub const EQMAC_UID: &str = "EQMDevice";

/// The custom property the driver exposes for renaming the device. Matches
/// `kCustomProperty_Name`.
pub const CUSTOM_PROPERTY_NAME: u32 = u32::from_be_bytes(*b"kdnm");

/// The custom property the app reports the route's delay through, in frames.
/// Matches `kCustomProperty_Latency`.
pub const CUSTOM_PROPERTY_LATENCY: u32 = u32::from_be_bytes(*b"kdlt");

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: AudioObjectId,
    pub name: String,
    pub uid: Option<String>,
    pub has_output: bool,
    pub has_input: bool,
    /// Whether CoreAudio will let anyone change this device's volume. False for
    /// most HDMI and DisplayPort outputs — the reason the virtual device exists.
    pub volume_is_settable: bool,
    pub volume: Option<f64>,
    pub muted: Option<bool>,
    pub sample_rate: Option<f64>,
    pub transport_type: Option<u32>,
}

impl Device {
    pub fn describe(id: AudioObjectId) -> Self {
        Self {
            id,
            name: audio::device_name(id).unwrap_or_else(|| format!("device {id}")),
            uid: audio::device_uid(id),
            has_output: audio::has_output(id),
            has_input: audio::has_input(id),
            volume_is_settable: audio::volume_is_settable(id),
            volume: audio::volume(id),
            muted: audio::is_muted(id),
            sample_rate: audio::nominal_sample_rate(id),
            transport_type: audio::transport_type(id),
        }
    }

    pub fn is_diseq(&self) -> bool {
        self.uid.as_deref() == Some(DISEQ_UID)
    }

    pub fn is_virtual(&self) -> bool {
        self.transport_type == Some(audio::TRANSPORT_TYPE_VIRTUAL)
            || self.transport_type == Some(audio::TRANSPORT_TYPE_AGGREGATE)
    }

    /// A device that needs routing through ours to get a volume slider: real
    /// hardware that publishes no settable volume of its own.
    pub fn needs_software_volume(&self) -> bool {
        self.has_output && !self.volume_is_settable && !self.is_virtual()
    }

    pub fn transport_type_name(&self) -> &'static str {
        match self.transport_type {
            Some(value) => match &value.to_be_bytes() {
                b"virt" => "virtual",
                b"grup" => "aggregate",
                b"bltn" => "built-in",
                b"usb " => "USB",
                b"hdmi" => "HDMI",
                b"dprt" => "DisplayPort",
                b"blue" => "Bluetooth",
                b"airp" => "AirPlay",
                b"3915" => "Thunderbolt",
                b"pci " => "PCI",
                b"aggr" => "aggregate",
                _ => "other",
            },
            None => "unknown",
        }
    }
}

/// Every device with output channels, described.
///
/// Except any whose I/O is being tried right now: such a device answers nothing
/// else until the attempt ends, which on one that will not start is ten
/// seconds, and whoever asked — the panel opening, as a rule — waits with it.
/// It is not listed until it has been tried anyway.
pub fn outputs() -> Vec<Device> {
    audio::all_devices()
        .into_iter()
        .filter(|id| !crate::playable::is_being_tried(*id))
        .filter(|id| audio::has_output(*id))
        .map(Device::describe)
        .collect()
}

/// Every device the system knows about, output or not.
pub fn all() -> Vec<Device> {
    audio::all_devices()
        .into_iter()
        .map(Device::describe)
        .collect()
}

pub fn default_output() -> Option<Device> {
    audio::default_output_device().map(Device::describe)
}

pub fn by_uid(uid: &str) -> Option<Device> {
    audio::device_by_uid(uid).map(Device::describe)
}

/// Our virtual device, if the driver is installed and it is currently
/// published. Hidden is indistinguishable from absent by design.
///
/// Only ever a *live* object. A retracted device stays in the system's device
/// list for a few tens of milliseconds after it goes, still answering with its
/// UID — and every write to it succeeds and does nothing. Filtering on
/// `is_alive` is what keeps a publish immediately after a retract from handing
/// back the corpse of the last one.
pub fn diseq() -> Option<Device> {
    audio::all_devices()
        .into_iter()
        // Newest wins: during the overlap there can be two, and the one that
        // matters is the one that just arrived.
        .rfind(|id| audio::is_alive(*id) && audio::device_uid(*id).as_deref() == Some(DISEQ_UID))
        .map(Device::describe)
}

/// The bundle identifier of the HAL plug-in, which is how the plug-in object is
/// found while it is publishing no devices.
pub const DRIVER_BUNDLE_ID: &str = "com.koshka.DisEQ.driver";

/// Selector the plug-in publishes for its own visibility.
pub const CUSTOM_PROPERTY_SHOWN: u32 = u32::from_be_bytes(*b"kdsh");

/// Whether the plug-in is installed at all, published or not.
pub fn driver_is_installed() -> bool {
    audio::plugin_for_bundle_id(DRIVER_BUNDLE_ID).is_some()
}

/// Publishes the virtual device, and waits for the HAL to catch up.
///
/// The device appears with the application and goes away with it, which is what
/// eqMac does and the reason a machine with the driver installed but nothing
/// running has no stray entry in Sound settings.
///
/// Returns the device once the system can see it. `None` means the plug-in is
/// not loaded, or did not publish within the timeout — the property write is
/// asynchronous on the HAL's side, so the wait is not optional.
pub fn publish() -> Option<Device> {
    let plugin = audio::plugin_for_bundle_id(DRIVER_BUNDLE_ID)?;
    if !audio::set_bool_property(plugin, CUSTOM_PROPERTY_SHOWN, true) {
        return None;
    }
    for _ in 0..40 {
        if let Some(device) = diseq() {
            return Some(device);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

/// How long to wait for the HAL to finish a publish or a retract.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Takes the virtual device away again. Safe to call when it was never
/// published, and worth calling on the way out of anything that published it:
/// a device nobody is draining is silence with an explanation nobody can see.
pub fn unpublish() -> bool {
    let Some(plugin) = audio::plugin_for_bundle_id(DRIVER_BUNDLE_ID) else {
        return false;
    };
    if !audio::set_bool_property(plugin, CUSTOM_PROPERTY_SHOWN, false) {
        return false;
    }
    // Wait for it to actually go. A publish that starts while the last device
    // is still being torn down finds the old object first, and everything
    // written to it — the default-device change, the volume, the name — is
    // accepted and discarded.
    for _ in 0..20 {
        if diseq().is_none() {
            return true;
        }
        std::thread::sleep(SETTLE);
    }
    true
}

/// Whether eqMac's driver is present. Both want to own the default output, so
/// the app says so rather than fighting over it.
pub fn eqmac_is_installed() -> bool {
    by_uid(EQMAC_UID).is_some()
}

/// What the virtual device calls itself while it proxies `target`.
///
/// eqMac's convention, and the reason its device reads like the user's own
/// hardware: the entry in Sound settings names what is actually on the other
/// end of the route.
pub fn proxy_name(target: &str) -> String {
    format!("{target} (DisEQ)")
}

/// Renames our virtual device. The driver only accepts this from our own
/// bundle ID, so it fails silently from anything else — an example binary, for
/// instance.
pub fn set_name(device: &Device, name: &str) -> bool {
    audio::set_string_property(device.id, CUSTOM_PROPERTY_NAME, name)
}

/// Tells the driver how far behind our device the hardware actually plays, so
/// it can report that as the device's latency. Like renaming, only accepted
/// from our own bundle ID; a driver older than this ignores it and keeps
/// reporting zero.
pub fn set_latency(device: &Device, frames: u32) -> bool {
    audio::set_number_property(device.id, CUSTOM_PROPERTY_LATENCY, i64::from(frames))
}

pub fn set_default_output(device: &Device) -> bool {
    audio::set_default_output_device(device.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_uid_is_the_one_the_c_source_publishes() {
        // Cheap guard against the two drifting apart in a rename.
        let source = include_str!("../../../driver/Source/DisEQ.c");
        assert!(
            source.contains(&format!("#define kDeviceUID          \"{DISEQ_UID}\"")),
            "driver/Source/DisEQ.c no longer publishes {DISEQ_UID}"
        );
    }

    #[test]
    fn the_custom_name_selector_matches_the_driver() {
        let source = include_str!("../../../driver/Source/DisEQ.c");
        let fourcc = String::from_utf8(CUSTOM_PROPERTY_NAME.to_be_bytes().to_vec()).unwrap();
        assert!(
            source.contains(&format!("#define kCustomProperty_Name '{fourcc}'")),
            "driver/Source/DisEQ.c no longer uses '{fourcc}' for the name property"
        );
    }

    #[test]
    fn the_custom_latency_selector_matches_the_driver() {
        let source = include_str!("../../../driver/Source/DisEQ.c");
        let fourcc = String::from_utf8(CUSTOM_PROPERTY_LATENCY.to_be_bytes().to_vec()).unwrap();
        assert!(
            source.contains(&format!("#define kCustomProperty_Latency '{fourcc}'")),
            "driver/Source/DisEQ.c no longer uses '{fourcc}' for the latency property"
        );
    }

    #[test]
    fn enumeration_survives_whatever_this_machine_has() {
        // Not an assertion about the machine — just that describing every
        // device it does have does not panic or hang.
        for device in all() {
            assert!(!device.name.is_empty());
        }
    }
}
