//! What the Sound section of the panel drives.
//!
//! `kd-audio` has the engine; this owns the parts of it the UI can turn on and
//! off, and answers the questions the UI has to ask before it offers a control:
//! is the driver loaded, are taps permitted, is something else already holding
//! the default output.
//!
//! Both features are off until asked for. Routing takes over the system's
//! default output device and the App Mixer mutes applications, and neither is
//! something to do because the panel was opened.

use kd_audio::devices::{self, Device};
use kd_audio::eq::{self, Equalizer, Settings};
use kd_audio::mixer::{Mixer, MAX_FADERS};
use kd_audio::processes::{self, Process};
use kd_audio::router::{self, Health, Route};
use kd_core::Display;

use crate::prefs::{Config, EqConfig};

/// Ticks between checks of the system's default output device. At a ~33 ms
/// frame this is about 4 Hz: fast enough that picking a device in Sound
/// settings feels immediate, slow enough not to ask the HAL thirty times a
/// second for something that changes once an hour.
const POLL_TICKS: u32 = 8;

/// Ticks to ignore device changes for after making one ourselves.
///
/// Taking the default output is not instant, and for a moment afterwards the
/// system still reports the old device. Without a settling window the next poll
/// reads that as "the user picked something else", rebuilds the route at the
/// old device, and the two chase each other — the flapping eqMac avoids with
/// the same trick under the name `ignoreEvents`. At a ~33 ms frame this is
/// about a second, which is what eqMac waits too.
const SETTLE_TICKS: u32 = 30;

/// Ticks between writes of the settings file. A slider drag changes a value
/// thirty times a second and none of those are worth a write; a second's delay
/// is imperceptible and the file is written on the way out anyway.
const SAVE_TICKS: u32 = 30;

/// The part of a device poll the Sound card draws, sampled either side of
/// [`SoundService::follow_system_output`] so a poll can say whether the panel
/// has anything new to show.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Poll {
    routing: bool,
    output: String,
}

impl Poll {
    /// Whether the panel has to be rebuilt to show this poll.
    ///
    /// Routing steadily is deliberately not a change. It used to count as one,
    /// which redrew the entire panel about four times a second to animate a
    /// level meter the card no longer has. Every rebuild replaces the row under
    /// the pointer, and a fresh tracking area under a pointer that is not
    /// moving never receives `mouseEntered:` — so the highlight blinked — while
    /// a rebuild landing mid-press threw away the button holding the mouse down
    /// and swallowed the click.
    fn changed(&self, next: &Self) -> bool {
        self != next
    }
}

/// What this machine will let the Sound section do.
#[derive(Clone, Debug)]
pub struct Availability {
    /// The HAL plug-in is loaded. Everything the equaliser does depends on it:
    /// the audio arrives because the plug-in is the output device, which is
    /// also why none of it needs a capture permission.
    pub driver: bool,
}

impl Availability {
    fn probe() -> Self {
        Self {
            driver: devices::driver_is_installed(),
        }
    }

    /// Why the equaliser cannot be switched on, if it cannot.
    pub fn eq_blocked(&self) -> Option<String> {
        (!self.driver).then(|| "Driver not installed — run 'make driver-install'".to_string())
    }

    /// Why the App Mixer cannot be switched on, if it cannot.
    ///
    /// Never answered in advance. Asking costs a process tap, and creating one
    /// is what raises the system-audio-recording prompt — which belongs at the
    /// moment the user switches the mixer on, not at every launch. A refusal
    /// arrives as the error from [`SoundService::set_mixing`] instead.
    pub fn mixer_blocked(&self) -> Option<String> {
        None
    }
}

/// One application's fader, as the panel shows it.
#[derive(Clone, Debug)]
pub struct AppFader {
    pub process: Process,
    pub gain: f32,
}

