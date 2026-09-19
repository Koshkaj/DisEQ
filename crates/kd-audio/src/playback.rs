//! The output engine: reads the ring and plays it on the hardware device.
//!
//! The two devices run on unrelated clocks, so this side does two things the
//! capture side does not. It reads at a deliberate lag — [`Bridge`]'s safety
//! offset — so a late buffer still finds audio. And it plays through a
//! varispeed unit whose rate a slow control loop nudges by fractions of a
//! percent, so the lag stays where it was put instead of drifting until the
//! ring runs dry or overflows.
//!
//! Derived from eqMac, Copyright © Bitgapp Ltd, licensed under the Apache
//! License 2.0 (https://github.com/bitgapp/eqMac, v1.3.2), specifically
//! `Source/Audio/Outputs/Output.swift`. Changed: ported Swift → Rust; audio is
//! supplied by an `AVAudioSourceNode` rather than by installing a render
//! callback on the varispeed unit's input, which is the same position in the
//! graph through a supported API; the PID controller is driven by the caller
//! rather than by its own dispatch timer, so it runs on the thread that owns
//! the engine.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::AnyThread;
use objc2_audio_toolbox::{
    kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Output, AudioUnitSetProperty,
};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioFormat, AVAudioFrameCount, AVAudioSourceNode, AVAudioUnitVarispeed,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_foundation::NSError;

use crate::bridge::{Bridge, Fill};
use crate::devices::Device;
use crate::engine::EqUnit;
use crate::eq::Settings;
use crate::format;
use crate::shared::SharedRing;
use crate::unit;

/// How far the varispeed rate may stray from nominal, either way. Two parts in
/// a thousand is inaudible and far more than any real clock disagrees by.
const RATE_BOUND: f32 = 0.002;

/// Ticks per second the controller expects. Only used to scale the integral and
/// derivative terms, so a caller that ticks a little irregularly still behaves.
pub const TICKS_PER_SECOND: f64 = 10.0;

const KP: f64 = 0.0001;
const KI: f64 = 0.0;
const KD: f64 = 0.0001;

/// Ticks averaged before the controller acts. One second of history: shorter
/// and it chases jitter, longer and it lags the drift it is correcting.
const HISTORY: usize = TICKS_PER_SECOND as usize;

#[derive(Debug)]
pub enum PlaybackError {
    NoOutputUnit,
    DeviceRefused(i32),
    Format,
    Start(String),
}

impl std::fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOutputUnit => write!(f, "the engine has no output audio unit"),
            Self::DeviceRefused(status) => {
                write!(f, "the output device was refused (status {status})")
            }
            Self::Format => write!(f, "the output device published an unusable format"),
            Self::Start(why) => write!(f, "the output engine would not start: {why}"),
        }
    }
}

impl std::error::Error for PlaybackError {}

pub struct Playback {
    engine: Retained<AVAudioEngine>,
    /// The equaliser. It sits here rather than on the capture side because the
    /// capture side is a tap's IO proc now — a real-time C callback with no
    /// audio graph to host a unit in.
    eq: EqUnit,
    varispeed: Retained<AVAudioUnitVarispeed>,
    /// Held only to keep the node — and the block it owns — alive for as long
    /// as the engine can call it.
    _source: Retained<AVAudioSourceNode>,
    bridge: Arc<Bridge>,
    drift: Drift,
    sample_rate: f64,
}

