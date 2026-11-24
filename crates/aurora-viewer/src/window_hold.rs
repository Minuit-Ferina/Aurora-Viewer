//! Frames while Windows holds the window.
//!
//! A mouse button pressed on the window frame (title bar, border, close /
//! maximize / minimize button) and the system menu are followed by Windows
//! in a message loop of its own, inside the handling of the message that
//! started it. winit's loop does not turn for as long as it lasts:
//! `about_to_wait` is not called, the redraw it asks for each frame never
//! comes, and the viewer showed a still image until the button was released
//! or the menu closed.
//!
//! Those loops do not all deliver paint messages (the one of the title bar
//! buttons reads mouse messages only), but all of them deliver the messages
//! *sent* from another thread. For as long as the hold lasts, a helper
//! thread asks for an immediate repaint of the window, which winit turns
//! into a `RedrawRequested` on the main thread, as for any other frame.
//! Outside a hold the helper sleeps and nothing changes.
//!
//! The main thread must stay free for that loop: Windows handles one mouse
//! message between two frames, and a window dragged with a fast mouse trails
//! far behind the cursor if frames follow one another. So during a hold the
//! frame limiter does not wait on the main thread ([`paces`]): the helper
//! waits instead, between two requests, and it spaces the frames out while
//! the cursor moves ([`pause`]).
//!
//! A window outside the viewer's (outside.rs) is on the same thread and is
//! held the same way: its procedure is hooked too ([`follow`]), and the
//! repaint asked stays the main window's, whose frame draws them all.
//!
//! `AURORA_WINDOW_HOLD=0` turns it off, to compare.

use std::time::Duration;

use winit::window::Window;

/// While the cursor moves, the main thread draws at most one part of the
/// time in this many, and not more often than [`MOVING_MIN`].
const MOVING_SHARE: u32 = 5;
const MOVING_MIN: Duration = Duration::from_millis(16);
/// A request answered faster than this drew nothing (window gone, frame
/// refused by winit).
const NOTHING_DRAWN: Duration = Duration::from_micros(500);

/// Wait of the helper after a frame that kept the main thread for `work`:
/// the rest of the limiter's `period` (zero: no limit) while the cursor is
/// still, enough to leave the main thread to Windows while it moves.
#[cfg_attr(not(windows), allow(dead_code))]
fn pause(work: Duration, period: Duration, moving: bool) -> Duration {
    let pause = if moving {
        (work * (MOVING_SHARE - 1)).max(MOVING_MIN)
    } else {
        period.saturating_sub(work)
    };
    if work < NOTHING_DRAWN {
        pause.max(Duration::from_millis(2))
    } else {
        pause
    }
}

/// Frame limiter, on the main thread: `true` while Windows holds the window.
/// The helper then waits the `period` of the limiter (`None`: no limit)
/// between two frames, and the caller must not wait.
pub fn paces(period: Option<Duration>) -> bool {
    #[cfg(windows)]
    return imp::paces(period.unwrap_or_default());
    #[cfg(not(windows))]
    {
        let _ = period;
        false
    }
}

