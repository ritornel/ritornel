//! What the Settings window asks of AppKit, as the Tauri shell does it
//! (`src-tauri/src/settings.rs`): its height fitted to the section shown,
//! keeping its top where it is, and the system's folder picker as a sheet
//! on it. Also the language a page's `Intl` would write in, and an
//! appearance held for a developer's pictures.
//!
//! Each is run from the run loop rather than inside GPUI's call: a frame
//! animated turns the run loop, and a sheet makes the window resign key,
//! and either would call into GPUI while it is borrowed.

// AppKit is reached through `unsafe`, each block saying why it is sound.
#![allow(unsafe_code)]

use std::cell::Cell;
use std::path::PathBuf;

use block2::RcBlock;
use futures::channel::oneshot;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSModalResponseOK, NSOpenPanel, NSWindow,
};
use objc2_foundation::{NSLocale, NSPoint, NSRect, NSRunLoop, NSSize, NSString};

/// Runs `work` from the main run loop, when nothing of GPUI's is borrowed.
fn soon(work: impl Fn() + 'static) {
    let block = RcBlock::new(work);
    // SAFETY: the run loop copies the block and runs it once on the main thread.
    unsafe { NSRunLoop::mainRunLoop().performBlock(&block) };
}

thread_local! {
    /// Whether a fit is moving a window now.
    static FITTING: Cell<bool> = const { Cell::new(false) };
    /// The height asked for meanwhile, which the fit moves on to.
    static NEXT: Cell<Option<f64>> = const { Cell::new(None) };
}

/// Makes the window's content `height` points tall, keeping its top where
/// it is, as AppKit moves between a settings window's sections: animated
/// once the window is on screen, at once before. A window not yet shown is
/// shown then, taking the keyboard unless `quietly`.
pub(super) fn fit(window: Retained<NSWindow>, height: f64, quietly: bool) {
    soon(move || {
        // The animation turns the run loop, so a fit asked for while it
        // runs arrives inside it: it is kept for when it ends, rather
        // than undone by it.
        if FITTING.get() {
            NEXT.set(Some(height));
            return;
        }
        FITTING.set(true);
        let mut asked = Some(height);
        while let Some(height) = asked {
            resize(&window, height);
            asked = NEXT.take();
        }
        FITTING.set(false);
        if !window.isVisible() {
            if quietly {
                window.orderFrontRegardless();
            } else {
                window.makeKeyAndOrderFront(None);
            }
        }
    });
}

fn resize(window: &NSWindow, height: f64) {
    let frame = window.frame();
    let content = window.contentRectForFrameRect(frame);
    if (content.size.height - height).abs() < 0.5 {
        return;
    }
    let wanted = window.frameRectForContentRect(NSRect::new(
        content.origin,
        NSSize::new(content.size.width, height),
    ));
    // AppKit counts from the bottom: the top stays where it is.
    let top = frame.origin.y + frame.size.height;
    let to = NSRect::new(
        NSPoint::new(frame.origin.x, top - wanted.size.height),
        NSSize::new(frame.size.width, wanted.size.height),
    );
    window.setFrame_display_animate(to, true, window.isVisible());
}

/// Asks for a folder with the system's picker, as a sheet on `window`: its
/// path, or `None` if it was cancelled.
pub(super) async fn choose_folder(
    window: Retained<NSWindow>,
    prompt: String,
    message: String,
) -> Option<PathBuf> {
    let (send, chosen) = oneshot::channel();
    let send = Cell::new(Some(send));
    soon(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setAllowsMultipleSelection(false);
        panel.setCanCreateDirectories(false);
        panel.setPrompt(Some(&NSString::from_str(&prompt)));
        panel.setMessage(Some(&NSString::from_str(&message)));
        let answered = panel.clone();
        let send = Cell::new(send.take());
        let handler = RcBlock::new(move |response| {
            let path = (response == NSModalResponseOK)
                .then(|| answered.URL())
                .flatten()
                .and_then(|url| url.path())
                .map(|path| PathBuf::from(path.to_string()));
            if let Some(send) = send.take() {
                let _ = send.send(path);
            }
        });
        panel.beginSheetModalForWindow_completionHandler(&window, &handler);
    });
    chosen.await.ok().flatten()
}

/// The language a page's `Intl` writes in: the first the system prefers.
pub(super) fn locale() -> Option<String> {
    NSLocale::preferredLanguages()
        .firstObject()
        .map(|language| language.to_string())
}

/// Dresses the window in a light or a dark appearance whatever the
/// system's, for a developer's pictures: its buttons and title then match
/// the palette held.
pub(super) fn hold_appearance(window: &NSWindow, dark: bool) {
    // SAFETY: AppKit's own names for its two appearances.
    let name = unsafe {
        if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        }
    };
    let appearance = NSAppearance::appearanceNamed(name);
    window.setAppearance(appearance.as_deref());
}
