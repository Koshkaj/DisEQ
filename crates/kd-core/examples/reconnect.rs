//! Brings back displays that were disabled and never re-enabled — an app that
//! exits while holding a display off the device tree used to leave no way back
//! except a physical replug.
//!
//! Lists what the window server knows about, then re-enables everything that is
//! not currently online *and* has a panel plugged into it. Re-enabling a
//! display that is already on is a no-op, and an id with nothing behind it is
//! put straight back rather than left as an invented monitor — so this is safe
//! to run blind.
//!
//!     cargo run -p kd-core --example reconnect

use kd_sys::display::{self, DisplayId};
use kd_sys::power::{self, Strategy};

fn main() {
    let online = display::online_displays();
    let known = power::known_display_ids();

    println!(
        "online : {:?}",
        online.iter().map(|d| d.0).collect::<Vec<_>>()
    );
    println!(
        "known  : {:?}",
        known.iter().map(|d| d.0).collect::<Vec<_>>()
    );

    let missing: Vec<DisplayId> = known
        .iter()
        .copied()
        .filter(|id| !online.contains(id))
        .collect();

    let dark = kd_core::power::dark_panels();
    println!(
        "plugged in and dark: {:?}",
        dark.iter()
            .map(|panel| panel.name.as_deref().unwrap_or("<unnamed>"))
            .collect::<Vec<_>>()
    );

    if missing.is_empty() && !online.is_empty() {
        println!("\nnothing to do — every known display is online");
    }

    // Anything the window server lists but the public API does not is a
    // disabled display; the ones that are online might still be mirrored.
    let restored = kd_core::power::restore_disabled();
    println!("\nre-enabled {restored} display(s)");

    for id in display::online_displays() {
        let snapshot = display::snapshot(id);
        if snapshot.mirrors.is_some() {
            match power::connect(id, Strategy::Mirror) {
                Ok(()) => println!("un-mirrored display {} ({})", id.0, snapshot.name),
                Err(error) => println!("display {} refused: {error:?}", id.0),
            }
        }
    }

    println!("\nnow online:");
    for id in display::online_displays() {
        let snapshot = display::snapshot(id);
        println!(
            "  {} (id {})  active={} mirrors={:?}",
            snapshot.name,
            id.0,
            snapshot.is_active,
            snapshot.mirrors.map(|m| m.0)
        );
    }
}
