//! Tests whether the mirror-based soft disable (plan.md §3, strategy A) is
//! usable on this machine.
//!
//! FreeDisplay's BLOCKING.md B-004 reports that
//! `CGConfigureDisplayMirrorOfDisplay` on Apple Silicon triggers the system
//! hardware-mirror path, complete with the "Screen Mirroring" UI and cursor
//! stutter across displays, and they abandoned it. That was in a HiDPI context;
//! whether it also spoils the disable use case has to be measured here.
//!
//! Safety rules, enforced below rather than left to the operator:
//!   * at least two displays must be online — mirroring needs a source and a
//!     target, and with one display there is nothing to fall back to;
//!   * the main display is never the target, so the menu bar and Dock stay put;
//!   * the mirror is applied with session scope and reverted immediately, so
//!     nothing survives a crash of this probe;
//!   * it refuses to run without KD_MIRROR_PROBE=1, so it cannot go off by
//!     accident.
//!
//! Run: KD_MIRROR_PROBE=1 cargo run -p kd-core --example mirror_probe

use std::thread::sleep;
use std::time::{Duration, Instant};

use kd_sys::config::{self, Scope};
use kd_sys::display::{self, DisplayId};

fn main() {
    if std::env::var("KD_MIRROR_PROBE").as_deref() != Ok("1") {
        eprintln!("refusing to run without KD_MIRROR_PROBE=1");
        eprintln!("this probe briefly mirrors a display; read the header first");
        std::process::exit(1);
    }

    let displays = display::online_displays();
    if displays.len() < 2 {
        eprintln!("need at least 2 online displays, found {}", displays.len());
        eprintln!("mirroring the only display would leave no working screen");
        std::process::exit(1);
    }

    let main = display::main_display();
    let Some(target) = pick_target(&displays, main) else {
        eprintln!("no non-main display available as a target");
        std::process::exit(1);
    };

    let before = display::snapshot(target);
    println!(
        "target: {} (id {})   source: main (id {})",
        before.name, target.0, main.0
    );
    println!("mirroring for 3s, then reverting\n");

    let started = Instant::now();
    if !config::set_mirror(target, main, Scope::Session) {
        eprintln!("set_mirror failed or timed out");
        std::process::exit(1);
    }
    println!("  set_mirror took {:?}", started.elapsed());

    sleep(Duration::from_secs(3));

    let during = display::snapshot(target);
    println!(
        "  during: active={} mirrors={:?} bounds={:?}",
        during.is_active,
        during.mirrors.map(|m| m.0),
        during.size
    );

    let reverting = Instant::now();
    let reverted = config::clear_mirror(target, Scope::Session);
    println!(
        "  clear_mirror took {:?} -> {}",
        reverting.elapsed(),
        reverted
    );

    let after = display::snapshot(target);
    println!(
        "  after:  active={} mirrors={:?} bounds={:?}",
        after.is_active,
        after.mirrors.map(|m| m.0),
        after.size
    );

    println!("\nlayout restored: {}", after.size == before.size);
    println!("Judge by eye: did the Screen Mirroring UI appear, and did the");
    println!("cursor stutter moving between displays? If so, strategy A is out.");
}

fn pick_target(displays: &[DisplayId], main: DisplayId) -> Option<DisplayId> {
    displays.iter().copied().find(|id| *id != main)
}
