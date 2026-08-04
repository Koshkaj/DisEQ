//! Software brightness and colour adjustment via the display's gamma ramp.
//!
//! This is the fallback for displays with no controllable backlight and no
//! working DDC. It dims by scaling the transfer table rather than touching the
//! panel, so black stays black and only the white level moves.
//!
//! Sole owner of `CGSetDisplayTransferByTable` for a display: brightness and
//! colour both feed into one table here, because two callers writing the same
//! display's ramp would each clobber the other.
//!
//! Known to be unreliable on some macOS 26 hardware (FB18559786), so
//! [`verify`] measures whether it actually works before anything depends on it.

use std::collections::HashMap;
use std::sync::Mutex;

use objc2_core_graphics::{
    CGDisplayRestoreColorSyncSettings, CGGetDisplayTransferByTable, CGSetDisplayTransferByTable,
};

use crate::display::DisplayId;

const TABLE_SIZE: usize = 256;
/// Never dim to fully black — that leaves the user with no way to see the UI
/// they would need to undo it.
const MIN_FACTOR: f64 = 0.10;

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Adjustment {
    pub brightness: f64,
    pub red: f64,
    pub green: f64,
    pub blue: f64,
}

impl Default for Adjustment {
    fn default() -> Self {
        Self {
            brightness: 1.0,
            red: 1.0,
            green: 1.0,
            blue: 1.0,
        }
    }
}

impl Adjustment {
    pub fn is_identity(&self) -> bool {
        self.brightness >= 1.0 && self.red >= 1.0 && self.green >= 1.0 && self.blue >= 1.0
    }
}

fn state() -> &'static Mutex<HashMap<u32, Adjustment>> {
    static STATE: Mutex<Option<HashMap<u32, Adjustment>>> = Mutex::new(None);
    // A plain OnceLock<Mutex<..>> would do, but this keeps the map and its lock
    // as one item.
    static INIT: std::sync::OnceLock<Mutex<HashMap<u32, Adjustment>>> = std::sync::OnceLock::new();
    let _ = &STATE;
    INIT.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn current(display: DisplayId) -> Adjustment {
    state()
        .lock()
        .map(|map| map.get(&display.0).copied().unwrap_or_default())
        .unwrap_or_default()
}

pub fn apply(display: DisplayId, adjustment: Adjustment) -> bool {
    if let Ok(mut map) = state().lock() {
        map.insert(display.0, adjustment);
    }

    if adjustment.is_identity() {
        return reset(display);
    }

    let factor = adjustment.brightness.clamp(MIN_FACTOR, 1.0);
    let mut red = [0f32; TABLE_SIZE];
    let mut green = [0f32; TABLE_SIZE];
    let mut blue = [0f32; TABLE_SIZE];

    for index in 0..TABLE_SIZE {
        let level = index as f64 / (TABLE_SIZE - 1) as f64;
        let dimmed = level * factor;
        red[index] = (dimmed * adjustment.red.clamp(0.0, 1.0)) as f32;
        green[index] = (dimmed * adjustment.green.clamp(0.0, 1.0)) as f32;
        blue[index] = (dimmed * adjustment.blue.clamp(0.0, 1.0)) as f32;
    }

    let result = unsafe {
        CGSetDisplayTransferByTable(
            display.0,
            TABLE_SIZE as u32,
            red.as_ptr(),
            green.as_ptr(),
            blue.as_ptr(),
        )
    };
    result == objc2_core_graphics::CGError::Success
}

pub fn reset(display: DisplayId) -> bool {
    if let Ok(mut map) = state().lock() {
        map.remove(&display.0);
    }
    let mut ramp = [0f32; TABLE_SIZE];
    for (index, slot) in ramp.iter_mut().enumerate() {
        *slot = index as f32 / (TABLE_SIZE - 1) as f32;
    }
    let result = unsafe {
        CGSetDisplayTransferByTable(
            display.0,
            TABLE_SIZE as u32,
            ramp.as_ptr(),
            ramp.as_ptr(),
            ramp.as_ptr(),
        )
    };
    result == objc2_core_graphics::CGError::Success
}

/// Restores every display's ramp from its colour profile.
pub fn reset_all() {
    CGDisplayRestoreColorSyncSettings();
}

/// Writes a ramp and reads it back to confirm the system honoured it.
///
/// `CGSetDisplayTransferByTable` returns success on affected macOS 26 machines
/// while silently ignoring the table, so the return code alone proves nothing.
pub fn verify(display: DisplayId) -> bool {
    let probe = 0.5f32;
    let table = [probe; TABLE_SIZE];
    let wrote = unsafe {
        CGSetDisplayTransferByTable(
            display.0,
            TABLE_SIZE as u32,
            table.as_ptr(),
            table.as_ptr(),
            table.as_ptr(),
        )
    };
    if wrote != objc2_core_graphics::CGError::Success {
        return false;
    }

    let mut red = [0f32; TABLE_SIZE];
    let mut green = [0f32; TABLE_SIZE];
    let mut blue = [0f32; TABLE_SIZE];
    let mut count: u32 = 0;
    let read = unsafe {
        CGGetDisplayTransferByTable(
            display.0,
            TABLE_SIZE as u32,
            red.as_mut_ptr(),
            green.as_mut_ptr(),
            blue.as_mut_ptr(),
            &mut count,
        )
    };
    let honoured = read == objc2_core_graphics::CGError::Success
        && count > 0
        && (red[TABLE_SIZE - 1] - probe).abs() < 0.05;

    reset(display);
    honoured
}
