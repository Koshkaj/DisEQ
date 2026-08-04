//! Ten-band equaliser model: frequencies, gains, presets, and the ramp that
//! stops preset changes from clicking.
//!
//! Pure state — no CoreAudio, no Objective-C. [`crate::engine::EqUnit`] is what
//! pushes this onto an `AVAudioUnitEQ`.
//!
//! Portions derived from eqMac, Copyright © Bitgapp Ltd, licensed under the
//! Apache License 2.0 (https://github.com/bitgapp/eqMac, v1.3.2). Changed:
//! ported Swift → Rust; the preset table and the 500 ms/30 fps transition are
//! eqMac's, the auto-preamp and the ramp's frame-pull API are ours.

use std::ops::RangeInclusive;

pub const BAND_COUNT: usize = 10;

/// The ISO centres eqMac uses, and the ones the reference UI labels.
pub const FREQUENCIES: [f32; BAND_COUNT] = [
    32.0, 64.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// Octaves. Narrow enough that adjacent bands stay distinguishable, wide enough
/// that ten of them cover the spectrum without gaps.
pub const BANDWIDTH_OCTAVES: f32 = 0.5;

/// `AVAudioUnitEQ` accepts -96..=24 dB. We stop at ±24 so a fader's travel means
/// the same thing in both directions.
pub const GAIN_RANGE: RangeInclusive<f32> = -24.0..=24.0;

/// The preamp reaches much further down than up.
///
/// Down, because auto-preamp subtracts from it: a bank of +24 dB boosts has to
/// be able to pull the level back below unity. Up is capped at +5 dB
/// deliberately — the preamp lifts every band at once, so it runs out of
/// headroom far faster than a single band does, and a boost applied to a signal
/// that is already near full scale is what drives an amplifier into clipping.
/// `AVAudioUnitEQ` itself would accept +24.
pub const PREAMP_RANGE: RangeInclusive<f32> = -96.0..=5.0;

pub type Gains = [f32; BAND_COUNT];

pub const FLAT: Gains = [0.0; BAND_COUNT];

// --- settings ---------------------------------------------------------------

/// Everything the EQ needs to be reconstructed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub gains: Gains,
    pub preamp: f32,
    /// Pull the preamp down by the loudest band, so boosting cannot clip.
    /// eqMac calls this the peak limiter.
    pub auto_preamp: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            gains: FLAT,
            preamp: 0.0,
            auto_preamp: true,
        }
    }
}

impl Settings {
    /// The gain that actually reaches `globalGain`, auto-preamp included.
    pub fn global_gain(&self) -> f32 {
        let mut gain = self.preamp;
        if self.auto_preamp {
            let peak = self.gains.iter().copied().fold(0.0f32, f32::max);
            gain -= peak;
        }
        gain.clamp(*PREAMP_RANGE.start(), *PREAMP_RANGE.end())
    }

    fn clamped(mut self) -> Self {
        for gain in &mut self.gains {
            *gain = gain.clamp(*GAIN_RANGE.start(), *GAIN_RANGE.end());
        }
        self.preamp = self
            .preamp
            .clamp(*PREAMP_RANGE.start(), *PREAMP_RANGE.end());
        self
    }
}

// --- presets ----------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub gains: Gains,
}

