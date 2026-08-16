//! End-to-end check of the two reconnect guards, driven through the real
//! catalogue and the real refusal path rather than through their internals.
//!
//! Run it against a scratch home directory, because it writes offline records:
//!
//!     HOME=$(mktemp -d) cargo run -p kd-core --example guard_probe
//!
//! What it asserts:
//!
//! 1. A record for a panel that is *not* plugged in produces no card, and the
//!    record is dropped — the state that used to leave a toggle offering to
//!    reconnect a monitor that had been unplugged, and to invent one when
//!    pressed.
//! 2. A record for a panel that *is* plugged in still produces a card, so
//!    switching a display off and back on keeps working.
//! 3. Reconnecting the absent panel is refused rather than attempted.
//! 4. The built-in panel follows the lid.
//! 5. A monitor plugged back into a port that was switched off comes back on
//!    its own. Reproduced rather than described: disabling a display and then
//!    dropping its record leaves the same state a replug does.

use kd_core::offline::{self, Offline};
use kd_core::DisplayCatalog;
use kd_sys::display::DisplayId;
use kd_sys::panel;
use kd_sys::power::{PowerError, Strategy};

/// Polls `ready` until it holds or `limit` runs out.
fn wait_until(limit: std::time::Duration, ready: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if ready() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn main() {
    if std::env::var_os("HOME").is_none_or(|home| home.to_string_lossy().starts_with("/Users/")) {
        eprintln!("refusing to run against a real home directory — see the doc comment");
        std::process::exit(2);
    }

    let mut failures = 0;
    let mut check = |label: &str, ok: bool| {
        println!("  [{}] {label}", if ok { "pass" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    if !panel::detection_is_available() {
        println!("panel detection unavailable on this machine — the guards are inert by design");
        return;
    }

    // --- a panel that is not plugged in ------------------------------------
    println!("\na record for hardware that is not attached:");
    let absent = Offline {
        id: DisplayId(9_999),
        name: "Nonexistent Display".into(),
        is_builtin: false,
        vendor: 0xFFFE,
        model: 0xFFFE,
        serial: 0xFFFE_FFFE,
        unit: 99,
        strategy: Strategy::HardDisconnect,
    };
    offline::insert(absent.clone());
    check(
        "the record is there to begin with",
        offline::is_offline(absent.id),
    );

    let ghost = absent.ghost();
    check(
        "reconnecting it is refused",
        matches!(
            kd_core::power::may_connect(&ghost.snapshot),
            Err(PowerError::NotAttached)
        ),
    );

    let catalog = DisplayCatalog::load();
    check(
        "no card is shown for it",
        !catalog.displays.iter().any(|d| d.id() == absent.id),
    );
    check(
        "the record is dropped rather than kept forever",
        !offline::is_offline(absent.id),
    );

    // --- switching a real display off and back on ---------------------------
    //
    // The regression the absent-panel guard could easily cause: a display that
    // is switched off is also missing from the display list, and if that read
    // as "unplugged" its card would vanish along with the toggle that brings it
    // back. So this is the round trip, through the same service the panel uses.
    println!("\nswitching an attached display off and back on:");
    let service = kd_core::Service::start();
    let target = DisplayCatalog::load()
        .displays
        .into_iter()
        .find(|display| !display.snapshot.is_builtin && !display.snapshot.is_main);

    match target {
        Some(target) => {
            let id = target.id();
            println!("  target: {} (id {})", target.name(), id.0);

            check(
                "switching it off is accepted",
                service.set_connected(&target, false).is_ok(),
            );
            check("it reads as disconnected", !service.is_connected(id));

            let catalog = DisplayCatalog::load();
            let card = catalog.displays.iter().find(|d| d.id() == id);
            check("its card is still shown while it is off", card.is_some());
            check(
                "it is still recognised as plugged in",
                kd_core::power::dark_panels()
                    .iter()
                    .any(|p| p.matches_snapshot(&target.snapshot)),
            );

            // What the panel has to watch to notice the cable coming out while
            // that card is on screen. A display switched off is already gone
            // from the layout, so the layout cannot report the unplug — it has
            // nothing left to lose. Only the panel list still holds it, and so
            // only the panel list moves when the cable does.
            check(
                "the layout has already lost it, so it cannot report the unplug",
                !kd_sys::display::online_displays().contains(&id),
            );
            check(
                "the panel list still holds it, so it is what the unplug moves",
                panel::attached_panels()
                    .iter()
                    .any(|p| p.matches_snapshot(&target.snapshot)),
            );

            match card {
                Some(card) => {
                    check(
                        "reconnecting it is allowed",
                        kd_core::power::may_connect(&card.snapshot).is_ok(),
                    );
                    check(
                        "switching it back on is accepted",
                        service.set_connected(card, true).is_ok(),
                    );
                }
                None => check("reconnecting it is allowed", false),
            }
            check(
                "it is online again",
                kd_sys::display::online_displays().contains(&id),
            );
            check("it reads as connected again", service.is_connected(id));

            // --- plugged back into a port that was switched off --------------
            //
            // Switch a display off, unplug it, plug it back in: it stays dark
            // and never reappears. The window server's disable is attached to
            // the port, not to the panel, so it survives the cable coming out —
            // and the record that would have brought it back was dropped when
            // the panel it described disappeared.
            //
            // Reproduced exactly rather than by hand: disabling the display and
            // then forgetting its record leaves the same state a replug does —
            // panel attached, port disabled, nothing left that remembers why.
            println!("\na monitor plugged back into a port that was switched off:");
            if service.set_connected(&target, false).is_ok() {
                offline::forget(id);

                check(
                    "it is orphaned — attached, dark, and nothing knows why",
                    kd_core::power::orphaned_panels()
                        .iter()
                        .any(|p| p.matches_snapshot(&target.snapshot)),
                );
                check(
                    "no card is shown for it, so nothing is there to press",
                    !DisplayCatalog::load().displays.iter().any(|d| d.id() == id),
                );

                kd_core::power::recover_orphaned_panels_async();
                let recovered = wait_until(std::time::Duration::from_secs(15), || {
                    kd_sys::display::online_displays().contains(&id)
                });
                check("it is brought back automatically", recovered);
                check(
                    "nothing is left plugged in and dark",
                    kd_core::power::orphaned_panels().is_empty(),
                );
            } else {
                check("switching it off a second time is accepted", false);
            }
        }
        None => println!("  (skipped — no secondary external display attached)"),
    }

    // --- the lid ------------------------------------------------------------
    println!("\nthe built-in panel and the lid:");
    let built_in = Offline {
        id: DisplayId(9_997),
        name: "Built-in Display".into(),
        is_builtin: true,
        vendor: 0,
        model: 0,
        serial: 0,
        unit: 97,
        strategy: Strategy::HardDisconnect,
    };
    let allowed = kd_core::power::may_connect(&built_in.ghost().snapshot);
    match panel::lid_is_closed() {
        Some(true) => check(
            "the lid is closed, so it is refused",
            matches!(allowed, Err(PowerError::LidClosed)),
        ),
        Some(false) => check("the lid is open, so it is allowed", allowed.is_ok()),
        None => check("this machine has no lid, so it is allowed", allowed.is_ok()),
    }

    // --- the sweep ----------------------------------------------------------
    println!("\nthe blind sweep:");
    let before: Vec<DisplayId> = kd_sys::display::online_displays();
    let restored = kd_core::power::restore_disabled();
    let after: Vec<DisplayId> = kd_sys::display::online_displays();
    check(
        &format!(
            "it invents nothing when there is nothing to recover \
             (restored {restored}, {} online before, {} after)",
            before.len(),
            after.len()
        ),
        after.len() == before.len() + restored,
    );
    check(
        "no online display lacks hardware behind it",
        after.iter().all(|id| {
            let snapshot = kd_sys::display::snapshot(*id);
            snapshot.is_builtin
                || panel::attached_panels()
                    .iter()
                    .any(|p| p.matches_snapshot(&snapshot))
        }),
    );

    println!();
    if failures == 0 {
        println!("all guards hold");
    } else {
        println!("{failures} check(s) failed");
        std::process::exit(1);
    }
}