/// Called for every main window (at start and after a GPU recovery), on
/// the thread that created it.
pub fn install(window: &dyn Window) {
    #[cfg(windows)]
    if std::env::var("AURORA_WINDOW_HOLD").as_deref() != Ok("0") {
        imp::install(window);
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// Follow a detached panel's resize loop with the same synchronous redraws
/// as the main window. Called on the window thread by its chrome subclass.
#[cfg(windows)]
pub(crate) fn native_resize(hwnd: windows_sys::Win32::Foundation::HWND, active: bool) {
    imp::native_resize(hwnd as isize, active);
}

/// Whether the main window is inside a native resize gesture.
pub(crate) fn root_resizing(window: &dyn Window) -> bool {
    #[cfg(windows)]
    return hwnd(window).is_some_and(imp::root_resizing);
    #[cfg(not(windows))]
    {
        let _ = window;
        false
    }
}

/// Whether this panel is inside the native resize gesture that the helper follows.
pub(crate) fn native_resizing(window: &dyn Window) -> bool {
    #[cfg(windows)]
    return hwnd(window).is_some_and(imp::native_resizing);
    #[cfg(not(windows))]
    {
        let _ = window;
        false
    }
}

#[cfg(windows)]
fn hwnd(window: &dyn Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Called for every window outside the viewer's (outside.rs), on the thread
/// that created it: Windows holding its frame is a hold too. The frames
/// stay those of the main window, which draw the windows outside with it.
pub fn follow(window: &dyn Window) {
    #[cfg(windows)]
    if std::env::var("AURORA_WINDOW_HOLD").as_deref() != Ok("0") {
        imp::follow(window);
    }
    #[cfg(not(windows))]
    let _ = window;
}

#[cfg(windows)]
mod imp {
    use std::{
        sync::{
            OnceLock,
            atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        },
        thread::{self, Thread},
        time::{Duration, Instant},
    };

    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{RDW_INTERNALPAINT, RDW_UPDATENOW, RedrawWindow},
        UI::WindowsAndMessaging::{
            CallWindowProcW, GWLP_WNDPROC, GetCursorPos, SC_MOVE, SC_SIZE, SetWindowLongPtrW, WM_ENTERMENULOOP, WM_EXITMENULOOP,
            WM_EXITSIZEMOVE, WM_NCDESTROY, WM_NCLBUTTONDOWN, WM_SIZING, WM_SYSCOMMAND, WNDPROC,
        },
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// Window procedure in place before ours (winit's, the same for all of
    /// its windows).
    static PREVIOUS: AtomicIsize = AtomicIsize::new(0);
    /// The window the helper repaints: the last one installed (a GPU
    /// recovery opens a new window, app/gpu_recovery.rs).
    static TARGET: AtomicIsize = AtomicIsize::new(0);
    /// The main window whose border is currently being resized.
    static ROOT_RESIZING: AtomicIsize = AtomicIsize::new(0);
    /// The detached window currently being resized, or zero between gestures.
    static NATIVE_TARGET: AtomicIsize = AtomicIsize::new(0);
    /// Messages being handled that Windows may follow in a loop of its own
    /// (one inside the other: a press on the title bar starts a move).
    static HOLDS: AtomicU32 = AtomicU32::new(0);
    /// The system menu is open.
    static MENU: AtomicBool = AtomicBool::new(false);
    static HELPER: OnceLock<Thread> = OnceLock::new();
    /// Period of the frame limiter, in microseconds (0: no limit).
    static PERIOD: AtomicU32 = AtomicU32::new(0);
    /// The cursor counts as moving for this long after its last move.
    const MOVING_FOR: Duration = Duration::from_millis(150);

    fn root_held() -> bool {
        HOLDS.load(Ordering::Acquire) > 0 || MENU.load(Ordering::Acquire)
    }

    pub(super) fn root_resizing(hwnd: isize) -> bool {
        ROOT_RESIZING.load(Ordering::Acquire) == hwnd
    }

    pub(super) fn native_resizing(hwnd: isize) -> bool {
        NATIVE_TARGET.load(Ordering::Acquire) == hwnd
    }

    pub(super) fn native_resize(hwnd: isize, active: bool) {
        let Some(helper) = HELPER.get() else { return };
        if active {
            if NATIVE_TARGET.swap(hwnd, Ordering::Release) != hwnd {
                helper.unpark();
            }
        } else if NATIVE_TARGET
            .compare_exchange(hwnd, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            // SAFETY: Queue the final exact-size paint on this live window,
            // after WM_EXITSIZEMOVE, without re-entering the application.
            unsafe { RedrawWindow(hwnd as HWND, std::ptr::null(), std::ptr::null_mut(), RDW_INTERNALPAINT) };
        }
    }

    pub fn paces(period: Duration) -> bool {
        if !root_held() && NATIVE_TARGET.load(Ordering::Acquire) == 0 {
            return false;
        }
        PERIOD.store(period.as_micros().min(u32::MAX as u128) as u32, Ordering::Release);
        true
    }

    fn cursor() -> (i32, i32) {
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: `GetCursorPos` writes the point it is given.
        unsafe {
            GetCursorPos(&mut point);
        }
        (point.x, point.y)
    }

    pub fn install(window: &dyn winit::window::Window) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let hwnd = handle.hwnd.get();
        TARGET.store(hwnd, Ordering::Release);
        if HELPER.get().is_some() {
            subclass(hwnd);
            return;
        }
        let helper = thread::Builder::new().name("window-hold".into()).spawn(move || {
            let mut at = cursor();
            let mut moved = None;
            loop {
                let native = NATIVE_TARGET.load(Ordering::Acquire);
                if native == 0 && !root_held() {
                    thread::park();
                    moved = None;
                    continue;
                }
                let start = Instant::now();
                // Windows dispatches the synchronous paint to the window whose
                // border is held. Both it and the root paint the shared frame.
                let hwnd = if native != 0 { native } else { TARGET.load(Ordering::Acquire) };
                // SAFETY: `RedrawWindow` takes a window handle and flags; the
                // handle of a destroyed window makes it fail, nothing more.
                // The call returns once the window's thread has painted.
                unsafe {
                    RedrawWindow(
                        hwnd as HWND,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        RDW_INTERNALPAINT | RDW_UPDATENOW,
                    );
                }
                let work = start.elapsed();
                let now = cursor();
                if now != at {
                    at = now;
                    moved = Some(Instant::now());
                }
                let moving = moved.is_some_and(|t: Instant| t.elapsed() < MOVING_FOR);
                let period = if native != 0 {
                    super::MOVING_MIN
                } else {
                    Duration::from_micros(PERIOD.load(Ordering::Acquire).into())
                };
                let pause = super::pause(work, period, moving);
                if !pause.is_zero() {
                    thread::sleep(pause);
                }
            }
        });
        let Ok(helper) = helper else {
            return;
        };
        let _ = HELPER.set(helper.thread().clone());
        subclass(hwnd);
    }

    pub fn follow(window: &dyn winit::window::Window) {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        // (the helper is made with the main window: without it, no hold)
        if HELPER.get().is_some() {
            subclass(handle.hwnd.get());
        }
    }

    /// Our procedure in front of the window's. One previous procedure is kept
    /// for every window hooked: a window whose own differs (not winit's) is
    /// left as it was, without the hold.
    fn subclass(hwnd: isize) {
        // SAFETY: `hwnd` is a live window of this thread, and `hook` has the
        // signature of a window procedure. The previous procedure is stored
        // before any message can reach `hook`: the messages of this window
        // are only delivered on this thread.
        unsafe {
            let hook: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT = hook;
            let previous = SetWindowLongPtrW(hwnd as HWND, GWLP_WNDPROC, hook as usize as isize);
            if previous == 0 {
                // refused: the window keeps its own procedure
                log::warn!("window hold: the window procedure could not be replaced, left without the hold");
                return;
            }
            let kept = PREVIOUS.load(Ordering::Acquire);
            if kept != 0 && kept != previous {
                SetWindowLongPtrW(hwnd as HWND, GWLP_WNDPROC, previous);
                log::warn!("window hold: a window with another procedure, left without the hold");
                return;
            }
            PREVIOUS.store(previous, Ordering::Release);
        }
    }

    fn wake() {
        if let Some(helper) = HELPER.get() {
            helper.unpark();
        }
    }

    unsafe extern "system" fn hook(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: `PREVIOUS` holds the value `SetWindowLongPtrW` returned for
        // this window: a window procedure (or a handle to one, which
        // `CallWindowProcW` accepts), never 0 once the hook is installed.
        let previous: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(PREVIOUS.load(Ordering::Acquire)) };
        // a press on the frame, or a move / resize started from the keyboard
        // (the low four bits of a system command are not part of it)
        let hold = match msg {
            WM_NCLBUTTONDOWN => true,
            WM_SYSCOMMAND => matches!(wparam as u32 & 0xFFF0, SC_MOVE | SC_SIZE),
            _ => false,
        };
        match msg {
            WM_ENTERMENULOOP => {
                MENU.store(true, Ordering::Release);
                wake();
            }
            WM_EXITMENULOOP => MENU.store(false, Ordering::Release),
            WM_SIZING => ROOT_RESIZING.store(hwnd as isize, Ordering::Release),
            WM_EXITSIZEMOVE | WM_NCDESTROY => {
                let was_resizing = ROOT_RESIZING
                    .compare_exchange(hwnd as isize, 0, Ordering::Release, Ordering::Relaxed)
                    .is_ok();
                if was_resizing && msg == WM_EXITSIZEMOVE {
                    // SAFETY: Schedule the final exact-size frame on the live
                    // window without re-entering its paint handler.
                    unsafe { RedrawWindow(hwnd, std::ptr::null(), std::ptr::null_mut(), RDW_INTERNALPAINT) };
                }
            }
            _ => {}
        }
        if hold {
            HOLDS.fetch_add(1, Ordering::AcqRel);
            wake();
        }
        // SAFETY: the arguments are those Windows gave for this message.
        // For a hold, the call returns when Windows lets go of the window.
        let result = unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) };
        if hold {
            HOLDS.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_still_cursor_keeps_the_pace_of_the_limiter() {
        // 120 fps: a frame of 1 ms leaves the rest of the period
        let period = Duration::from_micros(8_333);
        assert_eq!(pause(MS, period, false), period - MS);
        // a frame longer than the period, or no limit: the next one at once
        assert_eq!(pause(20 * MS, period, false), Duration::ZERO);
        assert_eq!(pause(MS, Duration::ZERO, false), Duration::ZERO);
    }

    #[test]
    fn a_moving_cursor_leaves_the_main_thread_to_windows() {
        // short frames: not more often than every 16 ms, whatever the limit
        assert_eq!(pause(MS, Duration::from_micros(6_060), true), MOVING_MIN);
        assert_eq!(pause(MS, Duration::ZERO, true), MOVING_MIN);
        // long frames: one part of the time in five
        assert_eq!(pause(20 * MS, Duration::from_micros(8_333), true), 80 * MS);
    }

    #[test]
    fn a_request_that_drew_nothing_does_not_spin() {
        assert_eq!(pause(Duration::from_micros(50), Duration::ZERO, false), 2 * MS);
        assert_eq!(pause(Duration::from_micros(50), 8 * MS, false), 8 * MS - Duration::from_micros(50));
    }
}
