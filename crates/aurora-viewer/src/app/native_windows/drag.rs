//! Route an in-app drag to the viewer window under the pointer.
//! Winit's capture keeps delivering Windows drag events to the source HWND.

use std::sync::Arc;
use winit::dpi::PhysicalPosition;
use winit::window::{Window, WindowId};

use crate::ui::inventory::{drag::Cargo, task_drop::TaskDrag};
use crate::ui::places::drag::PlaceDrag;

enum Payload {
    Inventory(Cargo),
    Task(Arc<TaskDrag>),
    Place(Arc<PlaceDrag>),
}

impl Payload {
    #[cfg(windows)]
    fn carried(ctx: &egui::Context) -> bool {
        crate::ui::inventory::drag::carried_count(ctx).is_some()
            || egui::DragAndDrop::has_payload_of_type::<TaskDrag>(ctx)
            || egui::DragAndDrop::has_payload_of_type::<PlaceDrag>(ctx)
    }

    #[cfg(windows)]
    fn take(ctx: &egui::Context) -> Option<Self> {
        if egui::DragAndDrop::has_payload_of_type::<PlaceDrag>(ctx) {
            return egui::DragAndDrop::take_payload::<PlaceDrag>(ctx).map(Self::Place);
        }
        Cargo::take_desktop(ctx)
            .map(Self::Inventory)
            .or_else(|| egui::DragAndDrop::take_payload::<TaskDrag>(ctx).map(Self::Task))
    }

    fn present(&self, ctx: &egui::Context) {
        match self {
            Self::Inventory(cargo) => cargo.present(ctx),
            Self::Task(cargo) => egui::DragAndDrop::set_payload(ctx, (**cargo).clone()),
            Self::Place(cargo) => egui::DragAndDrop::set_payload(ctx, (**cargo).clone()),
        }
    }
}

pub(in crate::app) struct Input {
    point: PhysicalPosition<f64>,
    cargo: Arc<Payload>,
    modifiers: egui::Modifiers,
    released: bool,
}

impl Input {
    pub(in crate::app) fn add_events(&self, ctx: &egui::Context, window: &Window, raw: &mut egui::RawInput) {
        let scale = window.scale_factor() as f32 * ctx.zoom_factor();
        let point = egui::pos2(self.point.x as f32 / scale, self.point.y as f32 / scale);
        self.cargo.present(ctx);
        raw.events.push(egui::Event::ModifiersChanged(self.modifiers));
        raw.events.push(egui::Event::PointerMoved(point));
        if self.released {
            raw.events.push(egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: self.modifiers,
            });
        }
    }
}

#[derive(Default)]
pub(super) struct Session {
    current: Option<Transfer>,
}

struct Transfer {
    source: WindowId,
    cargo: Arc<Payload>,
    target: Option<(WindowId, PhysicalPosition<f64>, egui::Modifiers)>,
    #[cfg(windows)]
    capture_cursor: Option<win32::CapturedCursor>,
    released: bool,
}

impl Session {
    pub(super) fn cancel(&mut self) {
        self.current = None;
    }

    pub(super) fn forget_window(&mut self, window: WindowId) {
        if self
            .current
            .as_ref()
            .is_some_and(|transfer| transfer.source == window || transfer.target.is_some_and(|(target, _, _)| target == window))
        {
            self.cancel();
        }
    }

    #[cfg(windows)]
    pub(super) fn restore_capture_cursor(&self, source: WindowId) {
        if let Some(transfer) = &self.current
            && transfer.source == source
            && let Some(cursor) = &transfer.capture_cursor
        {
            cursor.apply();
        }
    }

    pub(super) fn frame(&mut self, window: WindowId) -> Option<Input> {
        let transfer = self.current.as_ref()?;
        #[cfg(windows)]
        if !transfer.released && transfer.capture_cursor.as_ref().is_some_and(|capture| !capture.has_capture()) {
            self.cancel();
            return None;
        }
        let (target, point, modifiers) = transfer.target?;
        if window != target {
            return None;
        }
        // Keep feeding the destination until the capture moves or ends;
        // egui's payload is scoped to each pass on the shared context.
        let input = Input {
            point,
            modifiers,
            cargo: Arc::clone(&transfer.cargo),
            released: transfer.released,
        };
        if transfer.released {
            self.cancel();
        }
        Some(input)
    }
}

#[cfg(windows)]
impl super::NativeWindows {
    /// The detached source still owns mouse capture while the viewer supplies
    /// the drop decision. egui-winit has no native pointer sample in the viewer.
    pub(in crate::app) fn root_drop_cursor(
        &mut self,
        root: WindowId,
        event_loop: &winit::event_loop::ActiveEventLoop,
        icon: egui::CursorIcon,
        image: Option<&egui::CustomCursorImage>,
    ) {
        let Some(transfer) = &mut self.cross_window_drag.current else {
            return;
        };
        if transfer.source != root
            && transfer.target.is_some_and(|(window, _, _)| window == root)
            && let Some(capture) = &mut transfer.capture_cursor
        {
            capture.update(event_loop, icon, image);
        }
    }

