use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString};
use objc2_core_graphics::{
    kCGDisplayShowDuplicateLowResolutionModes, CGDirectDisplayID, CGDisplayBounds,
    CGDisplayCopyAllDisplayModes, CGDisplayCopyDisplayMode, CGDisplayIsActive, CGDisplayIsBuiltin,
    CGDisplayIsMain, CGDisplayIsOnline, CGDisplayMirrorsDisplay, CGDisplayMode,
    CGDisplayModelNumber, CGDisplayRotation, CGDisplaySerialNumber, CGDisplayUnitNumber,
    CGDisplayVendorNumber, CGGetOnlineDisplayList, CGMainDisplayID,
};

const MAX_DISPLAYS: u32 = 16;

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct DisplayId(pub CGDirectDisplayID);

#[derive(Clone, PartialEq, Debug)]
pub struct DisplayMode {
    pub width: usize,
    pub height: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub refresh_rate: f64,
    pub io_mode_id: i32,
}

impl DisplayMode {
    pub fn is_hidpi(&self) -> bool {
        self.pixel_width > self.width
    }

    pub fn pixel_area(&self) -> usize {
        self.pixel_width * self.pixel_height
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct DisplaySnapshot {
    pub id: DisplayId,
    pub name: String,
    pub is_builtin: bool,
    pub is_main: bool,
    pub is_active: bool,
    pub is_online: bool,
    pub vendor: u32,
    pub model: u32,
    pub serial: u32,
    pub unit: u32,
    pub rotation: f64,
    pub origin: (f64, f64),
    pub size: (f64, f64),
    pub mirrors: Option<DisplayId>,
    pub current_mode: Option<DisplayMode>,
}

pub fn main_display() -> DisplayId {
    DisplayId(CGMainDisplayID())
}

pub fn online_displays() -> Vec<DisplayId> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS as usize];
    let mut count: u32 = 0;
    let err = unsafe { CGGetOnlineDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    if err != objc2_core_graphics::CGError::Success {
        return Vec::new();
    }
    ids[..count as usize]
        .iter()
        .copied()
        .map(DisplayId)
        .collect()
}

pub fn snapshot(id: DisplayId) -> DisplaySnapshot {
    let raw = id.0;
    let bounds = CGDisplayBounds(raw);
    let mirrors = match CGDisplayMirrorsDisplay(raw) {
        0 => None,
        source => Some(DisplayId(source)),
    };

    DisplaySnapshot {
        id,
        name: localized_name(id),
        is_builtin: CGDisplayIsBuiltin(raw),
        is_main: CGDisplayIsMain(raw),
        is_active: CGDisplayIsActive(raw),
        is_online: CGDisplayIsOnline(raw),
        vendor: CGDisplayVendorNumber(raw),
        model: CGDisplayModelNumber(raw),
        serial: CGDisplaySerialNumber(raw),
        unit: CGDisplayUnitNumber(raw),
        rotation: CGDisplayRotation(raw),
        origin: (bounds.origin.x, bounds.origin.y),
        size: (bounds.size.width, bounds.size.height),
        mirrors,
        current_mode: current_mode(id),
    }
}

pub fn current_mode(id: DisplayId) -> Option<DisplayMode> {
    let mode = CGDisplayCopyDisplayMode(id.0)?;
    Some(describe_mode(&mode))
}

/// All modes usable for the desktop GUI, including the HiDPI duplicates macOS
/// hides by default.
pub fn mode_query_options() -> CFRetained<CFDictionary> {
    let typed: CFRetained<CFDictionary<CFString, CFBoolean>> = CFDictionary::from_slices(
        &[unsafe { kCGDisplayShowDuplicateLowResolutionModes }],
        &[CFBoolean::new(true)],
    );
    unsafe { CFRetained::cast_unchecked(typed) }
}

pub fn modes(id: DisplayId) -> Vec<DisplayMode> {
    let options = mode_query_options();
    let Some(array) = (unsafe { CGDisplayCopyAllDisplayModes(id.0, Some(&options)) }) else {
        return Vec::new();
    };
    let array: CFRetained<CFArray<CGDisplayMode>> = unsafe { CFRetained::cast_unchecked(array) };

    let mut out = Vec::with_capacity(array.len());
    for mode in array.iter() {
        if CGDisplayMode::is_usable_for_desktop_gui(Some(&mode)) {
            out.push(describe_mode(&mode));
        }
    }
    out
}

/// The panel's native mode: the largest 1x mode, whose backing store equals the
/// physical panel resolution.
///
/// Two traps here. `CGDisplayPixelsWide/High` reports the *current* mode rather
/// than the panel, so it cannot be used. And the largest mode by pixel area is
/// not native either — scaled HiDPI modes render into a backing store larger
/// than the panel (a 4K panel offers 3360x1890@2x = 6720x3780) and macOS
/// downsamples. Only 1x modes describe real hardware.
pub fn native_mode(id: DisplayId) -> Option<DisplayMode> {
    let all = modes(id);
    all.iter()
        .filter(|m| !m.is_hidpi())
        .max_by_key(|m| m.pixel_area())
        .or_else(|| all.iter().max_by_key(|m| m.pixel_area()))
        .cloned()
}

fn describe_mode(mode: &CGDisplayMode) -> DisplayMode {
    DisplayMode {
        width: CGDisplayMode::width(Some(mode)),
        height: CGDisplayMode::height(Some(mode)),
        pixel_width: CGDisplayMode::pixel_width(Some(mode)),
        pixel_height: CGDisplayMode::pixel_height(Some(mode)),
        refresh_rate: CGDisplayMode::refresh_rate(Some(mode)),
        io_mode_id: CGDisplayMode::io_display_mode_id(Some(mode)),
    }
}

/// Display name via `NSScreen.localizedName`.
///
/// IOKit vendor/product matching is unreliable — CoreGraphics and IOKit do not
/// always agree on those identifiers for the same panel — so the AppKit name is
/// the only dependable source.
fn localized_name(id: DisplayId) -> String {
    use objc2_app_kit::NSScreen;
    use objc2_foundation::{ns_string, MainThreadMarker};

    let Some(mtm) = MainThreadMarker::new() else {
        return fallback_name(id);
    };

    for screen in NSScreen::screens(mtm).iter() {
        let description = screen.deviceDescription();
        let Some(number) = description.objectForKey(ns_string!("NSScreenNumber")) else {
            continue;
        };
        let Ok(number) = number.downcast::<objc2_foundation::NSNumber>() else {
            continue;
        };
        if number.as_u32() == id.0 {
            return screen.localizedName().to_string();
        }
    }
    fallback_name(id)
}

fn fallback_name(id: DisplayId) -> String {
    if CGDisplayIsBuiltin(id.0) {
        "Built-in Display".to_string()
    } else {
        format!("Display {}", id.0)
    }
}
