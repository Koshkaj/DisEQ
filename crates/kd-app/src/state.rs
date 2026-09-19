use std::collections::HashSet;

use kd_core::{Display, Service};
use kd_sys::display::DisplayId;

/// Which view the panel is showing. Submenus are transitions inside the same
/// panel, never a second window.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Route {
    #[default]
    Root,
    Submenu {
        display: usize,
        submenu: Submenu,
    },
    /// The equaliser's preset list. Not a display submenu — it belongs to the
    /// sound card and has no display to hang off.
    Presets,
    /// Physical devices that can receive the processed system audio.
    Outputs,
    /// Application-level preferences and recovery actions.
    Settings,
}

#[derive(Clone, Debug, Default)]
pub struct ViewState {
    pub route: Route,
    /// Shown in the card when an action was refused, so a toggle that springs
    /// back explains itself instead of looking broken.
    pub notice: Option<String>,
    /// Shown on the sound card, for the same reason.
    pub sound_notice: Option<String>,
    pub settings_notice: Option<String>,
    pub sound_expanded: bool,
    expanded: HashSet<usize>,
}

impl ViewState {
    pub fn is_expanded(&self, display: usize) -> bool {
        self.expanded.contains(&display)
    }

    pub fn toggle_expanded(&mut self, display: usize) {
        if !self.expanded.remove(&display) {
            self.expanded.insert(display);
        }
    }

    pub fn collapse(&mut self, display: usize) {
        self.expanded.remove(&display);
    }

    /// A new main display moves to the first row. Index-based navigation is no
    /// longer valid, so return to a known root state instead of opening another
    /// display's advanced rows or submenu.
    pub fn display_order_changed(&mut self) {
        self.expanded.clear();
        self.route = Route::Root;
    }
}

/// The switches on the sound card. Values are the item half of a sound tag, so
/// they must not collide with the band and fader ranges `views` reserves.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SoundAction {
    /// The EQ bands themselves, bypassable without tearing the route down.
    Equaliser = 2,
    /// Pull the preamp down by the loudest band so boosting cannot clip.
    AutoPreamp = 3,
    /// Tap the applications that are playing and give each one a fader.
    AppMixer = 4,
}

impl SoundAction {
    /// The switches the card lists, in order.
    ///
    /// Auto Preamp is not one of them. It sets the equaliser's preamp, which is
    /// bypassed along with the rest of the equaliser when that is off, so it
    /// lives in the equaliser's own section and shows only while it is on.
    pub const CARD: &[(&'static str, &'static str, SoundAction)] = &[
        ("Equaliser", "slider.vertical.3", SoundAction::Equaliser),
        ("App Mixer", "square.stack.3d.up", SoundAction::AppMixer),
    ];

    pub const AUTO_PREAMP: &'static str = "Auto Preamp";

    pub fn from_index(value: isize) -> Option<Self> {
        [Self::Equaliser, Self::AutoPreamp, Self::AppMixer]
            .into_iter()
            .find(|action| *action as isize == value)
    }
}

/// The per-card toggles that sit below the disclosure rows.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CardAction {
    HiDpi = 1,
    DisplayNotch = 2,
    SetAsMain = 3,
    BrightnessUpscaling = 4,
    AutoBrightness = 5,
    NightShift = 6,
}

impl CardAction {
    pub const ALL: &[(&'static str, &'static str, CardAction)] = &[
        ("High Resolution (HiDPI)", "sparkles", CardAction::HiDpi),
        (
            "Display Notch",
            "rectangle.tophalf.inset.filled",
            CardAction::DisplayNotch,
        ),
        ("Set as Main Display", "m.circle", CardAction::SetAsMain),
        (
            "Brightness Upscaling",
            "sun.max",
            CardAction::BrightnessUpscaling,
        ),
        ("Auto Brightness", "a.circle", CardAction::AutoBrightness),
        (
            "Night Shift (All Displays)",
            "moon.stars",
            CardAction::NightShift,
        ),
    ];

