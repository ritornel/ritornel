//! The system's own context menu on a Mac: an `NSMenu` popped up over
//! GPUI's view. Its tracking loop is modal, so it is opened from the run
//! loop after the frame that asked for it has been drawn, when nothing of
//! GPUI's is borrowed: opened inside GPUI's event handler, any event GPUI
//! was sent during tracking would find the app already borrowed.

// AppKit is reached through `unsafe`, each block saying why it is sound.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};

use futures::channel::oneshot;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
use objc2_foundation::{NSPoint, NSString};

use super::MenuItem;

thread_local! {
    /// The menu open now, for a script to put away.
    static OPEN: Cell<Option<Retained<NSMenu>>> = const { Cell::new(None) };
}

/// Puts away the menu that is open, as Esc would.
pub(super) fn dismiss() {
    if let Some(menu) = OPEN.with(Cell::take) {
        menu.cancelTracking();
    }
}

struct Request {
    view: Retained<NSView>,
    items: Vec<MenuItem>,
    /// The action the menu opens with under the pointer, as a pop-up button's does.
    under: Option<usize>,
    at: NSPoint,
    answer: oneshot::Sender<Option<usize>>,
}

#[derive(Default)]
struct Ivars {
    request: RefCell<Option<Request>>,
    chosen: Cell<Option<isize>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RitornelSpikeMenu"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        #[unsafe(method(chosen:))]
        fn chosen(&self, item: &NSMenuItem) {
            self.ivars().chosen.set(Some(item.tag()));
        }

        /// Runs from the run loop, not from GCD's main queue, so that
        /// GPUI's tasks, which are blocks on that queue, go on running
        /// while the menu tracks.
        #[unsafe(method(show))]
        fn show(&self) {
            let Some(request) = self.ivars().request.borrow_mut().take() else { return };
            let mtm = self.mtm();
            let mut numbered = 0;
            let menu = self.menu(&request.items, &mut numbered, mtm);
            OPEN.with(|open| open.set(Some(menu.clone())));
            let opened = std::time::Instant::now();
            let positioned = request.under.and_then(|index| menu.itemWithTag(index as isize));
            menu.popUpMenuPositioningItem_atLocation_inView(positioned.as_deref(), request.at, Some(&request.view));
            OPEN.with(Cell::take);
            crate::debug(format_args!("menu: tracked for {} ms", opened.elapsed().as_millis()));
            let chosen = self.ivars().chosen.take().and_then(|tag| usize::try_from(tag).ok());
            let _ = request.answer.send(chosen);
        }
    }
);

impl Target {
    /// A menu of `items`, its actions numbered on from `numbered`.
    fn menu(
        &self,
        items: &[MenuItem],
        numbered: &mut usize,
        mtm: MainThreadMarker,
    ) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        for item in items {
            match item {
                MenuItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                MenuItem::Action {
                    title,
                    enabled,
                    checked,
                } => {
                    // SAFETY: a fresh item, with a selector its target implements.
                    let entry = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(title),
                            Some(sel!(chosen:)),
                            &NSString::from_str(""),
                        )
                    };
                    entry.setTag(*numbered as isize);
                    *numbered += 1;
                    entry.setEnabled(*enabled);
                    if *checked {
                        entry.setState(objc2_app_kit::NSControlStateValueOn);
                    }
                    // SAFETY: this object outlives the menu's tracking.
                    unsafe { entry.setTarget(Some(self)) };
                    menu.addItem(&entry);
                }
                MenuItem::Submenu { title, items } => {
                    // SAFETY: a fresh item with no action: it opens its submenu.
                    let entry = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(title),
                            None,
                            &NSString::from_str(""),
                        )
                    };
                    let sub = self.menu(items, numbered, mtm);
                    sub.setTitle(&NSString::from_str(title));
                    entry.setSubmenu(Some(&sub));
                    menu.addItem(&entry);
                }
            }
        }
        menu
    }
}

/// Shows the menu at `x`, `y` (points from the view's top left). The
/// index of the item chosen, if any, once it has closed.
pub(super) async fn pop_up(
    view: Retained<NSView>,
    items: Vec<MenuItem>,
    under: Option<usize>,
    x: f32,
    y: f32,
) -> Option<usize> {
    let mtm = MainThreadMarker::new()?;
    let height = view.frame().size.height;
    let y = if view.isFlipped() {
        f64::from(y)
    } else {
        height - f64::from(y)
    };
    let (answer, answered) = oneshot::channel();
    let request = Request {
        view,
        items,
        under,
        at: NSPoint::new(f64::from(x), y),
        answer,
    };
    let target = Target::alloc(mtm).set_ivars(Ivars {
        request: RefCell::new(Some(request)),
        ..Ivars::default()
    });
    // SAFETY: NSObject's init.
    let target: Retained<Target> = unsafe { msg_send![super(target), init] };
    // SAFETY: `show` takes no argument; the run loop retains the target until it runs.
    let _: () = unsafe {
        msg_send![&*target, performSelector: sel!(show), withObject: None::<&AnyObject>, afterDelay: 0.0f64]
    };
    answered.await.ok().flatten()
}
