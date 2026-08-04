//! Brings back displays that were disabled and never re-enabled — an app that
//! exits while holding a display off the device tree used to leave no way back
//! except a physical replug.
//!
//! Lists what the window server knows about, then re-enables everything that is
//! not currently online. Re-enabling a display that is already on is a no-op,
//! and ids with no hardware behind them are refused, so this is safe to run
//! blind.
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

    if missing.is_empty() && !online.is_empty() {
        println!("\nnothing to do — every known display is online");
    }

    // Anything the window server lists but the public API does not is a
    // disabled display; the ones that are online might still be mirrored.
    for id in missing {
        match power::connect(id, Strategy::HardDisconnect) {
            Ok(()) => println!("\nre-enabled display {}", id.0),
            Err(error) => println!("\ndisplay {} refused: {error:?}", id.0),
        }
    }

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
