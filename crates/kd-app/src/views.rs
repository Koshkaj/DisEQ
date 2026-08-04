use kd_audio::eq;
use kd_core::{Display, DisplayCatalog, Service};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::runtime::Sel;
use objc2::sel;
use objc2::Message;
use objc2_app_kit::{
    NSControlStateValueOff, NSControlStateValueOn, NSLayoutAttribute, NSSlider, NSStackView,
    NSSwitch, NSTextAlignment, NSView,
};
use objc2_foundation::MainThreadMarker;

use crate::appkit;
use crate::sound::SoundService;
use crate::state::{CardAction, Route, SoundAction, Submenu, ViewState};
use crate::theme;

/// Tags carry a row's identity to the delegate's action methods, so no row
/// needs its own controller object: `display index * STRIDE + item`.
const TAG_STRIDE: isize = 100;

pub fn tag(display_index: usize, item: isize) -> isize {
    display_index as isize * TAG_STRIDE + item
}

pub fn decode_tag(tag: isize) -> (usize, isize) {
    ((tag / TAG_STRIDE) as usize, tag % TAG_STRIDE)
}

#[allow(clippy::too_many_arguments)]
pub fn root(
    mtm: MainThreadMarker,
    catalog: &DisplayCatalog,
    service: &Service,
    sound: &SoundService,
    state: &ViewState,
    target: &AnyObject,
) -> Retained<NSStackView> {
    let mut cards: Vec<Retained<NSView>> = Vec::new();

    match &state.route {
        Route::Submenu { display, submenu } => {
            if let Some(entry) = catalog.displays.get(*display) {
                cards.push(submenu_card(
                    mtm, entry, *display, *submenu, catalog, service, target,
                ));
            }
        }
        Route::Presets => cards.push(presets_card(mtm, sound, target)),
        Route::Outputs => cards.push(outputs_card(mtm, sound, target)),
        Route::Settings => cards.push(settings_card(mtm, state, target)),
        Route::Root => {
            for (index, display) in catalog.displays.iter().enumerate() {
                cards.push(display_card(
                    mtm, display, index, service, sound, state, target,
                ));
            }
            cards.push(sound_card(mtm, sound, state, target));
            cards.push(footer_row(mtm, target));
        }
    }

    let refs: Vec<&NSView> = cards.iter().map(|c| c.as_ref()).collect();
    appkit::vstack_filling(mtm, theme::CARD_SPACING, theme::panel_insets(), &refs)
}

// --- display card -----------------------------------------------------------

fn display_card(
    mtm: MainThreadMarker,
    display: &Display,
    index: usize,
    service: &Service,
    sound: &SoundService,
    state: &ViewState,
    target: &AnyObject,
) -> Retained<NSView> {
    let mut rows: Vec<Retained<NSView>> = vec![header_row(mtm, display, index, service, target)];

    // An offline display is only an identity plus the control that can bring
    // it back. With no live display behind it there is nothing meaningful to
    // disclose, and wrapping the card in a click surface would make the empty
    // card look interactive.
    if !service.is_connected(display.id()) {
        return card_from_rows(mtm, &rows);
    }

    // Closed cards are deliberately only their identity and power switch. The
    // name section above is the primary disclosure control.
    if !sound.display_card_is_open(display) {
        let card = card_from_rows(mtm, &rows);
        return appkit::clickable_surface(
            mtm,
            &card,
            tag(index, 0),
            target,
            sel!(toggleDisplayCard:),
            true,
        );
    }

    let brightness = brightness_row(mtm, display, index, service, target);
    let resolution = resolution_row(mtm, display, index, target);

    rows.push(brightness);
    rows.push(resolution);

    if state.is_expanded(index) {
        for (label, symbol, submenu) in Submenu::ALL {
            rows.push(disclosure_row(
                mtm,
                label,
                symbol,
                tag(index, *submenu as isize),
                target,
            ));
        }
        for (label, symbol, action) in CardAction::ALL {
            if !action.is_visible(display) {
                continue;
            }
            let (on, available) = action.state(display, service);
            rows.push(action_row(
                mtm,
                label,
                symbol,
                on,
                available,
                tag(index, *action as isize),
                target,
            ));
        }
    }

    if let Some(notice) = state.notice.as_deref() {
        let text = appkit::caption(mtm, notice);
        let spacer = appkit::spacer(mtm);
        rows.push(Retained::into_super(appkit::hstack(
            mtm,
            theme::ROW_SPACING,
            &[&text, &spacer],
        )));
    }

    rows.push(caret_row(
        mtm,
        state.is_expanded(index),
        tag(index, 0),
        target,
    ));
    let card = card_from_rows(mtm, &rows);
    appkit::clickable_surface(
        mtm,
        &card,
        tag(index, 0),
        target,
        sel!(toggleDisplayCard:),
        false,
    )
}

