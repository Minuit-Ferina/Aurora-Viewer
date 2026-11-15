//! Let Windows handle moving and sizing the compact client-side title bar.

use std::{cell::Cell, rc::Rc};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::ScreenToClient,
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GetClientRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsZoomed,
            WM_NCDESTROY, WM_NCHITTEST,
        },
    },
};
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

const SUBCLASS_ID: usize = 1;
const EDGE: f64 = 5.0;

#[derive(Clone, Copy, Default)]
struct Geometry {
    caption_right: i32,
    caption_bottom: i32,
    resize_border: i32,
}

pub(super) struct Caption(Rc<Cell<Geometry>>);

impl Caption {
    pub(super) fn install(window: &Window) -> Option<Self> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return None;
        };
        let hwnd = handle.hwnd.get() as HWND;
        let geometry = Rc::new(Cell::new(Geometry::default()));
        let reference = Rc::into_raw(Rc::clone(&geometry));
        // SAFETY: The event loop owns this HWND on the current thread. The
        // subclass owns one Rc reference until it receives WM_NCDESTROY.
        let installed = unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, reference as usize) };
        if installed == 0 {
            // SAFETY: No callback took ownership of this reference.
            drop(unsafe { Rc::from_raw(reference) });
            return None;
        }
        Some(Self(geometry))
    }

    pub(super) fn update(&self, window: &Window, pixels_per_point: f32, controls_left: f32, bottom: f32) {
        self.0.set(Geometry {
            caption_right: (controls_left * pixels_per_point).floor() as i32,
            caption_bottom: (bottom * pixels_per_point).ceil() as i32,
            resize_border: (EDGE * window.scale_factor()).ceil() as i32,
        });
    }
}

unsafe extern "system" fn subclass(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, id: usize, reference: usize) -> LRESULT {
    let geometry = reference as *const Cell<Geometry>;
    if message == WM_NCDESTROY {
        // SAFETY: This callback is installed once per window and is being
        // removed on its owning thread before releasing its one Rc reference.
        unsafe {
            RemoveWindowSubclass(hwnd, Some(subclass), id);
            drop(Rc::from_raw(geometry));
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
    }
    if message == WM_NCHITTEST {
        let mut point = screen_point(lparam);
        let mut client = RECT::default();
        // SAFETY: Windows supplied a live HWND and both outputs are local.
        if unsafe { ScreenToClient(hwnd, &mut point) != 0 && GetClientRect(hwnd, &mut client) != 0 } {
            // SAFETY: The subclass owns the reference on this thread until WM_NCDESTROY.
            let geometry = unsafe { (*geometry).get() };
            // SAFETY: The HWND is live for the duration of this callback.
            let maximized = unsafe { IsZoomed(hwnd) != 0 };
            if let Some(hit) = hit_test(geometry, point, client, maximized) {
                return hit as LRESULT;
            }
        }
    }
    // SAFETY: All unhandled messages continue along the same subclass chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn screen_point(lparam: LPARAM) -> POINT {
    // WM_NCHITTEST packs signed screen coordinates. Monitors can be left of
    // or above the primary display.
    POINT {
        x: i32::from(lparam as i16),
        y: i32::from((lparam >> 16) as i16),
    }
}

fn hit_test(geometry: Geometry, point: POINT, client: RECT, maximized: bool) -> Option<u32> {
    let POINT { x, y } = point;
    if x < client.left || x >= client.right || y < client.top || y >= client.bottom {
        return None;
    }
    if !maximized {
        let edge = geometry.resize_border;
        let border = match (
            x < client.left + edge,
            x >= client.right - edge,
            y < client.top + edge,
            y >= client.bottom - edge,
        ) {
            (true, _, true, _) => Some(HTTOPLEFT),
            (_, true, true, _) => Some(HTTOPRIGHT),
            (true, _, _, true) => Some(HTBOTTOMLEFT),
            (_, true, _, true) => Some(HTBOTTOMRIGHT),
            (true, _, _, _) => Some(HTLEFT),
            (_, true, _, _) => Some(HTRIGHT),
            (_, _, true, _) => Some(HTTOP),
            (_, _, _, true) => Some(HTBOTTOM),
            _ => None,
        };
        if border.is_some() {
            return border;
        }
    }
    (x < geometry.caption_right && y < geometry.caption_bottom).then_some(HTCAPTION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_caption_preserves_controls_and_resize_borders() {
        let geometry = Geometry {
            caption_right: 305,
            caption_bottom: 30,
            resize_border: 5,
        };
        let client = RECT {
            left: 0,
            right: 420,
            top: 0,
            bottom: 270,
        };
        let at = |x, y, maximized| hit_test(geometry, POINT { x, y }, client, maximized);
        assert_eq!(at(0, 0, false), Some(HTTOPLEFT));
        assert_eq!(at(419, 269, false), Some(HTBOTTOMRIGHT));
        assert_eq!(at(419, 12, false), Some(HTRIGHT));
        assert_eq!(at(145, 12, false), Some(HTCAPTION));
        assert_eq!(at(290, 28, false), Some(HTCAPTION));
        assert_eq!(at(305, 12, false), None);
        assert_eq!(at(393, 12, false), None);
        assert_eq!(at(145, 30, false), None);
        assert_eq!(at(-1, 0, false), None);
        assert_eq!(at(420, 269, false), None);
        assert_eq!(at(0, 0, true), Some(HTCAPTION));
        assert_eq!(at(419, 12, true), None);
    }

    #[test]
    fn win32_caption_decodes_screen_coordinates_on_negative_monitors() {
        let packed = ((-660_i16) as u16 as isize) | (((-312_i16) as u16 as isize) << 16);
        let POINT { x, y } = screen_point(packed);
        assert_eq!((x, y), (-660, -312));
    }
}