/// eqMac's advanced-equaliser preset table, verbatim. Ordered flat-first, then
/// alphabetically, which is how the picker shows them.
pub const PRESETS: [Preset; 23] = [
    Preset {
        id: "flat",
        name: "Flat",
        gains: FLAT,
    },
    Preset {
        id: "acoustic",
        name: "Acoustic",
        gains: [-8.3, 9.8, -15.68, 2.1, 18.22, 3.5, 7.0, 8.2, 7.1, 4.3],
    },
    Preset {
        id: "bass-booster",
        name: "Bass Booster",
        gains: [11.0, 8.5, 7.0, 5.0, 2.5, 0.0, 0.0, 0.0, 0.0, 0.0],
    },
    Preset {
        id: "bass-reducer",
        name: "Bass Reducer",
        gains: [-11.0, -8.5, -7.0, -5.0, -2.5, 0.0, 0.0, 0.0, 0.0, 0.0],
    },
    Preset {
        id: "classic",
        name: "Classic",
        gains: [9.5, 7.5, 6.0, 5.0, -3.0, -3.0, 0.0, 4.5, 6.5, 7.5],
    },
    Preset {
        id: "dance",
        name: "Dance",
        gains: [7.14, 13.1, 9.98, 0.0, 3.84, 7.3, 10.3, 9.08, 7.18, 0.0],
    },
    Preset {
        id: "deep",
        name: "Deep",
        gains: [9.9, 7.1, 3.5, 2.0, 5.7, 5.0, 2.9, -4.3, -7.1, -9.2],
    },
    Preset {
        id: "electronic",
        name: "Electronic",
        gains: [8.5, 7.6, 2.4, 0.0, -4.3, 4.5, 1.7, 2.5, 7.9, 9.6],
    },
    Preset {
        id: "hip-hop",
        name: "Hip-Hop",
        gains: [10.0, 8.5, 3.0, 6.0, -2.0, -2.0, 3.0, -1.0, 4.0, 6.0],
    },
    Preset {
        id: "jazz",
        name: "Jazz",
        gains: [8.0, 6.0, 3.0, 4.5, -3.0, -3.0, 0.0, 3.0, 6.0, 7.5],
    },
    Preset {
        id: "latin",
        name: "Latin",
        gains: [9.0, 6.0, 0.0, 0.0, -3.0, -3.0, -3.0, 0.0, 6.0, 9.0],
    },
    Preset {
        id: "loudness",
        name: "Loudness",
        gains: [12.0, 8.0, 0.0, 0.0, -4.0, 0.0, -2.0, -10.0, 10.0, 2.0],
    },
    Preset {
        id: "lounge",
        name: "Lounge",
        gains: [-6.0, -3.0, -1.0, 3.0, 8.0, 5.0, 0.0, -3.0, 4.0, 2.0],
    },
    Preset {
        id: "piano",
        name: "Piano",
        gains: [6.0, 4.0, 0.0, 5.0, 6.0, 3.0, 7.0, 9.0, 6.0, 7.0],
    },
    Preset {
        id: "pop",
        name: "Pop",
        gains: [-3.0, -2.0, 0.0, 4.0, 8.0, 8.0, 4.0, 0.0, -2.0, -3.0],
    },
    Preset {
        id: "rnb",
        name: "RnB",
        gains: [5.24, 13.84, 11.3, 2.66, -4.38, -3.0, 4.64, 5.3, 6.0, 7.5],
    },
    Preset {
        id: "rock",
        name: "Rock",
        gains: [10.0, 8.0, 6.0, 3.0, -1.0, -2.0, 1.0, 5.0, 7.0, 9.0],
    },
    Preset {
        id: "small-speakers",
        name: "Small Speakers",
        gains: [11.0, 8.5, 7.0, 5.0, 2.5, 0.0, -2.5, -5.0, -7.0, -8.5],
    },
    Preset {
        id: "spoken-word",
        name: "Spoken Word",
        gains: [-6.92, -0.94, 0.0, 1.38, 6.92, 9.22, 9.68, 8.56, 5.08, 0.0],
    },
    Preset {
        id: "treble-booster",
        name: "Treble Booster",
        gains: [0.0, 0.0, 0.0, 0.0, 0.0, 2.5, 5.0, 7.0, 8.5, 11.0],
    },
    Preset {
        id: "treble-reducer",
        name: "Treble Reducer",
        gains: [0.0, 0.0, 0.0, 0.0, 0.0, -2.5, -5.0, -7.0, -8.5, -11.0],
    },
    Preset {
        id: "vocal-booster",
        name: "Vocal Booster",
        gains: [-3.0, -6.0, -6.0, 3.0, 7.5, 7.5, 6.0, 3.0, 0.0, -3.0],
    },
    Preset {
        id: "manual",
        name: "Manual",
        gains: FLAT,
    },
];

