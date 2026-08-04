//! Built-in panel brightness.
//!
//! Two private frameworks expose this and neither is reliable alone:
//! `DisplayServices` is the modern path, `CoreDisplay` the one that still
//! answers on some machines when the former does not. Both are resolved with
//! `dlsym`, so a missing symbol degrades this to unavailable rather than
//! breaking the app.

use std::sync::OnceLock;

use crate::display::DisplayId;
use crate::dylib::Framework;

const DISPLAY_SERVICES: &str =
    "/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices";
const CORE_DISPLAY: &str = "/System/Library/Frameworks/CoreDisplay.framework/CoreDisplay";

type DsGet = unsafe extern "C" fn(u32, *mut f32) -> i32;
type DsSet = unsafe extern "C" fn(u32, f32) -> i32;
type DsCan = unsafe extern "C" fn(u32) -> bool;
type CdGet = unsafe extern "C" fn(u32) -> f64;
type CdSet = unsafe extern "C" fn(u32, f64);
type CdAutoGet = unsafe extern "C" fn(u32) -> bool;
type CdAutoSet = unsafe extern "C" fn(u32, bool);

#[derive(Default)]
struct Symbols {
    ds_get: Option<DsGet>,
    ds_set: Option<DsSet>,
    ds_can: Option<DsCan>,
    cd_get: Option<CdGet>,
    cd_set: Option<CdSet>,
    cd_auto_get: Option<CdAutoGet>,
    cd_auto_set: Option<CdAutoSet>,
}

fn symbols() -> &'static Symbols {
    static SYMBOLS: OnceLock<Symbols> = OnceLock::new();
    SYMBOLS.get_or_init(|| {
        let mut symbols = Symbols::default();
        if let Some(ds) = Framework::open(DISPLAY_SERVICES) {
            unsafe {
                symbols.ds_get = ds.symbol("DisplayServicesGetBrightness");
                symbols.ds_set = ds.symbol("DisplayServicesSetBrightness");
                symbols.ds_can = ds.symbol("DisplayServicesCanChangeBrightness");
            }
        }
        if let Some(cd) = Framework::open(CORE_DISPLAY) {
            unsafe {
                symbols.cd_get = cd.symbol("CoreDisplay_Display_GetUserBrightness");
                symbols.cd_set = cd.symbol("CoreDisplay_Display_SetUserBrightness");
                symbols.cd_auto_get = cd.symbol("CoreDisplay_Display_GetAutoBrightnessIsEnabled");
                symbols.cd_auto_set = cd.symbol("CoreDisplay_Display_SetAutoBrightnessIsEnabled");
            }
        }
        symbols
    })
}

/// Whether this display has a controllable backlight.
pub fn is_supported(display: DisplayId) -> bool {
    let symbols = symbols();
    if let Some(can) = symbols.ds_can {
        if unsafe { can(display.0) } {
            return true;
        }
    }
    get(display).is_some()
}

/// Current brightness in 0.0..=1.0.
pub fn get(display: DisplayId) -> Option<f64> {
    let symbols = symbols();

    if let Some(get) = symbols.ds_get {
        let mut value: f32 = 0.0;
        if unsafe { get(display.0, &mut value) } == 0 && value >= 0.0 {
            return Some(value as f64);
        }
    }
    if let Some(get) = symbols.cd_get {
        let value = unsafe { get(display.0) };
        if value > 0.0 {
            return Some(value.clamp(0.0, 1.0));
        }
    }
    None
}

pub fn set(display: DisplayId, value: f64) -> bool {
    let value = value.clamp(0.0, 1.0);
    let symbols = symbols();

    if let Some(set) = symbols.ds_set {
        if unsafe { set(display.0, value as f32) } == 0 {
            return true;
        }
    }
    if let Some(set) = symbols.cd_set {
        unsafe { set(display.0, value) };
        return true;
    }
    false
}

pub fn auto_brightness(display: DisplayId) -> Option<bool> {
    symbols().cd_auto_get.map(|get| unsafe { get(display.0) })
}

pub fn set_auto_brightness(display: DisplayId, enabled: bool) -> bool {
    match symbols().cd_auto_set {
        Some(set) => {
            unsafe { set(display.0, enabled) };
            true
        }
        None => false,
    }
}
