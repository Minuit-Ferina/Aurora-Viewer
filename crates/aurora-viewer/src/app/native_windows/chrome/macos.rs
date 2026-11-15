//! AppKit operations that preserve the borderless window's style and focus.

use std::cell::Cell;

use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSView, NSWindow};
use objc2_core_foundation::CGRect;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    MainThreadMarker::new()?;
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(raw) = handle.as_raw() else {
        return None;
    };
    // SAFETY: the borrowed winit handle keeps its AppKit view alive and this
    // code runs on the AppKit main thread.
    let view = unsafe { &*raw.ns_view.as_ptr().cast::<NSView>() };
    view.window()
}

#[derive(Default)]
pub(super) struct Zoom {
    restore_frame: Cell<Option<CGRect>>,
}

impl Zoom {
    // winit's is_maximized and set_maximized temporarily add the titled
    // style to a borderless NSWindow, which can interrupt input and dragging.
    pub(super) fn is_maximized(&self) -> bool {
        self.restore_frame.get().is_some()
    }

    pub(super) fn toggle(&self, window: &Window) {
        let Some(window) = ns_window(window) else { return };
        if let Some(frame) = self.restore_frame.take() {
            window.setFrame_display(frame, true);
        } else if let Some(screen) = window.screen() {
            self.restore_frame.set(Some(window.frame()));
            window.setFrame_display(screen.visibleFrame(), true);
        }
    }
}
