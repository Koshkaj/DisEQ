//! Thin wrappers over the AppKit classes this app uses.
//!
//! No maintained safe AppKit wrapper crate exists for `objc2`, so the parts we
//! need live here and the rest of the app stays free of message sends.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSButton, NSColor, NSCompositingOperation, NSControl,
    NSControlStateValueOff, NSControlStateValueOn, NSCursor, NSEvent, NSFont, NSImage, NSImageView,
    NSLayoutAttribute, NSLayoutConstraintOrientation, NSLineBreakMode, NSSlider, NSStackView,
    NSStackViewDistribution, NSSwitch, NSTextAlignment, NSTextField, NSTrackingArea,
    NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSEdgeInsets, NSObject, NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::CATransaction;

use crate::theme;

// --- text -------------------------------------------------------------------

pub fn title(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFont(Some(&NSFont::boldSystemFontOfSize(theme::LABEL_SIZE)));
    field
}

pub fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(theme::LABEL_SIZE)));
    field
}

pub fn caption(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(theme::CAPTION_SIZE)));
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}

/// A caption pinned to an exact width and forbidden from wrapping.
///
/// Two things this fixes. A label left to its intrinsic width makes every row
/// line up differently, so ten band rows come out ten different lengths. And a
/// label narrower than its text wraps by default — "16kHz" becomes two lines
/// and the row grows — which `usesSingleLineMode` turns into truncation
/// instead.
pub fn fixed_caption(
    mtm: MainThreadMarker,
    text: &str,
    width: f64,
    alignment: NSTextAlignment,
) -> Retained<NSTextField> {
    let field = caption(mtm, text);
    field.setAlignment(alignment);
    field.setUsesSingleLineMode(true);
    field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    field.setTranslatesAutoresizingMaskIntoConstraints(false);
    field
        .widthAnchor()
        .constraintEqualToConstant(width)
        .setActive(true);
    field
        .setContentHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    field
}

pub fn symbol_image(name: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
}

/// The artwork supplied for the menu-bar widget, embedded so the icon works
/// both from an application bundle and in development builds.
#[allow(deprecated)]
pub fn widget_image() -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(include_bytes!("../../../images/icon.png"));
    let source = NSImage::initWithData(NSImage::alloc(), &data)?;
    let size = source.size();

    // The supplied artwork occupies only the middle half of its transparent
    // canvas. Scaling that whole canvas made the actual glyph roughly half the
    // intended menu-bar size, so draw just the artwork into a compact image.
    let crop = NSRect::new(
        NSPoint::new(size.width * 0.254, size.height * 0.244),
        NSSize::new(size.width * 0.469, size.height * 0.512),
    );
    let output_size = NSSize::new(25.0, 18.0);
    let output = NSImage::initWithSize(NSImage::alloc(), output_size);
    output.lockFocus();
    source.drawInRect_fromRect_operation_fraction(
        NSRect::new(NSPoint::ZERO, output_size),
        crop,
        NSCompositingOperation::SourceOver,
        1.0,
    );
    output.unlockFocus();
    Some(output)
}

pub fn symbol_view(mtm: MainThreadMarker, name: &str) -> Retained<NSImageView> {
    let view = NSImageView::new(mtm);
    if let Some(image) = symbol_image(name) {
        view.setImage(Some(&image));
    }
    view.setContentHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    view
}

/// A leading row symbol centred in a shared fixed-width column.
///
/// SF Symbols intentionally have different intrinsic widths (`power` is much
/// narrower than `arrow.clockwise`, for example). The container keeps both the
/// symbol centres and the text following them aligned across every card.
pub fn row_icon(mtm: MainThreadMarker, name: &str) -> Retained<NSView> {
    let icon = symbol_view(mtm, name);
    let container = NSView::new(mtm);
    container.setTranslatesAutoresizingMaskIntoConstraints(false);
    icon.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(&icon);

    for constraint in [
        container
            .widthAnchor()
            .constraintEqualToConstant(crate::theme::ROW_ICON_WIDTH),
        icon.centerXAnchor()
            .constraintEqualToAnchor(&container.centerXAnchor()),
        icon.topAnchor()
            .constraintEqualToAnchor(&container.topAnchor()),
        icon.bottomAnchor()
            .constraintEqualToAnchor(&container.bottomAnchor()),
    ] {
        constraint.setActive(true);
    }
    container
        .setContentHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    container
}