fn header_row(
    mtm: MainThreadMarker,
    display: &Display,
    index: usize,
    service: &Service,
    target: &AnyObject,
) -> Retained<NSView> {
    let symbol = if display.snapshot.is_builtin {
        "laptopcomputer"
    } else {
        "display"
    };
    let icon = appkit::row_icon(mtm, symbol);
    let name = appkit::title(mtm, display.name());
    // A compact filled dot marks the primary display without competing with
    // the display-type icon or the power switch.
    let main = display
        .snapshot
        .is_main
        .then(|| appkit::indicator_dot(mtm, 6.0, "Main Display"));
    let spacer = appkit::spacer(mtm);

    let toggle = NSSwitch::new(mtm);
    toggle.setState(if service.is_connected(display.id()) {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    toggle.setTag(tag(index, 0));
    unsafe {
        toggle.setTarget(Some(target));
        toggle.setAction(Some(sel!(connectToggled:)));
    }

    let row = if let Some(main) = main.as_deref() {
        appkit::hstack(
            mtm,
            theme::ROW_SPACING,
            &[&icon, &name, main, &spacer, &toggle],
        )
    } else {
        appkit::hstack(mtm, theme::ROW_SPACING, &[&icon, &name, &spacer, &toggle])
    };
    Retained::into_super(row)
}

fn brightness_row(
    mtm: MainThreadMarker,
    display: &Display,
    index: usize,
    service: &Service,
    target: &AnyObject,
) -> Retained<NSView> {
    let backend = service.backend(display.id());
    let value = service.brightness(display.id());
    let readout = match value {
        Some(value) => format!("{:.0}%", value * 100.0),
        None => "—".to_string(),
    };

    slider_row(
        mtm,
        backend.caption(),
        &readout,
        value.unwrap_or(0.0),
        0.0,
        1.0,
        backend.is_available(),
        tag(index, 0),
        target,
        sel!(brightnessChanged:),
        true,
    )
}

fn resolution_row(
    mtm: MainThreadMarker,
    display: &Display,
    index: usize,
    target: &AnyObject,
) -> Retained<NSView> {
    let modes = display.selectable_modes();
    let Some(mode) = display.snapshot.current_mode.as_ref() else {
        return slider_row(
            mtm,
            "Resolution",
            "—",
            0.0,
            0.0,
            1.0,
            false,
            0,
            target,
            sel!(resolutionPreview:),
            false,
        );
    };

    let caption = if mode.is_hidpi() {
        "Resolution (HiDPI)"
    } else {
        "Resolution"
    };
    let readout = match display.resolution_scale() {
        Some(scale) => format!("{}x{} · {:.0}%", mode.width, mode.height, scale * 100.0),
        None => format!("{}x{}", mode.width, mode.height),
    };

    let last = modes.len().saturating_sub(1);
    let position = display.current_mode_index().unwrap_or(0) as f64;
    let row = slider_row(
        mtm,
        caption,
        &readout,
        position,
        0.0,
        last.max(1) as f64,
        modes.len() > 1,
        tag(index, 0),
        target,
        sel!(resolutionPreview:),
        // Preview continuously, but use a deferred slider so WindowServer is
        // reconfigured only once after mouse-up.
        false,
    );
    if let Some(slider) = find_slider(&row) {
        slider.setNumberOfTickMarks(modes.len() as isize);
        slider.setAllowsTickMarkValuesOnly(true);
    }
    row
}

// --- row kinds --------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn slider_row(
    mtm: MainThreadMarker,
    caption: &str,
    value: &str,
    position: f64,
    min: f64,
    max: f64,
    enabled: bool,
    tag_value: isize,
    target: &AnyObject,
    action: Sel,
    continuous: bool,
) -> Retained<NSView> {
    let caption_label = appkit::caption(mtm, caption);
    let spacer = appkit::spacer(mtm);
    let value_label = appkit::caption(mtm, value);
    let top = appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&caption_label, &spacer, &value_label],
    );

    let slider = if continuous {
        NSSlider::new(mtm)
    } else {
        appkit::deferred_slider(mtm)
    };
    slider.setMinValue(min);
    slider.setMaxValue(max);
    slider.setDoubleValue(position);
    slider.setEnabled(enabled);
    // Deferred sliders also send continuously; their subclass adds a distinct
    // commit action after tracking finishes.
    slider.setContinuous(true);
    slider.setTag(tag_value);
    unsafe {
        slider.setTarget(Some(target));
        slider.setAction(Some(action));
    }

    Retained::into_super(appkit::vstack_filling(
        mtm,
        2.0,
        theme::no_insets(),
        &[&top, &slider],
    ))
}

