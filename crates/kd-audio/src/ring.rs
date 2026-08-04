//! A circular buffer indexed by sample time.
//!
//! Two audio devices are never sample-locked. The processing engine reads the
//! virtual device on one clock and the output unit writes the hardware on
//! another, and the two drift apart continuously. This buffer is what sits
//! between them: the writer stores at its own sample times, the reader asks for
//! whatever range it needs, and anything outside what the buffer actually holds
//! comes back as silence rather than as a glitch.
//!
//! Ported from eqMac's `CircularBuffer.swift`, Copyright © Bitgapp Ltd,
//! licensed under the Apache License 2.0 (https://github.com/bitgapp/eqMac,
//! v1.3.2). Changed: Swift → Rust; storage is planar `f32` rather than an
//! `AudioBufferList`, so the buffer is testable without CoreAudio; and the
//! out-of-range paths in `read` now silence the *destination*, which is what
//! they were clearly meant to do — eqMac silences its own storage there and
//! leaves the caller's buffer holding whatever it held before.

use std::cell::UnsafeCell;
use std::ptr;
use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    /// More frames than the buffer can hold. The caller's buffer is too big or
    /// the ring is too small; either way nothing was written.
    TooMuch,
    /// The bounds could not be read consistently — the writer moved them faster
    /// than the reader could observe them, which in practice means the machine
    /// is overloaded.
    CpuOverload,
}

/// Number of bounds kept in the ring's history. The reader tolerates the writer
/// advancing under it as long as it can find a consistent snapshot within these.
const BOUNDS_HISTORY: usize = 32;

/// Attempts the reader makes to observe a consistent snapshot before giving up.
const BOUNDS_READ_ATTEMPTS: usize = 8;

#[derive(Clone, Copy, Default, Debug)]
struct Bounds {
    start: i64,
    end: i64,
    /// The queue index this entry was written at. A reader that sees an entry
    /// whose index no longer matches the head has been overtaken.
    index: i64,
}

/// The valid sample-time range of the buffer's contents, readable without a
/// lock from the audio thread.
///
/// A single-producer seqlock: the writer publishes into the slot after the head
/// and then advances the head; a reader that finds the slot's stamp still
/// matching the head it read has a consistent snapshot.
struct TimeBounds {
    queue: UnsafeCell<[Bounds; BOUNDS_HISTORY]>,
    head: AtomicI64,
}

impl TimeBounds {
    fn new() -> Self {
        Self {
            queue: UnsafeCell::new([Bounds::default(); BOUNDS_HISTORY]),
            head: AtomicI64::new(0),
        }
    }

    /// # Safety
    /// Only the writing thread may call this: it reads the head slot without
    /// checking whether it is being written.
    unsafe fn current(&self) -> Bounds {
        let head = self.head.load(Ordering::Relaxed);
        let slot = head.rem_euclid(BOUNDS_HISTORY as i64) as usize;
        (*self.queue.get())[slot]
    }

    /// # Safety
    /// Only one thread may call this, and it must be the same one throughout.
    unsafe fn set(&self, start: i64, end: i64) {
        let head = self.head.load(Ordering::Relaxed);
        let next = head + 1;
        let slot = next.rem_euclid(BOUNDS_HISTORY as i64) as usize;
        (*self.queue.get())[slot] = Bounds {
            start,
            end,
            index: next,
        };
        self.head.store(next, Ordering::Release);
    }

    fn get(&self) -> Option<Bounds> {
        for _ in 0..BOUNDS_READ_ATTEMPTS {
            let head = self.head.load(Ordering::Acquire);
            let slot = head.rem_euclid(BOUNDS_HISTORY as i64) as usize;
            // SAFETY: the slot is only written before the head is published, so
            // a matching stamp means this snapshot was not torn.
            let bounds = unsafe { (*self.queue.get())[slot] };
            if bounds.index == head {
                return Some(bounds);
            }
        }
        None
    }

    /// Narrows `start`/`end` to what the buffer actually holds. An empty result
    /// means the request lies entirely outside.
    fn clip(&self, start: &mut i64, end: &mut i64) -> bool {
        let Some(bounds) = self.get() else {
            return false;
        };
        if *start > bounds.end || *end < bounds.start {
            *start = bounds.start;
            *end = bounds.start;
            return true;
        }
        *start = (*start).max(bounds.start);
        *end = (*end).min(bounds.end);
        *end = (*end).max(*start);
        true
    }
}

