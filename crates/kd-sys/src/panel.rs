//! What is physically plugged in, and whether the lid is shut.
//!
//! CoreGraphics answers neither question. `CGGetOnlineDisplayList` drops a
//! display that has been disabled in exactly the same way it drops one whose
//! cable has been pulled, so on its own it cannot tell "switched off" from "not
//! there" — and the difference decides whether re-enabling that display is a
//! recovery or the invention of a monitor that does not exist.
//!
//! IOKit does answer it. Each panel the machine can actually see has an
//! `AppleCLCD2` node carrying the EDID it read off the wire — manufacturer,
//! product, serial, name. Reading EDID requires a display on the other end, so
//! the node's presence *is* the attachment: it survives the display being
//! disabled, and it goes when the cable does.
//!
//! The lid comes from `AppleClamshellState` on `IOPMrootDomain`, the same
//! property the power manager uses to decide whether the built-in panel may be
//! lit at all.

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};

use crate::display::DisplaySnapshot;
use crate::iokit::{self, PropertyDictionary};

/// The IORegistry class carrying one node per panel the machine can see.
///
/// Apple Silicon. On anything older the class is absent, every query here
/// reports [`Attachment::Unknown`], and callers fall back to their previous
/// behaviour rather than refusing to work.
const PANEL_CLASS: &str = "AppleCLCD2";

/// A panel physically connected to this machine, as its EDID describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedPanel {
    pub name: Option<String>,
    /// The three EDID fields CoreGraphics also reports, so a panel can be lined
    /// up with a `CGDirectDisplayID` — or with a record of one that is no
    /// longer online.
    pub vendor: u32,
    pub model: u32,
    /// Zero when the panel publishes no serial, which some do not.
    pub serial: u32,
}

impl AttachedPanel {
    /// Whether this is the panel those EDID fields describe.
    ///
    /// Serial numbers settle it when both sides have one. Where either does
    /// not, vendor and product stand alone — the same rule the DDC matcher
    /// uses, and the same caveat: two identical monitors are indistinguishable,
    /// which errs towards "attached" and so towards leaving the user's toggle
    /// working.
    pub fn matches(&self, vendor: u32, model: u32, serial: u32) -> bool {
        if self.vendor != vendor || self.model != model {
            return false;
        }
        if self.serial != 0 && serial != 0 {
            return self.serial == serial;
        }
        true
    }

    pub fn matches_snapshot(&self, snapshot: &DisplaySnapshot) -> bool {
        self.matches(snapshot.vendor, snapshot.model, snapshot.serial)
    }
}

/// Whether a panel is on the other end of the cable.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Attachment {
    /// EDID was read from it. It is there, lit or not.
    Attached,
    /// Panels were enumerated and this is not among them.
    Absent,
    /// Nothing could be enumerated, so the question was not answered. Callers
    /// must treat this as permission to proceed: a machine whose registry this
    /// code does not understand has to keep working.
    Unknown,
}

impl Attachment {
    /// True unless we positively know there is nothing there.
    ///
    /// The shape every caller wants: act on `Attached` and on `Unknown`, refuse
    /// only on `Absent`.
    pub fn is_plausible(self) -> bool {
        self != Attachment::Absent
    }
}

/// Every externally connected panel the machine can see.
///
/// The built-in panel is deliberately not among them. Its `ProductAttributes`
/// carry a packed string where an external panel carries a numeric EDID product
/// id, so it has nothing to match a `CGDirectDisplayID` on — and it is soldered
/// in, so the question this module exists to answer never applies to it.
pub fn attached_panels() -> Vec<AttachedPanel> {
    let mut panels = Vec::new();
    iokit::for_each_service(PANEL_CLASS, |service| {
        if let Some(panel) = read_panel(service) {
            panels.push(panel);
        }
        true
    });
    panels
}

/// Whether this machine's registry can be read at all.
///
/// False on hardware where [`PANEL_CLASS`] does not exist, which is what turns
/// every attachment query into [`Attachment::Unknown`].
pub fn detection_is_available() -> bool {
    let mut found = false;
    iokit::for_each_service(PANEL_CLASS, |_| {
        found = true;
        false
    });
    found
}

/// Whether the panel those EDID fields describe is plugged in.
pub fn attachment(vendor: u32, model: u32, serial: u32) -> Attachment {
    // A display with no EDID identity at all — which is what a snapshot of a
    // display that has already gone can amount to — cannot be looked up, so it
    // is not evidence of absence.
    if vendor == 0 && model == 0 {
        return Attachment::Unknown;
    }
    if !detection_is_available() {
        return Attachment::Unknown;
    }
    if attached_panels()
        .iter()
        .any(|panel| panel.matches(vendor, model, serial))
    {
        Attachment::Attached
    } else {
        Attachment::Absent
    }
}