/// The id the picker falls back to once a fader has been dragged.
pub const MANUAL: &str = "manual";

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// The preset whose gains these are, if any. Lets the picker re-select itself
/// after a restore without storing the id separately.
pub fn matching_preset(gains: &Gains) -> Option<&'static Preset> {
    PRESETS
        .iter()
        .filter(|preset| preset.id != MANUAL)
        .find(|preset| preset.gains == *gains)
}

// --- ramping ----------------------------------------------------------------

/// eqMac's transition: 500 ms at 30 fps, i.e. fifteen linear steps.
pub const RAMP_DURATION_MS: u32 = 500;
pub const RAMP_FPS: u32 = 30;
pub const RAMP_FRAMES: u32 = RAMP_FPS * RAMP_DURATION_MS / 1_000;
pub const RAMP_FRAME_MS: u32 = 1_000 / RAMP_FPS;

/// A linear interpolation between two settings, pulled one frame at a time.
///
/// eqMac pushes its frames onto the main queue with fifteen delayed closures.
/// Pulling instead means the caller owns the timer, so a ramp can be replaced
/// mid-flight without leaving orphaned closures writing stale gains.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    from: Gains,
    to: Gains,
    from_preamp: f32,
    to_preamp: f32,
    frame: u32,
}

impl Ramp {
    fn new(from: &Settings, to: &Settings) -> Self {
        Self {
            from: from.gains,
            to: to.gains,
            from_preamp: from.preamp,
            to_preamp: to.preamp,
            frame: 0,
        }
    }

    /// Advances one frame and reports where the ramp now sits. The last frame
    /// lands exactly on the target rather than near it.
    fn advance(&mut self) -> (Gains, f32) {
        self.frame = (self.frame + 1).min(RAMP_FRAMES);
        if self.finished() {
            return (self.to, self.to_preamp);
        }
        let t = self.frame as f32 / RAMP_FRAMES as f32;
        let mut gains = FLAT;
        for (index, gain) in gains.iter_mut().enumerate() {
            *gain = self.from[index] + (self.to[index] - self.from[index]) * t;
        }
        (
            gains,
            self.from_preamp + (self.to_preamp - self.from_preamp) * t,
        )
    }

    fn finished(&self) -> bool {
        self.frame >= RAMP_FRAMES
    }
}

// --- the model ---------------------------------------------------------------

/// The equaliser's state, including any ramp in flight.
///
/// Nothing here talks to CoreAudio. Callers read [`Equalizer::settings`] after
/// every mutation and after every [`Equalizer::tick`] that returns `true`, and
/// push the result at whatever holds the audio unit.
#[derive(Clone, Debug)]
pub struct Equalizer {
    settings: Settings,
    ramp: Option<Ramp>,
    /// What the ramp is heading for. `settings` lags it while ramping.
    target: Settings,
    selected: &'static str,
}

impl Default for Equalizer {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

impl Equalizer {
    pub fn new(settings: Settings) -> Self {
        let settings = settings.clamped();
        Self {
            settings,
            ramp: None,
            target: settings,
            selected: matching_preset(&settings.gains)
                .map(|preset| preset.id)
                .unwrap_or(MANUAL),
        }
    }

    /// The gains to send to the audio unit right now — mid-ramp values while a
    /// transition is running.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Where a running ramp is headed. Equals [`Self::settings`] when idle, and
    /// is what should be persisted: saving a mid-ramp value would restore a
    /// half-applied preset.
    pub fn target(&self) -> &Settings {
        &self.target
    }

    pub fn selected_preset(&self) -> &'static str {
        self.selected
    }

    pub fn is_ramping(&self) -> bool {
        self.ramp.is_some()
    }

