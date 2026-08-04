//! The capture side, without a capture API.
//!
//! coreaudiod hands our HAL plug-in every frame the machine plays, because the
//! plug-in *is* the output device. No permission guards that. The only reason
//! this app ever needed one was to get those frames out of coreaudiod:
//!
//! | Route in | Permission |
//! |---|---|
//! | the device's input stream, through `AVAudioEngine` — eqMac's way | Microphone |
//! | a process tap | System Audio Recording |
//! | POSIX shared memory, written by our own plug-in | none |
//!
//! The third is what this is. `com.apple.audio.coreaudiod.sb` allows
//! `ipc-posix-shm` outright, so the plug-in can publish a ring buffer, and this
//! app maps it read-only. No capture API is involved anywhere, so TCC has
//! nothing to ask about.
//!
//! One writer, one reader, no locks. The writer publishes `written` after the
//! frames it describes; a reader that trusts nothing beyond `written` cannot
//! read a frame that is only half there.
//!
//! Written from scratch: eqMac's public tree reads its driver's input stream
//! instead, and asks for the microphone to do it.

use std::ffi::CString;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

/// Must match `kSharedName` in `driver/Source/DisEQ.c`.
const NAME: &str = "/DisEQ.audio";
/// `'KDSN'`, written last so a reader that sees it sees a filled-in header.
const MAGIC: u32 = 0x4B44_534E;
const VERSION: u32 = 1;

/// The header the driver writes, laid out exactly as the C struct is.
///
/// Fixed-width types and explicit padding: this is read by another process
/// compiled by another compiler, so nothing here may depend on either.
#[repr(C)]
struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    channels: AtomicU32,
    capacity_frames: AtomicU32,
    sample_rate_bits: AtomicU64,
    written: AtomicI64,
    running: AtomicU32,
    _reserved: AtomicU32,
}

#[derive(Debug)]
pub enum SharedError {
    /// No segment. The driver is not installed, or has not been loaded since
    /// it was.
    Absent,
    /// The segment is there but could not be mapped.
    Unmappable(i32),
    /// A driver from a different version of this project.
    Version { found: u32, wanted: u32 },
    /// Mapped, but the header has not been filled in — the plug-in is loading.
    NotReady,
}

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => write!(
                f,
                "the DisEQ driver is not loaded — run 'make driver-install'"
            ),
            Self::Unmappable(errno) => {
                write!(f, "the driver's audio buffer could not be mapped ({errno})")
            }
            Self::Version { found, wanted } => write!(
                f,
                "the installed driver speaks version {found}, this app speaks {wanted} — \
                 run 'make driver-install'"
            ),
            Self::NotReady => write!(f, "the driver is still starting up"),
        }
    }
}

impl std::error::Error for SharedError {}

/// A read-only view of the driver's ring.
///
/// `Send` and `Sync`: the mapping is read-only for this process and every field
/// that changes is atomic, so any number of threads may look at it.
pub struct SharedRing {
    base: *const u8,
    bytes: usize,
    capacity: usize,
    channels: usize,
    sample_rate: f64,
}

// SAFETY: the mapping is read-only here and the only mutable state is atomics
// written by the driver.
unsafe impl Send for SharedRing {}
unsafe impl Sync for SharedRing {}

