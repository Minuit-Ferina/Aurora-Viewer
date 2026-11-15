//! AppKit's Quit command enters the viewer's app-wide exit workflow.
//!
//! The Legacy Viewer uses applicationShouldTerminate: (handleQuit,
//! indra/newview/llappdelegate-objc.mm). The menu target here lets winit
//! keep ownership of AppKit's application delegate.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::NSApplication;

static REQUESTED: AtomicBool = AtomicBool::new(false);

define_class!(
    // SAFETY: NSObject has no subclassing requirements and QuitTarget has
    // no custom destruction. AppKit calls menu actions on the main thread.
    #[unsafe(super = NSObject)]
    #[name = "AuroraApplicationQuitTarget"]
    #[thread_kind = MainThreadOnly]
    struct QuitTarget;

    impl QuitTarget {
        #[unsafe(method(auroraRequestQuit:))]
        fn request_quit(&self, _sender: &AnyObject) {
            REQUESTED.store(true, Ordering::Relaxed);
        }
    }
);

thread_local! {
    // NSMenuItem holds a non-owning reference to its target.
    static TARGET: OnceCell<Retained<QuitTarget>> = const { OnceCell::new() };
}

pub fn take_request() -> bool {
    REQUESTED.swap(false, Ordering::Relaxed)
}

pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("macOS menu: Quit setup must run on the main thread");
        return;
    };
    let Some(bar) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return;
    };
    for i in 0..bar.numberOfItems() {
        let Some(menu) = bar.itemAtIndex(i).and_then(|item| item.submenu()) else {
            continue;
        };
        for j in 0..menu.numberOfItems() {
            let Some(item) = menu.itemAtIndex(j) else {
                continue;
            };
            if item.action() == Some(sel!(terminate:)) {
                TARGET.with(|target| {
                    // SAFETY: QuitTarget inherits NSObject's init. The
                    // retained object outlives the menu's use of its selector.
                    let target = target.get_or_init(|| unsafe { msg_send![QuitTarget::alloc(mtm), init] });
                    // SAFETY: the selector is implemented by the retained
                    // target above with AppKit's one-argument action signature.
                    unsafe {
                        item.setTarget(Some(&**target));
                        item.setAction(Some(sel!(auroraRequestQuit:)));
                    }
                });
                return;
            }
        }
    }
}