impl AppFader {
    /// What to write on the row. Bundle IDs are what Core Audio gives us, and
    /// the last meaningful component reads better than the whole thing.
    pub fn name(&self) -> String {
        let Some(bundle) = self.process.bundle_id.as_deref() else {
            return format!("pid {}", self.process.pid);
        };
        // `com.brave.Browser.helper` is Brave, not "helper": the part that
        // names the application is the one before any helper suffix.
        let trimmed = bundle
            .strip_suffix(".helper")
            .or_else(|| bundle.split(".helper").next())
            .unwrap_or(bundle);
        let name = trimmed.rsplit('.').next().unwrap_or(trimmed);
        let mut characters = name.chars();
        match characters.next() {
            Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
            None => bundle.to_string(),
        }
    }
}

pub struct SoundService {
    availability: Availability,
    equalizer: Equalizer,
    /// The hardware the route plays to. `None` when there is no output at all.
    target: Option<Device>,
    route: Option<Route>,
    mixer: Option<Mixer>,
    /// Master volume, 0..1. Applies to the route when one is running and to the
    /// output device otherwise, so the slider means the same thing either way.
    volume: f64,
    /// Frames since construction, so the device poll can run at its own rate
    /// rather than the ramp's.
    ticks: u32,
    /// Ticks left before device changes are believed again. Set whenever we
    /// change the system's output ourselves.
    settle: u32,
    /// How many routes have been started. Churn here is a rebuild loop, and a
    /// rebuild loop is silence.
    generation: u32,
    /// Everything that outlives the process.
    config: Config,
    /// Whether `config` has changed since it was last written.
    dirty: bool,
}

impl SoundService {
    pub fn new() -> Self {
        // Retract first, choose second. A device still published at launch is
        // one a previous run left behind by being killed rather than quitting,
        // and it is the system's output — so asking "what is the system playing
        // through?" before retracting it answers "itself", and the real answer
        // is whatever the system falls back to once it is gone. Choosing the
        // target from the stale state is how a relaunch ends up routing to a
        // device the user is not listening to, which they then have to correct
        // by switching output by hand.
        if devices::diseq().is_some() {
            devices::unpublish();
        }

        let config = Config::load();
        let target = remembered_target(&config);
        // The saved volume, not the device's: the point of persistence is that
        // the machine comes back the way it was left.
        let volume = config.volume.clamp(0.0, 1.0);

        // Restored without a ramp — a launch comes up already at the settings it
        // was left at rather than gliding into them over half a second.
        //
        // The gains are the record, not the preset name: choosing a preset
        // writes its gains, and moving a band afterwards leaves gains that no
        // preset matches. Applying the saved name on top would throw those
        // edits away. The label is derived from the gains, which gets it right
        // in both cases.
        let equalizer = Equalizer::new(config.eq.settings());

        // What a launch restored, once, so a settings file that was not read the
        // way it was meant can be seen rather than inferred.
        if let Some(destination) = std::env::var_os("KD_SOUND_LOG") {
            log_line(
                &destination,
                &format!(
                    "loaded: preset {} · preamp {:+.1} · gains {:?} · volume {:.0}% · routing {} · mixing {}",
                    equalizer.selected_preset(),
                    equalizer.target().preamp,
                    equalizer.target().gains,
                    config.volume * 100.0,
                    config.routing,
                    config.mixing
                ),
            );
        }

        let mut service = Self {
            availability: Availability::probe(),
            equalizer,
            target,
            route: None,
            mixer: None,
            volume,
            ticks: 0,
            settle: 0,
            generation: 0,
            config,
            dirty: false,
        };
        // The route comes up with the application rather than waiting to be
        // switched on. Two reasons. The virtual device is silent unless
        // something drains it, so an installed driver the user selects by hand
        // is a bug report otherwise; and taking the default output later means
        // whatever is already playing stutters as it moves.
        if service.config.routing {
            // Captured before the route starts: `Route::start` adopts the
            // hardware's own volume and overwrites this, and the saved one is
            // the one the user chose.
            let saved = service.volume;
            let _ = service.set_routing(true);
            service.set_volume(saved);
        }
        if service.config.mixing {
            let _ = service.set_mixing(true);
        }
        service
    }