fn find_slider(row: &NSView) -> Option<Retained<NSSlider>> {
    for subview in row.subviews().iter() {
        if let Some(slider) = subview.downcast_ref::<NSSlider>() {
            return Some(slider.retain());
        }
        if let Some(found) = find_slider(&subview) {
            return Some(found);
        }
    }
    None
}

fn disclosure_row(
    mtm: MainThreadMarker,
    label: &str,
    symbol: &str,
    tag: isize,
    target: &AnyObject,
) -> Retained<NSView> {
    let icon = appkit::row_icon(mtm, symbol);
    let text = appkit::label(mtm, label);
    let spacer = appkit::spacer(mtm);
    let chevron = appkit::symbol_view(mtm, "chevron.right");
    let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&icon, &text, &spacer, &chevron]);

    appkit::clickable_row(mtm, &content, tag, target, sel!(openSubmenu:))
}

#[allow(clippy::too_many_arguments)]
fn action_row(
    mtm: MainThreadMarker,
    label: &str,
    symbol: &str,
    on: bool,
    available: bool,
    tag: isize,
    target: &AnyObject,
) -> Retained<NSView> {
    let icon = appkit::row_icon(mtm, symbol);
    let text = appkit::label(mtm, label);
    let spacer = appkit::spacer(mtm);
    let state = appkit::caption(mtm, if on { "On" } else { "Off" });
    let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&icon, &text, &spacer, &state]);

    let row = appkit::clickable_row(mtm, &content, tag, target, sel!(cardAction:));
    if !available {
        appkit::set_enabled(&row, false);
    }
    row
}

fn caret_row(
    mtm: MainThreadMarker,
    expanded: bool,
    tag: isize,
    target: &AnyObject,
) -> Retained<NSView> {
    let symbol = if expanded {
        "chevron.up"
    } else {
        "chevron.down"
    };
    // Pinned to the row's centre. The row stays clickable across its whole
    // width; the caret just stops drifting with whatever the card happens to
    // contain.
    let caret = appkit::symbol_view(mtm, symbol);
    let content = appkit::centered(mtm, &caret);

    appkit::clickable_row(mtm, &content, tag, target, sel!(toggleCard:))
}