/// A real fixed-size status dot. An SF Symbol image view keeps its own larger
/// intrinsic symbol metrics even when the image is assigned a smaller nominal
/// size, so it cannot guarantee the compact marker this UI needs.
pub fn indicator_dot(mtm: MainThreadMarker, diameter: f64, description: &str) -> Retained<NSView> {
    let view = NSView::new(mtm);
    let description = NSString::from_str(description);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setCornerRadius(diameter / 2.0);
        layer.setBackgroundColor(Some(&NSColor::labelColor().CGColor()));
    }
    view.setToolTip(Some(&description));
    view.setAccessibilityElement(true);
    view.setAccessibilityLabel(Some(&description));
    view.widthAnchor()
        .constraintEqualToConstant(diameter)
        .setActive(true);
    view.heightAnchor()
        .constraintEqualToConstant(diameter)
        .setActive(true);
    view.setContentHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    view
}

// --- stacks -----------------------------------------------------------------

fn stack(
    mtm: MainThreadMarker,
    orientation: NSUserInterfaceLayoutOrientation,
    spacing: f64,
    views: &[&NSView],
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(orientation);
    stack.setSpacing(spacing);
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);
    for view in views {
        stack.addArrangedSubview(view);
    }
    stack
}

/// Vertical stack whose children each span its full inner width.
///
/// Neither of the obvious approaches works. `Leading` alignment leaves every
/// row hugging its own content. `Width` alignment does not fill either — rows
/// end up narrower than the stack and pushed to one side. And constraining
/// children to the stack's width without accounting for the insets makes them
/// overflow by exactly the inset, clipping the right-hand side of every row.
///
/// So the insets are applied here and the width constraint subtracts them,
/// keeping the two in step.
pub fn vstack_filling(
    mtm: MainThreadMarker,
    spacing: f64,
    insets: NSEdgeInsets,
    views: &[&NSView],
) -> Retained<NSStackView> {
    let stack = stack(
        mtm,
        NSUserInterfaceLayoutOrientation::Vertical,
        spacing,
        views,
    );
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setEdgeInsets(insets);

    let horizontal_insets = insets.left + insets.right;
    for view in views {
        view.widthAnchor()
            .constraintEqualToAnchor_constant(&stack.widthAnchor(), -horizontal_insets)
            .setActive(true);
    }
    stack
}

pub fn hstack(mtm: MainThreadMarker, spacing: f64, views: &[&NSView]) -> Retained<NSStackView> {
    let stack = stack(
        mtm,
        NSUserInterfaceLayoutOrientation::Horizontal,
        spacing,
        views,
    );
    stack.setAlignment(NSLayoutAttribute::CenterY);
    stack
}

/// A view centred horizontally in a container that fills the row.
///
/// A spacer either side does not hold: a stack divides slack between its
/// spacers by priority rather than evenly, so the centred view slides left and
/// right as the row's contents change width. A centre constraint does not move.
pub fn centered(mtm: MainThreadMarker, view: &NSView) -> Retained<NSView> {
    let container = NSView::new(mtm);
    container.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(view);
    for constraint in [
        view.centerXAnchor()
            .constraintEqualToAnchor(&container.centerXAnchor()),
        view.topAnchor()
            .constraintEqualToAnchor(&container.topAnchor()),
        view.bottomAnchor()
            .constraintEqualToAnchor(&container.bottomAnchor()),
    ] {
        constraint.setActive(true);
    }
    container
}

/// An empty view that absorbs slack, pushing the views on either side apart.
pub fn spacer(mtm: MainThreadMarker) -> Retained<NSView> {
    let view = NSView::new(mtm);
    view.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    view
}

// --- controls ---------------------------------------------------------------

#[derive(Default)]
pub struct DeferredSliderIvars {
    tracking: Cell<bool>,
}

define_class!(
    // SAFETY:
    // - NSSlider supports subclassing and imposes no extra initialization.
    // - KDDeferredSlider has no resources requiring Drop.
    #[unsafe(super(NSSlider))]
    #[thread_kind = MainThreadOnly]
    #[name = "KDDeferredSlider"]
    #[ivars = DeferredSliderIvars]
    pub struct DeferredSlider;

    impl DeferredSlider {
        /// Let NSSlider deliver its normal continuous preview actions while it
        /// tracks, then send one separate commit action after mouse-up.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().tracking.set(true);
            unsafe { msg_send![super(self), mouseDown: event] }
            self.ivars().tracking.set(false);
            let Some(target) = self.target() else { return };
            let app = NSApplication::sharedApplication(self.mtm());
            unsafe {
                app.sendAction_to_from(
                    sel!(resolutionCommitted:),
                    Some(&target),
                    Some(self),
                )
            };
        }
    }
);

