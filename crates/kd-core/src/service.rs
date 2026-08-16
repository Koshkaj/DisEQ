//! Backend selection and the actions the UI invokes.
//!
//! Every display exposes the same controls, but what actually drives them
//! differs per display and per machine, so the backend is resolved once at
//! startup and cached.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kd_sys::audio;
use kd_sys::brightness as panel_brightness;
use kd_sys::config::{self, Scope};
use kd_sys::ddc::{vcp, MatchConfidence};
use kd_sys::display::{self, DisplayId};
use kd_sys::gamma::{self, Adjustment};
use kd_sys::power::{self, PowerError, Strategy};

use crate::ddc::DdcService;
use crate::display::Display;
use crate::offline;

/// What is actually driving a display's brightness.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BrightnessBackend {
    /// Built-in panel, via the private brightness frameworks.
    Panel,
    /// External display over DDC/CI.
    Ddc,
    /// Gamma ramp — dims the signal, not the backlight.
    Software,
    /// Nothing available: no backlight control, no DDC, and gamma is ignored.
    None,
}

impl BrightnessBackend {
    pub fn caption(self) -> &'static str {
        match self {
            BrightnessBackend::Panel => "Brightness",
            BrightnessBackend::Ddc => "Brightness (DDC)",
            BrightnessBackend::Software => "Brightness (Software)",
            BrightnessBackend::None => "Brightness (unavailable)",
        }
    }

    pub fn is_available(self) -> bool {
        self != BrightnessBackend::None
    }
}

#[derive(Default)]
struct Cache {
    brightness: HashMap<u32, f64>,
    ddc_max: HashMap<u32, u16>,
}

pub struct Service {
    ddc: DdcService,
    backends: HashMap<u32, BrightnessBackend>,
    confidence: HashMap<u32, MatchConfidence>,
    cache: Arc<Mutex<Cache>>,
    strategy: Strategy,
}

/// Set whenever the display layout changes, because which backend drives a
/// display is decided by what answered at probe time — a display that was off
/// then has no backend at all, and would show an empty brightness slider for
/// the rest of the session.
static BACKENDS_STALE: AtomicBool = AtomicBool::new(false);

impl Service {
    pub fn start() -> Self {
        let ddc = DdcService::start();
        let mut service = Self {
            ddc,
            backends: HashMap::new(),
            confidence: HashMap::new(),
            cache: Arc::new(Mutex::new(Cache::default())),
            // Off means off: the hard disconnect is the only mechanism that
            // darkens the panel and takes the display out of the layout in one
            // step. Where the private symbol is missing it falls back to the
            // mirror trick at disconnect time.
            strategy: if power::hard_disconnect_available() {
                Strategy::HardDisconnect
            } else {
                Strategy::Mirror
            },
        };
        service.probe();
        kd_sys::watch::on_reconfiguration(|_, _| {
            BACKENDS_STALE.store(true, Ordering::Relaxed);
        });
        service
    }

    /// Re-resolves backends if the layout has changed since the last probe.
    ///
    /// Probing walks the DDC bus, so it is done when something moved rather
    /// than on every panel open.
    pub fn refresh_if_stale(&mut self) {
        if BACKENDS_STALE.swap(false, Ordering::Relaxed) {
            self.probe();
        }
    }