    pub fn from_index(value: isize) -> Option<Self> {
        Self::ALL
            .iter()
            .map(|(_, _, action)| *action)
            .find(|action| *action as isize == value)
    }

    /// Night Shift is global. Showing it once, on the main display, avoids a
    /// row on every monitor that falsely looks independently configurable.
    pub fn is_visible(self, display: &Display) -> bool {
        self != Self::NightShift || display.snapshot.is_main
    }

    /// Current state, and whether the control can do anything at all.
    pub fn state(self, display: &Display, service: &Service) -> (bool, bool) {
        let id = display.id();
        match self {
            CardAction::HiDpi => {
                let on = display
                    .snapshot
                    .current_mode
                    .as_ref()
                    .is_some_and(|mode| mode.is_hidpi());
                (on, service.hidpi_available(id, !on))
            }
            CardAction::SetAsMain => (display.snapshot.is_main, !display.snapshot.is_main),
            CardAction::AutoBrightness => match service.auto_brightness(id) {
                Some(on) => (on, true),
                None => (false, false),
            },
            CardAction::NightShift => match service.night_shift() {
                Some(on) => (on, true),
                None => (false, false),
            },
            // Needs a mode that excludes the menu-bar safe area, which only
            // notched built-in panels publish.
            CardAction::DisplayNotch => (false, false),
            // Would need EDR headroom applied to the whole display, which is
            // only reachable through private CoreDisplay preset APIs.
            CardAction::BrightnessUpscaling => (false, false),
        }
    }
}

/// The disclosure rows from the reference UI, in its order.
///
/// The gaps in the discriminants are rows that were dropped for having no
/// working backend behind them; the remaining values stay put because they are
/// encoded into view tags.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Submenu {
    DisplayMode = 1,
    RefreshRate = 2,
    ColorMode = 3,
    MirrorDisplay = 4,
    MoveDisplay = 7,
    DeviceControl = 10,
    ConfigurationProtection = 12,
    ManageDisplay = 13,
}

/// What activating a submenu row does.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum SubmenuAction {
    SetMode(i32),
    SetMirror(Option<DisplayId>),
    SetMain,
    MoveTo(i32, i32),
    SetInput(u16),
    PanelPower(bool),
    SetColour { red: f64, green: f64, blue: f64 },
    ResetColour,
    SetProtected(bool),
}

pub struct SubmenuRow {
    pub label: String,
    pub detail: String,
    pub action: Option<SubmenuAction>,
    pub selected: bool,
}

impl SubmenuRow {
    fn info(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            action: None,
            selected: false,
        }
    }

    fn action(
        label: impl Into<String>,
        detail: impl Into<String>,
        action: SubmenuAction,
        selected: bool,
    ) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            action: Some(action),
            selected,
        }
    }
}

impl Submenu {
    pub const ALL: &[(&'static str, &'static str, Submenu)] = &[
        (
            "Display Mode",
            "rectangle.on.rectangle",
            Submenu::DisplayMode,
        ),
        ("Refresh Rate", "timer", Submenu::RefreshRate),
        ("Color Mode", "paintpalette", Submenu::ColorMode),
        ("Mirror Display", "rectangle.2.swap", Submenu::MirrorDisplay),
        (
            "Move Display",
            "arrow.up.and.down.and.arrow.left.and.right",
            Submenu::MoveDisplay,
        ),
        ("Device Control", "gearshape", Submenu::DeviceControl),
        (
            "Configuration Protection",
            "lock.shield",
            Submenu::ConfigurationProtection,
        ),
        ("Manage Display", "square.stack", Submenu::ManageDisplay),
    ];

