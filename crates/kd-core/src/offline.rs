//! Displays this app has taken out of the desktop layout.
//!
//! A hard disconnect removes the display from the device tree, so it stops
//! appearing in `CGGetOnlineDisplayList` — and with it, the card whose toggle
//! would bring it back. Every disconnect is recorded here, and the record
//! outlives the process: quitting while a display is off must not leave the
//! only way back a physical replug.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use kd_sys::display::{DisplayId, DisplayMode, DisplaySnapshot};
use kd_sys::power::Strategy;

use crate::display::Display;

/// One display that was switched off, in enough detail to show a card for it
/// and to recognise it when it comes back under a different id.
#[derive(Clone, Debug)]
pub struct Offline {
    pub id: DisplayId,
    pub name: String,
    pub is_builtin: bool,
    pub vendor: u32,
    pub model: u32,
    pub serial: u32,
    pub unit: u32,
    /// Which mechanism took it out, so the reverse can match.
    pub strategy: Strategy,
}

impl Offline {
    fn from_display(display: &Display, strategy: Strategy) -> Self {
        let snapshot = &display.snapshot;
        Self {
            id: snapshot.id,
            name: snapshot.name.clone(),
            is_builtin: snapshot.is_builtin,
            vendor: snapshot.vendor,
            model: snapshot.model,
            serial: snapshot.serial,
            unit: snapshot.unit,
            strategy,
        }
    }

    /// Same panel, different session: ids are handed out afresh on every boot,
    /// so identity is the hardware, not the number.
    pub fn matches(&self, snapshot: &DisplaySnapshot) -> bool {
        if self.vendor != 0 || self.model != 0 || self.serial != 0 {
            return self.vendor == snapshot.vendor
                && self.model == snapshot.model
                && self.serial == snapshot.serial;
        }
        self.is_builtin == snapshot.is_builtin && self.unit == snapshot.unit
    }

    /// A card for a display the system no longer reports, built from what was
    /// recorded before it went away.
    pub fn ghost(&self) -> Display {
        Display {
            snapshot: DisplaySnapshot {
                id: self.id,
                name: self.name.clone(),
                is_builtin: self.is_builtin,
                is_main: false,
                is_active: false,
                is_online: false,
                vendor: self.vendor,
                model: self.model,
                serial: self.serial,
                unit: self.unit,
                rotation: 0.0,
                origin: (0.0, 0.0),
                size: (0.0, 0.0),
                mirrors: None,
                current_mode: None,
            },
            modes: Vec::<DisplayMode>::new(),
            native: None,
        }
    }

    fn to_line(&self) -> String {
        let strategy = match self.strategy {
            Strategy::Mirror => "mirror",
            Strategy::HardDisconnect => "hard",
        };
        // Tab-separated: the name is last so it can contain anything but a tab.
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.id.0,
            self.vendor,
            self.model,
            self.serial,
            self.unit,
            self.is_builtin as u8,
            strategy,
            self.name
        )
    }

    fn from_line(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let mut next = || fields.next();
        Some(Self {
            id: DisplayId(next()?.parse().ok()?),
            vendor: next()?.parse().ok()?,
            model: next()?.parse().ok()?,
            serial: next()?.parse().ok()?,
            unit: next()?.parse().ok()?,
            is_builtin: next()? == "1",
            strategy: match next()? {
                "hard" => Strategy::HardDisconnect,
                "mirror" => Strategy::Mirror,
                _ => return None,
            },
            name: next()?.to_string(),
        })
    }
}

fn registry() -> &'static Mutex<HashMap<u32, Offline>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u32, Offline>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut map = HashMap::new();
        for entry in read_file() {
            map.insert(entry.id.0, entry);
        }
        Mutex::new(map)
    })
}

pub fn record(display: &Display, strategy: Strategy) {
    let entry = Offline::from_display(display, strategy);
    if let Ok(mut map) = registry().lock() {
        map.insert(entry.id.0, entry);
        write_file(&map);
    }
}

pub fn forget(display: DisplayId) {
    if let Ok(mut map) = registry().lock() {
        if map.remove(&display.0).is_some() {
            write_file(&map);
        }
    }
}

pub fn is_offline(display: DisplayId) -> bool {
    registry()
        .lock()
        .map(|map| map.contains_key(&display.0))
        .unwrap_or(false)
}

/// How `display` was taken out of the layout, if this app took it out.
pub fn strategy(display: DisplayId) -> Option<Strategy> {
    let map = registry().lock().ok()?;
    Some(map.get(&display.0)?.strategy)
}