    /// Works out which backend drives each display, and seeds the value cache.
    pub fn probe(&mut self) {
        self.backends.clear();
        self.confidence.clear();

        let links: HashMap<u32, MatchConfidence> = crate::ddc::survey()
            .into_iter()
            .map(|(id, confidence)| (id.0, confidence))
            .collect();

        for id in display::online_displays() {
            let is_builtin = display::snapshot(id).is_builtin;

            // The panel frameworks answer for external displays too — they
            // report a plausible value and silently ignore writes — so the
            // built-in check has to come first rather than trusting them.
            let backend = if is_builtin && panel_brightness::is_supported(id) {
                BrightnessBackend::Panel
            } else if links.contains_key(&id.0) && self.ddc_answers(id) {
                BrightnessBackend::Ddc
            } else if gamma::verify(id) {
                BrightnessBackend::Software
            } else {
                BrightnessBackend::None
            };

            self.backends.insert(id.0, backend);
            if let Some(confidence) = links.get(&id.0) {
                self.confidence.insert(id.0, *confidence);
            }

            let value = match backend {
                BrightnessBackend::Panel => panel_brightness::get(id),
                BrightnessBackend::Ddc => None, // filled in asynchronously below
                BrightnessBackend::Software => Some(gamma::current(id).brightness),
                BrightnessBackend::None => None,
            };
            if let (Ok(mut cache), Some(value)) = (self.cache.lock(), value) {
                cache.brightness.insert(id.0, value);
            }
            if backend == BrightnessBackend::Ddc {
                self.refresh_ddc_brightness(id);
            }
        }
    }

    fn ddc_answers(&self, display: DisplayId) -> bool {
        self.ddc
            .read(display, vcp::LUMINANCE)
            .recv()
            .ok()
            .flatten()
            .is_some()
    }

    /// Reads DDC brightness on the worker and caches it when it lands, so
    /// opening the panel never blocks on I2C.
    fn refresh_ddc_brightness(&self, display: DisplayId) {
        let receiver = self.ddc.read(display, vcp::LUMINANCE);
        let cache = Arc::clone(&self.cache);
        std::thread::spawn(move || {
            if let Ok(Some(value)) = receiver.recv() {
                if value.max > 0 {
                    if let Ok(mut cache) = cache.lock() {
                        cache
                            .brightness
                            .insert(display.0, value.current as f64 / value.max as f64);
                        cache.ddc_max.insert(display.0, value.max);
                    }
                }
            }
        });
    }

    pub fn backend(&self, display: DisplayId) -> BrightnessBackend {
        self.backends
            .get(&display.0)
            .copied()
            .unwrap_or(BrightnessBackend::None)
    }

    pub fn ddc_confidence(&self, display: DisplayId) -> Option<MatchConfidence> {
        self.confidence.get(&display.0).copied()
    }

    pub fn brightness(&self, display: DisplayId) -> Option<f64> {
        if !self.backend(display).is_available() {
            return None;
        }
        self.cache
            .lock()
            .ok()
            .and_then(|cache| cache.brightness.get(&display.0).copied())
    }

    pub fn set_brightness(&self, display: DisplayId, value: f64) {
        let value = value.clamp(0.0, 1.0);
        if let Ok(mut cache) = self.cache.lock() {
            cache.brightness.insert(display.0, value);
        }

        match self.backend(display) {
            BrightnessBackend::Panel => {
                panel_brightness::set(display, value);
            }
            BrightnessBackend::Ddc => {
                let max = self
                    .cache
                    .lock()
                    .ok()
                    .and_then(|cache| cache.ddc_max.get(&display.0).copied())
                    .unwrap_or(100);
                self.ddc
                    .write(display, vcp::LUMINANCE, (value * max as f64) as u16);
            }
            BrightnessBackend::Software => {
                let mut adjustment = gamma::current(display);
                adjustment.brightness = value;
                gamma::apply(display, adjustment);
            }
            BrightnessBackend::None => {}
        }
    }

    // --- volume -------------------------------------------------------------

    pub fn volume(&self) -> Option<f64> {
        audio::volume(audio::default_output_device()?)
    }

    pub fn set_volume(&self, value: f64) -> bool {
        match audio::default_output_device() {
            Some(device) => audio::set_volume(device, value),
            None => false,
        }
    }

    pub fn output_name(&self) -> Option<String> {
        audio::device_name(audio::default_output_device()?)
    }

    pub fn is_muted(&self) -> Option<bool> {
        audio::is_muted(audio::default_output_device()?)
    }

    pub fn set_muted(&self, muted: bool) -> bool {
        match audio::default_output_device() {
            Some(device) => audio::set_muted(device, muted),
            None => false,
        }
    }

    // --- display configuration ----------------------------------------------