// --- submenus ---------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn submenu_card(
    mtm: MainThreadMarker,
    display: &Display,
    index: usize,
    submenu: Submenu,
    catalog: &DisplayCatalog,
    service: &Service,
    target: &AnyObject,
) -> Retained<NSView> {
    let mut rows: Vec<Retained<NSView>> = vec![back_row(mtm, submenu.label(), target)];

    for (row_index, row) in submenu
        .rows(display, &catalog.displays, service)
        .into_iter()
        .enumerate()
    {
        let text = appkit::label(mtm, &row.label);
        let spacer = appkit::spacer(mtm);
        let detail = appkit::caption(
            mtm,
            if row.selected && row.detail.is_empty() {
                "Current"
            } else {
                &row.detail
            },
        );
        let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&text, &spacer, &detail]);

        if row.action.is_some() {
            rows.push(appkit::clickable_row(
                mtm,
                &content,
                tag(index, row_index as isize),
                target,
                sel!(submenuRow:),
            ));
        } else {
            rows.push(Retained::into_super(content));
        }
    }

    card_from_rows(mtm, &rows)
}

fn back_row(mtm: MainThreadMarker, title: &str, target: &AnyObject) -> Retained<NSView> {
    let text = appkit::title(mtm, title);
    let spacer = appkit::spacer(mtm);
    // Every chevron in the panel sits on the right edge, including this one:
    // an arrow that moves side to side as you step in and out of submenus
    // reads as the layout shifting rather than as navigation.
    let chevron = appkit::symbol_view(mtm, "chevron.left");
    let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&text, &spacer, &chevron]);

    appkit::clickable_row(mtm, &content, 0, target, sel!(goBack:))
}

// --- settings, footer -------------------------------------------------------

fn settings_card(mtm: MainThreadMarker, state: &ViewState, target: &AnyObject) -> Retained<NSView> {
    use kd_sys::login_item::Status;

    let status = kd_sys::login_item::status();
    let mut rows = vec![back_row(mtm, "Settings", target)];
    rows.push(switch_row(
        mtm,
        "Launch at Login",
        "power",
        status == Status::Enabled,
        status != Status::Unavailable,
        SwitchControl {
            tag: 0,
            target,
            action: sel!(launchAtLoginToggled:),
        },
    ));

    if status == Status::RequiresApproval {
        rows.push(note_row(mtm, "Approval is required in System Settings"));
        rows.push(settings_action_row(
            mtm,
            "Open Login Items Settings",
            "gearshape",
            target,
            sel!(openLoginItemsSettings:),
        ));
    }

    rows.push(settings_action_row(
        mtm,
        "Reconnect Displays",
        "arrow.clockwise",
        target,
        sel!(reconnectDisplays:),
    ));
    rows.push(settings_action_row(
        mtm,
        "Restart DisEQ",
        "arrow.clockwise.circle",
        target,
        sel!(restartApp:),
    ));
    rows.push(settings_action_row(
        mtm,
        "Quit DisEQ",
        "power",
        target,
        sel!(quitApp:),
    ));

    if let Some(notice) = state.settings_notice.as_deref() {
        rows.push(note_row(mtm, notice));
    }
    card_from_rows(mtm, &rows)
}

fn settings_action_row(
    mtm: MainThreadMarker,
    label: &str,
    symbol: &str,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSView> {
    let icon = appkit::row_icon(mtm, symbol);
    let text = appkit::label(mtm, label);
    let spacer = appkit::spacer(mtm);
    let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&icon, &text, &spacer]);
    appkit::clickable_row(mtm, &content, 0, target, action)
}

fn footer_row(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSView> {
    let left = appkit::spacer(mtm);
    let settings = appkit::icon_button(mtm, "gearshape", "Settings", target, sel!(openSettings:));
    // Dismisses the panel; unlike the separate Settings action, this never
    // quits the application when hit by reflex.
    let close = appkit::icon_button(mtm, "xmark", "Close", target, sel!(closePanel:));

    Retained::into_super(appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&left, &settings, &close],
    ))
}

