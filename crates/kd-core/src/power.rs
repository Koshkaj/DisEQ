//! When a display may be brought back, and what to do when bringing it back
//! produces something that is not there.
//!
//! `kd_sys::power` is the mechanism: it enables and disables displays and asks
//! no questions. This is the policy on top, and it exists because
//! `CGSConfigureDisplayEnabled` does not fail on a port with nothing plugged
//! into it. It succeeds, and the window server invents a display — one that
//! shows up in the layout, takes windows, and leaves the port in a state a real
//! monitor plugged in afterwards comes up "No Signal" on.
//!
//! Two rules, both of which need a source of truth CoreGraphics does not have:
//!
//! 1. **Only reconnect hardware that is there.** A disabled display and an
//!    unplugged one are indistinguishable in `CGGetOnlineDisplayList`;
//!    [`kd_sys::panel`] reads EDID out of the IORegistry, which only a panel on
//!    the other end of the cable can produce.
//! 2. **Never light the built-in panel with the lid shut.** The private call
//!    sits below whatever enforces that normally, so it has to be enforced
//!    here.
//!
//! Both rules degrade to "allow" where the machine cannot answer, so hardware
//! this code does not recognise keeps working exactly as it did.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kd_sys::display::{self, DisplayId, DisplaySnapshot};
use kd_sys::panel::{self, AttachedPanel, Attachment};
use kd_sys::power::{self as sys, PowerError, Strategy};

/// How long to give the window server to publish a display after a
/// configuration transaction has completed. The transaction returning is not
/// the same as the display being in `CGGetOnlineDisplayList`.
const SETTLE: Duration = Duration::from_millis(1_500);
const SETTLE_STEP: Duration = Duration::from_millis(50);

/// Panels that are plugged in but have no display in the desktop layout.
///
/// The set this module is trying to empty: everything in it is real hardware
/// sitting dark, and nothing else is worth turning a port on for.
pub fn dark_panels() -> Vec<AttachedPanel> {
    let online = online_snapshots();
    panel::attached_panels()
        .into_iter()
        .filter(|attached| {
            !online
                .iter()
                .any(|snapshot| attached.matches_snapshot(snapshot))
        })
        .collect()
}

/// Panels that are plugged in and dark for no reason this app knows of.
///
/// A display switched off from the panel keeps a record and stays off — that is
/// the user's choice, and undoing it behind their back would be worse than the
/// bug. A dark panel with *no* record is something else: hardware the window
/// server is refusing to light.
///
/// That state is reachable, and it is this app that gets the user into it.
/// Switch a display off and the window server disables it; unplug it and the
/// disable stays behind, attached to a port rather than to the panel that was
/// on it. Plug the monitor back in and nothing happens at all — it is attached,
/// it publishes EDID, and it stays dark, because as far as the window server is
/// concerned that port is off. Its record is gone by then, dropped when the
/// panel it described disappeared, so nothing is left to press.
pub fn orphaned_panels() -> Vec<AttachedPanel> {
    let recorded = crate::offline::all();
    dark_panels()
        .into_iter()
        .filter(|attached| {
            !recorded
                .iter()
                .any(|entry| attached.matches(entry.vendor, entry.model, entry.serial))
        })
        .collect()
}

fn key(panel: &AttachedPanel) -> (u32, u32, u32) {
    (panel.vendor, panel.model, panel.serial)
}

/// Panels a recovery pass has already been spent on, so a port that refuses to
/// come back is not retried every few seconds for the rest of the session. An
/// entry is dropped as soon as its panel stops being orphaned — including when
/// it is unplugged, so the next replug is a fresh attempt.
fn attempted() -> &'static Mutex<HashSet<(u32, u32, u32)>> {
    static ATTEMPTED: std::sync::OnceLock<Mutex<HashSet<(u32, u32, u32)>>> =
        std::sync::OnceLock::new();
    ATTEMPTED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Starts a recovery pass for orphaned panels, if there is one to make.
///
/// Runs on a worker thread: the transactions block for as long as the window
/// server takes to answer, and every caller is either the UI or a timer on it.
/// Returns whether a pass was started.
pub fn recover_orphaned_panels_async() -> bool {
    static RUNNING: AtomicBool = AtomicBool::new(false);

    let orphans = orphaned_panels();
    let present: HashSet<(u32, u32, u32)> = orphans.iter().map(key).collect();

    let fresh = {
        let Ok(mut attempted) = attempted().lock() else {
            return false;
        };
        // A panel that is no longer orphaned — lit, or unplugged — is no longer
        // something we have tried and failed at.
        attempted.retain(|entry| present.contains(entry));
        let fresh: Vec<_> = present.difference(&attempted).copied().collect();
        attempted.extend(fresh.iter().copied());
        fresh
    };

    if fresh.is_empty() {
        return false;
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }

    std::thread::spawn(move || {
        restore_disabled();
        RUNNING.store(false, Ordering::SeqCst);
    });
    true
}

/// Whether re-enabling this display could produce anything real.
///
/// Checked before the call rather than after, because the after is the damage.
pub fn may_connect(snapshot: &DisplaySnapshot) -> Result<(), PowerError> {
    if snapshot.is_builtin {
        // The panel is soldered in, so it is always attached. Whether it may be
        // lit is the lid's business.
        return if panel::lid_is_closed() == Some(true) {
            Err(PowerError::LidClosed)
        } else {
            Ok(())
        };
    }

    match panel::attachment(snapshot.vendor, snapshot.model, snapshot.serial) {
        Attachment::Absent => Err(PowerError::NotAttached),
        Attachment::Attached | Attachment::Unknown => Ok(()),
    }
}

