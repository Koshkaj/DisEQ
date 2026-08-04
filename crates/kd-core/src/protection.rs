//! Configuration protection: pin a display's layout and put it back when
//! something else changes it.
//!
//! macOS re-arranges displays on wake, on hotplug, and when apps reconfigure
//! them. Protection records the wanted state and restores it whenever the
//! system drifts.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use kd_sys::config::{self, Scope};
use kd_sys::display::{self, DisplayId};
use kd_sys::watch::{self, Change};

#[derive(Clone, PartialEq, Debug)]
struct Pinned {
    io_mode_id: i32,
    origin: (i32, i32),
    mirrors: Option<DisplayId>,
}

fn pinned() -> &'static Mutex<HashMap<u32, Pinned>> {
    static PINNED: OnceLock<Mutex<HashMap<u32, Pinned>>> = OnceLock::new();
    PINNED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Starts watching for reconfigurations. Safe to call more than once.
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        watch::on_reconfiguration(|display, change| {
            // A display that has just gone away cannot be restored, and one
            // that just arrived has no pinned state yet.
            if matches!(change, Change::Removed | Change::Added) {
                return;
            }
            restore(display);
        });
    });
}

pub fn is_protected(display: DisplayId) -> bool {
    pinned()
        .lock()
        .map(|map| map.contains_key(&display.0))
        .unwrap_or(false)
}

/// Pins the display's current layout.
pub fn protect(display: DisplayId) {
    install();
    let snapshot = display::snapshot(display);
    let Some(mode) = snapshot.current_mode else {
        return;
    };
    let entry = Pinned {
        io_mode_id: mode.io_mode_id,
        origin: (snapshot.origin.0 as i32, snapshot.origin.1 as i32),
        mirrors: snapshot.mirrors,
    };
    if let Ok(mut map) = pinned().lock() {
        map.insert(display.0, entry);
    }
}

pub fn unprotect(display: DisplayId) {
    if let Ok(mut map) = pinned().lock() {
        map.remove(&display.0);
    }
}

pub fn describe(display: DisplayId) -> Option<String> {
    let map = pinned().lock().ok()?;
    let entry = map.get(&display.0)?;
    Some(format!(
        "mode {} at {},{}",
        entry.io_mode_id, entry.origin.0, entry.origin.1
    ))
}

/// Puts `display` back to its pinned layout if it has drifted.
fn restore(display: DisplayId) {
    let Some(wanted) = pinned()
        .lock()
        .ok()
        .and_then(|map| map.get(&display.0).cloned())
    else {
        return;
    };

    let snapshot = display::snapshot(display);
    if !snapshot.is_online {
        return;
    }

    let mode_drifted = snapshot
        .current_mode
        .as_ref()
        .is_none_or(|mode| mode.io_mode_id != wanted.io_mode_id);
    if mode_drifted {
        config::set_mode(display, wanted.io_mode_id, Scope::Permanent);
    }

    let origin_drifted = (snapshot.origin.0 as i32, snapshot.origin.1 as i32) != wanted.origin;
    if origin_drifted {
        config::set_origin(display, wanted.origin.0, wanted.origin.1, Scope::Permanent);
    }

    if snapshot.mirrors != wanted.mirrors {
        match wanted.mirrors {
            Some(source) => config::set_mirror(display, source, Scope::Session),
            None => config::clear_mirror(display, Scope::Session),
        };
    }
}