// --- sound card -------------------------------------------------------------

/// Sound rows carry their identity in the same tag field as display rows, but
/// there is no display index to encode. Everything at or above this base
/// belongs to the sound card; below it, to a display.
const SOUND_TAG_BASE: isize = 100_000;

/// Where each kind of sound control starts inside the sound namespace. Bands
/// and app faders are ranges, so they cannot share the switch numbering.
const BAND_TAG_BASE: isize = 100;
const APP_TAG_BASE: isize = 200;
const PREAMP_TAG: isize = 50;
const MASTER_VOLUME_TAG: isize = 51;

pub fn sound_tag(item: isize) -> isize {
    SOUND_TAG_BASE + item
}

pub fn decode_sound_tag(tag: isize) -> Option<isize> {
    (tag >= SOUND_TAG_BASE).then(|| tag - SOUND_TAG_BASE)
}

pub fn band_tag(band: usize) -> isize {
    sound_tag(BAND_TAG_BASE + band as isize)
}

/// The band a tag refers to, if it refers to one.
pub fn decode_band(tag: isize) -> Option<usize> {
    let item = decode_sound_tag(tag)?;
    (BAND_TAG_BASE..APP_TAG_BASE)
        .contains(&item)
        .then(|| (item - BAND_TAG_BASE) as usize)
}

pub fn app_tag(index: usize) -> isize {
    sound_tag(APP_TAG_BASE + index as isize)
}

pub fn decode_app(tag: isize) -> Option<usize> {
    let item = decode_sound_tag(tag)?;
    (item >= APP_TAG_BASE).then(|| (item - APP_TAG_BASE) as usize)
}

pub fn preamp_tag() -> isize {
    sound_tag(PREAMP_TAG)
}

pub fn master_volume_tag() -> isize {
    sound_tag(MASTER_VOLUME_TAG)
}

fn sound_card(
    mtm: MainThreadMarker,
    sound: &SoundService,
    state: &ViewState,
    target: &AnyObject,
) -> Retained<NSView> {
    let icon = appkit::row_icon(mtm, "speaker.wave.2");
    let title = appkit::title(mtm, "Sound");
    let spacer = appkit::spacer(mtm);

    let output_name = appkit::caption(mtm, &sound.output_name());
    let output_chevron = appkit::symbol_view(mtm, "chevron.right");
    let output_content = appkit::hstack(mtm, theme::ROW_SPACING, &[&output_name, &output_chevron]);
    let output_section = appkit::cursor_clickable_row(
        mtm,
        &output_content,
        sound_tag(0),
        target,
        sel!(openOutputs:),
    );
    let header = appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&icon, &title, &spacer, &output_section],
    );

    let mut rows: Vec<Retained<NSView>> = vec![Retained::into_super(header)];
    if !sound.sound_card_is_open() {
        let card = card_from_rows(mtm, &rows);
        return appkit::clickable_surface(
            mtm,
            &card,
            sound_tag(0),
            target,
            sel!(toggleSoundCard:),
            true,
        );
    }

    rows.push(master_volume_row(mtm, sound, target));

    if state.sound_expanded {
        rows.extend(sound_rows(mtm, sound, target));
    }

    if let Some(notice) = state.sound_notice.as_deref() {
        let text = appkit::caption(mtm, notice);
        let spacer = appkit::spacer(mtm);
        rows.push(Retained::into_super(appkit::hstack(
            mtm,
            theme::ROW_SPACING,
            &[&text, &spacer],
        )));
    }

    rows.push(caret_row(mtm, state.sound_expanded, sound_tag(0), target));
    let card = card_from_rows(mtm, &rows);
    appkit::clickable_surface(
        mtm,
        &card,
        sound_tag(0),
        target,
        sel!(toggleSoundCard:),
        false,
    )
}

