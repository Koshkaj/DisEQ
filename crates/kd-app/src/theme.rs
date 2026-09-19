use objc2_foundation::NSEdgeInsets;

pub const PANEL_WIDTH: f64 = 400.0;
pub const PANEL_MARGIN: f64 = 6.0;
pub const PANEL_CORNER_RADIUS: f64 = 16.0;

pub const CARD_CORNER_RADIUS: f64 = 10.0;
pub const ROW_CORNER_RADIUS: f64 = 6.0;
pub const ROW_PADDING: f64 = 4.0;
pub const CARD_SPACING: f64 = 8.0;
pub const ROW_SPACING: f64 = 6.0;
/// Fixed column for leading SF Symbols. Their intrinsic widths vary, so rows
/// otherwise start their text at visibly different horizontal positions.
pub const ROW_ICON_WIDTH: f64 = 20.0;

pub const LABEL_SIZE: f64 = 13.0;
pub const CAPTION_SIZE: f64 = 11.0;

pub const DISABLED_ALPHA: f64 = 0.4;

/// Width reserved for a band's frequency, wide enough for "16kHz" at caption
/// size. Fixed rather than fitted so every band's slider starts at the same x.
pub const BAND_LABEL_WIDTH: f64 = 44.0;

/// Width reserved for an app's name in the mixer. Longer than a band's because
/// app names are words, not frequencies; anything longer truncates rather than
/// stealing the fader's travel.
pub const APP_LABEL_WIDTH: f64 = 88.0;

/// Width reserved for a readout. Wide enough for "-24.0" and "100%" both, so
/// the number never pushes the slider or wraps onto a second line.
pub const READOUT_WIDTH: f64 = 42.0;

/// Width of one band's column. A tenth of the card's inner width, near enough,
/// and wide enough for "125" and "-12" at caption size.
pub const BAND_COLUMN_WIDTH: f64 = 32.0;

/// Travel of an equaliser fader. Tall enough that ±24 dB is a usable gesture,
/// short enough that the bank plus its labels still fits the panel.
pub const FADER_HEIGHT: f64 = 110.0;

/// Vertical gap between the ten band rows. Tighter than `ROW_SPACING`: they
/// read as one control, not ten.
pub const BAND_SPACING: f64 = 2.0;

pub fn panel_insets() -> NSEdgeInsets {
    NSEdgeInsets {
        top: 10.0,
        left: 10.0,
        bottom: 10.0,
        right: 10.0,
    }
}

pub fn no_insets() -> NSEdgeInsets {
    NSEdgeInsets {
        top: 0.0,
        left: 0.0,
        bottom: 0.0,
        right: 0.0,
    }
}

/// The width a row inside a card has to work with.
pub fn card_content_width() -> f64 {
    let (panel, card) = (panel_insets(), card_insets());
    PANEL_WIDTH - panel.left - panel.right - card.left - card.right
}

pub fn card_insets() -> NSEdgeInsets {
    NSEdgeInsets {
        top: 10.0,
        left: 12.0,
        bottom: 10.0,
        right: 12.0,
    }
}
