//! Thin wrappers over the AppKit classes this app uses.
//!
//! No maintained safe AppKit wrapper crate exists for `objc2`, so the parts we
//! need live here and the rest of the app stays free of message sends.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSApplication, NSButton, NSColor, NSCompositingOperation, NSControl, NSControlStateValueOff,
    NSControlStateValueOn, NSEvent, NSFont, NSImage, NSImageView, NSLayoutAttribute,
    NSLayoutConstraintOrientation, NSLineBreakMode, NSSlider, NSStackView, NSStackViewDistribution,
    NSSwitch, NSTextAlignment, NSTextField, NSTrackingArea, NSTrackingAreaOptions,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{MainThreadMarker, NSData, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};

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
    let data = NSData::with_bytes(include_bytes!("../../../icon.png"));
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

/// An SF Symbol whose meaning is available to VoiceOver and as a hover tip.
pub fn described_symbol_view(
    mtm: MainThreadMarker,
    name: &str,
    description: &str,
    size: f64,
) -> Retained<NSImageView> {
    let view = NSImageView::new(mtm);
    let name = NSString::from_str(name);
    let description = NSString::from_str(description);
    if let Some(image) =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&description))
    {
        image.setSize(NSSize::new(size, size));
        view.setImage(Some(&image));
    }
    view.setToolTip(Some(&description));
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
    tag: Cell<isize>,
    target: RefCell<Option<Retained<AnyObject>>>,
    action: Cell<Option<Sel>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
    highlight_enabled: Cell<bool>,
    highlight_base_alpha: Cell<f64>,
    highlight_target: RefCell<Option<Retained<NSView>>>,
}

define_class!(
    // SAFETY:
    // - NSView imposes no subclassing requirements.
    // - KdRow does not implement Drop.
    #[unsafe(super(NSView))]
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
            if view.downcast_ref::<KdRow>().is_some()
                || view.downcast_ref::<NSControl>().is_some()
            {
                hit
            } else {
                (self as *const Self).cast_mut().cast()
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            let target = self.ivars().target.borrow().clone();
            let (Some(target), Some(action)) = (target, self.ivars().action.get()) else {
                return;
            };
            let app = NSApplication::sharedApplication(self.mtm());
            unsafe { app.sendAction_to_from(action, Some(&target), Some(self)) };
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.set_highlighted(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_highlighted(false);
        }

        /// Tracking areas are tied to a frame, and rows are re-laid out every
        /// time the panel rebuilds, so the old area has to be replaced.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(existing) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&existing);
            }
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    NSTrackingAreaOptions::MouseEnteredAndExited
                        | NSTrackingAreaOptions::ActiveInKeyWindow,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            *self.ivars().tracking.borrow_mut() = Some(area);
        }
    }
);

impl KdRow {
    /// The row's identity, as encoded by the view layer.
    pub fn tag_value(&self) -> isize {
        self.ivars().tag.get()
    }

    fn set_highlighted(&self, on: bool) {
        if !self.ivars().highlight_enabled.get() {
            return;
        }
        let target = self.ivars().highlight_target.borrow();
        let view = target.as_deref().unwrap_or(self);
        if let Some(layer) = view.layer() {
            let alpha = if on {
                0.10
            } else {
                self.ivars().highlight_base_alpha.get()
            };
            layer.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(1.0, alpha).CGColor()));
        }
    }
}

/// A row that responds to a click as a single unit.
pub fn clickable_row(
    mtm: MainThreadMarker,
    content: &NSView,
    tag: isize,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSView> {
    let row = KdRow::alloc(mtm).set_ivars(RowIvars::default());
    let row: Retained<KdRow> = unsafe { msg_send![super(row), init] };

    row.ivars().tag.set(tag);
    row.ivars().action.set(Some(action));
    row.ivars().highlight_enabled.set(true);
    *row.ivars().target.borrow_mut() =
        Some(unsafe { Retained::retain(target as *const _ as *mut _) }.unwrap());

    row.setTranslatesAutoresizingMaskIntoConstraints(false);
    row.setWantsLayer(true);
    if let Some(layer) = row.layer() {
        layer.setCornerRadius(theme::ROW_CORNER_RADIUS);
    }

    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    row.addSubview(content);
    pin_with_padding(content, &row, theme::ROW_PADDING);

    Retained::into_super(row)
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

    surface.ivars().tag.set(tag);
    surface.ivars().action.set(Some(action));
    surface.ivars().highlight_enabled.set(highlight);
    surface.ivars().highlight_base_alpha.set(0.06);
    *surface.ivars().highlight_target.borrow_mut() = Some(content.retain());
    *surface.ivars().target.borrow_mut() =
        Some(unsafe { Retained::retain(target as *const _ as *mut _) }.unwrap());

    surface.setTranslatesAutoresizingMaskIntoConstraints(false);
    surface.setWantsLayer(true);
    content.setTranslatesAutoresizingMaskIntoConstraints(false);
    surface.addSubview(content);
    pin(content, &surface);

    Retained::into_super(surface)
}

pub fn icon_button(
    mtm: MainThreadMarker,
    symbol: &str,
    target: &AnyObject,
    action: Sel,
) -> Retained<NSButton> {
    let button = NSButton::new(mtm);
    button.setTitle(&NSString::from_str(""));
    button.setBordered(false);
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
    let constraints = [
        view.leadingAnchor()
            .constraintEqualToAnchor_constant(&parent.leadingAnchor(), padding),
        view.trailingAnchor()
            .constraintEqualToAnchor_constant(&parent.trailingAnchor(), -padding),
        view.topAnchor()
            .constraintEqualToAnchor_constant(&parent.topAnchor(), padding),
        view.bottomAnchor()
            .constraintEqualToAnchor_constant(&parent.bottomAnchor(), -padding),
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