    /// Applies the mode at `index` in the display's selectable list.
    pub fn set_resolution(&self, display: &crate::Display, index: usize) -> bool {
        let modes = display.selectable_modes();
        let Some(mode) = modes.get(index) else {
            return false;
        };
        config::set_mode(display.id(), mode.io_mode_id, Scope::Permanent)
    }

    pub fn set_refresh_rate(&self, display: DisplayId, io_mode_id: i32) -> bool {
        config::set_mode(display, io_mode_id, Scope::Permanent)
    }

    pub fn set_main_display(&self, display: DisplayId) -> bool {
        config::set_main_display(display, Scope::Permanent)
    }

    pub fn set_hidpi(&self, display: DisplayId, enabled: bool) -> bool {
        match config::hidpi_twin(display, enabled) {
            Some(mode) => config::set_mode(display, mode.io_mode_id, Scope::Permanent),
            None => false,
        }
    }

    pub fn hidpi_available(&self, display: DisplayId, enabled: bool) -> bool {
        config::hidpi_twin(display, enabled).is_some()
    }

    pub fn set_mirror(&self, display: DisplayId, source: Option<DisplayId>) -> bool {
        match source {
            Some(source) => config::set_mirror(display, source, Scope::Session),
            None => config::clear_mirror(display, Scope::Session),
        }
    }

    pub fn move_display(&self, display: DisplayId, x: i32, y: i32) -> bool {
        config::set_origin(display, x, y, Scope::Permanent)
    }

    // --- connect / disconnect ------------------------------------------------

    pub fn strategy(&self) -> Strategy {
        self.strategy
    }

    pub fn is_connected(&self, display: DisplayId) -> bool {
        !offline::is_offline(display) && power::is_connected(display)
    }

    /// Takes the whole display rather than its id: a display that has been
    /// switched off is not in `CGGetOnlineDisplayList` any more, so its id is
    /// the one thing about it that cannot be looked up. Reconnecting safely
    /// needs the identity recorded before it went away.
    pub fn set_connected(&self, display: &Display, connected: bool) -> Result<(), PowerError> {
        let result = if connected {
            self.reconnect(display)
        } else {
            self.disconnect(display.id())
        };
        // The DDC handles for a display that just left the layout are stale,
        // and so is the backend map for one that just came back.
        self.ddc.rediscover();
        BACKENDS_STALE.store(true, Ordering::Relaxed);
        result
    }

    /// Takes the display out of the layout and turns its panel off.
    ///
    /// The hard disconnect does both at once. When it is unavailable or the
    /// system refuses it, the mirror trick removes the display from the layout
    /// and DDC standby takes care of the panel — mirroring on its own would
    /// leave a lit screen showing a duplicate desktop, which is not "off".
    fn disconnect(&self, display: DisplayId) -> Result<(), PowerError> {
        // Captured before the display goes away: once it is off the device
        // tree, its modes and name can no longer be read back.
        let snapshot = Display::load(display);

        if self.strategy == Strategy::HardDisconnect {
            match power::disconnect(display, Strategy::HardDisconnect) {
                Ok(()) => {
                    offline::record(&snapshot, Strategy::HardDisconnect);
                    return Ok(());
                }
                // Refusals are about the request, not the mechanism; falling
                // back would only refuse again.
                Err(error @ PowerError::LastActiveDisplay) => return Err(error),
                Err(_) => {}
            }
        }

        // Panel first: mirroring can invalidate the display's DDC handle, and a
        // dark panel with the desktop still on it is the worse half-state to be
        // stuck in, so it is also what gets undone if the mirror fails.
        self.set_panel_power(display, false);
        if let Err(error) = power::disconnect(display, Strategy::Mirror) {
            self.set_panel_power(display, true);
            return Err(error);
        }
        offline::record(&snapshot, Strategy::Mirror);
        Ok(())
    }