impl Playback {
    /// Builds the engine around `output` — the hardware the user hears — and
    /// starts it.
    ///
    /// `input_rate` is the virtual device's actual rate. The ratio between the
    /// two is the varispeed rate the controller starts from: if the driver runs
    /// fast, playback has to run fast too or the ring fills up.
    ///
    /// `mixer_gain` is the gain for *this* stage, not the master volume. Where
    /// the two differ is the whole point: hardware with a volume control of its
    /// own is driven directly and this stays at unity. Seeding it with the
    /// master volume regardless meant the gain was applied here *and* on the
    /// hardware, and a route started at 25% came up at 6%. `Route` owns that
    /// decision — see `Route::apply_volume`.
    pub fn start(
        output: &Device,
        input_rate: f64,
        bridge: Arc<Bridge>,
        shared: Arc<SharedRing>,
        mixer_gain: f32,
        settings: &Settings,
    ) -> Result<Self, PlaybackError> {
        let sample_rate = output
            .sample_rate
            .filter(|rate| *rate > 0.0)
            .ok_or(PlaybackError::Format)?;

        // SAFETY: AVFoundation and AudioToolbox calls with the arguments they
        // document; failures come back as status codes, null, or NSError.
        unsafe {
            let engine = AVAudioEngine::new();

            let output_node = engine.outputNode();
            let unit = unit::audio_unit(&*output_node);
            if unit.is_null() {
                return Err(PlaybackError::NoOutputUnit);
            }
            let mut device_id = output.id;
            let status = AudioUnitSetProperty(
                unit,
                kAudioOutputUnitProperty_CurrentDevice,
                kAudioUnitScope_Output,
                0,
                &mut device_id as *mut u32 as *const c_void,
                std::mem::size_of::<u32>() as u32,
            );
            if status != 0 {
                return Err(PlaybackError::DeviceRefused(status));
            }

            let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                sample_rate,
                format::CHANNELS as u32,
            )
            .ok_or(PlaybackError::Format)?;

            // The node copies the block, so the `RcBlock` only has to outlive
            // this call.
            let block = render_block(Arc::clone(&bridge), Arc::clone(&shared));
            let source = AVAudioSourceNode::initWithFormat_renderBlock(
                AVAudioSourceNode::alloc(),
                &format,
                &*block as *const _ as *mut _,
            );

            let varispeed = AVAudioUnitVarispeed::new();
            let nominal_rate = (input_rate / sample_rate) as f32;
            let nominal_rate = if nominal_rate.is_finite() && nominal_rate > 0.0 {
                nominal_rate
            } else {
                1.0
            };
            varispeed.setRate(nominal_rate);

            let mixer = engine.mainMixerNode();
            mixer.setOutputVolume(mixer_gain.clamp(0.0, 1.0));

            let eq = EqUnit::new();
            eq.apply(settings);

            engine.attachNode(&source);
            engine.attachNode(eq.node());
            engine.attachNode(&varispeed);
            // The EQ runs before the varispeed so its filters see the rate they
            // were configured for; the varispeed only ever nudges by fractions
            // of a percent, but the order costs nothing and says what is meant.
            engine.connect_to_format(&source, eq.node(), Some(&format));
            engine.connect_to_format(eq.node(), &varispeed, Some(&format));
            engine.connect_to_format(&varispeed, &mixer, Some(&format));

            engine.prepare();
            engine
                .startAndReturnError()
                .map_err(|error: Retained<NSError>| {
                    PlaybackError::Start(error.localizedDescription().to_string())
                })?;

            Ok(Self {
                engine,
                eq,
                varispeed,
                _source: source,
                bridge,
                drift: Drift::new(nominal_rate),
                sample_rate,
            })
        }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Pushes new equaliser settings at the running graph.
    pub fn apply(&self, settings: &Settings) {
        self.eq.apply(settings);
    }

    pub fn eq(&self) -> &EqUnit {
        &self.eq
    }

    pub fn is_running(&self) -> bool {
        // SAFETY: property read.
        unsafe { self.engine.isRunning() }
    }

    /// Master volume, 0..1. The mixer ramps to it rather than stepping, so this
    /// is safe to call from a slider drag.
    pub fn set_volume(&self, volume: f32) {
        // SAFETY: property write on a node the engine owns.
        unsafe {
            self.engine
                .mainMixerNode()
                .setOutputVolume(volume.clamp(0.0, 1.0));
        }
    }

    pub fn volume(&self) -> f32 {
        // SAFETY: property read.
        unsafe { self.engine.mainMixerNode().outputVolume() }
    }

    /// How long after the engine renders a frame the hardware plays it, in
    /// seconds: the output device's own latency, safety offset and buffering,
    /// as Core Audio reports them for the device the engine is driving.
    pub fn output_latency(&self) -> f64 {
        // SAFETY: property read on the engine's own output node.
        unsafe { self.engine.outputNode().presentationLatency() }
    }

    /// One step of the drift controller. Call about [`TICKS_PER_SECOND`] times
    /// a second, from the thread that owns this object.
    ///
    /// Returns the rate now in force, for diagnostics.
    pub fn tick(&mut self) -> f32 {
        let Some(observed) = self.bridge.observed_offset() else {
            // Nothing is aligned yet, so there is no drift to measure and the
            // history from before the last realignment is meaningless.
            self.drift.reset();
            // SAFETY: property write.
            unsafe { self.varispeed.setRate(self.drift.nominal) };
            return self.drift.nominal;
        };

        let wanted = self.bridge.safety_offset() as f64;
        let Some(rate) = self.drift.step(wanted, observed) else {
            return self.rate();
        };
        // SAFETY: an audio-unit parameter write, valid from any thread.
        unsafe { self.varispeed.setRate(rate) };
        rate
    }