    pub fn from_index(value: isize) -> Option<Self> {
        Self::ALL
            .iter()
            .map(|(_, _, submenu)| *submenu)
            .find(|submenu| *submenu as isize == value)
    }

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, _, submenu)| *submenu == self)
            .map(|(label, _, _)| *label)
            .unwrap_or("")
    }

    /// Rows to show, recomputed identically when a row is activated so the
    /// handler can act on the row index alone.
    pub fn rows(
        self,
        display: &Display,
        catalog: &[Display],
        service: &Service,
    ) -> Vec<SubmenuRow> {
        match self {
            Submenu::DisplayMode => display_mode_rows(display, catalog),
            Submenu::RefreshRate => refresh_rate_rows(display),
            Submenu::ColorMode => colour_rows(display, service),
            Submenu::MirrorDisplay => mirror_rows(display, catalog),
            Submenu::MoveDisplay => move_rows(display),
            Submenu::DeviceControl => device_rows(display),
            Submenu::ManageDisplay => manage_rows(display, service),
            Submenu::ConfigurationProtection => protection_rows(display),
        }
    }
}

fn display_mode_rows(display: &Display, catalog: &[Display]) -> Vec<SubmenuRow> {
    let mirroring = display.snapshot.mirrors.is_some();
    let mut rows = vec![
        SubmenuRow::action(
            "Extended",
            "",
            SubmenuAction::SetMirror(None),
            !mirroring && display.snapshot.is_active,
        ),
        SubmenuRow::action(
            "Set as main display",
            "",
            SubmenuAction::SetMain,
            display.snapshot.is_main,
        ),
    ];
    for other in catalog.iter().filter(|entry| entry.id() != display.id()) {
        rows.push(SubmenuRow::action(
            format!("Mirror of {}", other.name()),
            "",
            SubmenuAction::SetMirror(Some(other.id())),
            display.snapshot.mirrors == Some(other.id()),
        ));
    }
    rows
}

fn refresh_rate_rows(display: &Display) -> Vec<SubmenuRow> {
    let Some(current) = display.snapshot.current_mode.as_ref() else {
        return vec![SubmenuRow::info("No mode", "")];
    };

    let mut seen: Vec<i64> = Vec::new();
    let mut rows = Vec::new();
    for mode in display.modes.iter().filter(|mode| {
        mode.width == current.width
            && mode.height == current.height
            && mode.is_hidpi() == current.is_hidpi()
    }) {
        let key = mode.refresh_rate.round() as i64;
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let label = if mode.refresh_rate > 0.0 {
            format!("{:.0} Hz", mode.refresh_rate)
        } else {
            "Variable".to_string()
        };
        rows.push(SubmenuRow::action(
            label,
            "",
            SubmenuAction::SetMode(mode.io_mode_id),
            (mode.refresh_rate - current.refresh_rate).abs() < 0.5,
        ));
    }
    if rows.is_empty() {
        rows.push(SubmenuRow::info("No alternatives", ""));
    }
    rows
}

fn colour_rows(display: &Display, service: &Service) -> Vec<SubmenuRow> {
    let current = service.colour_adjustment(display.id());
    vec![
        SubmenuRow::action(
            "Neutral",
            "",
            SubmenuAction::SetColour {
                red: 1.0,
                green: 1.0,
                blue: 1.0,
            },
            current.red >= 1.0 && current.blue >= 1.0,
        ),
        SubmenuRow::action(
            "Warm",
            "",
            SubmenuAction::SetColour {
                red: 1.0,
                green: 0.94,
                blue: 0.84,
            },
            current.blue < 0.9 && current.blue >= 0.8,
        ),
        SubmenuRow::action(
            "Warmer",
            "",
            SubmenuAction::SetColour {
                red: 1.0,
                green: 0.86,
                blue: 0.70,
            },
            current.blue < 0.8,
        ),
        SubmenuRow::action(
            "Cool",
            "",
            SubmenuAction::SetColour {
                red: 0.92,
                green: 0.96,
                blue: 1.0,
            },
            current.red < 1.0,
        ),
        SubmenuRow::action("Reset", "", SubmenuAction::ResetColour, false),
    ]
}

