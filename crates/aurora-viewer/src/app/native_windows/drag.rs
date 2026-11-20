//! Carry the same egui inventory and place payload between native windows.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;
use winit::data_transfer::{DataTransferId, DataTransferLimits, DataTransferSendBuilder, TypeHint};
use winit::dpi::PhysicalPosition;
use winit::error::TransferError;
use winit::event::{ButtonSource, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, AsyncRequestSerial, DndAction};
use winit::window::{Window, WindowId};

use crate::ui::inventory::{drag::Cargo, task_drop::TaskDrag};
use crate::ui::places::drag::PlaceDrag;

// Claim XdndSelection while the pointer still belongs to the source. Mutter
// can then pin XWayland's pointer focus to the drag origin across the border.
pub(super) fn starts_inside_source(event_loop: &dyn ActiveEventLoop) -> bool {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::ActiveEventLoopExtX11 as _;
        event_loop.is_xwayland()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = event_loop;
        false
    }
}

enum Payload {
    Inventory(Cargo),
    Task(Arc<TaskDrag>),
    Place(Arc<PlaceDrag>),
}

impl Payload {
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

#[derive(Clone, Copy)]
pub(super) struct Origin {
    point: egui::Pos2,
    modifiers: egui::Modifiers,
}

impl Origin {
    pub(super) fn read(ctx: &egui::Context) -> Option<Self> {
        ctx.input(|input| {
            input.pointer.primary_down().then_some(())?;
            Some(Self {
                point: input.pointer.interact_pos()?,
                modifiers: input.modifiers,
            })
        })
    }

    fn release(self, point: egui::Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: self.modifiers,
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
    pub(in crate::app) fn add_events(&self, ctx: &egui::Context, window: &dyn Window, raw: &mut egui::RawInput) {
        let scale = window.scale_factor() as f32 * ctx.zoom_factor();
        let point = egui::pos2(self.point.x as f32 / scale, self.point.y as f32 / scale);
        self.cargo.present(ctx);
        // The OS may also deliver a button-up to the original window. Only
        // the verified transfer can release egui's shared payload.
        raw.events.retain(|event| {
            !matches!(
                event,
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                }
            )
        });
        raw.events.push(egui::Event::ModifiersChanged(self.modifiers));
        raw.events.push(egui::Event::PointerMoved(point));
        if self.released {
            raw.events.push(
                Origin {
                    point,
                    modifiers: self.modifiers,
                }
                .release(point),
            );
        }
    }
}

#[derive(Default)]
pub(super) struct Session {
    current: Option<Transfer>,
    reset: Option<(WindowId, Origin)>,
    failed_source: Option<WindowId>,
}

struct Transfer {
    source: WindowId,
    origin: Origin,
    outgoing: DataTransferId,
    token: String,
    cargo: Arc<Payload>,
    target: Option<Target>,
    expires_at: Option<Instant>,
}

struct Target {
    window: WindowId,
    incoming: DataTransferId,
    point: Option<PhysicalPosition<f64>>,
    phase: Phase,
    allowed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hover,
    Reading(AsyncRequestSerial),
    Ready,
}

impl Session {
    pub(super) fn start(&mut self, event_loop: &dyn ActiveEventLoop, window: WindowId, ctx: &egui::Context, origin: Origin) -> bool {
        if self.current.is_some() || self.failed_source == Some(window) {
            return false;
        }
        let Some(cargo) = Payload::take(ctx) else { return false };
        // Only a one-time token goes through the OS. The actual inventory IDs,
        // names and permissions stay in the viewer and are resolved on drop.
        let token = Uuid::new_v4().to_string();
        let offered = DataTransferSendBuilder::new(token.clone())
            .with_type(TypeHint::Plaintext, |value, _| Some(value.clone()))
            .build();
        let outgoing = match event_loop.start_drag(window, offered, &[DndAction::Copy], None) {
            Ok(outgoing) => outgoing,
            Err(err) => {
                self.failed_source = Some(window);
                cargo.present(ctx);
                log::debug!("native drag could not start for {window:?}: {err}");
                return false;
            }
        };
        log::debug!("native drag started: source={window:?}, id={outgoing:?}");
        self.current = Some(Transfer {
            source: window,
            origin,
            outgoing,
            token,
            cargo: Arc::new(cargo),
            target: None,
            expires_at: None,
        });
        true
    }

    pub(super) fn active(&self) -> bool {
        self.current.is_some()
    }

    pub(super) fn target_window(&self) -> Option<WindowId> {
        self.current.as_ref()?.target.as_ref().map(|target| target.window)
    }

    pub(super) fn cancel(&mut self) {
        if let Some(transfer) = self.current.take() {
            self.reset = Some((transfer.source, transfer.origin));
        }
        self.failed_source = None;
    }

