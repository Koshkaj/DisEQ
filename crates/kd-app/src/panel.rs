use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBorderType, NSColor, NSPanel, NSScreen, NSScrollView, NSStatusItem,
    NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::appkit;
use crate::theme;

/// Level used by menus and popovers — above ordinary windows, below the menu bar.
const POPUP_MENU_WINDOW_LEVEL: isize = 101;

define_class!(
    // SAFETY:
    // - NSPanel imposes no subclassing requirements beyond NSWindow's.
    // - KdPanel does not implement Drop.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "KDPanel"]
    struct KdPanel;

    impl KdPanel {
        /// A borderless window refuses key status by default, which would leave
        /// the panel unable to take input and — because `windowDidResignKey`
        /// would never fire — unable to dismiss itself when the user clicks
        /// away. `NonactivatingPanel` means taking key here still does not
        /// activate the app, so the frontmost app keeps its focus.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        /// Esc closes the panel.
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            self.orderOut(None);
        }
    }
);

/// The status-bar panel: a borderless, non-activating `NSPanel` with a vibrancy
/// background.
///
/// `NSPopover` would give anchoring and dismissal for free, but always draws an
/// arrow and cannot produce the detached, rounded look this UI needs.
pub struct Panel {
    window: Retained<KdPanel>,
    scroll: Retained<NSScrollView>,
}

impl Panel {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(theme::PANEL_WIDTH, 100.0),
        );

        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let window: Retained<KdPanel> = unsafe {
            msg_send![
                KdPanel::alloc(mtm),
                initWithContentRect: rect,
                styleMask: style,
                backing: NSBackingStoreType::Buffered,
                defer: false,
            ]
        };

        // NSWindow releases itself on close by default, which would leave this
        // handle dangling the first time the panel is dismissed.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setLevel(POPUP_MENU_WINDOW_LEVEL);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
        window.setHidesOnDeactivate(false);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        let content = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect);
        content.setMaterial(NSVisualEffectMaterial::HUDWindow);
        content.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        content.setState(NSVisualEffectState::Active);
        content.setWantsLayer(true);
        if let Some(layer) = content.layer() {
            layer.setCornerRadius(theme::PANEL_CORNER_RADIUS);
            layer.setMasksToBounds(true);
        }
        window.setContentView(Some(&content));

        // Two unfolded display cards are taller than the screen, so the body
        // scrolls rather than growing past the bottom edge where the rest of it
        // could not be reached.
        let scroll = NSScrollView::new(mtm);
        scroll.setDrawsBackground(false);
        scroll.setBorderType(NSBorderType::NoBorder);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        content.addSubview(&scroll);
        appkit::pin(&scroll, &content);

        Self { window, scroll }
    }

    /// Replaces the panel's contents and resizes to fit, up to what the screen
    /// has room for.
    pub fn set_body(&self, body: &NSView) {
        body.setTranslatesAutoresizingMaskIntoConstraints(false);
        self.scroll.setDocumentView(Some(body));

        // A document view is positioned by the clip view, not pinned to it, so
        // without this it collapses to its hugging width and the rows lose the
        // panel's full width.
        let clip = self.scroll.contentView();
        for constraint in [
            body.leadingAnchor()
                .constraintEqualToAnchor(&clip.leadingAnchor()),
            body.trailingAnchor()
                .constraintEqualToAnchor(&clip.trailingAnchor()),
            body.topAnchor().constraintEqualToAnchor(&clip.topAnchor()),
            body.widthAnchor()
                .constraintEqualToAnchor(&clip.widthAnchor()),
        ] {
            constraint.setActive(true);
        }

        let wanted = body.fittingSize().height.max(1.0);
        let height = wanted.min(self.max_height());
        self.window
            .setContentSize(NSSize::new(theme::PANEL_WIDTH, height));
        // Scrolling starts at the top: the panel is read from its first card
        // down, not from wherever the last view left the clip view.
        self.scroll
            .contentView()
            .scrollToPoint(NSPoint::new(0.0, 0.0));
        self.scroll.reflectScrolledClipView(&clip);
    }

    /// How tall the panel may grow: the screen's usable height, less the gap it
    /// keeps from the menu bar and the bottom edge.
    fn max_height(&self) -> f64 {
        let screen = self
            .window
            .screen()
            .or_else(|| MainThreadMarker::new().and_then(NSScreen::mainScreen));
        match screen {
            Some(screen) => {
                (screen.visibleFrame().size.height - theme::PANEL_MARGIN * 2.0).max(1.0)
            }
            None => f64::MAX,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn hide(&self) {
        self.window.orderOut(None);
    }

    /// Positions the panel under the status item and shows it.
    pub fn show(&self, status_item: &NSStatusItem, mtm: MainThreadMarker) {
        self.position_under(status_item, mtm);
        self.window.makeKeyAndOrderFront(None);
    }

    fn position_under(&self, status_item: &NSStatusItem, mtm: MainThreadMarker) {
        let Some(button) = status_item.button(mtm) else {
            return;
        };
        let Some(button_window) = button.window() else {
            return;
        };

        // AppKit screen coordinates are bottom-left origin, so the panel hangs
        // below the status item by subtracting its own height.
        let anchor = button_window.frame();
        let size = self.window.frame().size;
        let mut x = anchor.origin.x + (anchor.size.width - size.width) / 2.0;
        let mut y = anchor.origin.y - size.height - theme::PANEL_MARGIN;

        if let Some(screen) = button_window.screen().or_else(|| NSScreen::mainScreen(mtm)) {
            let visible = screen.visibleFrame();
            let min_x = visible.origin.x + theme::PANEL_MARGIN;
            let max_x = visible.origin.x + visible.size.width - size.width - theme::PANEL_MARGIN;
            x = x.clamp(min_x, max_x.max(min_x));
            // A panel as tall as the screen would otherwise start below the
            // menu bar and run off the bottom, hiding its own scroller.
            y = y.max(visible.origin.y + theme::PANEL_MARGIN);
        }

        self.window.setFrameOrigin(NSPoint::new(x, y));
    }

    pub fn window(&self) -> &NSPanel {
        &self.window
    }
}