    /// Re-reads what the machine offers. Cheap enough to call when the panel
    /// opens, which is when a driver installed since launch should appear.
    pub fn refresh(&mut self) {
        self.availability = Availability::probe();
        match &self.route {
            // While routing, the device the user hears is the route's, not the
            // system's — the system's is ours.
            Some(route) => self.target = Some(route.target().clone()),
            None => {
                self.target = router::default_target();
                if let Some(volume) = self.target.as_ref().and_then(|device| device.volume) {
                    self.volume = volume;
                }
                // A driver installed since launch, or a device selected by hand
                // while the route was down.
                self.follow_system_output();
            }
        }
    }

    pub fn availability(&self) -> &Availability {
        &self.availability
    }

    /// The device name to show in the header: what the user is actually
    /// hearing, which is the route's target rather than the default output.
    pub fn output_name(&self) -> String {
        match (&self.route, &self.target) {
            (Some(route), _) => route.target().name.clone(),
            (None, Some(target)) => target.name.clone(),
            (None, None) => "No output".into(),
        }
    }

    // --- equaliser ----------------------------------------------------------

    pub fn is_routing(&self) -> bool {
        self.route.is_some()
    }

    pub fn health(&self) -> Option<Health> {
        self.route.as_ref().map(|route| route.health())
    }

    pub fn settings(&self) -> &Settings {
        self.equalizer.settings()
    }

