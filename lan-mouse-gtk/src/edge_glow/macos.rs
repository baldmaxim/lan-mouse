//! macOS backend: a click-through borderless panel above everything.
//!
//! Must be called on the main thread, which is also the thread of the GTK
//! main loop. The panel is created on the first flash and reused afterwards.

use std::{
    cell::{Cell, RefCell},
    ptr::{NonNull, copy_nonoverlapping, null_mut},
};

use block2::RcBlock;
use input_event::edge::{Edge, Rect};
use objc2::{AllocAnyThread, MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSBackingStoreType, NSBitmapImageRep,
    NSColor, NSDeviceRGBColorSpace, NSImage, NSImageScaling, NSImageView, NSPanel, NSScreen,
    NSScreenSaverWindowLevel, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CAMediaTimingFunction, kCAMediaTimingFunctionEaseIn};

use super::render::{GLOW_DEPTH, GLOW_LENGTH, locate, place, render_glow};

/// duration of the fade out, seconds
const FADE_OUT_SECS: f64 = 0.22;

struct GlowPanel {
    panel: Retained<NSPanel>,
    view: Retained<NSImageView>,
}

thread_local! {
    static PANEL: RefCell<Option<GlowPanel>> = const { RefCell::new(None) };
    /// incremented on every flash, tells a finished fade out if it is still current
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

pub fn flash(edge: Edge, fraction: f64) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("edge glow: not on the main thread");
        return;
    };
    if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
        return;
    }

    // Cocoa screen frames have their origin at the bottom left of the primary
    // screen, the fraction refers to coordinates with the origin at the top
    // left (like CGDisplayBounds), so convert.
    let screens = NSScreen::screens(mtm);
    let Some(primary) = screens.iter().next() else {
        return;
    };
    let primary_height = primary.frame().size.height;
    let displays: Vec<(Rect, Retained<NSScreen>)> = screens
        .iter()
        .map(|screen| {
            let f = screen.frame();
            let y_top = primary_height - (f.origin.y + f.size.height);
            let rect = Rect {
                x_min: f.origin.x,
                y_min: y_top,
                x_max: f.origin.x + f.size.width,
                y_max: y_top + f.size.height,
            };
            (rect, screen)
        })
        .collect();
    let rects: Vec<Rect> = displays.iter().map(|(rect, _)| *rect).collect();
    let Some((display, point)) = locate(&rects, edge, fraction) else {
        return;
    };
    let Some((_, screen)) = displays.iter().find(|(rect, _)| *rect == display) else {
        return;
    };

    // geometry in points, the image in backing pixels of the screen
    let scale = screen.backingScaleFactor();
    let placement = place(display, edge, point, GLOW_DEPTH, GLOW_LENGTH);
    let rect = placement.rect;
    let size = NSSize::new(rect.x_max - rect.x_min, rect.y_max - rect.y_min);
    let px_width = (size.width * scale).round().max(1.0) as u32;
    let px_height = (size.height * scale).round().max(1.0) as u32;
    let pixels = render_glow(edge, px_width, px_height, placement.center_along * scale);
    let Some(image) = make_image(&pixels, px_width as usize, px_height as usize, size) else {
        log::warn!("edge glow: failed to create image");
        return;
    };
    let frame = NSRect::new(NSPoint::new(rect.x_min, primary_height - rect.y_max), size);

    PANEL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let glow = cell.get_or_insert_with(|| create_panel(mtm));
        let panel = &glow.panel;

        let generation = GENERATION.get().wrapping_add(1);
        GENERATION.set(generation);

        glow.view.setImage(Some(&image));
        panel.setFrame_display(frame, false);

        // full opacity at once; an animation group without duration also
        // replaces a fade out that is still running
        let reset = {
            let panel = panel.clone();
            RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
                // SAFETY: AppKit passes a valid context
                unsafe { ctx.as_ref() }.setDuration(0.0);
                panel.animator().setAlphaValue(1.0);
            })
        };
        NSAnimationContext::runAnimationGroup(&reset);
        panel.orderFrontRegardless();

        let fade = {
            let panel = panel.clone();
            RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
                // SAFETY: AppKit passes a valid context
                let ctx = unsafe { ctx.as_ref() };
                ctx.setDuration(FADE_OUT_SECS);
                // SAFETY: extern static of QuartzCore
                let ease_in = CAMediaTimingFunction::functionWithName(unsafe {
                    kCAMediaTimingFunctionEaseIn
                });
                ctx.setTimingFunction(Some(&ease_in));
                panel.animator().setAlphaValue(0.0);
            })
        };
        let done = {
            let panel = panel.clone();
            RcBlock::new(move || {
                // a newer flash took over the panel
                if GENERATION.get() == generation {
                    panel.orderOut(None);
                }
            })
        };
        NSAnimationContext::runAnimationGroup_completionHandler(&fade, Some(&done));
    });
}

fn create_panel(mtm: MainThreadMarker) -> GlowPanel {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        mtm.alloc(),
        rect,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the panel is owned by us and never closed
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(true);
    panel.setHidesOnDeactivate(false);
    panel.setLevel(NSScreenSaverWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

    let view = NSImageView::initWithFrame(mtm.alloc(), rect);
    view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    panel.setContentView(Some(&view));
    GlowPanel { panel, view }
}

/// Image of `size` points from premultiplied RGBA pixels.
fn make_image(
    pixels: &[u8],
    width: usize,
    height: usize,
    size: NSSize,
) -> Option<Retained<NSImage>> {
    // SAFETY: no planes are passed, the representation allocates its own buffer
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            (width * 4) as isize,
            32,
        )
    }?;
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let stride = rep.bytesPerRow() as usize;
    if stride < width * 4 || pixels.len() < width * height * 4 {
        return None;
    }
    for row in 0..height {
        // SAFETY: the buffer holds `height` rows of `stride` bytes
        unsafe {
            copy_nonoverlapping(
                pixels.as_ptr().add(row * width * 4),
                data.add(row * stride),
                width * 4,
            );
        }
    }
    // pixel size -> size in points
    rep.setSize(size);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    image.addRepresentation(&rep);
    Some(image)
}
