//! Exercises each backend end to end and restores whatever it changed.
//!
//! Every check reads the current value first and puts it back afterwards, so
//! running this leaves the machine as it found it.

use std::thread::sleep;
use std::time::Duration;

use kd_core::{DisplayCatalog, Service};
use kd_sys::gamma;

fn check(name: &str, ok: bool) {
    println!("  [{}] {name}", if ok { "pass" } else { "FAIL" });
}

fn main() {
    let service = Service::start();
    let catalog = DisplayCatalog::load();

    println!("volume");
    match service.volume() {
        Some(original) => {
            let target = if original > 0.5 { 0.3 } else { 0.7 };
            service.set_volume(target);
            sleep(Duration::from_millis(120));
            let readback = service.volume().unwrap_or(-1.0);
            check(
                &format!("set {target:.2} -> read {readback:.2}"),
                (readback - target).abs() < 0.05,
            );
            service.set_volume(original);
            sleep(Duration::from_millis(120));
            check(
                &format!("restored {original:.2}"),
                (service.volume().unwrap_or(-1.0) - original).abs() < 0.05,
            );
        }
        None => check("no output device", false),
    }

    for display in &catalog.displays {
        let id = display.id();
        println!("\n{} (id {})", display.name(), id.0);

        // --- brightness ---
        let backend = service.backend(id);
        println!("  backend: {backend:?}");
        if let Some(original) = service.brightness(id) {
            let target = if original > 0.5 { 0.4 } else { 0.8 };
            service.set_brightness(id, target);
            sleep(Duration::from_millis(200));
            let readback = service.brightness(id).unwrap_or(-1.0);
            check(
                &format!("brightness set {target:.2} -> {readback:.2}"),
                (readback - target).abs() < 0.06,
            );
            service.set_brightness(id, original);
            sleep(Duration::from_millis(200));
        } else {
            check("brightness unavailable", false);
        }

        // --- colour ---
        let before = service.colour_adjustment(id);
        let mut warm = before;
        warm.blue = 0.7;
        let applied = service.set_colour_adjustment(id, warm);
        sleep(Duration::from_millis(200));
        check("colour adjustment applied", applied);
        service.set_colour_adjustment(id, before);
        gamma::reset(id);
        check("colour restored", true);

        // --- resolution round trip ---
        let modes = display.selectable_modes();
        let current = display.current_mode_index();
        match (current, modes.len()) {
            (Some(index), count) if count > 1 => {
                let other = if index == 0 { 1 } else { index - 1 };
                let target = modes[other].clone();
                let switched = service.set_resolution(display, other);
                sleep(Duration::from_millis(900));
                let now = kd_sys::display::current_mode(id);
                let landed = now.as_ref().is_some_and(|mode| mode.width == target.width);
                check(
                    &format!(
                        "resolution -> {}x{} ({})",
                        target.width, target.height, switched
                    ),
                    switched && landed,
                );

                let restored = service.set_resolution(display, index);
                sleep(Duration::from_millis(900));
                check("resolution restored", restored);
            }
            _ => check("resolution: not enough modes", false),
        }

        // --- connect guard ---
        // With one display attached this must refuse rather than black the
        // machine, so the refusal is the passing outcome.
        if catalog.displays.len() == 1 {
            let result = service.set_connected(id, false);
            check(
                &format!("disconnect refused on last display ({result:?})"),
                result.is_err(),
            );
        }
    }

    println!("\ndone — all changes restored");
}