    pub fn rate(&self) -> f32 {
        // SAFETY: property read.
        unsafe { self.varispeed.rate() }
    }

    /// The nominal rate, before any correction.
    pub fn nominal_rate(&self) -> f32 {
        self.drift.nominal
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        // SAFETY: stopping an engine that is already stopped is allowed.
        unsafe { self.engine.stop() };
    }
}

/// What `AVAudioSourceNode` calls to get audio: a silence flag to set, the
/// time the frames will be played at, how many are wanted, and where to put
/// them.
type RenderBlock = dyn Fn(
    NonNull<Bool>,
    NonNull<AudioTimeStamp>,
    AVAudioFrameCount,
    NonNull<AudioBufferList>,
) -> i32;

/// Supplies the output engine with audio, on the real-time thread.
///
/// No allocation, no locks, no logging, no Objective-C: the closure captures an
/// `Arc<Bridge>` and only ever dereferences it.
/// The render block: the only place audio crosses from the driver's ring into
/// the engine.
///
/// It reads the driver's write position itself rather than being told by a
/// capture callback — there is no capture callback any more, and the ring is
/// the same shared memory the driver writes into. Everything else is as it
/// was: the bridge relates the two clocks and the varispeed corrects the drift
/// between them.
fn render_block(bridge: Arc<Bridge>, shared: Arc<SharedRing>) -> RcBlock<RenderBlock> {
    RcBlock::new(
        move |is_silence: NonNull<Bool>,
              timestamp: NonNull<AudioTimeStamp>,
              frames: AVAudioFrameCount,
              data: NonNull<AudioBufferList>|
              -> i32 {
            let frames = frames as usize;
            // SAFETY: AVFoundation hands the block a valid buffer list of its
            // output format — float, non-interleaved — and a timestamp for the
            // frames it is about to play.
            unsafe {
                // The driver's clock, straight from the shared header. Reading
                // it here rather than in a callback of our own is what lets
                // this app carry no capture permission at all.
                bridge.set_capturing(shared.is_running());
                bridge.wrote_through(shared.written());

                let output_time = timestamp.as_ref().mSampleTime as i64;
                match bridge.position_for(output_time) {
                    Fill::Silence => {
                        format::silence(data.as_ptr());
                        // Telling the engine the buffer is silent lets the
                        // nodes downstream skip work rather than multiply
                        // zeroes.
                        is_silence.write(Bool::YES);
                    }
                    Fill::Read { from } => {
                        let (mut channels, count) = format::planar_mut(data.as_ptr(), frames);
                        if count == 0 {
                            return 0;
                        }
                        if shared.read(&mut channels[..count], from, frames) {
                            // The meter is the difference between a route that
                            // is working and one that is aligned and carrying
                            // silence.
                            let mut peak = 0.0f32;
                            for channel in channels.iter().take(count) {
                                for sample in channel.iter().take(frames) {
                                    let magnitude = sample.abs();
                                    if magnitude > peak {
                                        peak = magnitude;
                                    }
                                }
                            }
                            bridge.observe_peak(peak);
                        } else {
                            // Outside what the driver has written and still
                            // holds. Silence, and re-align rather than play a
                            // hole.
                            format::silence(data.as_ptr());
                            is_silence.write(Bool::YES);
                            bridge.realign();
                        }
                    }
                }
            }
            0
        },
    )
}

/// The clock-drift controller: a PID loop on how far the reader trails the
/// writer, whose output is the varispeed rate.
///
/// Deliberately slow and weak. Its job is to cancel a disagreement of a few
/// parts per million between two crystals, not to react to anything audible.
struct Drift {
    nominal: f32,
    lowest: f32,
    highest: f32,
    history: [f64; HISTORY],
    filled: usize,
    next: usize,
    integral: f64,
    previous_error: f64,
    rate: f32,
}

impl Drift {
    fn new(nominal: f32) -> Self {
        Self {
            nominal,
            lowest: nominal * (1.0 - RATE_BOUND),
            highest: nominal * (1.0 + RATE_BOUND),
            history: [0.0; HISTORY],
            filled: 0,
            next: 0,
            integral: 0.0,
            previous_error: 0.0,
            rate: nominal,
        }
    }

    fn reset(&mut self) {
        self.filled = 0;
        self.next = 0;
        self.integral = 0.0;
        self.previous_error = 0.0;
        self.rate = self.nominal;
    }