fn mirror_rows(display: &Display, catalog: &[Display]) -> Vec<SubmenuRow> {
    let mut rows = vec![SubmenuRow::action(
        "Off",
        "",
        SubmenuAction::SetMirror(None),
        display.snapshot.mirrors.is_none(),
    )];
    for other in catalog.iter().filter(|entry| entry.id() != display.id()) {
        rows.push(SubmenuRow::action(
            other.name().to_string(),
            "",
            SubmenuAction::SetMirror(Some(other.id())),
            display.snapshot.mirrors == Some(other.id()),
        ));
    }
    if rows.len() == 1 {
        rows.push(SubmenuRow::info("No other display", ""));
    }
    rows
}

fn move_rows(display: &Display) -> Vec<SubmenuRow> {
    let (x, y) = display.snapshot.origin;
    let (w, h) = display.snapshot.size;
    let (x, y, w, h) = (x as i32, y as i32, w as i32, h as i32);
    vec![
        SubmenuRow::info("Origin", format!("{x}, {y}")),
        SubmenuRow::action(
            "To origin",
            "0, 0",
            SubmenuAction::MoveTo(0, 0),
            x == 0 && y == 0,
        ),
        SubmenuRow::action("Left", "", SubmenuAction::MoveTo(x - w, y), false),
        SubmenuRow::action("Right", "", SubmenuAction::MoveTo(x + w, y), false),
        SubmenuRow::action("Above", "", SubmenuAction::MoveTo(x, y - h), false),
        SubmenuRow::action("Below", "", SubmenuAction::MoveTo(x, y + h), false),
    ]
}

fn device_rows(display: &Display) -> Vec<SubmenuRow> {
    if display.snapshot.is_builtin {
        return vec![SubmenuRow::info("Not applicable", "Built-in display")];
    }
    vec![
        SubmenuRow::action("Panel on", "", SubmenuAction::PanelPower(true), false),
        SubmenuRow::action("Panel standby", "", SubmenuAction::PanelPower(false), false),
        SubmenuRow::action(
            "Input: DisplayPort",
            "",
            SubmenuAction::SetInput(0x0F),
            false,
        ),
        SubmenuRow::action("Input: HDMI 1", "", SubmenuAction::SetInput(0x11), false),
        SubmenuRow::action("Input: USB-C", "", SubmenuAction::SetInput(0x1B), false),
    ]
}

fn protection_rows(display: &Display) -> Vec<SubmenuRow> {
    let id = display.id();
    let on = kd_core::protection::is_protected(id);
    let mut rows = vec![
        SubmenuRow::action(
            "Protect this layout",
            "",
            SubmenuAction::SetProtected(true),
            on,
        ),
        SubmenuRow::action(
            "Stop protecting",
            "",
            SubmenuAction::SetProtected(false),
            !on,
        ),
    ];
    if let Some(detail) = kd_core::protection::describe(id) {
        rows.push(SubmenuRow::info("Pinned", detail));
    }
    rows
}

fn manage_rows(display: &Display, service: &Service) -> Vec<SubmenuRow> {
    let snapshot = &display.snapshot;
    let mut rows = vec![
        SubmenuRow::info("Display ID", snapshot.id.0.to_string()),
        SubmenuRow::info("Vendor", format!("0x{:04X}", snapshot.vendor)),
        SubmenuRow::info("Model", format!("0x{:04X}", snapshot.model)),
        SubmenuRow::info("Serial", format!("0x{:08X}", snapshot.serial)),
        SubmenuRow::info("Unit", snapshot.unit.to_string()),
        SubmenuRow::info("Brightness backend", service.backend(snapshot.id).caption()),
    ];
    if let Some(confidence) = service.ddc_confidence(snapshot.id) {
        rows.push(SubmenuRow::info("DDC match", format!("{confidence:?}")));
    }
    rows
}