    /// Selects a preset. `ramp` glides over [`RAMP_DURATION_MS`]; without it the
    /// change lands at once, which is what a restore at launch wants.
    ///
    /// Returns false for an unknown id.
    pub fn set_preset(&mut self, id: &str, ramp: bool) -> bool {
        let Some(preset) = preset(id) else {
            return false;
        };
        self.selected = preset.id;
        // "Manual" is a label for whatever the faders happen to say, not a set
        // of gains to move to.
        if preset.id != MANUAL {
            let mut target = self.target;
            target.gains = preset.gains;
            self.move_to(target, ramp);
        }
        true
    }

    /// Drags one fader. Cancels any ramp — the user is now the authority — and
    /// drops the picker to Manual unless the result happens to be a preset.
    pub fn set_gain(&mut self, band: usize, gain: f32) {
        if band >= BAND_COUNT {
            return;
        }
        self.ramp = None;
        self.settings.gains[band] = gain.clamp(*GAIN_RANGE.start(), *GAIN_RANGE.end());
        self.target = self.settings;
        self.selected = matching_preset(&self.settings.gains)
            .map(|preset| preset.id)
            .unwrap_or(MANUAL);
    }

    pub fn set_preamp(&mut self, preamp: f32) {
        self.ramp = None;
        self.settings.preamp = preamp.clamp(*PREAMP_RANGE.start(), *PREAMP_RANGE.end());
        self.target = self.settings;
    }

    pub fn set_auto_preamp(&mut self, on: bool) {
        self.settings.auto_preamp = on;
        self.target.auto_preamp = on;
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.settings.enabled = on;
        self.target.enabled = on;
    }

    /// Sets everything at once, e.g. restoring from disk.
    pub fn reset(&mut self, settings: Settings, ramp: bool) {
        self.move_to(settings.clamped(), ramp);
        self.selected = matching_preset(&self.target.gains)
            .map(|preset| preset.id)
            .unwrap_or(MANUAL);
    }

    fn move_to(&mut self, target: Settings, ramp: bool) {
        self.target = target;
        if ramp && target.gains != self.settings.gains {
            self.ramp = Some(Ramp::new(&self.settings, &target));
        } else {
            self.ramp = None;
            self.settings = target;
        }
    }

    /// Advances a running ramp by one frame. Call every [`RAMP_FRAME_MS`] while
    /// [`Self::is_ramping`]. Returns true when the settings changed.
    pub fn tick(&mut self) -> bool {
        let Some(ramp) = self.ramp.as_mut() else {
            return false;
        };
        let (gains, preamp) = ramp.advance();
        let finished = ramp.finished();
        self.settings.gains = gains;
        self.settings.preamp = preamp;
        if finished {
            self.ramp = None;
            self.settings = self.target;
        }
        true
    }