    pub(in crate::app) fn bridge_drag(
        &mut self,
        ctx: &egui::Context,
        root: &Arc<Window>,
        root_modifiers: egui::Modifiers,
        id: WindowId,
        event: &winit::event::WindowEvent,
    ) {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        use winit::keyboard::{Key, NamedKey};

        if matches!(event, WindowEvent::Focused(false) | WindowEvent::Destroyed)
            || matches!(event, WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && event.logical_key == Key::Named(NamedKey::Escape))
        {
            if self
                .cross_window_drag
                .current
                .as_ref()
                .is_some_and(|transfer| transfer.source == id)
            {
                self.cross_window_drag.cancel();
            }
            return;
        }
        let released = match event {
            WindowEvent::CursorMoved { .. } => false,
            WindowEvent::ModifiersChanged(_) if self.cross_window_drag.current.is_some() => false,
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: ElementState::Released,
                ..
            } => true,
            _ => return,
        };
        let source = self.hosts.iter().find(|host| host.window.id() == id && host.visible());
        if id != root.id() && source.is_none() {
            return;
        }
        if self
            .cross_window_drag
            .current
            .as_ref()
            .is_some_and(|transfer| transfer.released || transfer.source != id)
        {
            return;
        }
        if self.cross_window_drag.current.is_none() && !Payload::carried(ctx) {
            return;
        }
        let modifiers = match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                egui::Modifiers {
                    alt: state.alt_key(),
                    ctrl: state.control_key(),
                    shift: state.shift_key(),
                    command: state.control_key(),
                    ..egui::Modifiers::NONE
                }
            }
            _ => source.map_or(root_modifiers, |host| host.drag_modifiers),
        };
        let target = win32::under_pointer(
            std::iter::once(root.as_ref()).chain(self.hosts.iter().filter(|host| host.visible()).map(|host| host.window.as_ref())),
        );
        if self.cross_window_drag.current.is_none() && target.is_none_or(|(window, _)| window == id) {
            return;
        }
        if self.cross_window_drag.current.is_none() {
            let Some(cargo) = Payload::take(ctx) else { return };
            self.cross_window_drag.current = Some(Transfer {
                source: id,
                cargo: Arc::new(cargo),
                target: None,
                capture_cursor: Some(win32::CapturedCursor::new(Arc::clone(source.map_or(root, |host| &host.window)))),
                released: false,
            });
        }
        let Some(transfer) = &mut self.cross_window_drag.current else {
            return;
        };
        transfer.released = released;
        transfer.target = target.map(|(target, point)| (target, point, modifiers));
        if (released || target.is_none_or(|(window, _)| window != root.id()))
            && let Some(capture) = &mut transfer.capture_cursor
        {
            capture.reset();
        }
        if let Some((target, _)) = target {
            if target == root.id() {
                root.request_redraw();
            } else if let Some(host) = self.hosts.iter_mut().find(|host| host.window.id() == target) {
                host.set_repaint(std::time::Duration::ZERO);
            }
        } else if released {
            self.cross_window_drag.cancel();
        }
    }
}

