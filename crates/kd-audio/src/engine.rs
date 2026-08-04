//! Hosting `AVAudioUnitEQ` from Rust.
//!
//! The DSP is Apple's. This file only configures the unit — ten parametric
//! bands at the ISO centres — and pushes [`crate::eq::Settings`] onto it.
//!
//! Derived from eqMac, Copyright © Bitgapp Ltd, licensed under the Apache
//! License 2.0 (https://github.com/bitgapp/eqMac, v1.3.2). Changed: ported
//! Swift → Rust; band configuration and the `globalGain`-as-preamp convention
//! are eqMac's.

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_avf_audio::{AVAudioNode, AVAudioUnitEQ, AVAudioUnitEQFilterType};

use crate::eq::{Settings, BANDWIDTH_OCTAVES, BAND_COUNT, FREQUENCIES};

/// A configured ten-band `AVAudioUnitEQ`, ready to be attached to an engine.
///
/// Not `Send`: an audio unit belongs to the thread that made it. Gains reach
/// the render thread through the unit's own parameter mechanism, which is
/// already lock-free — that is the whole reason for using Apple's unit.
pub struct EqUnit {
    unit: Retained<AVAudioUnitEQ>,
}

impl Default for EqUnit {
    fn default() -> Self {
        Self::new()
    }
}

impl EqUnit {
    pub fn new() -> Self {
        // SAFETY: a plain init on a class with no threading requirement.
        let unit =
            unsafe { AVAudioUnitEQ::initWithNumberOfBands(AVAudioUnitEQ::alloc(), BAND_COUNT) };

        // SAFETY: bands() returns exactly BAND_COUNT parameter objects, owned by
        // the unit and valid for as long as it is.
        unsafe {
            let bands = unit.bands();
            for (index, frequency) in FREQUENCIES.iter().enumerate() {
                let band = bands.objectAtIndex(index);
                band.setFilterType(AVAudioUnitEQFilterType::Parametric);
                band.setFrequency(*frequency);
                band.setBandwidth(BANDWIDTH_OCTAVES);
                band.setGain(0.0);
                // Bands default to bypassed. A bypassed band ignores its gain.
                band.setBypass(false);
            }
            unit.setGlobalGain(0.0);
            unit.setBypass(false);
        }

        Self { unit }
    }

    /// Pushes settings at the unit. Safe to call every ramp frame — each setter
    /// is a parameter write, not a graph change.
    pub fn apply(&self, settings: &Settings) {
        // SAFETY: same indices the constructor configured.
        unsafe {
            let bands = self.unit.bands();
            for (index, gain) in settings.gains.iter().enumerate() {
                bands.objectAtIndex(index).setGain(*gain);
            }
            self.unit.setGlobalGain(settings.global_gain());
            self.unit.setBypass(!settings.enabled);
        }
    }

    /// Reads the gains back off the unit. For tests and diagnostics — the model
    /// in [`crate::eq`] is the authority.
    pub fn gains(&self) -> [f32; BAND_COUNT] {
        let mut gains = [0.0; BAND_COUNT];
        // SAFETY: as above.
        unsafe {
            let bands = self.unit.bands();
            for (index, gain) in gains.iter_mut().enumerate() {
                *gain = bands.objectAtIndex(index).gain();
            }
        }
        gains
    }

    pub fn global_gain(&self) -> f32 {
        // SAFETY: property read.
        unsafe { self.unit.globalGain() }
    }

    pub fn is_bypassed(&self) -> bool {
        // SAFETY: property read.
        unsafe { self.unit.bypass() }
    }

    /// The unit as a graph node, for `attach` and `connect`.
    pub fn node(&self) -> &AVAudioNode {
        &self.unit
    }

    pub fn unit(&self) -> &AVAudioUnitEQ {
        &self.unit
    }
}