    /// Feeds one observation in. Returns the new rate, or `None` while the
    /// average is still too noisy to act on.
    fn step(&mut self, wanted: f64, observed: f64) -> Option<f32> {
        self.history[self.next] = observed;
        self.next = (self.next + 1) % HISTORY;
        self.filled = (self.filled + 1).min(HISTORY);

        let average = self.history[..self.filled].iter().sum::<f64>() / self.filled as f64;
        if average.abs() < f64::EPSILON || wanted <= 0.0 {
            // No lag to hold: either nothing has been read yet or the safety
            // offset was never set. Dividing here would produce an infinity.
            return None;
        }

        // Positive error means the reader is trailing by less than it should —
        // it is catching up and will run out of audio, so playback must slow
        // down.
        let error = 1.0 - (wanted / average);
        let dt = 1.0 / TICKS_PER_SECOND;

        self.integral += error * dt;
        let derivative = (error - self.previous_error) / dt;
        self.previous_error = error;

        let change = (KP * error + KI * self.integral + KD * derivative) as f32;
        self.rate = (self.rate + change).clamp(self.lowest, self.highest);
        Some(self.rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settle(drift: &mut Drift, wanted: f64, observed: f64, ticks: usize) -> f32 {
        let mut rate = drift.rate;
        for _ in 0..ticks {
            if let Some(new_rate) = drift.step(wanted, observed) {
                rate = new_rate;
            }
        }
        rate
    }

    #[test]
    fn a_reader_trailing_by_exactly_the_safety_offset_is_left_alone() {
        let mut drift = Drift::new(1.0);
        assert_eq!(settle(&mut drift, 512.0, 512.0, 20), 1.0);
    }

    #[test]
    fn a_reader_falling_behind_is_told_to_speed_up() {
        // Observed lag above the target means audio is piling up: play faster.
        let mut drift = Drift::new(1.0);
        assert!(settle(&mut drift, 512.0, 600.0, 20) > 1.0);
    }

    #[test]
    fn a_reader_catching_up_is_told_to_slow_down() {
        let mut drift = Drift::new(1.0);
        assert!(settle(&mut drift, 512.0, 400.0, 20) < 1.0);
    }

    #[test]
    fn the_rate_never_leaves_its_bounds_however_bad_the_drift() {
        let mut drift = Drift::new(1.0);
        let fast = settle(&mut drift, 512.0, 1_000_000.0, 500);
        assert!(fast <= 1.0 * (1.0 + RATE_BOUND) + f32::EPSILON, "{fast}");

        let mut drift = Drift::new(1.0);
        let slow = settle(&mut drift, 512.0, 1.0, 500);
        assert!(slow >= 1.0 * (1.0 - RATE_BOUND) - f32::EPSILON, "{slow}");
    }

    #[test]
    fn the_bounds_follow_a_nominal_rate_that_is_not_one() {
        // A 44.1 kHz driver into a 48 kHz device starts well away from 1.0.
        let nominal = 44_100.0f32 / 48_000.0;
        let mut drift = Drift::new(nominal);
        let rate = settle(&mut drift, 512.0, 1_000_000.0, 500);
        assert!(rate <= nominal * (1.0 + RATE_BOUND) + f32::EPSILON);
        assert!(rate > nominal);
    }

    #[test]
    fn a_reset_returns_the_rate_to_nominal_and_forgets_the_history() {
        let mut drift = Drift::new(1.0);
        settle(&mut drift, 512.0, 600.0, 20);
        drift.reset();
        assert_eq!(drift.rate, 1.0);
        // With the history cleared, one observation at the target leaves the
        // rate alone rather than resuming the old correction.
        assert_eq!(settle(&mut drift, 512.0, 512.0, 1), 1.0);
    }

    #[test]
    fn an_unset_safety_offset_produces_no_correction() {
        let mut drift = Drift::new(1.0);
        assert_eq!(drift.step(0.0, 512.0), None);
    }

    #[test]
    fn the_controller_converges_rather_than_oscillating() {
        // Feeding the loop a lag that responds to the rate it asks for: a
        // crude plant model, enough to show the loop is stable.
        let mut drift = Drift::new(1.0);
        let wanted = 512.0;
        let mut observed = 900.0;
        for _ in 0..2_000 {
            if let Some(rate) = drift.step(wanted, observed) {
                // Faster playback drains the ring, so the lag falls.
                observed -= (rate as f64 - 1.0) * 20_000.0;
            }
        }
        assert!(
            (observed - wanted).abs() < 100.0,
            "settled at {observed}, wanted {wanted}"
        );
    }
}