    pub(super) fn forget_window(&mut self, window: WindowId) {
        if self
            .current
            .as_ref()
            .is_some_and(|transfer| transfer.source == window || transfer.target.as_ref().is_some_and(|target| target.window == window))
        {
            self.cancel();
        }
        if self.reset.is_some_and(|(source, _)| source == window) {
            self.reset = None;
        }
        if self.failed_source == Some(window) {
            self.failed_source = None;
        }
    }

    pub(super) fn reset_pointer(&mut self, window: WindowId, raw: &mut egui::RawInput) {
        if self.reset.is_some_and(|(source, _)| source == window)
            && let Some((_, origin)) = self.reset.take()
        {
            raw.events.push(origin.release(origin.point));
            raw.events.push(egui::Event::PointerGone);
        }
    }

    pub(super) fn frame(&mut self, window: WindowId) -> Option<Input> {
        let transfer = self.current.as_ref()?;
        if transfer.expires_at.is_some_and(|deadline| deadline <= Instant::now()) {
            self.cancel();
            return None;
        }
        let target = transfer.target.as_ref()?;
        if window != target.window {
            return None;
        }
        let released = target.phase == Phase::Ready;
        let input = Input {
            point: target.point?,
            cargo: Arc::clone(&transfer.cargo),
            modifiers: transfer.origin.modifiers,
            released,
        };
        if released {
            let source = transfer.source;
            self.cancel();
            if source == window {
                // This frame already releases the pointer in the source.
                self.reset = None;
            }
        }
        Some(input)
    }

    pub(super) fn feedback(&mut self, event_loop: &dyn ActiveEventLoop, window: WindowId, cursor: egui::CursorIcon) {
        let Some(target) = self.current.as_mut().and_then(|transfer| transfer.target.as_mut()) else {
            return;
        };
        if target.window != window || target.phase != Phase::Hover {
            return;
        }
        let allowed = matches!(cursor, egui::CursorIcon::Copy | egui::CursorIcon::Move | egui::CursorIcon::Grabbing);
        if allowed != target.allowed {
            log::debug!("native drag feedback: window={window:?}, cursor={cursor:?}, allowed={allowed}");
            target.allowed = allowed;
            set_allowed(event_loop, target.incoming, allowed);
        }
    }

