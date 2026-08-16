//! Putting the two halves together: system audio out of a process tap, through
//! the EQ, out to real hardware.
//!
//! ```text
//! apps ─▶ global tap (muted) ─▶ ring
//!                                 │
//!    hardware ◀─ ×volume ◀─ varispeed ◀─ EQ ◀─┘
//! ```
//!
//! The virtual device is published while the route runs and taken away when it
//! stops, and it is made the system's output while it exists. It carries no
//! audio — the tap does that — but it carries the volume control the hardware
//! may not have, and it carries the name. That is what gives external speakers
//! a working slider in the menu bar: the slider moves our device's volume, this
//! reads it, and the gain is applied on the way to the hardware.
//!
//! The tap and the hardware run on unrelated clocks, so the ring and its
//! varispeed drift controller stay exactly as they were when the capture side
//! was a virtual device.

use std::sync::Arc;

use kd_sys::audio;

use crate::bridge::Bridge;
use crate::devices::{self, Device};
use crate::eq::Settings;
use crate::format;
use crate::playback::{Playback, PlaybackError};
use crate::shared::{SharedError, SharedRing};

/// Ring capacity in seconds of audio. Far more than the safety offset needs —
/// it costs 384 KB per second at 48 kHz stereo, and having it means a stalled
/// reader recovers instead of hearing a hole.
const RING_SECONDS: f64 = 2.0;

/// Added to the measured safety offset. Both devices report what they need in
/// the steady state; this covers the first few buffers, when neither is in one.
const SAFETY_MARGIN: i64 = 512;

