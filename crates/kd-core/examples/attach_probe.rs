//! Read-only: what each source of truth says about the displays on this
//! machine, side by side.
//!
//!     cargo run -p kd-core --example attach_probe
//!
//! Three lists that are easy to assume are the same and are not:
//!
//! | Source | Says |
//! |---|---|
//! | `CGGetOnlineDisplayList` | displays the desktop can use |
//! | `CGSGetDisplayList` | displays the window server knows, disabled included |
//! | IOKit `AppleCLCD2` EDID | panels physically plugged in, lit or not |
//!
//! The third is the one this project was missing. A display that has been
//! switched off is still plugged in, and one whose cable has been pulled is
//! not — and nothing in CoreGraphics tells those apart, because a disabled
//! display drops out of the public list in exactly the same way.
//!
//! Writes nothing and changes nothing.

use kd_sys::display;
use kd_sys::panel;
use kd_sys::power;

fn main() {
    let online = display::online_displays();
    let known = power::known_display_ids();
    let attached = panel::attached_panels();

    println!("CGGetOnlineDisplayList ({}):", online.len());
    for id in &online {
        let snapshot = display::snapshot(*id);
        println!(
            "  id {:<10} {:<28} builtin={} active={} vendor=0x{:04X} model=0x{:04X} serial=0x{:08X}",
            id.0,
            snapshot.name,
            snapshot.is_builtin,
            snapshot.is_active,
            snapshot.vendor,
            snapshot.model,
            snapshot.serial
        );
    }

    println!("\nCGSGetDisplayList ({}):", known.len());
    for id in &known {
        println!(
            "  id {:<10} {}",
            id.0,
            if online.contains(id) {
                "online"
            } else {
                "NOT online — disabled, or a stale id with nothing behind it"
            }
        );
    }

    println!(
        "\nIOKit, physically attached external panels ({}, detection {}):",
        attached.len(),
        if panel::detection_is_available() {
            "available"
        } else {
            "UNAVAILABLE — every query answers Unknown"
        }
    );
    for panel in &attached {
        println!(
            "  {:<28} vendor=0x{:04X} model=0x{:04X} serial=0x{:08X}",
            panel.name.as_deref().unwrap_or("<unnamed>"),
            panel.vendor,
            panel.model,
            panel.serial
        );
    }

    println!("\nattached but not online — plugged in and dark:");
    let mut any = false;
    for panel in &attached {
        if online
            .iter()
            .any(|id| panel.matches_snapshot(&display::snapshot(*id)))
        {
            continue;
        }
        any = true;
        println!("  {}", panel.name.as_deref().unwrap_or("<unnamed>"));
    }
    if !any {
        println!("  (none)");
    }

    println!("\nonline external displays with no panel behind them — phantoms:");
    let mut any = false;
    for id in &online {
        let snapshot = display::snapshot(*id);
        if snapshot.is_builtin || attached.iter().any(|p| p.matches_snapshot(&snapshot)) {
            continue;
        }
        any = true;
        println!("  {} (id {})", snapshot.name, id.0);
    }
    if !any {
        println!("  (none)");
    }

    println!(
        "\nclamshell: {}",
        match panel::lid_is_closed() {
            Some(true) => "closed",
            Some(false) => "open",
            None => "unknown (no built-in panel, or the property is absent)",
        }
    );
}