#[cfg(windows)]
mod win32 {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, POINT},
        UI::Input::KeyboardAndMouse::GetCapture,
        UI::WindowsAndMessaging::{GetCursorPos, WindowFromPoint},
    };
    use winit::event_loop::ActiveEventLoop;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Cursor, CursorIcon, CustomCursor};

    pub(super) struct CapturedCursor {
        window: Arc<Window>,
        selection: Option<(egui::CursorIcon, Option<egui::CustomCursorImage>, Cursor)>,
    }

    impl CapturedCursor {
        pub(super) fn new(window: Arc<Window>) -> Self {
            Self { window, selection: None }
        }

        pub(super) fn has_capture(&self) -> bool {
            let Ok(handle) = self.window.window_handle() else { return false };
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                return false;
            };
            // SAFETY: on the event-loop thread. We compare the handles without dereferencing them.
            unsafe { GetCapture() == handle.hwnd.get() as HWND }
        }

        pub(super) fn reset(&mut self) {
            if self.selection.take().is_some() {
                self.window.set_cursor(CursorIcon::Default);
            }
        }

        pub(super) fn update(&mut self, event_loop: &ActiveEventLoop, icon: egui::CursorIcon, image: Option<&egui::CustomCursorImage>) {
            if self.selection.as_ref().is_some_and(|(current, bitmap, _)| {
                *current == icon
                    && match (bitmap.as_ref(), image) {
                        (Some(a), Some(b)) => Arc::ptr_eq(&a.rgba, &b.rgba) && a.size == b.size && a.hotspot == b.hotspot,
                        (None, None) => true,
                        _ => false,
                    }
            }) {
                return;
            }
            let cursor = image
                .and_then(|image| {
                    CustomCursor::from_rgba(
                        image.rgba.to_vec(),
                        image.size[0],
                        image.size[1],
                        image.hotspot[0],
                        image.hotspot[1],
                    )
                    .ok()
                })
                .map(|source| event_loop.create_custom_cursor(source).into())
                .unwrap_or_else(|| {
                    match icon {
                        egui::CursorIcon::Copy => CursorIcon::Copy,
                        egui::CursorIcon::Move => CursorIcon::Move,
                        egui::CursorIcon::Grabbing => CursorIcon::Grabbing,
                        egui::CursorIcon::NotAllowed => CursorIcon::NotAllowed,
                        _ => CursorIcon::Default,
                    }
                    .into()
                });
            self.selection = Some((icon, image.cloned(), cursor));
            self.apply();
        }

        pub(super) fn apply(&self) {
            if let Some((_, _, cursor)) = &self.selection {
                self.window.set_cursor(cursor.clone());
            }
        }
    }

    impl Drop for CapturedCursor {
        fn drop(&mut self) {
            self.reset();
        }
    }

    pub(super) fn under_pointer<'a>(windows: impl Iterator<Item = &'a Window>) -> Option<(WindowId, PhysicalPosition<f64>)> {
        let mut point = POINT::default();
        // SAFETY: Windows writes the current physical screen position into a local POINT.
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return None;
        }
        // SAFETY: The POINT was filled by GetCursorPos and no HWND is dereferenced.
        let hwnd = unsafe { WindowFromPoint(point) };
        for window in windows {
            let Ok(handle) = window.window_handle() else { continue };
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                continue;
            };
            if handle.hwnd.get() as HWND != hwnd {
                continue;
            }
            let origin = window.inner_position().ok()?;
            let size = window.inner_size();
            let x = f64::from(point.x) - f64::from(origin.x);
            let y = f64::from(point.y) - f64::from(origin.y);
            if x >= 0.0 && y >= 0.0 && x < f64::from(size.width) && y < f64::from(size.height) {
                return Some((window.id(), PhysicalPosition::new(x, y)));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::inventory::InvDrag;

    #[test]
    fn a_stationary_drag_stays_in_its_destination_until_release() {
        let source = WindowId::from(10);
        let destination = WindowId::from(20);
        let point = PhysicalPosition::new(51.0, 67.0);
        let mut session = Session {
            current: Some(Transfer {
                source,
                cargo: Arc::new(Payload::Inventory(Cargo::Item(Arc::new(InvDrag(uuid::Uuid::from_u128(42)))))),
                target: Some((destination, point, egui::Modifiers::NONE)),
                #[cfg(windows)]
                capture_cursor: None,
                released: false,
            }),
        };
        for _ in 0..3 {
            assert!(session.frame(source).is_none());
            let frame = session.frame(destination).expect("preview stays visible while held");
            assert_eq!(frame.point, point);
            assert!(!frame.released);
        }
        let transfer = session.current.as_mut().expect("active drag");
        transfer.released = true;
        assert!(session.frame(destination).expect("single drop").released);
        assert!(session.frame(destination).is_none());
    }

    #[test]
    fn a_release_is_delivered_once_to_its_own_window() {
        let source = WindowId::from(10);
        let destination = WindowId::from(20);
        let mut session = Session {
            current: Some(Transfer {
                source,
                cargo: Arc::new(Payload::Inventory(Cargo::Item(Arc::new(InvDrag(uuid::Uuid::from_u128(42)))))),
                target: Some((destination, PhysicalPosition::new(51.0, 67.0), egui::Modifiers::CTRL)),
                #[cfg(windows)]
                capture_cursor: None,
                released: true,
            }),
        };
        assert!(session.frame(source).is_none());
        let input = session.frame(destination).expect("destination's release");
        assert!(input.released);
        assert_eq!(input.point, PhysicalPosition::new(51.0, 67.0));
        assert_eq!(input.modifiers, egui::Modifiers::CTRL);
        assert!(session.frame(destination).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_places_drag_preserves_its_destination_across_windows() {
        let ctx = egui::Context::default();
        let target = crate::ui::places::drag::Target::History(glam::DVec3::new(1.0, 2.0, 3.0));
        egui::DragAndDrop::set_payload(
            &ctx,
            PlaceDrag {
                target,
                favorite: None,
                name: String::from("Destination"),
                thumbnail: None,
            },
        );
        let transfer = Payload::take(&ctx).expect("places transfer");
        assert!(!egui::DragAndDrop::has_any_payload(&ctx));
        transfer.present(&ctx);
        assert_eq!(egui::DragAndDrop::payload::<PlaceDrag>(&ctx).unwrap().target, target);
    }

    #[test]
    fn closing_the_destination_cancels_a_pending_release() {
        let source = WindowId::from(30);
        let destination = WindowId::from(40);
        let mut session = Session {
            current: Some(Transfer {
                source,
                cargo: Arc::new(Payload::Inventory(Cargo::Item(Arc::new(InvDrag(uuid::Uuid::from_u128(42)))))),
                target: Some((destination, PhysicalPosition::new(0.0, 0.0), egui::Modifiers::NONE)),
                #[cfg(windows)]
                capture_cursor: None,
                released: true,
            }),
        };
        session.forget_window(destination);
        assert!(session.frame(destination).is_none());
    }
}