    /// Jumps a running ramp to its end. For quitting, or for a second preset
    /// click that should not queue behind the first.
    pub fn finish_ramp(&mut self) {
        if self.ramp.take().is_some() {
            self.settings = self.target;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_unique_ids() {
        for (index, preset) in PRESETS.iter().enumerate() {
            assert!(
                PRESETS[..index].iter().all(|other| other.id != preset.id),
                "duplicate preset id {}",
                preset.id
            );
        }
    }

    #[test]
    fn preset_gains_are_within_range() {
        for preset in &PRESETS {
            for gain in preset.gains {
                assert!(
                    GAIN_RANGE.contains(&gain),
                    "{} has out-of-range gain {gain}",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn auto_preamp_cancels_the_loudest_boost() {
        let mut settings = Settings::default();
        settings.gains[0] = 11.0;
        settings.gains[1] = 8.5;
        assert_eq!(settings.global_gain(), -11.0);

        settings.auto_preamp = false;
        assert_eq!(settings.global_gain(), 0.0);
    }

    #[test]
    fn auto_preamp_leaves_cuts_alone() {
        let settings = Settings {
            gains: preset("bass-reducer").unwrap().gains,
            ..Settings::default()
        };
        assert_eq!(settings.global_gain(), 0.0);
    }

    #[test]
    fn ramp_lands_exactly_on_target() {
        let mut eq = Equalizer::default();
        assert!(eq.set_preset("bass-booster", true));
        assert!(eq.is_ramping());
        assert_ne!(eq.settings().gains, preset("bass-booster").unwrap().gains);

        for _ in 0..RAMP_FRAMES {
            assert!(eq.tick());
        }
        assert!(!eq.is_ramping());
        assert_eq!(eq.settings().gains, preset("bass-booster").unwrap().gains);
        assert!(!eq.tick());
    }

    #[test]
    fn ramp_is_monotonic_towards_the_target() {
        let mut eq = Equalizer::default();
        eq.set_preset("bass-booster", true);
        let mut previous = eq.settings().gains[0];
        while eq.tick() {
            let current = eq.settings().gains[0];
            assert!(current >= previous, "{current} went below {previous}");
            previous = current;
        }
    }

    #[test]
    fn unramped_preset_applies_at_once() {
        let mut eq = Equalizer::default();
        eq.set_preset("rock", false);
        assert!(!eq.is_ramping());
        assert_eq!(eq.settings().gains, preset("rock").unwrap().gains);
    }

    #[test]
    fn dragging_a_fader_selects_manual_and_stops_the_ramp() {
        let mut eq = Equalizer::default();
        eq.set_preset("rock", true);
        eq.tick();
        eq.set_gain(0, 3.0);
        assert!(!eq.is_ramping());
        assert_eq!(eq.selected_preset(), MANUAL);
        assert_eq!(eq.settings().gains[0], 3.0);
    }

    #[test]
    fn a_fader_landing_on_a_preset_reselects_it() {
        let mut eq = Equalizer::default();
        eq.set_preset("treble-booster", false);
        eq.set_gain(0, 5.0);
        assert_eq!(eq.selected_preset(), MANUAL);
        eq.set_gain(0, 0.0);
        assert_eq!(eq.selected_preset(), "treble-booster");
    }

    #[test]
    fn the_preamp_cannot_boost_past_its_ceiling() {
        // The preamp lifts every band at once, so its ceiling is far lower than
        // a band's. Asking for more is clamped rather than refused — including
        // from a settings file written by hand.
        let mut eq = Equalizer::default();
        eq.set_preamp(24.0);
        assert_eq!(eq.settings().preamp, *PREAMP_RANGE.end());
        assert_eq!(*PREAMP_RANGE.end(), 5.0);

        let restored = Settings {
            preamp: 18.0,
            auto_preamp: false,
            ..Settings::default()
        };
        assert_eq!(Equalizer::new(restored).settings().preamp, 5.0);
        assert_eq!(restored.clamped().global_gain(), 5.0);
    }

    #[test]
    fn gains_clamp_to_range() {
        let mut eq = Equalizer::default();
        eq.set_gain(0, 500.0);
        assert_eq!(eq.settings().gains[0], *GAIN_RANGE.end());
        eq.set_gain(0, -500.0);
        assert_eq!(eq.settings().gains[0], *GAIN_RANGE.start());
    }

    #[test]
    fn target_leads_settings_while_ramping() {
        let mut eq = Equalizer::default();
        eq.set_preset("rock", true);
        eq.tick();
        assert_eq!(eq.target().gains, preset("rock").unwrap().gains);
        assert_ne!(eq.settings().gains, eq.target().gains);
        eq.finish_ramp();
        assert_eq!(eq.settings().gains, eq.target().gains);
    }

    #[test]
    fn manual_keeps_the_current_gains() {
        let mut eq = Equalizer::default();
        eq.set_preset("rock", false);
        eq.set_preset(MANUAL, false);
        assert_eq!(eq.settings().gains, preset("rock").unwrap().gains);
        assert_eq!(eq.selected_preset(), MANUAL);
    }

    #[test]
    fn unknown_preset_is_rejected() {
        let mut eq = Equalizer::default();
        assert!(!eq.set_preset("nope", false));
        assert_eq!(eq.selected_preset(), "flat");
    }
}
