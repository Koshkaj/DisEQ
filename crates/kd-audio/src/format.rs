//! The one audio format the engine works in, and how to see an
//! `AudioBufferList` as Rust slices.
//!
//! Everything downstream of the driver is 32-bit float, non-interleaved,
//! stereo — the same shape [`crate::ring::Ring`] stores and the same shape
//! `AVAudioFormat`'s standard format uses, so no conversion happens anywhere in
//! the signal path.

use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM, AudioBufferList, AudioStreamBasicDescription,
};

/// Channels the engine carries. Stereo: it is what the reference UI shows, what
/// the driver publishes, and what every consumer path here assumes.
pub const CHANNELS: usize = 2;

const BYTES_PER_SAMPLE: u32 = std::mem::size_of::<f32>() as u32;

/// Float, non-interleaved, packed — one `f32` per sample, one buffer per
/// channel.
pub fn client_format(sample_rate: f64, channels: u32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: sample_rate,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat
            | kAudioFormatFlagIsPacked
            | kAudioFormatFlagIsNonInterleaved,
        // Non-interleaved: the per-frame and per-packet sizes describe one
        // channel's buffer, not the whole frame.
        mBytesPerPacket: BYTES_PER_SAMPLE,
        mFramesPerPacket: 1,
        mBytesPerFrame: BYTES_PER_SAMPLE,
        mChannelsPerFrame: channels,
        mBitsPerChannel: BYTES_PER_SAMPLE * 8,
        mReserved: 0,
    }
}

/// The buffers in `list`, however many it declares.
///
/// # Safety
/// `list` must point to a valid `AudioBufferList` — that is, `mNumberBuffers`
/// must describe the array that actually follows it.
pub unsafe fn buffers(
    list: *const AudioBufferList,
) -> &'static [objc2_core_audio_types::AudioBuffer] {
    let count = (*list).mNumberBuffers as usize;
    std::slice::from_raw_parts((*list).mBuffers.as_ptr(), count)
}

/// Borrows up to [`CHANNELS`] of `list` as read-only planar slices.
///
/// Returns the array and how many of its entries are real; the rest are empty.
/// Slices are truncated to whatever the buffer actually holds, so a buffer
/// smaller than `frames` yields a shorter slice rather than a read past its
/// end.
///
/// # Safety
/// `list` must be valid per [`buffers`], and each buffer's `mData` must point
/// to `mDataByteSize` readable bytes of `f32`.
pub unsafe fn planar<'a>(
    list: *const AudioBufferList,
    frames: usize,
) -> ([&'a [f32]; CHANNELS], usize) {
    let mut out: [&[f32]; CHANNELS] = [&[]; CHANNELS];
    let mut count = 0;
    for (index, buffer) in buffers(list).iter().take(CHANNELS).enumerate() {
        if buffer.mData.is_null() {
            continue;
        }
        let available = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
        out[index] = std::slice::from_raw_parts(buffer.mData as *const f32, available.min(frames));
        count = index + 1;
    }
    (out, count)
}

/// Borrows up to [`CHANNELS`] of `list` as writable planar slices.
///
/// # Safety
/// As [`planar`], and no other slice may alias the same buffers for as long as
/// the result lives.
pub unsafe fn planar_mut<'a>(
    list: *mut AudioBufferList,
    frames: usize,
) -> ([&'a mut [f32]; CHANNELS], usize) {
    let mut out: [&mut [f32]; CHANNELS] = std::array::from_fn(|_| &mut [][..]);
    let mut count = 0;
    let list_buffers = (*list).mBuffers.as_mut_ptr();
    let declared = (*list).mNumberBuffers as usize;
    for (index, slot) in out.iter_mut().enumerate().take(declared) {
        let buffer = &mut *list_buffers.add(index);
        if buffer.mData.is_null() {
            continue;
        }
        let available = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
        *slot = std::slice::from_raw_parts_mut(buffer.mData as *mut f32, available.min(frames));
        count = index + 1;
    }
    (out, count)
}

/// Zeroes every buffer in `list`.
///
/// What a render callback does when it has nothing to play: leaving the buffer
/// alone plays whatever the last caller left in it.
///
/// # Safety
/// `list` must be valid per [`buffers`].
pub unsafe fn silence(list: *mut AudioBufferList) {
    let declared = (*list).mNumberBuffers as usize;
    let list_buffers = (*list).mBuffers.as_mut_ptr();
    for index in 0..declared {
        let buffer = &mut *list_buffers.add(index);
        if !buffer.mData.is_null() {
            std::ptr::write_bytes(buffer.mData as *mut u8, 0, buffer.mDataByteSize as usize);
        }
    }
}

/// An `AudioBufferList` with null data pointers, for `AudioUnitRender` to fill
/// in.
///
/// AUHAL's input side owns its buffers; asking it to render into a list of null
/// pointers makes it hand back its own, which is one copy less than allocating
/// and one allocation less in a real-time callback.
#[repr(C)]
pub struct StereoBufferList {
    pub list: AudioBufferList,
    /// `AudioBufferList` declares one buffer inline; this is the second, laid
    /// out immediately after it as the C flexible array member expects.
    pub extra: [objc2_core_audio_types::AudioBuffer; CHANNELS - 1],
}

impl StereoBufferList {
    pub fn new() -> Self {
        let empty = objc2_core_audio_types::AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: 0,
            mData: std::ptr::null_mut(),
        };
        Self {
            list: AudioBufferList {
                mNumberBuffers: CHANNELS as u32,
                mBuffers: [empty],
            },
            extra: [empty; CHANNELS - 1],
        }
    }

    /// Resets the descriptors before each render: AUHAL overwrites `mData` and
    /// `mDataByteSize`, and stale values from the last call would describe a
    /// buffer that no longer exists.
    pub fn prepare(&mut self, frames: usize) {
        let bytes = (frames * std::mem::size_of::<f32>()) as u32;
        // SAFETY: the two buffers are laid out contiguously by repr(C).
        unsafe {
            let list_buffers = self.list.mBuffers.as_mut_ptr();
            for index in 0..CHANNELS {
                let buffer = &mut *list_buffers.add(index);
                buffer.mNumberChannels = 1;
                buffer.mDataByteSize = bytes;
                buffer.mData = std::ptr::null_mut();
            }
        }
        self.list.mNumberBuffers = CHANNELS as u32;
    }

    pub fn as_ptr(&mut self) -> *mut AudioBufferList {
        &mut self.list
    }
}