/// A slider that previews continuously but performs its expensive operation
/// only once, after mouse-up.
pub fn deferred_slider(mtm: MainThreadMarker) -> Retained<NSSlider> {
    let slider = DeferredSlider::alloc(mtm).set_ivars(DeferredSliderIvars::default());
    let slider: Retained<DeferredSlider> = unsafe { msg_send![super(slider), init] };
    Retained::into_super(slider)
}

pub fn deferred_slider_is_tracking(slider: &NSControl) -> bool {
    slider
        .downcast_ref::<DeferredSlider>()
        .is_some_and(|slider| slider.ivars().tracking.get())
}

/// Horizontal stack whose children are all the same width.
///
/// What a fader bank needs: ten columns of equal width, none of them wider
/// because its label happens to be longer.
pub fn hstack_even(
    mtm: MainThreadMarker,
    spacing: f64,
    views: &[&NSView],
) -> Retained<NSStackView> {
    let stack = stack(
        mtm,
        NSUserInterfaceLayoutOrientation::Horizontal,
        spacing,
        views,
    );
    stack.setAlignment(NSLayoutAttribute::CenterY);
    stack.setDistribution(NSStackViewDistribution::FillEqually);
    stack
}

/// A vertical fader, as a graphic equaliser draws it.
///
/// `NSSlider` decides its orientation from the aspect ratio of its frame unless
/// told otherwise, and a slider in an auto-layout stack has no frame yet — so
/// without `setVertical` every one of these comes out horizontal.
#[allow(clippy::too_many_arguments)]
pub fn vertical_fader(
    mtm: MainThreadMarker,
    value: f64,
    min: f64,
    max: f64,
    enabled: bool,
    tag: isize,
    target: &AnyObject,
    action: Sel,
    height: f64,
) -> Retained<NSSlider> {
    let slider = NSSlider::new(mtm);
    slider.setVertical(true);
    slider.setMinValue(min);
    slider.setMaxValue(max);
    slider.setDoubleValue(value);
    slider.setEnabled(enabled);
    slider.setContinuous(true);
    slider.setTag(tag);
    unsafe {
        slider.setTarget(Some(target));
        slider.setAction(Some(action));
    }
    slider.setTranslatesAutoresizingMaskIntoConstraints(false);
    slider
        .heightAnchor()
        .constraintEqualToConstant(height)
        .setActive(true);
    slider
}