#[derive(Debug)]
pub enum RouteError {
    /// There is no hardware output device to play to.
    NoHardwareOutput,
    /// Asked to play into a virtual device, which is a loop waiting to happen.
    WouldLoop,
    Capture(SharedError),
    Playback(PlaybackError),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHardwareOutput => write!(f, "there is no hardware output device to play to"),
            Self::WouldLoop => write!(f, "the target device is a virtual device"),
            Self::Capture(error) => write!(f, "{error}"),
            Self::Playback(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RouteError {}

impl From<SharedError> for RouteError {
    fn from(error: SharedError) -> Self {
        Self::Capture(error)
    }
}

impl From<PlaybackError> for RouteError {
    fn from(error: PlaybackError) -> Self {
        Self::Playback(error)
    }
}

/// A running route. Dropping it tears the route down and puts the system's
/// output device back where it was.
pub struct Route {
    /// Order matters on drop: playback stops reading before capture stops
    /// writing.
    playback: Playback,
    /// The driver's ring, mapped read-only. Held so the mapping outlives the
    /// render block that reads it.
    shared: Arc<SharedRing>,
    bridge: Arc<Bridge>,
    target: Device,
    /// The virtual device, published for as long as this route lives. `None`
    /// when the plug-in is not installed: the route still works, and the
    /// volume slider is the panel's own.
    driver: Option<Device>,
    /// The status code from the last attempt to claim the default output.
    claim_status: i32,
    /// Whether the system's output actually moved to it. Diagnostics: a route
    /// that failed to claim the default is one whose volume slider does
    /// nothing, and the difference is invisible otherwise.
    claimed: bool,
    /// What the system's output devices were before the route took them.
    previous: PreviousDefaults,
    /// Last volume read from the virtual device, so a change is noticed without
    /// reapplying the same gain thirty times a second.
    device_volume: f32,
    /// Set by the last tick when the volume moved outside the panel, so the UI
    /// can follow the menu-bar slider rather than fight it.
    volume_changed: Option<f32>,
    /// The rate the route was built at. The user can change the output device's
    /// rate from Audio MIDI Setup at any moment, and a graph built for another
    /// rate does not survive it.
    rate: f64,
    settings: Settings,
    /// Loudest sample seen in the last tick. Held here rather than read from
    /// the bridge on demand: reading the meter resets it, so exactly one
    /// reader may do it.
    level: f32,
}

impl Route {
    /// Routes everything the system plays through `target`.
    ///
    /// Makes the virtual device the system's default output, which is what
    /// puts every application's audio in front of the EQ.
    pub fn start(target: &Device, settings: Settings, volume: f32) -> Result<Self, RouteError> {
        if target.is_virtual() {
            return Err(RouteError::WouldLoop);
        }
        if !target.has_output {
            return Err(RouteError::NoHardwareOutput);
        }

        let rate = target
            .sample_rate
            .filter(|rate| *rate > 0.0)
            .unwrap_or(48_000.0);

        // The device the user will be pointed at: a stand-in for the hardware,
        // wearing its name, its sample rate and its volume. Published before
        // anything else so the system has somewhere to be sent.
        let driver = devices::publish();
        let previous = PreviousDefaults::take();

        // Adopt the hardware's own volume where it has one, rather than
        // imposing the last number the panel held. Switching output should not
        // change how loud the machine is.
        let volume = target
            .volume_is_settable
            .then(|| audio::volume(target.id))
            .flatten()
            .map(|volume| volume as f32)
            .unwrap_or(volume)
            .clamp(0.0, 1.0);

        let mut claimed = false;
        let mut claim_status = 0;
        if let Some(driver) = &driver {
            devices::set_name(driver, &devices::proxy_name(&target.name));
            // A stand-in that runs at another rate than the device it stands in
            // for makes every application resample twice over.
            if driver.sample_rate != Some(rate) {
                audio::set_nominal_sample_rate(driver.id, rate);
            }
            audio::set_volume(driver.id, f64::from(volume));
            audio::set_muted(driver.id, false);
            let (took, status) = claim_default(driver);
            claimed = took;
            claim_status = status;
        }

        let bridge = Arc::new(Bridge::new(
            format::CHANNELS,
            (rate * RING_SECONDS) as usize,
        ));
        bridge.set_safety_offset(safety_offset(target));

        // The ring has to exist before the engine that reads it. Mapping it is
        // also the check that the installed driver is the one this app was
        // built against.
        let started = (|| -> Result<(Arc<SharedRing>, Playback), RouteError> {
            let shared = Arc::new(SharedRing::open()?);
            let input_rate = shared
                .sample_rate()
                .max(shared.opened_sample_rate())
                .max(rate);
            let playback = Playback::start(
                target,
                input_rate,
                Arc::clone(&bridge),
                Arc::clone(&shared),
                // Unity. Where the master volume belongs is `apply_volume`'s
                // decision, and it is made below once the route exists — this
                // stage starts out of the way rather than guessing.
                1.0,
                &settings,
            )?;
            Ok((shared, playback))
        })();

        let (shared, playback) = match started {
            Ok(pair) => pair,
            Err(error) => {
                // A half-built route must not leave the system pointed at a
                // device nothing drains: that is silence with no explanation.
                previous.restore();
                devices::unpublish();
                return Err(error);
            }
        };

        let mut route = Self {
            playback,
            shared,
            bridge,
            target: target.clone(),
            driver,
            claimed,
            claim_status,
            previous,
            device_volume: volume.clamp(0.0, 1.0),
            volume_changed: None,
            rate,
            settings,
            level: 0.0,
        };
        // Stage the volume through the one place that knows where it belongs,
        // rather than leaving the mixer holding a copy of a gain the hardware is
        // already applying. Turning the effects on must not change how loud the
        // machine is.
        route.apply_volume(volume, true);
        Ok(route)
    }

    pub fn target(&self) -> &Device {
        &self.target
    }

    /// Stops routing without switching away from the hardware the user is
    /// currently hearing.
    ///
    /// The ordinary drop path restores the defaults captured when the route
    /// started. That is right while replacing a half-built route, but stale
    /// after the user has selected a different output and the route has been
    /// retargeted. An explicit switch-off should remove only our proxy, not
    /// undo the user's device choice.
    pub fn stop_on_target(mut self) {
        self.previous = PreviousDefaults {
            output: Some(self.target.id),
            system: Some(self.target.id),
        };
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Pushes new EQ settings at the running graph.
    pub fn apply(&mut self, settings: Settings) {
        self.playback.apply(&settings);
        self.settings = settings;
    }

    /// Sets the master volume, and mirrors it onto the virtual device so the
    /// menu bar shows what the panel shows.
    pub fn set_volume(&mut self, volume: f32) {
        self.apply_volume(volume, true);
    }

    /// Applies the master volume where it belongs.
    ///
    /// eqMac's rule, and the right one: hardware that has a volume control of
    /// its own gets driven directly, so the gain happens as late as possible
    /// and the device's own display agrees with ours. Hardware without one —
    /// the Dell over DisplayPort, most HDMI — is served by attenuating in our
    /// mixer instead, which is the whole reason the route exists.
    ///
    /// `mirror` writes the value back to the virtual device. False when the
    /// value came *from* the virtual device, which stops our own write being
    /// read back as a fresh user action.
    fn apply_volume(&mut self, volume: f32, mirror: bool) {
        let volume = volume.clamp(0.0, 1.0);
        if self.target.volume_is_settable {
            audio::set_volume(self.target.id, f64::from(volume));
            self.playback.set_volume(1.0);
        } else {
            self.playback.set_volume(volume);
        }
        if mirror {
            if let Some(driver) = &self.driver {
                audio::set_volume(driver.id, f64::from(volume));
            }
        }
        self.device_volume = volume;
    }

    /// The virtual device, while one is published.
    pub fn driver(&self) -> Option<&Device> {
        self.driver.as_ref()
    }

    /// Whether the system's output is the virtual device.
    pub fn claimed_default(&self) -> bool {
        self.claimed
    }

    /// Why it is not, when it is not. Core Audio's four-character status code.
    pub fn claim_status(&self) -> i32 {
        self.claim_status
    }

    /// Picks up a volume change made outside the panel — the menu-bar slider,
    /// the volume keys, another application.
    ///
    /// Returns the new volume when it moved, so the caller can follow it.
    /// Polled rather than watched: a property listener fires on a thread of the
    /// HAL's choosing, and none of this is `Send`.
    fn follow_device_volume(&mut self) -> Option<f32> {
        let driver = self.driver.as_ref()?;
        let muted = audio::is_muted(driver.id).unwrap_or(false);
        let volume = if muted {
            0.0
        } else {
            audio::volume(driver.id)? as f32
        };
        if (volume - self.device_volume).abs() < 0.001 {
            return None;
        }
        self.apply_volume(volume, false);
        Some(volume)
    }

    /// The master volume, wherever it is currently being applied.
    pub fn volume(&self) -> f32 {
        self.device_volume
    }

    /// The two places a gain can be applied on the way out: our own mixer, and
    /// the hardware's volume control where it has one.
    ///
    /// Their product is what the user hears, and it should equal [`Self::volume`]
    /// exactly — the volume is applied at one stage or the other, never both.
    /// Exposed because "applied twice" is inaudible as a bug and obvious as a
    /// number: it was applied twice across the driver boundary for a while, and
    /// at a low setting the product was silence.
    pub fn gain_stages(&self) -> (f32, Option<f64>) {
        (
            self.playback.volume(),
            self.target
                .volume_is_settable
                .then(|| audio::volume(self.target.id))
                .flatten(),
        )
    }

    pub fn is_running(&self) -> bool {
        self.shared.is_running() && self.playback.is_running()
    }

    /// One step of the drift controller. Call about
    /// [`crate::playback::TICKS_PER_SECOND`] times a second.
    pub fn tick(&mut self) -> Health {
        let rate = self.playback.tick();
        self.level = self.bridge.take_peak();
        self.volume_changed = self.follow_device_volume();
        Health {
            running: self.is_running(),
            rate,
            nominal_rate: self.playback.nominal_rate(),
            wanted_offset: self.bridge.safety_offset(),
            observed_offset: self.bridge.observed_offset(),
            realignments: self.bridge.realignments(),
            level: self.level,
        }
    }

    /// The volume the last tick picked up from the virtual device, if it moved.
    pub fn volume_changed(&self) -> Option<f32> {
        self.volume_changed
    }

    /// The rate the route was built for — the hardware's.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// The rate the driver is delivering at. Should match [`Self::rate`]: the
    /// ring indexes both ends by sample time, so two different rates make the
    /// offset walk away from the safety margin every buffer.
    pub fn capture_rate(&self) -> f64 {
        self.shared.sample_rate()
    }

    /// Whether the output device has been moved to another sample rate since
    /// the route was built — by Audio MIDI Setup, normally.
    ///
    /// A graph does not follow a rate change: it keeps rendering at the rate it
    /// was built for while the device runs at another, and what comes out is
    /// silence or a stutter. The caller rebuilds.
    pub fn rate_changed(&self) -> bool {
        match audio::nominal_sample_rate(self.target.id) {
            Some(rate) if rate > 0.0 => (rate - self.rate).abs() >= 1.0,
            _ => false,
        }
    }

    pub fn health(&self) -> Health {
        Health {
            running: self.is_running(),
            rate: self.playback.rate(),
            nominal_rate: self.playback.nominal_rate(),
            wanted_offset: self.bridge.safety_offset(),
            observed_offset: self.bridge.observed_offset(),
            realignments: self.bridge.realignments(),
            level: self.level,
        }
    }
}

impl Drop for Route {
    fn drop(&mut self) {
        // Order matters. Put the system back on a real device first, so the
        // moment our device disappears nothing is pointed at it; then take the
        // device away. The tap goes with the capture side, and dropping that
        // un-mutes every application it covered.
        self.previous.restore();
        if self.driver.is_some() {
            devices::unpublish();
        }
        self.bridge.reset();
    }
}

/// The output devices the system was using before the route took them.
///
/// Two, not one: `kAudioHardwarePropertyDefaultOutputDevice` is where audio
/// goes, and `...DefaultSystemOutputDevice` is where alerts go and what the
/// volume keys act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviousDefaults {
    pub output: Option<u32>,
    pub system: Option<u32>,
}

impl PreviousDefaults {
    fn take() -> Self {
        Self {
            output: audio::default_output_device(),
            system: audio::default_system_output_device(),
        }
    }

    fn restore(self) {
        if let Some(output) = self.output {
            audio::set_default_output_device(output);
        }
        if let Some(system) = self.system {
            audio::set_default_system_output_device(system);
        }
    }
}

/// What the route is doing, for a status line rather than for control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Health {
    pub running: bool,
    pub rate: f32,
    pub nominal_rate: f32,
    pub wanted_offset: i64,
    /// `None` until the two clocks have been related to each other.
    pub observed_offset: Option<f64>,
    pub realignments: u64,
    /// Loudest sample captured since the last report, 0..1. Zero with the route
    /// running means the input side is delivering silence — which is what a
    /// missing microphone permission looks like.
    pub level: f32,
}

impl Health {
    /// How far the actual lag has drifted from the intended one, in frames.
    pub fn drift(&self) -> Option<f64> {
        self.observed_offset
            .map(|observed| observed - self.wanted_offset as f64)
    }
}

/// Makes the virtual device the system's output, and checks that it took.
///
/// A device that has only just been published is discoverable before it is
/// eligible to be the default: the call is accepted, and the default stays
/// where it was. eqMac papers over the same lag with a flat 500 ms delay; this
/// asks and checks instead, which is quicker when the HAL is ready and more
/// patient when it is not.
///
/// The system output — where alerts go, and what the volume keys act on — is
/// taken with it. Taking only the first leaves those keys moving a device we do
/// not control, which on hardware with no volume of its own means they do
/// nothing at all.
fn claim_default(driver: &Device) -> (bool, i32) {
    let mut status = 0;
    for attempt in 0..12 {
        status = audio::set_default_output_device_status(driver.id);
        audio::set_default_system_output_device(driver.id);
        if audio::default_output_device() == Some(driver.id) {
            return (true, status);
        }
        if attempt == 0 {
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    (false, status)
}

/// How far behind the writer the reader should sit: everything the output can
/// be late by, twice over for the tap that feeds it, plus a margin for the
/// frames before either end has settled.
///
/// The tap publishes no safety offset of its own — it is not a device — so the
/// output's own figures stand in for both ends.
fn safety_offset(target: &Device) -> i64 {
    let safety = audio::safety_offset(target.id, true).unwrap_or(0);
    let buffer = audio::buffer_frame_size(target.id, true).unwrap_or(512);
    i64::from(safety + buffer) * 2 + SAFETY_MARGIN
}

/// Output devices worth routing to: real hardware, not our own driver and not
/// anything else virtual, since routing into another virtual device just moves
/// the problem.
pub fn targets() -> Vec<Device> {
    devices::outputs()
        .into_iter()
        .filter(|device| !device.is_diseq() && !device.is_virtual())
        .collect()
}

/// The device the user is most likely to mean: whatever the system is playing
/// through, unless that is already us.
///
/// The fallback matters more than it looks. When the default output is already
/// our own device — a previous run left it there, or the user picked it by hand
/// — the first device in enumeration order is as likely to be a monitor with no
/// speakers as anything else, and routing to it is silence that looks like
/// success. Built-in output is the one device that can be relied on to make a
/// sound.
pub fn default_target() -> Option<Device> {
    match devices::default_output() {
        Some(device) if !device.is_diseq() && !device.is_virtual() => Some(device),
        _ => {
            let candidates = targets();
            candidates
                .iter()
                .find(|device| device.transport_type_name() == "built-in")
                .cloned()
                .or_else(|| candidates.into_iter().next())
        }
    }
}
