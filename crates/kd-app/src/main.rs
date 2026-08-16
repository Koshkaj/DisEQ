mod appkit;
mod panel;
mod prefs;
mod sound;
mod state;
mod theme;
mod views;

use std::cell::{OnceCell, RefCell};
use std::process::{Command, Stdio};

use kd_core::{DisplayCatalog, Service};
use kd_sys::gamma::Adjustment;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSControl, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWindowDelegate,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSString, NSTimer,
};

use appkit::KdRow;
use panel::Panel;
use sound::SoundService;
use state::{CardAction, Route, SoundAction, Submenu, SubmenuAction, ViewState};

/// Turns a refusal into something worth showing the user.
fn describe(error: kd_sys::power::PowerError) -> String {
    use kd_sys::power::PowerError;
    match error {
        PowerError::LastActiveDisplay => "Cannot disconnect the only active display".to_string(),
        PowerError::NoMirrorTarget => "No other display to mirror onto".to_string(),
        PowerError::NotAttached => "Nothing is plugged into this display's port".to_string(),
        PowerError::LidClosed => {
            "The built-in display stays off while the lid is closed".to_string()
        }
        PowerError::Unavailable => "Disconnect is unavailable on this macOS build".to_string(),
        PowerError::Failed => "The system refused the change".to_string(),
    }
}

fn app_bundle_path() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let bundle = executable.parent()?.parent()?.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

#[derive(Default)]
struct Ivars {
    status_item: OnceCell<Retained<NSStatusItem>>,
    panel: OnceCell<Panel>,
    state: RefCell<ViewState>,
    /// The snapshot used to build the visible panel. Resolution previews read
    /// this instead of re-enumerating every display for every drag event.
    catalog: RefCell<Option<DisplayCatalog>>,
    service: OnceCell<RefCell<Service>>,
    sound: OnceCell<RefCell<SoundService>>,
    /// Drives preset ramps and the route's drift controller. Held so it can be
    /// invalidated; a repeating timer with no owner runs until the app quits.
    ticker: RefCell<Option<Retained<NSTimer>>>,
    /// Watches for displays that are plugged in and dark. Separate from
    /// `ticker`, which stops whenever the sound side has nothing to do — a
    /// monitor being plugged back in is not something that can be allowed to go
    /// unnoticed because no audio is playing.
    watchdog: RefCell<Option<Retained<NSTimer>>>,
    /// The displays the watchdog last saw online, so a hotplug redraws a panel
    /// that is already open.
    online: RefCell<Vec<u32>>,
}

/// Rows carry their identity in the view tag; anything else is not a row.
fn row_tag(sender: Option<&AnyObject>) -> Option<isize> {
    Some(sender?.downcast_ref::<KdRow>()?.tag_value())
}