/// A horizontal fader that gives up none of its width to its neighbours.
///
/// The default hugging priority would let the slider shrink to its intrinsic
/// width and leave the labels stretched; lowering it makes the slider the part
/// of the row that absorbs the slack instead.
#[allow(clippy::too_many_arguments)]
pub fn fader(
    mtm: MainThreadMarker,
    value: f64,
    min: f64,
    max: f64,
    enabled: bool,
    tag: isize,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSSlider> {
    let slider = NSSlider::new(mtm);
    slider.setMinValue(min);
    slider.setMaxValue(max);
    slider.setDoubleValue(value);
    slider.setEnabled(enabled);
    slider.setContinuous(true);
    slider.setTag(tag);
    unsafe {
        slider.setTarget(Some(target));
        slider.setAction(Some(action));
    }
    slider.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    slider
}

/// A switch, sized and wired. `NSSwitch` is the control the reference UI uses
/// for on/off rows.
pub fn switch(
    mtm: MainThreadMarker,
    on: bool,
    tag: isize,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSSwitch> {
    let switch = NSSwitch::new(mtm);
    switch.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    switch.setTag(tag);
    unsafe {
        switch.setTarget(Some(target));
        switch.setAction(Some(action));
    }
    switch
        .setContentHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    switch
}

// --- clickable rows ---------------------------------------------------------

#[derive(Default)]
pub struct RowIvars {
    hovered: Cell<bool>,
    hover_alpha: Cell<f64>,
}

define_class!(
    // SAFETY:
    // - NSButton supports subclassing and supplies the standard press/release
    //   tracking and action delivery that a clickable row needs.
    // - KdRow does not implement Drop.
    #[unsafe(super(NSButton))]
    #[thread_kind = MainThreadOnly]
    #[name = "KDRow"]
    #[ivars = RowIvars]
    pub struct KdRow;

    impl KdRow {
        /// Claims the click for the whole row.
        ///
        /// Without this the labels and image views inside the row answer the
        /// hit test themselves and swallow the click, so the row never sees
        /// `mouseDown:` and appears dead. Wrapping the content in an `NSButton`
        /// has the same problem for the same reason.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let hit: *mut NSView = unsafe { msg_send![super(self), hitTest: point] };
            let Some(view) = (unsafe { hit.as_ref() }) else {
                return std::ptr::null_mut();
            };

            // A clickable surface may contain a more specific row or a real
            // control. Those retain their actions; passive labels and images
            // defer to this row so the remaining surface acts as one target.
            // NSTextField is also an NSControl, even when configured as a
            // passive label. Preserve only controls that actually own an
            // interaction; labels and image views defer to the row.
            if owns_interaction(view) {
                hit
            } else {
                (self as *const Self).cast_mut().cast()
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.set_hovered(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_hovered(false);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            unsafe { msg_send![super(self), resetCursorRects] }
            self.addCursorRect_cursor(self.bounds(), &NSCursor::pointingHandCursor());
        }
    }
);

impl KdRow {
    /// The row's identity, as encoded by the view layer.
    pub fn tag_value(&self) -> isize {
        self.tag()
    }

    /// Whether the row is currently drawing its hover highlight.
    fn is_hovered(&self) -> bool {
        self.ivars().hovered.get()
    }

    /// Changes only the wrapper layer and disables implicit Core Animation
    /// actions. The row's content and the vibrancy/card layers never redraw.
    fn set_hovered(&self, hovered: bool) {
        let alpha = self.ivars().hover_alpha.get();
        if alpha <= 0.0 || self.ivars().hovered.replace(hovered) == hovered {
            return;
        }
        let Some(layer) = self.layer() else { return };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if hovered {
            layer.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(1.0, alpha).CGColor()));
        } else {
            layer.setBackgroundColor(None);
        }
        CATransaction::commit();
    }
}

fn owns_interaction(view: &NSView) -> bool {
    view.downcast_ref::<KdRow>().is_some()
        || view.downcast_ref::<NSButton>().is_some()
        || view.downcast_ref::<NSSlider>().is_some()
        || view.downcast_ref::<NSSwitch>().is_some()
}

#[derive(Default)]
struct InteractionProbeIvars {
    calls: Cell<usize>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and the probe owns no
    // resources requiring Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KDInteractionProbe"]
    #[ivars = InteractionProbeIvars]
    struct InteractionProbe;

    impl InteractionProbe {
        #[unsafe(method(probeClicked:))]
        fn probe_clicked(&self, _sender: Option<&AnyObject>) {
            self.ivars().calls.set(self.ivars().calls.get() + 1);
        }
    }
);

/// Runs focused AppKit interaction checks in the real application process.
/// Invoked through `DisEQ --interaction-self-test` by the release verifier.
pub fn run_interaction_self_test(mtm: MainThreadMarker) -> Result<(), String> {
    let probe = InteractionProbe::alloc(mtm).set_ivars(InteractionProbeIvars::default());
    let probe: Retained<InteractionProbe> = unsafe { msg_send![super(probe), init] };

    let text = label(mtm, "Clickable label");
    let view = build_clickable_row(mtm, &text, 7, &probe, sel!(probeClicked:), true);
    let row = view
        .downcast_ref::<KdRow>()
        .ok_or_else(|| "clickable row lost its concrete control type".to_string())?;
    row.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(220.0, 36.0),
    ));
    row.layoutSubtreeIfNeeded();

    let hit: *mut NSView = unsafe { msg_send![row, hitTest: NSPoint::new(110.0, 18.0)] };
    if hit != (row as *const KdRow).cast_mut().cast() {
        return Err("a passive label swallowed its row click".into());
    }

    unsafe { row.performClick(None) };
    if probe.ivars().calls.get() != 1 {
        return Err("the standard button action was not delivered".into());
    }
    row.setEnabled(false);
    unsafe { row.performClick(None) };
    if probe.ivars().calls.get() != 1 {
        return Err("a disabled row still delivered an action".into());
    }
    row.setEnabled(true);

    for _ in 0..250 {
        row.set_hovered(true);
        if row
            .layer()
            .and_then(|layer| layer.animationKeys())
            .is_some_and(|keys| !keys.is_empty())
        {
            return Err("hover created an implicit Core Animation".into());
        }
        row.set_hovered(false);
    }

    let nested = NSSwitch::new(mtm);
    let surface = clickable_surface(mtm, &nested, 8, &probe, sel!(probeClicked:), false);
    let surface = surface
        .downcast_ref::<KdRow>()
        .ok_or_else(|| "clickable surface lost its concrete control type".to_string())?;
    surface.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(220.0, 44.0),
    ));
    surface.layoutSubtreeIfNeeded();
    let nested_bounds = nested.bounds();
    let nested_center = NSPoint::new(
        nested_bounds.origin.x + nested_bounds.size.width / 2.0,
        nested_bounds.origin.y + nested_bounds.size.height / 2.0,
    );
    let nested_point = surface.convertPoint_fromView(nested_center, Some(&nested));
    let hit: *mut NSView = unsafe { msg_send![surface, hitTest: nested_point] };
    if hit != (&*nested as *const NSSwitch).cast_mut().cast() {
        return Err("a clickable card intercepted its nested switch".into());
    }

    // A rebuild replaces the hovered row with a fresh one whose highlight is
    // off, and a pointer that has not moved sends no crossing event to switch
    // it back on. `sync_hover` is what restores it; this checks the placement
    // logic it runs on the new tree.
    let stale = label(mtm, "Row that was replaced");
    let stale = build_clickable_row(mtm, &stale, 9, &probe, sel!(probeClicked:), true);
    let fresh = label(mtm, "Row now under the pointer");
    let fresh = build_clickable_row(mtm, &fresh, 10, &probe, sel!(probeClicked:), true);
    let tree = NSView::new(mtm);
    tree.addSubview(&stale);
    tree.addSubview(&fresh);
    let (stale, fresh) = (
        stale
            .downcast_ref::<KdRow>()
            .ok_or_else(|| "rebuilt row lost its concrete control type".to_string())?,
        fresh
            .downcast_ref::<KdRow>()
            .ok_or_else(|| "rebuilt row lost its concrete control type".to_string())?,
    );
    stale.set_hovered(true);

    let under_pointer: *const NSView = (fresh as *const KdRow).cast();
    apply_hover(&tree, &[under_pointer]);
    if !fresh.is_hovered() {
        return Err("a rebuilt row under the pointer did not regain its highlight".into());
    }
    if stale.is_hovered() {
        return Err("a row away from the pointer kept a stale highlight".into());
    }

    if mouse_is_down() {
        return Err("no mouse button is held, yet the press guard reports one".into());
    }

    Ok(())
}