/// The switches, the fader bank and the app strip — everything behind the
/// caret.
fn sound_rows(
    mtm: MainThreadMarker,
    sound: &SoundService,
    target: &AnyObject,
) -> Vec<Retained<NSView>> {
    let mut rows: Vec<Retained<NSView>> = Vec::new();
    let availability = sound.availability();

    for (label, symbol, action) in SoundAction::ALL {
        let (on, blocked) = match action {
            SoundAction::Routing => (sound.is_routing(), availability.eq_blocked()),
            SoundAction::Equaliser => (sound.settings().enabled, availability.eq_blocked()),
            SoundAction::AutoPreamp => (sound.settings().auto_preamp, availability.eq_blocked()),
            SoundAction::AppMixer => (sound.is_mixing(), availability.mixer_blocked()),
        };
        let available = blocked.is_none();
        rows.push(switch_row(
            mtm,
            label,
            symbol,
            on,
            available,
            SwitchControl {
                tag: sound_tag(*action as isize),
                target,
                action: sel!(soundSwitched:),
            },
        ));
        if let Some(reason) = blocked {
            rows.push(note_row(mtm, &reason));
        }
        // A switch that is on shows what it turns on, directly beneath itself.
        // The controls are the switch's state made visible, so a second gesture
        // to reveal them was one gesture too many.
        if !on || !available {
            continue;
        }
        match action {
            SoundAction::Equaliser => {
                rows.push(preset_row(mtm, sound, target));
                rows.push(band_bank(mtm, sound, target));
                // Auto-preamp owns the headroom while it is on, and a manual
                // offset on top of it can only add gain to a signal that has
                // already been measured as needing less. Hidden rather than
                // disabled: a control that cannot be used is better absent.
                if !sound.settings().auto_preamp {
                    rows.push(preamp_row(mtm, sound, target));
                }
            }
            SoundAction::AppMixer => {
                for (index, fader) in sound.app_faders().iter().enumerate() {
                    rows.push(fader_row(
                        mtm,
                        &fader.name(),
                        theme::APP_LABEL_WIDTH,
                        &format!("{:.0}%", fader.gain * 100.0),
                        fader.gain as f64,
                        0.0,
                        1.0,
                        true,
                        app_tag(index),
                        target,
                        sel!(appGainChanged:),
                    ));
                }
            }
            _ => {}
        }
    }

    rows
}

/// The master volume. Routing gives this meaning on a device that publishes no
/// volume of its own, which is the whole reason the route exists.
fn master_volume_row(
    mtm: MainThreadMarker,
    sound: &SoundService,
    target: &AnyObject,
) -> Retained<NSView> {
    let settable = sound.volume_is_settable();
    let value = sound.volume();
    slider_row(
        mtm,
        "Volume",
        &if settable {
            format!("{:.0}%", value * 100.0)
        } else {
            "Fixed".to_string()
        },
        value,
        0.0,
        1.0,
        settable,
        master_volume_tag(),
        target,
        sel!(volumeChanged:),
        true,
    )
}

fn preset_row(mtm: MainThreadMarker, sound: &SoundService, target: &AnyObject) -> Retained<NSView> {
    let selected = sound.selected_preset();
    let name = eq::preset(selected)
        .map(|preset| preset.name)
        .unwrap_or("Manual");

    let icon = appkit::row_icon(mtm, "list.bullet");
    let text = appkit::label(mtm, "Preset");
    let spacer = appkit::spacer(mtm);
    let detail = appkit::caption(mtm, name);
    let chevron = appkit::symbol_view(mtm, "chevron.right");
    let content = appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&icon, &text, &spacer, &detail, &chevron],
    );
    appkit::clickable_row(mtm, &content, sound_tag(0), target, sel!(openPresets:))
}

