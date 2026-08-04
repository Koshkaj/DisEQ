//! Rendering the EQ offline, so its effect can be measured instead of trusted.
//!
//! `AVAudioEngine`'s manual rendering mode runs the same graph the live path
//! will, minus the hardware and the clock. Feeding it a known signal and
//! measuring what comes back is the acceptance test for the EQ: a bass boost
//! either shows up in the numbers or it does not.

use std::ptr;

use objc2::rc::Retained;
use objc2::{msg_send, AllocAnyThread};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioEngineManualRenderingMode, AVAudioEngineManualRenderingStatus,
    AVAudioFormat, AVAudioFrameCount, AVAudioPCMBuffer, AVAudioPlayerNode,
};
use objc2_foundation::NSError;

use crate::engine::EqUnit;
use crate::eq::Settings;

/// Frames per render call. Larger than any realistic hardware buffer, so a
/// second of audio costs a handful of calls.
const RENDER_BLOCK: AVAudioFrameCount = 4_096;

#[derive(Debug)]
pub enum OfflineError {
    Format,
    ManualRendering(String),
    Start(String),
    Buffer,
    Render(AVAudioEngineManualRenderingStatus),
}

impl std::fmt::Display for OfflineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format => write!(f, "could not build the render format"),
            Self::ManualRendering(why) => write!(f, "manual rendering refused: {why}"),
            Self::Start(why) => write!(f, "engine would not start: {why}"),
            Self::Buffer => write!(f, "could not allocate a PCM buffer"),
            Self::Render(status) => write!(f, "render returned status {}", status.0),
        }
    }
}

impl std::error::Error for OfflineError {}

/// A graph of player → EQ → main mixer, rendering on demand.
pub struct OfflineRenderer {
    engine: Retained<AVAudioEngine>,
    player: Retained<AVAudioPlayerNode>,
    format: Retained<AVAudioFormat>,
    eq: EqUnit,
    sample_rate: f64,
}

impl OfflineRenderer {
    /// Builds the graph. Mono, because one channel is enough to measure a
    /// filter and keeps the analysis honest.
    pub fn new(sample_rate: f64) -> Result<Self, OfflineError> {
        // SAFETY: every call below is a documented AVFAudio API used with the
        // arguments it expects; failures come back as null or NSError.
        unsafe {
            let engine = AVAudioEngine::new();
            let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                sample_rate,
                1,
            )
            .ok_or(OfflineError::Format)?;

            // Must happen before mainMixerNode is touched: reaching for the
            // mixer first would configure the graph for the hardware.
            engine
                .enableManualRenderingMode_format_maximumFrameCount_error(
                    AVAudioEngineManualRenderingMode::Offline,
                    &format,
                    RENDER_BLOCK,
                )
                .map_err(|error| {
                    OfflineError::ManualRendering(error.localizedDescription().to_string())
                })?;

            let player = AVAudioPlayerNode::new();
            let eq = EqUnit::new();

            engine.attachNode(&player);
            engine.attachNode(eq.node());

            let mixer = engine.mainMixerNode();
            engine.connect_to_format(&player, eq.node(), Some(&format));
            engine.connect_to_format(eq.node(), &mixer, Some(&format));

            Ok(Self {
                engine,
                player,
                format,
                eq,
                sample_rate,
            })
        }
    }

    pub fn eq(&self) -> &EqUnit {
        &self.eq
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Renders `input` through the EQ at `settings` and returns the same number
    /// of frames back.
    pub fn render(&self, input: &[f32], settings: &Settings) -> Result<Vec<f32>, OfflineError> {
        self.eq.apply(settings);

        // SAFETY: buffers are allocated at the render format and never outlive
        // this call; the render loop asks for no more than RENDER_BLOCK frames.
        unsafe {
            let source = self.pcm_buffer(input.len() as AVAudioFrameCount)?;
            copy_into(&source, input);
            source.setFrameLength(input.len() as AVAudioFrameCount);

            self.engine.reset();
            self.engine
                .startAndReturnError()
                .map_err(|error| OfflineError::Start(error.localizedDescription().to_string()))?;

            self.player
                .scheduleBuffer_completionHandler(&source, ptr::null_mut());
            self.player.play();

            let sink = self.pcm_buffer(RENDER_BLOCK)?;
            let mut out = Vec::with_capacity(input.len());
            while out.len() < input.len() {
                let want = (input.len() - out.len()).min(RENDER_BLOCK as usize);
                let mut error: *mut NSError = ptr::null_mut();
                let status: AVAudioEngineManualRenderingStatus = msg_send![
                    &*self.engine,
                    renderOffline: want as AVAudioFrameCount,
                    toBuffer: &*sink,
                    error: &mut error,
                ];
                if status != AVAudioEngineManualRenderingStatus::Success {
                    self.engine.stop();
                    return Err(OfflineError::Render(status));
                }
                out.extend_from_slice(read_from(&sink));
            }

            self.player.stop();
            self.engine.stop();
            Ok(out)
        }
    }

    unsafe fn pcm_buffer(
        &self,
        frames: AVAudioFrameCount,
    ) -> Result<Retained<AVAudioPCMBuffer>, OfflineError> {
        AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            &self.format,
            frames,
        )
        .ok_or(OfflineError::Buffer)
    }
}

