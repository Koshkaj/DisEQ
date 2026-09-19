//! Which outputs will actually play.
//!
//! An external display publishes an audio device whenever it is plugged in,
//! whether or not its link will carry audio, and nothing about the device says
//! which. One that will not looks exactly like one that will — same formats,
//! same rates, same "can be default" — until I/O is started and Core Audio
//! gives up waiting for its clock ten seconds later. Picking it was a ten-second
//! freeze followed by an error, with the route gone.
//!
//! So display audio is tried before it is offered: its I/O is started on a
//! thread of its own, and the device is listed only once that has worked. Only
//! display audio. Starting I/O on an AirPlay or Bluetooth output is not a
//! harmless check — it connects to the speaker, or pulls the headphones over
//! from another device — and those outputs do not have this failure anyway.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kd_sys::audio;

use crate::devices::Device;

/// Devices whose I/O is being started right now, across every [`Playable`].
///
/// Process-wide because the hazard is: while `AudioDeviceStart` is waiting on a
/// device, every other request for that device from this process waits behind
/// it — its name, its streams, all of it. Enumerating outputs meanwhile stalled
/// the first panel open for the full ten seconds.
fn being_tried() -> &'static Mutex<HashSet<u32>> {
    static BEING_TRIED: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    BEING_TRIED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Whether `id`'s I/O is being tried at this moment, in which case asking it
/// anything waits until the attempt ends.
pub fn is_being_tried(id: u32) -> bool {
    being_tried().lock().is_ok_and(|ids| ids.contains(&id))
}

/// How long a verdict is believed before the output is tried again. A link can
/// come back, and one that worked can stop; neither announces it.
const STALE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    /// `None` until the first attempt finishes.
    plays: Option<bool>,
    checked: Option<Instant>,
    in_flight: bool,
}

/// Verdicts on display-audio outputs, by device id. A monitor plugged back in
/// comes back as a new device, and is tried afresh.
#[derive(Clone, Default)]
pub struct Playable {
    entries: Arc<Mutex<HashMap<u32, Entry>>>,
    /// Bumped whenever a verdict changes, so whatever is showing the list can
    /// tell it has.
    generation: Arc<AtomicU32>,
}

impl Playable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether to list `device` as somewhere to play.
    ///
    /// Display audio is held back until it has been tried, rather than listed
    /// on the chance that it works: a listed output that fails costs ten
    /// seconds to find out. One that something is already playing through
    /// needs no trying.
    pub fn offers(&self, device: &Device) -> bool {
        if !needs_trying(device) {
            return true;
        }
        match self.entry(device).and_then(|entry| entry.plays) {
            Some(plays) => plays,
            None => audio::is_running_somewhere(device.id),
        }
    }

    /// Whether `device` is known not to play. Unlike [`Self::offers`], an
    /// output that has not been tried yet is not held against it.
    pub fn failed(&self, device: &Device) -> bool {
        needs_trying(device) && self.entry(device).and_then(|entry| entry.plays) == Some(false)
    }

    /// Tries every display-audio output in `devices` that has not been tried,
    /// or not recently. Returns at once; the attempts run on threads of their
    /// own, and [`Self::generation`] moves as each one changes a verdict.
    pub fn check(&self, devices: &[Device]) {
        let now = Instant::now();
        for device in devices.iter().filter(|device| needs_trying(device)) {
            let id = device.id;
            {
                let Ok(mut entries) = self.entries.lock() else {
                    return;
                };
                let entry = entries.entry(id).or_default();
                let fresh = entry
                    .checked
                    .is_some_and(|checked| now.duration_since(checked) < STALE);
                if entry.in_flight || fresh {
                    continue;
                }
                // Something is playing through it, so it plays: no need to
                // start I/O on a device that is already running.
                if audio::is_running_somewhere(id) {
                    entry.checked = Some(now);
                    if entry.plays.replace(true) != Some(true) {
                        self.generation.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
                entry.in_flight = true;
            }

            let entries = Arc::clone(&self.entries);
            let generation = Arc::clone(&self.generation);
            if let Ok(mut ids) = being_tried().lock() {
                ids.insert(id);
            }
            std::thread::spawn(move || {
                let plays = audio::io_starts(id);
                if let Ok(mut ids) = being_tried().lock() {
                    ids.remove(&id);
                }
                let changed = entries.lock().is_ok_and(|mut entries| {
                    let entry = entries.entry(id).or_default();
                    entry.checked = Some(Instant::now());
                    entry.in_flight = false;
                    entry.plays.replace(plays) != Some(plays)
                });
                if changed {
                    generation.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    }

    /// Records an output that failed when it was actually used, so it stops
    /// being listed now rather than after its next check.
    pub fn record_failure(&self, device: &Device) {
        if !needs_trying(device) {
            return;
        }
        let changed = self.entries.lock().is_ok_and(|mut entries| {
            let entry = entries.entry(device.id).or_default();
            entry.checked = Some(Instant::now());
            entry.plays.replace(false) != Some(false)
        });
        if changed {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Moves whenever a verdict changes. A check that confirms what was
    /// already known leaves it alone, so re-trying does not redraw anything.
    pub fn generation(&self) -> u32 {
        self.generation.load(Ordering::Relaxed)
    }

    fn entry(&self, device: &Device) -> Option<Entry> {
        self.entries.lock().ok()?.get(&device.id).copied()
    }
}

/// Display audio: the outputs that can be listed and not play, and that can be
/// tried without side effects.
fn needs_trying(device: &Device) -> bool {
    matches!(
        device.transport_type.map(u32::to_be_bytes).as_ref(),
        Some(b"hdmi" | b"dprt")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An id no Core Audio object has, so nothing here touches hardware.
    const NO_SUCH_DEVICE: u32 = 0x7FFF_FFF0;

    fn device(transport: &[u8; 4]) -> Device {
        Device {
            id: NO_SUCH_DEVICE,
            name: "test".into(),
            uid: None,
            has_output: true,
            has_input: false,
            volume_is_settable: false,
            volume: None,
            muted: None,
            sample_rate: Some(48_000.0),
            transport_type: Some(u32::from_be_bytes(*transport)),
        }
    }

    #[test]
    fn only_display_audio_is_held_back() {
        let playable = Playable::new();
        assert!(playable.offers(&device(b"usb ")));
        assert!(playable.offers(&device(b"bltn")));
        assert!(playable.offers(&device(b"blue")));
        assert!(playable.offers(&device(b"airp")));
        assert!(!playable.offers(&device(b"dprt")));
        assert!(!playable.offers(&device(b"hdmi")));
    }

    /// Not yet tried is a reason not to list it, but not a reason to refuse
    /// it when it is the only thing left to play to.
    #[test]
    fn an_untried_output_has_not_failed() {
        let playable = Playable::new();
        assert!(!playable.failed(&device(b"hdmi")));
    }

    #[test]
    fn a_failure_in_use_stops_it_being_listed() {
        let playable = Playable::new();
        let before = playable.generation();
        let monitor = device(b"dprt");
        playable.record_failure(&monitor);
        assert!(!playable.offers(&monitor));
        assert!(playable.failed(&monitor));
        assert_ne!(playable.generation(), before);
    }

    /// Hardware that is not display audio never gets a verdict to lose.
    #[test]
    fn a_failure_on_other_hardware_changes_nothing() {
        let playable = Playable::new();
        let interface = device(b"usb ");
        playable.record_failure(&interface);
        assert!(playable.offers(&interface));
        assert!(!playable.failed(&interface));
    }
}