/// A sample-time-indexed circular buffer, planar and `f32`.
///
/// Shared between one writing thread and one reading thread, both real-time.
/// Neither allocates, and neither takes a lock.
pub struct Ring {
    channels: usize,
    capacity: usize,
    /// Channel-major: channel `c` occupies `[c * capacity, (c + 1) * capacity)`.
    storage: UnsafeCell<Box<[f32]>>,
    bounds: TimeBounds,
}

// SAFETY: the writer only touches frames inside the range it is about to
// publish, and the reader only touches frames the bounds say are valid; the
// bounds themselves are exchanged atomically. Sharing beyond one writer and one
// reader is not supported and is the caller's obligation to avoid.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    /// `capacity` is in frames. Sized generously: it costs 4 bytes per frame
    /// per channel and it is what absorbs drift.
    pub fn new(channels: usize, capacity: usize) -> Self {
        assert!(channels > 0, "a ring with no channels stores nothing");
        assert!(capacity > 0, "a ring with no capacity stores nothing");
        Self {
            channels,
            capacity,
            storage: UnsafeCell::new(vec![0.0; channels * capacity].into_boxed_slice()),
            bounds: TimeBounds::new(),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The sample-time range currently held, if it can be read consistently.
    pub fn valid_range(&self) -> Option<(i64, i64)> {
        self.bounds.get().map(|bounds| (bounds.start, bounds.end))
    }

    /// Stores `source` at sample times `start..end`.
    ///
    /// `source` is one slice per channel. Extra channels are ignored; missing
    /// ones are filled with silence, so a mono source into a stereo ring does
    /// not leave the right channel holding stale audio.
    ///
    /// # Safety
    /// Only one thread may write, and it must be the same one throughout.
    pub unsafe fn write(&self, source: &[&[f32]], start: i64, end: i64) -> Result<(), RingError> {
        let to_write = end - start;
        if to_write <= 0 {
            return Ok(());
        }
        if to_write > self.capacity as i64 {
            return Err(RingError::TooMuch);
        }

        let bounds = self.current_bounds();
        if start < bounds.end {
            // Time went backwards — the writer restarted. Nothing held is
            // meaningful any more.
            self.bounds.set(start, start);
        } else if end - bounds.start > self.capacity as i64 {
            // About to overwrite the oldest frames; move the start past them
            // before they become garbage rather than after.
            let new_start = end - self.capacity as i64;
            let new_end = new_start.max(bounds.end);
            self.bounds.set(new_start, new_end);
        }

        let last = self.current_bounds().end;
        let mut offset0;
        if start > last {
            // A gap: frames nobody wrote. Silence them, or the reader finds
            // whatever was there a ring ago.
            offset0 = self.wrap(last);
            let offset1 = self.wrap(start);
            if offset0 < offset1 {
                self.silence(offset0, offset1 - offset0);
            } else {
                self.silence(offset0, self.capacity - offset0);
                self.silence(0, offset1);
            }
            offset0 = offset1;
        } else {
            offset0 = self.wrap(start);
        }
        let offset1 = self.wrap(end);

        if offset0 < offset1 {
            self.store(source, 0, offset0, offset1 - offset0);
        } else {
            let count = self.capacity - offset0;
            self.store(source, 0, offset0, count);
            self.store(source, count, 0, offset1);
        }

        let start_bound = self.current_bounds().start;
        self.bounds.set(start_bound, end);
        Ok(())
    }

    /// Fills `destination` with sample times `from..to`, silencing whatever the
    /// buffer does not hold.
    ///
    /// # Safety
    /// Only one thread may read, and it must be the same one throughout.
    pub unsafe fn read(
        &self,
        destination: &mut [&mut [f32]],
        from: i64,
        to: i64,
    ) -> Result<(), RingError> {
        let count = to - from;
        if count <= 0 {
            return Ok(());
        }

        let requested_start = from.max(0);
        let requested_end = requested_start + count;
        let mut start = requested_start;
        let mut end = requested_end;

        if !self.bounds.clip(&mut start, &mut end) {
            return Err(RingError::CpuOverload);
        }

        // Nothing of the requested range is held.
        if start == end {
            silence_destination(destination, 0, count as usize);
            return Ok(());
        }

        let leading = (start - requested_start).max(0) as usize;
        if leading > 0 {
            silence_destination(destination, 0, leading.min(count as usize));
        }
        let available = (end - start) as usize;
        let trailing = (requested_end - end).max(0) as usize;
        if trailing > 0 {
            silence_destination(destination, leading + available, trailing);
        }

        let index_start = self.wrap(start);
        let index_end = self.wrap(end);

        if index_start < index_end {
            self.fetch(destination, index_start, leading, index_end - index_start);
        } else {
            let first = self.capacity - index_start;
            self.fetch(destination, index_start, leading, first);
            if index_end > 0 {
                self.fetch(destination, 0, leading + first, index_end);
            }
        }

        Ok(())
    }

    // --- internals ----------------------------------------------------------

    fn current_bounds(&self) -> Bounds {
        // SAFETY: only reached from the writing thread.
        unsafe { self.bounds.current() }
    }

    fn wrap(&self, sample_time: i64) -> usize {
        sample_time.rem_euclid(self.capacity as i64) as usize
    }

    /// The storage as a raw pointer.
    ///
    /// Deliberately not a `&mut [f32]`: the writer and the reader touch this
    /// buffer at the same time, in regions the bounds keep disjoint. A shared
    /// mutable slice would be a lie about that, and a `&mut` derived from `&self`
    /// is the exact shape of an aliasing bug.
    fn base(&self) -> *mut f32 {
        // SAFETY: no reference is formed, only a pointer taken.
        unsafe { (*self.storage.get()).as_mut_ptr() }
    }

    fn silence(&self, offset: usize, count: usize) {
        let base = self.base();
        for channel in 0..self.channels {
            // SAFETY: offset + count never exceeds capacity, checked by the
            // callers' wrap arithmetic. Zero bytes are zero floats.
            unsafe { ptr::write_bytes(base.add(channel * self.capacity + offset), 0, count) };
        }
    }

    fn store(&self, source: &[&[f32]], source_offset: usize, offset: usize, count: usize) {
        let base = self.base();
        for channel in 0..self.channels {
            // SAFETY: as above; the destination region is inside this channel's
            // span, and the source is a live slice at least `available` long.
            let destination = unsafe { base.add(channel * self.capacity + offset) };
            match source.get(channel) {
                Some(samples) if source_offset < samples.len() => {
                    let available = (samples.len() - source_offset).min(count);
                    unsafe {
                        ptr::copy_nonoverlapping(
                            samples.as_ptr().add(source_offset),
                            destination,
                            available,
                        );
                        // Whatever the source ran short of is silence, not history.
                        ptr::write_bytes(destination.add(available), 0, count - available);
                    }
                }
                // Fewer channels supplied than the ring carries.
                _ => unsafe { ptr::write_bytes(destination, 0, count) },
            }
        }
    }

    fn fetch(
        &self,
        destination: &mut [&mut [f32]],
        source_offset: usize,
        offset: usize,
        count: usize,
    ) {
        let base = self.base();
        for (channel, samples) in destination.iter_mut().enumerate() {
            if offset >= samples.len() {
                continue;
            }
            let writable = (samples.len() - offset).min(count);
            if channel < self.channels {
                // SAFETY: the source region is inside this channel's span and
                // the destination has room for `writable` samples.
                unsafe {
                    ptr::copy_nonoverlapping(
                        base.add(channel * self.capacity + source_offset),
                        samples.as_mut_ptr().add(offset),
                        writable,
                    );
                }
            } else {
                // The destination wants more channels than the ring holds.
                samples[offset..offset + writable].fill(0.0);
            }
        }
    }
}

fn silence_destination(destination: &mut [&mut [f32]], offset: usize, count: usize) {
    for samples in destination.iter_mut() {
        if offset >= samples.len() {
            continue;
        }
        let writable = (samples.len() - offset).min(count);
        samples[offset..offset + writable].fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPACITY: usize = 64;

    fn ramp(start: usize, count: usize) -> Vec<f32> {
        (0..count).map(|index| (start + index) as f32).collect()
    }

    /// Writes `count` frames of a per-channel ramp at `at`.
    fn write_ramp(ring: &Ring, at: i64, count: usize) -> Vec<Vec<f32>> {
        let planes: Vec<Vec<f32>> = (0..ring.channels())
            .map(|channel| {
                ramp(at as usize, count)
                    .iter()
                    .map(|sample| sample + channel as f32 * 1000.0)
                    .collect()
            })
            .collect();
        let borrowed: Vec<&[f32]> = planes.iter().map(|plane| plane.as_slice()).collect();
        unsafe { ring.write(&borrowed, at, at + count as i64) }.expect("write");
        planes
    }

    fn read_back(ring: &Ring, from: i64, count: usize) -> Result<Vec<Vec<f32>>, RingError> {
        let mut planes: Vec<Vec<f32>> = vec![vec![f32::NAN; count]; ring.channels()];
        {
            let mut borrowed: Vec<&mut [f32]> = planes
                .iter_mut()
                .map(|plane| plane.as_mut_slice())
                .collect();
            unsafe { ring.read(&mut borrowed, from, from + count as i64) }?;
        }
        Ok(planes)
    }

    #[test]
    fn what_goes_in_comes_out() {
        let ring = Ring::new(2, CAPACITY);
        let written = write_ramp(&ring, 0, 16);
        assert_eq!(read_back(&ring, 0, 16).unwrap(), written);
    }

    #[test]
    fn channels_stay_separate() {
        let ring = Ring::new(2, CAPACITY);
        write_ramp(&ring, 0, 8);
        let read = read_back(&ring, 0, 8).unwrap();
        assert_eq!(read[0][0], 0.0);
        assert_eq!(read[1][0], 1000.0);
    }

    #[test]
    fn reading_before_anything_is_written_is_silent() {
        let ring = Ring::new(2, CAPACITY);
        let read = read_back(&ring, 0, 8).unwrap();
        assert!(read.iter().all(|plane| plane.iter().all(|s| *s == 0.0)));
    }

    #[test]
    fn writes_wrap_the_buffer() {
        let ring = Ring::new(1, CAPACITY);
        // Straddle the wrap: the last write starts before the end and finishes
        // past it.
        write_ramp(&ring, 0, CAPACITY - 8);
        let across = write_ramp(&ring, (CAPACITY - 8) as i64, 16);
        assert_eq!(
            read_back(&ring, (CAPACITY - 8) as i64, 16).unwrap(),
            across,
            "frames written across the wrap did not come back"
        );
    }

    #[test]
    fn frames_older_than_the_buffer_read_as_silence() {
        let ring = Ring::new(1, CAPACITY);
        write_ramp(&ring, 0, CAPACITY);
        // Overwrite the first half; sample time 0 is now gone.
        write_ramp(&ring, CAPACITY as i64, CAPACITY / 2);

        let read = read_back(&ring, 0, 8).unwrap();
        assert!(
            read[0].iter().all(|sample| *sample == 0.0),
            "expected silence for evicted frames, got {:?}",
            read[0]
        );
    }

    #[test]
    fn a_request_straddling_the_start_is_silent_only_where_it_has_to_be() {
        let ring = Ring::new(1, CAPACITY);
        write_ramp(&ring, 100, 16);

        // Ask for eight frames before the write and eight inside it.
        let read = read_back(&ring, 92, 16).unwrap();
        assert!(
            read[0][..8].iter().all(|sample| *sample == 0.0),
            "frames before the written range should be silent, got {:?}",
            &read[0][..8]
        );
        assert_eq!(&read[0][8..], &ramp(100, 8)[..]);
    }

    #[test]
    fn a_request_past_the_end_is_silent_only_where_it_has_to_be() {
        let ring = Ring::new(1, CAPACITY);
        write_ramp(&ring, 0, 8);

        let read = read_back(&ring, 0, 16).unwrap();
        assert_eq!(&read[0][..8], &ramp(0, 8)[..]);
        assert!(
            read[0][8..].iter().all(|sample| *sample == 0.0),
            "frames past the written range should be silent, got {:?}",
            &read[0][8..]
        );
    }

    #[test]
    fn a_gap_between_writes_reads_as_silence() {
        let ring = Ring::new(1, CAPACITY);
        write_ramp(&ring, 0, 8);
        // Skip 8 frames.
        let later = write_ramp(&ring, 16, 8);

        let read = read_back(&ring, 8, 16).unwrap();
        assert!(
            read[0][..8].iter().all(|sample| *sample == 0.0),
            "the skipped frames should be silent, got {:?}",
            &read[0][..8]
        );
        assert_eq!(&read[0][8..], &later[0][..]);
    }

    #[test]
    fn writing_more_than_the_buffer_holds_is_refused() {
        let ring = Ring::new(1, CAPACITY);
        let samples = vec![0.0f32; CAPACITY + 1];
        let planes = [samples.as_slice()];
        assert_eq!(
            unsafe { ring.write(&planes, 0, CAPACITY as i64 + 1) },
            Err(RingError::TooMuch)
        );
    }

    #[test]
    fn time_going_backwards_discards_what_was_held() {
        let ring = Ring::new(1, CAPACITY);
        write_ramp(&ring, 1000, 16);
        // The writer restarted, so its old sample times mean nothing.
        let restarted = write_ramp(&ring, 0, 16);

        assert_eq!(read_back(&ring, 0, 16).unwrap(), restarted);
        let stale = read_back(&ring, 1000, 8).unwrap();
        assert!(
            stale[0].iter().all(|sample| *sample == 0.0),
            "frames from before the restart should be gone, got {:?}",
            stale[0]
        );
    }

    #[test]
    fn a_mono_source_silences_the_channels_it_does_not_supply() {
        let ring = Ring::new(2, CAPACITY);
        write_ramp(&ring, 0, 16);

        let mono = ramp(0, 8);
        let planes = [mono.as_slice()];
        unsafe { ring.write(&planes, 16, 24) }.expect("write");

        let read = read_back(&ring, 16, 8).unwrap();
        assert_eq!(read[0], mono);
        assert!(
            read[1].iter().all(|sample| *sample == 0.0),
            "the unsupplied channel should be silent, got {:?}",
            read[1]
        );
    }

    #[test]
    fn empty_ranges_are_accepted_and_do_nothing() {
        let ring = Ring::new(1, CAPACITY);
        let planes: [&[f32]; 1] = [&[]];
        assert_eq!(unsafe { ring.write(&planes, 5, 5) }, Ok(()));
        assert_eq!(ring.valid_range(), Some((0, 0)));
    }

    #[test]
    fn the_valid_range_follows_the_writer() {
        let ring = Ring::new(1, CAPACITY);
        write_ramp(&ring, 0, 16);
        assert_eq!(ring.valid_range(), Some((0, 16)));

        write_ramp(&ring, 16, CAPACITY);
        let (start, end) = ring.valid_range().unwrap();
        assert_eq!(end, 16 + CAPACITY as i64);
        assert_eq!(
            end - start,
            CAPACITY as i64,
            "the buffer should hold exactly its capacity once full"
        );
    }

    /// The case the whole file exists for: a writer and a reader on separate
    /// threads, unsynchronised, with the reader trailing by a fixed offset the
    /// way the output unit's safety offset makes it trail. Every frame it gets
    /// must be the frame it asked for.
    ///
    /// This does not test what happens when the reader falls a whole buffer
    /// behind — that is an underrun, and `frames_older_than_the_buffer_read_as
    /// _silence` already pins the behaviour. Here the reader stays in range by
    /// construction, so any silence would be a real defect.
    #[test]
    fn a_reader_trailing_a_writer_never_tears() {
        use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
        use std::sync::Arc;

        const BLOCK: usize = 128;
        const BLOCKS: i64 = 4_000;
        /// Frames the reader stays behind the writer, as a safety offset does.
        const TRAIL: i64 = 2 * BLOCK as i64;
        /// Room for the writer to keep going while a read is in flight.
        const CAPACITY: usize = 32_768;

        let ring = Arc::new(Ring::new(2, CAPACITY));
        let written_to = Arc::new(AtomicI64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let writer = {
            let ring = Arc::clone(&ring);
            let written_to = Arc::clone(&written_to);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for block in 0..BLOCKS {
                    let at = block * BLOCK as i64;
                    // Each frame carries its own sample time, so the reader can
                    // check it got the frame it asked for.
                    let plane: Vec<f32> =
                        (0..BLOCK).map(|index| (at + index as i64) as f32).collect();
                    let planes = [plane.as_slice(), plane.as_slice()];
                    unsafe { ring.write(&planes, at, at + BLOCK as i64) }.expect("write");
                    written_to.store(at + BLOCK as i64, Ordering::Release);
                    std::thread::yield_now();
                }
                stop.store(true, Ordering::Release);
            })
        };

        let reader = {
            let ring = Arc::clone(&ring);
            let written_to = Arc::clone(&written_to);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut checked = 0usize;
                while !stop.load(Ordering::Acquire) {
                    let written = written_to.load(Ordering::Acquire);
                    if written < TRAIL + BLOCK as i64 {
                        std::thread::yield_now();
                        continue;
                    }
                    // Chase the writer rather than advancing independently: a
                    // reader that falls behind is an underrun, not a tear.
                    let position = written - TRAIL;

                    let mut left = vec![f32::NAN; BLOCK];
                    let mut right = vec![f32::NAN; BLOCK];
                    {
                        let mut planes: Vec<&mut [f32]> = vec![&mut left, &mut right];
                        if unsafe { ring.read(&mut planes, position, position + BLOCK as i64) }
                            .is_err()
                        {
                            // The bounds could not be sampled consistently.
                            // Legitimate under load; try again.
                            continue;
                        }
                    }
                    for (index, sample) in left.iter().enumerate() {
                        let expected = (position + index as i64) as f32;
                        assert_eq!(
                            *sample,
                            expected,
                            "frame at {} came back as {sample}",
                            position + index as i64
                        );
                    }
                    assert_eq!(left, right, "channels diverged at {position}");
                    checked += BLOCK;
                    std::thread::yield_now();
                }
                checked
            })
        };

        writer.join().expect("writer");
        let checked = reader.join().expect("reader");
        assert!(checked > 0, "the reader never managed a read");
    }
}