impl SharedRing {
    /// Maps the driver's ring, read-only.
    pub fn open() -> Result<Self, SharedError> {
        let name = CString::new(NAME).expect("the name has no interior nul");
        // SAFETY: a well-formed name and flags; the descriptor is closed below
        // whatever happens.
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY) };
        if fd < 0 {
            return Err(SharedError::Absent);
        }

        let mut status: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `status` is the right size.
        if unsafe { libc::fstat(fd, &mut status) } != 0 {
            unsafe { libc::close(fd) };
            return Err(SharedError::Unmappable(errno()));
        }
        let bytes = status.st_size as usize;
        if bytes < std::mem::size_of::<Header>() {
            unsafe { libc::close(fd) };
            return Err(SharedError::NotReady);
        }

        // SAFETY: mapping `bytes` of a descriptor that reports that size.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        unsafe { libc::close(fd) };
        if base == libc::MAP_FAILED {
            return Err(SharedError::Unmappable(errno()));
        }
        let base = base as *const u8;

        // SAFETY: the mapping is at least a header long, checked above.
        let header = unsafe { &*(base as *const Header) };
        if header.magic.load(Ordering::Acquire) != MAGIC {
            unsafe { libc::munmap(base as *mut _, bytes) };
            return Err(SharedError::NotReady);
        }
        let version = header.version.load(Ordering::Acquire);
        if version != VERSION {
            unsafe { libc::munmap(base as *mut _, bytes) };
            return Err(SharedError::Version {
                found: version,
                wanted: VERSION,
            });
        }

        let capacity = header.capacity_frames.load(Ordering::Acquire) as usize;
        let channels = header.channels.load(Ordering::Acquire) as usize;
        let sample_rate = f64::from_bits(header.sample_rate_bits.load(Ordering::Acquire));
        if capacity == 0 || channels == 0 {
            unsafe { libc::munmap(base as *mut _, bytes) };
            return Err(SharedError::NotReady);
        }

        Ok(Self {
            base,
            bytes,
            capacity,
            channels,
            sample_rate,
        })
    }

    fn header(&self) -> &Header {
        // SAFETY: the mapping starts with a header and outlives this borrow.
        unsafe { &*(self.base as *const Header) }
    }

    fn samples(&self) -> *const f32 {
        // SAFETY: the samples follow the header, within the mapping.
        unsafe { self.base.add(std::mem::size_of::<Header>()) as *const f32 }
    }

    /// Sample time one past the last frame the driver has written.
    pub fn written(&self) -> i64 {
        self.header().written.load(Ordering::Acquire)
    }

    /// Whether the device has IO running. False means nothing is playing
    /// through it, and a reader should be silent rather than replay the ring.
    pub fn is_running(&self) -> bool {
        self.header().running.load(Ordering::Acquire) != 0
    }

    /// The rate the driver is running at, as it last published it.
    pub fn sample_rate(&self) -> f64 {
        f64::from_bits(self.header().sample_rate_bits.load(Ordering::Acquire))
    }

    /// The rate read when the ring was mapped, for callers that want a value
    /// that cannot change under them.
    pub fn opened_sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Copies `frames` frames starting at sample time `from` into `destination`,
    /// de-interleaving as it goes.
    ///
    /// Real-time safe: no allocation, no locks, no syscalls. Returns false when
    /// the request is not entirely inside what the driver has written and still
    /// holds, in which case `destination` is left untouched — the caller
    /// silences rather than playing whatever happened to be there.
    pub fn read(&self, destination: &mut [&mut [f32]], from: i64, frames: usize) -> bool {
        if frames == 0 {
            return true;
        }
        let written = self.written();
        let oldest = written - self.capacity as i64;
        if from < oldest || from + frames as i64 > written {
            return false;
        }

        let samples = self.samples();
        let channels = self.channels;
        for frame in 0..frames {
            let position = (from + frame as i64).rem_euclid(self.capacity as i64) as usize;
            for (channel, plane) in destination.iter_mut().enumerate() {
                if frame >= plane.len() {
                    break;
                }
                // A reader asking for more channels than the ring has gets the
                // last one repeated, which is the sane answer for mono.
                let source = channel.min(channels - 1);
                // SAFETY: `position` is inside the ring by construction and
                // `source` inside a frame.
                plane[frame] = unsafe { *samples.add(position * channels + source) };
            }
        }
        true
    }
}

impl Drop for SharedRing {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what was mapped, once.
        unsafe { libc::munmap(self.base as *mut _, self.bytes) };
    }
}

fn errno() -> i32 {
    // SAFETY: `__error` returns a pointer to this thread's errno.
    unsafe { *libc::__error() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The driver is compiled separately and loaded into coreaudiod, so a
    /// mismatch here is not a build error — it is silence at runtime, which is
    /// the hardest failure in this project to recognise.
    const DRIVER: &str = include_str!("../../../driver/Source/DisEQ.c");

    #[test]
    fn the_segment_name_is_the_one_the_driver_creates() {
        assert!(
            DRIVER.contains(&format!("#define kSharedName    \"{NAME}\"")),
            "driver/Source/DisEQ.c no longer creates {NAME}"
        );
    }

    #[test]
    fn the_magic_is_the_one_the_driver_publishes() {
        assert!(
            DRIVER.contains(&format!("#define kSharedMagic   0x{MAGIC:X}")),
            "driver/Source/DisEQ.c no longer publishes magic 0x{MAGIC:X}"
        );
    }

    #[test]
    fn the_version_is_the_one_the_driver_publishes() {
        assert!(
            DRIVER.contains(&format!("#define kSharedVersion {VERSION}u")),
            "driver/Source/DisEQ.c no longer publishes version {VERSION}"
        );
    }

    #[test]
    fn the_header_matches_the_c_struct_field_for_field() {
        // Same order, same widths: the reader casts the mapping straight to
        // this type, so a field inserted on one side alone silently shifts
        // every field after it.
        let c_fields: Vec<&str> = DRIVER
            .split("struct kd_SharedHeader {")
            .nth(1)
            .expect("the driver still declares kd_SharedHeader")
            .split('}')
            .next()
            .unwrap()
            .lines()
            .filter_map(|line| line.trim().strip_prefix("_Atomic "))
            .filter_map(|line| line.strip_suffix(';'))
            .collect();

        assert_eq!(
            c_fields,
            vec![
                "uint32_t magic",
                "uint32_t version",
                "uint32_t channels",
                "uint32_t capacityFrames",
                "uint64_t sampleRateBits",
                "int64_t written",
                "uint32_t running",
                "uint32_t reserved",
            ]
        );
        assert_eq!(std::mem::size_of::<Header>(), 4 * 4 + 8 + 8 + 4 + 4);
    }
}