/// Ten vertical faders side by side — the shape every graphic equaliser has,
/// and the one the reference UI uses.
///
/// The panel is 400 pt wide and cannot grow: the menu bar decides that. Ten
/// columns leave about 34 pt each, which is why the labels are "16k" rather
/// than "16kHz" and why every one of them is pinned to a width and forbidden to
/// wrap. A label that wraps grows the whole bank by a line.
fn band_bank(mtm: MainThreadMarker, sound: &SoundService, target: &AnyObject) -> Retained<NSView> {
    let gains = sound.settings().gains;
    let mut columns: Vec<Retained<NSView>> = Vec::with_capacity(eq::BAND_COUNT);

    for (band, frequency) in eq::FREQUENCIES.iter().enumerate() {
        let readout = appkit::fixed_caption(
            mtm,
            &format!("{:+.0}", gains[band]),
            theme::BAND_COLUMN_WIDTH,
            NSTextAlignment::Center,
        );
        let slider = appkit::vertical_fader(
            mtm,
            gains[band] as f64,
            *eq::GAIN_RANGE.start() as f64,
            *eq::GAIN_RANGE.end() as f64,
            true,
            band_tag(band),
            target,
            sel!(bandChanged:),
            theme::FADER_HEIGHT,
        );
        let label = appkit::fixed_caption(
            mtm,
            &frequency_label(*frequency),
            theme::BAND_COLUMN_WIDTH,
            NSTextAlignment::Center,
        );

        let column = appkit::vstack_filling(
            mtm,
            theme::BAND_SPACING,
            theme::no_insets(),
            &[&readout, &slider, &label],
        );
        // The stack aligns its children leading by default, which would push
        // every fader against the left edge of its own column.
        column.setAlignment(NSLayoutAttribute::CenterX);
        columns.push(Retained::into_super(column));
    }

    let refs: Vec<&NSView> = columns.iter().map(|column| column.as_ref()).collect();
    Retained::into_super(appkit::hstack_even(mtm, 2.0, &refs))
}

/// One line: `caption | ───────●─────── | value`, both labels fixed-width so
/// stacked rows start and end at the same x and a row's height never depends on
/// how long its caption is. What the preamp and the app faders use; the bands
/// are a bank of their own.
#[allow(clippy::too_many_arguments)]
fn fader_row(
    mtm: MainThreadMarker,
    caption: &str,
    caption_width: f64,
    value: &str,
    position: f64,
    min: f64,
    max: f64,
    enabled: bool,
    tag: isize,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSView> {
    let label = appkit::fixed_caption(mtm, caption, caption_width, NSTextAlignment::Right);
    let slider = appkit::fader(mtm, position, min, max, enabled, tag, target, action);
    let readout = appkit::fixed_caption(mtm, value, theme::READOUT_WIDTH, NSTextAlignment::Right);
    Retained::into_super(appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&label, &slider, &readout],
    ))
}

/// A tenth of the panel's width is about 34 pt. "16kHz" does not fit in it;
/// "16k" does, and the unit is obvious from the company it keeps.
fn frequency_label(frequency: f32) -> String {
    if frequency >= 1_000.0 {
        format!("{:.0}k", frequency / 1_000.0)
    } else {
        format!("{frequency:.0}")
    }
}

fn preamp_row(mtm: MainThreadMarker, sound: &SoundService, target: &AnyObject) -> Retained<NSView> {
    let settings = sound.settings();
    // With auto-preamp on the value is computed, not chosen: showing the
    // fader's own number would contradict what the audio is doing.
    // Only ever shown with auto-preamp off, so the readout and the slider are
    // the same number.
    let shown = settings.global_gain();
    let label = appkit::fixed_caption(
        mtm,
        "Preamp",
        theme::BAND_LABEL_WIDTH,
        NSTextAlignment::Right,
    );
    let slider = appkit::fader(
        mtm,
        settings.preamp as f64,
        *eq::PREAMP_RANGE.start() as f64,
        *eq::PREAMP_RANGE.end() as f64,
        true,
        preamp_tag(),
        target,
        sel!(preampChanged:),
    );
    let readout = appkit::fixed_caption(
        mtm,
        &format!("{shown:+.1}"),
        theme::READOUT_WIDTH,
        NSTextAlignment::Right,
    );
    let reset = appkit::clickable_row(mtm, &readout, preamp_tag(), target, sel!(resetPreamp:));
    Retained::into_super(appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&label, &slider, &reset],
    ))
}