    /// Reverses whichever mechanism took the display out.
    ///
    /// Refuses outright when there is nothing on the other end of the cable, or
    /// when it is the built-in panel and the lid is shut. The private enable
    /// call does not fail in either case — it fabricates a display, and leaves
    /// the port in a state that survives a real monitor being plugged into it.
    fn reconnect(&self, display: &Display) -> Result<(), PowerError> {
        crate::power::may_connect(&display.snapshot)?;

        let id = display.id();
        let strategy = offline::strategy(id).unwrap_or_else(|| {
            // Not ours: a display sitting in a mirror set was mirrored by
            // something else, anything else answers to the hard disconnect.
            if display::snapshot(id).mirrors.is_some() {
                Strategy::Mirror
            } else {
                self.strategy
            }
        });

        if let Err(error) = power::connect(id, strategy) {
            // A record written in an earlier session carries that session's id,
            // and the window server hands out new ones on every boot. Rather
            // than guess which display the stale id meant, sweep every display
            // that is known but not online — the sweep judges each one by what
            // it produced and undoes the ones that produced nothing real.
            if strategy != Strategy::HardDisconnect || crate::power::restore_disabled() == 0 {
                return Err(error);
            }
        }
        if strategy == Strategy::Mirror {
            // Leaving the mirror set gives the display a fresh DDC handle; the
            // one from before would send the wake to nothing. Some monitors
            // drop DDC in standby entirely and need their own power button.
            self.ddc.rediscover();
            self.set_panel_power(id, true);
        }
        offline::forget(id);
        Ok(())
    }

    /// Undoes every disconnect this app is holding, whichever session made it.
    ///
    /// The escape hatch for a display that went off in a previous run: its id
    /// has changed since, so the card's own toggle may no longer reach it.
    pub fn reconnect_all(&self) -> usize {
        let before = display::online_displays().len();
        crate::power::restore_disabled();

        for id in display::online_displays() {
            // Only mirrors this app set: one the user arranged themselves is
            // not something to undo behind their back.
            if display::snapshot(id).mirrors.is_some() && offline::is_offline(id) {
                let _ = self.reconnect(&Display::load(id));
            }
        }

        // Whatever is in the layout under its own power is not switched off,
        // whatever a record left over from an earlier session still claims.
        for id in display::online_displays() {
            if power::is_connected(id) {
                offline::forget(id);
            }
        }

        self.ddc.rediscover();
        BACKENDS_STALE.store(true, Ordering::Relaxed);
        display::online_displays().len().saturating_sub(before)
    }

    /// Puts an external display's panel into standby over DDC, leaving it in
    /// the desktop layout.
    pub fn set_panel_power(&self, display: DisplayId, on: bool) {
        self.ddc
            .write(display, vcp::POWER_MODE, if on { 1 } else { 5 });
    }

    // --- colour --------------------------------------------------------------

    pub fn colour_adjustment(&self, display: DisplayId) -> Adjustment {
        gamma::current(display)
    }

    pub fn set_colour_adjustment(&self, display: DisplayId, adjustment: Adjustment) -> bool {
        gamma::apply(display, adjustment)
    }

    // --- Night Shift ---------------------------------------------------------

    /// Native Night Shift is global even though macOS presents it inside
    /// Displays settings. The UI exposes it on the main display only so it is
    /// not mistaken for independent per-monitor state.
    pub fn night_shift(&self) -> Option<bool> {
        kd_sys::night_shift::enabled()
    }

    pub fn set_night_shift(&self, enabled: bool) -> bool {
        kd_sys::night_shift::set_enabled(enabled)
    }

    // --- auto brightness ------------------------------------------------------

    pub fn auto_brightness(&self, display: DisplayId) -> Option<bool> {
        panel_brightness::auto_brightness(display)
    }

    pub fn set_auto_brightness(&self, display: DisplayId, enabled: bool) -> bool {
        panel_brightness::set_auto_brightness(display, enabled)
    }

    // --- DDC device control ---------------------------------------------------

    pub fn set_input_source(&self, display: DisplayId, source: u16) {
        self.ddc.write(display, vcp::INPUT_SOURCE, source);
    }
}
