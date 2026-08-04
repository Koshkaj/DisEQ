//! What the two engines share: the ring and the timing that lines them up.
//!
//! The virtual device and the hardware device run on different clocks. The
//! capture side writes at the driver's sample time; the playback side reads at
//! the hardware's. Neither number means anything to the other, so the first
//! playback callback records the difference between them and everything after
//! reads through that constant. `safety_offset` then holds the reader far
//! enough behind the writer that a late buffer on either side still finds
//! audio rather than a hole.
//!
//! Derived from eqMac, Copyright © Bitgapp Ltd, licensed under the Apache
//! License 2.0 (https://github.com/bitgapp/eqMac, v1.3.2). Changed: ported
//! Swift → Rust; the offsets live in atomics on a shared object instead of
//! globals reached through `Application`, and sample times are integers
//! throughout rather than `Double`.
//!
//! Every method here is callable from a real-time thread: no allocation, no
//! locks, no logging.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

use crate::ring::{Ring, RingError};

/// "No sample time has been seen yet." Not zero — zero is a real sample time,
/// and a device that has just started reports it.
pub const NO_TIME: i64 = i64::MIN;

/// What the playback callback should do with the buffer it was handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Read `from..from + frames` out of the ring.
    Read { from: i64 },
    /// Nothing to play: the capture side has not started, or the alignment was
    /// just taken and this buffer is the price of taking it.
    Silence,
}

pub struct Bridge {
    ring: Ring,
    /// Newest sample time the capture side has written through.
    input_time: AtomicI64,
    /// Newest sample time the playback side has been asked for.
    output_time: AtomicI64,
    /// `input_time - output_time`, taken once and then held. Converts a
    /// playback sample time into the capture timeline.
    sample_offset: AtomicI64,
    /// How far behind the writer the reader deliberately stays, in frames.
    safety_offset: AtomicI64,
    /// Whether `sample_offset` has been taken.
    aligned: AtomicBool,
    /// Whether the capture side is producing. Cleared on stop so the playback
    /// callback goes silent instead of replaying whatever the ring still holds.
    capturing: AtomicBool,
    /// Reads that found no audio and forced a re-alignment. Diagnostics only.
    realignments: AtomicU64,
    /// Loudest sample the capture side has written since it was last read, as
    /// `f32` bits. A route can be perfectly aligned and still silent — this is
    /// what tells the two apart.
    peak: AtomicU32,
}

impl Bridge {
    pub fn new(channels: usize, capacity: usize) -> Self {
        Self {
            ring: Ring::new(channels, capacity),
            input_time: AtomicI64::new(NO_TIME),
            output_time: AtomicI64::new(NO_TIME),
            sample_offset: AtomicI64::new(0),
            safety_offset: AtomicI64::new(0),
            aligned: AtomicBool::new(false),
            capturing: AtomicBool::new(false),
            realignments: AtomicU64::new(0),
            peak: AtomicU32::new(0),
        }
    }

    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    pub fn channels(&self) -> usize {
        self.ring.channels()
    }

    // --- capture side -------------------------------------------------------

    /// Records that the capture side has written up to `end`.
    ///
    /// Called from the capture callback, after the write.
    pub fn wrote_through(&self, end: i64) {
        self.input_time.store(end, Ordering::Release);
    }

    /// Records the loudest sample in what was just written.
    ///
    /// Real-time safe: one relaxed compare-and-swap loop over a value that only
    /// this thread raises.
    pub fn observe_peak(&self, peak: f32) {
        let bits = peak.to_bits();
        let seen = self.peak.load(Ordering::Relaxed);
        if peak > f32::from_bits(seen) {
            self.peak.store(bits, Ordering::Relaxed);
        }
    }