    pub(super) fn event(&mut self, event_loop: &dyn ActiveEventLoop, window: WindowId, event: &WindowEvent, can_target: bool) -> bool {
        if self.failed_source == Some(window)
            && matches!(
                event,
                WindowEvent::PointerButton {
                    button: ButtonSource::Mouse(MouseButton::Left),
                    state: ElementState::Released,
                    ..
                } | WindowEvent::Focused(false)
            )
        {
            self.failed_source = None;
        }
        let Some(transfer) = self.current.as_mut() else { return false };
        match event {
            WindowEvent::DragEntered { id, position } => {
                let offers_text = can_target && offers_token(event_loop, *id);
                log::debug!("native drag entered: window={window:?}, id={id:?}, can_target={can_target}, text={offers_text}");
                if !can_target || !offers_text {
                    return false;
                }
                transfer.target = Some(Target {
                    window,
                    incoming: *id,
                    point: *position,
                    phase: Phase::Hover,
                    allowed: true,
                });
                set_allowed(event_loop, *id, true);
            }
            WindowEvent::DragPosition { id, position, .. } => {
                let Some(target) = transfer.target.as_mut().filter(|target| target.matches(window, *id)) else {
                    return false;
                };
                if target.phase == Phase::Hover {
                    target.point = Some(*position);
                    set_allowed(event_loop, *id, can_target && target.allowed);
                }
            }
            WindowEvent::DragDropped { id, .. } => {
                log::debug!("native drag dropped: window={window:?}, id={id:?}, can_target={can_target}");
                let Some(target) = transfer.target.as_mut().filter(|target| target.matches(window, *id)) else {
                    return false;
                };
                if target.phase != Phase::Hover {
                    return true;
                }
                if can_target
                    && target.point.is_some()
                    && let Ok(serial) = request_token(event_loop, *id)
                {
                    target.phase = Phase::Reading(serial);
                    transfer.expires_at = Some(Instant::now() + Duration::from_secs(5));
                } else {
                    self.cancel();
                }
            }
            WindowEvent::DragLeft { id } => {
                log::debug!("native drag left: window={window:?}, id={id:?}");
                let Some(target) = transfer.target.as_ref().filter(|target| target.matches(window, *id)) else {
                    return false;
                };
                if target.phase == Phase::Hover {
                    transfer.target = None;
                }
            }
            WindowEvent::DataTransferReceived { id, serial, value } => {
                let Some(target) = transfer
                    .target
                    .as_mut()
                    .filter(|target| target.matches(window, *id) && target.phase == Phase::Reading(*serial))
                else {
                    return false;
                };
                if value.try_as_string().is_ok_and(|token| token == transfer.token) {
                    target.phase = Phase::Ready;
                } else {
                    self.cancel();
                }
            }
            WindowEvent::OutgoingDragCanceled { id } if window == transfer.source && *id == transfer.outgoing => {
                log::debug!("native drag canceled: source={window:?}, id={id:?}");
                self.cancel();
            }
            WindowEvent::OutgoingDragDropped { id, .. } if window == transfer.source && *id == transfer.outgoing => {
                if transfer.target.is_none() {
                    self.cancel();
                } else {
                    transfer.expires_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
            WindowEvent::ModifiersChanged(value) => {
                if window == transfer.source || transfer.target.as_ref().is_some_and(|target| window == target.window) {
                    let state = value.state();
                    transfer.origin.modifiers = egui::Modifiers {
                        alt: state.alt_key(),
                        ctrl: state.control_key(),
                        shift: state.shift_key(),
                        mac_cmd: cfg!(target_os = "macos") && state.meta_key(),
                        command: if cfg!(target_os = "macos") {
                            state.meta_key()
                        } else {
                            state.control_key()
                        },
                    };
                }
                return false;
            }
            _ => return false,
        }
        true
    }
}

impl Target {
    fn matches(&self, window: WindowId, id: DataTransferId) -> bool {
        self.window == window && self.incoming == id
    }
}

fn offers_token(event_loop: &dyn ActiveEventLoop, id: DataTransferId) -> bool {
    let Ok(data) = event_loop.data_transfer(id) else { return false };
    let mut text = false;
    data.for_each_available_type(&mut |ty| {
        if ty.hint() == Some(TypeHint::Plaintext) {
            text = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    text
}

fn request_token(event_loop: &dyn ActiveEventLoop, id: DataTransferId) -> Result<AsyncRequestSerial, TransferError> {
    let request = event_loop.fetch_data_transfer_with_limits(
        id,
        &TypeHint::Plaintext,
        DataTransferLimits {
            max_bytes: 128,
            timeout: Duration::from_secs(5),
        },
    );
    match request {
        Err(TransferError::NotSupported(_)) => event_loop.fetch_data_transfer(id, &TypeHint::Plaintext),
        result => result,
    }
}

fn set_allowed(event_loop: &dyn ActiveEventLoop, id: DataTransferId, allowed: bool) {
    let actions: &[DndAction] = if allowed { &[DndAction::Copy] } else { &[] };
    let _ = event_loop.set_valid_dnd_actions(id, actions);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::inventory::InvDrag;

    fn drag(phase: Phase) -> Session {
        Session {
            current: Some(Transfer {
                source: WindowId::from_raw(10),
                origin: Origin {
                    point: egui::pos2(11.0, 21.0),
                    modifiers: egui::Modifiers::CTRL,
                },
                outgoing: DataTransferId::from_raw(4),
                token: "internal test".into(),
                cargo: Arc::new(Payload::Inventory(Cargo::Item(Arc::new(InvDrag(Uuid::from_u128(42)))))),
                target: Some(Target {
                    window: WindowId::from_raw(20),
                    incoming: DataTransferId::from_raw(5),
                    point: Some(PhysicalPosition::new(51.0, 67.0)),
                    phase,
                    allowed: true,
                }),
                expires_at: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_stationary_native_drag_keeps_its_preview_and_releases_once() {
        let source = WindowId::from_raw(10);
        let destination = WindowId::from_raw(20);
        let mut session = drag(Phase::Hover);
        for _ in 0..3 {
            assert!(session.frame(source).is_none());
            let frame = session.frame(destination).expect("preview while held");
            assert_eq!(frame.point, PhysicalPosition::new(51.0, 67.0));
            assert!(!frame.released);
        }
        let target = session.current.as_mut().unwrap().target.as_mut().unwrap();
        target.phase = Phase::Reading(AsyncRequestSerial::get());
        assert!(!session.frame(destination).unwrap().released);
        session.current.as_mut().unwrap().target.as_mut().unwrap().phase = Phase::Ready;
        assert!(session.frame(destination).unwrap().released);
        assert!(session.frame(destination).is_none());
        let mut raw = egui::RawInput::default();
        session.reset_pointer(source, &mut raw);
        assert_eq!(raw.events.len(), 2);
        session.reset_pointer(source, &mut raw);
        assert_eq!(raw.events.len(), 2);
    }

    #[test]
    fn a_closed_target_cancels_the_transfer() {
        let mut session = drag(Phase::Hover);
        session.forget_window(WindowId::from_raw(20));
        assert!(!session.active());
    }
}