/// Whether a mouse button is currently held down anywhere.
///
/// A rebuild that lands mid-press throws away the very button the user is
/// pressing, so the press has nothing to complete against and the click is
/// silently lost. Callers defer the rebuild until the button comes back up.
pub fn mouse_is_down() -> bool {
    NSEvent::pressedMouseButtons() != 0
}

/// Re-applies the hover highlight to whichever rows sit under the pointer.
///
/// A rebuild replaces every row with a fresh instance whose hover state starts
/// off, and AppKit only sends `mouseEntered:` when the pointer *crosses* a
/// tracking area's edge — installing one underneath a pointer that is not
/// moving produces no event at all. Without this the highlight drops on every
/// rebuild and comes back only when the user jiggles the mouse, which is what
/// reads as flicker.
pub fn sync_hover(root: &NSView) {
    let Some(window) = root.window() else { return };
    let Some(content) = window.contentView() else {
        return;
    };
    // Before the panel is on screen its frame is still the previous session's,
    // so the pointer cannot be resolved against it yet. Opening the panel ends
    // with a real crossing event anyway.
    if !window.isVisible() {
        return;
    }
    // `set_body` resizes the window, which moves every row. Hit-testing against
    // frames from before that resize would highlight the wrong row.
    content.layoutSubtreeIfNeeded();
    let screen_point = NSEvent::mouseLocation();
    // A pointer outside the window still has to clear stale highlights, and
    // `hitTest:` already answers nil for a point beyond the content view.
    let window_point = window.convertPointFromScreen(screen_point);
    let hit: *mut NSView = unsafe { msg_send![&*content, hitTest: window_point] };

    // Nested tracking areas all contain the point, so AppKit would highlight
    // the hit row and every clickable surface enclosing it. Walking the
    // superview chain reproduces exactly that set.
    let mut chain: Vec<*const NSView> = Vec::new();
    let mut node = unsafe { hit.as_ref() }.map(|view| view.retain());
    while let Some(view) = node {
        chain.push(Retained::as_ptr(&view));
        // SAFETY: the view is retained for the duration of this call and
        // `superview` only reads the hierarchy on the main thread.
        node = unsafe { view.superview() };
    }

    apply_hover(root, &chain);
}