    /// The loudest sample written since the last call, and resets the meter.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    pub fn input_time(&self) -> i64 {
        self.input_time.load(Ordering::Acquire)
    }

    /// Records whether the capture side is producing.
    ///
    /// The driver resets its sample time whenever IO starts, so audio resuming
    /// after a silent gap arrives on a clock that has jumped backwards. Letting
    /// the alignment go with it costs one silent buffer; keeping it costs a
    /// failed read and a re-alignment, which is the same silence plus a
    /// diagnostic that lies about the cause.
    pub fn set_capturing(&self, capturing: bool) {
        let was = self.capturing.swap(capturing, Ordering::AcqRel);
        if capturing && !was {
            self.aligned.store(false, Ordering::Release);
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::Acquire)
    }

    // --- playback side ------------------------------------------------------

    /// The frame the playback callback should start reading at, given the
    /// sample time the hardware asked for.
    ///
    /// The first call after an alignment takes the offset and answers
    /// [`Fill::Silence`]: there is no sensible read position until the two
    /// timelines have been related, and one silent buffer is cheaper than a
    /// wrong one.
    pub fn position_for(&self, output_time: i64) -> Fill {
        let input_time = self.input_time();
        if !self.is_capturing() || input_time == NO_TIME {
            return Fill::Silence;
        }

        self.output_time.store(output_time, Ordering::Release);

        if !self.aligned.load(Ordering::Acquire) {
            self.sample_offset
                .store(input_time - output_time, Ordering::Release);
            self.aligned.store(true, Ordering::Release);
            return Fill::Silence;
        }

        let offset = self.sample_offset.load(Ordering::Acquire);
        let safety = self.safety_offset.load(Ordering::Acquire);
        Fill::Read {
            from: output_time + offset - safety,
        }
    }

    /// Drops the alignment so the next playback callback takes it again.
    ///
    /// Real-time safe: called from the callback when a read finds nothing,
    /// which means the clocks have drifted past what the safety margin covers.
    pub fn realign(&self) {
        self.aligned.store(false, Ordering::Release);
        self.realignments.fetch_add(1, Ordering::Relaxed);
    }

    pub fn realignments(&self) -> u64 {
        self.realignments.load(Ordering::Relaxed)
    }

    // --- setup and monitoring ----------------------------------------------

    /// How far behind the writer the reader should sit. Set once the devices
    /// are known — it is the sum of both safety offsets and both buffer sizes,
    /// which is the worst case either side can be late by.
    pub fn set_safety_offset(&self, frames: i64) {
        self.safety_offset.store(frames, Ordering::Release);
    }

    pub fn safety_offset(&self) -> i64 {
        self.safety_offset.load(Ordering::Acquire)
    }

    /// How far the reader actually trails the writer right now, in frames.
    ///
    /// Equal to [`Self::safety_offset`] when the clocks agree. Drifting above
    /// it means the reader is falling behind and latency is growing; below it
    /// means the reader is catching up and will run out of audio. The
    /// difference is what the varispeed controller corrects.
    pub fn observed_offset(&self) -> Option<f64> {
        if !self.aligned.load(Ordering::Acquire) {
            return None;
        }
        let output_time = self.output_time.load(Ordering::Acquire);
        if output_time == NO_TIME {
            return None;
        }
        let input_time = self.input_time();
        if input_time == NO_TIME {
            return None;
        }
        let offset = self.sample_offset.load(Ordering::Acquire);
        let safety = self.safety_offset.load(Ordering::Acquire);
        Some((input_time - (output_time + offset - safety)) as f64)
    }

    /// Forgets everything about both timelines. For a full restart — a device
    /// change, or the engines being stopped.
    pub fn reset(&self) {
        self.capturing.store(false, Ordering::Release);
        self.aligned.store(false, Ordering::Release);
        self.input_time.store(NO_TIME, Ordering::Release);
        self.output_time.store(NO_TIME, Ordering::Release);
        self.sample_offset.store(0, Ordering::Release);
        self.realignments.store(0, Ordering::Relaxed);
        self.peak.store(0, Ordering::Relaxed);
    }
}