/// A switch, laid out like every other row on the card.
///
/// No caret: a switch that is on shows its controls beneath it, and one that is
/// off has nothing to show. The row is a plain stack because the switch itself
/// is the only interactive part.
struct SwitchControl<'a> {
    tag: isize,
    target: &'a AnyObject,
    action: Sel,
}

fn switch_row(
    mtm: MainThreadMarker,
    label: &str,
    symbol: &str,
    on: bool,
    available: bool,
    control: SwitchControl<'_>,
) -> Retained<NSView> {
    let icon = appkit::row_icon(mtm, symbol);
    let text = appkit::label(mtm, label);
    let spacer = appkit::spacer(mtm);
    let switch = appkit::switch(mtm, on, control.tag, control.target, control.action);
    let row = appkit::hstack(mtm, theme::ROW_SPACING, &[&icon, &text, &spacer, &switch]);

    let row = Retained::into_super(row);
    if !available {
        appkit::set_enabled(&row, false);
    }
    row
}

/// The equaliser's preset list, as a card of its own.
fn presets_card(
    mtm: MainThreadMarker,
    sound: &SoundService,
    target: &AnyObject,
) -> Retained<NSView> {
    let selected = sound.selected_preset();
    let mut rows: Vec<Retained<NSView>> = vec![back_row(mtm, "Preset", target)];

    for (index, preset) in eq::PRESETS.iter().enumerate() {
        let text = appkit::label(mtm, preset.name);
        let spacer = appkit::spacer(mtm);
        let detail = appkit::caption(mtm, if preset.id == selected { "Current" } else { "" });
        let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&text, &spacer, &detail]);
        rows.push(appkit::clickable_row(
            mtm,
            &content,
            sound_tag(index as isize),
            target,
            sel!(presetChosen:),
        ));
    }

    card_from_rows(mtm, &rows)
}

/// The physical destination picker opened from the Sound header.
fn outputs_card(
    mtm: MainThreadMarker,
    sound: &SoundService,
    target: &AnyObject,
) -> Retained<NSView> {
    let outputs = sound.outputs();
    let mut rows: Vec<Retained<NSView>> = vec![back_row(mtm, "Output", target)];

    if outputs.is_empty() {
        rows.push(note_row(mtm, "No hardware outputs are available"));
    }

    for output in &outputs {
        let icon = appkit::row_icon(mtm, "speaker.wave.2");
        let name = appkit::label(mtm, &output.name);
        let spacer = appkit::spacer(mtm);
        let current = appkit::caption(
            mtm,
            if sound.output_is_selected(output) {
                "Current"
            } else {
                ""
            },
        );
        let content = appkit::hstack(mtm, theme::ROW_SPACING, &[&icon, &name, &spacer, &current]);
        rows.push(appkit::clickable_row(
            mtm,
            &content,
            sound_tag(output.id as isize),
            target,
            sel!(outputChosen:),
        ));
    }

    card_from_rows(mtm, &rows)
}

// --- shared -----------------------------------------------------------------

/// A line of explanatory text, left-aligned across the card.
fn note_row(mtm: MainThreadMarker, text: &str) -> Retained<NSView> {
    let caption = appkit::caption(mtm, text);
    let spacer = appkit::spacer(mtm);
    Retained::into_super(appkit::hstack(
        mtm,
        theme::ROW_SPACING,
        &[&caption, &spacer],
    ))
}

fn card_from_rows(mtm: MainThreadMarker, rows: &[Retained<NSView>]) -> Retained<NSView> {
    let refs: Vec<&NSView> = rows.iter().map(|row| row.as_ref()).collect();
    let body = appkit::vstack_filling(mtm, theme::ROW_SPACING, theme::card_insets(), &refs);
    appkit::card(mtm, &body)
}
