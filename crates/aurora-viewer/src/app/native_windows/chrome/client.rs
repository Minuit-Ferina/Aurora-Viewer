//! Hand caption moves and Linux edge resizing to the window system.

use std::cell::Cell;
use std::time::{Duration, Instant};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::window::Window;

use super::EDGE;

#[derive(Default)]
pub(super) struct Caption {
    extent: Cell<(f64, f64)>,
    pointer: Option<PhysicalPosition<f64>>,
    last_press: Option<(Instant, PhysicalPosition<f64>)>,
    #[cfg(target_os = "macos")]
    zoom: super::macos::Zoom,
}

impl Caption {
    pub(super) fn update(&self, pixels_per_point: f32, controls_left: f32, bottom: f32) {
        self.extent.set((
            f64::from((controls_left * pixels_per_point).floor()),
            f64::from((bottom * pixels_per_point).ceil()),
        ));
    }

    pub(super) fn event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        match *event {
            WindowEvent::CursorMoved { position, .. } => self.pointer = Some(position),
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            WindowEvent::Moved(_) | WindowEvent::Focused(false) => self.last_press = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => return self.press(window),
            _ => {}
        }
        false
    }

    fn press(&mut self, window: &Window) -> bool {
        let Some(point) = self.pointer else { return false };
        let previous = self.last_press.take();
        #[cfg(target_os = "macos")]
        let maximized = self.is_maximized();
        #[cfg(target_os = "linux")]
        let maximized = window.is_maximized();
        let border = if maximized { 0.0 } else { (EDGE * window.scale_factor()).ceil() };
        #[cfg(target_os = "linux")]
        if window.is_resizable()
            && let Some(direction) = resize_direction(point, window.inner_size(), border)
        {
            return window.drag_resize_window(direction).is_ok();
        }
        // AppKit owns resizing on the edges of its resizable borderless window.
        // Use the press itself: Cocoa needs its mouse event and Wayland its serial.
        let (right, bottom) = self.extent.get();
        if point.x < border || point.y < border || point.x >= right || point.y >= bottom {
            return false;
        }
        let now = Instant::now();
        if previous.is_some_and(|(at, then)| {
            now.duration_since(at) <= Duration::from_millis(350)
                && (point.x - then.x).hypot(point.y - then.y) <= EDGE * window.scale_factor()
        }) {
            #[cfg(target_os = "macos")]
            self.toggle_maximized(window);
            #[cfg(target_os = "linux")]
            window.set_maximized(!maximized);
            return true;
        }
        self.last_press = Some((now, point));
        window.drag_window().is_ok()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn is_maximized(&self) -> bool {
        self.zoom.is_maximized()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn toggle_maximized(&self, window: &Window) {
        self.zoom.toggle(window);
    }

    #[cfg(target_os = "linux")]
    pub(super) fn resize_cursor(&self, window: &Window) -> Option<egui::CursorIcon> {
        use egui::CursorIcon;
        use winit::window::ResizeDirection;
        let point = self.pointer?;
        if !window.is_resizable() || window.is_maximized() {
            return None;
        }
        let border = (EDGE * window.scale_factor()).ceil();
        Some(match resize_direction(point, window.inner_size(), border)? {
            ResizeDirection::North => CursorIcon::ResizeNorth,
            ResizeDirection::South => CursorIcon::ResizeSouth,
            ResizeDirection::East => CursorIcon::ResizeEast,
            ResizeDirection::West => CursorIcon::ResizeWest,
            ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
            ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
            ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
            ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
        })
    }
}

#[cfg(target_os = "linux")]
fn resize_direction(
    point: PhysicalPosition<f64>,
    size: winit::dpi::PhysicalSize<u32>,
    border: f64,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection::*;
    let (width, height) = (f64::from(size.width), f64::from(size.height));
    if border <= 0.0 || !(0.0..width).contains(&point.x) || !(0.0..height).contains(&point.y) {
        return None;
    }
    let left = point.x < border;
    let right = point.x >= width - border;
    let top = point.y < border;
    let bottom = point.y >= height - border;
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => NorthWest,
        (_, true, true, _) => NorthEast,
        (true, _, _, true) => SouthWest,
        (_, true, _, true) => SouthEast,
        (_, _, true, _) => North,
        (_, _, _, true) => South,
        (true, _, _, _) => West,
        (_, true, _, _) => East,
        _ => return None,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use winit::dpi::PhysicalSize;
    use winit::window::ResizeDirection;

    #[test]
    fn linux_caption_resize_hits_edges_and_corners_at_display_scale() {
        let size = PhysicalSize::new(800, 600);
        let hit = |x, y| resize_direction(PhysicalPosition::new(x, y), size, 10.0);
        assert_eq!(hit(2.0, 3.0), Some(ResizeDirection::NorthWest));
        assert_eq!(hit(796.0, 595.0), Some(ResizeDirection::SouthEast));
        assert_eq!(hit(400.0, 595.0), Some(ResizeDirection::South));
        assert_eq!(hit(796.0, 300.0), Some(ResizeDirection::East));
        assert_eq!(hit(780.0, 14.0), None);
        assert_eq!(hit(-1.0, 30.0), None);
    }
}