fn apply_hover(view: &NSView, chain: &[*const NSView]) {
    if let Some(row) = view.downcast_ref::<KdRow>() {
        let ptr: *const NSView = (row as *const KdRow).cast();
        row.set_hovered(chain.contains(&ptr));
    }
    for subview in view.subviews() {
        apply_hover(&subview, chain);
    }
}

/// Installs a tracking area that follows the row's visible bounds.
fn install_hover(row: &KdRow, alpha: f64, radius: f64) {
    row.setWantsLayer(true);
    row.ivars().hover_alpha.set(alpha);
    if let Some(layer) = row.layer() {
        layer.setCornerRadius(radius);
    }

    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect,
            Some(row),
            None,
        )
    };
    row.addTrackingArea(&area);
}

/// A row that responds to a click as a single unit.
pub fn clickable_row(
    mtm: MainThreadMarker,
    content: &NSView,
    tag: isize,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSView> {
    build_clickable_row(mtm, content, tag, target, action, true)
}

fn build_clickable_row(
    mtm: MainThreadMarker,
    content: &NSView,
    tag: isize,
    target: &AnyObject,
    action: Sel,
    highlight: bool,
) -> Retained<NSView> {
    let row = KdRow::alloc(mtm).set_ivars(RowIvars::default());
    let row: Retained<KdRow> = unsafe { msg_send![super(row), init] };

    row.setTitle(&NSString::from_str(""));
    row.setBordered(false);
    row.setTag(tag);
    unsafe {
        row.setTarget(Some(target));
        row.setAction(Some(action));
    }
    row.setTranslatesAutoresizingMaskIntoConstraints(false);
    if highlight {
        install_hover(&row, 0.10, theme::ROW_CORNER_RADIUS);
    }
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    row.addSubview(content);
    // Card insets already provide horizontal breathing room. Keeping click-row
    // padding vertical-only means clickable and control-only rows share the
    // exact same leading column.
    pin_with_axis_padding(content, &row, 0.0, theme::ROW_PADDING);

    button_into_view(Retained::into_super(row))
}

/// A compact text action that signals clickability with the cursor only.
/// Useful for controls nested inside a larger card, where another hover layer
/// would visually compete with (and repaint over) the parent surface.
pub fn cursor_clickable_row(
    mtm: MainThreadMarker,
    content: &NSView,
    tag: isize,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSView> {
    build_clickable_row(mtm, content, tag, target, action, false)
}

/// Makes an existing surface clickable without drawing an inset row around it.
/// Descendant rows and controls keep their more specific actions.
pub fn clickable_surface(
    mtm: MainThreadMarker,
    content: &NSView,
    tag: isize,
    target: &AnyObject,
    action: Sel,
    highlight: bool,
) -> Retained<NSView> {
    let surface = KdRow::alloc(mtm).set_ivars(RowIvars::default());
    let surface: Retained<KdRow> = unsafe { msg_send![super(surface), init] };

    surface.setTitle(&NSString::from_str(""));
    surface.setBordered(false);
    surface.setTag(tag);
    unsafe {
        surface.setTarget(Some(target));
        surface.setAction(Some(action));
    }
    surface.setTranslatesAutoresizingMaskIntoConstraints(false);
    if highlight {
        // The card already has a 0.06 white background. Drawing another 0.04
        // behind it composites to approximately the former 0.10 hover.
        install_hover(&surface, 0.04, theme::CARD_CORNER_RADIUS);
    }
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    surface.addSubview(content);
    pin(content, &surface);

    button_into_view(Retained::into_super(surface))
}

fn button_into_view(button: Retained<NSButton>) -> Retained<NSView> {
    let control: Retained<NSControl> = Retained::into_super(button);
    Retained::into_super(control)
}

pub fn icon_button(
    mtm: MainThreadMarker,
    symbol: &str,
    description: &str,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSButton> {
    let button = NSButton::new(mtm);
    let description = NSString::from_str(description);
    button.setTitle(&NSString::from_str(""));
    button.setBordered(false);
    button.setToolTip(Some(&description));
    button.setAccessibilityLabel(Some(&description));
    if let Some(image) = symbol_image(symbol) {
        button.setImage(Some(&image));
    }
    unsafe {
        button.setTarget(Some(target));
        button.setAction(Some(action));
    }
    button
        .setContentHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    button
}

// --- containers -------------------------------------------------------------

pub fn card(mtm: MainThreadMarker, content: &NSView) -> Retained<NSView> {
    let container = NSView::new(mtm);
    container.setWantsLayer(true);
    if let Some(layer) = container.layer() {
        layer.setCornerRadius(theme::CARD_CORNER_RADIUS);
        layer.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.06).CGColor()));
    }

    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    container.addSubview(content);
    pin(content, &container);
    container
}