    pub fn selected_preset(&self) -> &'static str {
        self.equalizer.selected_preset()
    }

    // --- display-card state ------------------------------------------------

    /// Whether this display's card is open far enough to show its primary
    /// controls. The key describes the hardware, not its transient display id,
    /// so the choice survives restarts and display enumeration changes.
    pub fn display_card_is_open(&self, display: &Display) -> bool {
        self.config
            .open_displays
            .contains(&display_persistence_key(display))
    }

    /// Toggles and immediately saves a display card's primary disclosure.
    /// Unlike slider changes this is a single, infrequent click, so there is no
    /// reason to debounce it and risk losing the choice before shutdown.
    pub fn toggle_display_card(&mut self, display: &Display) -> bool {
        let key = display_persistence_key(display);
        let open = if self.config.open_displays.remove(&key) {
            false
        } else {
            self.config.open_displays.insert(key);
            true
        };
        self.dirty = true;
        self.save();
        open
    }

    /// Forces an offline display back to its compact state and persists it.
    pub fn close_display_card(&mut self, display: &Display) {
        if self
            .config
            .open_displays
            .remove(&display_persistence_key(display))
        {
            self.dirty = true;
            self.save();
        }
    }

    pub fn sound_card_is_open(&self) -> bool {
        self.config.sound_open
    }

    /// Toggles the Sound card's primary disclosure and saves it immediately.
    pub fn toggle_sound_card(&mut self) -> bool {
        self.config.sound_open = !self.config.sound_open;
        self.dirty = true;
        self.save();
        self.config.sound_open
    }

    /// Hardware outputs offered by the source picker.
    pub fn outputs(&self) -> Vec<Device> {
        router::targets()
    }

    pub fn output_is_selected(&self, device: &Device) -> bool {
        self.target.as_ref().map(|target| target.id) == Some(device.id)
    }

    /// Moves both direct and enhanced audio to a user-selected output.
    pub fn select_output(&mut self, device_id: u32) -> Result<(), String> {
        let Some(target) = router::targets()
            .into_iter()
            .find(|device| device.id == device_id)
        else {
            return Err("That output is no longer available".into());
        };
        if self.output_is_selected(&target) {
            return Ok(());
        }

        // A mixer writes to the route's proxy when enhancement is active and
        // directly to the hardware otherwise. Either destination changes here,
        // so rebuild its taps after the output has moved.
        let was_mixing = self.mixer.is_some();
        let moved = if self.route.is_some() {
            self.mixer = None;
            self.retarget(target)
        } else {
            if !devices::set_default_output(&target) {
                return Err("The system refused the output change".into());
            }
            kd_sys::audio::set_default_system_output_device(target.id);
            self.mixer = None;
            if let Some(volume) = target.volume {
                self.volume = volume;
            }
            self.remember_target(&target);
            self.target = Some(target);
            Ok(())
        };

        if let Err(error) = moved {
            if was_mixing {
                let _ = self.set_mixing(true);
            }
            return Err(error);
        }
        let mixer_result = if was_mixing {
            self.set_mixing(true)
        } else {
            Ok(())
        };
        self.save();
        mixer_result
    }

    /// Starts or stops routing. Returns an error to show on the card when the
    /// route refuses to start.
    pub fn set_routing(&mut self, on: bool) -> Result<(), String> {
        if on == self.is_routing() {
            return Ok(());
        }
        self.config.routing = on;
        self.dirty = true;
        if !on {
            // Remove our proxy without restoring the stale device snapshot
            // from when enhancement first started. The route may have followed
            // a later output selection, and switching enhancement off must not
            // undo that choice. Dropping still un-mutes every covered app.
            if let Some(route) = self.route.take() {
                self.target = Some(route.target().clone());
                route.stop_on_target();
            }
            return Ok(());
        }

        let Some(target) = self.target.clone() else {
            return Err("No output device to play to".into());
        };
        match Route::start(&target, *self.equalizer.target(), self.volume as f32) {
            Ok(route) => {
                self.remember_target(&target);
                // The route adopts the hardware's volume rather than imposing
                // ours, so the panel follows it rather than the other way
                // round.
                self.volume = f64::from(route.volume());
                self.route = Some(route);
                self.settle = SETTLE_TICKS;
                self.generation += 1;
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Moves a running route to another piece of hardware.
    ///
    /// The old route is dropped first: two global taps at once is one tap too
    /// many, and the gap costs a few milliseconds of audio rather than the
    /// double-muting the overlap would cause.
    fn retarget(&mut self, target: Device) -> Result<(), String> {
        self.route = None;
        self.target = Some(target.clone());
        match Route::start(&target, *self.equalizer.target(), self.volume as f32) {
            Ok(route) => {
                self.remember_target(&target);
                self.volume = f64::from(route.volume());
                self.route = Some(route);
                self.settle = SETTLE_TICKS;
                self.generation += 1;
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Keeps the route pointed at whatever the user picked in Sound settings.
    ///
    /// Nothing is rerouted at the system level any more, so this is the only
    /// thing that notices a device change: the tap keeps producing, and without
    /// this the audio would keep arriving at the device the user just left.
    ///
    /// Core Audio publishes a listener for it, but that fires on its own thread
    /// and the route is not `Send`; the frame timer is already running, so the
    /// check rides along with it.
    fn follow_system_output(&mut self) {
        if self.settle > 0 {
            self.settle = self.settle.saturating_sub(POLL_TICKS);
            return;
        }
        if self.follow_device_loss() {
            return;
        }
        if self.follow_driver_identity() {
            return;
        }
        self.follow_output_rate();
        let Some(current) = kd_sys::audio::default_output_device() else {
            return;
        };
        let Some(route) = &self.route else {
            return;
        };
        // While routing, the system's output is our own virtual device — that
        // is what makes the menu-bar slider work on hardware that has no volume
        // of its own. Anything else means the user picked a device in Sound
        // settings, and that is the hardware they want to hear.
        let ours = route.driver().map(|driver| driver.id);
        if Some(current) == ours || current == route.target().id {
            return;
        }
        let device = Device::describe(current);
        if device.is_diseq() {
            // Our own device, but not the object we hold: a retract and a
            // republish can leave two incarnations alive at once, and the
            // system is pointed at the one we are not writing to. Everything
            // the panel and the menu bar do lands on the wrong object.
            let target = route.target().clone();
            let _ = self.retarget(target);
            return;
        }
        if device.has_output && !device.is_virtual() {
            let _ = self.retarget(device);
        }
    }

    /// Notices the virtual device being replaced underneath the route.
    ///
    /// The route holds a device snapshot, and a HAL object that has been
    /// retracted and republished is a *different* object with the same UID.
    /// Every write to the old one — the default-output change, the volume, the
    /// mute — is accepted and discarded, so the symptom is a route that reports
    /// itself healthy while the menu bar controls nothing and, once the system
    /// has been moved off it, carries nothing either.
    ///
    /// It happens when a previous run was killed rather than quit, and whenever
    /// anything else toggles the plug-in's visibility.
    fn follow_driver_identity(&mut self) -> bool {
        let Some(route) = &self.route else {
            return false;
        };
        let Some(held) = route.driver().map(|device| device.id) else {
            return false;
        };
        if devices::diseq().map(|device| device.id) == Some(held) {
            return false;
        }
        let target = route.target().clone();
        let _ = self.retarget(target);
        true
    }

    /// Notices the route's hardware going away — unplugged, powered off, put to
    /// sleep — and moves to whatever is left.
    ///
    /// Without this the route keeps rendering into a device that no longer
    /// exists, which is silence with a perfectly healthy status line. Returns
    /// true when it acted, so the caller stops looking at a route that has
    /// just been replaced.
    fn follow_device_loss(&mut self) -> bool {
        let Some(route) = &self.route else {
            return false;
        };
        if kd_sys::audio::is_alive(route.target().id) {
            return false;
        }
        // Dropping first hands the system back to real hardware, so
        // `default_target` has something to find.
        self.route = None;
        self.target = router::default_target();
        let _ = self.set_routing(true);
        true
    }

    /// Rebuilds the route when the output device's sample rate changes under it
    /// — which is what Audio MIDI Setup does.
    ///
    /// A graph built for 44.1 kHz feeding a device now running at 48 kHz plays
    /// nothing recognisable, and nothing in the graph notices on its own.
    fn follow_output_rate(&mut self) {
        let Some(route) = &self.route else { return };
        if !route.rate_changed() {
            return;
        }
        let target = route.target().id;
        let _ = self.retarget(Device::describe(target));
    }

    pub fn set_eq_enabled(&mut self, on: bool) {
        self.equalizer.set_enabled(on);
        self.push();
        self.remember_eq();
    }

    pub fn set_band(&mut self, band: usize, gain: f32) {
        self.equalizer.set_gain(band, gain);
        self.push();
        self.remember_eq();
    }

    pub fn set_preamp(&mut self, preamp: f32) {
        self.equalizer.set_preamp(preamp);
        self.push();
        self.remember_eq();
    }

    pub fn set_auto_preamp(&mut self, on: bool) {
        self.equalizer.set_auto_preamp(on);
        self.push();
        self.remember_eq();
    }

    pub fn set_preset(&mut self, id: &str) -> bool {
        let changed = self.equalizer.set_preset(id, true);
        self.push();
        self.remember_eq();
        changed
    }

    pub fn is_ramping(&self) -> bool {
        self.equalizer.is_ramping()
    }

    /// Copies the equaliser's state into the settings to be written.
    ///
    /// Takes the ramp's destination rather than its current value: saving
    /// mid-transition would restore a half-applied preset.
    fn remember_eq(&mut self) {
        self.config.eq = EqConfig::from(self.equalizer.target(), self.equalizer.selected_preset());
        self.dirty = true;
    }

    // --- volume -------------------------------------------------------------

    pub fn volume(&self) -> f64 {
        self.volume
    }

    /// Whether the volume slider does anything. Routing gives every device a
    /// working slider; without it, only a device with its own volume control
    /// has one.
    pub fn volume_is_settable(&self) -> bool {
        self.is_routing()
            || self
                .target
                .as_ref()
                .map(|device| device.volume_is_settable)
                .unwrap_or(false)
    }

    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
        if self.config.volume != self.volume {
            self.config.volume = self.volume;
            self.dirty = true;
        }
        match &mut self.route {
            // The route decides where the gain lands: the hardware's own volume
            // control when it has one, our mixer when it does not.
            Some(route) => route.set_volume(self.volume as f32),
            None => {
                if let Some(target) = &self.target {
                    kd_sys::audio::set_volume(target.id, self.volume);
                }
            }
        }
    }

    // --- app mixer ----------------------------------------------------------

    pub fn is_mixing(&self) -> bool {
        self.mixer.is_some()
    }

    /// Applications with a fader right now. Empty when the mixer is off — there
    /// is nothing to show a level for until something is tapped.
    pub fn app_faders(&self) -> Vec<AppFader> {
        match &self.mixer {
            Some(mixer) => mixer
                .faders()
                .map(|(process, gain)| AppFader {
                    process: process.clone(),
                    gain,
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// What the mixer would cover if it were switched on now.
    pub fn playing(&self) -> Vec<Process> {
        let mut playing = processes::playing();
        // Our own audio would be tapped out of the mix and fed back into it.
        playing.retain(|process| process.pid != std::process::id() as i32);
        playing.truncate(MAX_FADERS);
        playing
    }

    pub fn set_mixing(&mut self, on: bool) -> Result<(), String> {
        if on == self.is_mixing() {
            return Ok(());
        }
        self.config.mixing = on;
        self.dirty = true;
        if !on {
            // Dropping it destroys the taps, which un-mutes every application.
            self.mixer = None;
            return Ok(());
        }

        let playing = self.playing();
        if playing.is_empty() {
            return Err("Nothing is playing audio right now".into());
        }
        // Mixed audio has to land in front of the EQ when there is one, so the
        // destination is the driver while routing and the hardware otherwise.
        // Mixed audio has to land in front of the EQ, which means the virtual
        // device while a route is running; without one it goes straight to the
        // hardware and the mixer works on its own.
        let destination = match self.route.as_ref().and_then(|route| route.driver()) {
            Some(driver) => driver.clone(),
            None => match &self.target {
                Some(target) => target.clone(),
                None => return Err("No output device to mix into".into()),
            },
        };
        match Mixer::start(&playing, &destination) {
            Ok(mixer) => {
                // Applications come back at the level they were left at, even
                // though the processes carrying their audio are new.
                for (process, _) in mixer.faders() {
                    if let Some(gain) = process
                        .bundle_id
                        .as_deref()
                        .and_then(|bundle| self.config.app_gains.get(bundle))
                    {
                        mixer.set_gain(process.id, *gain);
                    }
                }
                self.mixer = Some(mixer);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn set_app_gain(&mut self, process: u32, gain: f32) {
        let Some(mixer) = &self.mixer else {
            return;
        };
        mixer.set_gain(process, gain);
        // Keyed by bundle ID, because the process this fader belongs to will
        // have a different object — and a different pid — next time.
        if let Some(bundle) = mixer
            .faders()
            .find(|(candidate, _)| candidate.id == process)
            .and_then(|(candidate, _)| candidate.bundle_id.clone())
        {
            self.config.app_gains.insert(bundle, gain);
            self.dirty = true;
        }
    }

    /// Whether the applications playing have changed since the mixer was built.
    /// A new application needs a new tap, which needs a new aggregate device —
    /// and the route's tap has to stop covering it at the same moment.
    pub fn mixer_is_stale(&self) -> bool {
        match &self.mixer {
            Some(mixer) => !mixer.covers(&self.playing()),
            None => false,
        }
    }

    /// Rebuilds the mixer over whatever is playing now. Silently does nothing
    /// when the mixer is off.
    pub fn refresh_mixer(&mut self) -> Result<(), String> {
        if self.mixer.is_none() {
            return Ok(());
        }
        // Remember the faders so a rebuild does not reset levels the user set.
        let previous: Vec<(u32, f32)> = self
            .app_faders()
            .iter()
            .map(|fader| (fader.process.id, fader.gain))
            .collect();
        self.mixer = None;
        self.set_mixing(true)?;
        for (process, gain) in previous {
            self.set_app_gain(process, gain);
        }
        Ok(())
    }

    // --- upkeep -------------------------------------------------------------

    /// One step of everything that needs stepping: the preset ramp and the
    /// route's drift controller. Returns true when the UI should redraw.
    pub fn tick(&mut self) -> bool {
        let ramped = self.equalizer.tick();
        if ramped {
            self.push();
        }
        let mut moved = false;
        if let Some(route) = self.route.as_mut() {
            route.tick();
            // The menu-bar slider and the volume keys write to the virtual
            // device; the panel follows them rather than overwriting them on
            // the next redraw.
            if let Some(volume) = route.volume_changed() {
                self.volume = f64::from(volume);
                moved = true;
            }
        }
        if moved && self.config.volume != self.volume {
            // The menu bar and the volume keys are settings changes too.
            self.config.volume = self.volume;
            self.dirty = true;
        }
        let ramped = ramped || moved;

        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % SAVE_TICKS == 0 {
            self.save();
        }
        if self.ticks % POLL_TICKS != 0 {
            return ramped;
        }
        // The route reports itself when asked to. Taps deliver silence rather
        // than an error when audio capture has not been granted, so a level of
        // zero with everything else healthy is the symptom to look for — and
        // the panel is not always open to show it.
        if let Some(destination) = std::env::var_os("KD_SOUND_LOG") {
            if let Some(health) = self.health() {
                log_line(
                    &destination,
                    &format!(
                        "route #{} · {} · {:.0}/{:.0} Hz · device {:?} (default {:?}) claimed {} ({}) · volume {:.0}% · {} · level {} · drift {} · realignments {}",
                        self.generation,
                        self.output_name(),
                        self.route.as_ref().map(|r| r.rate()).unwrap_or(0.0),
                        self.route.as_ref().map(|r| r.capture_rate()).unwrap_or(0.0),
                        self.route.as_ref().and_then(|r| r.driver()).map(|d| d.id),
                        kd_sys::audio::default_output_device(),
                        self.route.as_ref().map(|r| r.claimed_default()).unwrap_or(false),
                        self.route
                            .as_ref()
                            .map(|r| status_name(r.claim_status()))
                            .unwrap_or_default(),
                        self.volume * 100.0,
                        if health.running { "running" } else { "STOPPED" },
                        if health.level > 0.0 {
                            format!("{:.1} dB", 20.0 * health.level.log10())
                        } else {
                            "SILENT".into()
                        },
                        health
                            .drift()
                            .map(|frames| format!("{frames:+.0}"))
                            .unwrap_or_else(|| "?".into()),
                        health.realignments
                    ),
                );
            }
        }

        let before = Poll {
            routing: self.is_routing(),
            output: self.output_name(),
        };
        self.follow_system_output();
        let after = Poll {
            routing: self.is_routing(),
            output: self.output_name(),
        };
        ramped || before.changed(&after)
    }

    /// Whether the frame timer is still worth running. The device poll needs it
    /// even with nothing playing, so an installed driver keeps it alive.
    pub fn wants_ticks(&self) -> bool {
        self.is_ramping() || self.is_routing() || self.availability.driver
    }

    /// How often [`Self::tick`] wants calling, in milliseconds. The faster of
    /// the two: a ramp frame is shorter than a drift step.
    pub const TICK_MS: u32 = eq::RAMP_FRAME_MS;

    /// Records the device the route is playing to, by UID.
    fn remember_target(&mut self, target: &Device) {
        if target.uid.is_some() && self.config.target_uid != target.uid {
            self.config.target_uid = target.uid.clone();
            self.dirty = true;
        }
    }

    /// Writes the settings if anything has changed.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.config.save();
        self.dirty = false;
    }

    /// Puts everything back: stops routing, un-mutes every tapped application,
    /// and restores the system's default output.
    pub fn shutdown(&mut self) {
        // Settings first: tearing the route down changes the device the volume
        // belongs to, and the file should describe the session that just ended.
        self.save();
        self.mixer = None;
        self.route = None;
    }

    fn push(&mut self) {
        let settings = *self.equalizer.settings();
        if let Some(route) = self.route.as_mut() {
            route.apply(settings);
        }
    }
}

/// A settings key for one physical display.
///
/// Core Graphics ids change between logins. Vendor/model/serial is the stable
/// identity used elsewhere in the app; built-in panels sometimes publish
/// zeroes for all three, so their unit and name provide the fallback.
fn display_persistence_key(display: &Display) -> String {
    let snapshot = &display.snapshot;
    if snapshot.vendor != 0 || snapshot.model != 0 || snapshot.serial != 0 {
        format!(
            "{:08x}:{:08x}:{:08x}",
            snapshot.vendor, snapshot.model, snapshot.serial
        )
    } else {
        format!(
            "builtin:{}:{}:{}",
            snapshot.is_builtin, snapshot.unit, snapshot.name
        )
    }
}

/// The device to route to at launch.
///
/// While routing, the system's default output is our own device, so a launch
/// that finds it there learns nothing from it — the answer is the device the
/// last run was playing to.
fn remembered_target(config: &Config) -> Option<Device> {
    match devices::default_output() {
        Some(device) if !device.is_diseq() && !device.is_virtual() => Some(device),
        // Only reached when the system's output is our own device or another
        // virtual one, which tells us nothing about what the user can hear.
        _ => config
            .target_uid
            .as_deref()
            .and_then(devices::by_uid)
            // A remembered device that has been unplugged since is worse than
            // no memory at all: routing to it is silence.
            .filter(|device| {
                device.has_output && !device.is_virtual() && kd_sys::audio::is_alive(device.id)
            })
            .or_else(router::default_target),
    }
}

/// Appends one diagnostic line. `KD_SOUND_LOG=1` writes to stderr; anything
/// else is taken as a path, which is what a bundle launched by LaunchServices
/// needs — it has no terminal to write to.
fn log_line(destination: &std::ffi::OsStr, line: &str) {
    if destination == "1" {
        eprintln!("DisEQ: {line}");
        return;
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(destination)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Core Audio status codes are four-character codes. Printed raw they are
/// meaningless nine-digit numbers.
fn status_name(status: i32) -> String {
    if status == 0 {
        return "ok".into();
    }
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|byte| byte.is_ascii_graphic()) {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

impl Default for SoundService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SoundService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fader(bundle: Option<&str>) -> AppFader {
        AppFader {
            process: Process {
                id: 1,
                pid: 42,
                bundle_id: bundle.map(str::to_string),
                is_playing: true,
            },
            gain: 1.0,
        }
    }

    #[test]
    fn a_helper_process_is_named_after_the_application_it_belongs_to() {
        assert_eq!(fader(Some("com.brave.Browser.helper")).name(), "Browser");
        assert_eq!(fader(Some("com.spotify.client")).name(), "Client");
        assert_eq!(
            fader(Some("com.tinyspeck.slackmacgap")).name(),
            "Slackmacgap"
        );
    }

    #[test]
    fn a_process_with_no_bundle_is_named_by_its_pid() {
        assert_eq!(fader(None).name(), "pid 42");
    }

    #[test]
    fn the_equaliser_is_blocked_without_the_driver() {
        let availability = Availability { driver: false };
        assert!(availability.eq_blocked().is_some());
        // The mixer never answers in advance: finding out costs a tap, and the
        // tap is the thing that prompts.
        assert!(availability.mixer_blocked().is_none());
    }

    #[test]
    fn nothing_is_blocked_once_the_driver_is_there() {
        let availability = Availability { driver: true };
        assert!(availability.eq_blocked().is_none());
        assert!(availability.mixer_blocked().is_none());
    }

    fn poll(routing: bool, output: &str) -> Poll {
        Poll {
            routing,
            output: output.to_string(),
        }
    }

    #[test]
    fn a_steady_route_does_not_ask_for_a_redraw() {
        // The regression that made the panel flicker and swallow clicks: a
        // route that is simply still running is not news, however long it runs.
        let before = poll(true, "Scarlett 2i2 USB");
        assert!(!before.changed(&poll(true, "Scarlett 2i2 USB")));
    }

    #[test]
    fn a_steady_silence_does_not_ask_for_a_redraw() {
        let before = poll(false, "MacBook Pro Speakers");
        assert!(!before.changed(&poll(false, "MacBook Pro Speakers")));
    }

    #[test]
    fn starting_and_stopping_a_route_asks_for_a_redraw() {
        let idle = poll(false, "MacBook Pro Speakers");
        let routing = poll(true, "MacBook Pro Speakers");
        assert!(idle.changed(&routing));
        assert!(routing.changed(&idle));
    }

    #[test]
    fn changing_the_output_asks_for_a_redraw() {
        let before = poll(true, "MacBook Pro Speakers");
        assert!(before.changed(&poll(true, "Scarlett 2i2 USB")));
    }
}
