use kd_sys::display::{self, DisplayId, DisplayMode, DisplaySnapshot};

/// A display as the UI needs it: the raw system snapshot plus the derived values
/// the panel renders.
#[derive(Clone, Debug)]
pub struct Display {
    pub snapshot: DisplaySnapshot,
    pub modes: Vec<DisplayMode>,
    pub native: Option<DisplayMode>,
}

impl Display {
    pub fn load(id: DisplayId) -> Self {
        Self {
            snapshot: display::snapshot(id),
            modes: display::modes(id),
            native: display::native_mode(id),
        }
    }

    pub fn id(&self) -> DisplayId {
        self.snapshot.id
    }

    pub fn name(&self) -> &str {
        &self.snapshot.name
    }

    /// Desktop space of the current mode relative to the widest mode available,
    /// as shown in the resolution readout (`1590x1028 · 92%`).
    ///
    /// Measured in points rather than pixels, because that is what determines
    /// how much fits on screen — a HiDPI mode and its 1x twin cover the same
    /// area at very different pixel counts.
    pub fn resolution_scale(&self) -> Option<f64> {
        let current = self.snapshot.current_mode.as_ref()?;
        let widest = self.selectable_modes().last()?.width;
        if widest == 0 {
            return None;
        }
        Some(current.width as f64 / widest as f64)
    }

    /// Modes matching the current aspect ratio, sorted by width — the list the
    /// resolution slider indexes into.
    pub fn selectable_modes(&self) -> Vec<&DisplayMode> {
        let Some(native) = self.native.as_ref() else {
            return Vec::new();
        };
        let target = aspect(native.width, native.height);

        let mut out: Vec<&DisplayMode> = self
            .modes
            .iter()
            .filter(|m| (aspect(m.width, m.height) - target).abs() < 0.01)
            .collect();
        out.sort_by_key(|m| m.width);
        out.dedup_by_key(|m| (m.width, m.height));
        out
    }

    pub fn current_mode_index(&self) -> Option<usize> {
        let current = self.snapshot.current_mode.as_ref()?;
        self.selectable_modes()
            .iter()
            .position(|m| m.width == current.width && m.height == current.height)
    }
}

fn aspect(width: usize, height: usize) -> f64 {
    if height == 0 {
        0.0
    } else {
        width as f64 / height as f64
    }
}

/// The set of displays currently attached, in a stable order: main first,
/// then built-in, then by display id so the panel does not reshuffle between
/// refreshes.
///
/// Displays this app hard-disconnected are included even though the system no
/// longer lists them, from the snapshot taken before they went away — without
/// them the card carrying the toggle that brings them back would vanish too.
#[derive(Clone, Debug, Default)]
pub struct DisplayCatalog {
    pub displays: Vec<Display>,
}

impl DisplayCatalog {
    pub fn load() -> Self {
        let online = display::online_displays();
        let mut displays: Vec<Display> = online.iter().copied().map(Display::load).collect();

        // Ids are reassigned between sessions, so a record written last launch
        // has to be matched to the panel it describes before it can be used.
        for entry in &displays {
            crate::offline::rekey(&entry.snapshot);
        }

        for entry in crate::offline::all() {
            let live = displays.iter().find(|d| d.id() == entry.id);
            match live {
                // Back on its own — replugged, reconnected by something else,
                // or never disabled in the first place.
                Some(display)
                    if display.snapshot.is_active && display.snapshot.mirrors.is_none() =>
                {
                    crate::offline::forget(entry.id);
                }
                // Still in the layout, just mirrored: the card is already there.
                Some(_) => {}
                // Off the device tree. Without a card built from the record,
                // the toggle that brings it back would not exist — but only
                // while there is still something to bring back. A display that
                // was switched off and then unplugged has no hardware behind
                // its record any more, and a card offering to reconnect it
                // offers to invent a monitor.
                None => match kd_sys::panel::attachment_for(
                    entry.is_builtin,
                    entry.vendor,
                    entry.model,
                    entry.serial,
                ) {
                    kd_sys::panel::Attachment::Absent => crate::offline::forget(entry.id),
                    _ => displays.push(entry.ghost()),
                },
            }
        }

        displays.sort_by_key(|d| (!d.snapshot.is_main, !d.snapshot.is_builtin, d.snapshot.id.0));
        Self { displays }
    }

    pub fn get(&self, id: DisplayId) -> Option<&Display> {
        self.displays.iter().find(|d| d.id() == id)
    }
}