/// Pins `view` to all four edges of `parent`.
pub fn pin(view: &NSView, parent: &NSView) {
    pin_with_padding(view, parent, 0.0);
}

fn pin_with_padding(view: &NSView, parent: &NSView, padding: f64) {
    pin_with_axis_padding(view, parent, padding, padding);
}

fn pin_with_axis_padding(view: &NSView, parent: &NSView, horizontal: f64, vertical: f64) {
    let constraints = [
        view.leadingAnchor()
            .constraintEqualToAnchor_constant(&parent.leadingAnchor(), horizontal),
        view.trailingAnchor()
            .constraintEqualToAnchor_constant(&parent.trailingAnchor(), -horizontal),
        view.topAnchor()
            .constraintEqualToAnchor_constant(&parent.topAnchor(), vertical),
        view.bottomAnchor()
            .constraintEqualToAnchor_constant(&parent.bottomAnchor(), -vertical),
    ];
    for constraint in &constraints {
        constraint.setActive(true);
    }
}

/// Updates the value shown beside a slider without rebuilding the panel.
///
/// Dragging is continuous, and a full rebuild per mouse-move would replace the
/// slider mid-drag and drop the gesture. The readout is the only thing that
/// needs to change, so it is written in place.
pub fn set_slider_readout(slider: &NSControl, text: &str) {
    let Some(row) = (unsafe { slider.superview() }) else {
        return;
    };
    let mut labels = Vec::new();
    collect_labels(&row, &mut labels);
    // Caption is first, value is last.
    if let Some(value) = labels.last() {
        value.setStringValue(&NSString::from_str(text));
    }
}

/// Updates both labels of a slider row for a value whose mode also changes its
/// caption, such as entering or leaving HiDPI while previewing a resolution.
pub fn set_slider_caption_and_readout(slider: &NSControl, caption: &str, readout: &str) {
    let Some(row) = (unsafe { slider.superview() }) else {
        return;
    };
    let mut labels = Vec::new();
    collect_labels(&row, &mut labels);
    if let Some(label) = labels.first() {
        label.setStringValue(&NSString::from_str(caption));
    }
    if let Some(value) = labels.last() {
        value.setStringValue(&NSString::from_str(readout));
    }
}

fn collect_labels(view: &NSView, out: &mut Vec<Retained<NSTextField>>) {
    for subview in view.subviews().iter() {
        if let Some(field) = subview.downcast_ref::<NSTextField>() {
            out.push(field.retain());
        }
        collect_labels(&subview, out);
    }
}

/// Greys a subtree out and stops it receiving clicks.
///
/// Alpha is applied once at the root; applying it per-view would compound with
/// every level of nesting.
pub fn set_enabled(view: &NSView, enabled: bool) {
    view.setAlphaValue(if enabled { 1.0 } else { theme::DISABLED_ALPHA });
    set_controls_enabled(view, enabled);
}

fn set_controls_enabled(view: &NSView, enabled: bool) {
    if let Some(control) = view.downcast_ref::<NSControl>() {
        control.setEnabled(enabled);
    }
    for subview in view.subviews().iter() {
        set_controls_enabled(&subview, enabled);
    }
}