pub fn all() -> Vec<Offline> {
    registry()
        .lock()
        .map(|map| map.values().cloned().collect())
        .unwrap_or_default()
}

/// Re-keys a record onto the id the display has now.
///
/// A display that comes back after a reboot is the same panel with a new id,
/// and the record has to follow it or the toggle would act on nothing.
pub fn rekey(snapshot: &DisplaySnapshot) -> bool {
    let Ok(mut map) = registry().lock() else {
        return false;
    };
    let Some(old) = map
        .values()
        .find(|entry| entry.id != snapshot.id && entry.matches(snapshot))
        .map(|entry| entry.id.0)
    else {
        return false;
    };

    if let Some(mut entry) = map.remove(&old) {
        entry.id = snapshot.id;
        entry.name = snapshot.name.clone();
        map.insert(snapshot.id.0, entry);
        write_file(&map);
    }
    true
}

// --- storage ----------------------------------------------------------------

fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/DisEQ")
            .join("offline.tsv"),
    )
}

fn read_file() -> Vec<Offline> {
    let Some(path) = path() else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines().filter_map(Offline::from_line).collect()
}

fn write_file(map: &HashMap<u32, Offline>) {
    let Some(path) = path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut text = String::new();
    for entry in map.values() {
        text.push_str(&entry.to_line());
        text.push('\n');
    }
    // Nothing to do about a failed write but carry on: the in-memory registry
    // still works for this session.
    let _ = fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Offline {
        Offline {
            id: DisplayId(3),
            name: "DELL U2720Q".to_string(),
            is_builtin: false,
            vendor: 0x10AC,
            model: 0x41B5,
            serial: 0x4237314C,
            unit: 2,
            strategy: Strategy::HardDisconnect,
        }
    }

    #[test]
    fn round_trips_through_a_line() {
        let entry = sample();
        let parsed = Offline::from_line(&entry.to_line()).expect("parses");

        assert_eq!(parsed.id, entry.id);
        assert_eq!(parsed.name, entry.name);
        assert_eq!(parsed.vendor, entry.vendor);
        assert_eq!(parsed.model, entry.model);
        assert_eq!(parsed.serial, entry.serial);
        assert_eq!(parsed.unit, entry.unit);
        assert_eq!(parsed.is_builtin, entry.is_builtin);
        assert_eq!(parsed.strategy, entry.strategy);
    }

    #[test]
    fn mirror_strategy_survives_the_round_trip() {
        let mut entry = sample();
        entry.strategy = Strategy::Mirror;
        let parsed = Offline::from_line(&entry.to_line()).expect("parses");
        assert_eq!(parsed.strategy, Strategy::Mirror);
    }

    #[test]
    fn rejects_junk_lines() {
        assert!(Offline::from_line("").is_none());
        assert!(Offline::from_line("3\t1\t2").is_none());
        assert!(Offline::from_line("3\t1\t2\t3\t4\t0\tsomething\tName").is_none());
    }

    /// The id changes between sessions; the panel behind it does not.
    #[test]
    fn matches_the_same_panel_under_a_new_id() {
        let entry = sample();
        let mut snapshot = entry.ghost().snapshot;
        snapshot.id = DisplayId(7);
        assert!(entry.matches(&snapshot));

        snapshot.serial = 0xDEADBEEF;
        assert!(!entry.matches(&snapshot));
    }

    /// Built-in panels report zeroed vendor/model/serial, so identity falls
    /// back to the unit number.
    #[test]
    fn matches_a_built_in_panel_by_unit() {
        let entry = Offline {
            id: DisplayId(1),
            name: "Built-in Display".to_string(),
            is_builtin: true,
            vendor: 0,
            model: 0,
            serial: 0,
            unit: 0,
            strategy: Strategy::HardDisconnect,
        };

        let mut snapshot = entry.ghost().snapshot;
        snapshot.id = DisplayId(4);
        assert!(entry.matches(&snapshot));

        snapshot.unit = 1;
        assert!(!entry.matches(&snapshot));
    }

    #[test]
    fn a_ghost_card_reads_as_disconnected() {
        let ghost = sample().ghost();
        assert!(!ghost.snapshot.is_active);
        assert!(!ghost.snapshot.is_online);
        assert!(ghost.snapshot.mirrors.is_none());
        assert!(ghost.snapshot.current_mode.is_none());
        assert_eq!(ghost.name(), "DELL U2720Q");
    }
}