/// Sliders and switches are real controls, so they report their own tag.
fn control(sender: Option<&AnyObject>) -> Option<&NSControl> {
    sender?.downcast_ref::<NSControl>()
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - AppDelegate does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KDAppDelegate"]
    #[ivars = Ivars]
    struct AppDelegate;

    impl AppDelegate {
        #[unsafe(method(togglePanel:))]
        fn toggle_panel(&self, _sender: Option<&NSObject>) {
            let mtm = self.mtm();
            let (Some(panel), Some(status_item)) =
                (self.ivars().panel.get(), self.ivars().status_item.get())
            else {
                return;
            };

            // Right-click (or control-click) is the app's only menu, so it is
            // handled here rather than by handing the status item a permanent
            // one — that would replace the left-click panel entirely.
            if self.is_secondary_click() {
                panel.hide();
                self.show_status_menu(status_item);
                return;
            }

            if panel.is_visible() {
                panel.hide();
                return;
            }

            // Opening always returns to the top level rather than resuming
            // whatever submenu was last open.
            {
                let mut state = self.ivars().state.borrow_mut();
                state.route = Route::Root;
                state.notice = None;
            }
            if let Some(sound) = self.sound() {
                let mut sound = sound.borrow_mut();
                sound.refresh();
                // Applications start and stop playing while the panel is shut,
                // so the faders on show are the ones that are current.
                if sound.mixer_is_stale() {
                    let _ = sound.refresh_mixer();
                }
            }
            self.rebuild();
            panel.show(status_item, mtm);
        }

        #[unsafe(method(toggleCard:))]
        fn toggle_card(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            {
                let mut state = self.ivars().state.borrow_mut();
                // The sound card shares this caret, and its tag carries no
                // display index to decode.
                if views::decode_sound_tag(tag).is_some() {
                    state.sound_expanded = !state.sound_expanded;
                } else {
                    let (index, _) = views::decode_tag(tag);
                    state.toggle_expanded(index);
                }
            }
            self.rebuild();
        }

        #[unsafe(method(toggleDisplayCard:))]
        fn toggle_display_card(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            let (index, _) = views::decode_tag(tag);
            let catalog = DisplayCatalog::load();
            let Some(display) = catalog.displays.get(index) else {
                return;
            };
            let Some(service) = self.service() else { return };
            if !service.borrow().is_connected(display.id()) {
                return;
            }
            let Some(sound) = self.sound() else { return };

            let open = sound.borrow_mut().toggle_display_card(display);
            if !open {
                // Reopening starts at the primary controls rather than
                // unexpectedly restoring the deeper advanced section too.
                self.ivars().state.borrow_mut().collapse(index);
            }
            self.rebuild();
        }

        #[unsafe(method(toggleSoundCard:))]
        fn toggle_sound_card(&self, _sender: Option<&AnyObject>) {
            let Some(sound) = self.sound() else { return };
            let open = sound.borrow_mut().toggle_sound_card();
            if !open {
                self.ivars().state.borrow_mut().sound_expanded = false;
            }
            self.rebuild();
        }

        #[unsafe(method(openSubmenu:))]
        fn open_submenu(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            let (index, item) = views::decode_tag(tag);
            let Some(submenu) = Submenu::from_index(item) else {
                return;
            };
            self.ivars().state.borrow_mut().route = Route::Submenu {
                display: index,
                submenu,
            };
            self.rebuild();
        }

        #[unsafe(method(goBack:))]
        fn go_back(&self, _sender: Option<&AnyObject>) {
            self.ivars().state.borrow_mut().route = Route::Root;
            self.rebuild();
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            let mut state = self.ivars().state.borrow_mut();
            state.route = Route::Settings;
            state.settings_notice = None;
            drop(state);
            self.rebuild();
        }

        #[unsafe(method(launchAtLoginToggled:))]
        fn launch_at_login_toggled(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let enabled = control.doubleValue() != 0.0;
            let notice = match kd_sys::login_item::set_enabled(enabled) {
                Ok(kd_sys::login_item::Status::RequiresApproval) => {
                    Some("Approve DisEQ under Login Items in System Settings".into())
                }
                Ok(_) => None,
                Err(error) => Some(error),
            };
            self.ivars().state.borrow_mut().settings_notice = notice;
            self.rebuild();
        }

        #[unsafe(method(openLoginItemsSettings:))]
        fn open_login_items_settings(&self, _sender: Option<&AnyObject>) {
            if !kd_sys::login_item::open_system_settings() {
                self.ivars().state.borrow_mut().settings_notice =
                    Some("Could not open Login Items in System Settings".into());
                self.rebuild();
            }
        }

        #[unsafe(method(installDriver:))]
        fn install_driver(&self, _sender: Option<&AnyObject>) {
            self.change_driver(kd_sys::driver_install::install, true);
        }

        #[unsafe(method(uninstallDriver:))]
        fn uninstall_driver(&self, _sender: Option<&AnyObject>) {
            self.change_driver(kd_sys::driver_install::uninstall, false);
        }

        #[unsafe(method(restartApp:))]
        fn restart_app(&self, _sender: Option<&AnyObject>) {
            let Some(bundle) = app_bundle_path() else {
                self.ivars().state.borrow_mut().settings_notice =
                    Some("Restart is available only from DisEQ.app".into());
                self.rebuild();
                return;
            };
            // Wait for applicationWillTerminate to finish restoring the audio
            // route and display gamma before starting the replacement process.
            // Passing the PID and path as arguments keeps the fixed helper
            // script independent of any shell escaping in the bundle path.
            let launched = Command::new("/bin/sh")
                .arg("-c")
                .arg(
                    "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; \
                     exec /usr/bin/open -n \"$2\"",
                )
                .arg("diseq-restart")
                .arg(std::process::id().to_string())
                .arg(bundle)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Err(error) = launched {
                self.ivars().state.borrow_mut().settings_notice =
                    Some(format!("Could not restart DisEQ: {error}"));
                self.rebuild();
                return;
            }
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        // --- controls -------------------------------------------------------

        #[unsafe(method(brightnessChanged:))]
        fn brightness_changed(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let (index, _) = views::decode_tag(control.tag());
            let value = control.doubleValue();

            self.with_display(index, |service, display| {
                service.set_brightness(display.id(), value);
            });
            appkit::set_slider_readout(control, &format!("{:.0}%", value * 100.0));
        }

        #[unsafe(method(volumeChanged:))]
        fn volume_changed(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let value = control.doubleValue();
            if let Some(sound) = self.sound() {
                sound.borrow_mut().set_volume(value);
            }
            appkit::set_slider_readout(control, &format!("{:.0}%", value * 100.0));
        }

        // --- sound ----------------------------------------------------------

        #[unsafe(method(soundSwitched:))]
        fn sound_switched(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let Some(item) = views::decode_sound_tag(control.tag()) else {
                return;
            };
            let Some(action) = SoundAction::from_index(item) else {
                return;
            };
            let Some(sound) = self.sound() else { return };

            let outcome = {
                let mut sound = sound.borrow_mut();
                match action {
                    SoundAction::Routing => {
                        let wanted = !sound.is_routing();
                        sound.set_routing(wanted)
                    }
                    SoundAction::Equaliser => {
                        let wanted = !sound.settings().enabled;
                        sound.set_eq_enabled(wanted);
                        Ok(())
                    }
                    SoundAction::AutoPreamp => {
                        let wanted = !sound.settings().auto_preamp;
                        sound.set_auto_preamp(wanted);
                        Ok(())
                    }
                    SoundAction::AppMixer => {
                        let wanted = !sound.is_mixing();
                        sound.set_mixing(wanted)
                    }
                }
            };
            // A switch that springs back with no explanation reads as broken,
            // so the refusal goes on the card.
            self.ivars().state.borrow_mut().sound_notice = outcome.err();
            // A running route needs its drift controller stepped; nothing else
            // here does, and `tick:` stops the timer once that stops being true.
            self.start_ticking();
            self.rebuild();
        }

        #[unsafe(method(bandChanged:))]
        fn band_changed(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let Some(band) = views::decode_band(control.tag()) else {
                return;
            };
            let gain = control.doubleValue() as f32;
            if let Some(sound) = self.sound() {
                sound.borrow_mut().set_band(band, gain);
            }
            // Rebuilding mid-drag would replace the fader and drop the gesture,
            // so only the number above it changes.
            appkit::set_slider_readout(control, &format!("{gain:+.1}"));
        }

        #[unsafe(method(preampChanged:))]
        fn preamp_changed(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let value = control.doubleValue() as f32;
            let Some(sound) = self.sound() else { return };
            let shown = {
                let mut sound = sound.borrow_mut();
                sound.set_preamp(value);
                sound.settings().global_gain()
            };
            appkit::set_slider_readout(control, &format!("{shown:+.1}"));
        }

        #[unsafe(method(resetPreamp:))]
        fn reset_preamp(&self, _sender: Option<&AnyObject>) {
            if let Some(sound) = self.sound() {
                sound.borrow_mut().set_preamp(0.0);
            }
            self.rebuild();
        }

        #[unsafe(method(appGainChanged:))]
        fn app_gain_changed(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let Some(index) = views::decode_app(control.tag()) else {
                return;
            };
            let gain = control.doubleValue() as f32;
            if let Some(sound) = self.sound() {
                let mut sound = sound.borrow_mut();
                if let Some(fader) = sound.app_faders().get(index) {
                    let process = fader.process.id;
                    sound.set_app_gain(process, gain);
                }
            }
            appkit::set_slider_readout(control, &format!("{:.0}%", gain * 100.0));
        }

        #[unsafe(method(openPresets:))]
        fn open_presets(&self, _sender: Option<&AnyObject>) {
            self.ivars().state.borrow_mut().route = Route::Presets;
            self.rebuild();
        }

        #[unsafe(method(openOutputs:))]
        fn open_outputs(&self, _sender: Option<&AnyObject>) {
            self.ivars().state.borrow_mut().route = Route::Outputs;
            self.rebuild();
        }

        #[unsafe(method(outputChosen:))]
        fn output_chosen(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            let Some(item) = views::decode_sound_tag(tag) else {
                return;
            };
            let Ok(device_id) = u32::try_from(item) else {
                return;
            };
            let Some(sound) = self.sound() else { return };
            let outcome = sound.borrow_mut().select_output(device_id);
            self.ivars().state.borrow_mut().sound_notice = outcome.err();
            self.ivars().state.borrow_mut().route = Route::Root;
            self.start_ticking();
            self.rebuild();
        }

        #[unsafe(method(presetChosen:))]
        fn preset_chosen(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            let Some(index) = views::decode_sound_tag(tag) else {
                return;
            };
            let Some(preset) = kd_audio::PRESETS.get(index as usize) else {
                return;
            };
            if let Some(sound) = self.sound() {
                sound.borrow_mut().set_preset(preset.id);
            }
            self.start_ticking();
            self.ivars().state.borrow_mut().route = Route::Root;
            self.rebuild();
        }

        /// One frame of whatever is in motion: a preset ramp, and the route's
        /// clock-drift correction.
        #[unsafe(method(tick:))]
        fn tick(&self, _sender: Option<&AnyObject>) {
            let Some(sound) = self.sound() else { return };
            let redraw = sound.borrow_mut().tick();
            let busy = sound.borrow().wants_ticks();
            if redraw && self.panel_is_visible() {
                self.rebuild();
            }
            if !busy {
                self.stop_ticking();
            }
        }

        /// Notices displays that are plugged in but dark, and redraws an open
        /// panel when the set of displays changes.
        ///
        /// Polled rather than driven by `CGDisplayRegisterReconfigurationCallback`,
        /// because the case that matters is a display CoreGraphics does not
        /// consider to exist: a disabled port publishes no reconfiguration when
        /// a monitor is plugged into it, so waiting for one waits forever.
        #[unsafe(method(watchDisplays:))]
        fn watch_displays(&self, _sender: Option<&AnyObject>) {
            // Off the main thread, and only when there is something to do — the
            // transactions block for as long as the window server takes.
            kd_core::power::recover_orphaned_panels_async();

            let online: Vec<u32> = kd_sys::display::online_displays()
                .into_iter()
                .map(|id| id.0)
                .collect();
            if *self.ivars().online.borrow() == online {
                return;
            }
            *self.ivars().online.borrow_mut() = online;
            if self.panel_is_visible() {
                self.rebuild();
            }
        }

        #[unsafe(method(resolutionPreview:))]
        fn resolution_preview(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let (index, _) = views::decode_tag(control.tag());
            let choice = control.doubleValue().round() as usize;

            if let Some((caption, readout)) = self.resolution_description(index, choice) {
                appkit::set_slider_caption_and_readout(control, &caption, &readout);
            }

            // Mouse tracking gets one explicit commit from KDDeferredSlider
            // after mouse-up. Keyboard and accessibility actions have no such
            // tracking loop, so commit those immediately.
            if !appkit::deferred_slider_is_tracking(control) {
                self.commit_resolution(control);
            }
        }

        #[unsafe(method(resolutionCommitted:))]
        fn resolution_committed(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            self.commit_resolution(control);
        }

        #[unsafe(method(connectToggled:))]
        fn connect_toggled(&self, sender: Option<&AnyObject>) {
            let Some(control) = control(sender) else { return };
            let (index, _) = views::decode_tag(control.tag());
            let wants_connected = control.doubleValue() != 0.0;

            let mut failure = None;
            self.with_display(index, |service, display| {
                if let Err(error) = service.set_connected(display, wants_connected) {
                    failure = Some(describe(error));
                } else if !wants_connected {
                    self.ivars().state.borrow_mut().collapse(index);
                    if let Some(sound) = self.sound() {
                        sound.borrow_mut().close_display_card(display);
                    }
                }
            });
            self.ivars().state.borrow_mut().notice = failure;
            self.rebuild();
        }

        #[unsafe(method(cardAction:))]
        fn card_action(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            let (index, item) = views::decode_tag(tag);
            let Some(action) = CardAction::from_index(item) else {
                return;
            };

            let Some(service) = self.service() else { return };
            let catalog = DisplayCatalog::load();
            let Some(display) = catalog.displays.get(index) else {
                return;
            };

            let display_order_changed = {
                let service = service.borrow();
                let (on, available) = action.state(display, &service);
                if !available {
                    return;
                }
                match action {
                    CardAction::HiDpi => {
                        service.set_hidpi(display.id(), !on);
                        false
                    }
                    CardAction::SetAsMain => service.set_main_display(display.id()),
                    CardAction::AutoBrightness => {
                        service.set_auto_brightness(display.id(), !on);
                        false
                    }
                    CardAction::NightShift => {
                        service.set_night_shift(!on);
                        false
                    }
                    CardAction::DisplayNotch | CardAction::BrightnessUpscaling => false,
                }
            };
            if display_order_changed {
                self.ivars().state.borrow_mut().display_order_changed();
            }
            self.rebuild();
        }

        #[unsafe(method(submenuRow:))]
        fn submenu_row(&self, sender: Option<&AnyObject>) {
            let Some(tag) = row_tag(sender) else { return };
            let (index, row_index) = views::decode_tag(tag);

            let route = self.ivars().state.borrow().route.clone();
            let Route::Submenu { submenu, .. } = route else {
                return;
            };
            let Some(service) = self.service() else { return };
            let catalog = DisplayCatalog::load();
            let Some(display) = catalog.displays.get(index) else {
                return;
            };

            let display_order_changed = {
                let service = service.borrow();
                // Recomputed from the same inputs the view used, so the row
                // index still refers to the row the user clicked.
                let rows = submenu.rows(display, &catalog.displays, &service);
                let Some(action) = rows.get(row_index as usize).and_then(|row| row.action) else {
                    return;
                };

                let id = display.id();
                match action {
                    SubmenuAction::SetMode(mode) => {
                        service.set_refresh_rate(id, mode);
                        false
                    }
                    SubmenuAction::SetMirror(source) => {
                        service.set_mirror(id, source);
                        false
                    }
                    SubmenuAction::SetMain => service.set_main_display(id),
                    SubmenuAction::MoveTo(x, y) => {
                        service.move_display(id, x, y);
                        false
                    }
                    SubmenuAction::SetInput(source) => {
                        service.set_input_source(id, source);
                        false
                    }
                    SubmenuAction::PanelPower(on) => {
                        service.set_panel_power(id, on);
                        false
                    }
                    SubmenuAction::SetColour { red, green, blue } => {
                        let mut adjustment = service.colour_adjustment(id);
                        adjustment.red = red;
                        adjustment.green = green;
                        adjustment.blue = blue;
                        service.set_colour_adjustment(id, adjustment);
                        false
                    }
                    SubmenuAction::ResetColour => {
                        service.set_colour_adjustment(id, Adjustment::default());
                        false
                    }
                    SubmenuAction::SetProtected(on) => {
                        if on {
                            kd_core::protection::protect(id);
                        } else {
                            kd_core::protection::unprotect(id);
                        }
                        false
                    }
                }
            };
            if display_order_changed {
                self.ivars().state.borrow_mut().display_order_changed();
            }
            self.rebuild();
        }

        /// The panel's close button: dismisses the window, leaves the app in
        /// the menu bar.
        #[unsafe(method(closePanel:))]
        fn close_panel(&self, _sender: Option<&AnyObject>) {
            if let Some(panel) = self.ivars().panel.get() {
                panel.hide();
            }
        }

        /// Brings back every display this app switched off, including in an
        /// earlier session — the way out when a display went off before a quit
        /// and its card's own toggle no longer reaches it.
        #[unsafe(method(reconnectDisplays:))]
        fn reconnect_displays(&self, _sender: Option<&AnyObject>) {
            let Some(service) = self.service() else { return };
            let restored = service.borrow().reconnect_all();
            let message = match restored {
                0 => Some("No disconnected displays to bring back".to_string()),
                1 => Some("1 display reconnected".to_string()),
                count => Some(format!("{count} displays reconnected")),
            };
            let mut state = self.ivars().state.borrow_mut();
            if state.route == Route::Settings {
                state.settings_notice = message;
            } else {
                state.notice = message;
            }
            drop(state);
            self.rebuild();

            // The menu click dismissed the panel, and the result of this is
            // written in the panel — so it comes back to report.
            if let (Some(panel), Some(status_item)) =
                (self.ivars().panel.get(), self.ivars().status_item.get())
            {
                panel.show(status_item, self.mtm());
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        /// Target for controls that have no backend yet.
        #[unsafe(method(noop:))]
        fn noop(&self, _sender: Option<&AnyObject>) {}
    }

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();

            let status_bar = NSStatusBar::systemStatusBar();
            let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
            if let Some(button) = item.button(mtm) {
                if let Some(image) = appkit::widget_image().or_else(|| appkit::symbol_image("display")) {
                    button.setImage(Some(&image));
                }
                unsafe {
                    button.setTarget(Some(self));
                    button.setAction(Some(sel!(togglePanel:)));
                    // A button fires on left-up only, so the right-click menu
                    // would never be reached without asking for the event.
                    button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
                }
            }

            let panel = Panel::new(mtm);
            panel
                .window()
                .setDelegate(Some(ProtocolObject::from_ref(self)));

            // Probing backends touches DDC, so it happens once here rather than
            // on every panel open.
            let _ = self.ivars().service.set(RefCell::new(Service::start()));
            let _ = self.ivars().sound.set(RefCell::new(SoundService::new()));
            kd_core::protection::install();
            // The status item is retained only by this cell — dropping it would
            // take the icon out of the menu bar.
            let _ = self.ivars().status_item.set(item);
            let _ = self.ivars().panel.set(panel);
            // The route starts with the application, so its drift controller
            // needs stepping from here rather than from the first click; the
            // same timer is what notices the user changing output device.
            self.start_ticking();
            // A display can be plugged in while the panel is shut and the sound
            // side is idle, so this timer runs for the life of the app.
            self.start_watching_displays();

            // Last, because it is modal: everything above has to be in place
            // before the app stops to ask a question.
            self.offer_driver_install();
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.stop_ticking();
            // Leaving a tap behind leaves an application silent, and leaving the
            // route up leaves the default output pointed at a device nothing is
            // draining.
            if let Some(sound) = self.sound() {
                sound.borrow_mut().shutdown();
            }
            // Gamma changes outlive the process, so a dimmed display would stay
            // dimmed with nothing left to undo it.
            kd_sys::gamma::reset_all();
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            if let Some(panel) = self.ivars().panel.get() {
                panel.hide();
            }
        }
    }
);

impl AppDelegate {
    fn service(&self) -> Option<&RefCell<Service>> {
        self.ivars().service.get()
    }

    /// Runs a privileged driver change and reports the outcome where the user
    /// is looking. `installed` is the state being moved to, which is what the
    /// wait afterwards is waiting for.
    ///
    /// The panel is rebuilt because this decides whether the equaliser's
    /// switches are offered at all.
    fn change_driver(
        &self,
        change: fn() -> Result<(), kd_sys::driver_install::Error>,
        installed: bool,
    ) {
        use kd_sys::driver_install::Error;

        let message = match change() {
            Ok(()) => {
                if let Some(sound) = self.sound() {
                    let mut sound = sound.borrow_mut();
                    sound.await_driver(installed);
                    // A driver removed on purpose should not be offered again
                    // at the next launch; one installed clears the refusal, so
                    // a later disappearance is worth asking about.
                    sound.set_driver_prompt_declined(!installed);
                }
                Some(if installed {
                    "Audio driver installed".to_string()
                } else {
                    "Audio driver removed".to_string()
                })
            }
            // Dismissing the authorisation dialog is an answer, not a failure.
            Err(Error::Cancelled) => return,
            Err(error) => Some(error.to_string()),
        };

        let mut state = self.ivars().state.borrow_mut();
        if state.route == Route::Settings {
            state.settings_notice = message;
        } else {
            state.sound_notice = message;
        }
        drop(state);
        self.rebuild();
    }

    /// Offers to install the bundled driver, once, at launch.
    ///
    /// Without the plug-in the equaliser cannot do anything at all, and the app
    /// is carrying the copy it needs — so the alternative to asking is a menu
    /// bar icon whose main feature is greyed out for no visible reason. A
    /// refusal is remembered, and Settings keeps the offer available.
    fn offer_driver_install(&self) {
        let state = kd_sys::driver_install::state();
        if !state.needs_install() {
            return;
        }
        let declined = self
            .sound()
            .is_some_and(|sound| sound.borrow().driver_prompt_declined());
        // An update is a different offer from the one that was refused.
        if declined && matches!(state, kd_sys::driver_install::State::NotInstalled) {
            return;
        }

        let updating = matches!(state, kd_sys::driver_install::State::Outdated { .. });
        let mtm = self.mtm();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(if updating {
            "Update the DisEQ audio driver?"
        } else {
            "Install the DisEQ audio driver?"
        }));
        alert.setInformativeText(&NSString::from_str(
            "The equaliser and per-app volume work by routing your Mac's audio \
             through a small audio driver. macOS will ask for your password, \
             and sound will stop for about a second while it loads.",
        ));
        alert.addButtonWithTitle(&NSString::from_str(if updating {
            "Update"
        } else {
            "Install"
        }));
        alert.addButtonWithTitle(&NSString::from_str("Later"));

        // The first button added is `NSAlertFirstButtonReturn`.
        if alert.runModal() == NSAlertFirstButtonReturn {
            self.change_driver(kd_sys::driver_install::install, true);
        } else if let Some(sound) = self.sound() {
            sound.borrow_mut().set_driver_prompt_declined(true);
        }
    }

    fn sound(&self) -> Option<&RefCell<SoundService>> {
        self.ivars().sound.get()
    }

    fn panel_is_visible(&self) -> bool {
        self.ivars()
            .panel
            .get()
            .map(|panel| panel.is_visible())
            .unwrap_or(false)
    }

    /// Starts the frame timer if anything needs one. Idempotent: a second call
    /// while it is already running leaves the existing timer alone.
    fn start_ticking(&self) {
        if self.ivars().ticker.borrow().is_some() {
            return;
        }
        let interval = f64::from(SoundService::TICK_MS) / 1_000.0;
        // SAFETY: the timer targets this delegate, which lives as long as the
        // application, and is invalidated in `stop_ticking`.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                interval,
                self,
                sel!(tick:),
                None,
                true,
            )
        };
        *self.ivars().ticker.borrow_mut() = Some(timer);
    }

    fn stop_ticking(&self) {
        if let Some(timer) = self.ivars().ticker.borrow_mut().take() {
            timer.invalidate();
        }
    }

    /// How often to look for a monitor that has been plugged into a port the
    /// window server still has switched off. Slow: it is a plug-in-a-cable
    /// event, and the check walks the IORegistry.
    const WATCHDOG_SECONDS: f64 = 3.0;

    fn start_watching_displays(&self) {
        if self.ivars().watchdog.borrow().is_some() {
            return;
        }
        // SAFETY: the timer targets this delegate, which lives as long as the
        // application.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                Self::WATCHDOG_SECONDS,
                self,
                sel!(watchDisplays:),
                None,
                true,
            )
        };
        *self.ivars().watchdog.borrow_mut() = Some(timer);
    }

    /// Whether the click being handled asked for a context menu.
    fn is_secondary_click(&self) -> bool {
        let Some(event) = NSApplication::sharedApplication(self.mtm()).currentEvent() else {
            return false;
        };
        let kind = event.r#type();
        let control_held = event
            .modifierFlags()
            .contains(NSEventModifierFlags::Control);

        matches!(
            kind,
            NSEventType::RightMouseDown | NSEventType::RightMouseUp
        ) || (control_held && matches!(kind, NSEventType::LeftMouseDown | NSEventType::LeftMouseUp))
    }

    /// The app's only menu: recovery, and the only way out — the panel's close
    /// button just dismisses the window.
    fn show_status_menu(&self, status_item: &NSStatusItem) {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        for (title, action, key) in [
            ("Reconnect Displays", sel!(reconnectDisplays:), ""),
            ("Quit DisEQ", sel!(quitApp:), "q"),
        ] {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            };
            unsafe { item.setTarget(Some(self)) };
            menu.addItem(&item);
        }

        // A status item that owns a menu shows it on every click and stops
        // sending its action, which would cost the panel its left click — so
        // the menu is attached only for as long as this one is tracking.
        status_item.setMenu(Some(&menu));
        if let Some(button) = status_item.button(mtm) {
            unsafe { button.performClick(None) };
        }
        status_item.setMenu(None);
    }

    fn with_display(&self, index: usize, body: impl FnOnce(&Service, &kd_core::Display)) {
        let Some(service) = self.service() else {
            return;
        };
        let catalog = DisplayCatalog::load();
        let Some(display) = catalog.displays.get(index) else {
            return;
        };
        body(&service.borrow(), display);
    }

    fn resolution_description(&self, index: usize, choice: usize) -> Option<(String, String)> {
        let catalog = self.ivars().catalog.borrow();
        let display = catalog.as_ref()?.displays.get(index)?;
        let modes = display.selectable_modes();
        let mode = modes.get(choice)?;
        let widest = modes.last()?.width;
        let caption = if mode.is_hidpi() {
            "Resolution (HiDPI)"
        } else {
            "Resolution"
        };
        let readout = if widest > 0 {
            format!(
                "{}x{} · {:.0}%",
                mode.width,
                mode.height,
                mode.width as f64 / widest as f64 * 100.0
            )
        } else {
            format!("{}x{}", mode.width, mode.height)
        };
        Some((caption.to_string(), readout))
    }

    fn commit_resolution(&self, control: &NSControl) {
        let (index, _) = views::decode_tag(control.tag());
        let choice = control.doubleValue().round() as usize;
        let display = self
            .ivars()
            .catalog
            .borrow()
            .as_ref()
            .and_then(|catalog| catalog.displays.get(index))
            .cloned()
            .or_else(|| DisplayCatalog::load().displays.get(index).cloned());

        if let (Some(service), Some(display)) = (self.service(), display) {
            service.borrow().set_resolution(&display, choice);
        }
        self.rebuild();
    }

    /// Rebuilds the panel body from a fresh snapshot, so hotplug and changes
    /// made elsewhere are always reflected.
    fn rebuild(&self) {
        let (Some(panel), Some(service), Some(sound)) =
            (self.ivars().panel.get(), self.service(), self.sound())
        else {
            return;
        };
        // Never swap the view tree out from under a press. The button holding
        // the mouse down would be discarded before it could match the mouse-up,
        // and the click would vanish with it. Whatever prompted this rebuild is
        // polled state, so the next tick redraws it a frame later.
        if appkit::mouse_is_down() {
            return;
        }
        // A display that was off when the backends were probed has none, so a
        // reconnected one would show a dead brightness slider until restart.
        service.borrow_mut().refresh_if_stale();
        // Opening the panel is the other moment a monitor plugged into a
        // switched-off port should come back, rather than waiting out the
        // watchdog's interval.
        kd_core::power::recover_orphaned_panels_async();

        let catalog = DisplayCatalog::load();
        *self.ivars().catalog.borrow_mut() = Some(catalog.clone());
        let state = self.ivars().state.borrow();
        let body = views::root(
            self.mtm(),
            &catalog,
            &service.borrow(),
            &sound.borrow(),
            &state,
            self,
        );
        panel.set_body(&body);
        // The rows under the pointer are new objects with their hover state
        // cleared, and a stationary pointer generates no crossing event to turn
        // it back on.
        appkit::sync_hover(&body);
    }

    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        unsafe { msg_send![super(this), init] }
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    if std::env::args().any(|arg| arg == "--interaction-self-test") {
        match appkit::run_interaction_self_test(mtm) {
            Ok(()) => {
                println!("interaction self-test passed");
                return;
            }
            Err(error) => {
                eprintln!("interaction self-test failed: {error}");
                std::process::exit(1);
            }
        }
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    app.run();
}