/// Whether a display that has just come online is one that actually exists.
///
/// The check that catches what [`may_connect`] cannot: a stale id carries no
/// identity to test beforehand, so the test is what appeared afterwards.
fn is_real(snapshot: &DisplaySnapshot) -> bool {
    if snapshot.is_builtin {
        return panel::lid_is_closed() != Some(true);
    }
    if !panel::detection_is_available() {
        // Nothing to check it against. A machine whose registry this code does
        // not understand keeps its old behaviour.
        return true;
    }
    panel::attached_panels()
        .iter()
        .any(|attached| attached.matches_snapshot(snapshot))
}

/// Re-enables displays the window server still lists but the system has
/// dropped, keeping only the ones that turn out to be real.
///
/// Returns how many came back.
///
/// Ids are enabled one at a time and each is judged on what it produced: a
/// display matching a panel that is plugged in stays, anything else is put
/// straight back. That is what makes this safe to run blind — which it has to
/// be, because a record written in an earlier session carries that session's
/// ids and the window server hands out new ones on every boot.
pub fn restore_disabled() -> usize {
    // Cheap way out of the common case: everything plugged in is already lit,
    // and the built-in is either lit too or shut under its lid. Worth taking,
    // because the alternative is an enable/disable cycle the user sees as a
    // flicker across every screen.
    let lid_shut = panel::lid_is_closed() == Some(true);
    if dark_panels().is_empty() && (built_in_is_online() || lid_shut) {
        return 0;
    }

    let mut restored = 0;
    for candidate in sys::known_display_ids() {
        if display::online_displays().contains(&candidate) {
            continue;
        }
        // Judged before the call where the id still says enough to judge it.
        // The check after the fact catches this too, but only by lighting the
        // panel and putting it out again — which on a shut laptop is a flash of
        // a screen nobody can see and a wake the machine did not ask for.
        if lid_shut && display::snapshot(candidate).is_builtin {
            continue;
        }
        if sys::connect(candidate, Strategy::HardDisconnect).is_err() {
            continue;
        }

        let appeared = settle_for(candidate);
        let real = appeared
            .as_ref()
            .is_some_and(|snapshot| snapshot.is_online && is_real(snapshot));
        if real {
            restored += 1;
        } else {
            // It lit nothing, or lit something with no hardware behind it. Put
            // the port back the way it was rather than leave a display the user
            // has to work out how to get rid of.
            let _ = sys::disconnect(candidate, Strategy::HardDisconnect);
        }
    }
    restored
}

/// Waits for `display` to appear, and reports what appeared.
fn settle_for(display: DisplayId) -> Option<DisplaySnapshot> {
    let until = Instant::now() + SETTLE;
    loop {
        if display::online_displays().contains(&display) {
            return Some(display::snapshot(display));
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(SETTLE_STEP);
    }
}

fn built_in_is_online() -> bool {
    online_snapshots()
        .iter()
        .any(|snapshot| snapshot.is_builtin)
}

fn online_snapshots() -> Vec<DisplaySnapshot> {
    display::online_displays()
        .into_iter()
        .map(display::snapshot)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(is_builtin: bool, vendor: u32, model: u32, serial: u32) -> DisplaySnapshot {
        DisplaySnapshot {
            id: DisplayId(1),
            name: "test".into(),
            is_builtin,
            is_main: false,
            is_active: false,
            is_online: false,
            vendor,
            model,
            serial,
            unit: 0,
            rotation: 0.0,
            origin: (0.0, 0.0),
            size: (0.0, 0.0),
            mirrors: None,
            current_mode: None,
        }
    }

    /// The rule that fixes lighting the panel on a shut laptop. Only assertable
    /// in one direction without a lid to close: with the lid open it must not
    /// refuse.
    #[test]
    fn the_built_in_panel_follows_the_lid() {
        let built_in = snapshot(true, 0, 0, 0);
        match panel::lid_is_closed() {
            Some(true) => assert!(matches!(may_connect(&built_in), Err(PowerError::LidClosed))),
            _ => assert!(may_connect(&built_in).is_ok()),
        }
    }

    /// A display with no identity cannot be looked up, and an unanswerable
    /// question must not read as "not there" — that would refuse a reconnect on
    /// every machine this code cannot enumerate.
    #[test]
    fn an_unidentifiable_display_is_not_refused() {
        assert!(may_connect(&snapshot(false, 0, 0, 0)).is_ok());
    }

    /// A vendor and product that belong to nothing plugged in is the case the
    /// whole module exists for.
    #[test]
    fn a_display_that_is_not_plugged_in_is_refused() {
        if !panel::detection_is_available() {
            return;
        }
        let invented = snapshot(false, 0xFFFE, 0xFFFE, 0xFFFF_FFFE);
        assert!(matches!(
            may_connect(&invented),
            Err(PowerError::NotAttached)
        ));
    }

    /// Every panel the machine can see is, by definition, one a reconnect may
    /// target.
    #[test]
    fn every_attached_panel_is_allowed() {
        for attached in panel::attached_panels() {
            let candidate = snapshot(false, attached.vendor, attached.model, attached.serial);
            assert!(
                may_connect(&candidate).is_ok(),
                "{:?} is plugged in but was refused",
                attached.name
            );
            assert!(is_real(&candidate));
        }
    }

    /// Read-only, and true whatever this machine has attached.
    #[test]
    fn dark_panels_are_a_subset_of_attached_ones() {
        let attached = panel::attached_panels();
        for dark in dark_panels() {
            assert!(attached.contains(&dark));
        }
    }
}
