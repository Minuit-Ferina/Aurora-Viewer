//! Desktop placement is independent of the embedded floater layout.

#[cfg(windows)]
use winit::dpi::PhysicalSize;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::Window;

#[derive(Clone, Copy, serde::Deserialize, serde::Serialize)]
pub(super) struct Geometry {
    monitor: [i32; 2],
    offset: [i32; 2],
    logical_size: [u32; 2],
}

#[derive(Clone, Copy)]
struct Area {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    scale: f64,
}

impl Geometry {
    pub(super) fn from_window(window: &dyn Window, maximized: bool) -> Option<Self> {
        if maximized || window.is_minimized() == Some(true) {
            return None;
        }
        let monitor = window.current_monitor()?;
        let area = Area::of(&monitor)?;
        let position = window.outer_position().ok()?;
        let size = window.surface_size();
        if size.width == 0 || size.height == 0 {
            return None;
        }
        let origin = monitor.position()?;
        Some(Self {
            monitor: [origin.x, origin.y],
            offset: [position.x.saturating_sub(area.left), position.y.saturating_sub(area.top)]
                .map(|pixels| (f64::from(pixels) / area.scale).round() as i32),
            logical_size: [size.width, size.height].map(|pixels| (f64::from(pixels) / window.scale_factor()).round() as u32),
        })
    }

    pub(super) fn restore(self, event_loop: &dyn ActiveEventLoop) -> Option<(PhysicalPosition<i32>, LogicalSize<f64>)> {
        let monitor = event_loop
            .available_monitors()
            .find(|monitor| monitor.position().is_some_and(|p| [p.x, p.y] == self.monitor))
            .or_else(|| event_loop.primary_monitor())
            .or_else(|| event_loop.available_monitors().next())?;
        Some(self.fit(Area::of(&monitor)?))
    }

    fn fit(self, area: Area) -> (PhysicalPosition<i32>, LogicalSize<f64>) {
        let logical = LogicalSize::new(
            f64::from(self.logical_size[0].max(200)).min(f64::from(area.width.saturating_sub(16).max(1)) / area.scale),
            f64::from(self.logical_size[1].max(120)).min(f64::from(area.height.saturating_sub(16).max(1)) / area.scale),
        );
        let size = logical.to_physical::<u32>(area.scale);
        let clamp = |origin: i32, offset: i32, available: u32, window: u32| {
            let maximum = available.saturating_sub(window).saturating_sub(8).min(i32::MAX as u32) as i32;
            origin.saturating_add(((f64::from(offset) * area.scale).round() as i32).clamp(0, maximum))
        };
        (
            PhysicalPosition::new(
                clamp(area.left, self.offset[0], area.width, size.width),
                clamp(area.top, self.offset[1], area.height, size.height),
            ),
            logical,
        )
    }
}

impl Area {
    fn of(monitor: &MonitorHandle) -> Option<Self> {
        #[cfg(windows)]
        if let Some((position, size)) = windows_work_area(monitor) {
            return Some(Self {
                left: position.x,
                top: position.y,
                width: size.width,
                height: size.height,
                scale: monitor.scale_factor(),
            });
        }
        let position = monitor.position()?;
        let size = monitor.current_video_mode()?.size();
        Some(Self {
            left: position.x,
            top: position.y,
            width: size.width,
            height: size.height,
            scale: monitor.scale_factor(),
        })
    }
}

#[cfg(windows)]
fn windows_work_area(monitor: &MonitorHandle) -> Option<(PhysicalPosition<i32>, PhysicalSize<u32>)> {
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: This handle is from winit and the correctly sized structure is local.
    if unsafe { GetMonitorInfoW(monitor.native_id() as usize as _, &mut info) } == 0 {
        return None;
    }
    let work = info.rcWork;
    let width = u32::try_from(work.right.checked_sub(work.left)?).ok()?;
    let height = u32::try_from(work.bottom.checked_sub(work.top)?).ok()?;
    Some((PhysicalPosition::new(work.left, work.top), PhysicalSize::new(width, height)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_an_oversized_window_inside_current_negative_work_area() {
        let saved = Geometry {
            monitor: [-1440, 0],
            offset: [i32::MAX, -2048],
            logical_size: [u32::MAX, 420],
        };
        let area = Area {
            left: -1440,
            top: 48,
            width: 1440,
            height: 800,
            scale: 1.25,
        };
        let (position, size) = saved.fit(area);
        assert!(position.x < 0);
        assert_eq!(position.y, 48);
        let size = size.to_physical::<u32>(area.scale);
        assert!(i64::from(position.x) + i64::from(size.width) <= 0);
        assert!(size.height + 48 <= 848);
    }
}
