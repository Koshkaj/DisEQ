//! Reaching the `AudioUnit` behind an `AVAudioNode`.
//!
//! `objc2-avf-audio` declares `-[AVAudioIONode audioUnit]` as returning
//! `objc2-audio-toolbox`'s `AudioUnit`, which is
//! `*mut OpaqueAudioComponentInstance`. The Objective-C runtime disagrees: the
//! method is compiled with the Carbon spelling the typedef still carries,
//! `ComponentInstanceRecord *`, encoded `^{ComponentInstanceRecord=[1q]}`.
//!
//! Same pointer either way, but objc2 verifies encodings on every message send
//! in a debug build and panics on the mismatch — which is a crash on the first
//! route, in exactly the build used for development. So the send is declared
//! here with the runtime's spelling and the result cast back.

use objc2::encode::{Encoding, RefEncode};
use objc2::{msg_send, Message};
use objc2_audio_toolbox::AudioUnit;

/// The runtime's `AudioUnit`: an opaque pointer whose encoding names the
/// Carbon-era struct. The single `long long` is what `[1q]` describes; nothing
/// reads it, and nothing may — the contents belong to AudioToolbox.
#[repr(C)]
pub struct ComponentInstanceRecord {
    _opaque: [i64; 1],
}

// SAFETY: the encoding is the one the runtime reports for `-[AVAudioIONode
// audioUnit]`, and the type is never constructed on this side — only pointed
// at.
unsafe impl RefEncode for ComponentInstanceRecord {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "ComponentInstanceRecord",
        &[Encoding::Array(1, &Encoding::LongLong)],
    ));
}

/// The `AudioUnit` behind an `AVAudioIONode` or an `AVAudioUnit`.
///
/// Returns null when the node has none, which is what the Objective-C method
/// promises; every caller here treats null as "this node cannot be configured".
///
/// # Safety
///
/// `node` must be an object that responds to `audioUnit` — an `AVAudioIONode`
/// or an `AVAudioUnit`. The returned unit is owned by the node and is only
/// valid while the node is.
pub unsafe fn audio_unit<T: Message>(node: &T) -> AudioUnit {
    let unit: *mut ComponentInstanceRecord = unsafe { msg_send![node, audioUnit] };
    unit.cast()
}