impl Default for StereoBufferList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_format_is_packed_float_planar() {
        let format = client_format(48_000.0, 2);
        assert_eq!(format.mBytesPerFrame, 4);
        assert_eq!(format.mBytesPerPacket, 4);
        assert_eq!(format.mFramesPerPacket, 1);
        assert_eq!(format.mBitsPerChannel, 32);
        assert_eq!(format.mChannelsPerFrame, 2);
        assert!(format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0);
        assert!(format.mFormatFlags & kAudioFormatFlagIsFloat != 0);
    }

    #[test]
    fn the_stereo_buffer_list_lays_its_second_buffer_out_contiguously() {
        let mut list = StereoBufferList::new();
        list.prepare(512);
        // SAFETY: reading back what prepare just wrote, through the same
        // flexible-array access the callbacks use.
        let seen = unsafe { buffers(list.as_ptr()) };
        assert_eq!(seen.len(), CHANNELS);
        for buffer in seen {
            assert_eq!(buffer.mDataByteSize, 512 * 4);
            assert_eq!(buffer.mNumberChannels, 1);
            assert!(buffer.mData.is_null());
        }
    }

    #[test]
    fn planar_views_borrow_the_buffers_they_are_given() {
        let mut left = [1.0f32; 8];
        let mut right = [2.0f32; 8];
        let mut list = StereoBufferList::new();
        list.prepare(8);
        // SAFETY: repr(C) lays the two buffers out contiguously.
        unsafe {
            let list_buffers = list.list.mBuffers.as_mut_ptr();
            (*list_buffers).mData = left.as_mut_ptr() as *mut _;
            (*list_buffers.add(1)).mData = right.as_mut_ptr() as *mut _;
        }

        // SAFETY: the buffers outlive the borrow.
        let (channels, count) = unsafe { planar(list.as_ptr(), 8) };
        assert_eq!(count, 2);
        assert_eq!(channels[0], &[1.0; 8]);
        assert_eq!(channels[1], &[2.0; 8]);

        // SAFETY: the immutable view above has been dropped.
        unsafe { silence(list.as_ptr()) };
        assert_eq!(left, [0.0; 8]);
        assert_eq!(right, [0.0; 8]);
    }

    #[test]
    fn a_shorter_buffer_yields_a_shorter_slice() {
        let mut samples = [3.0f32; 4];
        let mut list = StereoBufferList::new();
        list.prepare(4);
        // SAFETY: as above.
        unsafe {
            let list_buffers = list.list.mBuffers.as_mut_ptr();
            (*list_buffers).mData = samples.as_mut_ptr() as *mut _;
            (*list_buffers.add(1)).mData = std::ptr::null_mut();
        }

        // Asking for more frames than the buffer holds must not walk past it.
        // SAFETY: the buffer outlives the borrow.
        let (channels, _) = unsafe { planar(list.as_ptr(), 64) };
        assert_eq!(channels[0].len(), 4);
        assert!(channels[1].is_empty());
    }
}