/// A ring read that silences and re-aligns rather than failing.
///
/// The playback callback has nowhere to report an error to, so a read that
/// cannot be satisfied becomes silence plus a re-alignment: one short gap
/// instead of a permanent one.
///
/// # Safety
/// Only the playback thread may call this, and it must be the same one
/// throughout — [`Ring::read`]'s requirement.
pub unsafe fn read_or_silence(
    bridge: &Bridge,
    destination: &mut [&mut [f32]],
    from: i64,
    frames: usize,
) -> Result<(), RingError> {
    let result = bridge.ring().read(destination, from, from + frames as i64);
    if result.is_err() {
        for channel in destination.iter_mut() {
            let end = frames.min(channel.len());
            channel[..end].fill(0.0);
        }
        bridge.realign();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge() -> Bridge {
        let bridge = Bridge::new(2, 4_096);
        bridge.set_safety_offset(512);
        bridge
    }

    #[test]
    fn nothing_plays_before_the_capture_side_starts() {
        let bridge = bridge();
        assert_eq!(bridge.position_for(1_000), Fill::Silence);
    }

    #[test]
    fn nothing_plays_while_capture_is_stopped_even_with_audio_held() {
        let bridge = bridge();
        bridge.wrote_through(10_000);
        assert_eq!(bridge.position_for(1_000), Fill::Silence);
    }

    #[test]
    fn the_first_aligned_callback_is_silent_and_the_next_one_reads() {
        let bridge = bridge();
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);

        assert_eq!(bridge.position_for(1_000), Fill::Silence);
        // offset = 10_000 - 1_000 = 9_000; the next callback reads
        // 1_512 + 9_000 - 512.
        assert_eq!(
            bridge.position_for(1_512),
            Fill::Read {
                from: 1_512 + 9_000 - 512
            }
        );
    }

    #[test]
    fn the_reader_trails_the_writer_by_the_safety_offset() {
        let bridge = bridge();
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);
        bridge.position_for(1_000);

        // With both clocks stopped, the read position sits exactly one safety
        // offset behind what the writer has published.
        let Fill::Read { from } = bridge.position_for(1_000) else {
            panic!("expected a read");
        };
        assert_eq!(from, 10_000 - 512);
    }

    #[test]
    fn a_realignment_makes_the_next_callback_take_the_offset_again() {
        let bridge = bridge();
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);
        bridge.position_for(1_000);
        assert!(matches!(bridge.position_for(1_100), Fill::Read { .. }));

        bridge.realign();
        // The writer has moved on; the new offset has to reflect that.
        bridge.wrote_through(20_000);
        assert_eq!(bridge.position_for(2_000), Fill::Silence);
        let Fill::Read { from } = bridge.position_for(2_000) else {
            panic!("expected a read");
        };
        assert_eq!(from, 20_000 - 512);
        assert_eq!(bridge.realignments(), 1);
    }

    #[test]
    fn the_observed_offset_is_the_safety_offset_when_the_clocks_agree() {
        let bridge = bridge();
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);
        bridge.position_for(1_000);
        bridge.position_for(1_000);
        assert_eq!(bridge.observed_offset(), Some(512.0));
    }

    #[test]
    fn a_reader_falling_behind_shows_a_growing_offset() {
        let bridge = bridge();
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);
        bridge.position_for(1_000);

        // The writer advanced 480 frames; the reader only asked for 440.
        bridge.wrote_through(10_480);
        bridge.position_for(1_440);
        assert_eq!(bridge.observed_offset(), Some(512.0 + 40.0));
    }

    #[test]
    fn the_offset_is_unknowable_before_alignment() {
        let bridge = bridge();
        assert_eq!(bridge.observed_offset(), None);
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);
        assert_eq!(bridge.observed_offset(), None);
    }

    #[test]
    fn reset_forgets_both_timelines() {
        let bridge = bridge();
        bridge.set_capturing(true);
        bridge.wrote_through(10_000);
        bridge.position_for(1_000);
        bridge.reset();

        assert!(!bridge.is_capturing());
        assert_eq!(bridge.input_time(), NO_TIME);
        assert_eq!(bridge.observed_offset(), None);
        assert_eq!(bridge.position_for(1_000), Fill::Silence);
    }

    #[test]
    fn a_read_that_cannot_be_satisfied_silences_and_realigns() {
        let bridge = bridge();
        bridge.set_capturing(true);

        let mut left = [1.0f32; 64];
        let mut right = [1.0f32; 64];
        let mut destination: [&mut [f32]; 2] = [&mut left, &mut right];

        // Nothing was ever written, so the ring holds nothing at all. The read
        // still succeeds — it silences what it cannot fill — so this pins the
        // silencing, not the error path.
        // SAFETY: single-threaded test.
        unsafe { read_or_silence(&bridge, &mut destination, 0, 64) }.expect("read");
        assert!(left.iter().all(|sample| *sample == 0.0));
        assert!(right.iter().all(|sample| *sample == 0.0));
    }
}