/// Whether the display those EDID fields describe may be brought back.
///
/// The built-in panel is never unplugged, so it is always plausible on this
/// axis — whether it may be *lit* is [`lid_is_closed`]'s question, not this
/// one.
pub fn attachment_for(is_builtin: bool, vendor: u32, model: u32, serial: u32) -> Attachment {
    if is_builtin {
        Attachment::Attached
    } else {
        attachment(vendor, model, serial)
    }
}

/// Whether the lid is shut, where there is a lid to shut.
///
/// `None` on a machine with no clamshell, and on any machine whose power
/// manager does not publish the property.
pub fn lid_is_closed() -> Option<bool> {
    let mut state = None;
    iokit::for_each_service("IOPMrootDomain", |service| {
        state = iokit::property(service, "AppleClamshellState")
            .and_then(|value| value.downcast::<CFBoolean>().ok())
            .map(|value| value.value());
        // One root domain, and it either has the property or does not.
        false
    });
    state
}

fn read_panel(service: iokit::IoService) -> Option<AttachedPanel> {
    let attributes = dictionary(iokit::property(service, "DisplayAttributes")?)?;
    let product = dictionary(attributes.get(&CFString::from_str("ProductAttributes"))?)?;

    // `LegacyManufacturerID` is the numeric EDID vendor code, and the one
    // CoreGraphics reports; `ManufacturerID` next to it is the same thing
    // spelled out and does not compare.
    let vendor = number(&product, "LegacyManufacturerID")?;
    // The built-in panel puts a packed string here, which is how it excludes
    // itself: nothing that wide is an EDID product id.
    let model = number(&product, "ProductID")?;

    Some(AttachedPanel {
        name: string(&product, "ProductName"),
        vendor,
        model,
        serial: number(&product, "SerialNumber").unwrap_or(0),
    })
}

fn dictionary(value: CFRetained<CFType>) -> Option<CFRetained<PropertyDictionary>> {
    let dictionary = value.downcast::<CFDictionary>().ok()?;
    Some(unsafe { CFRetained::cast_unchecked::<PropertyDictionary>(dictionary) })
}

/// A registry integer, only when it is one an EDID field could hold.
fn number(dictionary: &PropertyDictionary, key: &str) -> Option<u32> {
    let value = dictionary.get(&CFString::from_str(key))?;
    let number = value.downcast_ref::<CFNumber>()?;
    u32::try_from(number.as_i64()?).ok()
}

fn string(dictionary: &PropertyDictionary, key: &str) -> Option<String> {
    let value = dictionary.get(&CFString::from_str(key))?;
    Some(value.downcast_ref::<CFString>()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not an assertion about this machine: only that asking does not panic or
    /// hang, whatever it happens to have plugged in.
    #[test]
    fn enumeration_survives_whatever_this_machine_has() {
        for panel in attached_panels() {
            assert!(panel.vendor != 0 || panel.model != 0);
        }
        let _ = lid_is_closed();
    }

    #[test]
    fn a_panel_with_no_identity_is_never_declared_absent() {
        assert_eq!(attachment(0, 0, 0), Attachment::Unknown);
        assert!(attachment(0, 0, 0).is_plausible());
    }

    /// The built-in is soldered in; nothing in the registry has to say so.
    #[test]
    fn the_built_in_panel_is_always_attached() {
        assert_eq!(attachment_for(true, 0, 0, 0), Attachment::Attached);
    }

    #[test]
    fn serials_settle_a_match_and_absent_ones_do_not_break_it() {
        let panel = AttachedPanel {
            name: None,
            vendor: 0x10AC,
            model: 0x41B5,
            serial: 0x4237314C,
        };
        assert!(panel.matches(0x10AC, 0x41B5, 0x4237314C));
        assert!(!panel.matches(0x10AC, 0x41B5, 0xDEADBEEF));
        // A caller that has no serial still matches on vendor and product.
        assert!(panel.matches(0x10AC, 0x41B5, 0));
        assert!(!panel.matches(0x10AC, 0x0001, 0));
    }

    #[test]
    fn only_absence_blocks() {
        assert!(Attachment::Attached.is_plausible());
        assert!(Attachment::Unknown.is_plausible());
        assert!(!Attachment::Absent.is_plausible());
    }
}