/// # Safety
/// `buffer` must be a float, mono buffer with capacity for `samples`.
unsafe fn copy_into(buffer: &AVAudioPCMBuffer, samples: &[f32]) {
    let channels = buffer.floatChannelData();
    if channels.is_null() {
        return;
    }
    let channel = (*channels).as_ptr();
    ptr::copy_nonoverlapping(samples.as_ptr(), channel, samples.len());
}

/// # Safety
/// `buffer` must be a float, mono buffer; the slice borrows its storage.
unsafe fn read_from(buffer: &AVAudioPCMBuffer) -> &[f32] {
    let channels = buffer.floatChannelData();
    if channels.is_null() {
        return &[];
    }
    let channel = (*channels).as_ptr();
    std::slice::from_raw_parts(channel, buffer.frameLength() as usize)
}

// --- measurement -------------------------------------------------------------

/// Amplitude of `frequency` in `samples`, by Goertzel.
///
/// One bin, no FFT, no dependency. Accurate enough to tell a 10 dB boost from a
/// flat response, which is all the acceptance test asks.
pub fn amplitude_at(samples: &[f32], sample_rate: f64, frequency: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let omega = 2.0 * std::f64::consts::PI * frequency / sample_rate;
    let coefficient = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for sample in samples {
        let s0 = *sample as f64 + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let real = s1 - s2 * omega.cos();
    let imaginary = s2 * omega.sin();
    2.0 * (real * real + imaginary * imaginary).sqrt() / samples.len() as f64
}

/// A sine at `frequency`, `seconds` long.
pub fn tone(sample_rate: f64, frequency: f64, seconds: f64, amplitude: f32) -> Vec<f32> {
    let frames = (sample_rate * seconds) as usize;
    (0..frames)
        .map(|frame| {
            let phase = 2.0 * std::f64::consts::PI * frequency * frame as f64 / sample_rate;
            amplitude * phase.sin() as f32
        })
        .collect()
}

/// Sum of several sines, each at `amplitude`.
pub fn tones(sample_rate: f64, frequencies: &[f64], seconds: f64, amplitude: f32) -> Vec<f32> {
    let frames = (sample_rate * seconds) as usize;
    (0..frames)
        .map(|frame| {
            frequencies
                .iter()
                .map(|frequency| {
                    let phase = 2.0 * std::f64::consts::PI * frequency * frame as f64 / sample_rate;
                    amplitude * phase.sin() as f32
                })
                .sum()
        })
        .collect()
}

/// Gain in dB from `before` to `after` at `frequency`.
pub fn gain_db(before: &[f32], after: &[f32], sample_rate: f64, frequency: f64) -> f64 {
    let reference = amplitude_at(before, sample_rate, frequency);
    let measured = amplitude_at(after, sample_rate, frequency);
    if reference <= f64::EPSILON {
        return f64::NEG_INFINITY;
    }
    20.0 * (measured / reference).log10()
}
